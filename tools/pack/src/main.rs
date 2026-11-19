// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The command: files in, a clip pack out.

use std::path::Path;
use std::process::ExitCode;

use onebitdawn_core::pack::{LABEL_MAX, NAME_MAX, ROOM};
use onebitdawn_pack::{Output, Source, Spec, build, convert};

const USAGE: &str = "\
Make a clip pack, the alarm sounds of a onebitdawn clock.

    onebitdawn-pack -o pack.bin [--sound-output buzzer|speaker] file[:option,…][=Label] …
    onebitdawn-pack --clip clip [--sound-output buzzer|speaker] file[:option,…]

A file is a tracker module (.xm, .mod, .s3m), a Commodore 64 tune
(.sid), a ZX Spectrum one (.pt3), a sound file (.wav), or a clip made
already (.xmc, .sidc, .pt3c, .bpr, .adp, .pcm), which goes in as it is:
up to 16 of them, 1152 KB together. A clip's label, which the clock's
pages show, is the tune's title where the file has one (a module, a
SID tune) and the file's name in words where it has not, unless =Label
gives another; its name is the letters of the same.

--clip writes one file's clip by itself and no pack: to keep, to put
in a pack later, or for a firmware built with the clip in it.

--sound-output is what the clock plays through, which the clips are
shaped for: its buzzer (the default), which has no bass and wants the
level pushed hard, or a speaker, in the buzzer's place or on the line
output's amplifier.

Options follow a file's name after a colon, with commas between them:

    onebitdawn-pack -o mine.bin tune.xm 'game.sid:song=2,seconds=90=The Game' ring.wav:seconds=20

Any file:
    name=NAME     the clip's name for the API, a-z and _, when the
                  file's is too long or taken
    loop, once    a ringing alarm plays the clip round and round with
                  no gap, or with a pause between; a tune loops and a
                  sound file does not unless these say otherwise

Tracker modules (.xm, .mod, .s3m), which the clock plays itself, up to
twenty channels; what a module uses that the player lacks is listed:
    highpass=HZ   the bass cut's corner (300; 100 for a speaker)
    passes=N      times it is applied, 12 dB an octave each, 0 to 2 (1)
    drive=N       what the limiter aims a peak at, 127 being full scale
    release=N     the limiter lets go by 1/2^N a sample, 1 to 15
                  (7; 10 for a speaker)
    knee=N        the level the limiter starts at, in 1024s of the mix;
                  above the tune's peaks it is off
    raise=I:N     play instrument I that many octaves higher
    rate=HZ       31250 (the default), 16000 or 8000 samples a second
    bits=N        16 (the default) or 8

Commodore 64 tunes (.sid, PSID), whose code is run here to the point
where the tune comes round:
    song=N        which of the file's songs, from 1
    seconds=S     cut the tune here, loop or not
    mute=FRAMES   keep the voices' gates closed at the start, for a
                  routine that clicks
    model=6581|8580, system=pal|ntsc
                  the chip and the machine, if not what the file says
    steps=2|4     steps of the chip a sample
    flat          no limiter: drive is a plain gain, 127 the three
                  voices at their top as full scale (a speaker's default)
    limiter       the limiter, where flat is the default
    plain         the record unpacked, for a firmware before 2026-11-16
    highpass, passes, drive, release, rate (31250 or 16000) as above;
                  a speaker's high-pass is 150 Hz, twice

ZX Spectrum tunes (.pt3, Pro Tracker 3 and Vortex Tracker II):
    restart       a ringing alarm starts over from the top, for a
                  module whose loop is only its ending
    machine=pentagon|spectrum
                  whose clock and frame rate, if not the tone table's
    clock=HZ      the chip's clock, if not the machine's
    chip=ay|ym    an AY-3-8910 (the default) or a YM2149
    flat          the chip as it is, no limiter; drive is a plain gain
    noise=N       the noise at N sixteenths of its level (16)
    noise-lowpass=HZ
                  a low-pass on the noise, which takes its hiss
    highpass (100), passes, drive, release, knee, rate as above

Sound files (.wav), 8 KB of the pack a second:
    seconds=S     keep this much, faded out at its end
    skip=S        from this far in
    rate=HZ       16000 (the default), 8000 or 31250
    format=adpcm|pcm
                  four bits a sample (the default) or eight
    highpass=HZ   the bass cut (900; 100 for a speaker)
    passes=N      times it is applied (2; 1 for a speaker)
    drive=DB      pushed into the limiter (20; 10 for a speaker)

The pack goes to the clock from its Clips page, http://<clock>/clips.
The same in a browser: https://anarsoul.github.io/onebitdawn/pack/";

fn run(args: &[String]) -> Result<(), String> {
    let mut out = None;
    let mut alone = None;
    let mut output = Output::default();
    let mut specs = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-o" | "--output" => out = Some(args.next().ok_or("-o needs the pack's file name")?.clone()),
            "--clip" => alone = Some(args.next().ok_or("--clip needs the clip's file name")?.clone()),
            "--sound-output" => {
                let name = args.next().ok_or("--sound-output needs buzzer or speaker")?;
                output = Output::parse(name).ok_or_else(|| format!("--sound-output {name}: buzzer or speaker"))?;
            }
            other => match other.strip_prefix("--sound-output=") {
                Some(name) => output = Output::parse(name).ok_or_else(|| format!("--sound-output {name}: buzzer or speaker"))?,
                None if other.starts_with('-') && other.len() > 1 && !Path::new(other).exists() => {
                    return Err(format!("no option {other}; --help says what there is"));
                }
                None => specs.push(other.to_string()),
            },
        }
    }
    if specs.is_empty() {
        return Err("no files; --help says how".into());
    }
    if alone.is_some() && (out.is_some() || specs.len() != 1) {
        return Err("--clip is one file's clip, and no pack with it".into());
    }
    let mut clips = Vec::new();
    for arg in &specs {
        // A file of that very name is the file, whatever its colons.
        let spec = if Path::new(arg).is_file() { Spec { file: arg.clone(), options: Vec::new(), label: None } } else { Spec::parse(arg) };
        let data = std::fs::read(&spec.file).map_err(|e| format!("{}: {e}", spec.file))?;
        let clip = convert(&Source { file: spec.file, data, options: spec.options, label: spec.label }, output)?;
        for note in &clip.notes {
            eprintln!("{arg}: {note}");
        }
        clips.push(clip);
    }
    let write = |out: &str, data: &[u8]| -> Result<(), String> {
        if let Some(dir) = Path::new(out).parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(out, data).map_err(|e| format!("{out}: {e}"))
    };
    if let Some(out) = alone {
        write(&out, &clips[0].data)?;
        println!("{out}: {} bytes{}", clips[0].data.len(), if clips[0].loops { ", loops" } else { "" });
        return Ok(());
    }
    let out = out.ok_or("no -o pack.bin; --help says how")?;
    let pack = build(&clips)?;
    write(&out, &pack)?;
    for clip in &clips {
        let loops = if clip.loops { ", loops" } else { "" };
        println!("  {:<NAME_MAX$} {:<LABEL_MAX$} {:>7} bytes{loops}", clip.name, clip.label, clip.data.len());
    }
    println!("{out}: {} clips, {} bytes of {ROOM} ({} %)", clips.len(), pack.len(), pack.len() * 100 / ROOM);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("onebitdawn-pack: {message}");
            ExitCode::FAILURE
        }
    }
}

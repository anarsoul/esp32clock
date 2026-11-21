// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The default pack's clips made again from the modules in `mods/`:
//! byte for byte what is in `clips/`, which the Python converters this
//! crate was ported from made, and which the clocks have. The kinds
//! the default pack has none of are held to `tests/data/`, the same
//! converters' output for three small files.

use std::path::Path;

use onebitdawn_core::pack::Pack;
use onebitdawn_pack::{Clip, Output, Source, Spec, build, convert};

fn repo(path: &str) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The clip of a file of the repository, by an argument of the command.
fn made(arg: &str, output: Output) -> Clip {
    let spec = Spec::parse(arg);
    let source = Source { data: repo(&spec.file), file: spec.file, options: spec.options, label: spec.label };
    convert(&source, output).unwrap()
}

#[test]
fn the_modules_come_out_as_the_clips_in_the_repository() {
    for (arg, clip) in [
        ("mods/Orion - Nero Burning ROM v5.5.7.8kg.xm:name=halftime", "clips/halftime.xmc"),
        ("mods/Orion-Nero6.3.0.2.xm:name=keygen", "clips/keygen.xmc"),
        ("mods/nq-synchronization.pt3:restart,name=synchronization", "clips/synchronization.pt3c"),
        ("mods/grieg-morning-bright.xm", "clips/morning_mood.xmc"),
        ("mods/grieg-morning-chip.xm", "clips/morning_mood_chip.xmc"),
        ("mods/grieg-mountain-king-bright.xm", "clips/mountain_king.xmc"),
        ("mods/suspended-seconds.xm:knee=12,release=10", "clips/suspended_seconds.xmc"),
        ("tools/pack/tests/data/tiny.mod", "tools/pack/tests/data/tiny-mod.xmc"),
        ("tools/pack/tests/data/tiny.s3m", "tools/pack/tests/data/tiny-s3m.xmc"),
        ("tools/pack/tests/data/tiny.sid", "tools/pack/tests/data/tiny.sidc"),
    ] {
        let made = made(arg, Output::Buzzer);
        assert!(made.data == repo(clip), "{arg} is not {clip}");
        assert!(made.loops);
    }
}

#[test]
fn a_clip_made_already_goes_in_as_it_is() {
    for clip in [
        "clips/trantor.bpr",
        "clips/halftime.xmc",
        "clips/keygen.xmc",
        "clips/synchronization.pt3c",
        "clips/morning_mood.xmc",
        "clips/morning_mood_chip.xmc",
        "clips/mountain_king.xmc",
        "clips/suspended_seconds.xmc",
    ] {
        let made = made(clip, Output::Speaker);
        assert!(made.data == repo(clip) && made.loops && made.notes.is_empty(), "{clip}");
    }
}

#[test]
fn the_default_pack_from_its_sources() {
    let clips = [
        made("clips/trantor.bpr", Output::Buzzer),
        made("mods/Orion - Nero Burning ROM v5.5.7.8kg.xm:name=halftime=Halftime", Output::Buzzer),
        made("mods/Orion-Nero6.3.0.2.xm:name=keygen=Keygen", Output::Buzzer),
        made("mods/nq-synchronization.pt3:restart,name=synchronization=Synchronization", Output::Buzzer),
        // The modules made here have their names and labels in their titles.
        made("mods/grieg-morning-bright.xm", Output::Buzzer),
        made("mods/grieg-morning-chip.xm", Output::Buzzer),
        made("mods/grieg-mountain-king-bright.xm", Output::Buzzer),
        made("mods/suspended-seconds.xm:knee=12,release=10", Output::Buzzer),
    ];
    let pack = build(&clips).unwrap();
    let pack = Pack::parse(&pack).unwrap();
    let names: Vec<&str> = (0..pack.count()).map(|k| pack.clip(k).unwrap().name).collect();
    assert_eq!(names, ["trantor", "halftime", "keygen", "synchronization", "morning_mood", "morning_mood_chip", "mountain_king", "suspended_seconds"]);
    let labels: Vec<&str> = (0..pack.count()).map(|k| pack.clip(k).unwrap().label).collect();
    assert_eq!(labels, ["Trantor", "Halftime", "Keygen", "Synchronization", "Morning Mood", "Morning Mood Chip", "Mountain King", "Suspended Seconds"]);
    // A speaker's clips are others, of the same lengths: the shaping
    // is the header's.
    // And the speaker's pack has a ninth, which the buzzer's has not
    // (scripts/speaker-clips.sh): its name and label are its title's.
    let ninth = made("mods/starlight-aurora.xm:knee=48,release=13", Output::Speaker);
    assert_eq!((ninth.name.as_str(), ninth.label.as_str(), ninth.loops), ("starlight_and_aurora", "Starlight and Aurora", true));
    // One note, the module's line: nothing in it that the player lacks.
    assert_eq!(ninth.notes.len(), 1, "{:?}", ninth.notes);
    for (arg, k) in [("mods/Orion-Nero6.3.0.2.xm:name=keygen", 2), ("mods/nq-synchronization.pt3:restart,name=synchronization", 3)] {
        let speaker = made(arg, Output::Speaker);
        assert!(speaker.data.len() == clips[k].data.len() && speaker.data != clips[k].data, "{arg}");
    }
}

#[test]
fn what_a_module_has_that_the_player_lacks_is_said() {
    let module = made("tools/pack/tests/data/tiny.mod", Output::Buzzer);
    assert!(module.notes.iter().any(|n| n == "not converted: the effect E0, the Amiga's filter"), "{:?}", module.notes);
    let module = made("tools/pack/tests/data/tiny.s3m", Output::Buzzer);
    assert!(module.notes.iter().any(|n| n == "not converted: the effects E, H, S"), "{:?}", module.notes);
    let module = made("tools/pack/tests/data/tiny.s3m:raise=1:1,rate=16000", Output::Speaker);
    assert_eq!(module.data.len(), repo("tools/pack/tests/data/tiny-s3m.xmc").len());
    let tune = made("tools/pack/tests/data/tiny.sid:seconds=3,plain", Output::Buzzer);
    assert!(tune.data.starts_with(b"SID1") && tune.notes.iter().any(|n| n.contains("cut, starts over from the top")), "{:?}", tune.notes);
    let refused = convert(&Source { file: "tune.xm".into(), data: b"Extended Module: cut".to_vec(), ..Default::default() }, Output::Buzzer);
    assert_eq!(refused.unwrap_err(), "tune.xm: the file is cut short");
}

/// A second and a half of a tone that dies away, as a WAV file of 16
/// bits and two channels.
fn wav(rate: u32) -> Vec<u8> {
    let frames = rate * 3 / 2;
    let mut out = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0".to_vec();
    out.extend(rate.to_le_bytes());
    out.extend((rate * 4).to_le_bytes());
    out.extend([4, 0, 16, 0]);
    out.extend(b"LIST\x03\0\0\0abc\0data");
    out.extend((frames * 4).to_le_bytes());
    for k in 0..frames {
        let t = k as f64 / rate as f64;
        let s = (12000.0 * (2.0 * core::f64::consts::PI * 1500.0 * t).sin() * (1.0 - t / 1.5)) as i16;
        out.extend(s.to_le_bytes());
        out.extend((s / 2).to_le_bytes());
    }
    out
}

#[test]
fn a_sound_file_is_a_clip_at_the_clips_rate() {
    for (rate, options, magic, bytes) in
        [(44100, "", &b"ADP1\x80\x3e\0\0"[..], 8 + 12000), (22050, "format=pcm,rate=31250", b"PCM1\x12\x7a\0\0", 8 + 46875), (16000, "format=pcm,rate=8000,seconds=1.25", b"", 10000)]
    {
        let source = Source { file: "ring.wav".into(), data: wav(rate), options: onebitdawn_pack::spec::parse_options(options).unwrap(), label: None };
        let clip = convert(&source, Output::Speaker).unwrap();
        assert!(clip.data.starts_with(magic) && !clip.loops, "{options}");
        assert_eq!(clip.data.len(), bytes, "{options}");
        // The tone is there, as loud as the clip gets, and it is
        // 1500 Hz still: the samples go through nought that often.
        if !magic.starts_with(b"ADP") {
            let samples: Vec<i32> = clip.data[magic.len()..].iter().map(|&b| b as i32 - 128).collect();
            let loudest = samples.iter().map(|s| s.abs()).max().unwrap();
            assert!((110..=128).contains(&loudest), "{options}: {loudest}");
            let out_rate = if magic.is_empty() { 8000 } else { 31250 };
            let part = &samples[out_rate / 10..out_rate / 2];
            let crossings = part.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
            let hz = crossings as f64 / 2.0 / (part.len() as f64 / out_rate as f64);
            assert!((hz - 1500.0).abs() < 40.0, "{options}: {hz} Hz");
        }
    }
    let cut = Source { file: "ring.wav".into(), data: wav(8000)[..60].to_vec(), ..Default::default() };
    assert!(convert(&cut, Output::Buzzer).is_err());
}

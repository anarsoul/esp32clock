// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A sound file (.wav) as a PCM or an ADPCM clip: mono, at the clip's
//! rate, shaped for what the clock plays it through.
//!
//! The buzzer is a resonator near 2 kHz and all but silent under
//! 1 kHz, so everything below 900 Hz is cut (twice, 24 dB an octave),
//! which leaves the line's range to what the buzzer can play, and the
//! level is then pushed 20 dB into a limiter. Thin, but as loud as it
//! gets. A speaker plays the bass and gives 20 dB of drive away as
//! hiss and overdrive: the cut at 100 Hz, once, and 10 dB.
//!
//! The steps and their order are those of `scripts/wav2pcm.sh`, which
//! the clips of 2026 were made with through sox: the mix to mono, the
//! cut in time, the high-pass, the new rate, the peak brought to
//! -0.5 dB, the drive into the limiter (sox's `gain -l`, which is
//! `LIMITER` here), the peak again, the fade.

use core::f64::consts::PI;

use onebitdawn_core::clip::PCM_MAGIC;

use crate::bytes::Data;
use crate::{Options, Output, adpcm};

/// The options a sound file takes.
pub const OPTIONS: &[&str] = &["seconds", "skip", "rate", "format", "highpass", "passes", "drive"];

/// Where the peak is brought to, in dB under full scale.
const PEAK_DB: f64 = -0.5;
/// The fade at the end of a clip cut short, in seconds.
const FADE_S: f64 = 1.5;
/// The resampler's kernel: zero crossings a side, the Kaiser window's
/// beta (some 90 dB down in the stop band), the part of the band kept
/// under the lower of the two Nyquist rates, and the table's points a
/// zero crossing.
const CROSSINGS: usize = 24;
const BETA: f64 = 9.0;
const BAND: f64 = 0.94;
const POINTS: usize = 512;

/// How the clip is cut and shaped.
#[derive(Clone, Debug)]
pub struct Shape {
    /// Seconds of sound kept, 0 for all of it, from `skip` seconds in.
    pub seconds: f64,
    pub skip: f64,
    /// 16000, or 8000 for half the flash, or 31250 for twice it.
    pub rate: u32,
    /// Four bits a sample and not eight.
    pub adpcm: bool,
    /// The high-pass's corner in Hz, and how many times it is applied,
    /// 12 dB an octave each.
    pub highpass: f64,
    pub passes: u8,
    /// The dB pushed into the limiter, 0 for none.
    pub drive: f64,
}

impl Shape {
    pub fn new(output: Output) -> Self {
        let (highpass, passes, drive) = match output {
            Output::Buzzer => (900.0, 2, 20.0),
            Output::Speaker => (100.0, 1, 10.0),
        };
        Shape { seconds: 0.0, skip: 0.0, rate: 16000, adpcm: true, highpass, passes, drive }
    }

    pub fn options(output: Output, options: &mut Options) -> Result<Self, String> {
        let mut shape = Shape::new(output);
        shape.seconds = options.number("seconds", shape.seconds)?;
        shape.skip = options.number("skip", shape.skip)?;
        shape.rate = options.number("rate", shape.rate)?;
        shape.adpcm = match options.text("format")?.as_deref() {
            None | Some("adpcm") => true,
            Some("pcm") => false,
            Some(other) => return Err(format!("format={other}: adpcm or pcm")),
        };
        shape.highpass = options.number("highpass", shape.highpass)?;
        shape.passes = options.number("passes", shape.passes)?;
        shape.drive = options.number("drive", shape.drive)?;
        Ok(shape)
    }
}

/// The file's sound as one channel between -1 and 1, and its rate.
pub fn read(file: &[u8]) -> Result<(Vec<f64>, u32), String> {
    let d = Data(file);
    if !(file.starts_with(b"RIFF") && d.slice(8, 12) == b"WAVE") {
        return Err("not a WAV file".into());
    }
    let (mut at, mut format, mut sound) = (12, None, None);
    while let (Ok(id), Ok(len)) = (d.bytes(at, 4), d.size(at + 4)) {
        match id {
            b"fmt " => format = Some(at + 8),
            // A recorder that never went back to write the length
            // leaves 0 or more than there is: what there is, then.
            b"data" => sound = Some(d.slice(at + 8, if len == 0 { file.len() } else { (at + 8).saturating_add(len) })),
            _ => {}
        }
        if sound.is_some() && format.is_some() {
            break;
        }
        at = (at + 8).saturating_add(len).saturating_add(len & 1);
    }
    let (Some(f), Some(sound)) = (format, sound) else {
        return Err("a WAV file with no sound in it".into());
    };
    let (mut tag, channels, rate, bits) = (d.u16(f)?, d.u16(f + 2)? as usize, d.u32(f + 4)?, d.u16(f + 14)? as usize);
    if tag == 0xFFFE {
        // WAVE_FORMAT_EXTENSIBLE: the format is the sub-format's.
        tag = d.u16(f + 24)?;
    }
    if channels == 0 || !(1000..=768_000).contains(&rate) {
        return Err(format!("a WAV file of {channels} channels at {rate} Hz"));
    }
    let bytes = bits / 8;
    let sample: fn(&[u8]) -> f64 = match (tag, bits) {
        (1, 8) => |b| (b[0] as f64 - 128.0) / 128.0,
        (1, 16) => |b| i16::from_le_bytes([b[0], b[1]]) as f64 / 32768.0,
        (1, 24) => |b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f64 / 8_388_608.0,
        (1, 32) => |b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64 / 2_147_483_648.0,
        (3, 32) => |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        (3, 64) => |b| f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        _ => return Err(format!("a WAV file of format {tag} at {bits} bits; plain PCM or float is what this reads")),
    };
    let mono = sound
        .chunks_exact(bytes * channels)
        .map(|frame| {
            let sum: f64 = frame.chunks_exact(bytes).map(sample).sum();
            let mean = sum / channels as f64;
            if mean.is_finite() { mean } else { 0.0 }
        })
        .collect();
    Ok((mono, rate))
}

/// A second-order Butterworth high-pass at `corner` Hz, over the sound
/// in place.
fn highpass(sound: &mut [f64], corner: f64, rate: f64) {
    let w = 2.0 * PI * corner / rate;
    let alpha = w.sin() / 2f64.sqrt();
    let c = w.cos();
    let a0 = 1.0 + alpha;
    let (b0, b1, b2, a1, a2) = ((1.0 + c) / 2.0 / a0, -(1.0 + c) / a0, (1.0 + c) / 2.0 / a0, -2.0 * c / a0, (1.0 - alpha) / a0);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    for s in sound {
        let x = *s;
        let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        (x2, x1, y2, y1) = (x1, x, y1, y);
        *s = y;
    }
}

/// The modified Bessel function of the first kind, order 0.
fn bessel(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > sum * 1e-17 {
        term *= (x / 2.0 / k) * (x / 2.0 / k);
        sum += term;
        k += 1.0;
    }
    sum
}

/// The sound at another rate: a sinc under a Kaiser window, as wide as
/// the lower rate's band, read from a table.
fn resample(sound: &[f64], from: u32, to: u32) -> Vec<f64> {
    if from == to {
        return sound.to_vec();
    }
    // One side of the kernel, by the distance in zero crossings.
    let size = CROSSINGS * POINTS;
    let table: Vec<f64> = (0..=size + 1)
        .map(|k| {
            let x = k as f64 / POINTS as f64;
            let sinc = if k == 0 { 1.0 } else { (PI * x).sin() / (PI * x) };
            let edge = (x / CROSSINGS as f64).min(1.0);
            sinc * bessel(BETA * (1.0 - edge * edge).sqrt()) / bessel(BETA)
        })
        .collect();
    // The band, in the input's cycles a sample doubled: 1 is all of it.
    let band = BAND * (to as f64 / from as f64).min(1.0);
    let reach = CROSSINGS as f64 / band;
    let count = (sound.len() as u64 * to as u64 / from as u64) as usize;
    let mut out = Vec::with_capacity(count);
    for n in 0..count {
        let centre = n as f64 * from as f64 / to as f64;
        let first = (centre - reach).ceil().max(0.0) as usize;
        let last = ((centre + reach).floor() as usize).min(sound.len().saturating_sub(1));
        let mut sum = 0.0;
        for (k, s) in sound.iter().enumerate().take(last + 1).skip(first) {
            let x = (k as f64 - centre).abs() * band * POINTS as f64;
            let at = (x as usize).min(size);
            let part = x - at as f64;
            sum += s * (table[at] + (table[at + 1] - table[at]) * part);
        }
        out.push(sum * band);
    }
    out
}

/// The peak brought to `PEAK_DB`; `false` for silence.
fn normalise(sound: &mut [f64]) -> bool {
    let peak = sound.iter().fold(0.0f64, |p, s| p.max(s.abs()));
    if peak <= 1e-9 {
        return false;
    }
    let gain = 10f64.powf(PEAK_DB / 20.0) / peak;
    sound.iter_mut().for_each(|s| *s *= gain);
    true
}

/// `db` of gain into a limiter with no memory, sox's `gain -l`: a
/// quiet sample gets all of the gain and full scale stays where it is.
fn limiter(sound: &mut [f64], db: f64) {
    let g = 10f64.powf(db / 20.0);
    sound.iter_mut().for_each(|s| *s = g * *s / (1.0 + (g - 1.0) * s.abs()));
}

/// The sound as the clip's samples would have it: cut, shaped and at
/// the clip's rate, between -1 and 1.
pub fn shape(mut sound: Vec<f64>, rate: u32, shape: &Shape) -> Result<Vec<f64>, String> {
    if !matches!(shape.rate, 8000 | 16000 | 31250) {
        return Err(format!("rate={}: 8000, 16000 or 31250", shape.rate));
    }
    if shape.passes > 2 {
        return Err("passes is 0, 1 or 2".into());
    }
    if !(0.0..=40.0).contains(&shape.drive) {
        return Err("drive is 0 to 40, the dB pushed into the limiter".into());
    }
    if shape.skip < 0.0 || shape.seconds < 0.0 || !(shape.highpass > 0.0 && shape.highpass < rate as f64 / 2.0) {
        return Err("skip, seconds and highpass are not less than nought".into());
    }
    let skip = (shape.skip * rate as f64) as usize;
    if skip >= sound.len() {
        return Err(format!("skip={}: the file is {:.1} s long", shape.skip, sound.len() as f64 / rate as f64));
    }
    sound.drain(..skip);
    let keep = (shape.seconds * rate as f64) as usize;
    let cut = shape.seconds > 0.0 && keep < sound.len();
    if cut {
        sound.truncate(keep);
    }
    for _ in 0..shape.passes {
        highpass(&mut sound, shape.highpass, rate as f64);
    }
    let mut sound = resample(&sound, rate, shape.rate);
    if !normalise(&mut sound) {
        return Err("nothing but silence".into());
    }
    if shape.drive > 0.0 {
        limiter(&mut sound, shape.drive);
        normalise(&mut sound);
    }
    if cut {
        // The last second and a half faded out.
        let fade = ((FADE_S * shape.rate as f64) as usize).min(sound.len());
        let start = sound.len() - fade;
        for (k, s) in sound[start..].iter_mut().enumerate() {
            *s *= 1.0 - (k + 1) as f64 / fade as f64;
        }
    }
    Ok(sound)
}

/// The clip of the sound file in `file`, and what there is to say of it.
pub fn convert(file: &[u8], how: &Shape, notes: &mut Vec<String>) -> Result<Vec<u8>, String> {
    let (sound, rate) = read(file)?;
    let whole = sound.len() as f64 / rate as f64;
    let sound = shape(sound, rate, how)?;
    let out = if how.adpcm {
        let samples: Vec<i16> = sound.iter().map(|s| (s * 32767.0).round().clamp(-32768.0, 32767.0) as i16).collect();
        adpcm::clip(&samples, how.rate, adpcm::SEARCH)
    } else {
        // Eight bits want dither, or what is quiet turns to a buzz of
        // its own: two random numbers a sample, a step wide together.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let samples = sound.iter().map(|s| (s * 127.0 + random() + random()).round().clamp(-128.0, 127.0) as i8 as u8 ^ 0x80);
        // The header says the rate; a clip without one is at 8000.
        let mut out = if how.rate == 8000 { Vec::new() } else { [&PCM_MAGIC[..], &how.rate.to_le_bytes()].concat() };
        out.extend(samples);
        out
    };
    notes.push(format!(
        "{:.1} s of {whole:.1} at {rate} Hz, as {} at {} Hz: {} bytes, {} a second",
        sound.len() as f64 / how.rate as f64,
        if how.adpcm { "ADPCM" } else { "PCM" },
        how.rate,
        out.len(),
        how.rate / if how.adpcm { 2 } else { 1 }
    ));
    Ok(out)
}

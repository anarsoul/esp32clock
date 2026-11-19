// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A ZX Spectrum tune (.pt3, Pro Tracker 3 or Vortex Tracker II) as a
//! clip for the clock's own player (`onebitdawn_core::pt3`) and its
//! AY-3-8910: the module as it is behind a header that says what the
//! module cannot, the machine it was written on, how long it plays,
//! and how the clock is to shape it.
//!
//! A module does not say what clocked its chip or how often its player
//! ran. A Pentagon has 1 750 000 Hz and 48.83 frames a second, a
//! Spectrum 128 1 773 400 Hz and 50.02; the default goes by the
//! module's tone table, since the ASM and "real" tables were worked out
//! for the first and the Pro Tracker and Sound Tracker ones for the
//! second.

use std::collections::BTreeMap;

use onebitdawn_core::pt3::{KNEES, MAGIC};

use crate::bytes::text;
use crate::shape::section;
use crate::{Options, Output};

const HEADER: usize = 40;
/// The header's flags.
const FINE: u8 = 1;
const AFRESH: u8 = 2;
const FAST: u8 = 4;
const FLAT: u8 = 8;
/// The chip's clock in Hz and its cycles a frame: the processor's are
/// twice both.
const PENTAGON: (u64, u64) = (1_750_000, 71680 / 2);
const SPECTRUM: (u64, u64) = (1_773_400, 70908 / 2);
const TABLES: [&str; 4] = ["Pro Tracker", "Sound Tracker", "ASM", "real"];
// Where things are in a module (`pt3.rs` has the same).
const VERSION: usize = 13;
const TURBO: usize = 98;
const TABLE: usize = 99;
const DELAY: usize = 100;
const POSITIONS: usize = 101;
const LOOP: usize = 102;
const PATTERNS: usize = 103;
const ORDER: usize = 201;
/// A tune this long has no end.
const RUNAWAY: u64 = 3600 * 50;

/// The bytes an effect takes after the row's last code, and its name.
fn effect(code: u8) -> Option<(usize, &'static str)> {
    Some(match code {
        1 => (3, "tone slide"),
        2 => (5, "portamento"),
        3 => (1, "sample offset"),
        4 => (1, "ornament offset"),
        5 => (2, "on/off vibrato"),
        8 => (3, "envelope slide"),
        9 => (1, "tempo"),
        _ => return None,
    })
}

/// The options a module takes.
pub const OPTIONS: &[&str] =
    &["machine", "clock", "chip", "restart", "flat", "noise", "noise-lowpass", "drive", "release", "knee", "rate", "highpass", "passes"];

/// What the header says besides what the module does.
#[derive(Clone, Debug)]
pub struct Shape {
    /// Whose clock and frame rate, if not the tone table's.
    pub pentagon: Option<bool>,
    /// The chip's clock in Hz, if not the machine's.
    pub clock: Option<u64>,
    /// A YM2149, whose envelope has 32 levels where an AY's has 16.
    pub ym: bool,
    /// Played round and round, start over from the top and not from
    /// the module's loop position: for a loop that is only an ending.
    pub restart: bool,
    /// No limiter but a plain gain: `drive` 127 is the three channels
    /// at their top level as full scale.
    pub flat: bool,
    /// A channel with noise in it is mixed at this many sixteenths of
    /// its level.
    pub noise: u8,
    /// The noise's low-pass corner in Hz, 0 for none.
    pub noise_lowpass: f64,
    pub drive: u8,
    pub release: u8,
    /// The level the limiter starts at, as `xmc::Shape`'s; the nearest
    /// of the clip's fifteen steps is taken.
    pub knee: u8,
    pub rate: u32,
    pub highpass: f64,
    pub passes: u8,
}

impl Shape {
    /// The default pack's Synchronization for the buzzer: the limiter,
    /// letting go over 128 ms, the noise at its full level and a
    /// high-pass at 100 Hz (the user, 2026-11-16: "beeper should not
    /// be flat for sure"). For a speaker a gentler limiter, its knee
    /// at 37 and its peaks aimed 1.4 dB over full scale (the user's
    /// ear on test-clock's speaker, the same day).
    pub fn new(output: Output) -> Self {
        let (knee, drive) = match output {
            Output::Buzzer => (0, 0),
            Output::Speaker => (37, 150),
        };
        Shape { pentagon: None, clock: None, ym: false, restart: false, flat: false, noise: 16, noise_lowpass: 0.0, drive, release: 10, knee, rate: 31250, highpass: 100.0, passes: 1 }
    }

    pub fn options(output: Output, options: &mut Options) -> Result<Self, String> {
        let mut shape = Shape::new(output);
        shape.flat = options.flag("flat")?;
        if shape.flat {
            // The chip as it is, 3 dB over three channels at full
            // scale, which clips the tops of a tune's loudest moments.
            (shape.drive, shape.release, shape.knee) = (180, 0, 0);
        }
        shape.pentagon = match options.text("machine")?.as_deref() {
            None => None,
            Some("pentagon") => Some(true),
            Some("spectrum") => Some(false),
            Some(other) => return Err(format!("machine={other}: pentagon or spectrum")),
        };
        shape.clock = options.maybe("clock")?;
        shape.ym = match options.text("chip")?.as_deref() {
            None | Some("ay") => false,
            Some("ym") => true,
            Some(other) => return Err(format!("chip={other}: ay or ym")),
        };
        shape.restart = options.flag("restart")?;
        shape.noise = options.number("noise", shape.noise)?;
        shape.noise_lowpass = options.number("noise-lowpass", shape.noise_lowpass)?;
        shape.drive = options.number("drive", shape.drive)?;
        shape.release = options.number("release", shape.release)?;
        shape.knee = options.number("knee", shape.knee)?;
        shape.rate = options.number("rate", shape.rate)?;
        shape.highpass = options.number("highpass", shape.highpass)?;
        shape.passes = options.number("passes", shape.passes)?;
        Ok(shape)
    }
}

struct Module<'a> {
    d: &'a [u8],
    uses: BTreeMap<&'static str, usize>,
}

impl Module<'_> {
    fn byte(&self, at: usize) -> u8 {
        self.d.get(at).copied().unwrap_or(0)
    }

    fn word(&self, at: usize) -> usize {
        self.byte(at) as usize | (self.byte(at + 1) as usize) << 8
    }

    /// One row of a channel's pattern from `at`, as the player reads
    /// it: where the next begins, the rows it lasts, the tempo after.
    fn row(&mut self, mut at: usize, mut rows: u8, mut delay: u8) -> (usize, u8, u8) {
        let mut effects: Vec<u8> = Vec::new();
        while at < self.d.len() {
            let code = self.d[at];
            at += 1;
            match code {
                0xD0 | 0xC0 | 0x50..=0xAF => break,
                0xF0..=0xFF | 0x10 => at += 1,
                0xB2..=0xBF => {
                    at += 2;
                    *self.uses.entry("the chip's envelope").or_default() += 1;
                }
                0xB1 => {
                    rows = self.byte(at);
                    at += 1;
                }
                0x11..=0x1F => {
                    at += 3;
                    *self.uses.entry("the chip's envelope").or_default() += 1;
                }
                _ if effect(code).is_some() => {
                    // One named twice counts where it was named last.
                    effects.retain(|&e| e != code);
                    effects.push(code);
                }
                _ => {}
            }
        }
        for &code in effects.iter().rev() {
            let (bytes, name) = effect(code).unwrap_or((0, ""));
            *self.uses.entry(name).or_default() += 1;
            if code == 9 {
                delay = self.byte(at);
            }
            at += bytes;
        }
        (at, rows, delay)
    }

    /// The frames each position begins at, and the frames in all.
    fn walk(&mut self) -> Result<(Vec<u64>, u64), String> {
        let mut delay = self.d[DELAY];
        let (mut rows, mut left, mut frames, mut starts) = ([0u8; 3], [1u8; 3], 0u64, Vec::new());
        for position in 0..self.d[POSITIONS] as usize {
            starts.push(frames);
            let pattern = self.word(PATTERNS) + 2 * self.byte(ORDER + position) as usize;
            let mut at = [self.word(pattern), self.word(pattern + 2), self.word(pattern + 4)];
            'position: loop {
                for c in 0..3 {
                    left[c] = left[c].wrapping_sub(1);
                    if left[c] != 0 {
                        continue;
                    }
                    // The first channel's pattern ends in a nought, and
                    // the position with it; its row is the next one's.
                    if c == 0 && self.byte(at[0]) == 0 {
                        left[0] = 1;
                        break 'position;
                    }
                    (at[c], rows[c], delay) = self.row(at[c], rows[c], delay);
                    left[c] = rows[c];
                }
                frames += if delay == 0 { 256 } else { delay as u64 };
                if frames > RUNAWAY {
                    return Err("a pattern that does not end; not a module the player can play".into());
                }
            }
        }
        Ok((starts, frames))
    }
}

/// The clip of the module in `d`, and what there is to say of it.
pub fn convert(d: &[u8], shape: &Shape, notes: &mut Vec<String>) -> Result<Vec<u8>, String> {
    if shape.noise == 0 || shape.noise > 16 {
        return Err("noise is 1 to 16".into());
    }
    if shape.passes > 2 {
        return Err("passes is 0, 1 or 2".into());
    }
    if shape.passes == 0 && !shape.flat {
        return Err("passes=0 is for a clip with flat: the limiter wants its high-pass".into());
    }
    if shape.knee != 0 && shape.flat {
        return Err("knee is the limiter's: not with flat".into());
    }
    if shape.release > 15 {
        return Err("release is 1 to 15".into());
    }
    if shape.clock.is_some_and(|c| !(500_000..=4_000_000).contains(&c)) {
        return Err("clock is 500000 to 4000000".into());
    }
    if !matches!(shape.rate, 16000 | 31250) {
        return Err(format!("rate={}: 16000 or 31250", shape.rate));
    }
    // The step whose knee is nearest in dB.
    let off = |k: usize| (KNEES[k] as f64 / shape.knee as f64).ln().abs();
    let step = if shape.knee != 0 { (1..16).fold(1, |best, k| if off(k) < off(best) { k } else { best }) } else { 0 };
    let section = section(shape.highpass, shape.rate)?;
    // The noise's low-pass, one pole: the part of the way it moves a sample.
    let lowpass = if shape.noise_lowpass != 0.0 {
        (256.0 * (1.0 - (-2.0 * core::f64::consts::PI * shape.noise_lowpass / shape.rate as f64).exp())).round_ties_even().clamp(1.0, 255.0) as u8
    } else {
        0
    };

    if d.len() <= ORDER || !(d.starts_with(b"ProTracker 3.") || d.starts_with(b"Vortex Tracker II")) {
        return Err("not a Pro Tracker 3 module".into());
    }
    let digit = d[VERSION] as char;
    if (d[TURBO] != 0x20 && matches!(digit, '7' | '9')) || d.ends_with(b"02TS") {
        return Err("a TurboSound module, for two chips; the clock has one".into());
    }
    let (positions, looped) = (d[POSITIONS] as usize, d[LOOP] as usize);
    if positions == 0 {
        return Err("no positions".into());
    }
    let mut module = Module { d, uses: BTreeMap::new() };
    let (starts, frames) = module.walk()?;
    if frames == 0 {
        return Err("nothing to play".into());
    }
    let table = (d[TABLE] as usize).min(3);
    let pentagon = shape.pentagon.unwrap_or(table >= 2);
    let (machine_clock, machine_cycles) = if pentagon { PENTAGON } else { SPECTRUM };
    let clock = shape.clock.unwrap_or(machine_clock);
    let loop_frame = if looped < positions && !shape.restart { starts[looped] } else { 0 };
    let ms = ((frames * machine_cycles * 1000) as f64 / machine_clock as f64).round_ties_even() as u32;
    // The frame is the machine's, in the chip's cycles as its clock is given.
    let cycles = ((machine_cycles * clock) as f64 / machine_clock as f64).round_ties_even() as u32;

    let fast = shape.rate == 31250;
    let flags = if shape.ym { FINE } else { 0 } | if shape.restart { AFRESH } else { 0 } | if fast { FAST } else { 0 } | if shape.flat { FLAT } else { 0 } | (step as u8) << 4;
    let mut out = MAGIC.to_vec();
    out.extend([flags, shape.drive, shape.release, shape.passes]);
    for n in [clock as u32, ms, cycles, frames as u32, loop_frame as u32] {
        out.extend(n.to_le_bytes());
    }
    for k in section {
        out.extend(k.to_le_bytes());
    }
    out.extend([shape.noise % 16, lowpass]);
    debug_assert_eq!(out.len(), HEADER);
    out.extend(d);

    let version = if digit.is_ascii_digit() { format!("Pro Tracker 3.{digit}") } else { "Vortex Tracker II".into() };
    notes.push(format!(
        "\"{}\" by {}, {version}, the {} tone table, {positions} positions",
        text(&d[30..62]).trim(),
        text(&d[66..98]).trim(),
        TABLES[table]
    ));
    notes.push(format!(
        "{}: {} at {clock} Hz, {:.2} frames a second",
        if pentagon { "pentagon" } else { "spectrum" },
        if shape.ym { "YM2149" } else { "AY-3-8910" },
        clock as f64 / cycles as f64
    ));
    let seconds = |f: u64| f as f64 * ms as f64 / frames as f64 / 1000.0;
    let round = if shape.restart {
        "starts over from the top".to_string()
    } else {
        format!("goes round from {:.1} s (position {looped} of {positions}, {:.1} s of it)", seconds(loop_frame), seconds(frames - loop_frame))
    };
    notes.push(format!("{:.1} s, {frames} frames, {round}; {} bytes", ms as f64 / 1000.0, out.len()));
    if !module.uses.is_empty() {
        let uses: Vec<String> = module.uses.iter().map(|(what, count)| format!("{what} ({count})")).collect();
        notes.push(format!("uses: {}", uses.join(", ")));
    }
    Ok(out)
}

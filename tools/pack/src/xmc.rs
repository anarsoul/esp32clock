// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A FastTracker 2 module (.xm) as a clip for the clock's own player
//! (`onebitdawn_core::xm`): the module rearranged for a player with no
//! memory to unpack into. Tables of where each pattern and each
//! instrument's sample is, the patterns as they are, the samples as
//! plain signed bytes (an XM stores differences), 16-bit ones cut to
//! 8, ping-pong loops unrolled. One sample an instrument, the first.
//!
//! The player cuts the bass, which the buzzer does not have, and
//! limits hard; how is the clip's to say (`Shape`).

use std::collections::BTreeMap;

use onebitdawn_core::xm::{MAGIC, MAX_CHANNELS};

use crate::bytes::{Data, text};
use crate::shape::{rate_flag, section};
use crate::{Options, Output};

const ENVELOPE_POINTS: usize = 12;
/// What `xm.rs` plays: effects by number (E's by its first digit too)
/// and the volume column by its high nibble.
const EFFECTS: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 9, 0xA, 0xB, 0xC, 0xD, 0xE, 0xF, 0x10, 0x14];
const E_EFFECTS: [u8; 7] = [1, 2, 9, 0xA, 0xB, 0xC, 0xD];
const VOLUMES: [u8; 13] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0xA, 0xB, 0xF];
/// Panning does nothing on one buzzer; not worth a note.
const SILENT: [u8; 3] = [8, 0x19, 0x1D];
const SILENT_VOLUMES: [u8; 3] = [0xC, 0xD, 0xE];

/// The options a module takes.
pub const OPTIONS: &[&str] = &["highpass", "passes", "drive", "release", "knee", "rate", "bits", "raise"];

/// How the player is to shape the mix.
#[derive(Clone, Debug)]
pub struct Shape {
    /// What the limiter aims a peak at, full scale being 127; 0 is the
    /// player's own.
    pub drive: u8,
    /// The limiter lets go by 1/2^N a sample at 8 kHz; 0 is the
    /// player's own, 7.
    pub release: u8,
    /// The level the limiter starts at, in 1024s of the mix where one
    /// voice at full level is 32; 0 is the player's own.
    pub knee: u8,
    /// The high-pass's corner in Hz, and how many times it is applied,
    /// 12 dB an octave each.
    pub highpass: f64,
    pub passes: u8,
    /// Samples a second the player makes of it.
    pub rate: u32,
    /// The width of the samples the player hands the modulator.
    pub bits: u8,
    /// Instruments played octaves higher, by number from 1.
    pub raised: BTreeMap<usize, i32>,
}

impl Shape {
    /// The default pack's for the buzzer: 300 Hz once, which is what
    /// the user's ear chose for two chiptunes over the PCM clips'
    /// 900 Hz twice. For a speaker the bass is left in and the limiter
    /// lets go over 128 ms and not 16, so its gain follows the tune's
    /// level and not its waveform (`scripts/speaker-clips.sh`, heard
    /// 2026-11-15).
    pub fn new(output: Output) -> Self {
        let (highpass, release) = match output {
            Output::Buzzer => (300.0, 0),
            Output::Speaker => (100.0, 10),
        };
        Shape { drive: 0, release, knee: 0, highpass, passes: 1, rate: 31250, bits: 16, raised: BTreeMap::new() }
    }

    pub fn options(output: Output, options: &mut Options) -> Result<Self, String> {
        let mut shape = Shape::new(output);
        shape.drive = options.number("drive", shape.drive)?;
        shape.release = options.number("release", shape.release)?;
        shape.knee = options.number("knee", shape.knee)?;
        shape.highpass = options.number("highpass", shape.highpass)?;
        shape.passes = options.number("passes", shape.passes)?;
        shape.rate = options.number("rate", shape.rate)?;
        shape.bits = options.number("bits", shape.bits)?;
        while let Some(item) = options.text("raise")? {
            let parsed = item.split_once(':').and_then(|(i, o)| Some((i.parse().ok()?, o.parse().ok()?)));
            let (instrument, octaves) = parsed.ok_or_else(|| format!("raise={item}: raise=INSTRUMENT:OCTAVES"))?;
            shape.raised.insert(instrument, octaves);
        }
        if shape.release > 15 {
            return Err("release is 1 to 15".into());
        }
        if shape.passes > 2 {
            return Err("passes is 0, 1 or 2".into());
        }
        if !matches!(shape.bits, 8 | 16) {
            return Err("bits is 8 or 16".into());
        }
        Ok(shape)
    }
}

/// The cells of a pattern's packed data, a row at a time: note,
/// instrument, volume, effect, parameter.
fn rows(data: &[u8], channels: usize) -> Vec<Vec<[u8; 5]>> {
    let (mut q, mut row, mut out) = (0, Vec::with_capacity(channels), Vec::new());
    let at = |q: usize| data.get(q).copied().unwrap_or(0);
    while q < data.len() {
        let b = data[q];
        q += 1;
        let mut cell = [0u8; 5];
        if b & 0x80 != 0 {
            for (i, c) in cell.iter_mut().enumerate() {
                if b & (1 << i) != 0 {
                    *c = at(q);
                    q += 1;
                }
            }
        } else {
            cell = [b, at(q), at(q + 1), at(q + 2), at(q + 3)];
            q += 4;
        }
        row.push(cell);
        if row.len() == channels {
            out.push(core::mem::replace(&mut row, Vec::with_capacity(channels)));
        }
    }
    out
}

fn effect_name(effect: u8, param: u8) -> String {
    let letter = if effect < 16 { format!("{effect:X}") } else { ((b'G' + effect - 16) as char).to_string() };
    if effect == 0xE { format!("{letter}{:X}", param >> 4) } else { letter }
}

#[derive(Default)]
struct Instrument {
    sample: Vec<u8>,
    looped: (usize, usize),
    volume: u8,
    finetune: i8,
    relative: i8,
    flags: u8,
    points: Vec<(u16, u16)>,
    sustain: u8,
    env_loop: (u8, u8),
    fade: u16,
    auto: [u8; 3],
}

/// The clip of the module in `module`, and what there is to say of it.
pub fn convert(module: &[u8], shape: &Shape, notes: &mut Vec<String>) -> Result<Vec<u8>, String> {
    let d = Data(module);
    if !module.starts_with(b"Extended Module: ") {
        return Err("not an XM module".into());
    }
    let section = section(shape.highpass, shape.rate)?;
    let rate = rate_flag(shape.rate)?;
    let header = d.size(60)?;
    let orders_n = d.u16(64)? as usize;
    let restart = d.u16(66)? as usize;
    let channels = d.u16(68)? as usize;
    let patterns_n = d.u16(70)? as usize;
    let instruments_n = d.u16(72)? as usize;
    let flags = d.u16(74)?;
    let (speed, tempo) = (d.u16(76)?, d.u16(78)?);
    if channels > MAX_CHANNELS {
        return Err(format!("{channels} channels; the player has {MAX_CHANNELS}"));
    }
    if channels == 0 {
        return Err("no channels".into());
    }
    if flags & 1 == 0 {
        return Err("the Amiga frequency table; the player has the linear one only".into());
    }
    let orders = d.slice(80, 80 + orders_n);
    let mut warnings: BTreeMap<String, usize> = BTreeMap::new();
    let mut warn = |what: String| *warnings.entry(what).or_default() += 1;

    let mut at = 60usize.checked_add(header).ok_or(crate::bytes::SHORT)?;
    let mut patterns: Vec<(usize, Vec<u8>)> = Vec::new();
    for _ in 0..patterns_n {
        let length = d.size(at)?;
        let rows = d.u16(at + 5)? as usize;
        let size = d.u16(at + 7)? as usize;
        let start = at.saturating_add(length);
        // A pattern with no data is all empty rows.
        patterns.push((rows, if size > 0 { d.slice(start, start + size).to_vec() } else { vec![0x80; rows * channels] }));
        at = start + size;
    }
    // An order may name a pattern the file does not have: 64 empty rows.
    if let Some(&most) = orders.iter().max() {
        while most as usize >= patterns.len() {
            patterns.push((64, vec![0x80; 64 * channels]));
        }
    }
    let cells: Vec<Vec<Vec<[u8; 5]>>> = patterns.iter().map(|(_, packed)| rows(packed, channels)).collect();

    for pattern in &cells {
        for row in pattern {
            for &[_note, _ins, volume, effect, param] in row {
                if (effect != 0 || param != 0) && !SILENT.contains(&effect) && (!EFFECTS.contains(&effect) || (effect == 0xE && !E_EFFECTS.contains(&(param >> 4)))) {
                    warn(format!("effect {}", effect_name(effect, param)));
                }
                if !VOLUMES.contains(&(volume >> 4)) && !SILENT_VOLUMES.contains(&(volume >> 4)) {
                    warn(format!("volume column {:X}x", volume >> 4));
                }
            }
        }
    }

    let mut instruments = Vec::new();
    for number in 1..=instruments_n {
        let size = d.size(at)?;
        let samples = d.u16(at + 27)? as usize;
        let mut entry = Instrument::default();
        if samples > 0 {
            let sample_header = d.size(at + 29)?;
            let points = (d.u8(at + 225)? as usize).min(ENVELOPE_POINTS);
            for i in 0..points {
                entry.points.push((d.u16(at + 129 + 4 * i)?, d.u16(at + 131 + 4 * i)?));
            }
            entry.sustain = d.u8(at + 227)?;
            entry.env_loop = (d.u8(at + 228)?, d.u8(at + 229)?);
            entry.flags = if points > 0 { d.u8(at + 233)? & 7 } else { 0 };
            entry.fade = d.u16(at + 239)?;
            // Auto-vibrato: sweep, depth, rate. The player's is a sine.
            entry.auto = [d.u8(at + 236)?, d.u8(at + 237)?, d.u8(at + 238)?];
            let waveform = d.u8(at + 235)?;
            if entry.auto[1] != 0 && waveform != 0 {
                warn(format!("instrument {number}: auto-vibrato waveform {waveform}, played as a sine"));
            }
            if samples > 1 {
                warn(format!("instrument {number}: {samples} samples, the first is used"));
            }
            let head = at.saturating_add(size);
            let body = sample_header.checked_mul(samples).and_then(|n| n.checked_add(head)).ok_or(crate::bytes::SHORT)?;
            let length = d.size(head)?;
            let (mut loop_start, mut loop_len) = (d.size(head + 4)?, d.size(head + 8)?);
            let volume = d.u8(head + 12)?;
            let finetune = d.i8(head + 13)?;
            let kind = d.u8(head + 14)?;
            let relative = d.i8(head + 16)?;
            let raw = d.slice(body, body.saturating_add(length));
            let mut values: Vec<u8> = Vec::with_capacity(raw.len());
            if kind & 0x10 != 0 {
                let mut acc = 0u16;
                for pair in raw.as_chunks::<2>().0 {
                    acc = acc.wrapping_add(u16::from_le_bytes(*pair));
                    values.push((acc >> 8) as u8);
                }
                loop_start /= 2;
                loop_len /= 2;
            } else {
                let mut acc = 0u8;
                for &delta in raw {
                    acc = acc.wrapping_add(delta);
                    values.push(acc);
                }
            }
            let looping = kind & 3;
            if looping == 0 || loop_len == 0 {
                loop_start = 0;
                loop_len = 0;
            } else {
                let start = loop_start.min(values.len());
                let end = loop_start.saturating_add(loop_len).min(values.len());
                if looping == 2 {
                    // Ping-pong: there and back is one forward loop.
                    let back: Vec<u8> = values[start..end].iter().rev().copied().collect();
                    values.truncate(end);
                    values.extend(back);
                    loop_len = loop_len.saturating_mul(2);
                } else {
                    values.truncate(end);
                }
            }
            let up = 12 * shape.raised.get(&number).copied().unwrap_or(0);
            entry.relative = (relative as i32 + up).clamp(-128, 127) as i8;
            entry.sample = values;
            entry.looped = (loop_start, loop_len);
            entry.volume = volume;
            entry.finetune = finetune;
            at = body;
            for i in 0..samples {
                at = at.saturating_add(d.size(head + sample_header * i)?);
            }
        } else {
            at = at.saturating_add(size);
        }
        instruments.push(entry);
    }

    // The length, played once as the player does: to the end of the
    // order list or to a jump back.
    let (mut ticks_ms, mut order, mut row, mut now_speed, mut now_tempo) = (0.0f64, 0usize, 0usize, speed as f64, tempo as f64);
    while order < orders.len() {
        let mut go: Option<(usize, usize)> = None;
        for cells_row in cells[orders[order] as usize].iter().skip(row) {
            for &[_note, _ins, _volume, effect, param] in cells_row {
                if effect == 0xF && param != 0 {
                    if param >= 0x20 {
                        now_tempo = param as f64;
                    } else {
                        now_speed = param as f64;
                    }
                } else if effect == 0xB {
                    go = Some((param as usize, 0));
                } else if effect == 0xD {
                    go = Some((go.map_or(order + 1, |g| g.0), (param >> 4) as usize * 10 + (param & 0xF) as usize));
                }
            }
            ticks_ms += now_speed * 2500.0 / now_tempo;
            if go.is_some() {
                break;
            }
        }
        if go.is_some_and(|g| g.0 <= order) {
            break;
        }
        (order, row) = go.unwrap_or((order + 1, 0));
    }

    let byte = |n: usize, what: &str| u8::try_from(n).map_err(|_| format!("{n} {what}; the clip has room for 255"));
    let mut out = MAGIC.to_vec();
    out.extend([
        channels as u8,
        byte(speed as usize, "ticks a row")?,
        byte(tempo as usize, "beats a minute")?,
        byte(orders.len(), "orders")?,
        byte(patterns.len(), "patterns")?,
        byte(instruments.len(), "instruments")?,
        shape.drive,
        shape.release,
    ]);
    out.extend((ticks_ms.round_ties_even() as u32).to_le_bytes());
    out.extend([shape.passes, rate | if shape.bits == 16 { 2 } else { 0 }]);
    for k in section {
        out.extend(k.to_le_bytes());
    }
    // The order to start over from, for the alarm, which plays the tune
    // round and round.
    out.extend([if restart < orders.len() { restart as u8 } else { 0 }, shape.knee, 0, 0]);
    out.extend(orders);
    let tables = out.len() + 6 * patterns.len() + 80 * instruments.len();
    let mut body: Vec<u8> = Vec::new();
    for (rows, packed) in &patterns {
        out.extend((*rows as u16).to_le_bytes());
        out.extend(((tables + body.len()) as u32).to_le_bytes());
        body.extend(packed);
    }
    for i in &instruments {
        for n in [tables + body.len(), i.sample.len(), i.looped.0, i.looped.1] {
            out.extend(u32::try_from(n).map_err(|_| "a sample's loop is past what a clip can say")?.to_le_bytes());
        }
        out.extend([i.volume, i.finetune as u8, i.relative as u8, i.flags, i.points.len() as u8, i.sustain, i.env_loop.0, i.env_loop.1]);
        out.extend(i.fade.to_le_bytes());
        out.extend(i.auto);
        out.extend([0; 3]);
        for p in 0..ENVELOPE_POINTS {
            let (x, y) = i.points.get(p).copied().unwrap_or((0, 0));
            out.extend(x.to_le_bytes());
            out.extend(y.to_le_bytes());
        }
        body.extend(&i.sample);
    }
    debug_assert_eq!(out.len(), tables);
    out.extend(body);

    let name = text(d.slice(17, 37));
    notes.push(format!(
        "\"{}\", {channels} channels, {} orders, {:.1} s, {} bytes (the module is {})",
        name.trim(),
        orders.len(),
        ticks_ms / 1000.0,
        out.len(),
        module.len()
    ));
    for (what, count) in &warnings {
        notes.push(format!("not played: {what} ({count} times)"));
    }
    Ok(out)
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A ProTracker module (.mod) as a FastTracker 2 one, which `xmc`
//! takes.
//!
//! A sample becomes an instrument of one sample under its own name,
//! the finetune kept; a period becomes the nearest note, 428 being C-4
//! and the pitch a PAL Amiga's, 16 cents under FT2's. The module is
//! written on the linear frequency table, which is what
//! `onebitdawn_core::xm` plays, so the slides, which a MOD counts in
//! periods, are converted at the period the channel has when each is
//! played, the song followed once in its order: 1, 2, 3 and 5, and E1
//! and E2. The effects 0, 4, 6, 7, 9, A, B, C, D, F and the other E's
//! are the same in both; E0, the Amiga's filter, is left out and named.
//! Not converted: the panning, a module being mono on the clock, and a
//! pattern loop's or a delay's part in the order the slides are read in.

use std::collections::{HashMap, HashSet};

use crate::bytes::Data;
use crate::kind::mod_channels;

/// What both converters to XM write alike.
pub(crate) mod xm {
    /// The file's head: the title, the counts, the order list. The
    /// linear table, flags 1: it is what `xm.rs` plays.
    #[allow(clippy::too_many_arguments)]
    pub fn header(title: &[u8], orders: &[u8], restart: u16, channels: usize, patterns: usize, instruments: usize, speed: u16, tempo: u16) -> Vec<u8> {
        let mut out = b"Extended Module: ".to_vec();
        out.extend(padded(title, 20));
        out.push(0x1a);
        out.extend(padded(b"onebitdawn-pack", 20));
        out.extend(0x0104u16.to_le_bytes());
        out.extend(276u32.to_le_bytes());
        for n in [orders.len() as u16, restart, channels as u16, patterns as u16, instruments as u16, 1, speed, tempo] {
            out.extend(n.to_le_bytes());
        }
        out.extend(orders);
        out.resize(out.len() + 256 - orders.len().min(256), 0);
        out
    }

    /// A field of `len` bytes, the text and then spaces.
    pub fn padded(text: &[u8], len: usize) -> Vec<u8> {
        let mut out = text[..text.len().min(len)].to_vec();
        out.resize(len, b' ');
        out
    }

    /// A pattern of 64 rows, its cells (note, instrument, volume,
    /// effect, parameter) packed.
    pub fn pattern(cells: impl Iterator<Item = [u8; 5]>) -> Vec<u8> {
        let mut data = Vec::new();
        for v in cells {
            let mask = (0..5).filter(|&i| v[i] != 0).fold(0u8, |m, i| m | 1 << i);
            if mask == 31 {
                data.extend(v);
            } else {
                data.push(0x80 | mask);
                data.extend(v.iter().filter(|&&x| x != 0));
            }
        }
        let mut out = 9u32.to_le_bytes().to_vec();
        out.push(0);
        out.extend(64u16.to_le_bytes());
        out.extend((data.len() as u16).to_le_bytes());
        out.extend(data);
        out
    }

    /// An instrument with no sample.
    pub fn silent(name: &[u8]) -> Vec<u8> {
        let mut out = 29u32.to_le_bytes().to_vec();
        out.extend(padded(name, 22));
        out.extend([0, 0, 0]);
        out
    }

    /// An instrument of one 8-bit sample, signed, which an XM stores
    /// as differences.
    pub fn instrument(name: &[u8], pcm: &[u8], looped: Option<(usize, usize)>, volume: u8, finetune: i8, relative: i8) -> Vec<u8> {
        let mut out = 263u32.to_le_bytes().to_vec();
        out.extend(padded(name, 22));
        out.extend([0, 1, 0]);
        out.extend(40u32.to_le_bytes());
        out.extend([0u8; 96 + 96 + 2 + 6 + 2 + 4 + 2 + 22]);
        let (start, len) = looped.unwrap_or((0, 0));
        for n in [pcm.len(), start, len] {
            out.extend((n as u32).to_le_bytes());
        }
        out.extend([volume.min(64), finetune as u8, looped.is_some() as u8, 128, relative as u8, 0]);
        out.extend(padded(name, 22));
        let mut prev = 0u8;
        out.extend(pcm.iter().map(|&b| {
            let delta = b.wrapping_sub(prev);
            prev = b;
            delta
        }));
        out
    }
}

/// A slide from one period to another over that many ticks, in
/// sixteenths of a semitone a tick.
fn linear(period: i32, to: i32, ticks: i32) -> u8 {
    if ticks <= 0 || period == to {
        return 0;
    }
    let step = ((12.0 * 16.0 * (period as f64 / to as f64).log2()).abs() / ticks as f64).round_ties_even();
    step.clamp(1.0, 255.0) as u8
}

/// The XM of the module in `module`.
pub fn convert(module: &[u8], notes: &mut Vec<String>) -> Result<Vec<u8>, String> {
    let d = Data(module);
    let nch = mod_channels(module).ok_or("not a module of 31 samples")?;
    let title = d.slice(0, 20).split(|&b| b == 0).next().unwrap_or(&[]);
    let orders = d.slice(952, 952 + d.u8(950)? as usize);
    let patn = d.bytes(952, 128)?.iter().max().map_or(0, |&m| m as usize) + 1;

    // The patterns as [pattern][row][channel] = period, instrument,
    // effect, parameter.
    let cell = |p: usize, r: usize, c: usize| -> [i32; 4] {
        let at = 1084 + ((p * 64 + r) * nch + c) * 4;
        let (a, b, e, par) = (module[at] as i32, module[at + 1] as i32, module[at + 2] as i32, module[at + 3] as i32);
        [(a & 15) << 8 | b, (a & 0xf0) | e >> 4, e & 15, par]
    };
    let samples_at = 1084 + patn * 64 * nch * 4;
    if module.len() < samples_at {
        return Err(crate::bytes::SHORT.into());
    }

    // The slides, in sixteenths of a semitone a tick for periods a
    // tick, at the period the channel is at; a pattern played again
    // keeps what its first time gave it.
    let mut speed = 6i32;
    let (mut period, mut target, mut porta) = (vec![0i32; nch], vec![0i32; nch], vec![0i32; nch]);
    let mut seen = HashSet::new();
    let mut fixed: HashMap<(usize, usize, usize), u8> = HashMap::new();
    let (mut o, mut row0) = (0usize, 0usize);
    let mut visited = HashSet::new();
    while o < orders.len() && visited.insert((o, row0)) {
        let p = orders[o] as usize;
        let first = seen.insert(p);
        let mut jump: Option<(usize, usize)> = None;
        for r in row0..64 {
            for c in 0..nch {
                let [_, _, fx, par] = cell(p, r, c);
                if fx == 0xF && 0 < par && par < 0x20 {
                    speed = par;
                }
            }
            for c in 0..nch {
                let [per, _, fx, par] = cell(p, r, c);
                let ticks = speed - 1;
                let mut new = None;
                if fx == 3 || fx == 5 {
                    if per != 0 {
                        target[c] = per;
                    }
                    if fx == 3 && par != 0 {
                        porta[c] = par;
                    }
                    if period[c] != 0 && target[c] != 0 {
                        let step = porta[c] * ticks;
                        let to = if target[c] < period[c] { target[c].max(period[c] - step) } else { target[c].min(period[c] + step) };
                        if fx == 3 {
                            new = Some(if par != 0 { linear(period[c], to, ticks) } else { 0 });
                        }
                        period[c] = to;
                    }
                } else {
                    if per != 0 {
                        period[c] = per;
                        target[c] = per;
                    }
                    if (fx == 1 || fx == 2) && period[c] != 0 {
                        let to = if fx == 1 { period[c] - par * ticks } else { period[c] + par * ticks };
                        let to = to.clamp(113, 856);
                        new = Some(linear(period[c], to, ticks));
                        period[c] = to;
                    } else if fx == 0xE && matches!(par >> 4, 1 | 2) && period[c] != 0 {
                        let to = if par >> 4 == 1 { period[c] - (par & 15) } else { period[c] + (par & 15) };
                        let to = to.clamp(113, 856);
                        new = Some((par & 0xf0) as u8 | linear(period[c], to, 1).min(15));
                        period[c] = to;
                    }
                }
                if let (Some(new), true) = (new, first) {
                    fixed.insert((p, r, c), new);
                }
                if fx == 0xB {
                    jump = Some((par as usize, 0));
                } else if fx == 0xD {
                    jump = Some((jump.map_or(o + 1, |j| j.0), (par >> 4) as usize * 10 + (par & 15) as usize));
                }
            }
            if jump.is_some() {
                break;
            }
        }
        (o, row0) = jump.unwrap_or((o + 1, 0));
        if row0 > 63 {
            row0 = 0;
        }
    }

    // FT2 plays C-4 at 8363 Hz, a PAL Amiga period 428 at
    // 7093789.2 / 856: 16 cents under, in the finetune's 128ths of a
    // semitone.
    let pal = (12.0 * 128.0 * (7093789.2f64 / 856.0 / 8363.0).log2()).round_ties_even() as i32;

    let mut filter = false;
    let restart = d.u8(951)?;
    let restart = if (restart as usize) < orders.len() { restart as u16 } else { 0 };
    let mut out = xm::header(title, orders, restart, nch, patn, 31, 6, 125);
    for p in 0..patn {
        let cells: Vec<[u8; 5]> = (0..64 * nch)
            .map(|k| {
                let (r, c) = (k / nch, k % nch);
                let [per, ins, mut fx, mut par] = cell(p, r, c);
                let note = if per != 0 { (12.0 * (428.0 / per as f64).log2()).round_ties_even() as i32 + 49 } else { 0 };
                if let Some(&new) = fixed.get(&(p, r, c)) {
                    par = new as i32;
                }
                if fx == 0xE && par >> 4 == 0 {
                    filter = true;
                    fx = 0;
                    par = 0;
                } else if fx == 0xC {
                    par = par.min(64);
                } else if fx == 0xF && par == 0 {
                    // F00 stops a MOD; FT2 has no such.
                    fx = 0;
                }
                [note.clamp(0, 255) as u8, ins as u8, 0, fx as u8, par as u8]
            })
            .collect();
        out.extend(xm::pattern(cells.into_iter()));
    }

    let mut at = samples_at;
    for i in 0..31 {
        let h = 20 + 30 * i;
        let name = d.slice(h, h + 22).split(|&b| b == 0).next().unwrap_or(&[]);
        let (ln, fine, vol) = (d.u16be(h + 22)? as usize * 2, d.u8(h + 24)?, d.u8(h + 25)?);
        let (lb, ll) = (d.u16be(h + 26)? as usize * 2, d.u16be(h + 28)? as usize * 2);
        let mut pcm = d.slice(at, at + ln);
        at += ln;
        if pcm.len() < 4 {
            out.extend(xm::silent(name));
            continue;
        }
        let looped = ll > 2 && lb < pcm.len();
        if looped {
            pcm = &pcm[..(lb + ll).min(pcm.len())];
        }
        // A signed nibble, eighths of a semitone.
        let fine = ((fine & 15) ^ 8) as i32 - 8;
        let finetune = (fine * 16 + pal).clamp(-128, 127) as i8;
        out.extend(xm::instrument(name, pcm, looped.then(|| (lb, pcm.len() - lb)), vol, finetune, 0));
    }
    notes.push(format!("a ProTracker module of {nch} channels, {} slides converted", fixed.len()));
    if filter {
        notes.push("not converted: the effect E0, the Amiga's filter".into());
    }
    Ok(out)
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A ScreamTracker 3 module (.s3m) as a FastTracker 2 one, which `xmc`
//! takes.
//!
//! A sample becomes an instrument of one sample, its C2Spd the
//! relative note and the finetune; the module is written on the linear
//! frequency table, which is what `onebitdawn_core::xm` plays. The
//! effects converted are A, B, C, D, O and T (D00's memory resolved in
//! playing order, since FT2 keeps the fine slides' apart); any other is
//! left out and named. Not converted: AdLib instruments, 16-bit, stereo
//! and packed samples, fast volume slides (refused), and the panning, a
//! module being mono on the clock.

use std::collections::{BTreeSet, HashSet};

use crate::bytes::Data;
use crate::modxm::xm;

/// The XM of the module in `module`.
pub fn convert(module: &[u8], notes: &mut Vec<String>) -> Result<Vec<u8>, String> {
    let d = Data(module);
    if d.slice(0x2c, 0x30) != b"SCRM" {
        return Err("not an S3M module".into());
    }
    let title = d.slice(0, 28).split(|&b| b == 0).next().unwrap_or(&[]);
    let (ordn, insn, patn) = (d.u16(0x20)? as usize, d.u16(0x22)? as usize, d.u16(0x24)? as usize);
    let (flags, cwt, ffi) = (d.u16(0x26)?, d.u16(0x28)?, d.u16(0x2a)?);
    let (speed, tempo) = (d.u8(0x31)?, d.u8(0x32)?);
    // PCM channels only, an even number of them.
    let last = d.bytes(0x40, 32)?.iter().rposition(|&c| c < 16).ok_or("no channels")?;
    let nch = last + 1 + (last + 1) % 2;
    let orders: Vec<u8> = d.slice(0x60, 0x60 + ordn).iter().copied().filter(|&o| o < 254).collect();
    if cwt == 0x1300 || flags & 0x40 != 0 {
        return Err("fast volume slides: not handled".into());
    }

    // The patterns as [pattern][row][channel] = note, instrument,
    // volume, effect, parameter, in S3M's terms.
    const EMPTY: [u8; 5] = [255, 0, 255, 0, 0];
    let mut pats = Vec::with_capacity(patn);
    for k in 0..patn {
        let p = d.u16(0x60 + ordn + insn * 2 + k * 2)? as usize;
        let mut rows = vec![vec![EMPTY; nch]; 64];
        if p != 0 {
            let (mut q, mut r) = (p * 16 + 2, 0);
            while r < 64 {
                let b = d.u8(q)?;
                q += 1;
                if b == 0 {
                    r += 1;
                    continue;
                }
                let c = (b & 31) as usize;
                let mut cell = EMPTY;
                if b & 32 != 0 {
                    (cell[0], cell[1]) = (d.u8(q)?, d.u8(q + 1)?);
                    q += 2;
                }
                if b & 64 != 0 {
                    cell[2] = d.u8(q)?;
                    q += 1;
                }
                if b & 128 != 0 {
                    (cell[3], cell[4]) = (d.u8(q)?, d.u8(q + 1)?);
                    q += 2;
                }
                if c < nch {
                    rows[r][c] = cell;
                }
            }
        }
        pats.push(rows);
    }

    // D00 repeats the channel's last slide: resolve it in playing order.
    let mut mem = vec![0u8; nch];
    let mut seen = HashSet::new();
    for &o in &orders {
        let first = seen.insert(o);
        let rows = pats.get_mut(o as usize).ok_or_else(|| format!("the order list names pattern {o}, which the module has not"))?;
        for row in rows {
            for (c, cell) in row.iter_mut().enumerate() {
                if cell[3] == 4 {
                    if cell[4] != 0 {
                        mem[c] = cell[4];
                    } else if first {
                        cell[4] = mem[c];
                    }
                }
            }
        }
    }

    let mut unhandled = BTreeSet::new();
    let mut convert = |[note, ins, vol, fx, par]: [u8; 5]| -> [u8; 5] {
        let xn = match note {
            255 => 0,
            254 => 97,
            _ => (note >> 4) * 12 + (note & 15) + 1,
        };
        let xv = if vol != 255 { 0x10 + vol.min(64) } else { 0 };
        let letter = if fx != 0 { (64 + fx) as char } else { ' ' };
        let (xf, xp) = match letter {
            'A' => (0xF, par.min(0x1F)),
            'T' => (0xF, par.max(0x20)),
            'B' => (0xB, par),
            'C' => (0xD, par),
            'O' => (0x9, par),
            'D' => match (par >> 4, par & 15) {
                // Up, down, fine up, fine down; of both ST3 takes the low nibble.
                (_, 0) | (0, _) => (0xA, par),
                (x, 15) => (0xE, 0xA0 | x),
                (15, y) => (0xE, 0xB0 | y),
                (_, y) => (0xA, y),
            },
            ' ' => (0, 0),
            other => {
                unhandled.insert(other);
                (0, 0)
            }
        };
        [xn, ins, xv, xf, xp]
    };

    let mut out = xm::header(title, &orders, 0, nch, patn, insn, speed as u16, tempo as u16);
    for rows in &pats {
        let cells: Vec<[u8; 5]> = rows.iter().flatten().map(|&cell| convert(cell)).collect();
        out.extend(xm::pattern(cells.into_iter()));
    }

    for i in 0..insn {
        let o = d.u16(0x60 + ordn + i * 2)? as usize * 16;
        let typ = d.u8(o)?;
        let name = d.slice(o + 0x30, o + 0x4c).split(|&b| b == 0).next().unwrap_or(&[]);
        let (ln, lb, le) = (d.size(o + 0x10)?, d.size(o + 0x14)?, d.size(o + 0x18)?);
        let (vol, packed, fl) = (d.u8(o + 0x1c)?, d.u8(o + 0x1e)?, d.u8(o + 0x1f)?);
        let c2 = d.u32(o + 0x20)?;
        if typ != 1 || ln == 0 {
            out.extend(xm::silent(name));
            continue;
        }
        if packed != 0 || fl & 6 != 0 {
            return Err(format!("sample {} is packed, stereo or 16-bit", i + 1));
        }
        if c2 == 0 {
            return Err(format!("sample {} has no C2Spd", i + 1));
        }
        let seg = ((d.u8(o + 0x0d)? as usize) << 16 | d.u16(o + 0x0e)? as usize) * 16;
        let mut pcm = d.slice(seg, seg.saturating_add(ln)).to_vec();
        if ffi == 2 {
            pcm.iter_mut().for_each(|b| *b ^= 0x80);
        }
        let looped = fl & 1 != 0 && le > lb;
        if looped {
            pcm.truncate(le);
        }
        let st = (12.0 * 128.0 * (c2 as f64 / 8363.0).log2()).round_ties_even() as i32;
        let rel = (st + 64) >> 7;
        let fine = st - rel * 128;
        out.extend(xm::instrument(name, &pcm, looped.then_some((lb, le - lb)), vol, fine as i8, rel.clamp(-128, 127) as i8));
    }
    notes.push(format!("a ScreamTracker module of {nch} channels"));
    if !unhandled.is_empty() {
        let letters: Vec<String> = unhandled.iter().map(char::to_string).collect();
        notes.push(format!("not converted: the effects {}", letters.join(", ")));
    }
    Ok(out)
}

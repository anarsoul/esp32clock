// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The IMA ADPCM encoder for `onebitdawn_core::adpcm`'s decoder: four
//! bits a sample, half the flash of 8-bit PCM.
//!
//! The encoder keeps the `search` best paths through the coder's
//! states (1 is the plain encoder, a sample's nearest code), which is
//! a decibel less error for the same decoder.

use onebitdawn_core::adpcm::{INDEX, MAGIC, STEPS};

/// Paths the encoder keeps.
pub const SEARCH: usize = 4;

/// The decoder's step: the sample a code gives and the next index.
fn step(code: u8, predicted: i32, index: i32) -> (i32, i32) {
    let size = STEPS[index as usize] as i32;
    let mut diff = size >> 3;
    if code & 1 != 0 {
        diff += size >> 2;
    }
    if code & 2 != 0 {
        diff += size >> 1;
    }
    if code & 4 != 0 {
        diff += size;
    }
    let predicted = if code & 8 != 0 { predicted - diff } else { predicted + diff }.clamp(-32768, 32767);
    (predicted, (index + INDEX[(code & 7) as usize] as i32).clamp(0, 88))
}

/// The samples the codes give.
pub fn decode(codes: &[u8]) -> Vec<i16> {
    let (mut predicted, mut index) = (0, 0);
    codes
        .iter()
        .map(|&code| {
            (predicted, index) = step(code, predicted, index);
            predicted as i16
        })
        .collect()
}

/// The code the plain encoder gives a sample.
fn nearest(sample: i32, predicted: i32, index: i32) -> u8 {
    let size = STEPS[index as usize] as i32;
    let mut diff = sample - predicted;
    let mut code = if diff < 0 { 8 } else { 0 };
    diff = diff.abs();
    for (bit, part) in [(4, size), (2, size >> 1), (1, size >> 2)] {
        if diff >= part {
            code |= bit;
            diff -= part;
        }
    }
    code
}

/// The codes for `samples`: of the paths through the coder's states,
/// the `search` with the least squared error so far are kept at each
/// sample, and the best at the end is the clip.
pub fn encode(samples: &[i16], search: usize) -> Vec<u8> {
    if search <= 1 {
        let (mut predicted, mut index) = (0, 0);
        return samples
            .iter()
            .map(|&sample| {
                let code = nearest(sample as i32, predicted, index);
                (predicted, index) = step(code, predicted, index);
                code
            })
            .collect();
    }
    // A path: its error, the state it is in, and its last code in
    // `trail`, where each code names the one before it.
    const START: u32 = u32::MAX;
    let mut trail: Vec<(u8, u32)> = Vec::with_capacity(samples.len() * search);
    let mut paths: Vec<(u64, (i32, i32), u32)> = vec![(0, (0, 0), START)];
    let mut reached: Vec<(u64, (i32, i32), u8, u32)> = Vec::new();
    for &sample in samples {
        reached.clear();
        for &(error, (predicted, index), before) in &paths {
            for code in 0..16 {
                let state = step(code, predicted, index);
                let off = (sample as i64 - state.0 as i64).unsigned_abs();
                let total = error + off * off;
                // A state keeps the place it was first reached in,
                // which is what breaks a tie between two paths.
                match reached.iter_mut().find(|path| path.1 == state) {
                    Some(path) => {
                        if total < path.0 {
                            *path = (total, state, code, before);
                        }
                    }
                    None => reached.push((total, state, code, before)),
                }
            }
        }
        reached.sort_by_key(|path| path.0);
        paths.clear();
        for &(total, state, code, before) in reached.iter().take(search) {
            trail.push((code, before));
            paths.push((total, state, (trail.len() - 1) as u32));
        }
    }
    let mut codes = Vec::with_capacity(samples.len());
    let mut at = paths[0].2;
    while at != START {
        let (code, before) = trail[at as usize];
        codes.push(code);
        at = before;
    }
    codes.reverse();
    codes
}

/// An ADPCM clip of these samples at `rate` a second.
pub fn clip(samples: &[i16], rate: u32, search: usize) -> Vec<u8> {
    let mut samples = samples.to_vec();
    // Whole bytes: an odd clip ends on its last sample again.
    if samples.len() % 2 == 1 {
        samples.push(samples[samples.len() - 1]);
    }
    let codes = encode(&samples, search);
    let mut out = MAGIC.to_vec();
    out.extend(rate.to_le_bytes());
    out.extend(codes.as_chunks::<2>().0.iter().map(|pair| pair[0] << 4 | pair[1]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// A random walk under a square wave, loud and quiet by turns.
    fn signal() -> Vec<i16> {
        let (mut x, mut level) = (12345i64, 0i64);
        (0..30001i64)
            .map(|k| {
                x = (x * 1103515245 + 12345) & 0x7FFF_FFFF;
                level = (level + (x >> 16) % 4001 - 2000).clamp(-30000, 30000);
                (level + if k / 37 % 2 == 1 { 8000 } else { -8000 } * (k / 5000 % 3)).clamp(-32768, 32767) as i16
            })
            .collect()
    }

    /// The codes are those of `scripts/adpcm.py`, which the encoder was
    /// first written as: the digests are of its output for this signal
    /// (2026-11-19).
    #[test]
    fn the_encoder_is_the_one_the_clips_were_made_with() {
        for (search, digest) in [
            (1, "d11e626de6b75b3ff011c3ac4bc07d14bfc72605f6f08b3ba6e52bf90322c2b3"),
            (4, "17dd7df9daa4dd80006b1e9fed31bdacea7df1bbe0f3be401a781dab90b144ca"),
        ] {
            let made = clip(&signal(), 16000, search);
            assert_eq!(&made[..8], b"ADP1\x80\x3e\0\0");
            assert_eq!(made.len(), 8 + 15001);
            let hex: String = Sha256::digest(&made[8..]).iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(hex, digest, "search {search}");
        }
    }

    #[test]
    fn the_search_is_nearer_than_the_plain_encoder_and_the_clock_decodes_it() {
        let samples = signal();
        let error = |search| -> f64 {
            let codes = encode(&samples, search);
            decode(&codes).iter().zip(&samples).map(|(&a, &b)| (a as f64 - b as f64).powi(2)).sum()
        };
        assert!(error(4) < error(1));
        // The clock's decoder gives what this one does.
        let made = clip(&samples, 16000, 4);
        let mut theirs = onebitdawn_core::adpcm::Decoder::new(&made);
        let codes: Vec<u8> = made[8..].iter().flat_map(|b| [b >> 4, b & 15]).collect();
        for ours in decode(&codes) {
            assert_eq!(theirs.next_sample(), Some(ours));
        }
    }
}

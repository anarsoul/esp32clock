// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A PCM clip at four bits a sample: IMA ADPCM.
//!
//! Sampled sound has nothing a reference could name (a bar played
//! again is not the same samples), so the way to store it smaller is
//! this lossy one: each sample is a 4-bit code, the step from the last
//! sample in units that grow after a large code and shrink after a
//! small one. Half the flash of 8-bit PCM at the same rate, and heard
//! as much nearer to it than PCM at half the rate (HISTORY.md,
//! 2026-11-16).
//!
//! The clip is `MAGIC`, the rate as a `u32`, then the codes, two a
//! byte, the high nibble first: sox's raw `ima`, so the body plays on
//! the host. The coder starts from a prediction of 0 and step index 0.
//! `scripts/adpcm.py` is the encoder. `Decoder` reads the codes in
//! place and gives 16-bit samples, in `xm::STEP`ths of PCM's 8-bit
//! step as a tune's are.

use crate::clip::{FAST_HZ, PCM_HZ};

/// The first bytes of an ADPCM clip.
pub const MAGIC: &[u8; 4] = b"ADP1";
/// The magic and the rate.
const HEADER: usize = 8;

/// The step sizes, by index.
const STEPS: [i16; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190,
    209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499,
    2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385,
    24623, 27086, 29794, 32767,
];
/// What a code's magnitude adds to the step index.
const INDEX: [i8; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// Whether `data` is an ADPCM clip.
pub const fn is_clip(data: &[u8]) -> bool {
    data.len() >= HEADER && data[0] == MAGIC[0] && data[1] == MAGIC[1] && data[2] == MAGIC[2] && data[3] == MAGIC[3]
}

/// Samples a second of the clip `data`: what `PCM1` may say, and
/// `PCM_HZ` for anything else.
pub const fn rate(data: &[u8]) -> u32 {
    if !is_clip(data) {
        return PCM_HZ;
    }
    match u32::from_le_bytes([data[4], data[5], data[6], data[7]]) {
        rate if rate == 2 * PCM_HZ || rate == FAST_HZ => rate,
        _ => PCM_HZ,
    }
}

/// Samples in the clip `data`: two a byte after the header.
pub const fn samples(data: &[u8]) -> usize {
    if is_clip(data) { 2 * (data.len() - HEADER) } else { 0 }
}

/// How long the clip plays, in milliseconds.
pub const fn duration_ms(data: &[u8]) -> u32 {
    (samples(data) as u64 * 1000 / rate(data) as u64) as u32
}

/// The clip's samples, one at a time.
pub struct Decoder<'a> {
    /// The codes, without the header.
    codes: &'a [u8],
    /// The next code, counted in nibbles.
    at: usize,
    /// The sample last given.
    predicted: i32,
    /// The step's place in `STEPS`.
    index: i32,
    /// Whether the clip starts over at its end.
    looping: bool,
}

impl<'a> Decoder<'a> {
    /// A decoder of the clip `data`; of nothing, if it is not one.
    pub const fn new(data: &'a [u8]) -> Self {
        let codes: &[u8] = if is_clip(data) { data.split_at(HEADER).1 } else { &[] };
        Self { codes, at: 0, predicted: 0, index: 0, looping: false }
    }

    /// Samples in the clip.
    pub const fn len(&self) -> usize {
        2 * self.codes.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Go round at the end, or stop there. The coder's state starts
    /// over with the clip, as the encoder's did.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// The next sample, `None` after the last of a clip played once.
    pub fn next_sample(&mut self) -> Option<i16> {
        if self.at >= self.len() {
            if !self.looping || self.is_empty() {
                return None;
            }
            (self.at, self.predicted, self.index) = (0, 0, 0);
        }
        let byte = self.codes[self.at / 2];
        let code = if self.at & 1 == 0 { byte >> 4 } else { byte & 15 };
        self.at += 1;
        let step = STEPS[self.index as usize] as i32;
        // (magnitude + 1/2) * step / 4, each term rounded down.
        let mut diff = step >> 3;
        if code & 1 != 0 {
            diff += step >> 2;
        }
        if code & 2 != 0 {
            diff += step >> 1;
        }
        if code & 4 != 0 {
            diff += step;
        }
        if code & 8 != 0 {
            diff = -diff;
        }
        self.predicted = (self.predicted + diff).clamp(i16::MIN as i32, i16::MAX as i32);
        self.index = (self.index + INDEX[(code & 7) as usize] as i32).clamp(0, STEPS.len() as i32 - 1);
        Some(self.predicted as i16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(rate: u32, codes: &[u8]) -> Vec<u8> {
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(codes);
        data
    }

    /// The plain encoder: each sample's nearest code, the decoder's own
    /// state carried along.
    fn encode(samples: &[i16]) -> Vec<u8> {
        let (mut predicted, mut index) = (0i32, 0i32);
        let mut nibbles = Vec::new();
        for &s in samples {
            let step = STEPS[index as usize] as i32;
            let mut diff = s as i32 - predicted;
            let mut code = 0u8;
            if diff < 0 {
                code = 8;
                diff = -diff;
            }
            let mut moved = step >> 3;
            for (bit, part) in [(4, step), (2, step >> 1), (1, step >> 2)] {
                if diff >= part {
                    code |= bit;
                    diff -= part;
                    moved += part;
                }
            }
            predicted = (predicted + if code & 8 != 0 { -moved } else { moved }).clamp(i16::MIN as i32, i16::MAX as i32);
            index = (index + INDEX[(code & 7) as usize] as i32).clamp(0, 88);
            nibbles.push(code);
        }
        nibbles.chunks(2).map(|pair| pair[0] << 4 | pair.get(1).copied().unwrap_or(0)).collect()
    }

    /// Codes that run the step up and the prediction into both rails,
    /// and what sox made of them (`sox -t ima -r 16000 -c 1`).
    #[test]
    fn the_samples_are_what_sox_decodes() {
        const CODES: [u8; 32] = [
            0xE6, 0x07, 0x79, 0x7E, 0xC7, 0x95, 0x77, 0x00, 0x75, 0x79, 0x07, 0xC7, 0x5C, 0x7C, 0xC0, 0x75, 0x79, 0x6F, 0x06, 0x97, 0xCF, 0x96, 0x7C,
            0xC5, 0xE7, 0x97, 0xC7, 0xC5, 0x39, 0x0E, 0x3C, 0x37,
        ];
        const SAMPLES: [i16; 64] = [
            -10, 10, 12, 50, 133, 97, 262, -46, -425, 340, 12, 1106, 3291, 7975, 8644, 9252, 17554, 30606, 32767, 21595, 24980, 32767, -4095, 32767,
            32767, -4095, 32767, -4095, -32768, -28673, 27190, 32767, 32767, 20481, 32767, -28669, -24574, 23841, 11555, 32767, -4095, -32768, -32768,
            15647, 32767, -4095, -32768, 12285, -32768, 28668, 16382, 32767, -4095, 32767, -4095, 32767, 32767, 21595, 24980, -15031, 13638, -19880,
            8789, 32767,
        ];
        let data = clip(16_000, &CODES);
        let mut decoder = Decoder::new(&data);
        let got: Vec<i16> = core::iter::from_fn(|| decoder.next_sample()).collect();
        assert_eq!(got, SAMPLES);
        assert_eq!(decoder.next_sample(), None, "and it stays ended");
    }

    #[test]
    fn a_sine_comes_back() {
        let sine: Vec<i16> = (0..8000).map(|i| (12_000.0 * (i as f64 * 0.2).sin()) as i16).collect();
        let data = clip(16_000, &encode(&sine));
        let mut decoder = Decoder::new(&data);
        let got: Vec<i16> = core::iter::from_fn(|| decoder.next_sample()).collect();
        assert_eq!(got.len(), sine.len());
        // Past the first samples, where the step is still growing.
        let signal: f64 = sine[200..].iter().map(|&s| (s as f64).powi(2)).sum();
        let error: f64 = sine[200..].iter().zip(&got[200..]).map(|(&a, &b)| (a as f64 - b as f64).powi(2)).sum();
        let snr = 10.0 * (signal / error).log10();
        assert!(snr > 30.0, "a 500 Hz sine at 16 kHz within 30 dB, got {snr:.1}");
    }

    #[test]
    fn a_looping_clip_starts_over_as_it_started() {
        let data = clip(16_000, &[0x17, 0x7A, 0xF3]);
        let mut once = Decoder::new(&data);
        let pass: Vec<i16> = core::iter::from_fn(|| once.next_sample()).collect();
        assert_eq!(pass.len(), 6);
        let mut round = Decoder::new(&data);
        round.set_looping(true);
        for k in 0..18 {
            assert_eq!(round.next_sample(), Some(pass[k % 6]), "sample {k}");
        }
        // An empty clip goes nowhere, looped or not.
        let empty = clip(16_000, &[]);
        let mut decoder = Decoder::new(&empty);
        decoder.set_looping(true);
        assert_eq!(decoder.next_sample(), None);
    }

    #[test]
    fn the_header_says_the_rate_and_the_length() {
        let data = clip(16_000, &[0; 8000]);
        assert!(is_clip(&data));
        assert_eq!((rate(&data), samples(&data), duration_ms(&data)), (16_000, 16_000, 1000));
        assert_eq!(rate(&clip(FAST_HZ, &[])), FAST_HZ);
        assert_eq!(rate(&clip(8_000, &[])), PCM_HZ);
        assert_eq!(rate(&clip(44_100, &[])), PCM_HZ, "a rate the line cannot run at");
        // Not a clip: too short for the header, or another magic.
        assert!(!is_clip(b"ADP1\x80\x3e\x00") && !is_clip(b"PCM1\x80\x3e\x00\x00"));
        assert_eq!((samples(b"ADP1"), Decoder::new(b"PCM1\x80\x3e\x00\x00\x77").next_sample()), (0, None));
    }
}

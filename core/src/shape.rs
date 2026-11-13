// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! What a player on the chip does to its mix before the modulator, and
//! `scripts/wav2pcm.sh` does to a PCM clip on the host, since the
//! buzzer is the same: the bass cut by a high-pass and the level pushed
//! hard into a limiter. How much bass and how hard are the clip's to
//! say, in its header; a tune whose melody sits under 900 Hz wants a
//! lower cut than the PCM clips' 900 Hz twice over.
//!
//! The tracker modules (`xm`) and the SID tunes (`sid`) both end here.

/// The limiter: a level under this gets the full gain, one over it is
/// brought down to it. One channel at full volume peaks at 32768
/// before the filter, so this is 20 dB of drive, as the host's
/// conversion gives a PCM clip.
const LIMIT: i32 = 3000;
/// The limiter lets go by a 128th a sample at `clip::PCM_HZ`, 16 ms.
const RELEASE_SHIFT: u32 = 7;
/// What the limiter aims a peak at, of a full scale of 127: just that.
/// With these two the test modules come out at the RMS the host's
/// conversion of them has, 0.42 of full scale. A release of 4 ms and
/// peaks aimed a quarter over full scale, the excess clipped, are 3 dB
/// more (0.60, a sixth of the samples at the rails) and were the
/// default while the tunes ran at 8 kHz; at 16 kHz the user's ear
/// took the cleaner one (2026-11-12). A clip may still ask for its own.
const DRIVE: i32 = 127;

/// What one 8-bit step is in the samples the shaper gives.
pub const STEP: i32 = 256;

/// A second-order section, transposed direct form II.
#[derive(Clone, Copy, Default)]
struct Biquad {
    z1: i64,
    z2: i64,
}

impl Biquad {
    /// `k` is b0, b1, b2, a1, a2 in Q14.
    fn run(&mut self, k: &[i64; 5], x: i32) -> i32 {
        let x = x as i64;
        let y = (k[0] * x + self.z1) >> 14;
        self.z1 = k[1] * x - k[3] * y + self.z2;
        self.z2 = k[2] * x - k[4] * y;
        y as i32
    }
}

/// The high-pass and the limiter of one tune being played.
pub struct Shaper {
    /// The high-pass: its section, how many of them, and their states.
    highpass: [i64; 5],
    passes: usize,
    filter: [Biquad; 2],
    envelope: i32,
    drive: i32,
    release: u32,
    /// The limiter's knee: a level under it gets the full gain.
    knee: i32,
    /// Whether the samples keep what the limiter's division leaves
    /// under the 8-bit step.
    wide: bool,
}

impl Shaper {
    /// As a clip's header gives them: the limiter's `drive` and
    /// `release` (0 for the defaults; the release is given for
    /// `clip::PCM_HZ`, and `twice` says the samples come at twice that,
    /// where a shift more is the same time), the high-pass's section in
    /// Q14 (b0 b1 b2 a1 a2, worked out by the converter since the chip
    /// has no tangent) and how many times it is applied (0 to 2), and
    /// whether the samples are to be finer than the 8-bit step; and
    /// the limiter's `knee` in 1024s of the mix's units (0 for `LIMIT`):
    /// 32 is one voice at full level, and a knee above a tune's own
    /// peak is the limiter off, a plain gain, for a speaker, where the
    /// buzzer's 20 dB of drive is heard as overdrive.
    pub fn new(drive: u8, release: u8, twice: bool, section: [i16; 5], passes: u8, wide: bool, knee: u8) -> Self {
        Self {
            highpass: section.map(|k| k as i64),
            passes: (passes as usize).min(2),
            filter: [Biquad::default(); 2],
            envelope: 0,
            drive: match drive {
                0 => DRIVE,
                d => d as i32,
            },
            release: match release {
                0 => RELEASE_SHIFT,
                r => (r as u32).min(14),
            } + twice as u32,
            knee: match knee {
                0 => LIMIT,
                k => k as i32 * 1024,
            },
            wide,
        }
    }

    /// One sample of the mix, 32768 for one voice at full level, as the
    /// modulator's: signed, in `STEP`ths of the 8-bit step a PCM clip
    /// has, so full scale is 127 of those. Whole steps unless `wide`.
    pub fn run(&mut self, x: i32) -> i16 {
        let mut y = x;
        for filter in &mut self.filter[..self.passes] {
            y = filter.run(&self.highpass, y);
        }
        self.envelope = (self.envelope - (self.envelope >> self.release)).max(y.abs());
        let gain = self.envelope.max(self.knee);
        if self.wide {
            (y as i64 * (self.drive * STEP) as i64 / gain as i64).clamp(-127 * STEP as i64, 127 * STEP as i64) as i16
        } else {
            ((y * self.drive / gain).clamp(-127, 127) * STEP) as i16
        }
    }
}

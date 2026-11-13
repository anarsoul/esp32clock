// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Sampled sound for a 1-bit line, the pure half: the pulse-width
//! modulator that turns 8 kHz PCM into the bitstream the buzzer's
//! transistor is switched with, and the bookkeeping that keeps a ring
//! of chunks filled ahead of the DMA reading it.
//!
//! The clip is raw PCM, 8 bits unsigned, mono, `PCM_HZ` samples a
//! second (`scripts/wav2pcm.sh`). The line carries `BIT_RATE` bits a
//! second, a 1 for the buzzer on, in periods of `PERIOD_BITS` at
//! `CARRIER_HZ`: each period is one pulse, a run of ones in its
//! middle, as long as the level at mid-period asks for (a pulse that
//! grew from the period's start would move its centre with the level,
//! which is 20 dB of distortion at 1 kHz under a 16 kHz carrier). The first version (2026-11-12)
//! was a sigma-delta stream, a decision per bit, and on the clock it
//! was quieter than this: a pulse a period keeps the on time long and
//! the edges few, for a coil and a transistor that 4 µs bits are
//! short for.
//!
//! The pulse length has only `PERIOD_BITS + 1` values, so the rounding
//! error is fed back and shaped out of the band as far as the carrier
//! allows: second order with four periods a sample, first with two,
//! none with one, where there is no room above the band to push it to
//! (`ORDER`). Between PCM samples the input is interpolated linearly.
//!
//! Mid-scale is a 50 % duty, so a clip that started and stopped there
//! would step the coil's DC twice. `Player` wraps the clip in two
//! ramps, from all zeros up to mid-scale and back, and after the end it
//! writes zeros, the released line.

use core::ops::Range;

use crate::config;

/// Samples per second of a clip.
pub const PCM_HZ: u32 = 8_000;
/// Bits per second on the line.
pub const BIT_RATE: u32 = config::CLIP_KBPS * 1000;
/// Bits per PCM sample.
pub const OSR: usize = (BIT_RATE / PCM_HZ) as usize;
/// Bytes of bitstream per PCM sample.
pub const BYTES_PER_SAMPLE: usize = OSR / 8;
/// Pulses per second.
pub const CARRIER_HZ: u32 = config::CLIP_CARRIER_KHZ * 1000;
/// Bits in one period of the carrier: the pulse is 0 to this many ones.
pub const PERIOD_BITS: usize = (BIT_RATE / CARRIER_HZ) as usize;
/// Carrier periods per PCM sample.
pub const PERIODS: usize = (CARRIER_HZ / PCM_HZ) as usize;
const PERIOD_BYTES: usize = PERIOD_BITS / 8;
// Whole bytes per period keep the pulses on byte boundaries, and a
// power of two keeps the level steps exact.
const _: () = assert!(PERIOD_BITS.is_power_of_two() && PERIOD_BITS >= 8, "CLIP_KBPS / CLIP_CARRIER_KHZ must be 8 or more");
const _: () = assert!(PERIODS * PERIOD_BITS == OSR && BYTES_PER_SAMPLE * 8 == OSR);
/// How far the rounding error of the pulse length is shaped: the
/// in-band share of the error falls by 11 dB at second order with the
/// band an eighth of the carrier, by 1 dB at first order with a
/// quarter, and would rise with a half.
const ORDER: usize = match PERIODS {
    1 => 0,
    2 => 1,
    _ => 2,
};

/// Bytes in one chunk of the ring: one DMA descriptor.
pub const CHUNK: usize = 1024;
/// Chunks in the ring: 16 KB, half a second at 256 kbit/s and a quarter
/// at 512, all but two chunks of it ahead of the DMA. It was eight
/// until clips skipped on a busy clock (2026-11-12).
pub const SLOTS: usize = 16;
const _: () = assert!(CHUNK.is_multiple_of(BYTES_PER_SAMPLE) && SLOTS >= 4);

/// How long the line takes to send one chunk at `bit_rate`, in microseconds.
pub const fn chunk_us(bit_rate: u32) -> u64 {
    CHUNK as u64 * 8 * 1_000_000 / bit_rate as u64
}

/// The level of a period that is all ones, and minus that of one
/// that is all zeros, in the units of `LEVEL`.
const FULL: i32 = 1 << 16;
/// Whether the PCM is modulated a bit at a time (`CLIP_MODULATOR=sd`
/// at build time) and not a pulse a period: the first version, kept to
/// try against a changed buzzer circuit. Second order,
/// `NTF = (1 - z^-1)^2`, 15 dB less noise in the band per doubling of
/// the bit rate, and a flip of the line as often as every bit.
const SIGMA_DELTA: bool = config::CLIP_SIGMA_DELTA;
/// One step of the 8-bit sample. 127 steps are 0.95 of `FULL` for the
/// pulses: with a pulse length to choose rather than one bit the loop
/// holds to the rails, and what is left is the ramps' room. A 1-bit
/// loop of second order is stable to about 0.8, so there it is 0.7.
const LEVEL: i32 = if SIGMA_DELTA { 361 } else { 490 };
const _: () = assert!(128 * LEVEL <= FULL);
/// `FULL` at the scale the loop runs at: every level is carried times
/// `PERIODS` so that the interpolation is one addition per period.
const FB: i32 = FULL * PERIODS as i32;
/// What one bit of pulse length is worth.
const BIT: i32 = 2 * FB / PERIOD_BITS as i32;
/// Clamp of the integrators, far outside what they reach.
const LIMIT: i32 = 16 * FB;

/// PCM samples in each ramp: 30 ms.
pub const RAMP: usize = 240;

/// Turns a clip into its bitstream, a buffer at a time.
pub struct Player<'a> {
    pcm: &'a [u8],
    /// Index of the next point of the level sequence: the up ramp, the
    /// samples, the down ramp.
    next: usize,
    /// The level at the point just reached.
    cur: i32,
    s1: i32,
    s2: i32,
    /// The level of the pulse last written.
    fb: i32,
}

impl<'a> Player<'a> {
    pub const fn new(pcm: &'a [u8]) -> Self {
        Self { pcm, next: 1, cur: -FULL, s1: 0, s2: 0, fb: if SIGMA_DELTA { -FULL * OSR as i32 } else { -FB } }
    }

    /// The level at point `k`, `None` past the end. Point 0 is the
    /// rail the line idles at, and so is the last.
    fn level(&self, k: usize) -> Option<i32> {
        let n = self.pcm.len();
        if k < RAMP {
            Some(-FULL + FULL * k as i32 / RAMP as i32)
        } else if k < RAMP + n {
            Some((self.pcm[k - RAMP] as i32 - 128) * LEVEL)
        } else if k < RAMP + n + RAMP {
            Some(-FULL * (k - RAMP - n + 1) as i32 / RAMP as i32)
        } else {
            None
        }
    }

    /// Whether the clip and its ramps have been written out; `fill`
    /// writes zeros from then on.
    pub fn done(&self) -> bool {
        self.next >= 2 * RAMP + self.pcm.len()
    }

    /// PCM sample periods the whole stream lasts, ramps included.
    pub const fn periods(&self) -> usize {
        2 * RAMP + self.pcm.len() - 1
    }

    /// Write the next `out.len()` bytes of the stream, MSB first. The
    /// length must be a multiple of `BYTES_PER_SAMPLE`.
    pub fn fill(&mut self, out: &mut [u8]) {
        debug_assert!(out.len().is_multiple_of(BYTES_PER_SAMPLE));
        if SIGMA_DELTA {
            return self.fill_bits(out);
        }
        let (mut s1, mut s2, mut fb) = (self.s1, self.s2, self.fb);
        for group in out.as_chunks_mut::<BYTES_PER_SAMPLE>().0 {
            let Some(target) = self.level(self.next) else {
                group.fill(0);
                (s1, s2, fb) = (0, 0, -FB);
                continue;
            };
            self.next += 1;
            // From `cur` to `target` in `PERIODS` steps, at the loop's scale.
            let step = target - self.cur;
            let mut x = self.cur * PERIODS as i32;
            self.cur = target;
            for period in group.as_chunks_mut::<PERIOD_BYTES>().0 {
                // The level at the middle of the period, where the pulse is.
                let mid = x + step / 2;
                x += step;
                let wanted = match ORDER {
                    0 => mid,
                    1 => {
                        s1 = (s1 + mid - fb).clamp(-LIMIT, LIMIT);
                        s1
                    }
                    _ => {
                        s1 = (s1 + mid - fb).clamp(-LIMIT, LIMIT);
                        s2 = (s2 + s1 - fb).clamp(-LIMIT, LIMIT);
                        s2
                    }
                };
                // The nearest pulse length, 0 to PERIOD_BITS ones.
                let ones = (wanted + FB + BIT / 2).div_euclid(BIT).clamp(0, PERIOD_BITS as i32);
                fb = ones * BIT - FB;
                // Centred: bits `from..to` of the period, MSB first.
                let from = (PERIOD_BITS as u32 - ones as u32) / 2;
                let to = from + ones as u32;
                for (i, byte) in period.iter_mut().enumerate() {
                    let base = 8 * i as u32;
                    let a = from.clamp(base, base + 8) - base;
                    let b = to.clamp(base, base + 8) - base;
                    *byte = ((0xFFu16 >> a) & (0xFF00u16 >> b)) as u8;
                }
            }
        }
        (self.s1, self.s2, self.fb) = (s1, s2, fb);
    }

    /// `fill` for the sigma-delta stream: a decision a bit.
    fn fill_bits(&mut self, out: &mut [u8]) {
        // The loop's scale here: levels times `OSR`, a step a bit.
        const FB: i32 = FULL * OSR as i32;
        const LIMIT: i32 = 16 * FB;
        let (mut s1, mut s2, mut fb) = (self.s1, self.s2, self.fb);
        for group in out.as_chunks_mut::<BYTES_PER_SAMPLE>().0 {
            let Some(target) = self.level(self.next) else {
                group.fill(0);
                (s1, s2, fb) = (0, 0, -FB);
                continue;
            };
            self.next += 1;
            let step = target - self.cur;
            let mut x = self.cur * OSR as i32;
            self.cur = target;
            for byte in group {
                let mut bits = 0u8;
                for _ in 0..8 {
                    x += step;
                    s1 = (s1 + x - fb).clamp(-LIMIT, LIMIT);
                    s2 = (s2 + s1 - fb).clamp(-LIMIT, LIMIT);
                    let one = s2 >= 0;
                    fb = if one { FB } else { -FB };
                    bits = bits << 1 | one as u8;
                }
                *byte = bits;
            }
        }
        (self.s1, self.s2, self.fb) = (s1, s2, fb);
    }
}

/// The first bytes of a beeper clip; a clip with neither magic is PCM.
pub const BEEPER_MAGIC: &[u8; 4] = b"BPR1";
/// Bytes before a beeper clip's runs: the magic and the length in ticks.
const BEEPER_HEADER: usize = 8;
/// Ticks per second of a beeper clip's run lengths.
pub const BEEPER_TICK_HZ: u32 = 128_000;
/// Bits on the line per tick.
const BITS_PER_TICK: u32 = BIT_RATE / BEEPER_TICK_HZ;
const _: () = assert!(BITS_PER_TICK >= 1 && BITS_PER_TICK * BEEPER_TICK_HZ == BIT_RATE);

const fn has_magic(data: &[u8], magic: &[u8; 4], header: usize) -> bool {
    data.len() >= header && data[0] == magic[0] && data[1] == magic[1] && data[2] == magic[2] && data[3] == magic[3]
}

/// Whether `data` is a beeper clip.
pub const fn is_beeper(data: &[u8]) -> bool {
    has_magic(data, BEEPER_MAGIC, BEEPER_HEADER)
}

/// How long a clip of any kind plays, in milliseconds.
pub const fn duration_ms(data: &[u8]) -> u32 {
    if is_beeper(data) {
        let ticks = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        (ticks as u64 * 1000 / BEEPER_TICK_HZ as u64) as u32
    } else if is_wham(data) {
        let steps = u16::from_le_bytes([data[6], data[7]]) as u64;
        (steps * (tone_step_t(data[4]) + STEP_OVERHEAD_T) as u64 * 1000 / CPU_HZ as u64) as u32
    } else {
        (data.len() as u64 * 1000 / PCM_HZ as u64) as u32
    }
}

/// The line as runs: a level and how many bits it lasts. A run of no
/// bits is allowed.
pub trait RunSource {
    fn next_run(&mut self) -> Option<(bool, u32)>;
}

/// Writes a `RunSource` out as the line's bytes, MSB first, zeros after
/// its end.
pub struct Runs<S> {
    source: S,
    on: bool,
    /// Bits of the current run still to write.
    left: u32,
    finished: bool,
}

impl<S: RunSource> Runs<S> {
    pub const fn new(source: S) -> Self {
        Self { source, on: false, left: 0, finished: false }
    }

    pub fn done(&self) -> bool {
        self.finished
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        for byte in out {
            if self.left >= 8 {
                self.left -= 8;
                *byte = if self.on { 0xFF } else { 0 };
                continue;
            }
            let mut bits = 0u16;
            let mut need = 8;
            while need > 0 {
                if self.left == 0 {
                    if self.finished {
                        break;
                    }
                    match self.source.next_run() {
                        Some((on, n)) => (self.on, self.left) = (on, n),
                        None => (self.on, self.finished) = (false, true),
                    }
                    continue;
                }
                let k = self.left.min(need);
                bits = bits << k | if self.on { (1 << k) - 1 } else { 0 };
                self.left -= k;
                need -= k;
            }
            *byte = (bits << need) as u8;
        }
    }
}

/// A beeper clip: what a 1-bit speaker was driven with, as it was, with
/// no modulator between. The clip is the line's level as run lengths,
/// off then on then off and so on, each a LEB128 count of ticks of
/// `1 / BEEPER_TICK_HZ`, after `BEEPER_MAGIC` and the total in ticks as
/// a little-endian `u32` (`scripts/zxbeeper.py` writes it from a ZX
/// Spectrum program's port writes). A run of nothing is allowed, for a
/// clip that starts on or a run too long for one count.
pub struct Beeper<'a> {
    runs: &'a [u8],
    on: bool,
}

impl<'a> Beeper<'a> {
    /// `data` must be a beeper clip (`is_beeper`).
    pub fn new(data: &'a [u8]) -> Self {
        // The first run read is an off one.
        Self { runs: &data[BEEPER_HEADER.min(data.len())..], on: true }
    }
}

impl RunSource for Beeper<'_> {
    fn next_run(&mut self) -> Option<(bool, u32)> {
        let mut ticks = 0u32;
        let mut shift = 0;
        loop {
            let (&byte, rest) = self.runs.split_first()?;
            self.runs = rest;
            if shift < 32 {
                ticks |= ((byte & 0x7F) as u32) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
        }
        self.on = !self.on;
        Some((self.on, ticks.saturating_mul(BITS_PER_TICK)))
    }
}

/// The first bytes of a tune for the two-channel player.
pub const WHAM_MAGIC: &[u8; 4] = b"WHM1";
/// Bytes before the notes: the magic, the tempo, the octave shift and
/// the number of steps as a little-endian `u16`.
const WHAM_HEADER: usize = 8;

/// Whether `data` is a tune for the two-channel player.
pub const fn is_wham(data: &[u8]) -> bool {
    has_magic(data, WHAM_MAGIC, WHAM_HEADER)
}

/// The bit rate the two-channel player's line runs at, whatever
/// `CLIP_KBPS` says: a channel's slot is 48 T-states, which is seven
/// bits here and three and a half at 256 kbit/s, where the slots came
/// out three and four bits by turns and the tunes crackled
/// (2026-11-12).
pub const WHAM_BIT_RATE: u32 = 512_000;
/// A channel's slot of the tone loop on the line. Whole bits, so every
/// slot is the same: 0.3 % short of the original's 48 T-states, which
/// is five cents sharp and as much fast.
const SLOT_BITS: u32 = 7;

/// The clock the player's timing is written in: the T-states of the
/// 3.5 MHz Z80 it was written for.
const CPU_HZ: u32 = 3_500_000;
/// One pass of the tone loop; each channel has half of it.
const LOOP_T: u32 = 96;
/// Between two steps: fetching the notes and a look at the keyboard.
const STEP_OVERHEAD_T: u32 = 1164;
/// One pass of the silent loop, and what each 256 of them add.
const REST_LOOP_T: u32 = 94;
const REST_OUTER_T: u32 = 14;
/// One pass of the noise loop with and without a sample taken.
const NOISE_ON_T: u32 = 96;
const NOISE_OFF_T: u32 = 90;
/// The sweep drum: the first half period, what each cycle pair adds,
/// and the gap between pairs.
const SWEEP_FIRST_T: u32 = 130;
const SWEEP_STEP_T: u32 = 1020;
const SWEEP_GAP_T: u32 = 360;
/// Cycle pairs of the sweep drum by `255 - tempo`.
const SWEEP_PAIRS: [u8; 35] =
    [1, 1, 1, 2, 2, 11, 12, 13, 13, 14, 14, 14, 15, 15, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 21, 21, 21, 22, 22, 22, 22, 23, 23, 24];
/// Loop passes between flips for each semitone, lowest first: 255 is
/// 71.5 Hz, and twelve further on is half of it. `REST` and anything
/// past the table is silence.
const WHAM_PERIODS: [u8; 53] = [
    255, 240, 227, 215, 203, 192, 180, 171, 161, 151, 144, 136, 128, 121, 114, 108, 102, 96, 91, 86, 81, 76, 72, 68, 64, 61, 57, 54,
    51, 48, 45, 43, 40, 38, 36, 34, 32, 30, 28, 27, 25, 24, 23, 21, 20, 19, 18, 17, 16, 15, 14, 13, 12,
];
/// The note code of a rest.
pub const WHAM_REST: u8 = 53;
/// Channel 1 codes from here are drums, with channel 2's byte as the
/// noise's pattern.
const DRUM: u8 = 0x80;
const DRUM_NOISE: u8 = 0xFF;
const DRUM_SWEEP: u8 = 0xC0;

/// T-states of one step of tones at this tempo.
const fn tone_step_t(tempo: u8) -> u32 {
    (256 - tempo as u32) * 256 * LOOP_T
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fetch,
    /// The tone loop: which channel's slot is next.
    Tone { second: bool },
    /// The four quarters of a drum, most significant bit of the code's
    /// low nibble first: noise or silence.
    Quarters,
    Noise,
    Sweep { pair: u8, part: u8 },
    /// The gap after a step.
    Gap,
}

/// A two-channel tune for a 1-bit speaker, played the way Mark
/// Alexander's "Wham! The Music Box" (ZX Spectrum, 1985) plays its
/// tunes; `scripts/wham2clip.py` takes them from that program's tape.
/// This is a new player written from what the original does, checked
/// against it in an emulator flip for flip on the tones and to tens of
/// microseconds a step on the rests and drums.
///
/// A tune is steps of two note codes. A step runs a fixed loop
/// `(256 - tempo) * 256` times; each pass gives the line to channel 1
/// for half of it and to channel 2 for the other half, so the two
/// square waves are mixed by time, 36 kHz apart, and each channel flips
/// its level every `WHAM_PERIODS[code]` passes. Both channels resting is a
/// silent step, a little shorter. Channel 1 codes from 0x80 are drums
/// that take the whole step: noise gated by channel 2's byte as a bit
/// pattern, in four quarters picked by the code's low nibble, or for
/// the whole step (0xFF), or a falling sweep (0xC0).
///
/// Two things are not the original's. A channel that rests while the
/// other plays is held low; the original flips it every pass, an
/// 18 kHz tone at half level. And the noise is a shift register's; the
/// original reads it out of its machine's ROM.
///
/// `octave` in the header shifts every period right, for a buzzer that
/// has no bass.
pub struct Wham<'a> {
    c1: &'a [u8],
    c2: &'a [u8],
    tempo: u8,
    octave: u8,
    step: usize,
    phase: Phase,
    /// The line's level, held through gaps and rests.
    level: bool,
    /// Fraction of a bit carried from run to run, in `1 / CPU_HZ` bits.
    carry: u32,
    levels: [bool; 2],
    /// Passes until each channel flips, and its period; 0 is a rest.
    count: [u8; 2],
    period: [u8; 2],
    /// Passes of the current loop still to run.
    passes: u32,
    /// The drum code and its quarters still to play.
    code: u8,
    quarters: u8,
    pattern: u8,
    lfsr: u16,
}

impl<'a> Wham<'a> {
    /// `data` must be such a tune (`is_wham`).
    pub fn new(data: &'a [u8]) -> Self {
        let steps = u16::from_le_bytes([data[6], data[7]]) as usize;
        let notes = &data[WHAM_HEADER..];
        let steps = steps.min(notes.len() / 2);
        Self {
            c1: &notes[..steps],
            c2: &notes[steps..2 * steps],
            tempo: data[4],
            octave: data[5].min(3),
            step: 0,
            phase: Phase::Fetch,
            level: false,
            carry: 0,
            levels: [false; 2],
            count: [0; 2],
            period: [0; 2],
            passes: 0,
            code: 0,
            quarters: 0,
            pattern: 0,
            lfsr: 0,
        }
    }

    /// The bits `t` T-states take, with the fraction carried over.
    fn bits(&mut self, mut t: u32) -> u32 {
        // 8000 T-states at the bit rate, with the carry, fit in a u32.
        const PIECE: u32 = 8000;
        const _: () = assert!(PIECE as u64 * WHAM_BIT_RATE as u64 + CPU_HZ as u64 <= u32::MAX as u64);
        let mut bits = 0;
        while t > 0 {
            let piece = t.min(PIECE);
            let x = self.carry + piece * WHAM_BIT_RATE;
            bits += x / CPU_HZ;
            self.carry = x % CPU_HZ;
            t -= piece;
        }
        bits
    }

    fn period_of(&self, code: u8) -> u8 {
        match WHAM_PERIODS.get(code as usize) {
            Some(&p) => (p >> self.octave).max(2),
            None => 0,
        }
    }

    /// Passes of the silent and noise loops in a whole step, and in a
    /// quarter of one.
    fn drum_passes(&self) -> u32 {
        (255 - self.tempo as u32) * 256
    }

    fn quarter_passes(&self) -> u32 {
        ((256 - self.tempo as u32) / 4).max(1) * 256
    }

    fn rest_t(passes: u32) -> u32 {
        passes * REST_LOOP_T + passes / 256 * REST_OUTER_T
    }

    fn start_noise(&mut self, passes: u32) {
        self.passes = passes;
        // The same noise every time, as the original's is.
        self.lfsr = 0xACE1;
        self.phase = Phase::Noise;
    }
}

impl RunSource for Wham<'_> {
    fn next_run(&mut self) -> Option<(bool, u32)> {
        loop {
            match self.phase {
                Phase::Fetch => {
                    let (&c1, &c2) = (self.c1.get(self.step)?, self.c2.get(self.step)?);
                    self.step += 1;
                    if c1 >= DRUM {
                        self.code = c1;
                        self.pattern = c2;
                        self.quarters = 0;
                        match c1 {
                            DRUM_NOISE => self.start_noise(self.drum_passes()),
                            DRUM_SWEEP => self.phase = Phase::Sweep { pair: 0, part: 0 },
                            _ => {
                                self.quarters = 4;
                                self.phase = Phase::Quarters;
                            }
                        }
                        continue;
                    }
                    self.period = [self.period_of(c1), self.period_of(c2)];
                    if self.period == [0, 0] {
                        self.phase = Phase::Gap;
                        let t = Self::rest_t(self.drum_passes());
                        return Some((self.level, self.bits(t)));
                    }
                    // Both low, and each flips on the first pass.
                    self.levels = [false; 2];
                    self.count = [1; 2];
                    self.passes = tone_step_t(self.tempo) / LOOP_T;
                    self.phase = Phase::Tone { second: false };
                }
                Phase::Tone { second } => {
                    let ch = second as usize;
                    self.level = self.levels[ch];
                    if self.period[ch] != 0 {
                        self.count[ch] -= 1;
                        if self.count[ch] == 0 {
                            self.count[ch] = self.period[ch];
                            self.levels[ch] = !self.levels[ch];
                        }
                    }
                    self.phase = Phase::Tone { second: !second };
                    if second {
                        self.passes -= 1;
                        if self.passes == 0 {
                            self.phase = Phase::Gap;
                        }
                    }
                    return Some((self.level, SLOT_BITS));
                }
                Phase::Quarters => {
                    if self.quarters == 0 {
                        self.phase = Phase::Gap;
                        continue;
                    }
                    self.quarters -= 1;
                    let passes = self.quarter_passes();
                    if self.code >> self.quarters & 1 == 1 {
                        self.start_noise(passes);
                    } else {
                        let t = Self::rest_t(passes);
                        return Some((self.level, self.bits(t)));
                    }
                }
                Phase::Noise => {
                    if self.passes == 0 {
                        self.phase = if self.code == DRUM_NOISE { Phase::Gap } else { Phase::Quarters };
                        continue;
                    }
                    self.passes -= 1;
                    let sample = self.pattern & 1 == 1;
                    self.pattern = self.pattern.rotate_right(1);
                    if !sample {
                        return Some((self.level, self.bits(NOISE_OFF_T)));
                    }
                    let bit = self.lfsr & 1 == 1;
                    self.lfsr = self.lfsr >> 1 ^ if bit { 0xB400 } else { 0 };
                    self.level = bit;
                    return Some((self.level, self.bits(NOISE_ON_T)));
                }
                Phase::Sweep { pair, part } => {
                    let pairs = SWEEP_PAIRS[(255 - self.tempo as usize).min(SWEEP_PAIRS.len() - 1)];
                    if pair == pairs {
                        self.phase = Phase::Gap;
                        continue;
                    }
                    // High, low, high for a half period each, then a short low.
                    let half = SWEEP_FIRST_T + SWEEP_STEP_T * pair as u32;
                    self.level = part % 2 == 0;
                    let t = if part == 3 { SWEEP_GAP_T } else { half };
                    self.phase = if part == 3 { Phase::Sweep { pair: pair + 1, part: 0 } } else { Phase::Sweep { pair, part: part + 1 } };
                    return Some((self.level, self.bits(t)));
                }
                Phase::Gap => {
                    self.phase = Phase::Fetch;
                    return Some((self.level, self.bits(STEP_OVERHEAD_T)));
                }
            }
        }
    }
}

/// A clip of any kind as the line's bitstream.
pub enum Stream<'a> {
    Pcm(Player<'a>),
    Beeper(Runs<Beeper<'a>>),
    Wham(Runs<Wham<'a>>),
}

impl<'a> Stream<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        if is_beeper(data) {
            Self::Beeper(Runs::new(Beeper::new(data)))
        } else if is_wham(data) {
            Self::Wham(Runs::new(Wham::new(data)))
        } else {
            Self::Pcm(Player::new(data))
        }
    }

    /// Write the next `out.len()` bytes, a multiple of
    /// `BYTES_PER_SAMPLE`; zeros after the end.
    pub fn fill(&mut self, out: &mut [u8]) {
        match self {
            Self::Pcm(p) => p.fill(out),
            Self::Beeper(b) => b.fill(out),
            Self::Wham(w) => w.fill(out),
        }
    }

    /// The bit rate the line must run at for this clip.
    pub fn bit_rate(&self) -> u32 {
        match self {
            Self::Wham(_) => WHAM_BIT_RATE,
            _ => BIT_RATE,
        }
    }

    pub fn done(&self) -> bool {
        match self {
            Self::Pcm(p) => p.done(),
            Self::Beeper(b) => b.done(),
            Self::Wham(w) => w.done(),
        }
    }
}

/// Chunks the line has sent `elapsed_us` after the start at `bit_rate`,
/// by the clock.
pub const fn chunks_in(elapsed_us: u64, bit_rate: u32) -> u32 {
    (elapsed_us / chunk_us(bit_rate)) as u32
}

/// Keeps the ring filled ahead of the DMA. Chunks are counted from the
/// start of the transfer; chunk `i` lives in slot `i % SLOTS`.
///
/// Where the DMA is comes from the clock: the line's bit clock and the
/// timer are divided from one crystal, so the time since the start is
/// the chunk being read, to within the start's own few microseconds.
/// The chunk before it is left alone as well, for that margin.
///
/// The DMA channel's descriptor register is not used for it. It was,
/// taken as naming the descriptor being read, and on the C3 it names
/// the one two further on (measured 2026-11-12: slot 2 at the start,
/// then the clock's chunk plus two throughout). The pacer therefore
/// thought the DMA two chunks ahead of where it was and rewrote the
/// chunk being played with the one a lap later, which was heard as the
/// clip overlapping itself, as skips and as crackle, with not one
/// underrun counted. The register is still read, as a check: a slot
/// that is not `DSCR_AHEAD` on from the clock's chunk, give or take
/// one, is counted as a stray.
pub struct Pacer {
    /// The next chunk to write.
    written: u32,
    /// The chunk the DMA was at when last asked; never goes back.
    played: u32,
    underruns: u32,
    /// Times the DMA's slot was not where the clock put it.
    strays: u32,
}

impl Pacer {
    /// How far on from the chunk being read the DMA's descriptor
    /// register points.
    pub const DSCR_AHEAD: usize = 2;

    /// For a ring whose first `SLOTS - 1` slots were filled before the
    /// start.
    pub const fn new() -> Self {
        Self { written: SLOTS as u32 - 1, played: 0, underruns: 0, strays: 0 }
    }

    /// The chunk the DMA is at: the clock's `estimate`, never going
    /// back. `slot`, what the descriptor register names, is only
    /// checked against it.
    pub fn played(&mut self, slot: Option<usize>, estimate: u32) -> u32 {
        if let Some(slot) = slot {
            let n = SLOTS as u32;
            let expected = (estimate + Self::DSCR_AHEAD as u32) % n;
            // 0 on the mark, 1 and n - 1 a chunk either side.
            let off = (slot as u32 % n + n - expected) % n;
            if off > 1 && off < n - 1 {
                self.strays += 1;
            }
        }
        self.played = self.played.max(estimate);
        self.played
    }

    /// The chunks to write now, with the DMA at chunk `played`: up to
    /// the slot before the one it just left. If the DMA has reached
    /// what was not written yet, that is an underrun: it is playing a
    /// lap-old chunk, and the writing resumes two chunks ahead of it.
    pub fn due(&mut self, played: u32) -> Range<u32> {
        if self.written <= played + 1 {
            self.underruns += 1;
            self.written = played + 2;
        }
        let end = played + SLOTS as u32 - 1;
        let from = self.written.min(end);
        self.written = end;
        from..end
    }

    pub fn underruns(&self) -> u32 {
        self.underruns
    }

    pub fn strays(&self) -> u32 {
        self.strays
    }
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole stream of a clip as bits, plus `extra` bytes after it.
    fn stream(pcm: &[u8], extra: usize) -> Vec<u8> {
        let mut p = Player::new(pcm);
        let mut out = vec![0xAAu8; p.periods() * BYTES_PER_SAMPLE + extra];
        // In uneven pieces, as the ring would never do, to show the
        // state carries across calls.
        let mut at = 0;
        let mut piece = BYTES_PER_SAMPLE;
        while at < out.len() {
            let n = piece.min(out.len() - at);
            p.fill(&mut out[at..at + n]);
            at += n;
            piece = piece % (7 * BYTES_PER_SAMPLE) + BYTES_PER_SAMPLE;
        }
        assert!(p.done());
        out
    }

    fn bit(stream: &[u8], i: usize) -> f64 {
        if stream[i / 8] >> (7 - i % 8) & 1 == 1 { 1.0 } else { -1.0 }
    }

    /// Three passes of a moving average `OSR` bits long: a low-pass
    /// with its nulls on the multiples of the PCM rate.
    fn lowpass(x: &[f64]) -> Vec<f64> {
        let mut y = x.to_vec();
        for _ in 0..3 {
            let mut acc = 0.0;
            let mut out = vec![0.0; y.len()];
            for i in 0..y.len() {
                acc += y[i];
                if i >= OSR {
                    acc -= y[i - OSR];
                }
                out[i] = acc / OSR as f64;
            }
            y = out;
        }
        y
    }

    fn sine(hz: f64, amplitude: f64, samples: usize) -> Vec<u8> {
        (0..samples)
            .map(|i| {
                let v = (2.0 * core::f64::consts::PI * hz * i as f64 / PCM_HZ as f64).sin();
                (128.0 + amplitude * v).round() as u8
            })
            .collect()
    }

    #[test]
    fn a_sine_comes_back_out_of_the_bits() {
        let pcm = sine(1000.0, 127.0, 4000);
        let s = stream(&pcm, 0);
        let p = Player::new(&pcm);
        // The input the modulator saw, bit by bit, in units of the feedback.
        let bits = s.len() * 8;
        let mut input = Vec::with_capacity(bits);
        for k in 1..=p.periods() {
            let (a, b) = (p.level(k - 1).unwrap() as f64, p.level(k).unwrap() as f64);
            for j in 1..=OSR {
                input.push((a + (b - a) * j as f64 / OSR as f64) / FULL as f64);
            }
        }
        let out: Vec<f64> = (0..bits).map(|i| bit(&s, i)).collect();
        let error: Vec<f64> = out.iter().zip(&input).map(|(o, i)| o - i).collect();
        let (signal, noise) = (lowpass(&input), lowpass(&error));
        // The body only: the ramps sit at the rail, outside the loop's range.
        let body = (RAMP + 100) * OSR..(RAMP + pcm.len() - 100) * OSR;
        let power = |v: &[f64]| v[body.clone()].iter().map(|x| x * x).sum::<f64>();
        let mean = signal[body.clone()].iter().sum::<f64>() / body.len() as f64;
        let ac = power(&signal) - mean * mean * body.len() as f64;
        let snr = 10.0 * (ac / power(&noise)).log10();
        println!("{} kbit/s, {} kHz carrier: 1 kHz sine at full scale, in-band SNR {snr:.1} dB", BIT_RATE / 1000, CARRIER_HZ / 1000);
        // The default, 32 pulse lengths at 16 kHz, is 34.5 dB. At 8 kHz
        // it is 19 to 25 with the bit rate, at 16 kHz 28 to 34, at
        // 32 kHz 37 to 42.
        let floor = match PERIODS {
            // A bit at a time: 43, 58 and 73 dB at 128, 256 and 512 kbit/s.
            _ if SIGMA_DELTA => 15.0 * (OSR as f64).log2() - 20.0,
            1 => 18.0,
            2 => 26.0,
            _ => 36.0,
        };
        assert!(snr > floor, "{snr:.1} dB");
    }

    #[test]
    fn starts_and_ends_on_a_released_line() {
        let pcm = [128u8; 800];
        let s = stream(&pcm, 64);
        let bits = Player::new(&pcm).periods() * OSR;
        let ones = |r: Range<usize>| r.filter(|&i| bit(&s, i) > 0.0).count();
        // The first and the last millisecond are all but silent.
        let ms = BIT_RATE as usize / 1000;
        assert!(ones(0..ms) <= ms / 20, "{} ones in the first ms", ones(0..ms));
        assert!(ones(bits - ms..bits) <= ms / 20, "{} ones in the last ms", ones(bits - ms..bits));
        // One pulse a period, in its middle.
        let periods = if SIGMA_DELTA { 0 } else { bits / PERIOD_BITS };
        for period in (0..bits / PERIOD_BITS).take(periods) {
            let at = period * PERIOD_BITS;
            let n = ones(at..at + PERIOD_BITS);
            let from = at + (PERIOD_BITS - n) / 2;
            assert_eq!(ones(from..from + n), n, "period {period}");
        }
        // The duty climbs through the up ramp and falls through the down one.
        let quarter = RAMP * OSR / 4;
        let up: Vec<usize> = (0..4).map(|q| ones(q * quarter..(q + 1) * quarter)).collect();
        assert!(up.windows(2).all(|w| w[0] < w[1]), "{up:?}");
        let tail = bits - RAMP * OSR;
        let down: Vec<usize> = (0..4).map(|q| ones(tail + q * quarter..tail + (q + 1) * quarter)).collect();
        assert!(down.windows(2).all(|w| w[0] > w[1]), "{down:?}");
        // Mid-scale is half the bits.
        let mid = RAMP * OSR..(RAMP + pcm.len()) * OSR;
        let duty = ones(mid.clone()) as f64 / mid.len() as f64;
        assert!((duty - 0.5).abs() < 0.01, "{duty}");
        // And after the end, zeros, whatever the buffer held.
        assert!(s[bits / 8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn the_loop_holds_at_full_scale() {
        // A full-scale square wave at 500 Hz, and the rails held.
        let mut pcm: Vec<u8> = (0..4000).map(|i| if i / 8 % 2 == 0 { 255 } else { 0 }).collect();
        pcm.extend([255u8; 2000]);
        pcm.extend([0u8; 2000]);
        let mut p = Player::new(&pcm);
        let mut buf = [0u8; CHUNK];
        let mut worst = 0;
        while !p.done() {
            p.fill(&mut buf);
            if p.next > RAMP + 16 && p.next < RAMP + pcm.len() {
                worst = worst.max(p.s1.abs()).max(p.s2.abs());
            }
        }
        let scale = if SIGMA_DELTA { FULL * OSR as i32 } else { FB };
        assert!(worst < 8 * scale, "integrators reached {} times full scale", worst / scale);
        // The held rails come out as the duty the level asks for.
        let s = stream(&pcm, 0);
        let duty = |from: usize, to: usize| {
            let r = (RAMP + from) * OSR..(RAMP + to) * OSR;
            r.clone().filter(|&i| bit(&s, i) > 0.0).count() as f64 / r.len() as f64
        };
        let high = 0.5 + 0.5 * 127.0 * LEVEL as f64 / FULL as f64;
        let low = 0.5 - 0.5 * 128.0 * LEVEL as f64 / FULL as f64;
        // Without feedback a held level is off by up to half a bit of pulse.
        let slack = 0.005 + if ORDER == 0 { 0.5 / PERIOD_BITS as f64 } else { 0.0 };
        assert!((duty(4100, 5900) - high).abs() < slack, "{}", duty(4100, 5900));
        assert!((duty(6100, 7900) - low).abs() < slack, "{}", duty(6100, 7900));
    }

    #[test]
    fn a_clip_lasts_its_samples_and_the_ramps() {
        let p = Player::new(&[128; 100]);
        assert_eq!(p.periods(), 100 + 2 * RAMP - 1);
        assert_eq!(p.level(0), Some(-FULL));
        assert_eq!(p.level(p.periods()), Some(-FULL));
        assert_eq!(p.level(p.periods() + 1), None);
        let mut p = Player::new(&[128; 100]);
        let mut buf = vec![0u8; (p.periods() - 1) * BYTES_PER_SAMPLE];
        p.fill(&mut buf);
        assert!(!p.done());
        p.fill(&mut [0u8; BYTES_PER_SAMPLE]);
        assert!(p.done());
        assert_eq!(127 * LEVEL * 100 / FULL, if SIGMA_DELTA { 69 } else { 94 }, "full scale is 0.95 of the rail, 0.7 a bit at a time");
    }

    /// A beeper clip of these runs, off first.
    fn beeper_clip(runs: &[u32]) -> Vec<u8> {
        let mut data = BEEPER_MAGIC.to_vec();
        data.extend(runs.iter().sum::<u32>().to_le_bytes());
        for &run in runs {
            let mut v = run;
            loop {
                let byte = (v & 0x7F) as u8;
                v >>= 7;
                data.push(byte | if v > 0 { 0x80 } else { 0 });
                if v == 0 {
                    break;
                }
            }
        }
        data
    }

    #[test]
    fn a_beeper_clip_comes_out_run_for_run() {
        // Short and long runs, ones that need two and three bytes, an
        // empty one in the middle (two offs in a row) and one at the start.
        let runs = [0, 6, 151, 1, 1, 130, 0, 20_000, 9, 3, 300, 127, 128];
        let data = beeper_clip(&runs);
        assert!(is_beeper(&data) && !is_beeper(&[128; 100]));
        assert_eq!(duration_ms(&data), runs.iter().sum::<u32>() * 1000 / BEEPER_TICK_HZ);
        assert_eq!(duration_ms(&[128; 16_000]), 2000);
        let per = BITS_PER_TICK as usize;
        let total = runs.iter().sum::<u32>() as usize * per;
        let mut out = vec![0xAAu8; total / 8 + 40];
        let mut s = Stream::new(&data);
        let mut at = 0;
        let mut piece = 1;
        while at < out.len() {
            assert!(!s.done() || at * 8 >= total);
            let n = piece.min(out.len() - at);
            s.fill(&mut out[at..at + n]);
            at += n;
            piece = piece % 13 + 1;
        }
        assert!(s.done());
        let mut i = 0;
        for (k, &run) in runs.iter().enumerate() {
            for _ in 0..run as usize * per {
                assert_eq!(bit(&out, i) > 0.0, k % 2 == 1, "bit {i}, run {k}");
                i += 1;
            }
        }
        assert!((i..out.len() * 8).all(|i| bit(&out, i) < 0.0), "zeros after the end");
        // A clip that is only its header is done at the first byte.
        let header = beeper_clip(&[]);
        let mut empty = Runs::new(Beeper::new(&header));
        let mut buf = [0xFFu8; 4];
        empty.fill(&mut buf);
        assert!(empty.done() && buf == [0; 4]);
        assert!(matches!(Stream::new(&[128; 100]), Stream::Pcm(_)));
    }

    /// A tune for the two-channel player.
    fn wham_tune(tempo: u8, octave: u8, c1: &[u8], c2: &[u8]) -> Vec<u8> {
        assert_eq!(c1.len(), c2.len());
        let mut data = WHAM_MAGIC.to_vec();
        data.extend([tempo, octave]);
        data.extend((c1.len() as u16).to_le_bytes());
        data.extend(c1);
        data.extend(c2);
        data
    }

    /// The whole stream of a tune as bits, one `bool` each.
    fn wham_bits(data: &[u8]) -> Vec<bool> {
        let mut s = Stream::new(data);
        assert!(matches!(s, Stream::Wham(_)));
        let mut bits = Vec::new();
        let mut buf = [0u8; 64];
        while !s.done() {
            s.fill(&mut buf);
            bits.extend((0..buf.len() * 8).map(|i| bit(&buf, i) > 0.0));
        }
        while bits.last() == Some(&false) {
            bits.pop();
        }
        bits
    }

    /// Bits of `t` T-states.
    fn t_bits(t: u32) -> f64 {
        t as f64 * WHAM_BIT_RATE as f64 / CPU_HZ as f64
    }

    /// The runs of ones and zeros in `bits` at least `min` long, after
    /// closing every gap shorter than that: the channel under the
    /// 36 kHz sharing.
    fn envelope(bits: &[bool], min: usize) -> Vec<(bool, usize)> {
        let mut smooth = bits.to_vec();
        let mut i = 0;
        while i < bits.len() {
            let j = (i..bits.len()).find(|&j| bits[j] != bits[i]).unwrap_or(bits.len());
            if !bits[i] && j - i < min && i > 0 && j < bits.len() {
                smooth[i..j].fill(true);
            }
            i = j;
        }
        let mut runs = Vec::new();
        let mut i = 0;
        while i < smooth.len() {
            let j = (i..smooth.len()).find(|&j| smooth[j] != smooth[i]).unwrap_or(smooth.len());
            runs.push((smooth[i], j - i));
            i = j;
        }
        runs
    }

    #[test]
    fn a_tone_has_its_pitch_and_half_the_line() {
        // Code 24 is 64 passes a flip: 3.5 MHz / (96 * 2 * 64) = 284.8 Hz.
        let tempo = 239;
        let data = wham_tune(tempo, 0, &[24], &[WHAM_REST]);
        let bits = wham_bits(&data);
        let step = t_bits(tone_step_t(tempo));
        assert!((bits.len() as f64) < step && bits.len() as f64 > step - t_bits(2 * 64 * LOOP_T));
        assert_eq!(duration_ms(&data), ((tone_step_t(tempo) + STEP_OVERHEAD_T) as u64 * 1000 / CPU_HZ as u64) as u32);
        // While the channel is high the line is its half of each pass.
        let runs = envelope(&bits, t_bits(LOOP_T) as usize);
        let half = t_bits(64 * LOOP_T);
        for &(on, len) in &runs[1..runs.len() - 1] {
            assert!((len as f64 - half).abs() < t_bits(LOOP_T), "{on} for {len} bits, {half} expected");
        }
        let highs = runs.iter().filter(|r| r.0).count();
        assert_eq!(highs, (17 * 256usize).div_ceil(128), "one high per 128 passes");
        let ones = bits.iter().filter(|&&b| b).count() as f64;
        assert!((ones / bits.len() as f64 - 0.25).abs() < 0.01, "half the time high, half of each pass");
        // An octave up is half the period, and two channels on the same
        // note fill the pass.
        let up = envelope(&wham_bits(&wham_tune(tempo, 1, &[24], &[WHAM_REST])), t_bits(LOOP_T) as usize);
        assert!((up[2].1 as f64 - half / 2.0).abs() < t_bits(LOOP_T));
        let both = wham_bits(&wham_tune(tempo, 0, &[24], &[24]));
        let ones = both.iter().filter(|&&b| b).count() as f64;
        assert!((ones / both.len() as f64 - 0.5).abs() < 0.01);
    }

    #[test]
    fn rests_and_drums_keep_the_time() {
        let tempo = 239;
        // A rest between two notes: the second note starts a silent
        // step and two gaps after the first.
        let bits = wham_bits(&wham_tune(tempo, 0, &[24, WHAM_REST, 24], &[WHAM_REST; 3]));
        let silent = t_bits(Wham::rest_t(16 * 256));
        let runs = envelope(&bits, t_bits(LOOP_T) as usize);
        let longest = runs.iter().filter(|r| !r.0).map(|r| r.1).max().unwrap() as f64;
        let expected = silent + t_bits(2 * STEP_OVERHEAD_T);
        assert!(longest >= expected && longest < expected + t_bits(130 * LOOP_T), "{longest} against {expected}");
        // A step of noise is as long as a silent one and is noise.
        let noise = wham_bits(&wham_tune(tempo, 0, &[DRUM_NOISE, 24], &[0xFF, WHAM_REST]));
        let n = t_bits(16 * 256 * NOISE_ON_T) as usize;
        let ones = noise[..n].iter().filter(|&&b| b).count() as f64 / n as f64;
        assert!((ones - 0.5).abs() < 0.05, "{ones}");
        let flips = noise[..n].windows(2).filter(|w| w[0] != w[1]).count();
        assert!(flips > 16 * 256 / 3, "{flips} flips");
        // A pattern with one bit in eight set samples an eighth as often.
        let sparse = wham_bits(&wham_tune(tempo, 0, &[DRUM_NOISE, 24], &[0x01, WHAM_REST]));
        // (Its passes are mostly the shorter kind, so the step is shorter.)
        let n = t_bits(16 * 256 * NOISE_OFF_T) as usize;
        let sparse_flips = sparse[..n].windows(2).filter(|w| w[0] != w[1]).count();
        assert!(sparse_flips < flips / 4, "{sparse_flips} against {flips}");
        // Quarters: 0x88 is noise for the first quarter only.
        let quarter = wham_bits(&wham_tune(tempo, 0, &[0x88, 24], &[0xFF, WHAM_REST]));
        let q = t_bits(4 * 256 * NOISE_ON_T) as usize;
        assert!(quarter[..q].windows(2).filter(|w| w[0] != w[1]).count() > 200);
        let tail = &quarter[q + 100..q + t_bits(Wham::rest_t(3 * 4 * 256)) as usize - 100];
        assert!(tail.windows(2).all(|w| w[0] == w[1]), "silent after the first quarter");
        // The sweep: sixteen pairs at this tempo, each period longer.
        let sweep = wham_bits(&wham_tune(tempo, 0, &[DRUM_SWEEP], &[0]));
        let highs: Vec<usize> = envelope(&sweep, 1).iter().filter(|r| r.0).map(|r| r.1).collect();
        assert_eq!(highs.len(), 32);
        // (To a bit: the two of a pair are the same length before rounding.)
        assert!(highs.windows(2).all(|w| w[0] <= w[1] + 1), "{highs:?}");
        assert!((highs[31] as f64 - t_bits(SWEEP_FIRST_T + 15 * SWEEP_STEP_T)).abs() < 2.0);
    }

    #[test]
    fn a_tune_ends_and_bad_ones_do_no_harm() {
        // Steps the header promises and the data lacks are not played.
        let mut data = wham_tune(239, 0, &[24, 24], &[24, 24]);
        data[6] = 200;
        let bits = wham_bits(&data);
        assert!((bits.len() as f64) < 2.0 * t_bits(tone_step_t(239) + STEP_OVERHEAD_T));
        // Codes past the table rest, and a tune of nothing is done at once.
        assert!(wham_bits(&wham_tune(239, 0, &[60, 0x7F], &[54, 100])).is_empty());
        assert!(wham_bits(&wham_tune(239, 9, &[], &[])).is_empty());
        assert!(!is_wham(b"WHM1") && !is_wham(&[128; 100]));
        // The periods are semitones: twelve on is half, within rounding.
        for i in 0..WHAM_PERIODS.len() - 12 {
            assert!((WHAM_PERIODS[i] as i32 - 2 * WHAM_PERIODS[i + 12] as i32).abs() <= 2, "{i}");
        }
    }

    /// Not a test: with `CLIP_DUMP=file` in the environment and a clip
    /// built in, writes the whole bitstream of the first clip, or of
    /// number `CLIP_INDEX`, there, to compare a player with its
    /// original (`cargo host-test dump_the_clip -- --ignored`).
    #[test]
    #[ignore]
    fn dump_the_clip() {
        let Ok(path) = std::env::var("CLIP_DUMP") else { return };
        let index = std::env::var("CLIP_INDEX").map_or(0, |i| i.parse().unwrap());
        let mut s = Stream::new(crate::melody::CLIP_DATA[index]);
        let mut out = Vec::new();
        let mut buf = [0u8; CHUNK];
        while !s.done() {
            s.fill(&mut buf);
            out.extend(buf);
        }
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn the_pacer_stays_ahead_and_off_the_slots_in_use() {
        let n = SLOTS as u32;
        let ahead = Pacer::DSCR_AHEAD;
        let mut p = Pacer::new();
        // Nothing to do at the start: all but one slot is filled.
        assert_eq!(p.played(Some(ahead), 0), 0);
        assert_eq!(p.due(0), n - 1..n - 1);
        // One chunk on, one chunk to write: the slot before the one just left.
        assert_eq!(p.played(Some(1 + ahead), 1), 1);
        assert_eq!(p.due(1), n - 1..n);
        let mut next = n;
        for played in 2..1000u32 {
            assert_eq!(p.played(Some((played as usize + ahead) % SLOTS), played), played);
            let due = p.due(played);
            assert_eq!(due.start, next);
            for chunk in due.clone() {
                let slot = chunk % n;
                assert_ne!(slot, played % n, "the slot being read");
                assert_ne!(slot, (played + n - 1) % n, "the slot just left");
                assert!(chunk > played + 1 && chunk < played + n, "ahead, and not over what is unplayed");
            }
            next = due.end;
        }
        assert_eq!((p.underruns(), p.strays()), (0, 0));
    }

    /// The playback loop of `src/clip.rs` against a model of the DMA:
    /// the ring holds, per slot, which piece of the stream was last
    /// written there, and the DMA reads chunk `i` from slot `i % SLOTS`
    /// through the `i`th chunk time while its register names the slot
    /// `register_ahead` on. `late` is how many ticks the clock's chunk
    /// trails the DMA's at each boundary. Returns, per chunk read, the
    /// piece it held and whether it was rewritten while being read.
    fn play_through(chunks: u32, ticks_per_chunk: u32, register_ahead: u32, late: u32, stall: Option<(u32, u32)>) -> Vec<(u32, bool)> {
        let n = SLOTS as u32;
        let mut ring = [u32::MAX; SLOTS];
        let mut next_piece = 0;
        // As `Sampler::play` fills the ring before the start.
        for slot in ring.iter_mut().take(SLOTS - 1) {
            *slot = next_piece;
            next_piece += 1;
        }
        let mut pacer = Pacer::new();
        let mut read = Vec::new();
        let mut at_start = ring[0];
        for t in 1..chunks * ticks_per_chunk {
            let chunk = t / ticks_per_chunk;
            if t % ticks_per_chunk == 0 {
                // The DMA leaves chunk - 1 and enters chunk.
                let left = ((chunk - 1) % n) as usize;
                read.push((at_start, ring[left] != at_start));
                at_start = ring[(chunk % n) as usize];
            }
            if stall.is_some_and(|(at, len)| t >= at && t < at + len) {
                continue;
            }
            let played = pacer.played(Some(((chunk + register_ahead) % n) as usize), t.saturating_sub(late) / ticks_per_chunk);
            for due in pacer.due(played) {
                ring[(due % n) as usize] = next_piece;
                next_piece += 1;
            }
        }
        read
    }

    #[test]
    fn the_playback_loop_feeds_the_dma_the_stream_in_order() {
        let in_order = |read: &[(u32, bool)]| read.iter().enumerate().all(|(i, &r)| r == (i as u32, false));
        // Two ticks a chunk, as the firmware's, and a slower and a
        // faster one; the register wherever it likes, since it is not
        // what paces; the clock on the DMA's boundary or a tick behind.
        for ticks in [1, 2, 5] {
            for ahead in 0..4 {
                for late in [0, 1] {
                    let read = play_through(200, ticks, ahead, late.min(ticks - 1), None);
                    assert!(in_order(&read), "{ticks} ticks a chunk, register {ahead} ahead, clock {late} late");
                }
            }
        }
        // A stall shorter than what is buffered is not heard.
        assert!(in_order(&play_through(200, 2, 2, 0, Some((60, 2 * (SLOTS as u32 - 4))))));
        // A longer one is: the DMA goes round on what it has. A lap
        // after the writing resumes the stream is whole again, later
        // than it would have been and in order, nothing rewritten
        // under the DMA.
        let stalled = 2 * (SLOTS as u32 + 4);
        let read = play_through(200, 2, 2, 0, Some((60, stalled)));
        assert!(!in_order(&read));
        let settled = (60 + stalled) as usize / 2 + SLOTS;
        assert!(read[settled..].windows(2).all(|w| w[1].0 == w[0].0 + 1 && !w[1].1), "{:?}", &read[settled..settled + 8]);
        assert!(read[settled].0 < settled as u32, "the stream is late, not skipped ahead");
    }

    #[test]
    fn the_pacer_goes_by_the_clock_and_counts_what_disagrees() {
        let n = SLOTS as u32;
        let ahead = Pacer::DSCR_AHEAD;
        let mut p = Pacer::new();
        // The register on the mark, and a chunk either side of it at a boundary.
        assert_eq!(p.played(Some((9 + ahead) % SLOTS), 9), 9);
        assert_eq!(p.played(Some((10 + ahead) % SLOTS), 9), 9);
        assert_eq!(p.played(Some((9 + ahead) % SLOTS), 10), 10);
        assert_eq!(p.strays(), 0);
        // The register naming the chunk being read, as it was once
        // taken to: a stray, and the clock is what counts.
        assert_eq!(p.played(Some(11), 11), 11);
        assert_eq!(p.strays(), 1);
        // No register to go by: the clock. And never backwards.
        assert_eq!(p.played(None, 12), 12);
        assert_eq!(p.played(Some((5 + ahead) % SLOTS), 5), 12);
        // The DMA has passed everything written (chunks 0 to n - 2): an
        // underrun, and the writing resumes two chunks ahead of it.
        assert_eq!(p.due(n + 1), n + 3..2 * n);
        assert_eq!(p.underruns(), 1);
        assert_eq!(chunks_in(chunk_us(BIT_RATE) * 5 + 1, BIT_RATE), 5);
        assert_eq!(chunk_us(512_000), 16_000);
    }
}

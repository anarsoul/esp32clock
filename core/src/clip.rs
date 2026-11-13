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
use crate::xm;

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
/// Where a `Player`'s samples come from.
#[allow(clippy::large_enum_variant, reason = "there is no heap to box the tune's state in, and one clip plays at a time")]
enum Body<'a> {
    /// Raw PCM, as it is.
    Pcm(&'a [u8]),
    /// A tracker module, played as it goes (`xm::Tune`).
    Tune(xm::Tune<'a>),
}

pub struct Player<'a> {
    body: Body<'a>,
    /// Samples in the body: known for PCM, and for a tune once it has
    /// ended; never, for a clip played round and round.
    len: Option<usize>,
    /// Whether the body starts over when it ends, with no ramp between.
    looping: bool,
    /// Samples a second of a PCM body.
    pcm_hz: u32,
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
    /// A player of bare PCM at `PCM_HZ`.
    pub const fn new(pcm: &'a [u8]) -> Self {
        Self::with(Body::Pcm(pcm), Some(pcm.len()))
    }

    /// A player of a PCM clip: bare samples at `PCM_HZ`, or behind
    /// `PCM_MAGIC` at the rate its header gives.
    pub fn pcm(data: &'a [u8]) -> Self {
        let mut player = Self::new(pcm_samples(data));
        player.pcm_hz = pcm_rate(data);
        player
    }

    /// A player of a tracker module.
    pub fn tune(tune: xm::Tune<'a>) -> Self {
        Self::with(Body::Tune(tune), None)
    }

    const fn with(body: Body<'a>, len: Option<usize>) -> Self {
        Self { body, len, looping: false, pcm_hz: PCM_HZ, next: 1, cur: -FULL, s1: 0, s2: 0, fb: if SIGMA_DELTA { -FULL * OSR as i32 } else { -FB } }
    }

    /// Play the body round and round, or once. For the alarm: PCM goes
    /// straight from its last sample to its first, and a tune follows
    /// its own loop.
    pub fn looped(mut self, looping: bool) -> Self {
        self.looping = looping;
        match &mut self.body {
            Body::Pcm(pcm) if looping && !pcm.is_empty() => self.len = None,
            Body::Pcm(_) => {}
            Body::Tune(tune) => tune.set_looping(looping),
        }
        self
    }

    /// The level at point `k`, `None` past the end. Point 0 is the
    /// rail the line idles at, and so is the last. Asked for the points
    /// in order, which is all a tune can give.
    fn level(&mut self, k: usize) -> Option<i32> {
        if k < RAMP {
            return Some(-FULL + FULL * k as i32 / RAMP as i32);
        }
        if self.len.is_none_or(|n| k < RAMP + n) {
            let sample = match &mut self.body {
                Body::Pcm(pcm) if self.looping && !pcm.is_empty() => Some(pcm[(k - RAMP) % pcm.len()]),
                Body::Pcm(pcm) => pcm.get(k - RAMP).copied(),
                Body::Tune(tune) => tune.next_sample(),
            };
            match sample {
                Some(s) => return Some((s as i32 - 128) * LEVEL),
                None => self.len = Some(k - RAMP),
            }
        }
        let n = self.len.unwrap_or(0);
        (k < RAMP + n + RAMP).then(|| -FULL * (k - RAMP - n + 1) as i32 / RAMP as i32)
    }

    /// Samples a second the body gives.
    pub fn sample_rate(&self) -> u32 {
        match &self.body {
            Body::Pcm(_) => self.pcm_hz,
            Body::Tune(tune) => tune.rate(),
        }
    }

    /// Whether the clip and its ramps have been written out; `fill`
    /// writes zeros from then on.
    pub fn done(&self) -> bool {
        self.len.is_some_and(|n| self.next >= 2 * RAMP + n)
    }

    /// PCM sample periods the whole stream lasts, ramps included, as
    /// far as is known: a tune's are not until it has ended.
    pub fn periods(&self) -> usize {
        2 * RAMP + self.len.unwrap_or(0) - 1
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

/// The first bytes of a PCM clip at another rate than `PCM_HZ`: the
/// magic, then the samples a second as a little-endian `u32`, which
/// must be `PCM_HZ` or twice it. PCM with no header is at `PCM_HZ`.
/// The modulator writes the same bits a sample either way, so the line
/// runs twice as fast for the higher rate (`Stream::bit_rate`).
pub const PCM_MAGIC: &[u8; 4] = b"PCM1";
const PCM_HEADER: usize = 8;

/// Samples a second of the PCM clip `data`.
pub const fn pcm_rate(data: &[u8]) -> u32 {
    if has_magic(data, PCM_MAGIC, PCM_HEADER) && u32::from_le_bytes([data[4], data[5], data[6], data[7]]) == 2 * PCM_HZ {
        2 * PCM_HZ
    } else {
        PCM_HZ
    }
}

/// The samples of the PCM clip `data`, without its header if it has one.
pub const fn pcm_samples(data: &[u8]) -> &[u8] {
    if has_magic(data, PCM_MAGIC, PCM_HEADER) { data.split_at(PCM_HEADER).1 } else { data }
}

/// The first bytes of a beeper clip; a clip with no magic is PCM.
pub const BEEPER_MAGIC: &[u8; 4] = b"BPR2";
/// Bytes before a beeper clip's runs: the magic and the length in ticks.
const BEEPER_HEADER: usize = 8;
/// Ticks per second of a beeper clip's run lengths.
///
/// 512 kHz, 1.95 µs, since 2026-11-12. The first format (`BPR1`) had
/// 128 kHz, 7.8 µs or 27 of the Spectrum's T-states, which was fine
/// for Trantor and not for Chronos: Tim Follin's player shapes its
/// sound with the pulse width in steps of 14 T-states, half a tick, so
/// widths that should have moved smoothly came out a tick this way or
/// that from pulse to pulse.
pub const BEEPER_TICK_HZ: u32 = 512_000;
/// The bit rate the line runs at for a beeper clip, whatever
/// `CLIP_KBPS` says: a bit a tick.
pub const BEEPER_BIT_RATE: u32 = BEEPER_TICK_HZ;

/// Whether `data` is a beeper clip of the first format, which this
/// firmware would take for PCM: convert it again.
pub const fn is_old_beeper(data: &[u8]) -> bool {
    has_magic(data, b"BPR1", BEEPER_HEADER)
}
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
    } else if xm::is_tune(data) {
        xm::duration_ms(data)
    } else {
        (pcm_samples(data).len() as u64 * 1000 / pcm_rate(data) as u64) as u32
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
    /// The clip's runs, and those still to play.
    all: &'a [u8],
    runs: &'a [u8],
    on: bool,
    /// Whether the clip starts over when it ends.
    looping: bool,
}

impl<'a> Beeper<'a> {
    /// `data` must be a beeper clip (`is_beeper`).
    pub fn new(data: &'a [u8]) -> Self {
        let all = &data[BEEPER_HEADER.min(data.len())..];
        // The first run read is an off one.
        Self { all, runs: all, on: true, looping: false }
    }

    /// Play the clip round and round, or once.
    pub fn looped(mut self, looping: bool) -> Self {
        self.looping = looping;
        self
    }
}

impl RunSource for Beeper<'_> {
    fn next_run(&mut self) -> Option<(bool, u32)> {
        if self.runs.is_empty() && self.looping && !self.all.is_empty() {
            // Round again: the first run is an off one once more.
            self.runs = self.all;
            self.on = true;
        }
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
        Some((self.on, ticks))
    }
}

/// A clip of any kind as the line's bitstream.
#[allow(clippy::large_enum_variant, reason = "there is no heap to box the tune's state in, and one clip plays at a time")]
pub enum Stream<'a> {
    /// PCM, or a tracker module through the same modulator.
    Pcm(Player<'a>),
    Beeper(Runs<Beeper<'a>>),
}

impl<'a> Stream<'a> {
    /// The clip `data` as a stream, played once or, `looping`, round
    /// and round with no gap, which never ends.
    pub fn new(data: &'a [u8], looping: bool) -> Self {
        if is_beeper(data) {
            Self::Beeper(Runs::new(Beeper::new(data).looped(looping)))
        } else if xm::is_tune(data) {
            Self::Pcm(Player::tune(xm::Tune::new(data)).looped(looping))
        } else {
            Self::Pcm(Player::pcm(data).looped(looping))
        }
    }

    /// Write the next `out.len()` bytes, a multiple of
    /// `BYTES_PER_SAMPLE`; zeros after the end.
    pub fn fill(&mut self, out: &mut [u8]) {
        match self {
            Self::Pcm(p) => p.fill(out),
            Self::Beeper(b) => b.fill(out),
        }
    }

    /// The bit rate the line must run at for this clip.
    pub fn bit_rate(&self) -> u32 {
        match self {
            // The modulator writes `OSR` bits a sample whatever the
            // rate: a tune at twice `PCM_HZ` needs the line twice as fast.
            Self::Pcm(p) => BIT_RATE / PCM_HZ * p.sample_rate(),
            Self::Beeper(_) => BEEPER_BIT_RATE,
        }
    }

    pub fn done(&self) -> bool {
        match self {
            Self::Pcm(p) => p.done(),
            Self::Beeper(b) => b.done(),
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
        let mut p = Player::new(&pcm);
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
        let mut p = Player::new(&[128; 100]);
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
        let per = 1;
        let total = runs.iter().sum::<u32>() as usize * per;
        let mut out = vec![0xAAu8; total / 8 + 40];
        let mut s = Stream::new(&data, false);
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
        assert!(matches!(Stream::new(&[128; 100], false), Stream::Pcm(_)));
        assert_eq!((Stream::new(&data, false).bit_rate(), Stream::new(&[128; 100], false).bit_rate()), (BEEPER_BIT_RATE, BIT_RATE));
        assert!(is_old_beeper(b"BPR1\0\0\0\0") && !is_old_beeper(&data));
    }

    #[test]
    fn pcm_at_twice_the_rate_runs_the_line_twice_as_fast() {
        let samples = [128u8; 16_000];
        let mut clip = PCM_MAGIC.to_vec();
        clip.extend((2 * PCM_HZ).to_le_bytes());
        clip.extend(samples);
        assert_eq!((pcm_rate(&clip), pcm_rate(&samples)), (2 * PCM_HZ, PCM_HZ));
        assert_eq!((pcm_samples(&clip).len(), pcm_samples(&samples).len()), (16_000, 16_000));
        assert_eq!((duration_ms(&clip), duration_ms(&samples)), (1000, 2000));
        assert_eq!((Stream::new(&clip, false).bit_rate(), Stream::new(&samples, false).bit_rate()), (2 * BIT_RATE, BIT_RATE));
        // The same samples make the same bits; only the line's clock differs.
        let fill = |data: &[u8]| {
            let mut s = Stream::new(data, false);
            let mut out = vec![0u8; 64 * BYTES_PER_SAMPLE];
            s.fill(&mut out);
            out
        };
        assert_eq!(fill(&clip), fill(&samples));
        // A header with a rate the player does not have is PCM at the
        // usual rate, header and all: nothing worse than eight odd samples.
        let mut odd = clip.clone();
        odd[4..8].copy_from_slice(&11_025u32.to_le_bytes());
        assert_eq!(pcm_rate(&odd), PCM_HZ);
    }

    #[test]
    fn a_looping_clip_goes_round_with_no_gap() {
        // A beeper clip: the bits of one pass, then the same again at
        // once, and never done.
        let runs = [3, 5, 200, 7, 11, 130];
        let data = beeper_clip(&runs);
        let pass = runs.iter().sum::<u32>() as usize;
        let mut s = Stream::new(&data, true);
        let mut out = vec![0u8; (3 * pass).div_ceil(8) + 8];
        for piece in out.chunks_mut(5) {
            s.fill(piece);
        }
        assert!(!s.done());
        for i in 0..3 * pass {
            assert_eq!(bit(&out, i), bit(&out, i % pass), "bit {i}");
        }
        assert!((0..pass).any(|i| bit(&out, i) > 0.0));
        // PCM: from the last sample to the first with no ramp between,
        // so the levels go round with the clip's length.
        let pcm: Vec<u8> = (0..50u8).map(|i| 100 + i).collect();
        let mut p = Player::new(&pcm).looped(true);
        let levels: Vec<i32> = (RAMP..RAMP + 175).map(|k| p.level(k).unwrap()).collect();
        assert!((0..125).all(|i| levels[i] == levels[i + 50]) && levels[49] != levels[50]);
        assert_eq!(levels[50], (100 - 128) * LEVEL);
        let mut buf = [0u8; 64 * BYTES_PER_SAMPLE];
        for _ in 0..20 {
            p.fill(&mut buf);
        }
        assert!(!p.done());
        // Played once it still ends, and a clip of nothing ends at once
        // looping or not.
        let mut once = Stream::new(&data, false);
        once.fill(&mut out);
        assert!(once.done());
        let header = beeper_clip(&[]);
        let mut nothing = Stream::new(&header, true);
        nothing.fill(&mut out[..4]);
        assert!(nothing.done());
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
        let mut s = Stream::new(crate::melody::CLIP_DATA[index], false);
        let mut out = Vec::new();
        let mut buf = [0u8; CHUNK];
        while !s.done() {
            s.fill(&mut buf);
            out.extend(buf);
        }
        std::fs::write(path, out).unwrap();
    }

    /// Not a test either: with `CLIP_PCM_DUMP=file`, writes the samples a
    /// built-in tracker module gives the modulator, 8 bits unsigned at
    /// `PCM_HZ`, to compare the player with another's render.
    #[test]
    #[ignore]
    fn dump_the_tune() {
        let Ok(path) = std::env::var("CLIP_PCM_DUMP") else { return };
        let index = std::env::var("CLIP_INDEX").map_or(0, |i| i.parse().unwrap());
        let data = crate::melody::CLIP_DATA[index];
        // `CLIP_RAW` asks for the mix before the shaping, as `i16`.
        if std::env::var("CLIP_RAW").is_ok() {
            let out: Vec<u8> = xm::raw_mix(data).iter().flat_map(|s| s.to_le_bytes()).collect();
            return std::fs::write(path, out).unwrap();
        }
        let mut tune = xm::Tune::new(data);
        let mut out = Vec::new();
        while let Some(s) = tune.next_sample() {
            out.push(s);
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

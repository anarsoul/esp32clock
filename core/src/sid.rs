// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A Commodore 64 tune as a clip: the SID chip, played from a record
//! of what a tune's player routine wrote to it.
//!
//! A `.sid` file is not notes but the program that played them, 6502
//! code to be called once a frame, each call writing some of the
//! chip's 25 registers. `scripts/sid2clip.py` runs that code on the
//! host and keeps the writes of each frame (`MAGIC`); the clock has the
//! chip. A tune of a minute is ten or twenty kilobytes.
//!
//! The chip here is written for the clock from the SID's published
//! behaviour, and is stepped 64 000 times a second rather than at
//! the C64's megahertz: some fifteen of its clock cycles at a time.
//! Per voice: the 24-bit phase accumulator; triangle (with ring
//! modulation), sawtooth, pulse and noise, a combined waveform as the
//! AND of its parts (near what an 8580 does; a 6581's are quieter);
//! hard sync and the test bit; the envelope with the chip's rate
//! periods, its exponential decay, and the rate counter's wrap that
//! delays a note when a slow rate is swapped for a fast one, which
//! tunes are written around. Then the filter, a two-integrator loop
//! with the register's resonance and its three taps, the routing of
//! voices into or past it, voice 3's mute and the volume.
//!
//! Not played: samples through the volume register (the record keeps a
//! frame's writes, not their timing within it) and anything read back
//! from the chip, which the converter answers with nought.
//!
//! The steps are averaged down to `RATE` and shaped as a tracker
//! module is (`shape::Shaper`).

use crate::clip::PCM_HZ;
use crate::shape::Shaper;

/// The first bytes of a converted tune.
pub const MAGIC: &[u8; 4] = b"SID1";
/// The header: magic; flags (bit 0: an 8580, else a 6581); the
/// limiter's drive and release (0 for the shaper's own) and how many
/// times the high-pass is applied; the C64's clock in Hz as `u32`; the
/// length in ms, played once; the clock cycles a frame; the frames;
/// the file offset the tune starts over from when it is played round
/// and round; the high-pass's section as five `i16` in Q14; the steps
/// of the chip a sample (2 or 4; 0 for `OVERSAMPLE`); one spare.
const HEADER: usize = 40;

/// Samples a second a tune gives: twice the PCM clips' 8 kHz.
pub const RATE: u32 = 2 * PCM_HZ;
/// Steps of the chip an output sample, unless the clip says 2. A
/// waveform's edge lands on a step, so its harmonics past half the
/// stepping rate fold back: at four a 2 kHz note's are 30 dB down, at
/// two 24, for half the processor's time (26 % of it at four on the
/// bench, 2026-11-13, where a module of four channels takes 12 %).
const OVERSAMPLE: u32 = 4;
/// Steps of the chip a second at that.
#[cfg(test)]
const MIX_HZ: u32 = RATE * OVERSAMPLE;

/// The registers a record may write.
pub const REGISTERS: u8 = 25;

/// The control register.
const GATE: u8 = 0x01;
const SYNC: u8 = 0x02;
const RING: u8 = 0x04;
const TEST: u8 = 0x08;
const TRIANGLE: u8 = 0x10;
const SAWTOOTH: u8 = 0x20;
const PULSE: u8 = 0x40;
const NOISE: u8 = 0x80;

/// The accumulator here is the chip's 24 bits with 8 more under them,
/// so its top bit is the chip's bit 23.
const MSB: u32 = 1 << 31;
/// The noise register after a reset.
const NOISE_RESET: u32 = 0x7F_FFF8;

/// Clock cycles between envelope steps for each of the sixteen rates.
const RATE_PERIOD: [u16; 16] = [9, 32, 63, 95, 149, 220, 267, 313, 392, 977, 1954, 3126, 3907, 11720, 19532, 31251];
/// The rate counter is 15 bits and steps the envelope when it equals
/// the period: one already past a newly set, shorter period goes all
/// the way round first.
const RATE_WRAP: u32 = 0x8000 << 8;

/// A 6581's cutoff in Hz at every 128th value of the 11-bit register:
/// flat at the bottom, steep in the middle. A typical chip; they
/// differ from one to the next by a factor of two.
const CUTOFF_6581: [u16; 17] = [220, 230, 250, 300, 420, 780, 1600, 3200, 5000, 7200, 9500, 12000, 14500, 16000, 17100, 17700, 18000];
/// An 8580's cutoff is a straight line to this.
const CUTOFF_8580_MAX: u32 = 12_500;
/// 2π in Q16.
const TWO_PI: u64 = 411_775;
/// The filter's coefficient is held to 1 (10 kHz at four steps a
/// sample, 5 kHz at two): the loop is stable at every resonance under it, and the
/// output has nothing over 8 kHz anyway.
const W0_MAX: i32 = 1 << 16;
/// 1/Q in Q10 for each resonance, Q from 0.707 to 1.707.
const DAMPING: [i32; 16] = {
    let mut table = [0; 16];
    let mut r = 0;
    while r < 16 {
        table[r] = 1024 * 15_000 / (10_605 + 1000 * r as i32);
        r += 1;
    }
    table
};
/// Where the integrators are held, far outside what they reach.
const STATE_MAX: i32 = 1 << 24;

/// Whether `data` is a converted SID tune.
pub const fn is_tune(data: &[u8]) -> bool {
    data.len() >= HEADER && data[0] == MAGIC[0] && data[1] == MAGIC[1] && data[2] == MAGIC[2] && data[3] == MAGIC[3]
}

/// How long the tune plays once, as the converter worked it out.
pub const fn duration_ms(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[12], data[13], data[14], data[15]])
}

fn byte(d: &[u8], at: usize) -> u8 {
    d.get(at).copied().unwrap_or(0)
}

fn word(d: &[u8], at: usize) -> u16 {
    byte(d, at) as u16 | (byte(d, at + 1) as u16) << 8
}

fn long(d: &[u8], at: usize) -> u32 {
    word(d, at) as u32 | (word(d, at + 2) as u32) << 16
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Attack,
    DecaySustain,
    Release,
}

#[derive(Clone, Copy)]
struct Voice {
    freq: u16,
    /// The pulse width, 12 bits.
    pw: u16,
    control: u8,
    attack_decay: u8,
    sustain_release: u8,
    acc: u32,
    /// What the accumulator gains a step.
    step: u32,
    noise: u32,
    /// The waveform output last made, 12 bits: with no waveform chosen
    /// the chip's converter keeps what it had.
    held: u16,
    env: u8,
    phase: Phase,
    /// Clock cycles since the envelope's last step, in 256ths.
    rate_counter: u32,
    /// The decay's divider: steps of the rate counter an envelope step.
    exp_counter: u8,
    exp_period: u8,
    /// The envelope came down to nought and stays until the next gate.
    hold_zero: bool,
}

impl Voice {
    const RESET: Self = Self {
        freq: 0,
        pw: 0,
        control: 0,
        attack_decay: 0,
        sustain_release: 0,
        acc: 0,
        step: 0,
        noise: NOISE_RESET,
        held: 0x800,
        env: 0,
        phase: Phase::Release,
        rate_counter: 0,
        exp_counter: 0,
        exp_period: 1,
        hold_zero: true,
    };

    /// One step of the oscillator; whether the accumulator's top bit
    /// rose, which is what a synced voice restarts on.
    fn advance(&mut self) -> bool {
        if self.control & TEST != 0 {
            self.acc = 0;
            return false;
        }
        let before = self.acc;
        self.acc = before.wrapping_add(self.step);
        // The noise register shifts when the chip's bit 19 rises.
        let rises = |acc: u32| ((acc >> 27) + 1) >> 1;
        for _ in 0..rises(self.acc).wrapping_sub(rises(before)) & 0xF {
            let bit = (self.noise >> 22 ^ self.noise >> 17) & 1;
            self.noise = (self.noise << 1 | bit) & 0x7F_FFFF;
        }
        before & MSB == 0 && self.acc & MSB != 0
    }

    /// The waveform's 12 bits; `source` is the accumulator of the voice
    /// this one is ring modulated by.
    fn waveform(&mut self, source: u32) -> u16 {
        let c = self.control;
        if c & 0xF0 == 0 {
            return self.held;
        }
        let top = (self.acc >> 20) as u16;
        let mut out = 0xFFF;
        if c & TRIANGLE != 0 {
            let fold = if c & RING != 0 { self.acc ^ source } else { self.acc };
            let ramp = (self.acc >> 19) as u16;
            out &= if fold & MSB != 0 { !ramp } else { ramp } & 0xFFF;
        }
        if c & SAWTOOTH != 0 {
            out &= top;
        }
        if c & PULSE != 0 && c & TEST == 0 && top < self.pw {
            out = 0;
        }
        if c & NOISE != 0 {
            // With another waveform the register is pulled to nought.
            let n = self.noise;
            out = if c & 0x70 != 0 { 0 } else { ((n >> 11 & 0x800) | (n >> 10 & 0x400) | (n >> 7 & 0x200) | (n >> 5 & 0x100) | (n >> 4 & 0x80) | (n >> 1 & 0x40) | (n << 1 & 0x20) | (n << 2 & 0x10)) as u16 };
        }
        self.held = out;
        out
    }

    /// The envelope `cycles` clock cycles on, in 256ths.
    fn envelope(&mut self, cycles: u32) {
        let rate = match self.phase {
            Phase::Attack => self.attack_decay >> 4,
            Phase::DecaySustain => self.attack_decay & 0xF,
            Phase::Release => self.sustain_release & 0xF,
        };
        let period = (RATE_PERIOD[rate as usize] as u32) << 8;
        // Past it already: nothing until the counter has gone round.
        let past = self.rate_counter >= period;
        self.rate_counter += cycles;
        if past {
            if self.rate_counter < RATE_WRAP {
                return;
            }
            self.rate_counter -= RATE_WRAP;
        }
        while self.rate_counter >= period {
            self.rate_counter -= period;
            self.tick();
        }
    }

    /// The rate counter reached its period.
    fn tick(&mut self) {
        if self.phase == Phase::Attack {
            self.exp_counter = 0;
            self.env = self.env.wrapping_add(1);
            if self.env == 0xFF {
                self.phase = Phase::DecaySustain;
            }
        } else {
            self.exp_counter += 1;
            if self.exp_counter < self.exp_period {
                return;
            }
            self.exp_counter = 0;
            if self.hold_zero || (self.phase == Phase::DecaySustain && self.env == (self.sustain_release >> 4) * 0x11) {
                return;
            }
            self.env = self.env.wrapping_sub(1);
        }
        // The decay slows as the level falls: the chip's exponential.
        match self.env {
            0xFF => self.exp_period = 1,
            0x5D => self.exp_period = 2,
            0x36 => self.exp_period = 4,
            0x1A => self.exp_period = 8,
            0x0E => self.exp_period = 16,
            0x06 => self.exp_period = 30,
            0x00 => {
                self.exp_period = 1;
                self.hold_zero = true;
            }
            _ => {}
        }
    }

    fn set_control(&mut self, value: u8) {
        if value & GATE != 0 && self.control & GATE == 0 {
            self.phase = Phase::Attack;
            self.hold_zero = false;
        } else if value & GATE == 0 && self.control & GATE != 0 {
            self.phase = Phase::Release;
        }
        self.control = value;
    }
}

/// The chip.
struct Chip {
    voice: [Voice; 3],
    /// The filter's registers: the cutoff's 11 bits, resonance and
    /// routing, mode and volume.
    cutoff: u16,
    res_filt: u8,
    mode_vol: u8,
    /// The cutoff as the loop's coefficient, Q16, and the two
    /// integrators.
    w0: i32,
    band: i32,
    low: i32,
    new_model: bool,
    /// What an accumulator gains a step for a frequency of 1, in
    /// 256ths, and the clock cycles an output sample, in 256ths.
    phase_step: u64,
    cycles: u32,
    /// Steps to the envelopes' next: they move once an output sample,
    /// which is as fine as their fastest rate is heard.
    envelope_in: u32,
    /// Steps an output sample, and a second.
    oversample: u32,
    mix_hz: u32,
    /// The volume register's 0 to 15 as a multiplier in 4096ths.
    volume: i32,
}

impl Chip {
    fn new(clock_hz: u32, new_model: bool, oversample: u32) -> Self {
        let mix_hz = RATE * oversample;
        let mut chip = Self {
            voice: [Voice::RESET; 3],
            cutoff: 0,
            res_filt: 0,
            mode_vol: 0,
            w0: 0,
            band: 0,
            low: 0,
            new_model,
            phase_step: ((clock_hz as u64) << 16) / mix_hz as u64,
            cycles: (((clock_hz as u64) << 8) / RATE as u64) as u32,
            envelope_in: 0,
            oversample,
            mix_hz,
            volume: 0,
        };
        chip.set_cutoff();
        chip
    }

    fn write(&mut self, register: u8, value: u8) {
        match register {
            0..21 => {
                let v = &mut self.voice[register as usize / 7];
                match register % 7 {
                    0 => v.freq = v.freq & 0xFF00 | value as u16,
                    1 => v.freq = v.freq & 0x00FF | (value as u16) << 8,
                    2 => v.pw = v.pw & 0xF00 | value as u16,
                    3 => v.pw = v.pw & 0x0FF | (value as u16 & 0xF) << 8,
                    4 => v.set_control(value),
                    5 => v.attack_decay = value,
                    _ => v.sustain_release = value,
                }
                v.step = ((v.freq as u64 * self.phase_step) >> 8) as u32;
            }
            21 => self.cutoff = self.cutoff & 0x7F8 | value as u16 & 7,
            22 => self.cutoff = self.cutoff & 7 | (value as u16) << 3,
            23 => self.res_filt = value,
            24 => {
                self.mode_vol = value;
                self.volume = (value & 0xF) as i32 * 4096 / 15;
            }
            _ => {}
        }
        if matches!(register, 21 | 22) {
            self.set_cutoff();
        }
    }

    fn set_cutoff(&mut self) {
        let hz = if self.new_model {
            self.cutoff as u32 * CUTOFF_8580_MAX / 0x7FF
        } else {
            let (i, part) = (self.cutoff as usize >> 7, self.cutoff as u32 & 0x7F);
            let (a, b) = (CUTOFF_6581[i] as u32, CUTOFF_6581[i + 1] as u32);
            (a * (128 - part) + b * part) >> 7
        };
        self.w0 = ((hz as u64 * TWO_PI / self.mix_hz as u64) as i32).min(W0_MAX);
    }

    /// One step: the chip's output, 32640 for one voice at full level
    /// and volume.
    fn clock(&mut self) -> i32 {
        let mut rose = [false; 3];
        for (v, rose) in self.voice.iter_mut().zip(&mut rose) {
            *rose = v.advance();
        }
        if self.envelope_in == 0 {
            self.envelope_in = self.oversample;
            for v in &mut self.voice {
                v.envelope(self.cycles);
            }
        }
        self.envelope_in -= 1;
        // Voice 1 is synced to and ring modulated by voice 3, 2 by 1, 3 by 2.
        for i in 0..3 {
            if self.voice[i].control & SYNC != 0 && rose[(i + 2) % 3] {
                self.voice[i].acc = 0;
            }
        }
        let acc = [self.voice[0].acc, self.voice[1].acc, self.voice[2].acc];
        let (mut filtered, mut direct) = (0, 0);
        for (i, v) in self.voice.iter_mut().enumerate() {
            let out = ((v.waveform(acc[(i + 2) % 3]) as i32 - 0x800) * v.env as i32) >> 4;
            if self.res_filt & 1 << i != 0 {
                filtered += out;
            } else if i != 2 || self.mode_vol & 0x80 == 0 {
                direct += out;
            }
        }
        // The filter, which turns its input over as the chip's does.
        let damping = DAMPING[(self.res_filt >> 4) as usize] as i64;
        let high = ((self.band as i64 * damping) >> 10) as i32 - self.low - filtered;
        self.band = (self.band - ((self.w0 as i64 * high as i64) >> 16) as i32).clamp(-STATE_MAX, STATE_MAX);
        self.low = (self.low - ((self.w0 as i64 * self.band as i64) >> 16) as i32).clamp(-STATE_MAX, STATE_MAX);
        let mut out = direct;
        if self.mode_vol & 0x10 != 0 {
            out += self.low;
        }
        if self.mode_vol & 0x20 != 0 {
            out += self.band;
        }
        if self.mode_vol & 0x40 != 0 {
            out += high;
        }
        // Held to where the multiplication fits; the limiter is next.
        (out.clamp(-(1 << 19), 1 << 19) * self.volume) >> 12
    }
}

/// A tune being played.
pub struct Tune<'a> {
    d: &'a [u8],
    /// The next byte of the record, and the frames still to pass
    /// before it is read.
    cursor: usize,
    wait: u32,
    /// Time to the next frame: a step takes the clock's Hz out of a
    /// pool that a frame fills with its cycles times the steps a second.
    pool: i64,
    frame: i64,
    clock: i64,
    /// Where the record goes on from at its end, when played round.
    loop_at: usize,
    looping: bool,
    ended: bool,
    chip: Chip,
    shaper: Shaper,
}

impl<'a> Tune<'a> {
    /// `data` must be a converted tune (`is_tune`).
    pub fn new(data: &'a [u8]) -> Self {
        // Held to what a C64 could be, so that a broken header is a
        // wrong pitch and not an endless loop.
        let clock = long(data, 8).clamp(500_000, 2_000_000);
        let loop_at = long(data, 24) as usize;
        let oversample = if byte(data, 38) == 2 { 2 } else { OVERSAMPLE };
        Self {
            d: data,
            cursor: HEADER,
            wait: 0,
            pool: 0,
            frame: long(data, 16).clamp(256, 1 << 24) as i64 * (RATE * oversample) as i64,
            clock: clock as i64,
            loop_at: if (HEADER..data.len()).contains(&loop_at) { loop_at } else { HEADER },
            looping: false,
            ended: data.len() <= HEADER,
            chip: Chip::new(clock, byte(data, 4) & 1 != 0, oversample),
            shaper: Shaper::new(byte(data, 5), byte(data, 6), true, core::array::from_fn(|i| word(data, 28 + 2 * i) as i16), byte(data, 7), true),
        }
    }

    /// Play the tune round and round, from where the converter found
    /// it to repeat, or once.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Samples a second `next_sample` gives.
    pub fn rate(&self) -> u32 {
        RATE
    }

    /// The next sample, as `xm::Tune::next_sample` gives them; `None`
    /// when the tune has ended.
    pub fn next_sample(&mut self) -> Option<i16> {
        self.next_mix().map(|mix| self.shaper.run(mix))
    }

    /// The chip's output for one sample, before the shaping.
    fn next_mix(&mut self) -> Option<i32> {
        let mut sum = 0;
        for _ in 0..self.chip.oversample {
            while self.pool <= 0 {
                self.next_frame();
                if self.ended {
                    return None;
                }
                self.pool += self.frame;
            }
            self.pool -= self.clock;
            sum += self.chip.clock();
        }
        Some(sum / self.chip.oversample as i32)
    }

    /// A frame begins: its writes, if the record has any for it.
    fn next_frame(&mut self) {
        if self.wait > 0 {
            self.wait -= 1;
            return;
        }
        if self.cursor >= self.d.len() {
            if !self.looping {
                self.ended = true;
                return;
            }
            self.cursor = self.loop_at;
        }
        // Pairs of register and value, to a byte with the top bit set:
        // the frames to the next writes, less one.
        while let Some(&b) = self.d.get(self.cursor) {
            self.cursor += 1;
            if b & 0x80 != 0 {
                self.wait = (b & 0x7F) as u32;
                return;
            }
            self.chip.write(b, byte(self.d, self.cursor));
            self.cursor += 1;
        }
    }
}

/// The whole tune's mix before the shaping, scaled to fit `i16` with
/// the three voices at full level: for comparing the player with
/// another, from a test.
#[cfg(test)]
pub fn raw_mix(data: &[u8]) -> Vec<i16> {
    let mut t = Tune::new(data);
    let mut out = Vec::new();
    while let Some(mix) = t.next_mix() {
        out.push((mix / 4).clamp(-32768, 32767) as i16);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAL: u32 = 985_248;
    const FRAME: u32 = 19_656;

    fn chip() -> Chip {
        let mut c = Chip::new(PAL, true, OVERSAMPLE);
        c.write(24, 0x0F);
        c
    }

    /// A voice at full level at once: attack 0, sustain 15.
    fn voice(c: &mut Chip, n: u8, freq: u16, control: u8) {
        c.write(7 * n, freq as u8);
        c.write(7 * n + 1, (freq >> 8) as u8);
        c.write(7 * n + 5, 0x00);
        c.write(7 * n + 6, 0xF0);
        c.write(7 * n + 4, control | GATE);
    }

    fn run(c: &mut Chip, steps: usize) -> Vec<i32> {
        (0..steps).map(|_| c.clock()).collect()
    }

    fn hz(freq: u16) -> f64 {
        freq as f64 * PAL as f64 / (1u32 << 24) as f64
    }

    /// Upward crossings of the mean a second.
    fn pitch(out: &[i32]) -> f64 {
        let mean = out.iter().map(|&s| s as i64).sum::<i64>() / out.len() as i64;
        let ups = out.windows(2).filter(|w| (w[0] as i64) <= mean && (w[1] as i64) > mean).count();
        ups as f64 * MIX_HZ as f64 / out.len() as f64
    }

    fn rms(out: &[i32]) -> f64 {
        let mean = out.iter().map(|&s| s as f64).sum::<f64>() / out.len() as f64;
        (out.iter().map(|&s| (s as f64 - mean).powi(2)).sum::<f64>() / out.len() as f64).sqrt()
    }

    /// A clip of `groups`: each the writes of a frame and the frames to
    /// the next. No high-pass; `loop_group` is the group the tune
    /// starts over from.
    fn clip(groups: &[(&[(u8, u8)], u8)], loop_group: usize) -> Vec<u8> {
        let mut body = Vec::new();
        let mut loop_at = HEADER;
        let mut frames = 0u32;
        for (i, (writes, wait)) in groups.iter().enumerate() {
            if i == loop_group {
                loop_at = HEADER + body.len();
            }
            for &(r, v) in *writes {
                body.extend([r, v]);
            }
            body.push(0x80 | (wait - 1));
            frames += *wait as u32;
        }
        let mut out = MAGIC.to_vec();
        out.extend([1, 0, 0, 0]);
        out.extend(PAL.to_le_bytes());
        out.extend((frames as u64 * FRAME as u64 * 1000 / PAL as u64).to_le_bytes()[..4].iter());
        out.extend(FRAME.to_le_bytes());
        out.extend(frames.to_le_bytes());
        out.extend((loop_at as u32).to_le_bytes());
        out.extend([0; 12]);
        assert_eq!(out.len(), HEADER);
        out.extend(body);
        out
    }

    #[test]
    fn a_frequency_register_is_its_pitch_on_every_waveform() {
        for freq in [0x0450u16, 0x1CD6, 0x7000] {
            for wave in [TRIANGLE, SAWTOOTH, PULSE] {
                let mut c = chip();
                c.write(3, 0x08);
                voice(&mut c, 0, freq, wave);
                let out = run(&mut c, MIX_HZ as usize);
                assert!((pitch(&out[1000..]) - hz(freq)).abs() < hz(freq) * 0.002 + 1.5, "{freq:#x} {wave:#x}: {} for {}", pitch(&out[1000..]), hz(freq));
                // Full level: the 12 bits' swing, times the envelope.
                let (low, high) = (out[1000..].iter().min().unwrap(), out[1000..].iter().max().unwrap());
                assert!(*high > 32_000 && *low < -32_000, "{low} {high}");
            }
        }
        // The same on an NTSC machine's clock is that much higher.
        let mut c = Chip::new(1_022_727, false, OVERSAMPLE);
        c.write(24, 0x0F);
        voice(&mut c, 1, 0x1CD6, SAWTOOTH);
        let out = run(&mut c, MIX_HZ as usize);
        assert!((pitch(&out) - hz(0x1CD6) * 1_022_727.0 / PAL as f64).abs() < 2.0);
    }

    #[test]
    fn the_waveforms_have_their_shapes() {
        // A pulse is high for the part of the cycle past its width.
        for (pw, low_part) in [(0x800u16, 0.5), (0x400, 0.25), (0xC00, 0.75)] {
            let mut c = chip();
            c.write(2, pw as u8);
            c.write(3, (pw >> 8) as u8);
            voice(&mut c, 0, 0x1000, PULSE);
            let out = run(&mut c, 40_000);
            let low = out[1000..].iter().filter(|&&s| s < 0).count() as f64 / 39_000.0;
            assert!((low - low_part).abs() < 0.01, "{pw:#x}: {low}");
        }
        // A sawtooth climbs evenly and falls at once; a triangle climbs
        // and falls evenly, twice as steeply.
        let slopes = |wave| {
            let mut c = chip();
            voice(&mut c, 0, 0x0800, wave);
            let out = run(&mut c, 20_000);
            let steps: Vec<i32> = out[1000..].windows(2).map(|w| w[1] - w[0]).collect();
            let up = steps.iter().filter(|&&s| s > 0).count();
            let down = steps.iter().filter(|&&s| s < 0).count();
            let rise = steps.iter().filter(|&&s| s > 0).map(|&s| s as i64).sum::<i64>() / up as i64;
            (up as f64 / down as f64, rise)
        };
        let (saw, triangle) = (slopes(SAWTOOTH), slopes(TRIANGLE));
        assert!(saw.0 > 50.0, "{}", saw.0);
        assert!((triangle.0 - 1.0).abs() < 0.05, "{}", triangle.0);
        assert!((triangle.1 as f64 / saw.1 as f64 - 2.0).abs() < 0.1, "{} {}", triangle.1, saw.1);
        // Noise: a new value each time bit 19 rises, sixteen a cycle,
        // of no pattern.
        let mut c = chip();
        voice(&mut c, 0, 0x2000, NOISE);
        let out = run(&mut c, MIX_HZ as usize);
        let changes = out.windows(2).filter(|w| w[0] != w[1]).count() as f64;
        assert!((changes / (16.0 * hz(0x2000)) - 1.0).abs() < 0.05, "{changes}");
        let seen: std::collections::HashSet<i32> = out.iter().copied().collect();
        assert!(seen.len() > 200, "{}", seen.len());
        assert!(rms(&out) > 10_000.0);
        // With no waveform the output stays where it was.
        c.write(4, GATE);
        let held = run(&mut c, 100);
        assert!(held.iter().all(|&s| s == held[0]));
        // Combined waveforms are the AND of their parts: never above either.
        let (mut both, mut saw, mut pulse) = (chip(), chip(), chip());
        for (c, wave) in [(&mut both, SAWTOOTH | PULSE), (&mut saw, SAWTOOTH), (&mut pulse, PULSE)] {
            c.write(3, 0x08);
            voice(c, 0, 0x1000, wave);
        }
        for _ in 0..5000 {
            let (b, s, p) = (both.clock(), saw.clock(), pulse.clock());
            assert_eq!(b, s.min(p));
        }
    }

    #[test]
    fn ring_modulation_sync_and_the_test_bit() {
        // Ring modulation turns the triangle over while the other
        // voice's top bit is set.
        let (mut plain, mut ring) = (chip(), chip());
        voice(&mut plain, 0, 0x0800, TRIANGLE);
        voice(&mut ring, 0, 0x0800, TRIANGLE | RING);
        ring.voice[2].acc = MSB;
        let (a, b) = (run(&mut plain, 5000), run(&mut ring, 5000));
        assert!(a[1000..].iter().zip(&b[1000..]).all(|(a, b)| (a + b + 16).abs() <= 16));
        assert!(rms(&a[1000..]) > 10_000.0);
        // Sync: a slower voice is started over each cycle of the one
        // before it, and never gets past a third of its own.
        for (sync, top) in [(0, 0xFF0), (SYNC, 0x560)] {
            let mut c = chip();
            c.write(1, 0x30);
            voice(&mut c, 1, 0x1000, SAWTOOTH | sync);
            let peak = run(&mut c, 20_000)[1000..].iter().copied().max().unwrap();
            let expect = ((top - 0x800) * 255) >> 4;
            assert!((peak - expect).abs() < 1200, "{sync}: {peak} for {expect}");
        }
        // And the synced voice sounds at the pitch of its master.
        let mut c = chip();
        voice(&mut c, 0, 0x3000, 0);
        voice(&mut c, 1, 0x1000, SAWTOOTH | SYNC);
        assert!((pitch(&run(&mut c, MIX_HZ as usize)[1000..]) - hz(0x3000)).abs() < 3.0);
        // The test bit stops the oscillator at the start of its cycle
        // and holds a pulse high.
        let mut c = chip();
        c.write(3, 0x08);
        voice(&mut c, 0, 0x1000, PULSE | TEST);
        let out = run(&mut c, 2000);
        assert!(out[1000..].iter().all(|&s| s > 32_000) && c.voice[0].acc == 0);
    }

    /// Steps until the envelope of voice 0 is `there`, in ms.
    fn ms_until(c: &mut Chip, there: impl Fn(u8) -> bool) -> f64 {
        let mut steps = 0;
        while !there(c.voice[0].env) {
            c.clock();
            steps += 1;
            assert!(steps < 100 * MIX_HZ, "never there");
        }
        steps as f64 * 1000.0 / MIX_HZ as f64
    }

    #[test]
    fn the_envelope_takes_the_chips_times() {
        // The data sheet's: attack 2 ms to 8 s, decay and release 6 ms
        // to 24 s, of which rates 0, 8 and 11 here.
        for (rate, attack_ms, decay_ms) in [(0u8, 2.0, 6.0), (8, 100.0, 300.0), (11, 800.0, 2400.0)] {
            let mut c = chip();
            c.write(5, rate << 4 | rate);
            c.write(6, 0xF0 | rate);
            c.write(4, PULSE | GATE);
            let up = ms_until(&mut c, |e| e == 0xFF);
            assert!((up / attack_ms - 1.0).abs() < 0.2, "attack {rate}: {up}");
            assert_eq!(c.voice[0].phase, Phase::DecaySustain);
            // It stays there until the sustain level is lowered.
            run(&mut c, 5000);
            assert_eq!(c.voice[0].env, 0xFF);
            c.write(6, rate);
            let down = ms_until(&mut c, |e| e == 0);
            assert!((down / decay_ms - 1.0).abs() < 0.25, "decay {rate}: {down}");
        }
        // The decay stops at the sustain level and the release goes on
        // from there, at the release's rate, when the gate drops.
        let mut c = chip();
        c.write(5, 0x00);
        c.write(6, 0xA8);
        c.write(4, PULSE | GATE);
        ms_until(&mut c, |e| e == 0xAA);
        run(&mut c, 10_000);
        assert_eq!(c.voice[0].env, 0xAA);
        c.write(4, PULSE);
        let down = ms_until(&mut c, |e| e == 0);
        // From two thirds of the way up, with most of the time in the
        // slow tail: the whole release's 300 ms less what 255 to 170 takes.
        assert!((180.0..300.0).contains(&down), "{down}");
        run(&mut c, 1000);
        assert_eq!(c.voice[0].env, 0);
        // The decay is exponential: half the level takes a small part
        // of the time, as the divider grows from 1 to 30.
        let mut c = chip();
        c.write(5, 0x08);
        c.write(6, 0xF0);
        c.write(4, PULSE | GATE);
        ms_until(&mut c, |e| e == 0xFF);
        c.write(6, 0x00);
        let (half, whole) = (ms_until(&mut c, |e| e <= 0x80), ms_until(&mut c, |e| e == 0));
        let whole = half + whole;
        assert!(half * 4.0 < whole, "{half} {whole}");
        // A gate in the middle of a release attacks from where it is.
        let mut c = chip();
        c.write(5, 0x00);
        c.write(6, 0xF8);
        c.write(4, PULSE | GATE);
        ms_until(&mut c, |e| e == 0xFF);
        c.write(4, PULSE);
        ms_until(&mut c, |e| e <= 0x60);
        let from = c.voice[0].env;
        c.write(4, PULSE | GATE);
        // (Within 35 ms: the rate counter may have to go round first.)
        run(&mut c, 2500);
        assert!(c.voice[0].env == 0xFF && from > 0x50);
    }

    #[test]
    fn a_fast_rate_after_a_slow_one_waits_for_the_counter() {
        // The release at the slowest rate lets the rate counter run to
        // 20 000; the attack's period of 9 is behind it by then, and
        // the note starts when the counter has been round, 13 ms late.
        let mut c = chip();
        c.write(5, 0x00);
        c.write(6, 0x0F);
        c.write(4, PULSE | GATE);
        ms_until(&mut c, |e| e > 0x80);
        ms_until(&mut c, |e| e == 0);
        assert_eq!(c.voice[0].phase, Phase::DecaySustain);
        c.write(4, PULSE);
        c.voice[0].rate_counter = 0;
        run(&mut c, (20_000u64 * MIX_HZ as u64 / PAL as u64) as usize);
        c.write(4, PULSE | GATE);
        let late = ms_until(&mut c, |e| e > 0);
        assert!((late - 12_768.0 * 1000.0 / PAL as f64).abs() < 0.2, "{late}");
        // A player that sets the release to 0 a frame before the note,
        // the hard restart, is on time.
        c.write(4, PULSE);
        c.write(6, 0x00);
        run(&mut c, (FRAME as u64 * 2 * MIX_HZ as u64 / PAL as u64) as usize);
        c.write(4, PULSE | GATE);
        assert!(ms_until(&mut c, |e| e > 0) < 0.1);
    }

    /// The level of a sawtooth at `freq` through the filter in `mode`
    /// at `cutoff`, over its level past the filter.
    fn through(new_model: bool, mode: u8, cutoff: u16, resonance: u8, freq: u16) -> f64 {
        let level = |filtered: bool| {
            let mut c = Chip::new(PAL, new_model, OVERSAMPLE);
            c.write(21, cutoff as u8 & 7);
            c.write(22, (cutoff >> 3) as u8);
            c.write(23, resonance << 4 | filtered as u8);
            c.write(24, if filtered { mode } else { 0 } | 0x0F);
            voice(&mut c, 0, freq, TRIANGLE);
            rms(&run(&mut c, 40_000)[8000..])
        };
        level(true) / level(false)
    }

    #[test]
    fn the_filter_passes_what_its_mode_says() {
        const LOW: u8 = 0x10;
        const BAND: u8 = 0x20;
        const HIGH: u8 = 0x40;
        // An 8580 with the cutoff register at 100: 610 Hz. Notes at
        // 100 Hz and at 3.5 kHz, five octaves apart round it.
        let (bass, treble) = (0x06A7, 0xE900);
        assert!(through(true, LOW, 100, 0, bass) > 0.9 && through(true, LOW, 100, 0, treble) < 0.1);
        assert!(through(true, HIGH, 100, 0, treble) > 0.9 && through(true, HIGH, 100, 0, bass) < 0.1);
        assert!(through(true, BAND, 100, 0, bass) < 0.35 && through(true, BAND, 100, 0, treble) < 0.35);
        // A note at the cutoff: the band-pass has it, and resonance lifts it.
        let at = (610.0 * (1u32 << 24) as f64 / PAL as f64) as u16;
        let (flat, peaked) = (through(true, BAND, 100, 0, at), through(true, BAND, 100, 15, at));
        assert!(flat > 0.5 && peaked > 1.5 * flat, "{flat} {peaked}");
        // The same register value is another cutoff on a 6581: at 512,
        // 420 Hz where an 8580 has 3.1 kHz, with a 1.9 kHz note between.
        assert!(through(false, LOW, 512, 0, 0x8000) < 0.15 && through(true, LOW, 512, 0, 0x8000) > 0.8);
        // With no mode chosen a filtered voice is gone, and voice 3 can
        // be muted when it is not filtered.
        assert!(through(true, 0, 100, 0, bass) < 0.001);
        let mut c = chip();
        voice(&mut c, 2, 0x1000, SAWTOOTH);
        assert!(rms(&run(&mut c, 2000)) > 10_000.0);
        c.write(24, 0x8F);
        assert!(run(&mut c, 2000).iter().all(|&s| s == 0));
        // The volume register scales the lot.
        let mut c = chip();
        voice(&mut c, 0, 0x1000, SAWTOOTH);
        let full = rms(&run(&mut c, 4000)[2000..]);
        c.write(24, 0x05);
        assert!((rms(&run(&mut c, 4000)) / full - 1.0 / 3.0).abs() < 0.02);
    }

    #[test]
    fn the_filter_stays_bounded_everywhere() {
        for (new_model, steps) in [(false, 4), (true, 4), (false, 2), (true, 2)] {
            for cutoff in [0u16, 1, 100, 512, 1024, 1500, 2047] {
                for resonance in [0u8, 8, 15] {
                    for mode in [0x10u8, 0x20, 0x40, 0x70] {
                        let mut c = Chip::new(PAL, new_model, steps);
                        c.write(21, cutoff as u8 & 7);
                        c.write(22, (cutoff >> 3) as u8);
                        c.write(23, resonance << 4 | 7);
                        c.write(24, mode | 0x0F);
                        c.write(3, 0x08);
                        for n in 0..3 {
                            voice(&mut c, n, 0x0400 << (2 * n), PULSE);
                        }
                        let peak = run(&mut c, 30_000).iter().map(|s| s.abs()).max().unwrap();
                        assert!(peak < 12 * 32_640, "{new_model} {steps} {cutoff} {resonance} {mode:#x}: {peak}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_frames_land_on_their_times_and_the_tune_goes_round() {
        // Three frames of one note, a rest of four, two of another;
        // round from the rest.
        let data = clip(&[(&[(24, 0x0F), (5, 0x00), (6, 0xF0), (1, 0x10), (4, SAWTOOTH | GATE)], 3), (&[(4, SAWTOOTH)], 4), (&[(1, 0x20), (4, SAWTOOTH | GATE)], 2)], 1);
        assert!(is_tune(&data) && !is_tune(b"SID1") && !crate::xm::is_tune(&data));
        assert_eq!(data.len(), HEADER + 11 + 3 + 5);
        let samples_in = |frames: u32| (frames as f64 * FRAME as f64 * RATE as f64 / PAL as f64) as usize;
        assert_eq!(duration_ms(&data), 9 * FRAME * 1000 / PAL);
        assert_eq!(crate::clip::duration_ms(&data), duration_ms(&data));
        // Once: nine frames, and what the registers hold on the way.
        let mut t = Tune::new(&data);
        assert_eq!(t.rate(), 16_000);
        let mut states = Vec::new();
        while t.next_sample().is_some() {
            states.push((t.chip.voice[0].freq, t.chip.voice[0].control & GATE));
        }
        assert!(states.len().abs_diff(samples_in(9)) <= 1, "{}", states.len());
        for (frame, state) in [(0, (0x1000, 1)), (2, (0x1000, 1)), (3, (0x1000, 0)), (6, (0x1000, 0)), (7, (0x2000, 1)), (8, (0x2000, 1))] {
            assert_eq!(states[samples_in(frame) + 2], state, "frame {frame}");
            assert_eq!(states[samples_in(frame + 1) - 2], state, "frame {frame}");
        }
        assert_eq!(t.next_sample(), None);
        // Round and round: after the ninth frame the fourth again, with
        // the registers as the pass before left them.
        let mut t = Tune::new(&data);
        t.set_looping(true);
        let mut states = Vec::new();
        for _ in 0..samples_in(9 + 3 * 6) {
            assert!(t.next_sample().is_some());
            states.push((t.chip.voice[0].freq, t.chip.voice[0].control & GATE));
        }
        for (frame, state) in [(8, (0x2000, 1)), (9, (0x2000, 0)), (12, (0x2000, 0)), (13, (0x2000, 1)), (15, (0x2000, 0)), (19, (0x2000, 1))] {
            assert_eq!(states[samples_in(frame) + 2], state, "frame {frame}");
        }
        // A record with nothing in it, or cut in the middle of a write,
        // ends and does not hang.
        assert_eq!(Tune::new(&data[..HEADER]).next_sample(), None);
        let mut t = Tune::new(&data[..HEADER + 3]);
        t.set_looping(true);
        for _ in 0..1000 {
            t.next_sample();
        }
    }

    #[test]
    fn the_samples_are_the_notes_shaped_as_a_modules() {
        // A second of a 433 Hz sawtooth: it comes out at its pitch and at
        // full scale, in steps finer than the 8-bit one.
        let data = clip(&[(&[(24, 0x0F), (5, 0x00), (6, 0xF0), (0, 0xD6), (1, 0x1C), (4, SAWTOOTH | GATE)], 50)], 0);
        let mut t = Tune::new(&data);
        let mut out = Vec::new();
        while let Some(s) = t.next_sample() {
            out.push(s as i32);
        }
        let ups = out[2000..].windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count() as f64 * RATE as f64 / (out.len() - 2000) as f64;
        assert!((ups - hz(0x1CD6)).abs() < 3.0, "{ups}");
        let peak = out[2000..].iter().map(|s| s.abs()).max().unwrap();
        assert!((120 * crate::shape::STEP..=127 * crate::shape::STEP).contains(&peak), "{peak}");
        assert!(out.iter().filter(|&&s| s % crate::shape::STEP != 0).count() > out.len() / 2);
        assert_eq!(raw_mix(&data).len(), out.len());
        // At two steps of the chip a sample: the same length, pitch and level.
        let mut coarse = data.clone();
        coarse[38] = 2;
        let mut t = Tune::new(&coarse);
        let mut half = Vec::new();
        while let Some(s) = t.next_sample() {
            half.push(s as i32);
        }
        assert_eq!(half.len(), out.len());
        let ups = half[2000..].windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count() as f64 * RATE as f64 / (half.len() - 2000) as f64;
        assert!((ups - hz(0x1CD6)).abs() < 3.0, "{ups}");
        assert!(half[2000..].iter().map(|s| s.abs()).max().unwrap() >= 120 * crate::shape::STEP);
        // A tune that writes nothing is silence, exactly.
        let quiet = clip(&[(&[], 10)], 0);
        let mut t = Tune::new(&quiet);
        let mut n = 0;
        while let Some(s) = t.next_sample() {
            assert_eq!(s, 0);
            n += 1;
        }
        assert!(n > 3000);
    }

    #[test]
    fn a_built_in_tune_plays_to_its_length() {
        for data in crate::melody::CLIP_DATA.iter().filter(|d| is_tune(d)) {
            let mut t = Tune::new(data);
            let (mut n, mut energy) = (0u64, 0u64);
            while let Some(s) = t.next_sample() {
                assert!(s.unsigned_abs() as i32 <= 127 * crate::shape::STEP);
                energy += (s as i64 * s as i64) as u64;
                n += 1;
            }
            let ms = n * 1000 / RATE as u64;
            assert!(ms.abs_diff(duration_ms(data) as u64) <= 25, "{ms} for {}", duration_ms(data));
            // Music, not silence: a tenth of full scale at least.
            assert!((energy / n).isqrt() > 12 * crate::shape::STEP as u64);
            // And it goes round: twice the length and still playing.
            let mut t = Tune::new(data);
            t.set_looping(true);
            assert!((0..2 * n + 1000).all(|_| t.next_sample().is_some()));
        }
    }
}

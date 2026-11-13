// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The AY-3-8910, the sound chip of the ZX Spectrum 128 and its kin
//! (and the YM2149, which differs in its envelope's steps), for the
//! players that write its registers: `pt3`.
//!
//! Written for the clock from the chip's published behaviour. All of
//! it moves on one clock, the chip's divided by eight (a `TICK`): a
//! tone's output turns over every `period` ticks, the noise register
//! shifts every two noise periods, the envelope steps every envelope
//! period through the 32 positions of a ramp. The three channels are
//! added up as they are, each at its measured level: a machine that
//! buffers each output (a Pentagon's stereo) does so, while one that
//! ties the three pins together (a Spectrum 128) lets loud channels
//! press each other down, which is not modelled. Between two such changes
//! the output is a constant, so `run` goes from change to change and
//! not tick by tick, and what it returns, the output summed over the
//! ticks, is exact: its mean over a sample's ticks is the sample, with
//! nothing point-sampled and so nothing to alias.
//!
//! A generator nothing listens to is not a change to stop at; it is
//! moved on in one go at the end of the span.

/// The registers: three tone periods (two bytes each), the noise
/// period, the mixer, three levels, the envelope's period and shape.
pub const REGISTERS: u8 = 14;
/// Clock cycles a tick.
pub const TICK: u32 = 8;

const NOISE: usize = 6;
const MIXER: usize = 7;
const LEVEL: usize = 8;
const SHAPE: u8 = 13;
/// A level register's bit for "the envelope's level, not mine".
const BY_ENVELOPE: u8 = 0x10;
/// The shape register's bits.
const HOLD: u8 = 1;
const ALTERNATE: u8 = 2;
const ATTACK: u8 = 4;
const CONTINUE: u8 = 8;

/// The whole of a level, in the sixteenths `set_noise_level` counts in.
const FULL: u32 = 16;
/// Positions in one ramp of the envelope.
const RAMP: u8 = 32;
/// What a channel puts out at each of the envelope's 32 positions, of
/// 32767, on a YM2149 and on an AY-3-8910, whose envelope has sixteen
/// levels, each for two positions. A level register's sixteen values
/// are the odd entries on both. Measured on the chips, as the emulators
/// have them (these are Ayumi's tables, by Peter Sovietov): not the
/// data sheet's even 3 dB a level but some 2 dB a level at the top and
/// more than 3 at the bottom, so a note at level 12 is half of full
/// scale where the ideal has it at a third (the first cut of this
/// chip had the ideal, and its notes were quiet beside its drums).
const LEVELS_YM: [u16; 32] = [
    0, 0, 152, 253, 359, 457, 557, 656, 798, 973, 1149, 1323, 1590, 1911, 2230, 2548, 3031, 3640, 4251, 4865, 5789, 6932, 8073, 9211, 10935, 13121, 15315, 17512, 20813, 24838, 28833, 32767,
];
const LEVELS_AY: [u16; 32] = [
    0, 0, 327, 327, 473, 473, 690, 690, 1006, 1006, 1492, 1492, 2113, 2113, 3518, 3518, 4148, 4148, 6717, 6717, 9575, 9575, 12217, 12217, 16139, 16139, 20818, 20818, 26397, 26397, 32767, 32767,
];

/// What `Chip::run_parts` answers, each a sum over the ticks.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Parts {
    /// The output of the channels that do not have the noise let through.
    pub plain: u32,
    /// The output of those that do.
    pub noisy: u32,
    /// What those would have put out with the noise's bit always set:
    /// their tones and levels alone.
    pub open: u32,
    /// The ticks at which the noise's bit was set, or half of them
    /// while no channel listens to it.
    pub noise: u32,
}

/// The chip.
pub struct Chip {
    regs: [u8; REGISTERS as usize],
    /// Ticks between a tone's turns, the ticks since its last, and its
    /// output.
    period: [u32; 3],
    count: [u32; 3],
    high: [bool; 3],
    noise_period: u32,
    noise_count: u32,
    /// The noise register, 17 bits; its bit 0 is the output.
    noise: u32,
    env_period: u32,
    env_count: u32,
    /// Where in the ramp the envelope is, and which way the ramp goes.
    env_pos: u8,
    rising: bool,
    /// The level the envelope stopped at, once it has.
    held: Option<u8>,
    /// The chip's levels: a YM2149's or an AY's.
    levels: &'static [u16; 32],
    /// What a channel with the noise let through is mixed at, in
    /// sixteenths of its level.
    noise_level: u32,
}

impl Chip {
    /// A chip after its reset: `fine` for a YM2149.
    pub fn new(fine: bool) -> Self {
        Self {
            regs: [0; REGISTERS as usize],
            period: [1; 3],
            count: [0; 3],
            high: [false; 3],
            noise_period: 2,
            noise_count: 0,
            noise: 1,
            env_period: 1,
            env_count: 0,
            env_pos: 0,
            rising: false,
            held: None,
            levels: if fine { &LEVELS_YM } else { &LEVELS_AY },
            noise_level: FULL,
        }
    }

    /// Mix a channel that has the noise let through at this many
    /// sixteenths of its level, 1 to 16. Not the chip's doing but the
    /// clock's: its buzzer makes more of white noise than of the notes,
    /// and a tune's drums are such channels.
    pub fn set_noise_level(&mut self, sixteenths: u8) {
        self.noise_level = (sixteenths as u32).clamp(1, FULL);
    }

    /// What a register holds.
    pub fn register(&self, register: u8) -> u8 {
        self.regs.get(register as usize).copied().unwrap_or(0)
    }

    /// A write to a register. One to the shape register starts the
    /// envelope over, whatever it held.
    pub fn write(&mut self, register: u8, value: u8) {
        let Some(slot) = self.regs.get_mut(register as usize) else { return };
        *slot = value;
        let r = &self.regs;
        // A period of nought is one of one.
        match register {
            0..6 => {
                let c = register as usize / 2;
                self.period[c] = (r[2 * c] as u32 | (r[2 * c + 1] as u32 & 0xF) << 8).max(1);
            }
            6 => {
                self.noise_period = 2 * (r[NOISE] as u32 & 0x1F).max(1);
            }
            11 | 12 => {
                self.env_period = (r[11] as u32 | (r[12] as u32) << 8).max(1);
            }
            SHAPE => {
                self.env_count = 0;
                self.env_pos = 0;
                self.rising = value & ATTACK != 0;
                self.held = None;
            }
            _ => {}
        }
    }

    /// The envelope's level, 0 to 31.
    fn env_level(&self) -> u8 {
        match self.held {
            Some(level) => level,
            None if self.rising => self.env_pos,
            None => RAMP - 1 - self.env_pos,
        }
    }

    /// The envelope's period has passed.
    fn env_step(&mut self) {
        self.env_pos += 1;
        if self.env_pos < RAMP {
            return;
        }
        self.env_pos = 0;
        let shape = self.regs[SHAPE as usize];
        if shape & CONTINUE == 0 {
            self.held = Some(0);
        } else if shape & HOLD != 0 {
            // Where the ramp ended, or the other end.
            self.held = Some(if self.rising != (shape & ALTERNATE != 0) { RAMP - 1 } else { 0 });
        } else if shape & ALTERNATE != 0 {
            self.rising = !self.rising;
        }
    }

    fn noise_step(&mut self) {
        let n = self.noise;
        self.noise = n >> 1 | ((n ^ n >> 3) & 1) << 16;
    }

    /// The three channels' output now, 32767 for one at full level:
    /// what the channels without the noise in them give, what those
    /// with it do, and what they would with the noise's bit set.
    fn out(&self) -> (u32, u32, u32) {
        let mixer = self.regs[MIXER];
        let noise = self.noise & 1 != 0;
        let (mut plain, mut noisy, mut open) = (0, 0, 0);
        for c in 0..3 {
            // A mixer bit set is that source off, which leaves the
            // channel's level standing.
            let listens = mixer & 8 << c == 0;
            if self.high[c] || mixer & 1 << c != 0 {
                let level = self.regs[LEVEL + c];
                let index = if level & BY_ENVELOPE == 0 { 2 * (level & 0xF) + 1 } else { self.env_level() };
                let level = self.levels[index as usize] as u32;
                if listens {
                    let level = level * self.noise_level / FULL;
                    open += level;
                    if noise {
                        noisy += level;
                    }
                } else {
                    plain += level;
                }
            }
        }
        (plain, noisy, open)
    }

    /// The chip `ticks` ticks on: the sum of its output over them.
    pub fn run(&mut self, ticks: u32) -> u32 {
        let parts = self.run_parts(ticks);
        parts.plain + parts.noisy
    }

    /// The same in parts, for a player that treats a tune's drums
    /// apart: the channels that do not have the noise let through,
    /// those that do, and for those the tone and the noise each by itself.
    pub fn run_parts(&mut self, ticks: u32) -> Parts {
        if ticks == 0 {
            return Parts::default();
        }
        // A counter past a period that was made shorter since turns at
        // the next tick. (Here and not at the write: a period is two
        // registers, and may be short for the time between two writes.)
        for c in 0..3 {
            self.count[c] = self.count[c].min(self.period[c] - 1);
        }
        self.noise_count = self.noise_count.min(self.noise_period - 1);
        self.env_count = self.env_count.min(self.env_period - 1);
        // What can be heard: a tone or the noise through a channel
        // that is not at nought, the envelope by a channel that asks.
        let mixer = self.regs[MIXER];
        let (mut tone, mut noise, mut envelope) = ([false; 3], false, false);
        for (c, tone) in tone.iter_mut().enumerate() {
            let level = self.regs[LEVEL + c];
            if level & (BY_ENVELOPE | 0xF) != 0 {
                *tone = mixer & 1 << c == 0;
                noise |= mixer & 8 << c == 0;
                envelope |= level & BY_ENVELOPE != 0;
            }
        }
        let mut parts = Parts { noise: if noise { 0 } else { ticks / 2 }, ..Parts::default() };
        let mut left = ticks;
        while left > 0 {
            let moving = envelope && self.held.is_none();
            let mut n = left;
            for c in 0..3 {
                if tone[c] {
                    n = n.min(self.period[c] - self.count[c]);
                }
            }
            if noise {
                n = n.min(self.noise_period - self.noise_count);
            }
            if moving {
                n = n.min(self.env_period - self.env_count);
            }
            let (plain, noisy, open) = self.out();
            parts.plain += plain * n;
            parts.noisy += noisy * n;
            parts.open += open * n;
            if noise {
                parts.noise += (self.noise & 1) * n;
            }
            for c in 0..3 {
                if tone[c] {
                    self.count[c] += n;
                    if self.count[c] == self.period[c] {
                        self.count[c] = 0;
                        self.high[c] = !self.high[c];
                    }
                }
            }
            if noise {
                self.noise_count += n;
                if self.noise_count == self.noise_period {
                    self.noise_count = 0;
                    self.noise_step();
                }
            }
            if moving {
                self.env_count += n;
                if self.env_count == self.env_period {
                    self.env_count = 0;
                    self.env_step();
                }
            }
            left -= n;
        }
        // The rest, unheard, in one go.
        for c in 0..3 {
            if !tone[c] {
                let t = self.count[c] + ticks;
                if t >= self.period[c] {
                    self.high[c] ^= (t / self.period[c]) & 1 != 0;
                    self.count[c] = t % self.period[c];
                } else {
                    self.count[c] = t;
                }
            }
        }
        if !noise {
            let mut t = self.noise_count + ticks;
            while t >= self.noise_period {
                t -= self.noise_period;
                self.noise_step();
            }
            self.noise_count = t;
        }
        if !envelope {
            let mut t = self.env_count + ticks;
            while t >= self.env_period && self.held.is_none() {
                t -= self.env_period;
                self.env_step();
            }
            self.env_count = t % self.env_period;
        }
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chip a tick at a time, with nothing clever: the output, then
    /// every counter one on.
    struct Model {
        regs: [u8; 14],
        count: [u32; 3],
        high: [bool; 3],
        noise_count: u32,
        noise: u32,
        env_count: u32,
        /// Steps since the shape was written.
        env_steps: u32,
        fine: bool,
        noise_level: u32,
    }

    impl Model {
        fn new(fine: bool) -> Self {
            Self { regs: [0; 14], count: [0; 3], high: [false; 3], noise_count: 0, noise: 1, env_count: 0, env_steps: 0, fine, noise_level: 16 }
        }

        fn write(&mut self, r: u8, v: u8) {
            self.regs[r as usize] = v;
            if r == 13 {
                self.env_count = 0;
                self.env_steps = 0;
            }
        }

        /// The envelope's level from the shape's picture in the data
        /// sheet: the first ramp, then what the shape does for ever.
        fn env_level(&self) -> u32 {
            let shape = self.regs[13] & 0xF;
            let (ramp, at) = (self.env_steps / 32, self.env_steps % 32);
            let (up, down) = (at, 31 - at);
            let first = if shape & 4 != 0 { up } else { down };
            if ramp == 0 {
                return first;
            }
            match shape {
                0..8 | 9 | 15 => 0,
                8 => down,
                12 => up,
                11 | 13 => 31,
                10 => [down, up][ramp as usize % 2],
                _ => [up, down][ramp as usize % 2],
            }
        }

        fn tick(&mut self) -> u32 {
            let r = self.regs;
            let mut out = 0;
            for c in 0..3 {
                let tone = self.high[c] || r[7] & 1 << c != 0;
                let noise = self.noise & 1 != 0 || r[7] & 8 << c != 0;
                if tone && noise {
                    let level = r[8 + c];
                    // An AY's envelope has sixteen levels, a level
                    // register's sixteen values on either chip.
                    let index = if level & 0x10 == 0 {
                        2 * (level as u32 & 0xF) + 1
                    } else if self.fine {
                        self.env_level()
                    } else {
                        self.env_level() | 1
                    };
                    let level = if self.fine { LEVELS_YM } else { LEVELS_AY }[index as usize] as u32;
                    out += if r[7] & 8 << c == 0 { level * self.noise_level / 16 } else { level };
                }
            }
            for c in 0..3 {
                self.count[c] += 1;
                if self.count[c] >= (r[2 * c] as u32 | (r[2 * c + 1] as u32 & 0xF) << 8).max(1) {
                    self.count[c] = 0;
                    self.high[c] = !self.high[c];
                }
            }
            self.noise_count += 1;
            if self.noise_count >= 2 * (r[6] as u32 & 0x1F).max(1) {
                self.noise_count = 0;
                self.noise = self.noise >> 1 | ((self.noise ^ self.noise >> 3) & 1) << 16;
            }
            self.env_count += 1;
            if self.env_count >= (r[11] as u32 | (r[12] as u32) << 8).max(1) {
                self.env_count = 0;
                // Held shapes do not need the count; keep it from wrapping.
                self.env_steps = (self.env_steps + 1).min(1 << 30);
            }
            out
        }
    }

    struct Random(u64);

    impl Random {
        fn next(&mut self, below: u32) -> u32 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as u32) % below
        }
    }

    #[test]
    fn run_is_the_chip_tick_by_tick() {
        for seed in 0..40u64 {
            let fine = seed % 2 == 1;
            let (mut chip, mut model) = (Chip::new(fine), Model::new(fine));
            // The noise at all of its level, and at less.
            let noise_level = [16, 8, 5, 1][seed as usize / 2 % 4];
            chip.set_noise_level(noise_level);
            model.noise_level = noise_level as u32;
            let mut random = Random(seed);
            let mut heard = 0u64;
            for round in 0..400 {
                // Some writes, as a player's frame makes them: short
                // periods now and then, since they are the hard ones.
                for _ in 0..random.next(6) {
                    let r = random.next(14) as u8;
                    let short = random.next(4) == 0;
                    let v = match r {
                        1 | 3 | 5 => random.next(if short { 16 } else { 2 }),
                        12 => random.next(if short { 256 } else { 2 }),
                        0 | 2 | 4 | 11 if short => random.next(4),
                        _ => random.next(256),
                    } as u8;
                    chip.write(r, v);
                    model.write(r, v);
                }
                let ticks = match random.next(4) {
                    0 => random.next(3),
                    1 => 4000 + random.next(1000),
                    _ => 1 + random.next(40),
                };
                let expect: u32 = (0..ticks).map(|_| model.tick()).sum();
                assert_eq!(chip.run(ticks), expect, "seed {seed} round {round}: {ticks} ticks, registers {:?}", model.regs);
                heard += expect as u64;
            }
            assert!(heard > 1_000_000, "seed {seed}: silence proves nothing");
        }
    }

    /// The ticks at which the output of a chip with one thing sounding
    /// rises.
    fn rises(chip: &mut Chip, ticks: u32) -> Vec<u32> {
        let mut last = chip.run(1);
        let mut at = Vec::new();
        for t in 1..ticks {
            let now = chip.run(1);
            if now > last {
                at.push(t);
            }
            last = now;
        }
        at
    }

    #[test]
    fn a_period_is_its_pitch_and_the_mixer_gates() {
        // Channel B at period 0x1FD: 1 750 000 / 16 / 509 = 214.9 Hz.
        let mut chip = Chip::new(false);
        chip.write(2, 0xFD);
        chip.write(3, 0xF1);
        chip.write(7, 0b111_101);
        chip.write(9, 15);
        let at = rises(&mut chip, 20_000);
        assert!(at.windows(2).all(|w| w[1] - w[0] == 2 * 0x1FD), "{at:?}");
        assert_eq!(at.len(), 20);
        // Full level is the top of the table, a square wave's half the time.
        let sum = chip.run(2 * 0x1FD * 10);
        assert_eq!(sum, 32767 * 0x1FD * 10);
        // The tone switched off leaves the level standing; the level at
        // nought is silence; the other channels never spoke.
        chip.write(7, 0b111_111);
        assert_eq!(chip.run(1000), 32767 * 1000);
        chip.write(9, 0);
        assert_eq!(chip.run(1000), 0);
        // Three channels add up, each at its own level, 3 dB a step.
        let mut chip = Chip::new(false);
        chip.write(7, 0b111_111);
        for (c, level) in [(0, 15), (1, 13), (2, 11)] {
            chip.write(8 + c, level);
        }
        assert_eq!(chip.run(1), 32767 + 20818 + 12217);
        // A period of nought plays as one of one.
        let mut chip = Chip::new(false);
        chip.write(7, 0b111_110);
        chip.write(8, 15);
        assert_eq!(rises(&mut chip, 100).len(), 50);
    }

    #[test]
    fn the_levels_are_the_chips() {
        let db = |a: u16, b: u16| 20.0 * (a as f64 / b as f64).log10();
        for table in [&LEVELS_AY, &LEVELS_YM] {
            assert_eq!((table[0], table[1], table[31]), (0, 0, 32767));
            assert!(table.windows(2).all(|w| w[0] <= w[1]));
            // Sixteen levels over some 40 dB, by 2 to 5 dB a level and
            // not the data sheet's even 3.
            let levels: Vec<u16> = table.iter().skip(1).step_by(2).copied().collect();
            assert!((38.0..47.0).contains(&db(levels[15], levels[1])));
            assert!(levels[1..].windows(2).all(|w| (1.3..5.2).contains(&db(w[1], w[0]))), "{levels:?}");
        }
        // Three levels down from the top an AY is at half, not at the
        // data sheet's third; a YM is nearer the sheet.
        assert!(db(LEVELS_AY[31], LEVELS_AY[25]) < 6.5 && db(LEVELS_YM[31], LEVELS_YM[25]) > 7.5);
        // An AY's envelope has each level twice, a YM's every one once.
        assert!(LEVELS_AY.as_chunks::<2>().0.iter().all(|pair| pair[0] == pair[1]));
        assert!(LEVELS_YM[2..].windows(2).all(|w| w[0] < w[1]));
        // And a channel is given its level's: 13 and 11 beside 15.
        let mut chip = Chip::new(true);
        chip.write(7, 0b111_111);
        chip.write(8, 13);
        assert_eq!(chip.run(1), LEVELS_YM[27] as u32);
    }

    #[test]
    fn noise_is_noise_at_its_rate() {
        for np in [1u8, 5, 31] {
            let mut chip = Chip::new(false);
            chip.write(6, np);
            chip.write(7, 0b110_111);
            chip.write(8, 15);
            let ticks = 400_000;
            let out: Vec<bool> = (0..ticks).map(|_| chip.run(1) != 0).collect();
            // It may change only every 2·NP ticks, does so about half
            // the time, and is on about half the time.
            let changes: Vec<usize> = (1..out.len()).filter(|&i| out[i] != out[i - 1]).collect();
            assert!(changes.iter().all(|i| i % (2 * np as usize) == 0));
            let steps = ticks as f64 / (2.0 * np as f64);
            assert!((changes.len() as f64 / steps - 0.5).abs() < 0.03, "{np}: {}", changes.len());
            let on = out.iter().filter(|&&b| b).count() as f64 / ticks as f64;
            assert!((on - 0.5).abs() < 0.03, "{np}: {on}");
        }
        // The register is 17 bits of maximal length: it comes round
        // after 131 071 steps and not before.
        let mut chip = Chip::new(false);
        let mut steps = 0;
        loop {
            chip.noise_step();
            steps += 1;
            if chip.noise == 1 {
                break;
            }
            assert!(steps < 200_000);
        }
        assert_eq!(steps, (1 << 17) - 1);
        // Mixed lower when asked: a channel with the noise let through,
        // tone under it or not, and no other.
        let mut chip = Chip::new(false);
        chip.set_noise_level(4);
        chip.write(7, 0b110_110);
        chip.write(8, 15);
        let peak = |chip: &mut Chip| (0..2000).map(|_| chip.run(1)).max().unwrap();
        assert_eq!(peak(&mut chip), 32767 / 4);
        chip.write(7, 0b111_110);
        assert_eq!(peak(&mut chip), 32767);
        chip.write(7, 0b110_111);
        assert_eq!(peak(&mut chip), 32767 / 4);
        chip.set_noise_level(0);
        assert_eq!(peak(&mut chip), 32767 / 16);
        // The two sums: a channel with the noise in it apart from the
        // others, at its lowered level.
        let mut chip = Chip::new(false);
        chip.set_noise_level(8);
        chip.write(7, 0b110_111);
        chip.write(8, 15);
        chip.write(9, 13);
        let parts = chip.run_parts(4000);
        assert_eq!(parts.plain, 4000 * 20818);
        // The noisy channel has no tone, so it is its level while the
        // noise's bit is set: about half the time.
        assert_eq!((parts.open, parts.noisy), (4000 * (32767 / 2), parts.noise * (32767 / 2)));
        assert!((1600..2400).contains(&parts.noise), "{}", parts.noise);
        // With a tone under the noise, `open` is the tone alone.
        chip.write(0, 50);
        chip.write(7, 0b110_110);
        let parts = chip.run_parts(4000);
        assert_eq!(parts.open, 2000 * (32767 / 2));
        assert!(parts.noisy < parts.open && parts.noisy > parts.open / 4);
        // While nothing listens to the noise it counts as half set.
        chip.write(7, 0b111_110);
        assert_eq!(chip.run_parts(4000), Parts { plain: 2000 * 32767 + 4000 * 20818, noisy: 0, open: 0, noise: 2000 });
        // Noise and tone on one channel: on only where both are.
        let (mut both, mut tone, mut noise) = (Chip::new(false), Chip::new(false), Chip::new(false));
        for (chip, mixer) in [(&mut both, 0b110_110), (&mut tone, 0b111_110), (&mut noise, 0b110_111)] {
            chip.write(0, 9);
            chip.write(6, 3);
            chip.write(7, mixer);
            chip.write(8, 15);
        }
        for _ in 0..5000 {
            assert_eq!(both.run(1), tone.run(1).min(noise.run(1)));
        }
    }

    /// The envelope's level at each of its first `steps` steps.
    fn envelope(shape: u8, fine: bool, steps: usize) -> Vec<u8> {
        let mut chip = Chip::new(fine);
        chip.write(7, 0b111_111);
        chip.write(8, 0x10);
        chip.write(11, 3);
        chip.write(13, shape);
        (0..steps)
            .map(|_| {
                let sum = chip.run(3);
                assert_eq!(sum % 3, 0, "one level a period");
                if fine { LEVELS_YM } else { LEVELS_AY }.iter().rposition(|&l| l as u32 == sum / 3).unwrap() as u8
            })
            .collect()
    }

    #[test]
    fn the_envelope_has_its_sixteen_shapes() {
        let down: Vec<u8> = (0..32).rev().collect();
        let up: Vec<u8> = (0..32).collect();
        let (low, high) = (vec![0u8; 32], vec![31u8; 32]);
        for shape in 0..16u8 {
            // The first ramp, and the two after it.
            let expect = match shape {
                0..4 | 9 => [&down, &low, &low],
                4..8 | 15 => [&up, &low, &low],
                8 => [&down, &down, &down],
                10 => [&down, &up, &down],
                11 => [&down, &high, &high],
                12 => [&up, &up, &up],
                13 => [&up, &high, &high],
                _ => [&up, &down, &up],
            };
            // (Level 1 is as silent as 0 and reads back as 1.)
            let expect: Vec<u8> = expect.iter().flat_map(|r| r.iter().map(|&l| l.max(1))).collect();
            assert_eq!(envelope(shape, true, 96), expect, "shape {shape}");
            // An AY has sixteen levels, each for two steps.
            let coarse: Vec<u8> = expect.iter().map(|l| l | 1).collect();
            assert_eq!(envelope(shape, false, 96), coarse, "shape {shape} on an AY");
        }
        // A write of the shape starts it over, the same shape too; one
        // of the period does not.
        let mut chip = Chip::new(true);
        chip.write(7, 0b111_111);
        chip.write(8, 0x10);
        chip.write(11, 10);
        chip.write(13, 12);
        chip.run(10 * 20 + 5);
        assert_eq!(chip.run(1), LEVELS_YM[20] as u32);
        chip.write(11, 10);
        assert_eq!(chip.run(1), LEVELS_YM[20] as u32);
        chip.write(13, 12);
        assert_eq!(chip.run(10), 0);
        assert_eq!(chip.run(10 * 2), 10 * LEVELS_YM[2] as u32);
        // An envelope nobody listens to goes on all the same.
        chip.write(13, 12);
        chip.write(8, 0);
        chip.run(10 * 20);
        chip.write(8, 0x10);
        assert_eq!(chip.run(1), LEVELS_YM[20] as u32);
        // The registers read back.
        assert_eq!((chip.register(11), chip.register(13), chip.register(40)), (10, 12, 0));
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A ZX Spectrum tune as a clip: a Pro Tracker 3 module, played on the
//! chip through an AY-3-8910 of the clock's own (`ay::Chip`).
//!
//! A `.pt3` file is notes, three channels of them, with the sounds they
//! are played in: up to 32 samples, each a list of lines that say for
//! one frame the level, an offset to the tone, one to the noise or the
//! envelope's period, and what the mixer lets through; and 16
//! ornaments, each a list of notes to add, a frame apiece. The
//! Spectrum's player read it once a frame and wrote the chip's
//! registers; `Tune` is that player, written for the clock from the
//! format's behaviour, reading the module in place. The clip
//! (`MAGIC`, `scripts/pt32clip.py`) is the module as it is behind a
//! header that says what the module cannot: the machine's clock and
//! frame, the length, and how the clock is to shape it.
//!
//! Played: Pro Tracker 3.x and Vortex Tracker II modules for one chip;
//! every pattern code and the effects (tone slide, portamento, sample
//! and ornament offsets, the on/off vibrato, envelope slide, tempo),
//! with the tone and volume tables of the module's version. Not
//! played: TurboSound, a module for two chips.
//!
//! A read past the module's end is nought and a row ends where the
//! module does, so a broken module is wrong notes and never a hang.

use crate::ay::{Chip, Parts, TICK};
use crate::clip::PCM_HZ;
use crate::shape::Shaper;

/// The first bytes of a converted tune.
pub const MAGIC: &[u8; 4] = b"PT31";
/// The header: magic; flags (`FINE`, `AFRESH`); the limiter's drive
/// and release (0 for the shaper's own) and how many times the
/// high-pass is applied; the chip's clock in Hz as `u32`; the length in
/// ms, played once; the clock cycles a frame; the frames; the frame
/// the tune goes round to; the high-pass's section as five `i16` in
/// Q14; the sixteenths of its level a channel with noise in it is
/// mixed at (0 for all sixteen; `ay::Chip::set_noise_level`); what a
/// low-pass over the noise moves by a sample, in 256ths of the way (0
/// for no low-pass). The module follows.
const HEADER: usize = 40;
/// The chip is a YM2149: 32 levels in its envelope, not 16.
const FINE: u8 = 1;
/// Played round and round, the tune starts over from its top with the
/// player and the chip as new, and not from the module's loop position.
const AFRESH: u8 = 2;

/// Samples a second a tune gives: twice the PCM clips' 8 kHz.
pub const RATE: u32 = 2 * PCM_HZ;

/// Where things are in a module: the version's digit in its first
/// line, the tone table's number, the frames a row, the positions and
/// the one to go round to, the address of the patterns' table, the
/// tables of the samples' and the ornaments' addresses, and the
/// position list, each entry a pattern's number times three.
const VERSION: usize = 13;
const TABLE: usize = 99;
const DELAY: usize = 100;
const POSITIONS: usize = 101;
const LOOP: usize = 102;
const PATTERNS: usize = 103;
const SAMPLES: usize = 105;
const ORNAMENTS: usize = 169;
const ORDER: usize = 201;

/// The highest note: eight octaves of twelve.
const TOP_NOTE: u8 = 95;

// The tone periods of the 96 notes: the format's four tables, each as
// modules of version 3.3 and before have it (`_OLD`) and as later ones
// do. A module names its table; they differ in tuning and in the
// clock they were worked out for.
const PT_OLD: [u16; 96] = [
    0xC21, 0xB73, 0xACE, 0xA33, 0x9A0, 0x916, 0x893, 0x818, 0x7A4, 0x736, 0x6CE, 0x66D,
    0x610, 0x5B9, 0x567, 0x519, 0x4D0, 0x48B, 0x449, 0x40C, 0x3D2, 0x39B, 0x367, 0x336,
    0x308, 0x2DC, 0x2B3, 0x28C, 0x268, 0x245, 0x224, 0x206, 0x1E9, 0x1CD, 0x1B3, 0x19B,
    0x184, 0x16E, 0x159, 0x146, 0x134, 0x122, 0x112, 0x103, 0x0F4, 0x0E6, 0x0D9, 0x0CD,
    0x0C2, 0x0B7, 0x0AC, 0x0A3, 0x09A, 0x091, 0x089, 0x081, 0x07A, 0x073, 0x06C, 0x066,
    0x061, 0x05B, 0x056, 0x051, 0x04D, 0x048, 0x044, 0x040, 0x03D, 0x039, 0x036, 0x033,
    0x030, 0x02D, 0x02B, 0x028, 0x026, 0x024, 0x022, 0x020, 0x01E, 0x01C, 0x01B, 0x019,
    0x018, 0x016, 0x015, 0x014, 0x013, 0x012, 0x011, 0x010, 0x00F, 0x00E, 0x00D, 0x00C,
];
const PT: [u16; 96] = [
    0xC22, 0xB73, 0xACF, 0xA33, 0x9A1, 0x917, 0x894, 0x819, 0x7A4, 0x737, 0x6CF, 0x66D,
    0x611, 0x5BA, 0x567, 0x51A, 0x4D0, 0x48B, 0x44A, 0x40C, 0x3D2, 0x39B, 0x367, 0x337,
    0x308, 0x2DD, 0x2B4, 0x28D, 0x268, 0x246, 0x225, 0x206, 0x1E9, 0x1CE, 0x1B4, 0x19B,
    0x184, 0x16E, 0x15A, 0x146, 0x134, 0x123, 0x112, 0x103, 0x0F5, 0x0E7, 0x0DA, 0x0CE,
    0x0C2, 0x0B7, 0x0AD, 0x0A3, 0x09A, 0x091, 0x089, 0x082, 0x07A, 0x073, 0x06D, 0x067,
    0x061, 0x05C, 0x056, 0x052, 0x04D, 0x049, 0x045, 0x041, 0x03D, 0x03A, 0x036, 0x033,
    0x031, 0x02E, 0x02B, 0x029, 0x027, 0x024, 0x022, 0x020, 0x01F, 0x01D, 0x01B, 0x01A,
    0x018, 0x017, 0x016, 0x014, 0x013, 0x012, 0x011, 0x010, 0x00F, 0x00E, 0x00D, 0x00C,
];
const ST: [u16; 96] = [
    0xEF8, 0xE10, 0xD60, 0xC80, 0xBD8, 0xB28, 0xA88, 0x9F0, 0x960, 0x8E0, 0x858, 0x7E0,
    0x77C, 0x708, 0x6B0, 0x640, 0x5EC, 0x594, 0x544, 0x4F8, 0x4B0, 0x470, 0x42C, 0x3FD,
    0x3BE, 0x384, 0x358, 0x320, 0x2F6, 0x2CA, 0x2A2, 0x27C, 0x258, 0x238, 0x216, 0x1F8,
    0x1DF, 0x1C2, 0x1AC, 0x190, 0x17B, 0x165, 0x151, 0x13E, 0x12C, 0x11C, 0x10A, 0x0FC,
    0x0EF, 0x0E1, 0x0D6, 0x0C8, 0x0BD, 0x0B2, 0x0A8, 0x09F, 0x096, 0x08E, 0x085, 0x07E,
    0x077, 0x070, 0x06B, 0x064, 0x05E, 0x059, 0x054, 0x04F, 0x04B, 0x047, 0x042, 0x03F,
    0x03B, 0x038, 0x035, 0x032, 0x02F, 0x02C, 0x02A, 0x027, 0x025, 0x023, 0x021, 0x01F,
    0x01D, 0x01C, 0x01A, 0x019, 0x017, 0x016, 0x015, 0x013, 0x012, 0x011, 0x010, 0x00F,
];
const ASM_OLD: [u16; 96] = [
    0xD3E, 0xC80, 0xBCC, 0xB22, 0xA82, 0x9EC, 0x95C, 0x8D6, 0x858, 0x7E0, 0x76E, 0x704,
    0x69F, 0x640, 0x5E6, 0x591, 0x541, 0x4F6, 0x4AE, 0x46B, 0x42C, 0x3F0, 0x3B7, 0x382,
    0x34F, 0x320, 0x2F3, 0x2C8, 0x2A1, 0x27B, 0x257, 0x236, 0x216, 0x1F8, 0x1DC, 0x1C1,
    0x1A8, 0x190, 0x179, 0x164, 0x150, 0x13D, 0x12C, 0x11B, 0x10B, 0x0FC, 0x0EE, 0x0E0,
    0x0D4, 0x0C8, 0x0BD, 0x0B2, 0x0A8, 0x09F, 0x096, 0x08D, 0x085, 0x07E, 0x077, 0x070,
    0x06A, 0x064, 0x05E, 0x059, 0x054, 0x050, 0x04B, 0x047, 0x043, 0x03F, 0x03C, 0x038,
    0x035, 0x032, 0x02F, 0x02D, 0x02A, 0x028, 0x026, 0x024, 0x022, 0x020, 0x01E, 0x01D,
    0x01B, 0x01A, 0x019, 0x018, 0x015, 0x014, 0x013, 0x012, 0x011, 0x010, 0x00F, 0x00E,
];
const ASM: [u16; 96] = [
    0xD10, 0xC55, 0xBA4, 0xAFC, 0xA5F, 0x9CA, 0x93D, 0x8B8, 0x83B, 0x7C5, 0x755, 0x6EC,
    0x688, 0x62A, 0x5D2, 0x57E, 0x52F, 0x4E5, 0x49E, 0x45C, 0x41D, 0x3E2, 0x3AB, 0x376,
    0x344, 0x315, 0x2E9, 0x2BF, 0x298, 0x272, 0x24F, 0x22E, 0x20F, 0x1F1, 0x1D5, 0x1BB,
    0x1A2, 0x18B, 0x174, 0x160, 0x14C, 0x139, 0x128, 0x117, 0x107, 0x0F9, 0x0EB, 0x0DD,
    0x0D1, 0x0C5, 0x0BA, 0x0B0, 0x0A6, 0x09D, 0x094, 0x08C, 0x084, 0x07C, 0x075, 0x06F,
    0x069, 0x063, 0x05D, 0x058, 0x053, 0x04E, 0x04A, 0x046, 0x042, 0x03E, 0x03B, 0x037,
    0x034, 0x031, 0x02F, 0x02C, 0x029, 0x027, 0x025, 0x023, 0x021, 0x01F, 0x01D, 0x01C,
    0x01A, 0x019, 0x017, 0x016, 0x015, 0x014, 0x012, 0x011, 0x010, 0x00F, 0x00E, 0x00D,
];
const REAL: [u16; 96] = [
    0xCDA, 0xC22, 0xB73, 0xACF, 0xA33, 0x9A1, 0x917, 0x894, 0x819, 0x7A4, 0x737, 0x6CF,
    0x66D, 0x611, 0x5BA, 0x567, 0x51A, 0x4D0, 0x48B, 0x44A, 0x40C, 0x3D2, 0x39B, 0x367,
    0x337, 0x308, 0x2DD, 0x2B4, 0x28D, 0x268, 0x246, 0x225, 0x206, 0x1E9, 0x1CE, 0x1B4,
    0x19B, 0x184, 0x16E, 0x15A, 0x146, 0x134, 0x123, 0x112, 0x103, 0x0F5, 0x0E7, 0x0DA,
    0x0CE, 0x0C2, 0x0B7, 0x0AD, 0x0A3, 0x09A, 0x091, 0x089, 0x082, 0x07A, 0x073, 0x06D,
    0x067, 0x061, 0x05C, 0x056, 0x052, 0x04D, 0x049, 0x045, 0x041, 0x03D, 0x03A, 0x036,
    0x033, 0x031, 0x02E, 0x02B, 0x029, 0x027, 0x024, 0x022, 0x020, 0x01F, 0x01D, 0x01B,
    0x01A, 0x018, 0x017, 0x016, 0x014, 0x013, 0x012, 0x011, 0x010, 0x00F, 0x00E, 0x00D,
];
const REAL_OLD: [u16; 96] = {
    let mut table = REAL;
    table[43] = 0x113;
    table
};

/// The level a channel is given for a sample's level (the column) at a
/// channel's volume (the row), in modules of version 3.4 and before.
const VOLUME_OLD: [[u8; 16]; 16] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2],
    [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3],
    [0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4],
    [0, 0, 0, 1, 1, 1, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5],
    [0, 0, 0, 1, 1, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, 6],
    [0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7],
    [0, 0, 1, 1, 2, 2, 3, 3, 4, 5, 5, 6, 6, 7, 7, 8],
    [0, 0, 1, 1, 2, 3, 3, 4, 5, 5, 6, 6, 7, 8, 8, 9],
    [0, 0, 1, 2, 2, 3, 4, 4, 5, 6, 6, 7, 8, 8, 9, 10],
    [0, 0, 1, 2, 3, 3, 4, 5, 6, 6, 7, 8, 9, 9, 10, 11],
    [0, 0, 1, 2, 3, 4, 4, 5, 6, 7, 8, 8, 9, 10, 11, 12],
    [0, 0, 1, 2, 3, 4, 5, 6, 7, 7, 8, 9, 10, 11, 12, 13],
    [0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];

/// The same for version 3.5 and later: the product, rounded.
const VOLUME: [[u8; 16]; 16] = {
    let mut table = [[0; 16]; 16];
    let mut v = 0;
    while v < 16 {
        let mut l = 0;
        while l < 16 {
            table[v][l] = ((2 * v * l + 15) / 30) as u8;
            l += 1;
        }
        v += 1;
    }
    table
};

/// Whether `data` is a converted PT3 module.
pub const fn is_tune(data: &[u8]) -> bool {
    data.len() >= HEADER + ORDER && data[0] == MAGIC[0] && data[1] == MAGIC[1] && data[2] == MAGIC[2] && data[3] == MAGIC[3]
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

/// A sample or an ornament: where its lines are, how many, and the
/// one it goes round to.
#[derive(Clone, Copy, Default)]
struct Lines {
    at: usize,
    len: u8,
    back: u8,
}

impl Lines {
    /// The one whose address is the word at `pointer`: its loop
    /// position, its length, then the lines.
    fn at(m: &[u8], pointer: usize) -> Self {
        let at = word(m, pointer) as usize;
        Self { at: at + 2, len: byte(m, at + 1), back: byte(m, at) }
    }

    /// The position after `pos`.
    fn after(&self, pos: u8) -> u8 {
        let next = pos.wrapping_add(1);
        if next >= self.len { self.back } else { next }
    }
}

/// One of the three channels.
#[derive(Clone, Copy, Default)]
struct Channel {
    /// The next byte of its pattern.
    at: usize,
    sample: Lines,
    sample_pos: u8,
    ornament: Lines,
    ornament_pos: u8,
    /// The channel's volume, 1 to 15.
    volume: u8,
    /// Rows a note lasts, and the rows until the pattern is read again.
    rows: u8,
    rows_left: u8,
    note: u8,
    /// A note is sounding, and with the chip's envelope.
    sounding: bool,
    envelope: bool,
    /// What the frame gave the chip: the tone period and the level.
    tone: u16,
    level: u8,
    /// What a sample's lines have added up to so far: to the tone, the
    /// level, the noise and the envelope's period.
    tone_sum: i32,
    level_slide: i8,
    noise_slide: u8,
    env_slide: u8,
    /// The tone slide: what it has added to the period, its step, the
    /// frames between steps and to the next. A portamento also has the
    /// note it ends on and how far that is.
    slide: i32,
    slide_step: i32,
    slide_every: u8,
    slide_in: u8,
    portamento: bool,
    target: u8,
    distance: i32,
    /// The on/off vibrato: frames sounding, frames silent, and frames
    /// until the next change.
    on_frames: u8,
    off_frames: u8,
    flip_in: u8,
}

impl Channel {
    /// A note or a rest starts everything a sample runs over.
    fn restart(&mut self, sounding: bool) {
        self.sample_pos = 0;
        self.ornament_pos = 0;
        self.level_slide = 0;
        self.noise_slide = 0;
        self.env_slide = 0;
        self.tone_sum = 0;
        self.slide = 0;
        self.slide_in = 0;
        self.flip_in = 0;
        self.sounding = sounding;
    }
}

/// A tune being played.
pub struct Tune<'a> {
    /// The clip, and the module in it.
    d: &'a [u8],
    m: &'a [u8],
    /// The module's version digit, 6 where it has none (Vortex Tracker).
    version: u8,
    tones: &'static [u16; 96],
    channel: [Channel; 3],
    position: u8,
    /// Frames a row, and until the next.
    delay: u8,
    delay_left: u8,
    noise_base: u8,
    /// The noise offset of the last sample line that had one.
    noise_add: u8,
    /// The envelope's period as the pattern set it, what a slide has
    /// added, the slide's step, the frames between steps and to the next.
    env_base: u16,
    env_slide: u16,
    env_step: u16,
    env_every: i8,
    env_in: i8,
    /// The envelope's shape, in a frame where the pattern set it.
    shape: Option<u8>,
    /// Ticks to the next frame and what is left of a tick, in clock
    /// cycles; the cycles a frame.
    frame_left: u32,
    frame_part: u32,
    frame: u32,
    /// Clock cycles a second, and what a sample's ticks left over, in
    /// cycles times `RATE`.
    clock: u32,
    sample_part: u32,
    /// The low-pass over the noise: its step in 256ths, and where it
    /// is, 65536 for the noise's bit set all the time.
    noise_step: i32,
    noise_mean: i32,
    looping: bool,
    ended: bool,
    chip: Chip,
    shaper: Shaper,
}

impl<'a> Tune<'a> {
    /// `data` must be a converted tune (`is_tune`).
    pub fn new(data: &'a [u8]) -> Self {
        let m = data.get(HEADER..).unwrap_or(&[]);
        let version = match byte(m, VERSION) {
            digit @ b'0'..=b'9' => digit - b'0',
            _ => 6,
        };
        let old = version <= 3;
        let mut tune = Self {
            d: data,
            m,
            version,
            tones: match (byte(m, TABLE), old) {
                (0, true) => &PT_OLD,
                (0, false) => &PT,
                (1, _) => &ST,
                (2, true) => &ASM_OLD,
                (2, false) => &ASM,
                (_, true) => &REAL_OLD,
                (_, false) => &REAL,
            },
            channel: [Channel::default(); 3],
            position: 0,
            delay: 0,
            delay_left: 0,
            noise_base: 0,
            noise_add: 0,
            env_base: 0,
            env_slide: 0,
            env_step: 0,
            env_every: 0,
            env_in: 0,
            shape: None,
            frame_left: 0,
            frame_part: 0,
            // Held to what a machine could have, so that a broken
            // header is a wrong tempo or pitch and not an endless loop.
            frame: long(data, 16).clamp(1024, 1 << 24),
            clock: long(data, 8).clamp(500_000, 4_000_000),
            sample_part: 0,
            noise_step: byte(data, 39) as i32,
            noise_mean: 0,
            looping: false,
            ended: byte(m, POSITIONS) == 0,
            chip: Chip::new(byte(data, 4) & FINE != 0),
            shaper: Shaper::new(byte(data, 5), byte(data, 6), true, core::array::from_fn(|i| word(data, 28 + 2 * i) as i16), byte(data, 7), true),
        };
        tune.start();
        tune
    }

    /// The player and the chip as a tune finds them.
    fn start(&mut self) {
        let m = self.m;
        self.chip = Chip::new(byte(self.d, 4) & FINE != 0);
        self.chip.set_noise_level(match byte(self.d, 38) {
            0 => 16,
            level => level,
        });
        self.noise_mean = 1 << 15;
        self.position = 0;
        self.delay = byte(m, DELAY);
        self.delay_left = 1;
        self.noise_base = 0;
        self.noise_add = 0;
        self.env_base = 0;
        self.env_slide = 0;
        self.env_step = 0;
        self.env_every = 0;
        self.env_in = 0;
        self.channel = [Channel { sample: Lines::at(m, SAMPLES + 2), ornament: Lines::at(m, ORNAMENTS), volume: 15, rows_left: 1, ..Channel::default() }; 3];
        self.seek();
    }

    /// The channels to the start of the pattern at `position`.
    fn seek(&mut self) {
        let pattern = word(self.m, PATTERNS) as usize + 2 * byte(self.m, ORDER + self.position as usize) as usize;
        for (c, channel) in self.channel.iter_mut().enumerate() {
            channel.at = word(self.m, pattern + 2 * c) as usize;
        }
    }

    /// Play the tune round and round, as its header says to, or once.
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

    /// The chip's output for one sample, before the shaping: its mean
    /// over the sample's ticks.
    fn next_mix(&mut self) -> Option<i32> {
        self.sample_part += self.clock;
        let ticks = self.sample_part / (TICK * RATE);
        self.sample_part %= TICK * RATE;
        let (mut sum, mut left) = (Parts::default(), ticks);
        while left > 0 {
            if self.frame_left == 0 {
                self.next_frame(true);
                if self.ended {
                    return None;
                }
                self.frame_part += self.frame;
                self.frame_left = self.frame_part / TICK;
                self.frame_part %= TICK;
            }
            let n = left.min(self.frame_left);
            let parts = self.chip.run_parts(n);
            sum.plain += parts.plain;
            sum.noisy += parts.noisy;
            sum.open += parts.open;
            sum.noise += parts.noise;
            left -= n;
            self.frame_left -= n;
        }
        let mut noisy = (sum.noisy / ticks) as i32;
        if self.noise_step != 0 {
            // The noise alone is low-passed, as the part of the sample
            // its bit was set for, and the channels that listen to it
            // are their tones and levels times that: a tone under a
            // drum's noise keeps its edges.
            let set = (sum.noise * 65_536 / ticks) as i32;
            self.noise_mean += ((set - self.noise_mean) * self.noise_step) >> 8;
            noisy = ((sum.open / ticks) as i64 * self.noise_mean as i64 >> 16) as i32;
        }
        Some((sum.plain / ticks) as i32 + noisy)
    }

    /// A frame begins: the patterns' next row if it is time for one,
    /// then every channel's sample and ornament a line on, and the
    /// registers. `first` unless this is the tune starting afresh
    /// within the frame that found its end.
    fn next_frame(&mut self, first: bool) {
        self.shape = None;
        self.delay_left = self.delay_left.wrapping_sub(1);
        if self.delay_left == 0 {
            for c in 0..3 {
                self.channel[c].rows_left = self.channel[c].rows_left.wrapping_sub(1);
                if self.channel[c].rows_left != 0 {
                    continue;
                }
                // The first channel's pattern ends in a nought, and
                // with it the position.
                if c == 0 && byte(self.m, self.channel[0].at) == 0 {
                    self.position = self.position.wrapping_add(1);
                    if self.position >= byte(self.m, POSITIONS) {
                        let afresh = byte(self.d, 4) & AFRESH != 0;
                        if !self.looping || (afresh && !first) {
                            self.ended = true;
                            return;
                        }
                        if afresh {
                            self.start();
                            return self.next_frame(false);
                        }
                        self.position = byte(self.m, LOOP);
                    }
                    self.seek();
                    self.noise_base = 0;
                }
                self.row(c);
            }
            self.delay_left = self.delay;
        }
        let (mut mixer, mut env_add) = (0u8, 0i8);
        for c in 0..3 {
            self.line(c, &mut mixer, &mut env_add);
            let channel = &self.channel[c];
            self.chip.write(2 * c as u8, channel.tone as u8);
            self.chip.write(2 * c as u8 + 1, (channel.tone >> 8) as u8);
            self.chip.write(8 + c as u8, channel.level);
        }
        self.chip.write(6, self.noise_base.wrapping_add(self.noise_add) & 0x1F);
        self.chip.write(7, mixer);
        let period = self.env_base.wrapping_add(env_add as u16).wrapping_add(self.env_slide);
        self.chip.write(11, period as u8);
        self.chip.write(12, (period >> 8) as u8);
        if let Some(shape) = self.shape {
            self.chip.write(13, shape);
        }
        if self.env_in > 0 {
            self.env_in -= 1;
            if self.env_in == 0 {
                self.env_in = self.env_every;
                self.env_slide = self.env_slide.wrapping_add(self.env_step);
            }
        }
    }

    /// The pattern sets the envelope: its shape, and its period from
    /// the two bytes that follow, high byte first.
    fn set_envelope(&mut self, c: usize, shape: u8) {
        let at = self.channel[c].at;
        self.shape = Some(shape);
        self.env_base = (byte(self.m, at + 1) as u16) << 8 | byte(self.m, at + 2) as u16;
        self.env_slide = 0;
        self.env_in = 0;
        self.channel[c].at = at + 2;
        self.channel[c].envelope = true;
    }

    /// A row of channel `c`'s pattern: codes to the one that ends the
    /// row (a note, a rest or a bare end), then what the row's effects
    /// take, the last named first.
    fn row(&mut self, c: usize) {
        let m = self.m;
        let (was_note, was_slide) = (self.channel[c].note, self.channel[c].slide);
        // The effects named, in order; one named twice counts where it
        // was named last.
        let mut effects = [0u8; 7];
        let mut named = 0;
        while self.channel[c].at < m.len() {
            let code = m[self.channel[c].at];
            let ch = &mut self.channel[c];
            let mut ends = false;
            match code {
                0xF0..=0xFF => {
                    ch.ornament = Lines::at(m, ORNAMENTS + 2 * (code as usize - 0xF0));
                    ch.at += 1;
                    ch.sample = Lines::at(m, SAMPLES + 2 * (byte(m, ch.at) as usize / 2));
                    ch.envelope = false;
                    ch.ornament_pos = 0;
                }
                0xD1..=0xEF => ch.sample = Lines::at(m, SAMPLES + 2 * (code as usize - 0xD0)),
                0xD0 => ends = true,
                0xC1..=0xCF => ch.volume = code - 0xC0,
                0xC0 => {
                    ch.restart(false);
                    ends = true;
                }
                0xB2..=0xBF => {
                    self.set_envelope(c, code - 0xB1);
                    self.channel[c].ornament_pos = 0;
                }
                0xB1 => {
                    ch.at += 1;
                    ch.rows = byte(m, ch.at);
                }
                0xB0 => {
                    ch.envelope = false;
                    ch.ornament_pos = 0;
                }
                0x50..=0xAF => {
                    ch.note = code - 0x50;
                    ch.restart(true);
                    ends = true;
                }
                0x40..=0x4F => {
                    ch.ornament = Lines::at(m, ORNAMENTS + 2 * (code as usize - 0x40));
                    ch.ornament_pos = 0;
                }
                0x20..=0x3F => self.noise_base = code - 0x20,
                0x10..=0x1F => {
                    if code == 0x10 {
                        ch.envelope = false;
                    } else {
                        self.set_envelope(c, code - 0x10);
                    }
                    let ch = &mut self.channel[c];
                    ch.at += 1;
                    ch.sample = Lines::at(m, SAMPLES + 2 * (byte(m, ch.at) as usize / 2));
                    ch.ornament_pos = 0;
                }
                1..=5 | 8 | 9 => {
                    let mut kept = 0;
                    for i in 0..named {
                        if effects[i] != code {
                            effects[kept] = effects[i];
                            kept += 1;
                        }
                    }
                    effects[kept] = code;
                    named = kept + 1;
                }
                _ => {}
            }
            self.channel[c].at += 1;
            if ends {
                break;
            }
        }
        for &effect in effects[..named].iter().rev() {
            let ch = &mut self.channel[c];
            let at = ch.at;
            match effect {
                // A tone slide: frames between steps, and the step.
                1 => {
                    ch.slide_every = byte(m, at);
                    ch.slide_in = ch.slide_every;
                    ch.slide_step = word(m, at + 1) as i16 as i32;
                    ch.at += 3;
                    ch.portamento = false;
                    ch.flip_in = 0;
                    if ch.slide_in == 0 && self.version >= 7 {
                        ch.slide_in = 1;
                    }
                }
                // A portamento: the note this row named is where the
                // one before it slides to.
                2 => {
                    ch.slide_every = byte(m, at);
                    ch.slide_in = ch.slide_every;
                    ch.slide_step = (word(m, at + 3) as i16 as i32).abs();
                    ch.at += 5;
                    ch.portamento = true;
                    ch.flip_in = 0;
                    ch.distance = self.tones[ch.note.min(TOP_NOTE) as usize] as i32 - self.tones[was_note.min(TOP_NOTE) as usize] as i32;
                    ch.target = ch.note;
                    ch.note = was_note;
                    if self.version >= 6 {
                        ch.slide = was_slide;
                    }
                    if ch.distance - ch.slide < 0 {
                        ch.slide_step = -ch.slide_step;
                    }
                }
                3 => {
                    ch.sample_pos = byte(m, at);
                    ch.at += 1;
                }
                4 => {
                    ch.ornament_pos = byte(m, at);
                    ch.at += 1;
                }
                5 => {
                    ch.on_frames = byte(m, at);
                    ch.off_frames = byte(m, at + 1);
                    ch.at += 2;
                    ch.flip_in = ch.on_frames;
                    ch.slide_in = 0;
                    ch.slide = 0;
                }
                8 => {
                    self.env_every = byte(m, at) as i8;
                    self.env_in = self.env_every;
                    self.env_step = word(m, at + 1);
                    ch.at += 3;
                }
                _ => {
                    self.delay = byte(m, at);
                    ch.at += 1;
                }
            }
        }
        self.channel[c].rows_left = self.channel[c].rows;
    }

    /// Channel `c` a frame on: the line of its sample it is at makes
    /// its tone and level, and its share of the mixer, the noise and
    /// the envelope's period.
    fn line(&mut self, c: usize, mixer: &mut u8, env_add: &mut i8) {
        let m = self.m;
        let volumes = if self.version <= 4 { &VOLUME_OLD } else { &VOLUME };
        let ch = &mut self.channel[c];
        if ch.sounding {
            // A line: flags and the noise or envelope offset, flags and
            // the level, the tone offset as a word.
            let at = ch.sample.at + 4 * ch.sample_pos as usize;
            let (b0, b1) = (byte(m, at), byte(m, at + 1));
            let offset = (word(m, at + 2) as i32).wrapping_add(ch.tone_sum);
            if b1 & 0x40 != 0 {
                ch.tone_sum = offset;
            }
            let note = ch.note.wrapping_add(byte(m, ch.ornament.at + ch.ornament_pos as usize));
            let note = if note >= 0x80 { 0 } else { note.min(TOP_NOTE) };
            ch.tone = (offset.wrapping_add(ch.slide).wrapping_add(self.tones[note as usize] as i32) & 0xFFF) as u16;
            if ch.slide_in > 0 {
                ch.slide_in -= 1;
                if ch.slide_in == 0 {
                    ch.slide += ch.slide_step;
                    ch.slide_in = ch.slide_every;
                    let there = if ch.slide_step < 0 { ch.slide <= ch.distance } else { ch.slide >= ch.distance };
                    if ch.portamento && there {
                        ch.note = ch.target;
                        ch.slide_in = 0;
                        ch.slide = 0;
                    }
                }
            }
            if b0 & 0x80 != 0 {
                if b0 & 0x40 != 0 {
                    ch.level_slide = (ch.level_slide + 1).min(15);
                } else {
                    ch.level_slide = (ch.level_slide - 1).max(-15);
                }
            }
            let level = ((b1 & 0xF) as i8 + ch.level_slide).clamp(0, 15);
            ch.level = volumes[(ch.volume & 0xF) as usize][level as usize];
            if b0 & 1 == 0 && ch.envelope {
                ch.level |= 0x10;
            }
            // Bits 1 to 5 of the first byte are an offset: to the
            // envelope's period, signed, if the line masks the noise,
            // and to the noise's if it does not.
            if b1 & 0x80 != 0 {
                let add = if b0 & 0x20 != 0 { b0 >> 1 | 0xF0 } else { b0 >> 1 & 0x0F }.wrapping_add(ch.env_slide);
                if b1 & 0x20 != 0 {
                    ch.env_slide = add;
                }
                *env_add = env_add.wrapping_add(add as i8);
            } else {
                self.noise_add = (b0 >> 1).wrapping_add(ch.noise_slide);
                if b1 & 0x20 != 0 {
                    ch.noise_slide = self.noise_add;
                }
            }
            // The mixer's bits for this channel: tone off, noise off.
            *mixer |= (b1 >> 4 & 1) << c | (b1 >> 7) << (3 + c);
            ch.sample_pos = ch.sample.after(ch.sample_pos);
            ch.ornament_pos = ch.ornament.after(ch.ornament_pos);
        } else {
            ch.level = 0;
        }
        if ch.flip_in > 0 {
            ch.flip_in -= 1;
            if ch.flip_in == 0 {
                ch.sounding = !ch.sounding;
                ch.flip_in = if ch.sounding { ch.on_frames } else { ch.off_frames };
            }
        }
    }
}

/// The whole tune's mix before the shaping, scaled to fit `i16` with
/// the three channels at full level: for comparing the player with
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

/// The registers the player gives the chip, fourteen a frame with 255
/// for a shape not written: for comparing the player with another.
#[cfg(test)]
pub fn registers(data: &[u8]) -> Vec<[u8; 14]> {
    let mut t = Tune::new(data);
    let mut out = Vec::new();
    loop {
        t.next_frame(true);
        if t.ended {
            return out;
        }
        let mut frame: [u8; 14] = core::array::from_fn(|r| t.chip.register(r as u8));
        frame[13] = t.shape.unwrap_or(0xFF);
        out.push(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Pentagon: the clock, and the cycles a frame.
    const CLOCK: u32 = 1_750_000;
    const FRAME: u32 = 35_840;

    /// A module under construction: samples and ornaments by number,
    /// patterns as three channels' bytes, the position list.
    struct Module {
        version: &'static [u8],
        table: u8,
        delay: u8,
        samples: Vec<(u8, Vec<u8>, u8)>,
        ornaments: Vec<(u8, Vec<u8>, u8)>,
        patterns: Vec<[Vec<u8>; 3]>,
        order: Vec<u8>,
        back: u8,
        flags: u8,
    }

    /// A sample line: the level, the tone on and the noise masked.
    fn plain(level: u8) -> [u8; 4] {
        [0, 0x80 | level, 0, 0]
    }

    impl Module {
        /// Sample 1 a steady full level, ornament 0 nothing, as every
        /// module has them.
        fn new() -> Self {
            Self {
                version: b"ProTracker 3.6 compilation of ",
                table: 2,
                delay: 3,
                samples: vec![(1, plain(15).to_vec(), 0)],
                ornaments: vec![(0, vec![0], 0)],
                patterns: Vec::new(),
                order: Vec::new(),
                back: 0,
                flags: 0,
            }
        }

        /// A pattern played at the next position: channel A's rows,
        /// with B and C silent beside it unless given.
        fn pattern(mut self, a: &[u8], b: &[u8], c: &[u8]) -> Self {
            let quiet = |rows: &[u8]| if rows.is_empty() { vec![0xB1, 0xFF, 0xD0] } else { rows.to_vec() };
            self.order.push(3 * self.patterns.len() as u8);
            self.patterns.push([[a, &[0]].concat(), quiet(b), quiet(c)]);
            self
        }

        fn sample(mut self, number: u8, lines: &[[u8; 4]], back: u8) -> Self {
            self.samples.push((number, lines.concat(), back));
            self
        }

        fn ornament(mut self, number: u8, notes: &[i8], back: u8) -> Self {
            self.ornaments.push((number, notes.iter().map(|&n| n as u8).collect(), back));
            self
        }

        /// The clip: no high-pass.
        fn clip(&self) -> Vec<u8> {
            let mut m = vec![b' '; ORDER];
            m[..self.version.len()].copy_from_slice(self.version);
            m[SAMPLES..ORDER].fill(0);
            m.extend(&self.order);
            m.push(0xFF);
            (m[TABLE], m[DELAY], m[POSITIONS], m[LOOP]) = (self.table, self.delay, self.order.len() as u8, self.back);
            let put = |m: &mut Vec<u8>, at: usize, value: usize| m[at..at + 2].copy_from_slice(&(value as u16).to_le_bytes());
            let table = m.len();
            put(&mut m, PATTERNS, table);
            m.resize(table + 6 * self.patterns.len(), 0);
            for (p, channels) in self.patterns.iter().enumerate() {
                for (c, bytes) in channels.iter().enumerate() {
                    let at = m.len();
                    put(&mut m, table + 6 * p + 2 * c, at);
                    m.extend(bytes);
                }
            }
            for (pointers, kind, line) in [(SAMPLES, &self.samples, 4), (ORNAMENTS, &self.ornaments, 1)] {
                for (number, lines, back) in kind {
                    let at = m.len();
                    put(&mut m, pointers + 2 * *number as usize, at);
                    m.extend([*back, (lines.len() / line) as u8]);
                    m.extend(lines);
                }
            }
            let mut out = MAGIC.to_vec();
            out.extend([self.flags, 0, 0, 0]);
            out.extend(CLOCK.to_le_bytes());
            out.extend([0; 4]);
            out.extend(FRAME.to_le_bytes());
            out.extend([0; 20]);
            assert_eq!(out.len(), HEADER);
            out.extend(m);
            out
        }

        /// What the chip is given, frame by frame.
        fn registers(&self) -> Vec<[u8; 14]> {
            registers(&self.clip())
        }
    }

    fn tone(frame: &[u8; 14], c: usize) -> u16 {
        frame[2 * c] as u16 | (frame[2 * c + 1] as u16) << 8
    }

    /// Note `n` as a pattern's byte.
    const fn note(n: u8) -> u8 {
        0x50 + n
    }

    #[test]
    fn a_note_is_its_tables_period_for_its_rows() {
        // A-4 on the ASM table, two rows of three frames; then a rest
        // for one; then C-1 with the channel's volume at 8.
        let module = Module::new().pattern(&[0xB1, 2, note(45), 0xB1, 1, 0xC0, 0xC8, note(0)], &[], &[]);
        let frames = module.registers();
        assert_eq!(frames.len(), 12);
        for (f, frame) in frames.iter().enumerate() {
            // (The mixer: the sample masks the noise while it sounds.)
            let (period, level, mixer) = match f {
                0..6 => (0x0F9, 15, 0b001_000),
                6..9 => (0x0F9, 0, 0),
                _ => (0xD10, 8, 0b001_000),
            };
            assert_eq!((tone(frame, 0), frame[8], frame[7]), (period, level, mixer), "frame {f}");
            // Nothing from the two channels that rest, and no shape written.
            assert_eq!((frame[9], frame[10], frame[13]), (0, 0, 0xFF));
        }
        // 0xF9 at a Pentagon's clock is 439.3 Hz.
        assert!((CLOCK as f64 / 16.0 / 0xF9 as f64 - 440.0).abs() < 1.0);
        // The module names its table, and its version the table's cut.
        for (table, version, a4, top) in [
            (0u8, &b"ProTracker 3.3 compilation of "[..], 0x0E6, 0x00C),
            (0, b"ProTracker 3.5 compilation of ", 0x0E7, 0x00C),
            (1, b"ProTracker 3.5 compilation of ", 0x11C, 0x00F),
            (2, b"ProTracker 3.3 compilation of ", 0x0FC, 0x00E),
            (2, b"Vortex Tracker II 1.0 module: ", 0x0F9, 0x00D),
            (3, b"ProTracker 3.3 compilation of ", 0x0F5, 0x00D),
            (3, b"ProTracker 3.6 compilation of ", 0x0F5, 0x00D),
        ] {
            let mut module = Module::new().pattern(&[0xB1, 1, note(45), note(95)], &[], &[]);
            (module.table, module.version) = (table, version);
            let frames = module.registers();
            assert_eq!((tone(&frames[0], 0), tone(&frames[3], 0)), (a4, top), "table {table} {:?}", core::str::from_utf8(version));
        }
        assert_eq!((REAL_OLD[43], REAL[43]), (0x113, 0x112));
        // The three channels are the chip's three.
        let frames = Module::new().pattern(&[note(45)], &[note(33)], &[0xC4, note(57)]).registers();
        assert_eq!((tone(&frames[0], 0), tone(&frames[0], 1), tone(&frames[0], 2)), (0x0F9, 0x1F1, 0x07C));
        assert_eq!(frames[0][8..11], [15, 15, 4]);
    }

    #[test]
    fn the_volume_tables_are_the_formats() {
        // 3.5's is the product rounded; the older one is never more
        // than a step from it.
        for (v, row) in VOLUME.iter().enumerate() {
            for (l, &level) in row.iter().enumerate() {
                assert_eq!(level as f64, (v as f64 * l as f64 / 15.0).round());
                assert!(VOLUME_OLD[v][l].abs_diff(level) <= 1);
            }
        }
        assert_eq!((VOLUME_OLD[8][7], VOLUME[8][7]), (3, 4));
        let rows = [0xC8, note(45)];
        let mut module = Module::new().sample(2, &[plain(7)], 0).pattern(&[0xD2, rows[0], rows[1]], &[], &[]);
        assert_eq!(module.registers()[0][8], 4);
        module.version = b"ProTracker 3.4 compilation of ";
        assert_eq!(module.registers()[0][8], 3);
    }

    #[test]
    fn a_sample_is_played_a_line_a_frame() {
        // Levels 15, 10, 5 and round the last two; the second line a
        // tone offset of +3, the third with the tone off and the noise
        // offset 9; six rows of one frame.
        let lines = [plain(15), [0, 10, 3, 0], [9 << 1, 0x10 | 5, 0, 0]];
        let mut module = Module::new().sample(2, &lines, 1).pattern(&[0xB1, 6, 0xD2, 0x25, note(45)], &[], &[]);
        module.delay = 1;
        let frames = module.registers();
        let got: Vec<_> = frames.iter().map(|f| (tone(f, 0), f[8], f[7] & 0b001_001, f[6])).collect();
        // (The noise offset stays what the last line with one left.)
        assert_eq!(got, [(0xF9, 15, 8, 5), (0xFC, 10, 0, 5), (0xF9, 5, 1, 14), (0xFC, 10, 0, 5), (0xF9, 5, 1, 14), (0xFC, 10, 0, 5)]);
        // A line may keep what it adds: the tone offset (bit 6 of the
        // second byte) and the level's slide (bit 7 of the first, with
        // bit 6 for up), each frame one more.
        let module = Module::new().sample(2, &[[0x80, 0x40 | 15, 0xFE, 0xFF]], 0).pattern(&[0xB1, 5, 0xD2, note(45)], &[], &[]);
        let frames = module.registers();
        let got: Vec<_> = frames.iter().take(5).map(|f| (tone(f, 0), f[8])).collect();
        assert_eq!(got, [(0xF7, 14), (0xF5, 13), (0xF3, 12), (0xF1, 11), (0xEF, 10)]);
        // A new note starts the sample and its sums over.
        let module = Module::new().sample(2, &[[0xC0, 0x40 | 3, 1, 0]], 0).pattern(&[0xB1, 1, 0xD2, note(45), note(45)], &[], &[]);
        let got: Vec<_> = module.registers().iter().map(|f| (tone(f, 0), f[8])).collect();
        assert_eq!(got, [(0xFA, 4), (0xFB, 5), (0xFC, 6), (0xFA, 4), (0xFB, 5), (0xFC, 6)]);
        // The noise: a line that masks it says so in the mixer, and the
        // pattern's base is added to a line's offset.
        let module = Module::new().sample(2, &[[3 << 1, 0x80 | 15, 0, 0], [3 << 1, 15, 0, 0]], 0).pattern(&[0xD2, 0x30, note(45)], &[], &[]);
        let frames = module.registers();
        assert_eq!((frames[0][7], frames[0][6]), (0b001_000, 0x10));
        assert_eq!((frames[1][7], frames[1][6]), (0, 0x13));
    }

    #[test]
    fn an_ornament_is_notes_added_a_frame_apiece() {
        // A major chord from A-4, round and round; then the same with
        // the ornament begun at its third note; held to the table's ends.
        let chord = [0x0F9, 0x0C5, 0x0A6];
        let module = Module::new().ornament(1, &[0, 4, 7], 0).ornament(2, &[-60, 60], 0).pattern(&[0xB1, 2, 0x41, note(45), 0x04, note(45), 2, 0x42, note(45)], &[], &[]);
        let got: Vec<u16> = module.registers().iter().map(|f| tone(f, 0)).collect();
        assert_eq!(got[..6], [chord[0], chord[1], chord[2], chord[0], chord[1], chord[2]]);
        assert_eq!(got[6..12], [chord[2], chord[0], chord[1], chord[2], chord[0], chord[1]]);
        assert_eq!(got[12..14], [0xD10, 0x00D]);
        // Ornament and sample in one code, the sample's number doubled.
        let module = Module::new().ornament(3, &[12], 0).sample(4, &[plain(9)], 0).pattern(&[0xF3, 8, note(45)], &[], &[]);
        let frames = module.registers();
        assert_eq!((tone(&frames[0], 0), frames[0][8]), (0x7C, 9));
    }

    #[test]
    fn the_tone_slides_and_a_portamento_arrives() {
        // A slide of -2 every frame, then of +5 every second.
        let module = Module::new().pattern(&[0xB1, 2, 0x01, note(45), 1, 0xFE, 0xFF, 0x01, note(45), 2, 5, 0], &[], &[]);
        let got: Vec<u16> = module.registers().iter().map(|f| tone(f, 0)).collect();
        assert_eq!(got[..6], [0xF9, 0xF7, 0xF5, 0xF3, 0xF1, 0xEF]);
        assert_eq!(got[6..12], [0xF9, 0xF9, 0xFE, 0xFE, 0x103, 0x103]);
        // A portamento from A-4 up to B-4 (0xDD, 28 away) by 8 a frame:
        // it stops on the note, and stays.
        let module = Module::new().pattern(&[0xB1, 2, note(45), 0x02, note(47), 1, 0, 0, 8, 0, 0xD0], &[], &[]);
        let got: Vec<u16> = module.registers().iter().map(|f| tone(f, 0)).collect();
        assert_eq!(got[6..], [0xF9, 0xF1, 0xE9, 0xE1, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD, 0xDD]);
        // And down, to A-3, from where a slide had got to.
        let module = Module::new().pattern(&[0xB1, 1, 0x01, note(45), 1, 0x10, 0, 0x02, note(33), 1, 0, 0, 0x60, 0, 0xB1, 2, 0xD0], &[], &[]);
        let got: Vec<u16> = module.registers().iter().map(|f| tone(f, 0)).collect();
        assert_eq!(got, [0xF9, 0x109, 0x119, 0x129, 0x189, 0x1E9, 0x1F1, 0x1F1, 0x1F1, 0x1F1, 0x1F1, 0x1F1]);
    }

    #[test]
    fn the_envelope_and_the_vibrato() {
        // Shape 14 at period 0x0123 with sample 1: the level register
        // says "the envelope's", and the shape is written that frame only.
        let module = Module::new().pattern(&[0xB1, 2, 0x1E, 0x01, 0x23, 2, note(45), 0xB0, note(45), 0xBD, 0x00, 0x10, note(45)], &[], &[]);
        let frames = module.registers();
        let got: Vec<_> = frames.iter().map(|f| (f[8], f[11], f[12], f[13])).collect();
        assert_eq!(got[..3], [(0x1F, 0x23, 0x01, 14), (0x1F, 0x23, 0x01, 0xFF), (0x1F, 0x23, 0x01, 0xFF)]);
        assert_eq!(got[6], (0x0F, 0x23, 0x01, 0xFF));
        assert_eq!(got[12], (0x1F, 0x10, 0x00, 12));
        // A sample's line may keep the envelope out, or move its
        // period, signed, and keep what it adds.
        let lines = [[1, 15, 0, 0], [0x3E, 0x80 | 15, 0, 0], [2 << 1, 0xA0 | 15, 0, 0]];
        let module = Module::new().sample(2, &lines, 2).pattern(&[0xB1, 5, 0x18, 0x00, 0x40, 4, note(45)], &[], &[]);
        let got: Vec<_> = module.registers().iter().take(5).map(|f| (f[8], f[11])).collect();
        assert_eq!(got, [(0x0F, 0x40), (0x1F, 0x3F), (0x1F, 0x42), (0x1F, 0x44), (0x1F, 0x46)]);
        // The envelope's own slide: +3 every second frame.
        let module = Module::new().pattern(&[0xB1, 2, 0x08, 0x1C, 0x00, 0x40, 2, note(45), 2, 3, 0], &[], &[]);
        let got: Vec<u8> = module.registers().iter().map(|f| f[11]).collect();
        assert_eq!(got, [0x40, 0x40, 0x43, 0x43, 0x46, 0x46]);
        // The on/off vibrato: two frames on, one off.
        let module = Module::new().pattern(&[0xB1, 3, 0x05, note(45), 2, 1], &[], &[]);
        let got: Vec<u8> = module.registers().iter().map(|f| f[8]).collect();
        assert_eq!(got, [15, 15, 0, 15, 15, 0, 15, 15, 0]);
    }

    #[test]
    fn the_positions_the_tempo_and_the_way_round() {
        // Three patterns of one row, the second at four frames a row
        // from then on; round from the second.
        let build = || {
            let mut module = Module::new().pattern(&[0xB1, 1, note(45)], &[], &[]).pattern(&[0x09, note(47), 4], &[], &[]).pattern(&[note(49)], &[], &[]);
            module.back = 1;
            module
        };
        let notes = |data: &[u8], frames: usize| -> Vec<u16> {
            let mut t = Tune::new(data);
            t.set_looping(true);
            (0..frames)
                .map(|_| {
                    t.next_frame(true);
                    assert!(!t.ended);
                    tone(&core::array::from_fn(|r| t.chip.register(r as u8)), 0)
                })
                .collect()
        };
        let (a, b, c) = (0xF9, 0xDD, 0xC5);
        let once: Vec<u16> = build().registers().iter().map(|f| tone(f, 0)).collect();
        assert_eq!(once, [a, a, a, b, b, b, b, c, c, c, c]);
        assert_eq!(notes(&build().clip(), 20), [a, a, a, b, b, b, b, c, c, c, c, b, b, b, b, c, c, c, c, b]);
        // Afresh: from the top, at the tempo the module began with.
        let mut module = build();
        module.flags = AFRESH;
        assert_eq!(notes(&module.clip(), 20), [a, a, a, b, b, b, b, c, c, c, c, a, a, a, b, b, b, b, c, c]);
        // A position list may name a pattern twice.
        let mut module = build();
        module.order = vec![0, 6, 0];
        assert_eq!(module.registers().iter().map(|f| tone(f, 0)).collect::<Vec<_>>(), [a, a, a, c, c, c, a, a, a]);
    }

    #[test]
    fn a_broken_module_ends_and_does_not_hang() {
        let module = Module::new().pattern(&[0xB1, 2, note(45), note(47)], &[note(33)], &[]).clip();
        assert!(is_tune(&module) && !is_tune(&module[..HEADER + 100]) && !crate::sid::is_tune(&module) && !crate::xm::is_tune(&module));
        // Whole, it ends after its four rows of three frames.
        let mut t = Tune::new(&module);
        assert_eq!((0..100_000).take_while(|_| t.next_sample().is_some()).count().abs_diff(12 * 16_000 * FRAME as usize / CLOCK as usize), 0);
        // Cut anywhere, played once or round and round, afresh or not:
        // what it plays is anyone's guess, but every sample comes.
        for cut in HEADER + ORDER..module.len() {
            for (flags, looping) in [(0, false), (0, true), (AFRESH, true)] {
                let mut data = module[..cut].to_vec();
                data[4] = flags;
                let mut t = Tune::new(&data);
                t.set_looping(looping);
                for _ in 0..20_000 {
                    t.next_sample();
                }
            }
        }
        // Bytes of no meaning for patterns.
        let mut random = 12345u32;
        for _ in 0..50 {
            let mut data = module.clone();
            for b in &mut data[HEADER + TABLE..] {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (random >> 24) as u8;
            }
            let mut t = Tune::new(&data);
            t.set_looping(true);
            for _ in 0..20_000 {
                t.next_sample();
            }
        }
        // No positions: nothing to play.
        let mut data = module.clone();
        data[HEADER + POSITIONS] = 0;
        assert_eq!(Tune::new(&data).next_sample(), None);
    }

    #[test]
    fn the_samples_are_the_notes_on_their_frames() {
        // A-4 for 30 frames, A-5 for 30: each at its pitch for its
        // time, at full scale, in steps finer than the 8-bit one.
        let mut module = Module::new().pattern(&[0xB1, 10, note(45), note(57)], &[], &[]);
        let data = module.clip();
        let mut t = Tune::new(&data);
        assert_eq!(t.rate(), 16_000);
        let mut out = Vec::new();
        while let Some(s) = t.next_sample() {
            out.push(s as i32);
        }
        let frame = FRAME as f64 * RATE as f64 / CLOCK as f64;
        assert!(out.len().abs_diff((60.0 * frame) as usize) <= 1, "{}", out.len());
        assert_eq!(raw_mix(&data).len(), out.len());
        let pitch = |part: &[i32]| part.windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count() as f64 * RATE as f64 / part.len() as f64;
        let half = (30.0 * frame) as usize;
        let (first, second) = (pitch(&out[2000..half - 10]), pitch(&out[half + 10..]));
        assert!((first - CLOCK as f64 / 16.0 / 0xF9 as f64).abs() < 3.0, "{first}");
        assert!((second - CLOCK as f64 / 16.0 / 0x7C as f64).abs() < 3.0, "{second}");
        // With no high-pass the square wave is nought and full scale,
        // and between the 8-bit steps only where an edge falls in a sample.
        let peak = out[2000..].iter().map(|s| s.abs()).max().unwrap();
        assert_eq!(peak, 127 * crate::shape::STEP);
        assert!(out.iter().filter(|&&s| s % crate::shape::STEP != 0).count() > out.len() / 50);
        // The mix is the chip's mean over each sample: a square wave of
        // 31 samples and a bit, at the top level or at nought but for
        // the samples an edge falls in.
        let mix = raw_mix(&data);
        let flat = mix[100..5000].iter().filter(|&&s| s == 0 || s == 32767 / 4).count();
        assert!(flat > 4900 * 28 / 31 && flat < 4900, "{flat}");
        // The header may ask for the noise lower: a drum, the noise
        // alone at full level, is that much quieter in the mix.
        let drum = Module::new().sample(2, &[[0, 0x10 | 15, 0, 0]], 0).pattern(&[0xB1, 10, 0xD2, note(45)], &[], &[]);
        let mut data = drum.clip();
        let loud = raw_mix(&data).into_iter().max().unwrap();
        data[38] = 4;
        assert_eq!((loud, raw_mix(&data).into_iter().max().unwrap()), (32767 / 4, 32767 / 16));
        // Or the noise through a low-pass: the drum's level is kept and
        // its sample-to-sample jumps are not, while a note is left as it was.
        let rough = |mix: &[i16]| mix[2000..6000].windows(2).map(|w| (w[1] as i64 - w[0] as i64).pow(2)).sum::<i64>();
        let mean = |mix: &[i16]| mix[2000..6000].iter().map(|&s| s as i64).sum::<i64>() / 4000;
        data[38] = 0;
        let (open, tone) = (raw_mix(&data), raw_mix(&module.clip()));
        data[39] = 64;
        let dull = raw_mix(&data);
        assert!(mean(&dull).abs_diff(mean(&open)) < 100 && rough(&dull) * 8 < rough(&open), "{} {}", rough(&dull), rough(&open));
        let mut data = module.clip();
        data[39] = 64;
        assert_eq!(raw_mix(&data), tone);
        // A tone under the noise is not filtered: its edges are all
        // there, at the half the noise comes to.
        let both = Module::new().sample(2, &[[0, 15, 0, 0]], 0).pattern(&[0xB1, 20, 0xD2, note(45)], &[], &[]);
        let mut data = both.clip();
        data[39] = 16;
        let under = raw_mix(&data);
        let edges = rough(&under) as f64 / rough(&tone) as f64;
        assert!((0.15..0.4).contains(&edges), "{edges}");
        // A YM2149 by the header's flag; the same notes.
        module.flags = FINE;
        assert_eq!(raw_mix(&module.clip()).len(), mix.len());
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
            assert_eq!(registers(data).len(), long(data, 20) as usize);
            // Music, not silence: a tenth of full scale at least.
            assert!((energy / n).isqrt() > 12 * crate::shape::STEP as u64);
            // And it goes round: twice the length and still playing.
            let mut t = Tune::new(data);
            t.set_looping(true);
            assert!((0..2 * n + 1000).all(|_| t.next_sample().is_some()));
        }
    }

    /// Not a test: with `CLIP_REG_DUMP=file`, writes the registers a
    /// built-in module gives the chip, fourteen bytes a frame, for
    /// comparing with another player's. `CLIP_INDEX` says which clip.
    #[test]
    #[ignore]
    fn dump_the_registers() {
        let Ok(path) = std::env::var("CLIP_REG_DUMP") else { return };
        let index = std::env::var("CLIP_INDEX").map_or(0, |i| i.parse().unwrap());
        std::fs::write(path, registers(crate::melody::CLIP_DATA[index]).concat()).unwrap();
    }
}

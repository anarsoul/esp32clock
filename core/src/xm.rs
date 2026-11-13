// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A tracker module as a clip: a player for the part of FastTracker 2's
//! XM format that chiptunes use, giving the samples the clip's
//! modulator takes, `clip::PCM_HZ` a second.
//!
//! A module is a few short recorded waveforms and patterns of notes
//! that say when to play which at what pitch and volume, so a tune of
//! a minute is ten or twenty kilobytes where its PCM is half a
//! megabyte. The price is this player. It is written for the clock
//! and not after any other: no heap, the module read in place from
//! flash, a fixed number of channels.
//!
//! The file is not the XM itself but what `scripts/xm2clip.py` makes
//! of one (`MAGIC`): the same patterns, still packed, behind tables of
//! where each pattern and each instrument's sample is, with the samples
//! as plain signed bytes (an XM stores them as differences, which
//! cannot be read from the middle) and ping-pong loops unrolled. One
//! sample an instrument.
//!
//! What plays: notes with the instrument's relative note and finetune
//! on the linear frequency table, forward loops, the volume envelope
//! with its sustain and loop points and the fade after a key-off, the
//! volume column's set, slides, vibrato and tone portamento, and the
//! effects 0 arpeggio (in FastTracker's tick order), 1 2 3 portamento,
//! 4 vibrato, 5 6 with volume slide, 9 offset, A volume slide, B jump,
//! C volume, D break, E1 E2 fine portamento, E9 retrigger, EA EB fine
//! volume, EC cut, ED delay, F speed and tempo, G global volume, K key
//! off; and an instrument's auto-vibrato, as a sine. Panning is nothing on one buzzer. The converter says what a
//! module uses beyond that.
//!
//! The channels are mixed at four times the output rate with linear
//! interpolation and averaged down. Then the tune gets what the PCM
//! clips get on the host in `scripts/wav2pcm.sh`, since the buzzer is
//! the same: the bass cut by a high-pass and the level pushed hard into
//! a limiter. How much bass and how hard are the clip's to say, in its
//! header; a tune whose melody sits under 900 Hz wants a lower cut than
//! the PCM clips' 900 Hz twice over.
//!
//! The tune is played through once: the order list to its end, or to
//! a jump back.

use crate::clip::PCM_HZ;

/// The first bytes of a converted module.
pub const MAGIC: &[u8; 4] = b"XMC2";
/// The header: magic, channels, speed, tempo, orders, patterns,
/// instruments, the limiter's drive and release (0 for `DRIVE` and
/// `RELEASE_SHIFT`), the length in ms as `u32`; then the high-pass:
/// how many times it is applied (0 to 2), the output rate (0 for
/// `PCM_HZ`, 1 for twice that), and its
/// second-order section as five `i16` in Q14, b0 b1 b2 a1 a2, which
/// the converter works out since the chip has no tangent; the order to
/// start over from when the tune is played round and round; three
/// spare.
const HEADER: usize = 32;

/// Whether `data` is a converted module of the first layout, which
/// this firmware would take for PCM: convert it again.
pub const fn is_old_tune(data: &[u8]) -> bool {
    data.len() >= 4 && data[0] == b'X' && data[1] == b'M' && data[2] == b'C' && data[3] == b'1'
}
/// A pattern's entry: rows as `u16`, its data's offset as `u32`.
const PATTERN_ENTRY: usize = 6;
/// An instrument's entry; see `Instrument`.
const INSTRUMENT_ENTRY: usize = 80;
/// Points of a volume envelope.
const ENVELOPE_POINTS: usize = 12;
/// Channels played; the converter refuses a module with more.
pub const MAX_CHANNELS: usize = 8;

/// The mixing rate, whatever the output's: four times the PCM clips'
/// 8 kHz, and twice the 16 kHz a clip may ask for instead.
const MIX_HZ: u32 = PCM_HZ * 4;
/// A tick is 2.5 / tempo seconds: each mixed sample takes `tempo` out
/// of a pool this large.
const TICK_POOL: i32 = (MIX_HZ * 5 / 2) as i32;

/// Period units a semitone, and the period of the lowest note.
const SEMITONE: i32 = 64;
const PERIOD_MAX: i32 = 10 * 12 * SEMITONE;
/// The period at which a sample plays at `C4_HZ`.
const PERIOD_C4: i32 = PERIOD_MAX - 4 * 12 * SEMITONE;
const C4_HZ: u32 = 8363;
/// Steps of the pitch table an octave.
const OCTAVE: i32 = 12 * SEMITONE;

/// 2^(i / 768) for one octave, as Q15 from 32768 up.
const POW: [u16; OCTAVE as usize] = {
    // 2^(1 / 768) in Q32.
    const STEP: u64 = 4_298_845_406;
    let mut table = [0u16; OCTAVE as usize];
    let mut x: u64 = 1 << 32;
    let mut i = 0;
    while i < table.len() {
        table[i] = ((x + (1 << 16)) >> 17) as u16;
        x = ((x as u128 * STEP as u128) >> 32) as u64;
        i += 1;
    }
    table
};

/// Half a sine wave, 32 steps, peak 255: the vibrato's.
const SINE: [u8; 32] =
    [0, 24, 49, 74, 97, 120, 141, 161, 180, 197, 212, 224, 235, 244, 250, 253, 255, 253, 250, 244, 235, 224, 212, 197, 180, 161, 141, 120, 97, 74, 49, 24];

/// The key-off note.
const KEY_OFF: u8 = 97;
/// The fade's full level.
const FADE_FULL: u32 = 32768;

/// The limiter: a level under this gets the full gain, one over it is
/// brought down to it. One channel at full volume peaks at 32768
/// before the filter, so this is 20 dB of drive, as the host's
/// conversion gives a PCM clip.
const LIMIT: i32 = 3000;
/// The limiter lets go by a 32nd a sample, 4 ms: fast enough that the
/// level is back at the top between a note's peaks, which is where the
/// loudness is.
const RELEASE_SHIFT: u32 = 5;
/// What the limiter aims a peak at, of a full scale of 127: a quarter
/// over it, the excess clipped. With a release of 16 ms and no excess
/// the test module came out at the RMS the host's conversion of it has,
/// 0.42 of full scale; the user asked for more (2026-11-12), and this
/// is 0.59, 3 dB, with a sixth of the samples at the rails.
const DRIVE: i32 = 160;

/// Whether `data` is a converted module.
pub const fn is_tune(data: &[u8]) -> bool {
    data.len() >= HEADER && data[0] == MAGIC[0] && data[1] == MAGIC[1] && data[2] == MAGIC[2] && data[3] == MAGIC[3]
}

/// How long the tune plays, as the converter worked it out.
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

/// An instrument's entry in the file: where its sample is (offset,
/// length, loop start and loop length as `u32`, a loop length of 0 for
/// none), its volume, finetune and relative note, the envelope's flags
/// (1 on, 2 sustain, 4 loop), number of points, sustain point and loop
/// points, the fade as `u16`, the auto-vibrato's sweep, depth and rate,
/// three spare bytes, then twelve points of frame and level as `u16`
/// each.
#[derive(Clone, Copy)]
struct Instrument<'a> {
    d: &'a [u8],
    at: usize,
}

impl Instrument<'_> {
    fn sample(&self) -> (u32, u32, u32, u32) {
        (long(self.d, self.at), long(self.d, self.at + 4), long(self.d, self.at + 8), long(self.d, self.at + 12))
    }

    fn volume(&self) -> u8 {
        byte(self.d, self.at + 16).min(64)
    }

    fn finetune(&self) -> i32 {
        byte(self.d, self.at + 17) as i8 as i32
    }

    fn relative(&self) -> i32 {
        byte(self.d, self.at + 18) as i8 as i32
    }

    fn envelope_flags(&self) -> u8 {
        byte(self.d, self.at + 19)
    }

    fn points(&self) -> usize {
        (byte(self.d, self.at + 20) as usize).min(ENVELOPE_POINTS)
    }

    fn sustain(&self) -> usize {
        byte(self.d, self.at + 21) as usize
    }

    fn envelope_loop(&self) -> (usize, usize) {
        (byte(self.d, self.at + 22) as usize, byte(self.d, self.at + 23) as usize)
    }

    fn fade(&self) -> u32 {
        word(self.d, self.at + 24) as u32
    }

    /// The auto-vibrato: ticks to full depth, the depth in period
    /// units, and the rate, 256 to a cycle.
    fn auto_vibrato(&self) -> (u32, u32, u8) {
        (byte(self.d, self.at + 26) as u32, byte(self.d, self.at + 27) as u32, byte(self.d, self.at + 28))
    }

    /// Point `i` of the envelope: its frame and its level, 0 to 64.
    fn point(&self, i: usize) -> (u32, u32) {
        let at = self.at + 32 + 4 * i.min(ENVELOPE_POINTS - 1);
        (word(self.d, at) as u32, (word(self.d, at + 2) as u32).min(64))
    }
}

/// One cell of a pattern.
#[derive(Clone, Copy, Default)]
struct Cell {
    note: u8,
    instrument: u8,
    volume: u8,
    effect: u8,
    param: u8,
}

impl Cell {
    /// Tone portamento, by effect or by the volume column: the note is
    /// where to slide to, not one to start.
    fn slides_to_note(&self) -> bool {
        self.effect == 3 || self.effect == 5 || self.volume >> 4 == 0xF
    }

    fn has_vibrato(&self) -> bool {
        self.effect == 4 || self.effect == 6 || self.volume >> 4 == 0xB
    }

    /// `EDx` with a tick to wait for.
    fn delay(&self) -> u8 {
        if self.effect == 0xE && self.param >> 4 == 0xD { self.param & 0xF } else { 0 }
    }
}

#[derive(Clone, Copy, Default)]
struct Channel {
    cell: Cell,
    /// The instrument last named, from 1; 0 for none yet.
    instrument: u8,
    /// The sample being played: where, how long, and its loop.
    data: u32,
    len: u32,
    loop_start: u32,
    loop_len: u32,
    playing: bool,
    /// Position in the sample, with 16 bits of fraction, and the step a
    /// mixed sample.
    pos: u32,
    frac: u32,
    step: u32,
    /// The note's period, where a tone portamento is going, and what
    /// vibrato and arpeggio add for now.
    period: i32,
    target: i32,
    vibrato: i32,
    arpeggio: i32,
    /// The period being sounded, all of those together.
    sounding: i32,
    /// 0 to 64.
    volume: u8,
    /// What the mixer multiplies by, 0 to 4096.
    amp: i32,
    key_on: bool,
    envelope_frame: u32,
    fade: u32,
    /// Effect memories.
    porta: u8,
    tone_speed: u8,
    slide: u8,
    vibrato_speed: u8,
    vibrato_depth: u8,
    vibrato_pos: u8,
    /// The instrument's own vibrato: where in its cycle, and its depth
    /// so far in 256ths of a period unit.
    auto_pos: u8,
    auto_depth: u32,
}

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

/// A module being played.
pub struct Tune<'a> {
    d: &'a [u8],
    channels: usize,
    order_len: usize,
    patterns: usize,
    instruments: usize,
    patterns_at: usize,
    instruments_at: usize,
    speed: u8,
    tempo: u8,
    global: u8,
    /// Where in the song: the order, the row about to be read, the rows
    /// of this pattern and the next cell's place in the file.
    order: usize,
    row: u16,
    rows: u16,
    cursor: usize,
    tick: u8,
    /// A jump or a break read on this row: the order and row to go to.
    go: Option<(usize, u16)>,
    pool: i32,
    /// Whether the tune goes round and round: it follows a jump back,
    /// and from the end of the order list it starts over.
    looping: bool,
    /// The tick being played is the tune's last.
    last: bool,
    ended: bool,
    channel: [Channel; MAX_CHANNELS],
    /// The high-pass: its section, how many of them, and their states.
    highpass: [i64; 5],
    passes: usize,
    filter: [Biquad; 2],
    envelope: i32,
    drive: i32,
    release: u32,
    /// Mixed samples an output sample: 4 at `PCM_HZ`, 2 at twice it.
    oversample: u32,
}

impl<'a> Tune<'a> {
    /// `data` must be a converted module (`is_tune`).
    pub fn new(data: &'a [u8]) -> Self {
        let order_len = byte(data, 7) as usize;
        let patterns = byte(data, 8) as usize;
        let patterns_at = HEADER + order_len;
        let mut tune = Self {
            d: data,
            channels: (byte(data, 4) as usize).min(MAX_CHANNELS),
            order_len,
            patterns,
            instruments: byte(data, 9) as usize,
            patterns_at,
            instruments_at: patterns_at + PATTERN_ENTRY * patterns,
            speed: byte(data, 5).max(1),
            tempo: byte(data, 6).max(32),
            global: 64,
            order: 0,
            row: 0,
            rows: 0,
            cursor: 0,
            tick: 0,
            go: None,
            pool: 0,
            looping: false,
            last: false,
            ended: false,
            channel: [Channel::default(); MAX_CHANNELS],
            highpass: core::array::from_fn(|i| word(data, 18 + 2 * i) as i16 as i64),
            passes: (byte(data, 16) as usize).min(2),
            filter: [Biquad::default(); 2],
            envelope: 0,
            drive: match byte(data, 10) {
                0 => DRIVE,
                d => d as i32,
            },
            // The release is given for `PCM_HZ`: a shift more at twice the
            // rate is the same time.
            release: match byte(data, 11) {
                0 => RELEASE_SHIFT,
                r => (r as u32).min(14),
            } + (byte(data, 17) & 1) as u32,
            oversample: if byte(data, 17) & 1 == 0 { 4 } else { 2 },
        };
        tune.enter(0, 0);
        tune
    }

    /// The next sample, unsigned with 128 for silence; `None` when the
    /// tune has ended.
    /// Play the tune round and round, as a tracker does, or once.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// Samples a second `next_sample` gives: `PCM_HZ`, or twice it for a
    /// clip that asks.
    pub fn rate(&self) -> u32 {
        MIX_HZ / self.oversample
    }

    pub fn next_sample(&mut self) -> Option<u8> {
        let mut sum = 0;
        for _ in 0..self.oversample {
            if self.pool <= 0 {
                self.step_tick();
                self.pool += TICK_POOL;
            }
            if self.ended {
                return None;
            }
            self.pool -= self.tempo as i32;
            sum += self.mix();
        }
        Some(self.shape(sum / self.oversample as i32))
    }

    fn instrument(&self, number: u8) -> Option<Instrument<'a>> {
        let index = (number as usize).checked_sub(1).filter(|&i| i < self.instruments)?;
        Some(Instrument { d: self.d, at: self.instruments_at + INSTRUMENT_ENTRY * index })
    }

    /// Go to `row` of the pattern at `order`; the end of the list is
    /// the end of the tune, once the tick being played is over.
    fn enter(&mut self, order: usize, row: u16) {
        let mut order = order;
        if order >= self.order_len && self.looping {
            // Round again, from where the module says, if that is anywhere.
            order = Some(byte(self.d, 28) as usize).filter(|&o| o < self.order_len).unwrap_or(0);
        }
        let pattern = byte(self.d, HEADER + order) as usize;
        if order >= self.order_len || pattern >= self.patterns {
            self.last = true;
            return;
        }
        let entry = self.patterns_at + PATTERN_ENTRY * pattern;
        self.order = order;
        self.rows = word(self.d, entry);
        self.cursor = long(self.d, entry + 2) as usize;
        self.row = row.min(self.rows.saturating_sub(1));
        for _ in 0..self.row as usize * self.channels {
            self.read_cell();
        }
    }

    /// Unpack the cell at the cursor: a byte with its top bit set says
    /// which of the five fields follow, any other byte is the note and
    /// all four follow.
    fn read_cell(&mut self) -> Cell {
        let mut next = || {
            let b = byte(self.d, self.cursor);
            self.cursor += 1;
            b
        };
        let first = next();
        let mut cell = Cell::default();
        if first & 0x80 == 0 {
            cell.note = first;
            cell.instrument = next();
            cell.volume = next();
            cell.effect = next();
            cell.param = next();
            return cell;
        }
        if first & 1 != 0 {
            cell.note = next();
        }
        if first & 2 != 0 {
            cell.instrument = next();
        }
        if first & 4 != 0 {
            cell.volume = next();
        }
        if first & 8 != 0 {
            cell.effect = next();
        }
        if first & 16 != 0 {
            cell.param = next();
        }
        cell
    }

    /// One tick: a new row on the first of a row's ticks, the running
    /// effects on the others, then every channel's pitch and level.
    fn step_tick(&mut self) {
        if self.last || self.ended {
            self.ended = true;
            return;
        }
        if self.tick == 0 {
            if self.row >= self.rows {
                self.ended = true;
                return;
            }
            for c in 0..self.channels {
                let cell = self.read_cell();
                self.channel[c].cell = cell;
                self.row_start(c);
            }
            self.row += 1;
        } else {
            for c in 0..self.channels {
                self.row_tick(c);
            }
        }
        for c in 0..self.channels {
            self.settle(c);
        }
        self.tick += 1;
        if self.tick >= self.speed {
            self.tick = 0;
            match self.go.take() {
                // A jump to where the tune has been is its loop: the end.
                Some((order, _)) if order <= self.order && !self.looping => self.last = true,
                Some((order, row)) => self.enter(order, row),
                None if self.row >= self.rows => self.enter(self.order + 1, 0),
                None => {}
            }
        }
    }

    /// The note, instrument and volume of the channel's cell.
    fn trigger(&mut self, c: usize) {
        let cell = self.channel[c].cell;
        if cell.instrument != 0 {
            self.channel[c].instrument = cell.instrument;
        }
        let instrument = self.instrument(self.channel[c].instrument);
        let ch = &mut self.channel[c];
        if cell.instrument != 0 {
            ch.volume = instrument.map_or(0, |i| i.volume());
            ch.key_on = true;
            ch.envelope_frame = 0;
            ch.fade = FADE_FULL;
        }
        if cell.note == KEY_OFF {
            Self::key_off(ch, instrument);
        } else if cell.note != 0 {
            let period = match instrument {
                Some(i) => PERIOD_MAX - (cell.note as i32 - 1 + i.relative()).clamp(0, 119) * SEMITONE - i.finetune() / 2,
                None => 0,
            };
            if cell.slides_to_note() && ch.playing {
                ch.target = period;
            } else if let Some(i) = instrument {
                (ch.data, ch.len, ch.loop_start, ch.loop_len) = i.sample();
                ch.period = period;
                ch.target = period;
                ch.frac = 0;
                // 9xx: start 256 bytes in for each of xx.
                ch.pos = if cell.effect == 9 { cell.param as u32 * 256 } else { 0 };
                ch.playing = ch.pos < ch.len;
                ch.vibrato_pos = 0;
                ch.auto_pos = 0;
                ch.auto_depth = if i.auto_vibrato().0 == 0 { i.auto_vibrato().1 << 8 } else { 0 };
            } else {
                ch.playing = false;
            }
        }
        match cell.volume >> 4 {
            1..=5 => ch.volume = (cell.volume - 0x10).min(64),
            8 => ch.volume = ch.volume.saturating_sub(cell.volume & 0xF),
            9 => ch.volume = (ch.volume + (cell.volume & 0xF)).min(64),
            _ => {}
        }
    }

    fn key_off(ch: &mut Channel, instrument: Option<Instrument>) {
        ch.key_on = false;
        // With no envelope there is nothing to fade along: silence.
        if instrument.is_none_or(|i| i.envelope_flags() & 1 == 0) {
            ch.volume = 0;
        }
    }

    /// The first tick of a row.
    fn row_start(&mut self, c: usize) {
        let cell = self.channel[c].cell;
        if cell.delay() == 0 {
            self.trigger(c);
        }
        let (hi, lo) = (cell.param >> 4, cell.param & 0xF);
        let order = self.order;
        let instrument = self.instrument(self.channel[c].instrument);
        let ch = &mut self.channel[c];
        ch.arpeggio = 0;
        if !cell.has_vibrato() {
            ch.vibrato = 0;
        }
        match cell.volume >> 4 {
            0xA => ch.vibrato_speed = cell.volume & 0xF,
            0xB if cell.volume & 0xF != 0 => ch.vibrato_depth = cell.volume & 0xF,
            0xF if cell.volume & 0xF != 0 => ch.tone_speed = (cell.volume & 0xF) << 4,
            _ => {}
        }
        match cell.effect {
            1 | 2 if cell.param != 0 => ch.porta = cell.param,
            3 if cell.param != 0 => ch.tone_speed = cell.param,
            4 => {
                if hi != 0 {
                    ch.vibrato_speed = hi;
                }
                if lo != 0 {
                    ch.vibrato_depth = lo;
                }
            }
            5 | 6 | 0xA if cell.param != 0 => ch.slide = cell.param,
            0xB => self.go = Some((cell.param as usize, 0)),
            0xC => ch.volume = cell.param.min(64),
            0xD => self.go = Some((self.go.map_or(order + 1, |g| g.0), (hi * 10 + lo) as u16)),
            0xE => match hi {
                1 => ch.period = (ch.period - lo as i32 * 4).max(1),
                2 => ch.period = (ch.period + lo as i32 * 4).min(PERIOD_MAX),
                0xA => ch.volume = (ch.volume + lo).min(64),
                0xB => ch.volume = ch.volume.saturating_sub(lo),
                _ => {}
            },
            0xF if cell.param >= 0x20 => self.tempo = cell.param,
            0xF if cell.param != 0 => self.speed = cell.param,
            // G.
            0x10 => self.global = cell.param.min(64),
            // K.
            0x14 if cell.param == 0 => Self::key_off(ch, instrument),
            _ => {}
        }
    }

    /// The other ticks of a row.
    fn row_tick(&mut self, c: usize) {
        let cell = self.channel[c].cell;
        let (tick, speed) = (self.tick, self.speed);
        if cell.delay() == tick {
            self.trigger(c);
        }
        let instrument = self.instrument(self.channel[c].instrument);
        let ch = &mut self.channel[c];
        let lo = cell.param & 0xF;
        match cell.volume >> 4 {
            6 => ch.volume = ch.volume.saturating_sub(cell.volume & 0xF),
            7 => ch.volume = (ch.volume + (cell.volume & 0xF)).min(64),
            0xB => Self::vibrato(ch),
            0xF => Self::tone_portamento(ch),
            _ => {}
        }
        match cell.effect {
            0 if cell.param != 0 => ch.arpeggio = Self::arpeggio(cell.param, speed, tick) * SEMITONE,
            1 => ch.period = (ch.period - ch.porta as i32 * 4).max(1),
            2 => ch.period = (ch.period + ch.porta as i32 * 4).min(PERIOD_MAX),
            3 => Self::tone_portamento(ch),
            4 => Self::vibrato(ch),
            5 => {
                Self::tone_portamento(ch);
                Self::volume_slide(ch);
            }
            6 => {
                Self::vibrato(ch);
                Self::volume_slide(ch);
            }
            0xA => Self::volume_slide(ch),
            0xE => match cell.param >> 4 {
                9 if lo != 0 && tick % lo == 0 => {
                    ch.pos = 0;
                    ch.frac = 0;
                    ch.playing = ch.len > 0;
                }
                0xC if tick == lo => ch.volume = 0,
                _ => {}
            },
            0x14 if tick == cell.param => Self::key_off(ch, instrument),
            _ => {}
        }
    }

    /// FastTracker's arpeggio: which of the three notes a tick plays
    /// depends on the speed, the row's last ticks being base, y, x.
    fn arpeggio(param: u8, speed: u8, tick: u8) -> i32 {
        let (x, y) = ((param >> 4) as i32, (param & 0xF) as i32);
        let offset = speed % 3;
        if offset == 2 && tick == 1 {
            return x;
        }
        if tick < offset {
            return 0;
        }
        match (tick - offset) % 3 {
            0 => 0,
            1 => y,
            _ => x,
        }
    }

    fn volume_slide(ch: &mut Channel) {
        let (up, down) = (ch.slide >> 4, ch.slide & 0xF);
        ch.volume = if up != 0 { (ch.volume + up).min(64) } else { ch.volume.saturating_sub(down) };
    }

    fn tone_portamento(ch: &mut Channel) {
        let speed = ch.tone_speed as i32 * 4;
        ch.period = if ch.period < ch.target { (ch.period + speed).min(ch.target) } else { (ch.period - speed).max(ch.target) };
    }

    /// A sine of at most `depth * 255 / 32` period units either way,
    /// up in pitch first.
    fn vibrato(ch: &mut Channel) {
        let wave = (SINE[(ch.vibrato_pos >> 2 & 31) as usize] as i32 * ch.vibrato_depth as i32) >> 5;
        ch.vibrato = if ch.vibrato_pos & 0x80 == 0 { -wave } else { wave };
        ch.vibrato_pos = ch.vibrato_pos.wrapping_add(ch.vibrato_speed << 2);
    }

    /// The level of the instrument's envelope at the channel's frame,
    /// 0 to 64, and the frame moved on.
    fn envelope(ch: &mut Channel, instrument: Instrument) -> u32 {
        let flags = instrument.envelope_flags();
        let points = instrument.points();
        if flags & 1 == 0 || points == 0 {
            return 64;
        }
        let frame = ch.envelope_frame;
        let mut level = instrument.point(points - 1).1;
        for i in 1..points {
            let (x1, y1) = instrument.point(i);
            if frame < x1 {
                let (x0, y0) = instrument.point(i - 1);
                level = if x1 > x0 { (y0 * (x1 - frame) + y1 * (frame.max(x0) - x0)) / (x1 - x0) } else { y1 };
                break;
            }
        }
        let held = flags & 2 != 0 && ch.key_on && frame == instrument.point(instrument.sustain()).0;
        if !held {
            ch.envelope_frame = frame.saturating_add(1);
            let (from, to) = instrument.envelope_loop();
            if flags & 4 != 0 && ch.envelope_frame == instrument.point(to).0 {
                ch.envelope_frame = instrument.point(from).0;
            }
        }
        if !ch.key_on {
            ch.fade = ch.fade.saturating_sub(instrument.fade());
        }
        level
    }

    /// The channel's step through its sample and its level, from what
    /// the tick left.
    fn settle(&mut self, c: usize) {
        let instrument = self.instrument(self.channel[c].instrument);
        let global = self.global as u32;
        let ch = &mut self.channel[c];
        let level = match instrument {
            Some(i) => Self::envelope(ch, i),
            None => 64,
        };
        ch.amp = ((ch.volume as u32 * level * global / 64) * ch.fade / FADE_FULL) as i32;
        // The instrument's vibrato: a sine that grows to its depth over
        // the sweep, whatever the pattern says.
        let mut auto = 0;
        if let Some((sweep, depth, rate)) = instrument.map(|i| i.auto_vibrato())
            && depth != 0
        {
            let wave = ((SINE[(ch.auto_pos >> 2 & 31) as usize] as u32 * ch.auto_depth) >> 16) as i32;
            auto = if ch.auto_pos & 0x80 == 0 { -wave } else { wave };
            ch.auto_pos = ch.auto_pos.wrapping_add(rate);
            ch.auto_depth = (ch.auto_depth + (depth << 8) / sweep.max(1)).min(depth << 8);
        }
        let period = (ch.period + ch.vibrato + auto - ch.arpeggio).clamp(1, PERIOD_MAX);
        ch.sounding = period;
        // The frequency is C4_HZ * 2^((PERIOD_C4 - period) / 768).
        let exponent = PERIOD_C4 - period;
        let (octave, rest) = (exponent.div_euclid(OCTAVE), exponent.rem_euclid(OCTAVE));
        // Hz in Q15, then a step in 16.16 at the mixing rate.
        let hz = C4_HZ as u64 * POW[rest as usize] as u64;
        let step = (hz << 1) / MIX_HZ as u64;
        ch.step = (if octave >= 0 { step << octave.min(16) } else { step >> (-octave).min(16) }).min(u32::MAX as u64) as u32;
    }

    /// One mixed sample: the channels' sum, 32768 for one channel at
    /// full level.
    fn mix(&mut self) -> i32 {
        let mut sum = 0;
        for ch in &mut self.channel[..self.channels] {
            if !ch.playing {
                continue;
            }
            let at = |pos: u32| byte(self.d, (ch.data + pos) as usize) as i8 as i32;
            let mut after = ch.pos + 1;
            if ch.loop_len != 0 && after >= ch.loop_start + ch.loop_len {
                after -= ch.loop_len;
            }
            let (a, b) = (at(ch.pos), if after < ch.len { at(after) } else { 0 });
            let sample = a * 256 + (b - a) * (ch.frac >> 8) as i32;
            sum += (sample * ch.amp) >> 12;
            let frac = ch.frac + (ch.step & 0xFFFF);
            ch.frac = frac & 0xFFFF;
            ch.pos = ch.pos.saturating_add((ch.step >> 16) + (frac >> 16));
            if ch.loop_len != 0 {
                let end = ch.loop_start + ch.loop_len;
                if ch.pos >= end {
                    ch.pos = ch.loop_start + (ch.pos - end) % ch.loop_len;
                }
            } else if ch.pos >= ch.len {
                ch.playing = false;
            }
        }
        sum
    }

    /// The high-pass, as often as the clip asks, and the limiter; out as
    /// an unsigned byte.
    fn shape(&mut self, x: i32) -> u8 {
        let mut y = x;
        for filter in &mut self.filter[..self.passes] {
            y = filter.run(&self.highpass, y);
        }
        self.envelope = (self.envelope - (self.envelope >> self.release)).max(y.abs());
        let out = y * self.drive / self.envelope.max(LIMIT);
        (128 + out.clamp(-127, 127)) as u8
    }
}

/// The whole tune's mix before the shaping, a sample an output sample,
/// scaled to fit `i16` with four channels at full level: for comparing
/// the player with another, from a test.
#[cfg(test)]
pub fn raw_mix(data: &[u8]) -> Vec<i16> {
    let mut t = Tune::new(data);
    let mut out = Vec::new();
    loop {
        let mut sum = 0;
        for _ in 0..t.oversample {
            if t.pool <= 0 {
                t.step_tick();
                t.pool += TICK_POOL;
            }
            if t.ended {
                return out;
            }
            t.pool -= t.tempo as i32;
            sum += t.mix();
        }
        out.push((sum / (t.oversample as i32 * 4)).clamp(-32768, 32767) as i16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One instrument of a test module.
    #[derive(Clone)]
    struct Ins {
        sample: Vec<i8>,
        loop_start: u32,
        loop_len: u32,
        volume: u8,
        finetune: i8,
        relative: i8,
        envelope: Vec<(u16, u16)>,
        flags: u8,
        sustain: u8,
        fade: u16,
        /// The auto-vibrato: sweep, depth, rate.
        auto: (u8, u8, u8),
    }

    /// A square wave of `period` samples, looped.
    fn square(period: usize) -> Ins {
        let sample = (0..period).map(|i| if i < period / 2 { 127 } else { -127 }).collect();
        Ins { sample, loop_start: 0, loop_len: period as u32, volume: 64, finetune: 0, relative: 0, envelope: vec![], flags: 0, sustain: 0, fade: 0, auto: (0, 0, 0) }
    }

    /// A cell: note, instrument, volume column, effect, parameter.
    type Cell5 = (u8, u8, u8, u8, u8);
    type Row = Vec<Cell5>;
    /// A cell and the row it goes on.
    type At = (usize, Cell5);

    /// A converted module, as `scripts/xm2clip.py` lays one out.
    fn module(channels: u8, speed: u8, tempo: u8, orders: &[u8], patterns: &[Vec<Row>], instruments: &[Ins]) -> Vec<u8> {
        let mut d = MAGIC.to_vec();
        d.extend([channels, speed, tempo, orders.len() as u8, patterns.len() as u8, instruments.len() as u8, 0, 0]);
        d.extend(0u32.to_le_bytes());
        // 900 Hz at 8 kHz, second order, twice: `sox highpass 900` twice.
        d.extend([2, 0]);
        for k in [9883i16, -19766, 9883, -17075, 6072] {
            d.extend(k.to_le_bytes());
        }
        d.extend([0; 4]);
        assert_eq!(d.len(), HEADER);
        d.extend(orders);
        let tables = d.len() + PATTERN_ENTRY * patterns.len() + INSTRUMENT_ENTRY * instruments.len();
        let mut body = Vec::new();
        for rows in patterns {
            d.extend((rows.len() as u16).to_le_bytes());
            d.extend(((tables + body.len()) as u32).to_le_bytes());
            for row in rows {
                assert_eq!(row.len(), channels as usize);
                for &(note, instrument, volume, effect, param) in row {
                    // Packed where it is all nothing, plain otherwise, to try both.
                    if (note, instrument, volume, effect, param) == (0, 0, 0, 0, 0) {
                        body.push(0x80);
                    } else if instrument == 0 && volume == 0 && param == 0 && effect == 0 {
                        body.extend([0x81, note]);
                    } else {
                        body.extend([note, instrument, volume, effect, param]);
                    }
                }
            }
        }
        for i in instruments {
            d.extend(((tables + body.len()) as u32).to_le_bytes());
            d.extend((i.sample.len() as u32).to_le_bytes());
            d.extend(i.loop_start.to_le_bytes());
            d.extend(i.loop_len.to_le_bytes());
            d.extend([i.volume, i.finetune as u8, i.relative as u8, i.flags, i.envelope.len() as u8, i.sustain, 0, 0]);
            d.extend(i.fade.to_le_bytes());
            d.extend([i.auto.0, i.auto.1, i.auto.2, 0, 0, 0]);
            for p in 0..ENVELOPE_POINTS {
                let (x, y) = i.envelope.get(p).copied().unwrap_or((0, 0));
                d.extend(x.to_le_bytes());
                d.extend(y.to_le_bytes());
            }
            body.extend(i.sample.iter().map(|&s| s as u8));
        }
        assert_eq!(d.len(), tables);
        d.extend(body);
        d
    }

    const EMPTY: Cell5 = (0, 0, 0, 0, 0);

    /// `rows` rows of one channel, the given cells at the given rows.
    fn pattern(rows: usize, cells: &[At]) -> Vec<Row> {
        let mut p = vec![vec![EMPTY]; rows];
        for &(row, cell) in cells {
            p[row] = vec![cell];
        }
        p
    }

    /// A tune stepped tick by tick, mixing nothing: the first
    /// channel's (period, volume, playing) after each tick.
    fn ticks(data: &[u8]) -> Vec<(i32, u8, bool)> {
        let mut t = Tune::new(data);
        let mut seen = Vec::new();
        loop {
            t.step_tick();
            if t.ended {
                return seen;
            }
            let ch = &t.channel[0];
            seen.push((ch.sounding, ch.volume, ch.playing));
            assert!(seen.len() < 100_000, "a tune that does not end");
        }
    }

    /// The mix before the shaping, a sample a mixed sample.
    fn mixed(data: &[u8]) -> Vec<i32> {
        let mut t = Tune::new(data);
        let mut out = Vec::new();
        loop {
            if t.pool <= 0 {
                t.step_tick();
                t.pool += TICK_POOL;
            }
            if t.ended {
                return out;
            }
            t.pool -= t.tempo as i32;
            out.push(t.mix());
        }
    }

    /// Upward crossings of zero a second.
    fn pitch(samples: &[i32]) -> f64 {
        let ups = samples.windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count();
        ups as f64 * MIX_HZ as f64 / samples.len() as f64
    }

    // Note numbers: C-4 is 49, and an instrument with no relative note
    // plays its sample at 8363 Hz there.
    const C4: u8 = 49;

    #[test]
    fn the_pitch_table_is_an_octave_of_semitones() {
        assert_eq!(POW[0], 32768);
        assert!((POW[384] as f64 - 32768.0 * 2f64.sqrt()).abs() < 1.5);
        assert!((POW[767] as f64 - 65536.0 / 2f64.powf(1.0 / 768.0)).abs() < 1.5);
        assert!(POW.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_note_plays_its_sample_at_its_pitch_for_its_rows() {
        // A square of 32 samples at C-4: 8363 / 32 = 261.3 Hz. Speed 6
        // at tempo 125 is 0.12 s a row.
        let rows = 16;
        let data = module(1, 6, 125, &[0], &[pattern(rows, &[(0, (C4, 1, 0, 0, 0))])], &[square(32)]);
        assert!(is_tune(&data) && !is_tune(b"XMC") && !is_tune(&[128; 64]));
        let out = mixed(&data);
        assert_eq!(out.len(), rows * 6 * (MIX_HZ as usize * 5 / 2 / 125));
        assert!((pitch(&out) - 8363.0 / 32.0).abs() < 1.0, "{}", pitch(&out));
        assert_eq!(out.iter().map(|s| s.abs()).max(), Some(127 * 256));
        // An octave up is twice that, a relative note of -12 undoes it,
        // and a finetune of 64 is half a semitone.
        let at = |note, relative, finetune| {
            let ins = Ins { relative, finetune, ..square(32) };
            pitch(&mixed(&module(1, 6, 125, &[0], &[pattern(rows, &[(0, (note, 1, 0, 0, 0))])], &[ins])))
        };
        assert!((at(C4 + 12, 0, 0) - 2.0 * 8363.0 / 32.0).abs() < 2.0);
        assert!((at(C4 + 12, -12, 0) - 8363.0 / 32.0).abs() < 1.0);
        assert!((at(C4, 0, 64) - 8363.0 / 32.0 * 2f64.powf(0.5 / 12.0)).abs() < 1.0);
        // A sample with no loop ends, and the channel with it.
        let once = Ins { loop_len: 0, ..square(320) };
        let out = mixed(&module(1, 6, 125, &[0], &[pattern(rows, &[(0, (C4, 1, 0, 0, 0))])], &[once]));
        let last = out.iter().rposition(|&s| s != 0).unwrap();
        assert!((last as f64 - 320.0 * MIX_HZ as f64 / 8363.0).abs() < 4.0, "{last}");
    }

    #[test]
    fn volume_comes_from_the_instrument_the_column_and_the_effects() {
        let ins = Ins { volume: 48, ..square(32) };
        let play = |cells: &[At]| ticks(&module(1, 4, 125, &[0], &[pattern(4, cells)], std::slice::from_ref(&ins)));
        let volumes = |cells: &[At]| play(cells).iter().map(|t| t.1).collect::<Vec<_>>();
        // The instrument's, then the column's, then C's.
        assert_eq!(volumes(&[(0, (C4, 1, 0, 0, 0)), (1, (0, 0, 0x30, 0, 0)), (2, (0, 0, 0, 0xC, 0x10))])[..12], [48, 48, 48, 48, 32, 32, 32, 32, 16, 16, 16, 16]);
        // A slides on every tick but the first, and remembers its parameter.
        assert_eq!(volumes(&[(0, (C4, 1, 0, 0xA, 0x02)), (1, (0, 0, 0, 0xA, 0))])[..8], [48, 46, 44, 42, 42, 40, 38, 36]);
        assert_eq!(volumes(&[(0, (C4, 1, 0x10, 0xA, 0x40))])[..4], [0, 4, 8, 12]);
        // The fine slides act once, on the first tick; EC cuts on its tick.
        assert_eq!(volumes(&[(0, (C4, 1, 0, 0xE, 0xB3)), (1, (0, 0, 0, 0xE, 0xA1)), (2, (0, 0, 0, 0xE, 0xC2))])[..12], [45, 45, 45, 45, 46, 46, 46, 46, 46, 46, 0, 0]);
        // An instrument number alone brings the volume back.
        assert_eq!(volumes(&[(0, (C4, 1, 0x10, 0, 0)), (1, (0, 1, 0, 0, 0))])[..8], [0, 0, 0, 0, 48, 48, 48, 48]);
        // The mixer's level follows: half volume, half the peak, and the
        // global volume on top.
        let peak = |cells: &[At]| {
            let out = mixed(&module(1, 4, 125, &[0], &[pattern(4, cells)], &[square(32)]));
            out.iter().map(|s| s.abs()).max().unwrap()
        };
        assert_eq!(peak(&[(0, (C4, 1, 0x30, 0, 0))]), 127 * 256 / 2);
        assert_eq!(peak(&[(0, (C4, 1, 0x30, 0x10, 32))]), 127 * 256 / 4);
    }

    #[test]
    fn pitch_effects_move_the_period_as_fasttracker_does() {
        let play = |speed, cells: &[At]| {
            ticks(&module(1, speed, 125, &[0], &[pattern(4, cells)], &[square(32)])).iter().map(|t| t.0).collect::<Vec<_>>()
        };
        let base = PERIOD_C4;
        // Arpeggio 0xy at speed 4: base, base, y, x; at speed 6: base, y, x twice.
        let s = SEMITONE;
        assert_eq!(play(4, &[(0, (C4, 1, 0, 0, 0x47))])[..4], [base, base, base - 7 * s, base - 4 * s]);
        assert_eq!(play(6, &[(0, (C4, 1, 0, 0, 0x47))])[..6], [base, base - 7 * s, base - 4 * s, base, base - 7 * s, base - 4 * s]);
        assert_eq!(play(5, &[(0, (C4, 1, 0, 0, 0x47))])[..5], [base, base - 4 * s, base, base - 7 * s, base - 4 * s]);
        // And the row after it is the plain note again.
        assert_eq!(play(4, &[(0, (C4, 1, 0, 0, 0x47))])[4], base);
        // Portamento: four units a tick for each of the parameter, not
        // on the first tick, remembered.
        assert_eq!(play(4, &[(0, (C4, 1, 0, 1, 3)), (1, (0, 0, 0, 2, 0))])[..8], [base, base - 12, base - 24, base - 36, base - 36, base - 24, base - 12, base]);
        assert_eq!(play(4, &[(0, (C4, 1, 0, 0xE, 0x12)), (1, (0, 0, 0, 0xE, 0x21))])[..8], [base - 8, base - 8, base - 8, base - 8, base - 4, base - 4, base - 4, base - 4]);
        // Tone portamento goes to the note and stops there; the note
        // does not start the sample again.
        let up = play(4, &[(0, (C4, 1, 0, 0, 0)), (1, (C4 + 1, 0, 0, 3, 8)), (2, (0, 0, 0, 3, 0))]);
        assert_eq!(up[4..12], [base, base - 32, base - 64, base - 64, base - 64, base - 64, base - 64, base - 64]);
        // An instrument's own vibrato: no effect asks for it, it grows to
        // its depth over the sweep and swings both ways.
        let auto = Ins { auto: (4, 16, 32), ..square(32) };
        let seen: Vec<i32> = ticks(&module(1, 16, 125, &[0], &[pattern(1, &[(0, (C4, 1, 0, 0, 0))])], &[auto])).iter().map(|t| t.0 - base).collect();
        assert_eq!(seen[0], 0);
        assert!(seen[1] < 0 && seen[1] > seen[2], "{seen:?}");
        assert_eq!((seen.iter().min(), seen.iter().max()), (Some(&-15), Some(&15)), "{seen:?}");
        // Vibrato swings about the note, up first, and is gone with it.
        let vib = play(6, &[(0, (C4, 1, 0, 4, 0xF4)), (1, (0, 0, 0, 0, 0))]);
        assert_eq!(vib[0], base);
        assert!(vib[1..6].iter().all(|&p| (p - base).abs() <= 4 * 255 / 32) && vib[2] < base && vib[3..6].iter().any(|&p| p > base), "{:?}", &vib[..6]);
        assert_eq!(vib[6], base);
    }

    #[test]
    fn the_song_follows_its_orders_jumps_and_speed() {
        let note = |n| (0, (n, 1, 0, 0, 0));
        let rows = |data: &[u8]| ticks(data).len();
        // Two patterns, played in the order 1, 0, 1.
        let a = pattern(4, &[note(C4)]);
        let b = pattern(8, &[note(C4 + 2)]);
        let data = module(1, 3, 125, &[1, 0, 1], &[a.clone(), b.clone()], &[square(32)]);
        let seen = ticks(&data);
        assert_eq!(seen.len(), (8 + 4 + 8) * 3);
        assert_eq!((seen[0].0, seen[24].0, seen[36].0), (PERIOD_C4 - 2 * SEMITONE, PERIOD_C4, PERIOD_C4 - 2 * SEMITONE));
        // D breaks to a row of the next order, in decimal.
        let brk = pattern(4, &[note(C4), (1, (0, 0, 0, 0xD, 0x05))]);
        assert_eq!(rows(&module(1, 3, 125, &[0, 1], &[brk, b.clone()], &[square(32)])), (2 + 3) * 3);
        // B jumps on; back, or to where it is, is the tune's loop and its end.
        let on = pattern(4, &[note(C4), (0, (0, 0, 0, 0xB, 2))]);
        assert_eq!(rows(&module(1, 3, 125, &[0, 1, 1], &[on, b.clone()], &[square(32)])), (1 + 8) * 3);
        let back = pattern(4, &[note(C4), (2, (0, 0, 0, 0xB, 0))]);
        assert_eq!(rows(&module(1, 3, 125, &[1, 0, 1], &[back, b.clone()], &[square(32)])), (8 + 3) * 3);
        // Played round and round, the jump back is followed and the end
        // of the list goes to the order the header names: no end.
        let rounds = |data: &[u8], ticks: usize| {
            let mut t = Tune::new(data);
            t.set_looping(true);
            (0..ticks).map(|_| { t.step_tick(); assert!(!t.ended); t.channel[0].sounding }).collect::<Vec<_>>()
        };
        let back = pattern(4, &[note(C4), (2, (0, 0, 0, 0xB, 0))]);
        let data = module(1, 3, 125, &[1, 0, 1], &[back, b.clone()], &[square(32)]);
        let seen = rounds(&data, 3 * (8 + 3) * 3);
        assert_eq!(seen[..33], seen[33..66]);
        assert_eq!((seen[0], seen[24], seen[33]), (PERIOD_C4 - 2 * SEMITONE, PERIOD_C4, PERIOD_C4 - 2 * SEMITONE));
        let mut data = module(1, 3, 125, &[1, 0], &[a.clone(), b.clone()], &[square(32)]);
        data[28] = 1;
        let seen = rounds(&data, (8 + 4 + 4) * 3 + 1);
        assert_eq!((seen[0], seen[24], seen[36], seen[47]), (PERIOD_C4 - 2 * SEMITONE, PERIOD_C4, PERIOD_C4, PERIOD_C4));
        // F sets the speed below 0x20 and the tempo from there on.
        let fast = pattern(4, &[(0, (C4, 1, 0, 0xF, 2)), (2, (0, 0, 0, 0xF, 250))]);
        let data = module(1, 6, 125, &[0], &[fast], &[square(32)]);
        assert_eq!(rows(&data), 4 * 2);
        let tick = MIX_HZ as usize * 5 / 2;
        assert_eq!(mixed(&data).len(), 4 * tick / 125 + 4 * tick / 250);
        // A note delayed by EDx starts on tick x; a pattern the file
        // lacks, or a module cut short, ends the tune and nothing more.
        let late = ticks(&module(1, 4, 125, &[0], &[pattern(2, &[(0, (C4, 1, 0, 0xE, 0xD2))])], &[square(32)]));
        assert_eq!(late.iter().map(|t| t.2).collect::<Vec<_>>()[..4], [false, false, true, true]);
        assert_eq!(rows(&module(1, 3, 125, &[0, 9], std::slice::from_ref(&a), &[square(32)])), 4 * 3);
        let whole = module(1, 3, 125, &[0], &[a], &[square(32)]);
        for cut in [HEADER, HEADER + 3, whole.len() - 40] {
            assert!(ticks(&whole[..cut]).len() <= 4 * 3);
        }
    }

    #[test]
    fn the_envelope_shapes_a_note_and_a_key_off_ends_it() {
        // Up over 4 ticks, held at point 1 while the key is down, then
        // down to nothing over 8.
        let ins = Ins { envelope: vec![(0, 0), (4, 64), (12, 0)], flags: 3, sustain: 1, fade: 4096, ..square(32) };
        let amps = |cells: &[At]| {
            let data = module(1, 4, 125, &[0], &[pattern(8, cells)], std::slice::from_ref(&ins));
            let mut t = Tune::new(&data);
            let mut seen = Vec::new();
            loop {
                t.step_tick();
                if t.ended {
                    return seen;
                }
                seen.push(t.channel[0].amp);
            }
        };
        let held = amps(&[(0, (C4, 1, 0, 0, 0))]);
        assert_eq!(held[..6], [0, 1024, 2048, 3072, 4096, 4096]);
        assert_eq!(held[31], 4096, "held at the sustain point");
        // Key off on row 2: the envelope runs on and the fade, an
        // eighth a tick here, runs with it.
        let off = amps(&[(0, (C4, 1, 0, 0, 0)), (2, (KEY_OFF, 0, 0, 0, 0))]);
        assert_eq!(off[7], 4096);
        assert!(off[8..16].windows(2).all(|w| w[1] < w[0]), "{:?}", &off[8..17]);
        assert_eq!(off[16], 0);
        // With no envelope a key-off is silence at once.
        let plain = ticks(&module(1, 4, 125, &[0], &[pattern(4, &[(0, (C4, 1, 0, 0, 0)), (1, (0, 0, 0, 0x14, 2))])], &[square(32)]));
        assert_eq!(plain.iter().map(|t| t.1).collect::<Vec<_>>()[..8], [64, 64, 64, 64, 64, 64, 0, 0]);
    }

    #[test]
    fn the_shaping_cuts_the_bass_and_fills_the_range() {
        // 261 Hz and its odd harmonics: the fundamental goes, the top
        // stays, and the limiter brings what is left to full scale.
        let data = module(1, 6, 125, &[0], &[pattern(16, &[(0, (C4, 1, 0, 0, 0))])], &[square(32)]);
        let mut t = Tune::new(&data);
        let mut out = Vec::new();
        while let Some(s) = t.next_sample() {
            out.push(s as i32 - 128);
        }
        assert_eq!(out.len(), 16 * 6 * (PCM_HZ as usize * 5 / 2 / 125));
        assert_eq!(crate::clip::duration_ms(&data), 0, "the converter's figure, which this test module leaves at nought");
        let settled = &out[2000..];
        let peak = settled.iter().map(|s| s.abs()).max().unwrap();
        assert!((120..=127).contains(&peak), "{peak}");
        let mean = settled.iter().sum::<i32>() as f64 / settled.len() as f64;
        assert!(mean.abs() < 1.0, "{mean}");
        // A quieter note comes out as loud: the limiter's gain.
        let quiet = module(1, 6, 125, &[0], &[pattern(16, &[(0, (C4, 1, 0x20, 0, 0))])], &[square(32)]);
        let mut t = Tune::new(&quiet);
        let mut peak = 0;
        while let Some(s) = t.next_sample() {
            peak = peak.max((s as i32 - 128).abs());
        }
        assert!(peak >= 120, "{peak}");
        // The header may ask for its own drive: at 64 the peaks are half scale.
        let mut soft = data.clone();
        soft[10] = 64;
        soft[11] = 7;
        let mut t = Tune::new(&soft);
        let mut peak = 0;
        while let Some(s) = t.next_sample() {
            peak = peak.max((s as i32 - 128).abs());
        }
        assert!((58..=64).contains(&peak), "{peak}");
        // With no high-pass asked for the fundamental stays: the output
        // is the square itself, at its 261 Hz and not above.
        let crossings = |d: &[u8]| {
            let mut t = Tune::new(d);
            let mut out = Vec::new();
            while let Some(s) = t.next_sample() {
                out.push(s as i32 - 128);
            }
            out[2000..].windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count() as f64 * PCM_HZ as f64 / (out.len() - 2000) as f64
        };
        let mut plain = data.clone();
        plain[16] = 0;
        assert!((crossings(&plain) - 261.3).abs() < 3.0, "{}", crossings(&plain));
        assert!(crossings(&data) > 400.0, "{}", crossings(&data));
        assert!(is_old_tune(b"XMC1....") && !is_old_tune(&data));
        // At twice the rate: twice the samples, the same pitch and level.
        // (The section in the test module is for 8 kHz, so no high-pass.)
        let mut fast = plain.clone();
        fast[17] = 1;
        let mut t = Tune::new(&fast);
        assert_eq!((t.rate(), Tune::new(&plain).rate()), (2 * PCM_HZ, PCM_HZ));
        let mut out = Vec::new();
        while let Some(s) = t.next_sample() {
            out.push(s as i32 - 128);
        }
        assert_eq!(out.len(), 2 * 16 * 6 * (PCM_HZ as usize * 5 / 2 / 125));
        let ups = out[4000..].windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count() as f64 * 2.0 * PCM_HZ as f64 / (out.len() - 4000) as f64;
        assert!((ups - 261.3).abs() < 3.0, "{ups}");
        assert!(out[4000..].iter().map(|s| s.abs()).max().unwrap() >= 120);
        // A tune of rests is silence, exactly.
        let rests = module(2, 6, 125, &[0], &[vec![vec![EMPTY, EMPTY]; 8]], &[square(32)]);
        let mut t = Tune::new(&rests);
        let mut n = 0;
        while let Some(s) = t.next_sample() {
            assert_eq!(s, 128);
            n += 1;
        }
        assert!(n > 0);
    }
}

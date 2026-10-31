// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! HU-058 panel map: which driver, COM pair and SEG line each segment sits
//! on, and a framebuffer with per-position colour that renders to per-slot,
//! per-bit-plane SS words.
//!
//! Everything here is from `3rdparty/HU-058_ESPHome/docs/display-map.md`.

/// COM pair scan order, as the stock firmware runs it. Index into this is the
/// "slot" used throughout.
pub const COM_SEQ: [u8; 4] = [0x30, 0x0C, 0x03, 0xC0];

/// Number of COM slots in a scan cycle.
pub const SLOTS: usize = COM_SEQ.len();

/// Binary-weighted sub-frames per slot. A channel level is 0..2^PLANES.
/// Three, not four: every extra plane doubles the cycle length, and the
/// phantom-array shimmer of a short pulse train is visible into the
/// kilohertz, so the scan rate matters more than the extra colour depth.
pub const PLANES: usize = 3;

/// Highest channel level.
pub const MAX_LEVEL: u8 = (1 << PLANES) - 1;

/// Each digit is one driver plus a pair of COM pairs, ten LED positions.
#[derive(Clone, Copy)]
pub struct Block {
    pub drv: usize,
    pub com_lo: u8,
    pub com_hi: u8,
}

/// Blocks left to right. Driver 0 carries the hours, driver 1 the minutes.
pub const BLOCKS: [Block; 4] = [
    Block { drv: 0, com_lo: 0x30, com_hi: 0xC0 }, // hour tens,   annunciator AM
    Block { drv: 0, com_lo: 0x03, com_hi: 0x0C }, // hour ones,   annunciator colon
    Block { drv: 1, com_lo: 0x30, com_hi: 0xC0 }, // minute tens, annunciator date dash
    Block { drv: 1, com_lo: 0x03, com_hi: 0x0C }, // minute ones, annunciator degree mark
];

/// Position of a segment within a block. `hi` selects `com_hi` over
/// `com_lo`, `led` is 1..=5.
#[derive(Clone, Copy)]
pub struct SegPos {
    pub hi: bool,
    pub led: u8,
}

/// Segment order: A B C D E F G, then the annunciator, then the second
/// annunciator (lower colon dot, block 1 only).
pub const SEGMAP: [SegPos; 9] = [
    SegPos { hi: true, led: 5 },  // A top
    SegPos { hi: true, led: 4 },  // B top right
    SegPos { hi: true, led: 3 },  // C bottom right
    SegPos { hi: true, led: 2 },  // D bottom
    SegPos { hi: false, led: 5 }, // E bottom left
    SegPos { hi: false, led: 4 }, // F top left
    SegPos { hi: false, led: 3 }, // G middle
    SegPos { hi: true, led: 1 },  // annunciator
    SegPos { hi: false, led: 1 }, // second annunciator, block 1 only
];

pub const SEG_A: usize = 0;
pub const SEG_G: usize = 6;
pub const SEG_ANNUN: usize = 7;
pub const SEG_ANNUN2: usize = 8;
pub const SEGS: usize = SEGMAP.len();

/// Duty level per channel, 0..=MAX_LEVEL, linear light. The panel's
/// colours are written in this form directly; there is no RGB in between.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Levels {
    pub blue: u8,
    pub green: u8,
    pub red: u8,
}

impl Levels {
    pub const fn new(blue: u8, green: u8, red: u8) -> Self {
        Levels { blue, green, red }
    }

    /// Levels in SEG order within an LED triple: +0 blue, +1 green, +2 red.
    pub const fn by_channel(self) -> [u8; 3] {
        [self.blue, self.green, self.red]
    }
}

/// Seven segment font, bit 0 = A through bit 6 = G.
pub const fn glyph(c: char) -> u8 {
    match c {
        '0' => 0b0111111,
        '1' => 0b0000110,
        '2' => 0b1011011,
        '3' => 0b1001111,
        '4' => 0b1100110,
        '5' => 0b1101101,
        '6' => 0b1111101,
        '7' => 0b0000111,
        '8' => 0b1111111,
        '9' => 0b1101111,
        '-' => 0b1000000,
        'A' => 0b1110111,
        'b' => 0b1111100,
        'C' => 0b0111001,
        'c' => 0b1011000,
        'd' => 0b1011110,
        'E' => 0b1111001,
        'F' => 0b1110001,
        'H' => 0b1110110,
        'h' => 0b1110100,
        'L' => 0b0111000,
        'n' => 0b1010100,
        'o' => 0b1011100,
        'P' => 0b1110011,
        'r' => 0b1010000,
        't' => 0b1111000,
        'U' => 0b0111110,
        'u' => 0b0011100,
        'y' => 0b1101110,
        _ => 0,
    }
}

const fn com_index(cs: u8) -> usize {
    let mut i = 0;
    while i < SLOTS {
        if COM_SEQ[i] == cs {
            return i;
        }
        i += 1;
    }
    0
}

/// What the scan engine needs for one cycle: an SS word per driver, per COM
/// slot, per bit plane, and the current step shared by both drivers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScanImage {
    pub ss: [[[u16; PLANES]; SLOTS]; 2],
    pub is_step: u8,
}

impl ScanImage {
    pub const DARK: ScanImage = ScanImage { ss: [[[0; PLANES]; SLOTS]; 2], is_step: 0 };
}

/// Segment framebuffer with a colour per position and a current step.
#[derive(Clone, Copy)]
pub struct Panel {
    on: [[bool; SEGS]; 4],
    level: [[Levels; SEGS]; 4],
    is_step: u8,
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel {
    pub const fn new() -> Self {
        Panel {
            on: [[false; SEGS]; 4],
            level: [[Levels::new(0, 0, MAX_LEVEL); SEGS]; 4],
            is_step: 5,
        }
    }

    /// Turn every position off. Colours are kept.
    pub fn clear(&mut self) {
        self.on = [[false; SEGS]; 4];
    }

    /// Colour of the whole panel.
    pub fn set_color(&mut self, color: Levels) {
        self.level = [[color; SEGS]; 4];
    }

    /// Constant-current step, 0 (2.5 mA) to 15 (40.4 mA) per lit sink.
    /// This is brightness. It must never be derived from what is lit.
    pub fn set_current(&mut self, is_step: u8) {
        self.is_step = is_step.min(15);
    }

    /// Write one character into a digit. Leaves the annunciators alone.
    pub fn set_char(&mut self, block: usize, c: char) {
        let bits = glyph(c);
        for seg in SEG_A..=SEG_G {
            self.on[block][seg] = bits & (1 << seg) != 0;
        }
    }

    /// Both colon dots. Block 1 annunciator is the upper dot, its second
    /// annunciator the lower one.
    pub fn set_colon(&mut self, on: bool) {
        self.on[1][SEG_ANNUN] = on;
        self.on[1][SEG_ANNUN2] = on;
    }

    pub fn set_am(&mut self, on: bool) {
        self.on[0][SEG_ANNUN] = on;
    }

    pub fn set_degree(&mut self, on: bool) {
        self.on[3][SEG_ANNUN] = on;
    }

    /// Light every wired position. A hardware check; keep the current low.
    pub fn lamp_test(&mut self) {
        for (b, blk) in self.on.iter_mut().enumerate() {
            for (s, on) in blk.iter_mut().enumerate() {
                *on = s != SEG_ANNUN2 || b == 1;
            }
        }
    }

    /// Collapse the framebuffer into per-slot, per-plane SS words. A channel
    /// at level L is lit in plane k when bit k of L is set.
    pub fn render(&self) -> ScanImage {
        let mut img = ScanImage { ss: [[[0; PLANES]; SLOTS]; 2], is_step: self.is_step };
        for (b, blk) in BLOCKS.iter().enumerate() {
            for (seg, pos) in SEGMAP.iter().enumerate() {
                if !self.on[b][seg] {
                    continue;
                }
                let cs = if pos.hi { blk.com_hi } else { blk.com_lo };
                let slot = com_index(cs);
                let base = 3 * pos.led as u16 - 2; // LED1 is SEG1..SEG3
                for (ch, level) in self.level[b][seg].by_channel().iter().enumerate() {
                    let bit = 1u16 << (base + ch as u16);
                    for (k, plane) in img.ss[blk.drv][slot].iter_mut().enumerate() {
                        if level & (1 << k) != 0 {
                            *plane |= bit;
                        }
                    }
                }
            }
        }
        img
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_use_seven_segments_and_unknown_is_blank() {
        for c in "0123456789-AbCcdEFHhLnoPrtUuy".chars() {
            let g = glyph(c);
            assert!(g != 0 && g < 0x80, "{c:?} -> {g:#09b}");
        }
        assert_eq!(glyph(' '), 0);
        assert_eq!(glyph('z'), 0);
        assert_eq!(glyph('8'), 0b1111111);
    }

    #[test]
    fn a_dark_panel_renders_dark() {
        let img = Panel::new().render();
        assert_eq!(img.ss, ScanImage::DARK.ss);
        assert_eq!(img.is_step, 5);
    }

    #[test]
    fn levels_go_to_their_bit_planes() {
        let mut p = Panel::new();
        p.set_color(Levels::new(1, 2, 4)); // blue in plane 0, green in plane 1, red in plane 2
        p.set_char(1, '1'); // segments B and C: com_hi of block 1, LED 4 and LED 3
        let img = p.render();
        let slot = COM_SEQ.iter().position(|&c| c == BLOCKS[1].com_hi).unwrap();
        let base = |led: u16| 3 * led - 2;
        for (plane, ch) in (0..PLANES).zip(0u16..) {
            let expect = (1u16 << (base(4) + ch)) | (1u16 << (base(3) + ch));
            assert_eq!(img.ss[0][slot][plane], expect, "plane {plane}");
        }
        // Nothing else lit: the other slots of driver 0, and driver 1.
        for s in 0..SLOTS {
            if s != slot {
                assert_eq!(img.ss[0][s], [0; PLANES]);
            }
            assert_eq!(img.ss[1][s], [0; PLANES]);
        }
    }

    #[test]
    fn annunciators_sit_on_led_one_of_their_block() {
        let mut p = Panel::new();
        p.set_color(Levels::new(MAX_LEVEL, MAX_LEVEL, MAX_LEVEL));
        p.set_colon(true);
        p.set_am(true);
        p.set_degree(true);
        let img = p.render();
        let slot_of = |cs| COM_SEQ.iter().position(|&c| c == cs).unwrap();
        let led1: u16 = 0b1110; // SEG1..SEG3, all three channels
        // Colon: block 1, upper dot on com_hi, lower on com_lo.
        assert_eq!(img.ss[0][slot_of(BLOCKS[1].com_hi)], [led1; PLANES]);
        assert_eq!(img.ss[0][slot_of(BLOCKS[1].com_lo)], [led1; PLANES]);
        // AM: block 0 com_hi. Degree: block 3 (driver 1) com_hi.
        assert_eq!(img.ss[0][slot_of(BLOCKS[0].com_hi)], [led1; PLANES]);
        assert_eq!(img.ss[1][slot_of(BLOCKS[3].com_hi)], [led1; PLANES]);
        assert_eq!(img.ss[1][slot_of(BLOCKS[2].com_hi)], [0; PLANES]);
    }

    #[test]
    fn current_step_is_clamped_and_clear_keeps_colour() {
        let mut p = Panel::new();
        p.set_current(99);
        assert_eq!(p.render().is_step, 15);
        p.set_color(Levels::new(0, 0, 1));
        p.set_char(0, '8');
        p.clear();
        assert_eq!(p.render().ss, ScanImage::DARK.ss);
        p.set_char(0, '-');
        let img = p.render();
        assert!(img.ss[0].iter().flatten().any(|&w| w != 0));
    }

    #[test]
    fn lamp_test_lights_every_wired_position() {
        let mut p = Panel::new();
        p.set_color(Levels::new(1, 1, 1));
        p.lamp_test();
        let img = p.render();
        for drv in 0..2 {
            for slot in 0..SLOTS {
                // LEDs 1 to 5 of every COM pair are wired, except LED 1 on
                // the second annunciator's pair of blocks 0, 2 and 3.
                let lit = img.ss[drv][slot][0];
                assert!(lit & 0b0111_1111_1111_1110 != 0, "drv {drv} slot {slot} dark");
            }
        }
    }
}

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

/// 8-bit colour as a person would pick it. Perceptual, not linear.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    pub const RED: Rgb = Rgb::new(255, 0, 0);
    pub const GREEN: Rgb = Rgb::new(0, 255, 0);
    pub const BLUE: Rgb = Rgb::new(0, 0, 255);
    pub const YELLOW: Rgb = Rgb::new(255, 255, 0);
    pub const CYAN: Rgb = Rgb::new(0, 255, 255);
    pub const MAGENTA: Rgb = Rgb::new(255, 0, 255);
    pub const WHITE: Rgb = Rgb::new(255, 255, 255);
    pub const ORANGE: Rgb = Rgb::new(255, 96, 0);
}

/// Duty level per channel, 0..=MAX_LEVEL, linear light.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Levels {
    pub blue: u8,
    pub green: u8,
    pub red: u8,
}

impl Levels {
    pub const OFF: Levels = Levels { blue: 0, green: 0, red: 0 };

    /// Linearise an 8-bit colour into duty levels.
    pub fn from_rgb(c: Rgb) -> Self {
        Levels {
            blue: GAMMA[c.b as usize],
            green: GAMMA[c.g as usize],
            red: GAMMA[c.r as usize],
        }
    }

    /// Levels in SEG order within an LED triple: +0 blue, +1 green, +2 red.
    pub const fn by_channel(self) -> [u8; 3] {
        [self.blue, self.green, self.red]
    }
}

/// 8-bit component to duty level 0..=MAX_LEVEL, gamma 2.2.
const GAMMA: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
    6, 6, 6, 6, 6, 6, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7,
];

/// Brightness percent to current step. Perceived output is interpolated
/// between the 2.5 mA floor and 40.4 mA with gamma 2.8 and snapped to the
/// nearest step, as the reference firmware does, so all sixteen steps are
/// spread across the range rather than a third of it being clamped at the
/// floor.
const BRIGHTNESS_STEP: [u8; 101] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3,
    3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 6, 6, 6,
    6, 6, 6, 7, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 9, 9, 9, 9, 10, 10,
    10, 10, 10, 11, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, 14, 14, 14, 15,
    15,
];

/// Current step (0..=15) for a brightness in percent.
pub const fn current_step_for(percent: u8) -> u8 {
    BRIGHTNESS_STEP[if percent > 100 { 100 } else { percent as usize }]
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
            level: [[Levels { blue: 0, green: 0, red: MAX_LEVEL }; SEGS]; 4],
            is_step: 5,
        }
    }

    /// Turn every position off. Colours are kept.
    pub fn clear(&mut self) {
        self.on = [[false; SEGS]; 4];
    }

    /// Colour of the whole panel.
    pub fn set_color(&mut self, color: Rgb) {
        let l = Levels::from_rgb(color);
        self.level = [[l; SEGS]; 4];
    }

    /// Colour of one digit block, annunciators included.
    pub fn set_digit_color(&mut self, block: usize, color: Rgb) {
        self.level[block] = [Levels::from_rgb(color); SEGS];
    }

    /// Colour of one position.
    pub fn set_position_color(&mut self, block: usize, seg: usize, color: Rgb) {
        self.level[block][seg] = Levels::from_rgb(color);
    }

    /// Constant-current step, 0 (2.5 mA) to 15 (40.4 mA) per lit sink.
    /// This is brightness. It must never be derived from what is lit.
    pub fn set_current(&mut self, is_step: u8) {
        self.is_step = is_step.min(15);
    }

    pub fn set_segment(&mut self, block: usize, seg: usize, on: bool) {
        self.on[block][seg] = on;
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

    pub fn set_dash(&mut self, on: bool) {
        self.on[2][SEG_ANNUN] = on;
    }

    pub fn set_degree(&mut self, on: bool) {
        self.on[3][SEG_ANNUN] = on;
    }

    /// HH:MM with the leading zero of the hour suppressed.
    pub fn set_time(&mut self, hour: u8, minute: u8) {
        let hour = hour % 24;
        let minute = minute % 60;
        self.set_char(0, if hour >= 10 { digit(hour / 10) } else { ' ' });
        self.set_char(1, digit(hour % 10));
        self.set_char(2, digit(minute / 10));
        self.set_char(3, digit(minute % 10));
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

const fn digit(d: u8) -> char {
    (b'0' + d) as char
}

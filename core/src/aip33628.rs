//! AiP33628 frame format and the quad-SPI waveform encoder.
//!
//! The chip is a 30-bit shift register in front of an output latch, two wires
//! (CLK, DIN), no strobe. Frame, LSB first on the wire:
//!
//! | bits  | field    | meaning                                      |
//! |-------|----------|----------------------------------------------|
//! | 0-15  | SS[15:0] | SEG15..SEG0 sink on (1 = on)                 |
//! | 16-23 | CS[7:0]  | COM7..COM0 anode driven (1 = on)             |
//! | 24-27 | IS[3:0]  | constant-current step, bit reversed on wire  |
//! | 28-29 | reserved | 0                                            |
//!
//! Shift on CLK rising edge. Latch = DIN rising edge while CLK is high after
//! bit 29. DIN must not change while CLK is high during the shift.
//!
//! Both drivers of the panel are driven at once: the four wires are the four
//! data lines of the SPI peripheral in quad mode, and every nibble the SPI
//! clocks out is the complete state of all four wires for one clock period.

/// Current step (0..=15) to the nibble that goes on the wire. The frame
/// carries IS[0] at bit 27 and IS[3] at bit 24, so the field is bit reversed.
/// The table is its own inverse.
pub const IS_WIRE: [u8; 16] = [
    0x0, 0x8, 0x4, 0xC, 0x2, 0xA, 0x6, 0xE, 0x1, 0x9, 0x5, 0xD, 0x3, 0xB, 0x7, 0xF,
];

/// Nibble bit that drives each wire. The bit index is the SPI SIO line.
pub const BIT_DATA1: u8 = 1 << 0; // SIO0 -> DATA,   driver 1
pub const BIT_CLK1: u8 = 1 << 1; // SIO1 -> CLK,    driver 1
pub const BIT_DATA2: u8 = 1 << 2; // SIO2 -> DATA_1, driver 2
pub const BIT_CLK2: u8 = 1 << 3; // SIO3 -> CLK_1,  driver 2

const CLKS: u8 = BIT_CLK1 | BIT_CLK2;
const DATAS: u8 = BIT_DATA1 | BIT_DATA2;

/// Bits in a frame.
pub const FRAME_BITS: usize = 30;

/// Nibbles a frame to both drivers takes: three per bit except the last,
/// which leaves CLK high, plus the four-nibble latch tail.
pub const FRAME_NIBBLES: usize = (FRAME_BITS - 1) * 3 + 2 + 4;

/// Bytes a frame occupies in the SPI stream, rounded up to whole bytes.
pub const FRAME_BYTES: usize = FRAME_NIBBLES.div_ceil(2);

/// Pack SS, CS and the current step into the 30-bit frame word.
pub const fn frame_word(ss: u16, cs: u8, is_step: u8) -> u32 {
    (ss as u32) | ((cs as u32) << 16) | ((IS_WIRE[(is_step & 0xF) as usize] as u32) << 24)
}

struct NibbleWriter<'a> {
    out: &'a mut [u8],
    idx: usize,
}

impl<'a> NibbleWriter<'a> {
    fn new(out: &'a mut [u8]) -> Self {
        out.fill(0);
        Self { out, idx: 0 }
    }

    /// Append one nibble. The SPI peripheral sends the high nibble of each
    /// byte first (MSB-first write order), so nibble 0 is the high half of
    /// byte 0.
    #[inline]
    fn push(&mut self, nibble: u8) {
        let byte = &mut self.out[self.idx / 2];
        if self.idx.is_multiple_of(2) {
            *byte = nibble << 4;
        } else {
            *byte |= nibble & 0xF;
        }
        self.idx += 1;
    }
}

/// Encode one frame to both drivers, with the same CS and IS and separate
/// SS words, as `FRAME_BYTES` of quad-SPI nibbles. Trailing nibbles are
/// zero, which leaves all four wires low.
///
/// Per bit: data set with clocks low, clocks high (shift), clocks low with
/// data still stable. Three steps rather than two so DIN never changes in the
/// same step as a CLK falling edge, which the chip could read as a latch.
pub fn encode_pair(ss1: u16, ss2: u16, cs: u8, is_step: u8, out: &mut [u8; FRAME_BYTES]) {
    let f1 = frame_word(ss1, cs, is_step);
    let f2 = frame_word(ss2, cs, is_step);
    let mut w = NibbleWriter::new(out);

    for i in 0..FRAME_BITS {
        let mut d = 0;
        if (f1 >> i) & 1 != 0 {
            d |= BIT_DATA1;
        }
        if (f2 >> i) & 1 != 0 {
            d |= BIT_DATA2;
        }
        w.push(d);
        w.push(d | CLKS);
        if i < FRAME_BITS - 1 {
            w.push(d);
        }
    }

    // CLK is still high after bit 29 (both reserved bits are 0, so DIN is
    // already low). A DIN rising edge now is the latch.
    w.push(CLKS);
    w.push(CLKS | DATAS);
    w.push(CLKS);
    w.push(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_wire_is_its_own_inverse() {
        for i in 0..16u8 {
            assert_eq!(IS_WIRE[IS_WIRE[i as usize] as usize], i);
        }
        assert_eq!(IS_WIRE[1], 0x8, "IS[0] goes to the top of the field");
    }

    #[test]
    fn frame_word_layout() {
        assert_eq!(frame_word(0xFFFF, 0, 0), 0xFFFF);
        assert_eq!(frame_word(0, 0xFF, 0), 0xFF << 16);
        assert_eq!(frame_word(0, 0, 1), 0x8 << 24);
        assert_eq!(frame_word(0, 0, 15), 0xF << 24);
        assert_eq!(frame_word(0xFFFF, 0xFF, 0xFF) >> 28, 0, "the reserved bits stay 0");
    }

    /// One AiP33628 as the datasheet describes it: shift on the CLK rising
    /// edge, latch on a DIN rising edge while CLK is high.
    #[derive(Default)]
    struct Chip {
        clk: bool,
        din: bool,
        shift: u32,
        count: u32,
        latched: Vec<(u32, u32)>,
        din_moved_under_clk: u32,
    }

    impl Chip {
        fn step(&mut self, clk: bool, din: bool) {
            if clk && !self.clk {
                self.shift |= (din as u32) << self.count;
                self.count += 1;
            }
            if clk && self.clk && din != self.din {
                // DIN changing while CLK is high: a latch if rising.
                self.din_moved_under_clk += 1;
                if din {
                    self.latched.push((self.shift, self.count));
                }
            }
            self.clk = clk;
            self.din = din;
        }
    }

    fn run(out: &[u8; FRAME_BYTES]) -> (Chip, Chip) {
        let (mut a, mut b) = (Chip::default(), Chip::default());
        for byte in out {
            for nibble in [byte >> 4, byte & 0xF] {
                a.step(nibble & BIT_CLK1 != 0, nibble & BIT_DATA1 != 0);
                b.step(nibble & BIT_CLK2 != 0, nibble & BIT_DATA2 != 0);
            }
        }
        (a, b)
    }

    #[test]
    fn encode_pair_shifts_thirty_bits_and_latches_once() {
        let mut out = [0u8; FRAME_BYTES];
        for (ss1, ss2, cs, is) in [(0x0001, 0x8000, 0x30, 5), (0xFFFF, 0x0000, 0xC0, 15), (0x5A5A, 0xA5A5, 0x03, 0), (0, 0, 0, 0)] {
            encode_pair(ss1, ss2, cs, is, &mut out);
            let (a, b) = run(&out);
            for (chip, ss) in [(&a, ss1), (&b, ss2)] {
                assert_eq!(chip.count, FRAME_BITS as u32, "{ss:#06x}: bits shifted");
                assert_eq!(chip.latched, vec![(frame_word(ss, cs, is), FRAME_BITS as u32)], "{ss:#06x}: one latch of the whole frame");
                // The only DIN move under a high CLK is the latch itself (up, then down).
                assert_eq!(chip.din_moved_under_clk, 2, "{ss:#06x}: DIN quiet during the shift");
            }
            // Trailing nibbles are zero: all wires low after the frame.
            let last = out[FRAME_BYTES - 1];
            assert_eq!(last & 0xF, 0);
        }
        assert_eq!(FRAME_NIBBLES, 29 * 3 + 2 + 4);
        assert_eq!(FRAME_BYTES, FRAME_NIBBLES.div_ceil(2));
    }
}

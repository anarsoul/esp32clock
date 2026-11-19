// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A Commodore 64 tune (.sid, PSID) as a clip for the clock's own SID
//! (`onebitdawn_core::sid`).
//!
//! A .sid file is the C64's own player routine and its data: 6502 code
//! to be called once a frame, each call writing some of the SID chip's
//! registers. The clock has a SID but no 6502, so this runs the code
//! here, in a 6502 of its own (`Cpu`), and the clip is what reached the
//! chip: the register writes of each frame, in order, less the ones
//! that change nothing.
//!
//! A tune goes on for ever, so the machine's state is remembered after
//! every call: when one comes round again the tune has looped, the
//! clip ends there and says where it starts over. `seconds` cuts it
//! sooner; a tune cut with no loop found starts over from its
//! beginning.
//!
//! The record is written packed (`SID2`): writes and waits that the
//! tune has had before are a reference to them (`packrefs`), a fifth
//! of the bytes for a long tune.

use std::collections::{BTreeMap, HashMap};

use onebitdawn_core::refs::{self, Refs};
use onebitdawn_core::sid::{MAGIC, MIN_REF, PACKED_MAGIC, REGISTERS};

use crate::bytes::{Data, text};
use crate::packrefs::{Tokens, leb128, pack};
use crate::shape::section;
use crate::{Options, Output};

const MIN: usize = MIN_REF as usize;
const HEADER: usize = 40;
const REGS: usize = REGISTERS as usize;
const FAST: u32 = 31250;
/// Clock in Hz, cycles a screen, and what CIA 1's timer A holds when
/// the KERNAL has set it up: 60 Hz.
const PAL: (u64, u64, u64) = (985248, 312 * 63, 0x4025);
const NTSC: (u64, u64, u64) = (1022727, 263 * 65, 0x4295);
/// A routine that runs this many instructions is not coming back.
const RUNAWAY: usize = 2_000_000;
/// Where a routine called from here returns to.
const HOME: u16 = 0xFFF0;

/// The options a tune takes.
pub const OPTIONS: &[&str] = &["song", "seconds", "mute", "model", "system", "highpass", "passes", "drive", "release", "steps", "rate", "flat", "limiter", "plain"];

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Imp,
    Acc,
    Imm,
    Zp,
    Zpx,
    Zpy,
    Abs,
    Abx,
    Aby,
    Izx,
    Izy,
    Rel,
    Ind,
}
use Mode::*;

#[derive(Clone, Copy, PartialEq)]
enum Flag {
    N,
    V,
    D,
    I,
    Z,
    C,
}

#[derive(Clone, Copy, PartialEq)]
enum Reg {
    A,
    X,
    Y,
    Sp,
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Ora,
    And,
    Eor,
    Adc,
    Sta,
    Lda,
    Cmp,
    Sbc,
    Asl,
    Rol,
    Lsr,
    Ror,
    Dec,
    Inc,
    Slo,
    Rla,
    Sre,
    Rra,
    Dcp,
    Isc,
    Stx,
    Ldx,
    Sty,
    Ldy,
    Cpx,
    Cpy,
    Bit,
    Jmp,
    Jsr,
    Rts,
    Rti,
    Brk,
    Sax,
    Lax,
    Anc,
    Alr,
    Arr,
    Sbx,
    Nop,
    /// Taken when the flag is this.
    Branch(Flag, bool),
    Set(Flag, bool),
    /// To, from; flags by the value.
    Move(Reg, Reg),
    Txs,
    Pha,
    Pla,
    Php,
    Plp,
    Step(Reg, u8),
}

/// The instructions by their codes. Put in this order: a later line
/// takes a code an earlier one's pattern gave away.
fn table() -> [Option<(Op, Mode)>; 256] {
    let mut ops = [None; 256];
    let mut put = |what: Op, codes: &[(u8, Mode)]| {
        for &(code, mode) in codes {
            ops[code as usize] = Some((what, mode));
        }
    };
    let column = [(0x01, Izx), (0x05, Zp), (0x09, Imm), (0x0D, Abs), (0x11, Izy), (0x15, Zpx), (0x19, Aby), (0x1D, Abx)];
    for (base, what) in [(0x00, Op::Ora), (0x20, Op::And), (0x40, Op::Eor), (0x60, Op::Adc), (0x80, Op::Sta), (0xA0, Op::Lda), (0xC0, Op::Cmp), (0xE0, Op::Sbc)] {
        for (low, mode) in column {
            if base + low != 0x89 {
                put(what, &[(base + low, mode)]);
            }
        }
    }
    let column = [(0x06, Zp), (0x0A, Acc), (0x0E, Abs), (0x16, Zpx), (0x1E, Abx)];
    for (base, what) in [(0x00, Op::Asl), (0x20, Op::Rol), (0x40, Op::Lsr), (0x60, Op::Ror), (0xC0, Op::Dec), (0xE0, Op::Inc)] {
        for (low, mode) in column {
            if mode != Acc || base < 0x80 {
                put(what, &[(base + low, mode)]);
            }
        }
    }
    // The undocumented ones that players use.
    let column = [(0x03, Izx), (0x07, Zp), (0x0F, Abs), (0x13, Izy), (0x17, Zpx), (0x1B, Aby), (0x1F, Abx)];
    for (base, what) in [(0x00, Op::Slo), (0x20, Op::Rla), (0x40, Op::Sre), (0x60, Op::Rra), (0xC0, Op::Dcp), (0xE0, Op::Isc)] {
        for (low, mode) in column {
            put(what, &[(base + low, mode)]);
        }
    }
    put(Op::Stx, &[(0x86, Zp), (0x96, Zpy), (0x8E, Abs)]);
    put(Op::Ldx, &[(0xA2, Imm), (0xA6, Zp), (0xB6, Zpy), (0xAE, Abs), (0xBE, Aby)]);
    put(Op::Sty, &[(0x84, Zp), (0x94, Zpx), (0x8C, Abs)]);
    put(Op::Ldy, &[(0xA0, Imm), (0xA4, Zp), (0xB4, Zpx), (0xAC, Abs), (0xBC, Abx)]);
    put(Op::Cpx, &[(0xE0, Imm), (0xE4, Zp), (0xEC, Abs)]);
    put(Op::Cpy, &[(0xC0, Imm), (0xC4, Zp), (0xCC, Abs)]);
    put(Op::Bit, &[(0x24, Zp), (0x2C, Abs)]);
    put(Op::Jmp, &[(0x4C, Abs), (0x6C, Ind)]);
    put(Op::Jsr, &[(0x20, Abs)]);
    put(Op::Rts, &[(0x60, Imp)]);
    put(Op::Rti, &[(0x40, Imp)]);
    put(Op::Brk, &[(0x00, Imp)]);
    put(Op::Sax, &[(0x83, Izx), (0x87, Zp), (0x8F, Abs), (0x97, Zpy)]);
    put(Op::Lax, &[(0xA3, Izx), (0xA7, Zp), (0xAF, Abs), (0xB3, Izy), (0xB7, Zpy), (0xBF, Aby)]);
    put(Op::Anc, &[(0x0B, Imm), (0x2B, Imm)]);
    put(Op::Alr, &[(0x4B, Imm)]);
    put(Op::Arr, &[(0x6B, Imm)]);
    put(Op::Sbx, &[(0xCB, Imm)]);
    put(Op::Sbc, &[(0xEB, Imm)]);
    for code in [0xEA, 0x1A, 0x3A, 0x5A, 0x7A, 0xDA, 0xFA] {
        put(Op::Nop, &[(code, Imp)]);
    }
    for code in [0x80, 0x82, 0x89, 0xC2, 0xE2] {
        put(Op::Nop, &[(code, Imm)]);
    }
    for code in [0x04, 0x44, 0x64] {
        put(Op::Nop, &[(code, Zp)]);
    }
    for code in [0x14, 0x34, 0x54, 0x74, 0xD4, 0xF4] {
        put(Op::Nop, &[(code, Zpx)]);
    }
    put(Op::Nop, &[(0x0C, Abs)]);
    for code in [0x1C, 0x3C, 0x5C, 0x7C, 0xDC, 0xFC] {
        put(Op::Nop, &[(code, Abx)]);
    }
    for (code, flag) in [(0x10, Flag::N), (0x50, Flag::V), (0x90, Flag::C), (0xD0, Flag::Z)] {
        put(Op::Branch(flag, false), &[(code, Rel)]);
        put(Op::Branch(flag, true), &[(code + 0x20, Rel)]);
    }
    for (code, flag, value) in [(0x18, Flag::C, false), (0x38, Flag::C, true), (0x58, Flag::I, false), (0x78, Flag::I, true), (0xB8, Flag::V, false), (0xD8, Flag::D, false), (0xF8, Flag::D, true)] {
        put(Op::Set(flag, value), &[(code, Imp)]);
    }
    put(Op::Move(Reg::X, Reg::A), &[(0xAA, Imp)]);
    put(Op::Move(Reg::A, Reg::X), &[(0x8A, Imp)]);
    put(Op::Move(Reg::Y, Reg::A), &[(0xA8, Imp)]);
    put(Op::Move(Reg::A, Reg::Y), &[(0x98, Imp)]);
    put(Op::Move(Reg::X, Reg::Sp), &[(0xBA, Imp)]);
    put(Op::Txs, &[(0x9A, Imp)]);
    put(Op::Pha, &[(0x48, Imp)]);
    put(Op::Pla, &[(0x68, Imp)]);
    put(Op::Php, &[(0x08, Imp)]);
    put(Op::Plp, &[(0x28, Imp)]);
    put(Op::Step(Reg::X, 1), &[(0xE8, Imp)]);
    put(Op::Step(Reg::X, 0xFF), &[(0xCA, Imp)]);
    put(Op::Step(Reg::Y, 1), &[(0xC8, Imp)]);
    put(Op::Step(Reg::Y, 0xFF), &[(0x88, Imp)]);
    ops
}

/// A 6502 with 64 KB of RAM and a SID's registers at $D400.
struct Cpu {
    mem: Vec<u8>,
    a: u8,
    x: u8,
    y: u8,
    sp: u8,
    pc: u16,
    n: bool,
    v: bool,
    d: bool,
    i: bool,
    z: bool,
    c: bool,
    /// The chip's registers as last written, and the writes of the
    /// frame in hand that changed one.
    sid: [u8; REGS],
    writes: Vec<(u8, u8)>,
    volume_writes: usize,
    notes: BTreeMap<String, usize>,
    raster: u8,
    ops: [Option<(Op, Mode)>; 256],
}

impl Cpu {
    fn new() -> Self {
        Cpu {
            mem: vec![0; 0x10000],
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFF,
            pc: 0,
            n: false,
            v: false,
            d: false,
            i: false,
            z: false,
            c: false,
            sid: [0; REGS],
            writes: Vec::new(),
            volume_writes: 0,
            notes: BTreeMap::new(),
            raster: 0,
            ops: table(),
        }
    }

    // Memory.

    fn read(&mut self, addr: u16) -> u8 {
        if (0xD400..0xD800).contains(&addr) {
            let register = addr & 0x1F;
            if register == 0x1B || register == 0x1C {
                let what = if register == 0x1B { "oscillator" } else { "envelope" };
                *self.notes.entry(format!("reads voice 3's {what}, which is nought here")).or_default() += 1;
                return 0;
            }
        } else if addr == 0xD012 {
            // The raster line: something that moves, for a routine
            // that waits on it.
            self.raster = self.raster.wrapping_add(1);
            return self.raster;
        }
        self.mem[addr as usize]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.mem[addr as usize] = value;
        if (0xD400..0xD800).contains(&addr) {
            let register = (addr & 0x1F) as usize;
            if register < REGS {
                if register == 24 {
                    self.volume_writes += 1;
                }
                if self.sid[register] != value {
                    self.sid[register] = value;
                    self.writes.push((register as u8, value));
                }
            }
        }
    }

    fn at(&self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }

    fn word(&self, addr: u16) -> u16 {
        self.at(addr) as u16 | (self.at(addr.wrapping_add(1)) as u16) << 8
    }

    fn push(&mut self, value: u8) {
        self.mem[0x100 + self.sp as usize] = value;
        self.sp = self.sp.wrapping_sub(1);
    }

    fn pull(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.mem[0x100 + self.sp as usize]
    }

    fn flags(&self) -> u8 {
        (self.n as u8) << 7 | (self.v as u8) << 6 | 0x30 | (self.d as u8) << 3 | (self.i as u8) << 2 | (self.z as u8) << 1 | self.c as u8
    }

    fn set_flags(&mut self, p: u8) {
        (self.n, self.v, self.d, self.i, self.z, self.c) = (p & 0x80 != 0, p & 0x40 != 0, p & 8 != 0, p & 4 != 0, p & 2 != 0, p & 1 != 0);
    }

    fn flag(&mut self, flag: Flag) -> &mut bool {
        match flag {
            Flag::N => &mut self.n,
            Flag::V => &mut self.v,
            Flag::D => &mut self.d,
            Flag::I => &mut self.i,
            Flag::Z => &mut self.z,
            Flag::C => &mut self.c,
        }
    }

    fn reg(&mut self, reg: Reg) -> &mut u8 {
        match reg {
            Reg::A => &mut self.a,
            Reg::X => &mut self.x,
            Reg::Y => &mut self.y,
            Reg::Sp => &mut self.sp,
        }
    }

    fn nz(&mut self, value: u8) -> u8 {
        (self.n, self.z) = (value & 0x80 != 0, value == 0);
        value
    }

    /// The address an instruction works on.
    fn operand(&mut self, mode: Mode) -> u16 {
        let pc = self.pc;
        let zero = |cpu: &Cpu, z: u8| cpu.at(z as u16) as u16 | (cpu.at(z.wrapping_add(1) as u16) as u16) << 8;
        match mode {
            Imm => {
                self.pc = pc.wrapping_add(1);
                pc
            }
            Zp | Zpx | Zpy | Izx | Izy => {
                self.pc = pc.wrapping_add(1);
                let m = self.at(pc);
                match mode {
                    Zp => m as u16,
                    Zpx => m.wrapping_add(self.x) as u16,
                    Zpy => m.wrapping_add(self.y) as u16,
                    Izx => zero(self, m.wrapping_add(self.x)),
                    _ => zero(self, m).wrapping_add(self.y as u16),
                }
            }
            _ => {
                self.pc = pc.wrapping_add(2);
                let addr = self.word(pc);
                match mode {
                    Abx => addr.wrapping_add(self.x as u16),
                    Aby => addr.wrapping_add(self.y as u16),
                    _ => addr,
                }
            }
        }
    }

    /// What an instruction works on.
    fn value(&mut self, mode: Mode) -> u8 {
        let addr = self.operand(mode);
        self.read(addr)
    }

    fn add(&mut self, value: u8) {
        let (a, value, carry) = (self.a as i32, value as i32, self.c as i32);
        let total = a + value + carry;
        if self.d {
            let mut low = (a & 15) + (value & 15) + carry;
            if low > 9 {
                low += 6;
            }
            let mut high = (a >> 4) + (value >> 4) + (low > 15) as i32;
            self.z = total & 0xFF == 0;
            self.n = high >> 3 & 1 != 0;
            self.v = !(a ^ value) & (a ^ high << 4) & 0x80 != 0;
            if high > 9 {
                high += 6;
            }
            self.c = high > 15;
            self.a = (high << 4 | low & 15) as u8;
            return;
        }
        self.v = !(a ^ value) & (a ^ total) & 0x80 != 0;
        self.c = total >> 8 != 0;
        self.a = self.nz(total as u8);
    }

    fn subtract(&mut self, value: u8) {
        let (a, value, borrow) = (self.a as i32, value as i32, 1 - self.c as i32);
        let total = a - value - borrow;
        self.v = (a ^ value) & (a ^ total) & 0x80 != 0;
        if self.d {
            let mut low = (a & 15) - (value & 15) - borrow;
            let mut high = (a >> 4) - (value >> 4);
            if low < 0 {
                low -= 6;
                high -= 1;
            }
            if high < 0 {
                high -= 6;
            }
            self.nz(total as u8);
            self.c = total >= 0;
            self.a = (high << 4 | low & 15) as u8;
            return;
        }
        self.c = total >= 0;
        self.a = self.nz(total as u8);
    }

    fn compare(&mut self, register: u8, value: u8) {
        self.c = register >= value;
        self.nz(register.wrapping_sub(value));
    }

    fn shift(&mut self, what: Op, value: u8) -> u8 {
        let carry = self.c as u8;
        let out = match what {
            Op::Asl | Op::Slo => {
                self.c = value & 0x80 != 0;
                value << 1
            }
            Op::Lsr | Op::Sre => {
                self.c = value & 1 != 0;
                value >> 1
            }
            Op::Rol | Op::Rla => {
                self.c = value & 0x80 != 0;
                value << 1 | carry
            }
            Op::Ror | Op::Rra => {
                self.c = value & 1 != 0;
                value >> 1 | carry << 7
            }
            Op::Inc => value.wrapping_add(1),
            Op::Dec => value.wrapping_sub(1),
            // The two that leave the flags to what follows.
            Op::Isc => return value.wrapping_add(1),
            _ => return value.wrapping_sub(1),
        };
        self.nz(out)
    }

    /// Read, change and write back; what was written.
    fn modify(&mut self, what: Op, mode: Mode) -> u8 {
        if mode == Acc {
            self.a = self.shift(what, self.a);
            return self.a;
        }
        let addr = self.operand(mode);
        let read = self.read(addr);
        let value = self.shift(what, read);
        self.write(addr, value);
        value
    }

    fn run(&mut self, what: Op, mode: Mode) -> Result<(), String> {
        match what {
            Op::Ora => {
                let v = self.a | self.value(mode);
                self.a = self.nz(v);
            }
            Op::And => {
                let v = self.a & self.value(mode);
                self.a = self.nz(v);
            }
            Op::Eor => {
                let v = self.a ^ self.value(mode);
                self.a = self.nz(v);
            }
            Op::Adc => {
                let v = self.value(mode);
                self.add(v);
            }
            Op::Sbc => {
                let v = self.value(mode);
                self.subtract(v);
            }
            Op::Cmp => {
                let v = self.value(mode);
                self.compare(self.a, v);
            }
            Op::Cpx => {
                let v = self.value(mode);
                self.compare(self.x, v);
            }
            Op::Cpy => {
                let v = self.value(mode);
                self.compare(self.y, v);
            }
            Op::Lda => {
                let v = self.value(mode);
                self.a = self.nz(v);
            }
            Op::Ldx => {
                let v = self.value(mode);
                self.x = self.nz(v);
            }
            Op::Ldy => {
                let v = self.value(mode);
                self.y = self.nz(v);
            }
            Op::Sta => {
                let addr = self.operand(mode);
                self.write(addr, self.a);
            }
            Op::Stx => {
                let addr = self.operand(mode);
                self.write(addr, self.x);
            }
            Op::Sty => {
                let addr = self.operand(mode);
                self.write(addr, self.y);
            }
            Op::Bit => {
                let v = self.value(mode);
                (self.n, self.v, self.z) = (v & 0x80 != 0, v & 0x40 != 0, v & self.a == 0);
            }
            Op::Asl | Op::Lsr | Op::Rol | Op::Ror | Op::Inc | Op::Dec => {
                self.modify(what, mode);
            }
            Op::Branch(flag, want) => {
                let offset = self.at(self.pc) as i8;
                self.pc = self.pc.wrapping_add(1);
                if *self.flag(flag) == want {
                    self.pc = self.pc.wrapping_add(offset as u16);
                }
            }
            Op::Jmp => {
                let mut addr = self.word(self.pc);
                if mode == Ind {
                    // The page is not crossed for the high byte: the
                    // 6502's own bug.
                    addr = self.at(addr) as u16 | (self.at(addr & 0xFF00 | addr.wrapping_add(1) & 0xFF) as u16) << 8;
                }
                self.pc = addr;
            }
            Op::Jsr => {
                let back = self.pc.wrapping_add(1);
                self.push((back >> 8) as u8);
                self.push(back as u8);
                self.pc = self.word(self.pc);
            }
            Op::Rts => {
                let low = self.pull() as u16;
                self.pc = (low | (self.pull() as u16) << 8).wrapping_add(1);
            }
            Op::Rti => {
                let p = self.pull();
                self.set_flags(p);
                let low = self.pull() as u16;
                self.pc = low | (self.pull() as u16) << 8;
            }
            Op::Brk => {
                return Err(format!(
                    "a BRK at ${:04X}: the tune ran into memory it did not load (the KERNAL or BASIC, perhaps)",
                    self.pc.wrapping_sub(1)
                ));
            }
            Op::Nop => {
                if mode != Imp {
                    self.operand(mode);
                }
            }
            Op::Slo => {
                let v = self.a | self.modify(what, mode);
                self.a = self.nz(v);
            }
            Op::Rla => {
                let v = self.a & self.modify(what, mode);
                self.a = self.nz(v);
            }
            Op::Sre => {
                let v = self.a ^ self.modify(what, mode);
                self.a = self.nz(v);
            }
            Op::Rra => {
                let v = self.modify(what, mode);
                self.add(v);
            }
            Op::Dcp => {
                let v = self.modify(what, mode);
                self.compare(self.a, v);
            }
            Op::Isc => {
                let v = self.modify(what, mode);
                self.subtract(v);
            }
            Op::Sax => {
                let addr = self.operand(mode);
                self.write(addr, self.a & self.x);
            }
            Op::Lax => {
                let v = self.value(mode);
                self.a = self.nz(v);
                self.x = self.a;
            }
            Op::Anc => {
                let v = self.a & self.value(mode);
                self.a = self.nz(v);
                self.c = self.n;
            }
            Op::Alr => {
                let v = self.a & self.value(mode);
                self.a = self.shift(Op::Lsr, v);
            }
            Op::Arr => {
                let v = (self.a & self.value(mode)) >> 1 | (self.c as u8) << 7;
                self.a = self.nz(v);
                self.c = self.a >> 6 & 1 != 0;
                self.v = (self.a >> 6 ^ self.a >> 5) & 1 != 0;
            }
            Op::Sbx => {
                let value = (self.a & self.x) as i32 - self.value(mode) as i32;
                self.c = value >= 0;
                self.x = self.nz(value as u8);
            }
            Op::Set(flag, value) => *self.flag(flag) = value,
            Op::Move(to, from) => {
                let v = *self.reg(from);
                *self.reg(to) = self.nz(v);
            }
            Op::Txs => self.sp = self.x,
            Op::Pha => self.push(self.a),
            Op::Pla => {
                let v = self.pull();
                self.a = self.nz(v);
            }
            Op::Php => self.push(self.flags()),
            Op::Plp => {
                let p = self.pull();
                self.set_flags(p);
            }
            Op::Step(reg, by) => {
                let v = self.reg(reg).wrapping_add(by);
                *self.reg(reg) = self.nz(v);
            }
        }
        Ok(())
    }

    /// Run the routine at `addr` until it returns.
    fn call(&mut self, addr: u16, a: u8) -> Result<(), String> {
        self.call_within(addr, a, RUNAWAY)
    }

    fn call_within(&mut self, addr: u16, a: u8, runaway: usize) -> Result<(), String> {
        (self.a, self.x, self.y, self.sp) = (a, 0, 0, 0xFF);
        self.push(((HOME - 1) >> 8) as u8);
        self.push((HOME - 1) as u8);
        self.pc = addr;
        for _ in 0..runaway {
            if self.pc == HOME {
                return Ok(());
            }
            let code = self.at(self.pc);
            self.pc = self.pc.wrapping_add(1);
            let Some((what, mode)) = self.ops[code as usize] else {
                return Err(format!("instruction ${code:02X} at ${:04X}, which this 6502 does not have", self.pc.wrapping_sub(1)));
            };
            self.run(what, mode)?;
        }
        Err(format!("the routine at ${addr:04X} does not return"))
    }

    /// The machine's state as a number, for telling when one comes
    /// round again: two chains over the memory and the chip's
    /// registers, 128 bits between them.
    fn state(&self) -> u128 {
        let (mut h1, mut h2) = (0x243F_6A88_85A3_08D3u64, 0x1319_8A2E_0370_7344u64);
        let mut mix = |w: u64| {
            h1 = (h1 ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(31);
            h2 = (h2.rotate_left(27) ^ w).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ (h2 >> 29);
        };
        for chunk in self.mem.as_chunks::<8>().0 {
            mix(u64::from_le_bytes(*chunk));
        }
        for chunk in self.sid.chunks(8) {
            let mut w = [0u8; 8];
            w[..chunk.len()].copy_from_slice(chunk);
            mix(u64::from_le_bytes(w));
        }
        (h1 as u128) << 64 | h2 as u128
    }
}

/// An item of the record: a write (register, value) or a wait.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Item {
    Write(u8, u8),
    Wait(u8),
}

struct SidTokens;

impl Tokens<Item> for SidTokens {
    fn literal(&self, item: &Item) -> Vec<u8> {
        match *item {
            Item::Write(register, value) => vec![register, value],
            Item::Wait(frames) => vec![frames],
        }
    }

    fn reference(&self, count: usize, back: usize) -> Vec<u8> {
        let mut out = if count - MIN < 32 {
            vec![0x40 | (count - MIN) as u8]
        } else {
            let mut out = vec![0x60];
            out.extend(leb128(count));
            out
        };
        out.extend(leb128(back));
        out
    }
}

/// A packed record plain, read as `sid::Tune` reads it, and where the
/// token at `mark` came to be.
fn unpack(tokens: &[u8], mark: usize) -> Option<(Vec<u8>, usize)> {
    let (mut out, mut marked, mut at) = (Vec::new(), 0, 0);
    let mut refs = Refs::new();
    loop {
        refs.leave(&mut at);
        if at == mark && refs.idle() {
            marked = out.len();
        }
        if at >= tokens.len() {
            return Some((out, marked));
        }
        let start = at;
        let first = tokens[at];
        at += 1;
        if (0x40..0x80).contains(&first) {
            let count = match first {
                0x40..0x60 => (first & 0x1F) as u32 + MIN_REF,
                0x60 => refs::leb128(tokens, &mut at)?,
                _ => return None,
            };
            let back = refs::leb128(tokens, &mut at)? as usize;
            if !refs.enter(&mut at, start, count, back) {
                return None;
            }
            continue;
        }
        let size = if first & 0x80 != 0 { 1 } else { 2 };
        out.extend(tokens.get(start..start + size)?);
        at = start + size;
        refs.gave();
    }
}

/// How the tune is played and how the player is to shape it.
#[derive(Clone, Debug)]
pub struct Shape {
    /// Which of the file's songs, from 1; 0 is the one the file names.
    pub song: usize,
    /// Cut the tune here, loop or not; 0 is at its loop, looked for
    /// over 600 s.
    pub seconds: f64,
    /// Keep the voices' gates closed for this many frames from the
    /// start: for a routine that clicks as it starts up.
    pub mute: usize,
    /// The chip, for the filter's curve, if not what the file asks for.
    pub model_8580: Option<bool>,
    /// The machine, for the chip's clock and the calls a second, if not
    /// what the file asks for: a PAL tune on an NTSC C64 is called
    /// 59.83 times a second and not 50.12, two thirds of a semitone up.
    pub ntsc: Option<bool>,
    pub highpass: f64,
    pub passes: u8,
    pub drive: u8,
    pub release: u8,
    /// Steps of the chip a sample; 0 is 2, or 4 at 16 000 samples a
    /// second.
    pub steps: u8,
    pub rate: u32,
    /// No limiter: `drive` is then a plain gain, 127 for the three
    /// voices at their top level as full scale.
    pub flat: bool,
    /// The record unpacked (`SID1`), for a firmware before 2026-11-16.
    pub plain: bool,
}

impl Shape {
    /// For the buzzer as a tracker module's: 300 Hz once and the
    /// player's limiter. For a speaker no limiter, the three voices at
    /// their top as full scale, and the high-pass at 150 Hz twice: the
    /// user's ear on white-clock's speaker, 2026-11-19. The limiter,
    /// even letting go over 128 ms as a module's does, was heard as
    /// clipping (a SID clip's header has no knee to make it gentler
    /// with), and flat with a module's 100 Hz once as "a bit
    /// bass-heavy".
    pub fn new(output: Output) -> Self {
        let (highpass, passes, flat) = match output {
            Output::Buzzer => (300.0, 1, false),
            Output::Speaker => (150.0, 2, true),
        };
        Shape { song: 0, seconds: 0.0, mute: 0, model_8580: None, ntsc: None, highpass, passes, drive: 0, release: 0, steps: 0, rate: FAST, flat, plain: false }
    }

    pub fn options(output: Output, options: &mut Options) -> Result<Self, String> {
        let mut shape = Shape::new(output);
        // Either says which, whatever the output's own is.
        match (options.flag("flat")?, options.flag("limiter")?) {
            (true, true) => return Err("flat or limiter, not both".into()),
            (true, false) => shape.flat = true,
            (false, true) => shape.flat = false,
            (false, false) => {}
        }
        shape.plain = options.flag("plain")?;
        shape.song = options.number("song", 0)?;
        shape.seconds = options.number("seconds", 0.0)?;
        shape.mute = options.number("mute", 0)?;
        shape.model_8580 = match options.text("model")?.as_deref() {
            None => None,
            Some("6581") => Some(false),
            Some("8580") => Some(true),
            Some(other) => return Err(format!("model={other}: 6581 or 8580")),
        };
        shape.ntsc = match options.text("system")?.as_deref() {
            None => None,
            Some("pal") => Some(false),
            Some("ntsc") => Some(true),
            Some(other) => return Err(format!("system={other}: pal or ntsc")),
        };
        shape.highpass = options.number("highpass", shape.highpass)?;
        shape.passes = options.number("passes", shape.passes)?;
        shape.drive = options.number("drive", shape.drive)?;
        shape.release = options.number("release", shape.release)?;
        shape.steps = options.number("steps", 0)?;
        shape.rate = options.number("rate", shape.rate)?;
        Ok(shape)
    }
}

/// The clip: the header's 36 bytes after the magic (its loop offset is
/// set here), and the plain `record`, which starts over from its
/// offset `loop_at`.
fn clip(mut header: Vec<u8>, record: Vec<u8>, mut loop_at: usize, plain: bool) -> Result<Vec<u8>, String> {
    let mut body = record;
    if !plain {
        // The record as its items, and which of them is at `loop_at`.
        let (mut items, mut marked, mut at) = (Vec::new(), 0, 0);
        while at < body.len() {
            if at == loop_at {
                marked = items.len();
            }
            if body[at] & 0x80 != 0 {
                items.push(Item::Wait(body[at]));
                at += 1;
            } else {
                items.push(Item::Write(body[at], body.get(at + 1).copied().unwrap_or(0)));
                at += 2;
            }
        }
        let (packed, landed) = pack(&items, &SidTokens, MIN, marked);
        let landed = landed.unwrap_or(0);
        let plain_mark: usize = items[..marked].iter().map(|i| SidTokens.literal(i).len()).sum();
        if unpack(&packed, landed) != Some((body, plain_mark)) {
            return Err("the packed tune does not unpack to the record: a bug in this converter".into());
        }
        (body, loop_at) = (packed, landed);
    }
    header[20..24].copy_from_slice(&((HEADER + loop_at) as u32).to_le_bytes());
    debug_assert_eq!(header.len(), HEADER - 4);
    let mut out = if plain { MAGIC.to_vec() } else { PACKED_MAGIC.to_vec() };
    out.extend(header);
    out.extend(body);
    Ok(out)
}

/// The clip of the tune in `file`, and what there is to say of it.
pub fn convert(file: &[u8], shape: &Shape, notes: &mut Vec<String>) -> Result<Vec<u8>, String> {
    let d = Data(file);
    if shape.release > 15 {
        return Err("release is 1 to 15".into());
    }
    if shape.passes > 2 {
        return Err("passes is 0, 1 or 2".into());
    }
    if !matches!(shape.rate, 16000 | FAST) {
        return Err(format!("rate={}: 16000 or 31250", shape.rate));
    }
    if !matches!(shape.steps, 0 | 2 | 4) {
        return Err("steps is 2 or 4".into());
    }
    let steps = if shape.steps != 0 { shape.steps } else if shape.rate == FAST { 2 } else { 4 };
    let section = section(shape.highpass, shape.rate)?;

    if file.starts_with(b"RSID") {
        return Err("an RSID, which needs the whole C64 (its interrupts and KERNAL), not just a 6502".into());
    }
    if !file.starts_with(b"PSID") {
        return Err("not a PSID file".into());
    }
    let be = |at: usize| d.u16be(at).map(|v| v as usize);
    let (version, offset, mut load, init, play, songs, start) = (be(4)?, be(6)?, be(8)?, be(10)?, be(12)?, be(14)?, be(16)?);
    let speed = (be(18)? as u32) << 16 | be(20)? as u32;
    let field = |at: usize| text(d.slice(at, at + 32)).trim().to_string();
    let (name, author, released) = (field(0x16), field(0x36), field(0x56));
    let flags = if version >= 2 { be(0x76)? } else { 0 };
    let mut data = d.slice(offset, file.len());
    if load == 0 {
        load = Data(data).u16(0)? as usize;
        data = &data[2..];
    }
    if play == 0 {
        return Err("no play address; the tune hooks the C64's interrupts itself, which this does not run".into());
    }
    if load + data.len() > 0x10000 {
        return Err("loads past the end of memory".into());
    }
    let song = if shape.song != 0 { shape.song } else if start != 0 { start } else { 1 };
    if !(1..=songs).contains(&song) {
        return Err(format!("song {song}: the file has {songs}"));
    }
    let ntsc = shape.ntsc.unwrap_or(flags >> 2 & 3 == 2);
    let (clock, screen, cia) = if ntsc { NTSC } else { PAL };
    let is_8580 = shape.model_8580.unwrap_or(flags >> 4 & 3 == 2);

    let mut cpu = Cpu::new();
    cpu.mem[load..load + data.len()].copy_from_slice(data);
    cpu.mem[1] = 0x37;
    cpu.call(if init != 0 { init } else { load } as u16, (song - 1) as u8)?;
    // A song is called once a screen, or by CIA 1's timer A as the
    // init routine left it (bits past the 32nd go by the 32nd).
    let cycles = if speed >> (song - 1).min(31) & 1 != 0 {
        let timer = cpu.word(0xDC04) as u64;
        (if timer != 0 { timer } else { cia }) + 1
    } else {
        screen
    };
    let seconds = if shape.seconds != 0.0 { shape.seconds } else { 600.0 };
    let limit = (seconds * clock as f64 / cycles as f64).round_ties_even() as usize;
    // A frame's writes, and the state the machine was left in after
    // each: when one comes round again, so will everything after.
    let mut frames: Vec<Vec<(u8, u8)>> = Vec::new();
    let mut seen: HashMap<u128, usize> = HashMap::new();
    let mut looped = None;
    let mut digis = 0;
    while frames.len() < limit {
        cpu.volume_writes = 0;
        cpu.call(play as u16, 0)?;
        frames.push(core::mem::take(&mut cpu.writes));
        digis += (cpu.volume_writes > 4) as usize;
        if shape.seconds == 0.0
            && let Some(before) = seen.insert(cpu.state(), frames.len() - 1)
        {
            looped = Some(before + 1);
            break;
        }
    }
    if digis > 0 {
        *cpu.notes.entry("writes the volume register many times a call (a sample?); the clock keeps the last of a frame's".into()).or_default() += digis;
    }
    let found = looped.is_some();
    let looped = looped.unwrap_or_else(|| {
        // Cut: it starts over from the top, with the chip as it was then.
        frames.push((0..REGS).filter(|&r| cpu.sid[r] != 0).map(|r| (r as u8, 0)).collect());
        0
    });
    if looped >= frames.len() {
        return Err("the tune stops changing at once; nothing to play".into());
    }

    if shape.mute > 0 {
        // The routine runs as it does, but no voice is gated before
        // frame `mute`; there the control registers get what it meant
        // them to have.
        let mute = shape.mute;
        if mute > if looped != 0 { looped } else { frames.len() - 1 } {
            return Err(format!("mute={mute}: the tune goes round to frame {looped}, and has {}", frames.len()));
        }
        const CONTROLS: [u8; 3] = [4, 11, 18];
        let (mut meant, mut written) = ([0u8; REGS], [0u8; REGS]);
        for (n, writes) in frames.iter_mut().enumerate().take(mute + 1) {
            let mut kept = Vec::new();
            if n == mute {
                for r in CONTROLS {
                    if written[r as usize] != meant[r as usize] {
                        kept.push((r, meant[r as usize]));
                        written[r as usize] = meant[r as usize];
                    }
                }
            }
            for &(r, value) in writes.iter() {
                meant[r as usize] = value;
                let value = if n < mute && CONTROLS.contains(&r) { value & 0xFE } else { value };
                if written[r as usize] != value {
                    written[r as usize] = value;
                    kept.push((r, value));
                }
            }
            *writes = kept;
        }
    }

    let mut body: Vec<u8> = Vec::new();
    let mut loop_at = 0;
    // Where the last frame's count of frames is.
    let mut rest: Option<usize> = None;
    for (n, writes) in frames.iter().enumerate() {
        if n == looped {
            (loop_at, rest) = (body.len(), None);
        }
        if let (true, Some(rest)) = (writes.is_empty(), rest)
            && body[rest] < 0xFF
        {
            body[rest] += 1;
            continue;
        }
        for &(register, value) in writes {
            body.extend([register, value]);
        }
        rest = Some(body.len());
        body.push(0x80);
    }
    let ms = ((frames.len() as u64 * cycles * 1000) as f64 / clock as f64).round_ties_even() as u32;
    let mut header = vec![is_8580 as u8 | if shape.flat { 2 } else { 0 }, shape.drive, shape.release, shape.passes];
    for n in [clock as u32, ms, cycles as u32, frames.len() as u32, 0] {
        header.extend(n.to_le_bytes());
    }
    for k in section {
        header.extend(k.to_le_bytes());
    }
    header.extend([steps, (shape.rate == FAST) as u8]);
    let unpacked = HEADER + body.len();
    let out = clip(header, body, loop_at, shape.plain)?;

    notes.push(format!(
        "\"{name}\" by {author} ({released}), song {song} of {songs}, {}, {}, {:.2} frames a second",
        if is_8580 { "8580" } else { "6581" },
        if ntsc { "NTSC" } else { "PAL" },
        clock as f64 / cycles as f64
    ));
    let round = if found { format!("goes round from {:.1} s", (looped as u64 * cycles) as f64 / clock as f64) } else { "cut, starts over from the top".into() };
    notes.push(format!("{:.1} s, {round}; {} bytes, {unpacked} unpacked (the file is {})", ms as f64 / 1000.0, out.len(), file.len()));
    for (what, count) in &cpu.notes {
        notes.push(format!("not played: {what} ({count} times)"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The 6502 against the one of `scripts/sid2clip.py`, which the
    /// clips of 2026 were made with: four thousand programs of random
    /// bytes, run from a random place until they return, stop or have
    /// run 3000 instructions, and everything they left behind hashed.
    /// The digest is the Python's (2026-11-19); seed 2162 runs off the
    /// end of memory, which that one did not survive.
    #[test]
    fn the_6502_is_the_one_the_clips_were_made_with() {
        let mut total = Sha256::new();
        let mut kinds = [0; 4];
        for seed in 0..4000 {
            if seed == 2162 {
                continue;
            }
            let mut cpu = Cpu::new();
            for k in 0..2048 {
                cpu.mem[32 * k..32 * k + 32].copy_from_slice(&Sha256::digest(format!("cpu{seed}.{k}")));
            }
            cpu.set_flags(cpu.mem[7]);
            let (addr, a) = (cpu.word(2), cpu.mem[5]);
            let kind = match cpu.call_within(addr, a, 3000) {
                Ok(()) => 0,
                Err(e) if e.starts_with("a BRK") => 1,
                Err(e) if e.starts_with("instruction") => 2,
                Err(_) => 3,
            };
            kinds[kind as usize] += 1;
            total.update(&cpu.mem);
            total.update([cpu.a, cpu.x, cpu.y, cpu.sp, cpu.pc as u8, (cpu.pc >> 8) as u8, cpu.flags(), cpu.raster, kind]);
            total.update(cpu.sid);
            for (r, v) in &cpu.writes {
                total.update([*r, *v]);
            }
        }
        assert_eq!(kinds, [118, 175, 3673, 33]);
        assert_eq!(hex(&total.finalize()), "624b67ea7f563601be751210556b926bcc4aa4f30e0a41d636ce14833ac156b4");
    }
}

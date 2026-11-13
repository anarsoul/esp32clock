#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Convert a Commodore 64 tune (.sid, PSID) into a clip for the clock.

A .sid file is the C64's own player routine and its data: 6502 code to
be called once a frame, each call writing some of the SID chip's
registers. The clock has a SID (core/src/sid.rs) but no 6502, so this
runs the code here, in a 6502 of its own, and the clip is what reached
the chip: the register writes of each frame, in order, less the ones
that change nothing.

    scripts/sid2clip.py tune.sid tune.sidc [--song N] [--seconds S] [--mute FRAMES]
                        [--model 6581|8580] [--highpass HZ] [--passes N]
                        [--drive N] [--release N] [--steps 2]

A tune goes on for ever, so the memory is hashed after every call: when
a state comes round again the tune has looped, the clip ends there and
says where it starts over. --seconds cuts it sooner, or says how far to
look (default 600); a tune cut with no loop found starts over from its
beginning.

The player cuts the bass, which the buzzer does not have, and limits
hard, as it does for a tracker module: --highpass and --passes set the
cut (300 Hz at 12 dB an octave by default), --drive and --release the
limiter (scripts/xm2clip.py has the same four). --steps 2 steps the
chip 32 000 times a second and not 64 000: half the processor's time on
the clock, and more aliasing in high notes.

Some routines sound a short burst as they start, before the music;
--mute keeps every voice's gate closed for that many frames.

Says what the tune does that the clock will not play.
Then: ALARM_CLIP=tune.sidc ALARM_CLIP_LABEL='…' cargo build --release
"""
import argparse
import hashlib
import math
import struct
import sys
from collections import Counter

MAGIC = b"SID1"  # sid::MAGIC
HEADER = 40
REGISTERS = 25  # sid::REGISTERS
RATE = 16000  # sid::RATE
# Clock in Hz and cycles a screen.
PAL = (985248, 312 * 63)
NTSC = (1022727, 263 * 65)
# What CIA 1's timer A holds when the KERNAL has set it up: 60 Hz.
CIA_DEFAULT = {PAL: 0x4025, NTSC: 0x4295}
# A routine that runs this many instructions is not coming back.
RUNAWAY = 2_000_000
# Where a routine called from here returns to.
HOME = 0xFFF0

IMP, ACC, IMM, ZP, ZPX, ZPY, ABS, ABX, ABY, IZX, IZY, REL, IND = range(13)


class Stop(Exception):
    pass


class Cpu:
    """A 6502 with 64 KB of RAM and a SID's registers at $D400."""

    def __init__(self):
        self.mem = bytearray(0x10000)
        self.a = self.x = self.y = 0
        self.sp = 0xFF
        self.pc = 0
        self.n = self.v = self.d = self.i = self.z = self.c = 0
        # The chip's registers as last written, and the writes of the
        # frame in hand that changed one.
        self.sid = bytearray(REGISTERS)
        self.writes = []
        self.volume_writes = 0
        self.notes = Counter()
        self.raster = 0
        self.ops = self.table()

    # Memory.

    def read(self, addr):
        if 0xD000 <= addr < 0xE000:
            if 0xD400 <= addr < 0xD800:
                register = addr & 0x1F
                if register in (0x1B, 0x1C):
                    self.notes["reads voice 3's %s, which is nought here" % ("oscillator" if register == 0x1B else "envelope")] += 1
                    return 0
            elif addr == 0xD012:
                # The raster line: something that moves, for a routine
                # that waits on it.
                self.raster = (self.raster + 1) & 0xFF
                return self.raster
        return self.mem[addr]

    def write(self, addr, value):
        self.mem[addr] = value
        if 0xD400 <= addr < 0xD800:
            register = addr & 0x1F
            if register < REGISTERS:
                if register == 24:
                    self.volume_writes += 1
                if self.sid[register] != value:
                    self.sid[register] = value
                    self.writes.append((register, value))

    def push(self, value):
        self.mem[0x100 + self.sp] = value
        self.sp = (self.sp - 1) & 0xFF

    def pull(self):
        self.sp = (self.sp + 1) & 0xFF
        return self.mem[0x100 + self.sp]

    def flags(self, brk=1):
        return self.n << 7 | self.v << 6 | 0x20 | brk << 4 | self.d << 3 | self.i << 2 | self.z << 1 | self.c

    def set_flags(self, p):
        self.n, self.v, self.d, self.i, self.z, self.c = p >> 7 & 1, p >> 6 & 1, p >> 3 & 1, p >> 2 & 1, p >> 1 & 1, p & 1

    def nz(self, value):
        self.n, self.z = value >> 7, int(value == 0)
        return value

    def operand(self, mode):
        """The address an instruction works on."""
        m, pc = self.mem, self.pc
        if mode == IMM:
            self.pc = pc + 1
            return pc
        if mode == ZP:
            self.pc = pc + 1
            return m[pc]
        if mode == ZPX:
            self.pc = pc + 1
            return (m[pc] + self.x) & 0xFF
        if mode == ZPY:
            self.pc = pc + 1
            return (m[pc] + self.y) & 0xFF
        if mode == IZX:
            self.pc = pc + 1
            z = (m[pc] + self.x) & 0xFF
            return m[z] | m[(z + 1) & 0xFF] << 8
        if mode == IZY:
            self.pc = pc + 1
            z = m[pc]
            return ((m[z] | m[(z + 1) & 0xFF] << 8) + self.y) & 0xFFFF
        self.pc = pc + 2
        addr = m[pc] | m[pc + 1] << 8
        if mode == ABX:
            return (addr + self.x) & 0xFFFF
        if mode == ABY:
            return (addr + self.y) & 0xFFFF
        return addr

    # Instructions. Each takes its addressing mode.

    def ora(self, mode):
        self.a = self.nz(self.a | self.read(self.operand(mode)))

    def and_(self, mode):
        self.a = self.nz(self.a & self.read(self.operand(mode)))

    def eor(self, mode):
        self.a = self.nz(self.a ^ self.read(self.operand(mode)))

    def add(self, value):
        a = self.a
        total = a + value + self.c
        if self.d:
            low = (a & 15) + (value & 15) + self.c
            if low > 9:
                low += 6
            high = (a >> 4) + (value >> 4) + (low > 15)
            self.z = int(total & 0xFF == 0)
            self.n = high >> 3 & 1
            self.v = int((~(a ^ value) & (a ^ high << 4) & 0x80) != 0)
            if high > 9:
                high += 6
            self.c = int(high > 15)
            self.a = (high << 4 | low & 15) & 0xFF
            return
        self.v = int((~(a ^ value) & (a ^ total) & 0x80) != 0)
        self.c = total >> 8
        self.a = self.nz(total & 0xFF)

    def subtract(self, value):
        a, borrow = self.a, 1 - self.c
        total = a - value - borrow
        self.v = int(((a ^ value) & (a ^ total) & 0x80) != 0)
        if self.d:
            low = (a & 15) - (value & 15) - borrow
            high = (a >> 4) - (value >> 4)
            if low < 0:
                low -= 6
                high -= 1
            if high < 0:
                high -= 6
            self.nz(total & 0xFF)
            self.c = int(total >= 0)
            self.a = (high << 4 | low & 15) & 0xFF
            return
        self.c = int(total >= 0)
        self.a = self.nz(total & 0xFF)

    def adc(self, mode):
        self.add(self.read(self.operand(mode)))

    def sbc(self, mode):
        self.subtract(self.read(self.operand(mode)))

    def compare(self, register, value):
        self.c = int(register >= value)
        self.nz((register - value) & 0xFF)

    def cmp(self, mode):
        self.compare(self.a, self.read(self.operand(mode)))

    def cpx(self, mode):
        self.compare(self.x, self.read(self.operand(mode)))

    def cpy(self, mode):
        self.compare(self.y, self.read(self.operand(mode)))

    def lda(self, mode):
        self.a = self.nz(self.read(self.operand(mode)))

    def ldx(self, mode):
        self.x = self.nz(self.read(self.operand(mode)))

    def ldy(self, mode):
        self.y = self.nz(self.read(self.operand(mode)))

    def sta(self, mode):
        self.write(self.operand(mode), self.a)

    def stx(self, mode):
        self.write(self.operand(mode), self.x)

    def sty(self, mode):
        self.write(self.operand(mode), self.y)

    def bit(self, mode):
        value = self.read(self.operand(mode))
        self.n, self.v, self.z = value >> 7, value >> 6 & 1, int(value & self.a == 0)

    def modify(self, mode, how):
        """Read, change and write back; what was written."""
        if mode == ACC:
            self.a = how(self.a)
            return self.a
        addr = self.operand(mode)
        value = how(self.read(addr))
        self.write(addr, value)
        return value

    def shift_left(self, value):
        self.c = value >> 7
        return self.nz(value << 1 & 0xFF)

    def shift_right(self, value):
        self.c = value & 1
        return self.nz(value >> 1)

    def rotate_left(self, value):
        carry, self.c = self.c, value >> 7
        return self.nz((value << 1 | carry) & 0xFF)

    def rotate_right(self, value):
        carry, self.c = self.c, value & 1
        return self.nz(value >> 1 | carry << 7)

    def asl(self, mode):
        self.modify(mode, self.shift_left)

    def lsr(self, mode):
        self.modify(mode, self.shift_right)

    def rol(self, mode):
        self.modify(mode, self.rotate_left)

    def ror(self, mode):
        self.modify(mode, self.rotate_right)

    def inc(self, mode):
        self.modify(mode, lambda v: self.nz((v + 1) & 0xFF))

    def dec(self, mode):
        self.modify(mode, lambda v: self.nz((v - 1) & 0xFF))

    def branch(self, taken):
        offset = self.mem[self.pc]
        self.pc += 1
        if taken:
            self.pc = (self.pc + offset - (offset & 0x80) * 2) & 0xFFFF

    def jmp(self, mode):
        addr = self.mem[self.pc] | self.mem[self.pc + 1] << 8
        if mode == IND:
            # The page is not crossed for the high byte: the 6502's own bug.
            addr = self.mem[addr] | self.mem[addr & 0xFF00 | (addr + 1) & 0xFF] << 8
        self.pc = addr

    def jsr(self, mode):
        back = self.pc + 1
        self.push(back >> 8)
        self.push(back & 0xFF)
        self.pc = self.mem[self.pc] | self.mem[self.pc + 1] << 8

    def rts(self, mode):
        self.pc = ((self.pull() | self.pull() << 8) + 1) & 0xFFFF

    def rti(self, mode):
        self.set_flags(self.pull())
        self.pc = self.pull() | self.pull() << 8

    def brk(self, mode):
        raise Stop("a BRK at $%04X: the tune ran into memory it did not load (the KERNAL or BASIC, perhaps)" % (self.pc - 1))

    def nop(self, mode):
        if mode != IMP:
            self.operand(mode)

    # The undocumented ones that players use.

    def slo(self, mode):
        self.a = self.nz(self.a | self.modify(mode, self.shift_left))

    def rla(self, mode):
        self.a = self.nz(self.a & self.modify(mode, self.rotate_left))

    def sre(self, mode):
        self.a = self.nz(self.a ^ self.modify(mode, self.shift_right))

    def rra(self, mode):
        self.add(self.modify(mode, self.rotate_right))

    def dcp(self, mode):
        self.compare(self.a, self.modify(mode, lambda v: (v - 1) & 0xFF))

    def isc(self, mode):
        self.subtract(self.modify(mode, lambda v: (v + 1) & 0xFF))

    def sax(self, mode):
        self.write(self.operand(mode), self.a & self.x)

    def lax(self, mode):
        self.a = self.x = self.nz(self.read(self.operand(mode)))

    def anc(self, mode):
        self.a = self.nz(self.a & self.read(self.operand(mode)))
        self.c = self.n

    def alr(self, mode):
        self.a = self.shift_right(self.a & self.read(self.operand(mode)))

    def arr(self, mode):
        self.a = self.nz((self.a & self.read(self.operand(mode))) >> 1 | self.c << 7)
        self.c = self.a >> 6 & 1
        self.v = (self.a >> 6 ^ self.a >> 5) & 1

    def sbx(self, mode):
        value = (self.a & self.x) - self.read(self.operand(mode))
        self.c = int(value >= 0)
        self.x = self.nz(value & 0xFF)

    def table(self):
        ops = {}

        def put(what, codes):
            for code, mode in codes.items():
                ops[code] = (what, mode)

        column = {0x01: IZX, 0x05: ZP, 0x09: IMM, 0x0D: ABS, 0x11: IZY, 0x15: ZPX, 0x19: ABY, 0x1D: ABX}
        for base, what in ((0x00, self.ora), (0x20, self.and_), (0x40, self.eor), (0x60, self.adc), (0x80, self.sta), (0xA0, self.lda), (0xC0, self.cmp), (0xE0, self.sbc)):
            put(what, {base + low: mode for low, mode in column.items() if base + low != 0x89})
        column = {0x06: ZP, 0x0A: ACC, 0x0E: ABS, 0x16: ZPX, 0x1E: ABX}
        for base, what in ((0x00, self.asl), (0x20, self.rol), (0x40, self.lsr), (0x60, self.ror), (0xC0, self.dec), (0xE0, self.inc)):
            put(what, {base + low: mode for low, mode in column.items() if mode != ACC or base < 0x80})
        column = {0x03: IZX, 0x07: ZP, 0x0F: ABS, 0x13: IZY, 0x17: ZPX, 0x1B: ABY, 0x1F: ABX}
        for base, what in ((0x00, self.slo), (0x20, self.rla), (0x40, self.sre), (0x60, self.rra), (0xC0, self.dcp), (0xE0, self.isc)):
            put(what, {base + low: mode for low, mode in column.items()})
        put(self.stx, {0x86: ZP, 0x96: ZPY, 0x8E: ABS})
        put(self.ldx, {0xA2: IMM, 0xA6: ZP, 0xB6: ZPY, 0xAE: ABS, 0xBE: ABY})
        put(self.sty, {0x84: ZP, 0x94: ZPX, 0x8C: ABS})
        put(self.ldy, {0xA0: IMM, 0xA4: ZP, 0xB4: ZPX, 0xAC: ABS, 0xBC: ABX})
        put(self.cpx, {0xE0: IMM, 0xE4: ZP, 0xEC: ABS})
        put(self.cpy, {0xC0: IMM, 0xC4: ZP, 0xCC: ABS})
        put(self.bit, {0x24: ZP, 0x2C: ABS})
        put(self.jmp, {0x4C: ABS, 0x6C: IND})
        put(self.jsr, {0x20: ABS})
        put(self.rts, {0x60: IMP})
        put(self.rti, {0x40: IMP})
        put(self.brk, {0x00: IMP})
        put(self.sax, {0x83: IZX, 0x87: ZP, 0x8F: ABS, 0x97: ZPY})
        put(self.lax, {0xA3: IZX, 0xA7: ZP, 0xAF: ABS, 0xB3: IZY, 0xB7: ZPY, 0xBF: ABY})
        put(self.anc, {0x0B: IMM, 0x2B: IMM})
        put(self.alr, {0x4B: IMM})
        put(self.arr, {0x6B: IMM})
        put(self.sbx, {0xCB: IMM})
        put(self.sbc, {0xEB: IMM})
        put(self.nop, {code: IMP for code in (0xEA, 0x1A, 0x3A, 0x5A, 0x7A, 0xDA, 0xFA)})
        put(self.nop, {code: IMM for code in (0x80, 0x82, 0x89, 0xC2, 0xE2)})
        put(self.nop, {code: ZP for code in (0x04, 0x44, 0x64)})
        put(self.nop, {code: ZPX for code in (0x14, 0x34, 0x54, 0x74, 0xD4, 0xF4)})
        put(self.nop, {0x0C: ABS})
        put(self.nop, {code: ABX for code in (0x1C, 0x3C, 0x5C, 0x7C, 0xDC, 0xFC)})
        for code, flag in ((0x10, "n"), (0x50, "v"), (0x90, "c"), (0xD0, "z")):
            put(lambda mode, flag=flag: self.branch(not getattr(self, flag)), {code: REL})
            put(lambda mode, flag=flag: self.branch(getattr(self, flag)), {code + 0x20: REL})
        for code, flag, value in ((0x18, "c", 0), (0x38, "c", 1), (0x58, "i", 0), (0x78, "i", 1), (0xB8, "v", 0), (0xD8, "d", 0), (0xF8, "d", 1)):
            put(lambda mode, flag=flag, value=value: setattr(self, flag, value), {code: IMP})

        def move(to, source):
            return lambda mode: setattr(self, to, self.nz(getattr(self, source)))

        put(move("x", "a"), {0xAA: IMP})
        put(move("a", "x"), {0x8A: IMP})
        put(move("y", "a"), {0xA8: IMP})
        put(move("a", "y"), {0x98: IMP})
        put(move("x", "sp"), {0xBA: IMP})
        put(lambda mode: setattr(self, "sp", self.x), {0x9A: IMP})
        put(lambda mode: self.push(self.a), {0x48: IMP})
        put(lambda mode: setattr(self, "a", self.nz(self.pull())), {0x68: IMP})
        put(lambda mode: self.push(self.flags()), {0x08: IMP})
        put(lambda mode: self.set_flags(self.pull()), {0x28: IMP})

        def step(register, by):
            return lambda mode: setattr(self, register, self.nz((getattr(self, register) + by) & 0xFF))

        put(step("x", 1), {0xE8: IMP})
        put(step("x", -1), {0xCA: IMP})
        put(step("y", 1), {0xC8: IMP})
        put(step("y", -1), {0x88: IMP})
        return ops

    def call(self, addr, a=0):
        """Run the routine at `addr` until it returns."""
        self.a, self.x, self.y, self.sp = a, 0, 0, 0xFF
        self.push((HOME - 1) >> 8)
        self.push((HOME - 1) & 0xFF)
        self.pc = addr
        ops, mem = self.ops, self.mem
        for _ in range(RUNAWAY):
            if self.pc == HOME:
                return
            code = mem[self.pc]
            self.pc += 1
            try:
                what, mode = ops[code]
            except KeyError:
                raise Stop("instruction $%02X at $%04X, which this 6502 does not have" % (code, self.pc - 1)) from None
            what(mode)
            self.pc &= 0xFFFF
        raise Stop("the routine at $%04X does not return" % addr)


def text(raw):
    return raw.split(b"\0")[0].decode("latin1").strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("sid", help="the .sid file")
    parser.add_argument("out", help="the clip to write")
    parser.add_argument("--song", type=int, default=0, help="which of the file's songs, from 1 (default: the one the file names)")
    parser.add_argument("--seconds", type=float, default=0, help="cut the tune here, loop or not (default: at its loop, looked for over 600 s)")
    parser.add_argument("--mute", type=int, default=0, metavar="FRAMES", help="keep the voices' gates closed for this many frames from the start: for a routine that clicks as it starts up")
    parser.add_argument("--model", choices=("6581", "8580"), help="the chip, for the filter's curve (default: what the file asks for, else 6581)")
    parser.add_argument("--drive", type=int, default=0, help="what the limiter aims a peak at, full scale being 127; over that clips (default: the player's, 127)")
    parser.add_argument("--release", type=int, default=0, help="the limiter lets go by 1/2^N a sample at 8 kHz (default: the player's, 7)")
    parser.add_argument("--highpass", type=float, default=300, help="the high-pass's corner in Hz (default 300)")
    parser.add_argument("--steps", type=int, default=4, choices=(2, 4), help="steps of the chip a sample (default 4; 2 is half the processor's time for more aliasing)")
    parser.add_argument("--passes", type=int, default=1, choices=(0, 1, 2), help="how many times it is applied, 12 dB an octave each (default 1)")
    args = parser.parse_args()
    if not (0 <= args.drive <= 255 and 0 <= args.release <= 15):
        sys.exit("--drive is 1 to 255, --release 1 to 15")
    # The high-pass as a second-order Butterworth section at the
    # output rate, in Q14: b0 b1 b2 a1 a2.
    w = 2 * math.pi * args.highpass / RATE
    alpha = math.sin(w) / math.sqrt(2)
    section = [(1 + math.cos(w)) / 2, -(1 + math.cos(w)), (1 + math.cos(w)) / 2, -2 * math.cos(w), 1 - alpha]
    section = [round(k / (1 + alpha) * 16384) for k in section]

    d = open(args.sid, "rb").read()
    if d[:4] == b"RSID":
        sys.exit(f"{args.sid}: an RSID, which needs the whole C64 (its interrupts and KERNAL), not just a 6502")
    if d[:4] != b"PSID":
        sys.exit(f"{args.sid}: not a PSID file")
    version, offset, load, init, play, songs, start, speed = struct.unpack_from(">HHHHHHHI", d, 4)
    name, author, released = (text(d[at : at + 32]) for at in (0x16, 0x36, 0x56))
    flags = struct.unpack_from(">H", d, 0x76)[0] if version >= 2 else 0
    data = d[offset:]
    if load == 0:
        load, data = struct.unpack_from("<H", data)[0], data[2:]
    if play == 0:
        sys.exit(f"{args.sid}: no play address; the tune hooks the C64's interrupts itself, which this does not run")
    if load + len(data) > 0x10000:
        sys.exit(f"{args.sid}: loads past the end of memory")
    song = args.song or start or 1
    if not 1 <= song <= songs:
        sys.exit(f"song {song}: the file has {songs}")
    system = NTSC if flags >> 2 & 3 == 2 else PAL
    clock, screen = system
    model = args.model or ("8580" if flags >> 4 & 3 == 2 else "6581")

    cpu = Cpu()
    cpu.mem[load : load + len(data)] = data
    cpu.mem[1] = 0x37
    try:
        cpu.call(init or load, song - 1)
        # A song is called once a screen, or by CIA 1's timer A as the
        # init routine left it (bits past the 32nd go by the 32nd).
        if speed >> min(song - 1, 31) & 1:
            timer = cpu.mem[0xDC04] | cpu.mem[0xDC05] << 8
            cycles = (timer or CIA_DEFAULT[system]) + 1
        else:
            cycles = screen
        limit = round((args.seconds or 600) * clock / cycles)
        # A frame's writes, and the state the machine was left in after
        # each: when one comes round again, so will everything after.
        frames, seen, loop = [], {}, None
        digis = 0
        while len(frames) < limit:
            cpu.volume_writes = 0
            cpu.call(play)
            frames.append(cpu.writes)
            cpu.writes = []
            digis += cpu.volume_writes > 4
            state = hashlib.blake2b(bytes(cpu.mem) + bytes(cpu.sid), digest_size=16).digest()
            if state in seen and not args.seconds:
                loop = seen[state] + 1
                break
            seen[state] = len(frames) - 1
    except Stop as e:
        sys.exit(f"{args.sid}: {e}")
    if digis:
        cpu.notes["writes the volume register many times a call (a sample?); the clock keeps the last of a frame's"] += digis
    if loop is None:
        # Cut: it starts over from the top, with the chip as it was then.
        frames.append([(r, 0) for r in range(REGISTERS) if cpu.sid[r]])
        loop = 0
    if loop >= len(frames):
        sys.exit("the tune stops changing at once; nothing to play")

    if args.mute:
        # The routine runs as it does, but no voice is gated before
        # frame `mute`; there the control registers get what it meant
        # them to have.
        if not 0 < args.mute <= (loop or len(frames) - 1):
            sys.exit(f"--mute {args.mute}: the tune goes round to frame {loop}, and has {len(frames)}")
        controls = (4, 11, 18)
        meant, written = bytearray(REGISTERS), bytearray(REGISTERS)
        for n, writes in enumerate(frames[: args.mute + 1]):
            kept = []
            if n == args.mute:
                kept = [(r, meant[r]) for r in controls if written[r] != meant[r]]
                for r, value in kept:
                    written[r] = value
            for r, value in writes:
                meant[r] = value
                if n < args.mute and r in controls:
                    value &= 0xFE
                if written[r] != value:
                    written[r] = value
                    kept.append((r, value))
            frames[n] = kept

    body = bytearray()
    loop_at = HEADER
    rest = None  # where the last frame's count of frames is
    for n, writes in enumerate(frames):
        if n == loop:
            loop_at, rest = HEADER + len(body), None
        if not writes and rest is not None and body[rest] < 0xFF:
            body[rest] += 1
            continue
        for register, value in writes:
            body += bytes([register, value])
        rest = len(body)
        body.append(0x80)
    ms = round(len(frames) * cycles * 1000 / clock)
    out = bytearray(MAGIC)
    out += struct.pack("<BBBBIIIII5hBx", int(model == "8580"), args.drive, args.release, args.passes, clock, ms, cycles, len(frames), loop_at, *section, args.steps)
    assert len(out) == HEADER
    out += body
    open(args.out, "wb").write(out)

    print(f'{args.out}: "{name}" by {author} ({released}), song {song} of {songs}, {model}, {"NTSC" if system == NTSC else "PAL"}, {clock / cycles:.2f} frames a second')
    looped = f"goes round from {loop * cycles / clock:.1f} s" if loop or not args.seconds else "cut, starts over from the top"
    print(f"  {ms / 1000:.1f} s, {looped}; {len(out)} bytes, {len(body) * 1000 // max(ms, 1)} a second (the file is {len(d)})")
    if args.mute:
        print(f"  the gates closed for the first {args.mute} frames")
    if args.passes:
        print(f"  high-pass {args.highpass:g} Hz, {12 * args.passes} dB an octave")
    for what, count in sorted(cpu.notes.items()):
        print(f"  not played: {what} ({count} times)")


if __name__ == "__main__":
    main()

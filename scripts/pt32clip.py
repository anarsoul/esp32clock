#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Convert a ZX Spectrum tune (.pt3, Pro Tracker 3) into a clip for the clock.

The clock plays the module itself (core/src/pt3.rs) through an
AY-3-8910 of its own (core/src/ay.rs), so the clip is the module as it
is behind a header that says what the module cannot: the machine it was
written on, how long it plays, and how the clock is to shape it.

    scripts/pt32clip.py tune.pt3 tune.pt3c [--machine pentagon|spectrum] [--clock HZ]
                        [--chip ay|ym] [--restart] [--noise N] [--noise-lowpass HZ]
                        [--highpass HZ] [--passes N] [--drive N] [--release N]

A module does not say what clocked its chip or how often its player
ran. A Pentagon has 1 750 000 Hz and 48.83 frames a second, a Spectrum
128 1 773 400 Hz and 50.02; the default goes by the module's tone table,
since the ASM and "real" tables were worked out for the first and the
Pro Tracker and Sound Tracker ones for the second. --chip ym plays it
on a YM2149, whose envelope has 32 levels where an AY's has 16.

A module names the position it goes round to when it ends. Where that
is an ending that repeats, --restart makes a ringing alarm start the
tune over from its top instead.

A tune's drums are the chip's noise, and the buzzer makes more of white
noise than of the notes: --noise N mixes every channel that has the
noise in it at N sixteenths of its level (11 by default, 3 dB down,
which the user's ear chose on the clock; 16 is the chip's own),
and --noise-lowpass HZ puts the noise through a low-pass, 6 dB an
octave, which takes its hiss; a tone under the noise keeps its edge.

The player cuts the bass, which the buzzer does not have, and limits
hard, as it does for a tracker module: --highpass and --passes set the
cut (300 Hz at 12 dB an octave by default), --drive and --release the
limiter (scripts/xm2clip.py has the same four).

Then: ALARM_CLIP=tune.pt3c ALARM_CLIP_LABEL='…' cargo build --release
"""
import argparse
import math
import struct
import sys
from collections import Counter

MAGIC = b"PT31"  # pt3::MAGIC
HEADER = 40
RATE = 16000  # pt3::RATE
FINE, AFRESH = 1, 2  # the header's flags
# The chip's clock in Hz and its cycles a frame: the processor's are
# twice both.
MACHINES = {"pentagon": (1_750_000, 71680 // 2), "spectrum": (1_773_400, 70908 // 2)}
TABLES = ["Pro Tracker", "Sound Tracker", "ASM", "real"]
# Where things are in a module (pt3.rs has the same).
VERSION, TURBO, TABLE, DELAY, POSITIONS, LOOP, PATTERNS, SAMPLES, ORNAMENTS, ORDER = 13, 98, 99, 100, 101, 102, 103, 105, 169, 201
# Bytes a pattern's code is followed by, and those an effect takes
# after the row's last code.
EFFECT_BYTES = {1: 3, 2: 5, 3: 1, 4: 1, 5: 2, 8: 3, 9: 1}
EFFECT_NAMES = {1: "tone slide", 2: "portamento", 3: "sample offset", 4: "ornament offset", 5: "on/off vibrato", 8: "envelope slide", 9: "tempo"}
# A tune this long has no end.
RUNAWAY = 3600 * 50


class Module:
    def __init__(self, data):
        self.d = data
        self.uses = Counter()

    def byte(self, at):
        return self.d[at] if at < len(self.d) else 0

    def word(self, at):
        return self.byte(at) | self.byte(at + 1) << 8

    def row(self, at, rows, delay):
        """One row of a channel's pattern from `at`, as the player reads
        it: where the next begins, the rows it lasts, the tempo after."""
        effects = []
        while at < len(self.d):
            code = self.d[at]
            at += 1
            if code == 0xD0 or code == 0xC0 or 0x50 <= code <= 0xAF:
                break
            if code >= 0xF0 or code == 0x10:
                at += 1
            elif 0xB2 <= code <= 0xBF:
                at += 2
                self.uses["the chip's envelope"] += 1
            elif code == 0xB1:
                rows = self.byte(at)
                at += 1
            elif 0x11 <= code <= 0x1F:
                at += 3
                self.uses["the chip's envelope"] += 1
            elif code in EFFECT_BYTES:
                # One named twice counts where it was named last.
                effects = [e for e in effects if e != code] + [code]
        for effect in reversed(effects):
            self.uses[EFFECT_NAMES[effect]] += 1
            if effect == 9:
                delay = self.byte(at)
            at += EFFECT_BYTES[effect]
        return at, rows, delay

    def walk(self):
        """The frames each position begins at, and the frames in all."""
        delay = self.d[DELAY]
        rows, left, frames, starts = [0, 0, 0], [1, 1, 1], 0, []
        for position in range(self.d[POSITIONS]):
            starts.append(frames)
            pattern = self.word(PATTERNS) + 2 * self.byte(ORDER + position)
            at = [self.word(pattern + 2 * c) for c in range(3)]
            while True:
                for c in range(3):
                    left[c] = (left[c] - 1) & 0xFF
                    if left[c]:
                        continue
                    # The first channel's pattern ends in a nought, and
                    # the position with it; its row is the next one's.
                    if c == 0 and self.byte(at[0]) == 0:
                        left[0] = 1
                        break
                    at[c], rows[c], delay = self.row(at[c], rows[c], delay)
                    left[c] = rows[c]
                else:
                    frames += delay or 256
                    if frames > RUNAWAY:
                        sys.exit("a pattern that does not end; not a module the player can play")
                    continue
                break
        return starts, frames


def text(raw):
    return raw.decode("latin1").strip("\0 ")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("module", help="the .pt3 file")
    parser.add_argument("out", help="the clip to write")
    parser.add_argument("--machine", choices=sorted(MACHINES), help="whose clock and frame rate (default: by the module's tone table)")
    parser.add_argument("--clock", type=int, help="the chip's clock in Hz, if not the machine's")
    parser.add_argument("--chip", choices=("ay", "ym"), default="ay", help="an AY-3-8910 (default) or a YM2149, whose envelope is twice as fine")
    parser.add_argument("--restart", action="store_true", help="played round and round, start over from the top and not from the module's loop position")
    parser.add_argument("--noise", type=int, default=11, choices=range(1, 17), metavar="N", help="mix a channel with noise in it at N sixteenths of its level (default 11; 16 is all of it)")
    parser.add_argument("--noise-lowpass", type=float, default=0, metavar="HZ", help="low-pass the noise at this corner (default: none)")
    parser.add_argument("--drive", type=int, default=0, help="what the limiter aims a peak at, full scale being 127 (default: the player's, 127)")
    parser.add_argument("--release", type=int, default=0, help="the limiter lets go by 1/2^N a sample at 8 kHz (default: the player's, 7)")
    parser.add_argument("--highpass", type=float, default=300, help="the high-pass's corner in Hz (default 300)")
    parser.add_argument("--passes", type=int, default=1, choices=(1, 2), help="how many times it is applied, 12 dB an octave each (default 1; the chip's output is all above nought, so there is no doing without)")
    args = parser.parse_args()
    if not (0 <= args.drive <= 255 and 0 <= args.release <= 15):
        sys.exit("--drive is 1 to 255, --release 1 to 15")
    if args.clock and not 500_000 <= args.clock <= 4_000_000:
        sys.exit("--clock is 500000 to 4000000")
    # The high-pass as a second-order Butterworth section at the
    # output rate, in Q14: b0 b1 b2 a1 a2.
    w = 2 * math.pi * args.highpass / RATE
    alpha = math.sin(w) / math.sqrt(2)
    section = [(1 + math.cos(w)) / 2, -(1 + math.cos(w)), (1 + math.cos(w)) / 2, -2 * math.cos(w), 1 - alpha]
    section = [round(k / (1 + alpha) * 16384) for k in section]

    # The noise's low-pass, one pole: the part of the way it moves a sample.
    lowpass = max(1, min(255, round(256 * (1 - math.exp(-2 * math.pi * args.noise_lowpass / RATE))))) if args.noise_lowpass else 0

    d = open(args.module, "rb").read()
    if len(d) <= ORDER or not (d.startswith(b"ProTracker 3.") or d.startswith(b"Vortex Tracker II")):
        sys.exit(f"{args.module}: not a Pro Tracker 3 module")
    digit = chr(d[VERSION])
    if (d[TURBO] != 0x20 and digit in "79") or d.endswith(b"02TS"):
        sys.exit(f"{args.module}: a TurboSound module, for two chips; the clock has one")
    positions, loop = d[POSITIONS], d[LOOP]
    if not positions:
        sys.exit(f"{args.module}: no positions")
    module = Module(d)
    starts, frames = module.walk()
    if not frames:
        sys.exit(f"{args.module}: nothing to play")
    table = d[TABLE] if d[TABLE] < 3 else 3
    machine = args.machine or ("spectrum" if table < 2 else "pentagon")
    clock, cycles = MACHINES[machine]
    clock = args.clock or clock
    loop_frame = starts[loop] if loop < positions and not args.restart else 0
    ms = round(frames * cycles * 1000 / MACHINES[machine][0])
    # The frame is the machine's, in the chip's cycles as its clock is given.
    cycles = round(cycles * clock / MACHINES[machine][0])

    flags = (FINE if args.chip == "ym" else 0) | (AFRESH if args.restart else 0)
    out = bytearray(MAGIC)
    out += struct.pack("<BBBBIIIII5hBB", flags, args.drive, args.release, args.passes, clock, ms, cycles, frames, loop_frame, *section, args.noise % 16, lowpass)
    assert len(out) == HEADER
    out += d
    open(args.out, "wb").write(out)

    version = f"Pro Tracker 3.{digit}" if digit.isdigit() else "Vortex Tracker II"
    print(f'{args.out}: "{text(d[30:62])}" by {text(d[66:98])}, {version}, the {TABLES[table]} tone table, {positions} positions')
    print(f"  {machine}: {'YM2149' if args.chip == 'ym' else 'AY-3-8910'} at {clock} Hz, {clock / cycles:.2f} frames a second")
    if args.restart:
        looped = "starts over from the top"
    else:
        looped = f"goes round from {loop_frame * ms / frames / 1000:.1f} s (position {loop} of {positions}, {(frames - loop_frame) * ms / frames / 1000:.1f} s of it)"
    print(f"  {ms / 1000:.1f} s, {frames} frames, {looped}; {len(out)} bytes")
    print(f"  high-pass {args.highpass:g} Hz, {12 * args.passes} dB an octave" + (f"; noise at {args.noise}/16" if args.noise < 16 else "") + (f"; noise low-passed at {args.noise_lowpass:g} Hz ({lowpass}/256 a sample)" if lowpass else ""))
    if module.uses:
        print("  uses: " + ", ".join(f"{what} ({count})" for what, count in sorted(module.uses.items())))


if __name__ == "__main__":
    main()

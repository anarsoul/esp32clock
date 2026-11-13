#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Convert a FastTracker 2 module (.xm) into a clip for the clock.

The clock plays the module itself (core/src/xm.rs), so the clip is the
module rearranged for a player with no memory to unpack into: tables of
where each pattern and each instrument's sample is, the patterns as
they are, the samples as plain signed bytes (an XM stores differences),
16-bit ones cut to 8, ping-pong loops unrolled. One sample an
instrument, the first.

Says what the module uses that the player does not play.

    scripts/xm2clip.py tune.xm tune.xmc [--drive N] [--release N]
                       [--highpass HZ] [--passes N] [--raise INSTRUMENT:OCTAVES]
                       [--rate 8000]

The player cuts the bass, which the buzzer does not have, and limits
hard; how is the clip's to say. --highpass and --passes set the cut
(300 Hz at 12 dB an octave by default, which is what the user's ear
chose for two chiptunes over the PCM clips' 900 Hz at 24), and --raise
plays an instrument octaves higher, should a bass line need it.
Then: ALARM_CLIP=tune.xmc ALARM_CLIP_LABEL='…' cargo build --release
"""
import argparse
import math
import struct
import sys
from collections import Counter

MAGIC = b"XMC2"  # xm::MAGIC
MAX_CHANNELS = 8  # xm::MAX_CHANNELS
ENVELOPE_POINTS = 12
# What xm.rs plays: effects by number (E's by its first digit too) and
# the volume column by its high nibble.
EFFECTS = {0, 1, 2, 3, 4, 5, 6, 9, 0xA, 0xB, 0xC, 0xD, 0xF, 0x10, 0x14}
E_EFFECTS = {1, 2, 9, 0xA, 0xB, 0xC, 0xD}
VOLUMES = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0xA, 0xB, 0xF}
# Panning does nothing on one buzzer; not worth a warning.
SILENT = {8, 0x19, 0x1D}
SILENT_VOLUMES = {0xC, 0xD, 0xE}


def effect_name(effect, param):
    letter = "%X" % effect if effect < 16 else chr(ord("G") + effect - 16)
    return letter + ("%X" % (param >> 4) if effect == 0xE else "")


def cells(data, channels):
    """The cells of a pattern's packed data, a row at a time."""
    q, row = 0, []
    while q < len(data):
        b = data[q]
        q += 1
        if b & 0x80:
            cell = [0, 0, 0, 0, 0]
            for i in range(5):
                if b & (1 << i):
                    cell[i] = data[q]
                    q += 1
        else:
            cell = [b, *data[q : q + 4]]
            q += 4
        row.append(cell)
        if len(row) == channels:
            yield row
            row = []


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("module", help="the .xm file")
    parser.add_argument("out", help="the clip to write")
    parser.add_argument("--drive", type=int, default=0, help="what the limiter aims a peak at, full scale being 127; over that clips (default: the player's, 160)")
    parser.add_argument("--release", type=int, default=0, help="the limiter lets go by 1/2^N a sample at 8 kHz (default: the player's, 5; 7 is gentler)")
    parser.add_argument("--highpass", type=float, default=300, help="the high-pass's corner in Hz (default 300)")
    parser.add_argument("--passes", type=int, default=1, choices=(0, 1, 2), help="how many times it is applied, 12 dB an octave each (default 1)")
    parser.add_argument("--rate", type=int, default=16000, choices=(8000, 16000), help="samples a second the player makes of it (default 16000, at twice the PCM clips' bit rate on the line; 8000 is theirs)")
    parser.add_argument("--raise", dest="raised", action="append", default=[], metavar="INSTRUMENT:OCTAVES", help="play this instrument that many octaves up; may be repeated")
    args = parser.parse_args()
    if not (0 <= args.drive <= 255 and 0 <= args.release <= 15):
        sys.exit("--drive is 1 to 255, --release 1 to 15")
    raised = {}
    for item in args.raised:
        instrument, _, octaves = item.partition(":")
        raised[int(instrument)] = int(octaves)
    # The high-pass as a second-order Butterworth section at the
    # output rate, in Q14: b0 b1 b2 a1 a2.
    w = 2 * math.pi * args.highpass / args.rate
    alpha = math.sin(w) / math.sqrt(2)
    section = [(1 + math.cos(w)) / 2, -(1 + math.cos(w)), (1 + math.cos(w)) / 2, -2 * math.cos(w), 1 - alpha]
    section = [round(k / (1 + alpha) * 16384) for k in section]
    d = open(args.module, "rb").read()
    if d[:17] != b"Extended Module: ":
        sys.exit(f"{args.module}: not an XM module")
    header, orders_n, restart, channels, patterns_n, instruments_n, flags, speed, tempo = struct.unpack_from("<IHHHHHHHH", d, 60)
    if channels > MAX_CHANNELS:
        sys.exit(f"{channels} channels; the player has {MAX_CHANNELS}")
    if not flags & 1:
        sys.exit("the Amiga frequency table; the player has the linear one only")
    orders = list(d[80 : 80 + orders_n])
    warnings = Counter()

    at = 60 + header
    patterns = []
    for _ in range(patterns_n):
        length, _packing, rows, size = struct.unpack_from("<IBHH", d, at)
        packed = d[at + length : at + length + size]
        # A pattern with no data is all empty rows.
        patterns.append((rows, packed if size else bytes([0x80]) * (rows * channels)))
        at += length + size
    # An order may name a pattern the file does not have: 64 empty rows.
    for n in sorted(set(orders)):
        while n >= len(patterns):
            patterns.append((64, bytes([0x80]) * (64 * channels)))

    for rows, packed in patterns:
        for row in cells(packed, channels):
            for _note, _ins, volume, effect, param in row:
                if (effect or param) and effect not in SILENT:
                    if effect not in EFFECTS or (effect == 0xE and param >> 4 not in E_EFFECTS):
                        warnings["effect " + effect_name(effect, param)] += 1
                if volume >> 4 not in VOLUMES | SILENT_VOLUMES:
                    warnings["volume column %Xx" % (volume >> 4)] += 1

    instruments = []
    for number in range(1, instruments_n + 1):
        size = struct.unpack_from("<I", d, at)[0]
        samples = struct.unpack_from("<H", d, at + 27)[0]
        entry = dict(sample=b"", loop=(0, 0), volume=0, finetune=0, relative=0, flags=0, points=[], sustain=0, env_loop=(0, 0), fade=0, auto=(0, 0, 0))
        if samples:
            sample_header = struct.unpack_from("<I", d, at + 29)[0]
            envelope = struct.unpack_from("<24H", d, at + 129)
            points = min(d[at + 225], ENVELOPE_POINTS)
            entry.update(
                points=[(envelope[2 * i], envelope[2 * i + 1]) for i in range(points)],
                sustain=d[at + 227],
                env_loop=(d[at + 228], d[at + 229]),
                flags=d[at + 233] & 7 if points else 0,
                fade=struct.unpack_from("<H", d, at + 239)[0],
            )
            # Auto-vibrato: type, sweep, depth, rate. The player's is a sine.
            entry["auto"] = (d[at + 236], d[at + 237], d[at + 238])
            if d[at + 237] and d[at + 235]:
                warnings[f"instrument {number}: auto-vibrato waveform {d[at + 235]}, played as a sine"] += 1
            if samples > 1:
                warnings[f"instrument {number}: {samples} samples, the first is used"] += 1
            head = at + size
            body = head + sample_header * samples
            length, loop_start, loop_len, volume, finetune, kind, _pan, relative = struct.unpack_from("<IIIBbBBb", d, head)
            raw = d[body : body + length]
            if kind & 0x10:
                values, acc = [], 0
                for (delta,) in struct.iter_unpack("<h", raw):
                    acc = (acc + delta) & 0xFFFF
                    values.append(((acc ^ 0x8000) - 0x8000) >> 8)
                loop_start, loop_len = loop_start // 2, loop_len // 2
            else:
                values, acc = [], 0
                for delta in raw:
                    acc = (acc + delta) & 0xFF
                    values.append((acc ^ 0x80) - 0x80)
            loop = kind & 3
            if not loop or loop_len == 0:
                loop_start = loop_len = 0
            elif loop == 2:
                # Ping-pong: there and back is one forward loop.
                there = values[loop_start : loop_start + loop_len]
                values = values[:loop_start] + there + there[::-1]
                loop_len *= 2
            else:
                values = values[: loop_start + loop_len]
            relative = max(-128, min(127, relative + 12 * raised.get(number, 0)))
            entry.update(sample=bytes(v & 0xFF for v in values), loop=(loop_start, loop_len), volume=volume, finetune=finetune, relative=relative)
            at = body + sum(struct.unpack_from("<I", d, head + sample_header * i)[0] for i in range(samples))
        else:
            at += size
        instruments.append(entry)

    # The length, played once as the player does: to the end of the
    # order list or to a jump back.
    ticks_ms, order, row, now_speed, now_tempo = 0.0, 0, 0, speed, tempo
    while order < len(orders):
        rows, packed = patterns[orders[order]]
        go = None
        for r, cells_row in enumerate(cells(packed, channels)):
            if r < row:
                continue
            for _note, _ins, _volume, effect, param in cells_row:
                if effect == 0xF and param:
                    if param >= 0x20:
                        now_tempo = param
                    else:
                        now_speed = param
                elif effect == 0xB:
                    go = (param, 0)
                elif effect == 0xD:
                    go = (go[0] if go else order + 1, (param >> 4) * 10 + (param & 0xF))
            ticks_ms += now_speed * 2500 / now_tempo
            if go:
                break
        if go and go[0] <= order:
            break
        order, row = go if go else (order + 1, 0)

    out = bytearray(MAGIC)
    out += bytes([channels, speed, tempo, len(orders), len(patterns), len(instruments), args.drive, args.release])
    out += struct.pack("<I", round(ticks_ms))
    # The order to start over from, for the alarm, which plays the tune
    # round and round.
    out += struct.pack("<BB5hB3x", args.passes, args.rate // 16000, *section, restart if restart < len(orders) else 0)
    out += bytes(orders)
    tables = len(out) + 6 * len(patterns) + 80 * len(instruments)
    body = bytearray()
    for rows, packed in patterns:
        out += struct.pack("<HI", rows, tables + len(body))
        body += packed
    for i in instruments:
        out += struct.pack("<IIII", tables + len(body), len(i["sample"]), *i["loop"])
        out += struct.pack("<BbbBBBBBH3B3x", i["volume"], i["finetune"], i["relative"], i["flags"], len(i["points"]), i["sustain"], *i["env_loop"], i["fade"], *i["auto"])
        for p in range(ENVELOPE_POINTS):
            out += struct.pack("<HH", *(i["points"][p] if p < len(i["points"]) else (0, 0)))
        body += i["sample"]
    assert len(out) == tables
    out += body
    open(args.out, "wb").write(out)

    name = d[17:37].decode("latin1").strip("\0 ")
    print(f'{args.out}: "{name}", {channels} channels, {len(orders)} orders, {ticks_ms / 1000:.1f} s, {len(out)} bytes (the module is {len(d)})')
    if args.passes:
        print(f"  {args.rate} Hz; high-pass {args.highpass:g} Hz, {12 * args.passes} dB an octave" + "".join(f"; instrument {i} up {o} octaves" for i, o in sorted(raised.items())))
    for what, count in sorted(warnings.items()):
        print(f"  not played: {what} ({count} times)")


if __name__ == "__main__":
    main()

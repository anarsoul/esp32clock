#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Take a tune out of "Wham! The Music Box" (ZX Spectrum) for the clock.

The program keeps a tune as two channels of note bytes, 1000 each, a
tempo byte before the first, and 0x40 after the last note of each. Its
tape carries a store of such tunes, 2000 bytes apiece from address
30000, and their ten-letter names at 65300. This writes one of them as
the firmware's two-channel tune (core/src/clip.rs, `Wham`): the notes
as the player's table indexes, nothing rendered.

    scripts/wham2clip.py WHAM.TAP                 list the tunes on the tape
    scripts/wham2clip.py WHAM.TAP 2 out.whm       tune 2
    scripts/wham2clip.py tune.bin 1 out.whm       a tune saved from the program (2000 bytes)
Then: ALARM_CLIP=out.whm ALARM_CLIP_LABEL='…' cargo build --release
"""
import argparse
import struct
import sys

MAGIC = b"WHM1"  # clip::WHAM_MAGIC
SLOT = 2000  # a tune in the store: two channels of 1000
STORE, NAMES = 30000, 65300  # where the tape's blocks load
END = 0x40  # after a channel's last note
DEFAULT_TEMPO = 239  # the program's, kept when a tune's first byte is not above 220


def tap_blocks(data):
    """{load address: bytes} for the CODE blocks of a TAP file."""
    blocks, i, header = {}, 0, None
    while i + 2 <= len(data):
        (n,) = struct.unpack_from("<H", data, i)
        block = data[i + 2 : i + 2 + n]
        i += 2 + n
        if len(block) == 19 and block[0] == 0 and block[1] == 3:
            header = struct.unpack_from("<H", block, 14)[0]
        elif header is not None and block and block[0] == 0xFF:
            blocks[header] = block[1:-1]
            header = None
    return blocks


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("source", help="the program's TAP file, or a tune saved from it")
    parser.add_argument("tune", type=int, nargs="?", help="which tune of the store, from 1")
    parser.add_argument("out", nargs="?", help="the file to write")
    parser.add_argument("--octave", type=int, default=0, choices=range(4), help="octaves to raise it by (default 0)")
    parser.add_argument("--tempo", type=int, help="override the tempo, 221 (slow) to 255")
    args = parser.parse_args()

    data = open(args.source, "rb").read()
    if len(data) <= SLOT + 2:
        store, names = data[-SLOT:], b""
    else:
        blocks = tap_blocks(data)
        if STORE not in blocks:
            sys.exit(f"{args.source}: no block that loads at {STORE}")
        store, names = blocks[STORE], blocks.get(NAMES, b"")

    def tune(k):
        slot = store[SLOT * (k - 1) : SLOT * k]
        channels = []
        for channel in (slot[1:1000], slot[1001:2000]):
            end = channel.find(END)
            # The player adds 12 to a note before it looks it up.
            channels.append(bytes((b + 12) & 0xFF for b in channel[: end if end >= 0 else 0]))
        steps = min(len(c) for c in channels)
        tempo = slot[0] if slot and slot[0] > 220 else DEFAULT_TEMPO
        return tempo, channels[0][:steps], channels[1][:steps]

    def name(k):
        return names[10 * (k - 1) : 10 * k].decode("ascii", "replace").strip()

    if args.tune is None or args.out is None:
        for k in range(1, len(store) // SLOT + 1):
            tempo, c1, c2 = tune(k)
            if c1:
                seconds = len(c1) * ((256 - tempo) * 256 * 96 + 1164) / 3.5e6
                print(f"{k}: {name(k) or '(no name)':10}  {len(c1)} steps, tempo {tempo}, {seconds:.0f} s")
        return

    tempo, c1, c2 = tune(args.tune)
    if not c1:
        sys.exit(f"tune {args.tune} is empty")
    if args.tempo is not None:
        tempo = args.tempo
    with open(args.out, "wb") as f:
        f.write(MAGIC + bytes([tempo, args.octave]) + struct.pack("<H", len(c1)) + c1 + c2)
    print(f"{args.out}: {name(args.tune)}, {len(c1)} steps, tempo {tempo}, octave +{args.octave}, {8 + 2 * len(c1)} bytes")


if __name__ == "__main__":
    main()

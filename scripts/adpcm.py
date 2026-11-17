#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""A PCM clip as IMA ADPCM, four bits a sample: half the flash.

    scripts/adpcm.py wav/tune.pcm wav/tune.adp            a PCM clip (scripts/wav2pcm.sh) packed
    scripts/adpcm.py --s16 --rate 16000 in.raw tune.adp   raw signed 16-bit samples, little-endian, mono
    scripts/adpcm.py --decode tune.adp out.raw            the clip's samples, raw signed 16-bit

The clip is `ADP1`, the rate, then the codes, two a byte with the high
nibble first (core/src/adpcm.rs, whose decoder `decode` here is), which
is sox's raw `ima`:

    tail -c +9 tune.adp | play -t ima -r 16000 -c 1 -

`scripts/wav2pcm.sh in.wav out.adp` makes one from a sound file, from
sox's samples at 16 bits and not through a PCM clip's 8.

The encoder keeps the `--search` best paths through the coder's states
(4 by default; 1 is the plain encoder, a sample's nearest code), which
is a decibel less error for the same decoder and takes a minute or so
for a long clip.
"""
import argparse
import math
import struct
import sys
from pathlib import Path

MAGIC = b"ADP1"
PCM_MAGIC = b"PCM1"
HEADER = 8
RATES = (8000, 16000, 31250)
# adpcm::STEPS and INDEX.
STEPS = [7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173,
         190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
         2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818,
         18500, 20350, 22385, 24623, 27086, 29794, 32767]
INDEX = [-1, -1, -1, -1, 2, 4, 6, 8]


def fail(message):
    sys.exit(f"adpcm: {message}")


def step(code, predicted, index):
    """The decoder's step: the sample a code gives and the next index."""
    size = STEPS[index]
    diff = size >> 3
    if code & 1:
        diff += size >> 2
    if code & 2:
        diff += size >> 1
    if code & 4:
        diff += size
    predicted = max(-32768, min(32767, predicted - diff if code & 8 else predicted + diff))
    return predicted, max(0, min(88, index + INDEX[code & 7]))


def decode(codes):
    predicted = index = 0
    out = []
    for code in codes:
        predicted, index = step(code, predicted, index)
        out.append(predicted)
    return out


def nearest(sample, predicted, index):
    """The code the plain encoder gives a sample."""
    size = STEPS[index]
    diff = sample - predicted
    code = 8 if diff < 0 else 0
    diff = abs(diff)
    for bit, part in ((4, size), (2, size >> 1), (1, size >> 2)):
        if diff >= part:
            code |= bit
            diff -= part
    return code


def encode(samples, search):
    """The codes for `samples`: of the paths through the coder's states,
    the `search` with the least squared error so far are kept at each
    sample, and the best at the end is the clip."""
    if search <= 1:
        predicted = index = 0
        codes = []
        for sample in samples:
            code = nearest(sample, predicted, index)
            predicted, index = step(code, predicted, index)
            codes.append(code)
        return codes
    paths = [(0, 0, 0, None)]  # error, predicted, index, (code, the path before)
    for sample in samples:
        reached = {}
        for error, predicted, index, trail in paths:
            for code in range(16):
                state = step(code, predicted, index)
                total = error + (sample - state[0]) ** 2
                if state not in reached or total < reached[state][0]:
                    reached[state] = (total, *state, (code, trail))
        paths = sorted(reached.values(), key=lambda path: path[0])[:search]
    codes = []
    trail = paths[0][3]
    while trail:
        codes.append(trail[0])
        trail = trail[1]
    return codes[::-1]


def read_clip(data, path):
    if data[:4] != MAGIC or len(data) < HEADER:
        fail(f"{path}: not an ADPCM clip")
    rate = struct.unpack_from("<I", data, 4)[0]
    return rate, [nibble for byte in data[HEADER:] for nibble in (byte >> 4, byte & 15)]


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--s16", action="store_true", help="the input is raw signed 16-bit samples, not a PCM clip")
    parser.add_argument("--rate", type=int, help="samples a second of --s16 input")
    parser.add_argument("--search", type=int, default=4, help="paths the encoder keeps (default 4, 1 for the plain encoder)")
    parser.add_argument("--decode", action="store_true", help="an ADPCM clip back to raw signed 16-bit samples")
    args = parser.parse_args()
    try:
        data = args.input.read_bytes()
    except OSError as e:
        fail(f"{args.input}: {e.strerror}")

    if args.decode:
        rate, codes = read_clip(data, args.input)
        samples = decode(codes)
        args.output.write_bytes(struct.pack(f"<{len(samples)}h", *samples))
        print(f"{args.output}: {len(samples)} samples at {rate} Hz, signed 16-bit")
        return

    if args.s16:
        if args.rate is None:
            fail("--s16 needs --rate")
        rate = args.rate
        samples = list(struct.unpack(f"<{len(data) // 2}h", data[: len(data) & ~1]))
    else:
        if data[:4] == MAGIC:
            fail(f"{args.input}: an ADPCM clip already")
        if data[:3] in (b"BPR", b"XMC", b"SID", b"PT3"):
            fail(f"{args.input}: not a PCM clip")
        if data[:4] == PCM_MAGIC and len(data) >= HEADER:
            rate, data = struct.unpack_from("<I", data, 4)[0], data[HEADER:]
        else:
            rate = 8000
        if args.rate not in (None, rate):
            fail(f"{args.input}: the clip is at {rate} Hz")
        samples = [(byte - 128) << 8 for byte in data]
    if rate not in RATES:
        fail(f"the rate must be one of {', '.join(map(str, RATES))}")
    if not samples:
        fail(f"{args.input}: no samples")
    # Whole bytes: an odd clip ends on its last sample again.
    if len(samples) % 2:
        samples.append(samples[-1])

    codes = encode(samples, args.search)
    body = bytes(codes[k] << 4 | codes[k + 1] for k in range(0, len(codes), 2))
    args.output.write_bytes(MAGIC + struct.pack("<I", rate) + body)
    signal = sum(s * s for s in samples)
    error = sum((a - b) ** 2 for a, b in zip(samples, decode(codes)))
    snr = f"{10 * math.log10(signal / error):.1f} dB" if signal and error else "exact"
    print(f"{args.output}: {HEADER + len(body)} bytes, {len(samples) / rate:.1f} s at {rate} Hz, the error {snr} under the signal")


if __name__ == "__main__":
    main()

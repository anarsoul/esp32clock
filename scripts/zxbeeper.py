#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Record what a ZX Spectrum program plays on the beeper as a clip.

The tape is loaded and run in SkoolKit's simulator, every flip of the
speaker bit of port 0xFE is taken with its time, and the result is
written as the firmware's beeper clip: the line's level as run lengths
(core/src/clip.rs, `Beeper`). No sound is synthesised or filtered; the
clock's buzzer is driven with the pulses the Spectrum's speaker was.

The runs are written packed (`BPR3`): where a stretch of them has
been played before, a reference to it and not the runs again, which
is about half the bytes for a tune and nothing lost. --plain writes
them one by one (`BPR2`) as a firmware before 2026-11-16 wants them.
A beeper clip in the tape's place is packed as it is, with no
SkoolKit needed:
    scripts/zxbeeper.py old.bpr new.bpr

Beeper music is narrow pulses, a few percent of the time on, which is
quiet on the clock's buzzer; --stretch widens every pulse by a factor,
never into the next one.

Some tapes do not run by themselves: the loader unpacks the program and
waits for a key before it starts it (`PAUSE 0: RANDOMIZE USR n`). For
those, --settle gives the unpacking its time and --start is the address
to start at then, the `n` of the loader:
    … scripts/zxbeeper.py CHRON.TAP out.bpr --settle 20 --start 64000 --seconds 185

Needs SkoolKit, so run it with a Python that has it, for instance
    pipx install skoolkit
    ~/.local/pipx/venvs/skoolkit/bin/python scripts/zxbeeper.py game.tap out.bpr --seconds 60
Then: ALARM_CLIP=out.bpr ALARM_CLIP_LABEL='…' cargo build --release
"""
import argparse
import os
import struct
import tempfile

from packrefs import Refs, leb128, pack, read_leb128

CPU_HZ = 3_500_000  # the 48K Spectrum's clock; SkoolKit counts in its T-states
TICK_HZ = 512_000  # clip::BEEPER_TICK_HZ
PLAIN = b"BPR2"  # clip::BEEPER_MAGIC
PACKED = b"BPR3"  # clip::BEEPER_PACKED_MAGIC
MIN_REF = 2  # clip::BEEPER_MIN_REF
SHORT_REFS = 32  # counts a reference's first byte has room for


def flips(tape, tstates, settle=0, start=None):
    """T-states of every flip of the speaker bit, from the end of the load,
    or from a jump to `start` made `settle` T-states after it."""
    from skoolkit import tap2sna, trace

    kept = []

    class Spy(trace.Tracer):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            kept.append(self)

    trace.Tracer = Spy
    with tempfile.TemporaryDirectory() as tmp:
        snapshot = os.path.join(tmp, "loaded.z80")
        tap2sna.main([tape, snapshot])
        begin = []
        if start is not None:
            settled = os.path.join(tmp, "settled.z80")
            trace.main(["-M", str(settle), snapshot, settled])
            snapshot, begin = settled, ["-s", str(start)]
            kept.clear()
        # A WAV to write is what makes the tracer log the port.
        trace.main(begin + ["-M", str(tstates), snapshot, os.path.join(tmp, "out.wav")])
    return [t for t, register, _ in kept[-1].audio_log if register == 0xFF]


def run_token(ticks):
    if ticks < 128:
        return bytes([ticks])
    if ticks < 128 + (1 << 14):
        return bytes([0x80 | (ticks - 128) >> 8, (ticks - 128) & 0xFF])
    return b"\xe0" + leb128(ticks)


def reference(count, back):
    if count - MIN_REF < SHORT_REFS:
        return bytes([0xC0 | (count - MIN_REF)]) + leb128(back)
    return b"\xf0" + leb128(count) + leb128(back)


def unpack(tokens):
    """The runs of a packed clip, read as `clip::Beeper` reads them."""
    runs = []
    at = 0
    refs = Refs()
    while True:
        at = refs.leave(at)
        if at == len(tokens):
            return runs
        start = at
        first = tokens[at]
        at += 1
        if first < 0x80:
            ticks = first
        elif first < 0xC0:
            ticks = ((first & 0x3F) << 8 | tokens[at]) + 128
            at += 1
        elif first == 0xE0:
            ticks, at = read_leb128(tokens, at)
        elif first < 0xE0 or first == 0xF0:
            if first == 0xF0:
                count, at = read_leb128(tokens, at)
            else:
                count = (first & 0x1F) + MIN_REF
            back, at = read_leb128(tokens, at)
            at = refs.enter(at, start, count, back)
            continue
        else:
            raise SystemExit("packed clip: an unknown token")
        runs.append(ticks)
        refs.gave()


def read_clip(data):
    """The runs of a beeper clip of either format."""
    if data[:4] == PACKED:
        return unpack(data[8:])
    if data[:4] != PLAIN:
        raise SystemExit("a beeper clip of a format this script does not read")
    runs = []
    at = 8
    while at < len(data):
        run, at = read_leb128(data, at)
        runs.append(run)
    return runs


def write_clip(path, runs, plain):
    if plain:
        body = b"".join(leb128(run) for run in runs)
    else:
        body, _ = pack(runs, run_token, reference, MIN_REF)
        if unpack(body) != runs:
            raise SystemExit("the packed clip does not unpack to the runs: a bug in this script")
    with open(path, "wb") as f:
        f.write((PLAIN if plain else PACKED) + struct.pack("<I", sum(runs)) + body)
    return len(body) + 8


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("tape", help="TAP, TZX or PZX file, or a zip of one; or a beeper clip to pack again")
    parser.add_argument("out", help="the clip to write")
    parser.add_argument("--seconds", type=float, default=60, help="how much to keep, from the first sound (default 60)")
    parser.add_argument("--skip", type=float, default=0, help="seconds of sound to drop first")
    parser.add_argument("--stretch", type=float, default=1, help="widen each pulse by this factor (default 1, as played)")
    parser.add_argument("--boot", type=float, default=60, help="seconds to allow before the first sound (default 60)")
    parser.add_argument("--settle", type=float, default=20, help="with --start: seconds to run the loaded tape first (default 20)")
    parser.add_argument("--start", type=int, help="jump to this address after --settle, for a loader that waits for a key")
    parser.add_argument("--plain", action="store_true", help="write the runs unpacked (BPR2), for a firmware before 2026-11-16")
    args = parser.parse_args()

    with open(args.tape, "rb") as f:
        head = f.read()
    if head[:3] == b"BPR":
        runs = read_clip(head)
        size = write_clip(args.out, runs, args.plain)
        print(f"{args.out}: {len(runs)} runs, {sum(runs) / TICK_HZ:.1f} s, {len(head)} bytes in, {size} bytes")
        return

    limit = int((args.boot + args.skip + args.seconds) * CPU_HZ)
    times = flips(args.tape, limit, int(args.settle * CPU_HZ), args.start)
    if len(times) < 2:
        raise SystemExit("no sound on the beeper")
    # The speaker is off before the first flip, so flips come in pairs: on, off.
    start = times[0] + int(args.skip * CPU_HZ)
    end = start + int(args.seconds * CPU_HZ)
    pulses = [(on, off) for on, off in zip(times[0::2], times[1::2]) if on >= start and off <= end]
    if not pulses:
        raise SystemExit("no sound in that window")
    start = pulses[0][0]

    tick = lambda t: (t - start) * TICK_HZ // CPU_HZ
    runs = []
    at = 0  # ticks written so far
    on_ticks = 0
    for i, (on, off) in enumerate(pulses):
        width = (off - on) * args.stretch
        if i + 1 < len(pulses):
            # Leave a tick of gap before the next pulse.
            width = min(width, pulses[i + 1][0] - on - CPU_HZ / TICK_HZ)
        a = max(tick(on), at)
        b = max(tick(on + int(width)), a + 1)
        runs += [a - at, b - a]
        on_ticks += b - a
        at = b

    size = write_clip(args.out, runs, args.plain)
    print(f"{args.out}: {len(pulses)} pulses, {at / TICK_HZ:.1f} s, on {100 * on_ticks / at:.1f} % of the time, {size} bytes")


if __name__ == "__main__":
    main()

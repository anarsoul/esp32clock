#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Record what a ZX Spectrum program plays on the beeper as a clip.

The tape is loaded and run in SkoolKit's simulator, every flip of the
speaker bit of port 0xFE is taken with its time, and the result is
written as the firmware's beeper clip: the line's level as run lengths
(core/src/clip.rs, `Beeper`). No sound is synthesised or filtered; the
clock's buzzer is driven with the pulses the Spectrum's speaker was.

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

from skoolkit import tap2sna, trace

CPU_HZ = 3_500_000  # the 48K Spectrum's clock; SkoolKit counts in its T-states
TICK_HZ = 512_000  # clip::BEEPER_TICK_HZ
MAGIC = b"BPR2"  # clip::BEEPER_MAGIC


def flips(tape, tstates, settle=0, start=None):
    """T-states of every flip of the speaker bit, from the end of the load,
    or from a jump to `start` made `settle` T-states after it."""
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


def leb128(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        out.append(byte | (0x80 if value else 0))
        if not value:
            return bytes(out)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("tape", help="TAP, TZX or PZX file, or a zip of one")
    parser.add_argument("out", help="the clip to write")
    parser.add_argument("--seconds", type=float, default=60, help="how much to keep, from the first sound (default 60)")
    parser.add_argument("--skip", type=float, default=0, help="seconds of sound to drop first")
    parser.add_argument("--stretch", type=float, default=1, help="widen each pulse by this factor (default 1, as played)")
    parser.add_argument("--boot", type=float, default=60, help="seconds to allow before the first sound (default 60)")
    parser.add_argument("--settle", type=float, default=20, help="with --start: seconds to run the loaded tape first (default 20)")
    parser.add_argument("--start", type=int, help="jump to this address after --settle, for a loader that waits for a key")
    args = parser.parse_args()

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

    body = b"".join(leb128(run) for run in runs)
    with open(args.out, "wb") as f:
        f.write(MAGIC + struct.pack("<I", at) + body)
    print(
        f"{args.out}: {len(pulses)} pulses, {at / TICK_HZ:.1f} s, on {100 * on_ticks / at:.1f} % of the time, "
        f"{len(body) + 8} bytes"
    )


if __name__ == "__main__":
    main()

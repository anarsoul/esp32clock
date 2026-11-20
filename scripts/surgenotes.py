#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Single notes of a Surge XT patch as WAV files, for `scripts/mkxm.py`
to cut samples from.

    scripts/surgenotes.py patch.fxp xm/surge/acoustic-grand F3:56 C4:78 …

Each `note:velocity` is struck with the sustain pedal down, let go
after `--held` seconds and left to ring for `--seconds`, by itself, and
written as `<note>-<velocity>.wav` (44 100 Hz, stereo, 16 bits) in the
directory named.

Needs Surge XT's VST3 plugin (`--plugin`, /usr/lib/vst3/Surge XT.vst3)
and `pedalboard` (`pip install pedalboard`, in a virtual environment),
which hosts it with no window and no sound card.
"""

import argparse
import os
import re
import struct
import sys

import numpy as np
from scipy.io import wavfile

RATE = 44100
# JUCE's own base 64, in which its VST3 wrapper keeps a plugin's state:
# the length, a dot, then six bits a character from the low end.
DIGITS = ".ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+"
STEPS = {"C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11}


def juce64(data):
    value = int.from_bytes(data, "little")
    return f"{len(data)}." + "".join(DIGITS[value >> 6 * i & 63] for i in range((len(data) * 8 + 5) // 6))


def with_patch(state, patch):
    """The host's state of the plugin with the patch of an `.fxp` file
    in it. The file is a header of 60 bytes and Surge's own chunk
    (`sub3`), which is what the plugin gives the host as its state."""
    assert patch[:4] == b"CcnK" and patch[60:64] == b"sub3", "not a Surge XT patch"
    assert state[:4] == b"VC2!", "not the state of a VST3 plugin as JUCE keeps it"
    size = struct.unpack("<I", state[4:8])[0]
    xml = state[8:8 + size].decode()
    found = re.search(r"<IComponent>(.*?)</IComponent>", xml, re.S)
    assert found, "no component state"
    xml = (xml[:found.start(1)] + juce64(patch[60:]) + xml[found.end(1):]).encode()
    return b"VC2!" + struct.pack("<I", len(xml)) + xml + state[8 + size:]


def key_of(name):
    found = re.fullmatch(r"([A-G])(#?)(-?\d)", name)
    assert found, name
    return STEPS[found[1]] + (1 if found[2] else 0) + 12 * (int(found[3]) + 1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("patch")
    ap.add_argument("out")
    ap.add_argument("notes", nargs="+", metavar="note:velocity")
    ap.add_argument("--plugin", default="/usr/lib/vst3/Surge XT.vst3")
    ap.add_argument("--held", type=float, default=0.375)
    ap.add_argument("--seconds", type=float, default=6.0)
    args = ap.parse_args()
    from pedalboard import load_plugin

    synth = load_plugin(args.plugin)
    with open(args.patch, "rb") as f:
        synth.raw_state = with_patch(synth.raw_state, f.read())

    def strike(key, velocity):
        events = [(bytes([0xB0, 64, 127]), 0.0), (bytes([0x90, key, velocity]), 0.05),
                  (bytes([0x80, key, 0]), 0.05 + args.held)]
        x = synth(events, duration=args.seconds, sample_rate=RATE, reset=True)
        # The pedal up and everything off, and time for it to die.
        synth([(bytes([0xB0, 64, 0]), 0.0), (bytes([0xB0, 123, 0]), 0.0)], duration=2.0, sample_rate=RATE,
              reset=False)
        return x.T

    # The first note after the patch is loaded is not the patch's yet.
    strike(60, 64)
    os.makedirs(args.out, exist_ok=True)
    for spec in args.notes:
        name, velocity = spec.split(":")
        x = strike(key_of(name), int(velocity))
        peak = float(np.abs(x).max())
        late = float(np.abs(x[RATE:RATE * 3 // 2]).max())
        assert peak > 0.01 and late > peak * 0.01, f"{spec}: nothing like a held note came out"
        path = os.path.join(args.out, f"{name}-{velocity}.wav")
        wavfile.write(path, RATE, np.round(np.clip(x, -1, 1) * 32767).astype(np.int16))
        print(f"{path}: peak {peak:.3f}", file=sys.stderr)


if __name__ == "__main__":
    main()

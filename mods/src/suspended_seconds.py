# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Vasily Khoruzhick, "Suspended Seconds" (19 November 2026), a tune for
piano, for `scripts/mkxm.py`.

The tune is not under the project's licence, which covers this script
only: its copyright is the composer's, and it may be copied and
distributed, unmodified or as converted from its module by
onebitdawn's tools, as part of onebitdawn clip packs, including
commercially; all other rights reserved. `suspended_seconds.mid` beside this
file is the tune as he wrote and played it, one piano track, 96 beats
at 120 to the minute, and the same holds for it and for the module
made of it.

Nothing is arranged. Three things of the file are kept with care,
since they are the tune:

- The velocities, 29 to 93, as each note's volume.
- The sustain pedal: a note let go under it rings until the pedal
  comes up, and each has a channel to ring on.
- The times: each note starts on its eighth. The file's second half
  is as played, hundredths of a beat off the grid, and the composer
  asked for it quantized.

The tune ends dry, the pedal up an eighth before its last beat is
over, and goes round to its first bar.

The kits are pianos, a note played by the recording nearest it:
`steinway` is the University of Iowa's Steinway B at mezzo-forte
(the composer plays the tune on a concert grand; its pianissimo is 15
dB over the tape's noise and of no use here), `upright` VSCO 2 CE's
upright at mezzo-forte and `soft` the same at pianissimo.
`steinway-ff` is the Steinway at fortissimo, which is brighter by
its strings; `soft-bright` and `upright-bright` are those two with
their treble lifted, and `steinway-bright` is `steinway-ff` with it.
"""

import os

from mkxm import Module, Sample, pitch_of, read_midi

HERE = os.path.dirname(os.path.abspath(__file__))
IOWA = "https://theremin.music.uiowa.edu/sound files/MIS/Piano_Other/piano/"
KITS = ("steinway", "soft", "upright", "steinway-bright", "soft-bright", "upright-bright", "steinway-ff")
# A `-bright` kit's lift: so many dB more of what is over so many Hz.
# The soft layers have next to nothing up there: in a note's first
# second the octave from 2 to 4 kHz is 24 to 55 dB under the whole and
# what is over 4 kHz 45 dB under or more, so it is not the samples'
# rates (16 000 and 22 050) that make them dull, and a lift finds
# little to lift: none at all in the Steinway's mezzo-forte, whose
# bright kit is therefore its fortissimo.
LIFT = {"steinway-bright": (12.0, 1000), "soft-bright": (12.0, 1000), "upright-bright": (10.0, 2000)}
ROW = 4        # rows a beat: a row is a sixteenth
BEATS = 96
SPEED = 12     # ticks a row
BPM = 240      # FastTracker's: a tick is 2.5 / 240 s, a row 125 ms, a beat half a second
TICK = 2.5 / BPM
DAMP = 12      # ticks the damper takes, 125 ms
LOUDEST = 93   # the file's loudest velocity, which is full volume
CHANNELS = 10  # nine notes at once at the most, dampers counted
# How much of a recording's fall is the envelope's and not the
# sample's, in dB: a quarter.
DEPTH = 12.0
# The notes' starts go to the nearest eighth, their ends and the
# pedal's moves to the nearest thirty-second.
GRID = (0.5, 0.125)


def volume(velocity):
    """A velocity as a volume of 64. A piano's level goes about as the
    square of the velocity; that would put the softest note here 20 dB
    under the loudest, which is under what a clock can say. A power of
    1.5 is 15 dB."""
    return max(1, min(64, round(64 * (velocity / LOUDEST) ** 1.5)))


def best(cut, loop):
    """The sample cut with the loop, of a few lengths about `loop`,
    whose end goes on best into its start: a piano's strings beat, and
    a loop that is not a whole number of beats long is heard going
    round."""
    return max((cut(loop * f) for f in (0.8, 0.9, 1.0, 1.1, 1.2, 1.3)), key=lambda s: s.loop_match)


def score():
    """The notes: (start, length, pitch, velocity) in beats, the pedal
    in the lengths."""
    return read_midi(os.path.join(HERE, "suspended_seconds.mid"), pedal=True, grid=GRID)[1]


def tune(kit="upright"):
    assert kit in KITS, kit
    bright = LIFT.get(kit, 0.0)
    kit = "steinway-ff" if kit == "steinway-bright" else kit.removesuffix("-bright")
    m = Module("Suspended Seconds", channels=CHANNELS, speed=SPEED, bpm=BPM, pattern_rows=64)
    # The tempo is set at the top for when the tune is played round.
    m.tempo(0, BPM)

    # The recordings: the note, the rate it is kept at, where its loop
    # starts and how long that is. The recording is kept whole for as
    # long as its tone is still changing, which is what a piano is
    # known by; a high string has settled sooner.
    piano = {}
    if kit in ("upright", "soft"):
        layer = "pp" if kit == "soft" else "mf"
        for name, rate, attack, loop in (
                ("C3", 16000, 1.5, 0.6), ("G3", 16000, 1.5, 0.6), ("C4", 22050, 1.3, 0.5), ("G4", 22050, 1.3, 0.5),
                ("C5", 22050, 1.1, 0.4), ("G5", 22050, 1.0, 0.4), ("C6", 22050, 0.8, 0.3), ("G6", 22050, 0.6, 0.25)):
            piano[pitch_of(name)] = m.instrument(best(lambda loop: Sample(
                f"upright {layer} {name}", f"Keys/Upright Nr1/UR1_{name}_{layer}_RR1.wav", pitch_of(name), rate,
                attack=attack, loop=loop, bright=bright, struck=(TICK, DEPTH), onset=0.2), loop), damp=DAMP)
    else:
        layer = "ff" if kit == "steinway-ff" else "mf"
        # The left microphone was over the bass strings and the right
        # over the treble.
        for name, rate, attack, loop in (
                ("C3", 16000, 1.5, 0.6), ("G3", 16000, 1.5, 0.6), ("C4", 22050, 1.3, 0.5), ("A4", 22050, 1.3, 0.5),
                ("C5", 22050, 1.1, 0.4), ("F5", 22050, 1.0, 0.4), ("A5", 22050, 0.9, 0.35), ("C6", 22050, 0.8, 0.3),
                ("E6", 22050, 0.7, 0.3)):
            note = pitch_of(name)
            piano[note] = m.instrument(best(lambda loop: Sample(
                f"steinway {layer} {name}", f"{IOWA}Piano.{layer}.{name}.aiff", note, rate, attack=attack, loop=loop,
                bright=bright, struck=(TICK, DEPTH), onset=0.2, channel=0 if note < 60 else 1),
                loop), damp=DAMP)

    def string(pitch):
        """The recording nearest the note; of two as near, the lower,
        played up."""
        return piano[min(piano, key=lambda n: (abs(pitch - n), n))]

    keys = m.voices(list(range(CHANNELS)))
    end = BEATS * ROW
    m.played = []
    for start, length, pitch, velocity in score():
        at = start * ROW
        # The last key-offs stay inside the last row: the row after it
        # is the first again.
        stop = min(at + length * ROW, end - 1 / SPEED)
        ins = string(pitch)
        keys.note(at, stop - at, pitch, ins, volume(velocity))
        m.played.append((start, length, pitch, velocity, ins))
    m.length(end)
    assert m.rows == end, m.rows
    return m

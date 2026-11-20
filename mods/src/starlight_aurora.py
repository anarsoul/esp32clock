# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Vasily Khoruzhick, "Starlight & Aurora" (20 November 2026), for
`scripts/mkxm.py`.

The tune is not under the project's licence, which covers this script
only: its copyright is the composer's, and it may be copied and
distributed, unmodified or as converted from its module by
onebitdawn's tools, as part of onebitdawn clip packs, including
commercially; all other rights reserved. `starlight_aurora.mid` beside
this file is the tune as he wrote and played it, four parts, 148 beats
at 80 to the minute, and the same holds for it and for the module made
of it.

Nothing is arranged. The parts, and what plays each here:

- The piano (the file's first track, an eighth to a note, the pedal
  down from the first beat to the last) is Surge XT's "Acoustic Grand"
  (the LinnStrument MPE patches), which is what the composer played it
  with: each of its seven notes rendered from the synthesizer by
  itself (`scripts/surgenotes.py`, into `xm/surge/acoustic-grand/`),
  soft and loud, and none played off its pitch.
- The bell (his was JE-8086's "MKS80 Bell/Space") is made here after
  what his recording of the part measures as: a tone two octaves over
  the note that starts at once and dies slowly, a click at the twelfth
  partial, and the note itself coming up four tenths of a second
  later; a held note goes on dying, and its room is the instrument's
  release.
- The second lead and the bass are sines (Surge XT's "Sine Lead"),
  made here. The lead is a sine with a little of its octave. The bass
  has its harmonics to the fifth at strength, to be heard by: the note
  itself is 87 to 110 Hz, and the composer chose this bass over the
  plain sine.

It is a tune for a clock with a speaker. On a buzzer it does not
sound (the composer, 2026-11-20), and only the speaker's pack has it
(`scripts/speaker-clips.sh`).

The times are the file's, to the tick, a ninety-sixth of a beat: the
piano is on its eighths, and the other three parts are as he left
them ("в миди уже всё выравнено").

The module's title, which is the clip's label on the clock's pages,
is "Starlight and Aurora": a label may not have an `&`.
"""

import math
import os

import numpy as np

from mkxm import ROOT, Module, Sample, midi_hz, pitch_of, read_midi

HERE = os.path.dirname(os.path.abspath(__file__))
SURGE = os.path.join(ROOT, "xm", "surge", "acoustic-grand")
ROW = 4        # rows a beat: a row is a sixteenth
BEATS = 148
SPEED = 12     # ticks a row
BPM = 160      # FastTracker's: a tick is 2.5 / 160 s, a row 187.5 ms, a beat three quarters of a second
TICK = 2.5 / BPM
DAMP = 8       # ticks the piano's damper takes, 125 ms
# How much of a rendered note's fall is the envelope's and not the
# sample's, in dB.
DEPTH = 12.0
# The piano's two layers: the velocity each was rendered at, and the
# velocity from which a note is the loud one's. The file's are 46 to
# 86, half of them under 66.
SOFT, LOUD, SPLIT = 56, 78, 66
# A piano note is given up after so many beats if its key is not struck
# again first: the pedal never comes up, and by then the note is 30 dB
# down.
RING = 5.0
# The bell's room, the ticks its release takes: 2.5 s.
ROOM = 160
# A bell held goes on dying, as the composer's does: so many dB a
# second from a beat after it starts, a row at a time in the volume
# column (the player ramps a change of level). The sample's own fall
# stops where its loop starts.
FADE = 4.0
# The parts' levels, of 64, a note's volume at the part's loudest
# velocity, set so that each is as loud beside the piano as its stem
# is beside the piano's.
LEVEL = {"piano": 64, "bell": 32, "back": 33, "bass": 15}


def volume(velocity, loudest, level, power):
    """A velocity as a volume, of `level` for the part's loudest. The
    piano's level goes as the velocity to the power of 1.6 (the
    synthesizer's peaks at 46, 62 and 86)."""
    return max(1, min(64, round(level * (velocity / loudest) ** power)))


def best(cut, loop):
    """The sample cut with the loop, of a few lengths about `loop`,
    whose end goes on best into its start."""
    return max((cut(loop * f) for f in (0.8, 0.9, 1.0, 1.1, 1.2, 1.3)), key=lambda s: s.loop_match)


def bell(note, rate=12000, attack=1.1, loop=0.25):
    """The bell at `note`, as a sample with its loop: see the top."""
    n = round(attack * rate)
    # The loop is a whole number of periods and of samples, and the
    # bell is made at the pitch that makes it so; the instrument's
    # tuning has the difference.
    periods = round(midi_hz(note) * loop)
    end = n + round(periods * rate / midi_hz(note))
    hz = periods * rate / (end - n)
    t = np.arange(end) / rate
    held = np.minimum(t, attack)  # the loop is what it is at the loop's start
    # The tone two octaves up: there at once, 5 dB down in a second.
    x = np.sin(2 * math.pi * 4 * hz * t) * 10 ** (-5.0 * held / 20)
    # The click, where the rate has room for it: 40 dB a second.
    if 12 * hz < 0.38 * rate:
        x += 0.11 * np.sin(2 * math.pi * 12 * hz * t) * 10 ** (-40.0 * held / 20)
    # The note itself: nothing for a quarter of a second, over the tone
    # at half a second, then under it.
    swell = np.interp(held, [0.0, 0.25, 0.4, 0.55, 0.75, 1.0, attack], [0.0, 0.0, 0.5, 1.4, 0.9, 0.6, 0.6])
    x += swell * np.sin(2 * math.pi * hz * t)
    return Sample(f"bell {note}", None, note, rate, made=(x, rate, hz, (n, end)))


def sine(*over, n=128):
    """A period of a sine with so much of its second harmonic, its
    third and so on."""
    a = 2 * math.pi * np.arange(n) / n
    return np.sin(a) + sum(level * np.sin((i + 2) * a) for i, level in enumerate(over))


def score():
    """The four parts' notes: (start, length, pitch, velocity) in
    beats. The piano's lengths are the pedal's."""
    path = os.path.join(HERE, "starlight_aurora.mid")
    piano = read_midi(path, pedal=True)[1]
    others = read_midi(path)
    return piano, others[2], others[3], others[4]


def tune(only=None):
    """`only` names one part to write alone, for measuring it."""
    m = Module("Starlight and Aurora", channels=19, speed=SPEED, bpm=BPM, pattern_rows=64)
    # The tempo is set at the top for when the tune is played round.
    m.tempo(0, BPM)

    # The piano: for each key its soft and its loud note, as far as
    # the tune has both.
    piano = {}

    def string(name, velocity):
        path = os.path.join(SURGE, f"{name}-{velocity}.wav")
        if not os.path.exists(path):
            raise SystemExit(f"{path} is not there: render the piano's notes first (scripts/surgenotes.py, "
                             f"`clips/README.md` has the command)")
        note = pitch_of(name)
        piano[note, velocity] = m.instrument(best(lambda loop: Sample(
            f"grand {name} {velocity}", path, note, 11025, attack=0.9, loop=loop, struck=(TICK, DEPTH)), 0.35),
            damp=DAMP)

    for name, velocity in (("F3", SOFT), ("F3", LOUD), ("G3", SOFT), ("A3", LOUD), ("C4", SOFT), ("C4", LOUD),
                           ("D4", SOFT), ("E4", SOFT), ("E4", LOUD), ("F4", LOUD)):
        string(name, velocity)

    # The bell: four of them, none played more than two semitones off.
    bells = {note: m.instrument(bell(note), release=ROOM) for note in (62, 67, 72, 77)}
    back = m.instrument(Sample.wave("sine", sine(0.06), 66), release=32, rise=1)
    bass = m.instrument(Sample.wave("bass", sine(0.7, 0.6, 0.5, 0.4), 43), release=20, rise=2)

    keys = m.voices(list(range(0, 8)))
    rooms = m.voices(list(range(8, 14)))
    leads = m.voices(list(range(14, 17)))
    lows = m.voices(list(range(17, 19)))
    end = BEATS * ROW
    last = end - 1 / SPEED  # the last key-offs stay inside the last row
    p, b, s, l = score()

    def part(name, notes, voices, pick, loudest, power=1.6, fade=0.0):
        if only and only != name:
            return
        for start, length, pitch, velocity in notes:
            at = start * ROW
            if at >= end:
                continue
            stop = min(at + length * ROW, last)
            level = volume(velocity, loudest, LEVEL[name], power)
            if voices.note(at, stop - at, pitch, pick(pitch, velocity), level) and fade:
                line = voices.lines[voices.last]
                for row in range(math.floor(at) + 1, math.ceil(stop)):
                    held = (row - at) / ROW - 1.0
                    if held > 0:
                        line.vol(row, level * 10 ** (-fade * held * 60 / 80 / 20))

    part("piano", [(start, min(length, RING), pitch, velocity) for start, length, pitch, velocity in p], keys,
         lambda pitch, velocity: piano[pitch, LOUD if velocity >= SPLIT else SOFT], 86)
    part("bell", b, rooms, lambda pitch, _: bells[min(bells, key=lambda n: (abs(pitch - n), n))], 90, 1.0, FADE)
    part("back", s, leads, lambda *_: back, 83, 1.0)
    part("bass", l, lows, lambda *_: bass, 96, 1.0)
    m.length(end)
    assert m.rows == end, m.rows
    return m

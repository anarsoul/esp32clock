# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Edvard Grieg, In the Hall of the Mountain King (Peer Gynt, suite
No. 1, Op. 46 No. 4), for `scripts/mkxm.py`.

The notes are the Mutopia Project's piano score, piece 1888, typeset by
Coyau after The University Society's edition of 1918 and placed in the
public domain: `mountain_king.mid` beside this file, as Mutopia gives
it, with its source `mountain_king.ly`. The instruments are given to
its two staves here, after the composer's orchestra: the cellos and
basses plucked with the bassoons, then plucked violins and oboes, then
everyone. The tempo rises as a performance's does, which the MIDI
file's does not. Every note of the score is staccato; a wind or a bow
gives such a note about two thirds of its length and its tone, and a
plucked string rings on, so the notes have channels enough to end by
themselves.
"""

import os

from mkxm import Module, Sample, pulse, read_midi, saw, triangle

HERE = os.path.dirname(os.path.abspath(__file__))
BAR = 16  # rows: a row is a sixteenth


def tune(kit="bright"):
    assert kit in ("bright", "chip"), kit
    m = Module("Mountain King" + (" Chip" if kit == "chip" else ""), channels=17, speed=6, bpm=104, pattern_rows=64)

    # The tempo first, since how long a note rings in rows goes by it:
    # a walk at first, a run by the end.
    def ramp(first, last, a, b):
        for bar in range(first, last + 1):
            m.tempo((bar - 1) * BAR, a + (b - a) * (bar - first) / max(1, last - first))

    ramp(1, 17, 104, 112)
    ramp(18, 33, 116, 138)
    ramp(34, 49, 144, 176)
    ramp(50, 73, 180, 208)
    ramp(74, 84, 208, 208)
    ramp(85, 88, 196, 196)

    def struck(name, path, note, rate, length, **how):
        return m.instrument(Sample(name, path, note, rate, length=length, **how))

    def held(name, path, note, rate, release, **how):
        return m.instrument(Sample(name, path, note, rate, **how), release=release)

    def wave(name, cycle, note, gain, release, **how):
        return m.instrument(Sample.wave(name, cycle, note, gain=gain), release=release, **how)

    def plucked(name, cycle, note, seconds, decay, rate):
        return m.instrument(Sample.pluck(name, cycle, note, seconds, decay, rate))

    if kit == "chip":
        # Plain waves, as a chiptune has them.
        bassoon = wave("bassoon", pulse(0.25), 48, 0.5, 3)
        oboe = wave("oboe", pulse(0.125), 74, 0.5, 3)
        violins = wave("violins", saw(), 83, 0.4, 3, rise=1)
        trumpet = wave("trumpet", saw(64, 16), 77, 0.5, 3)
        trombone = wave("trombone", pulse(0.25), 53, 0.5, 3)
        horn = wave("horn", saw(64, 5), 60, 0.6, 10, rise=2)
        cello = plucked("cello pizz", saw(64, 12), 50, 0.5, 0.5, 16000)
        bass = plucked("bass pizz", saw(64, 8), 40, 0.6, 0.6, 11025)
        pizz = plucked("violin pizz", saw(), 69, 0.35, 0.3, 22050)
        timpani = plucked("timpani", triangle(), 47, 0.6, 0.6, 8000)
        crash = m.instrument(Sample.noise("cymbals", 0.8, 22050, 0.7))
    else:
        # VSCO 2 CE: its loud layers where there are any, and 10 dB
        # more of what is over 2 kHz. The soft ones as they are were
        # dull (the user's ear).
        lift = 10.0
        v = "v3"
        bassoon = held("bassoon", "Woodwinds/Bassoon/sus/PSBassoon_C2_v2_1.wav", 48, 16000, 3,
                       attack=0.2, loop=0.3, bright=lift)
        oboe = held("oboe", "Woodwinds/Oboe/Sus/Oboe_Sus_D4_v3_Main.wav", 74, 32000, 3,
                    attack=0.2, loop=0.25, bright=lift)
        violins = held("violins", "Strings/Violin Section/susVib/VlnEns_susVib_B4_v2.wav", 83, 32000, 3,
                       attack=0.3, loop=0.5, bright=lift)
        trumpet = held("trumpet", "Brass/Trumpet/sus/Sum_SHTrumpet_sus_F4_v3_rr1.wav", 77, 32000, 3,
                       attack=0.2, loop=0.25, bright=lift)
        trombone = held("trombone", f"Brass/Tenor Trombone/sus/tenortbn_sus_F2_{v}_1.wav", 53, 16000, 3,
                        attack=0.25, loop=0.3, bright=lift)
        horn = held("horn", f"Brass/F Horn/sus/MOHorn_sus_C3_{v}_1.wav", 60, 22050, 10,
                    attack=0.3, loop=0.4, bright=lift)
        cello = struck("cello pizz", "Strings/Cello Section/pizzT/pizzT_D2_v2_RR1.wav", 50, 16000, 0.9, bright=lift)
        bass = struck("bass pizz", "Strings/Solo Contrabass/Pizz/BKCtbss_Pizz_E1_v3_rr1.wav", 40, 11025, 1.0,
                      bright=lift)
        pizz = struck("violin pizz", "Strings/Violin Section/Pizz/VlnEns_Pizz_A3_v2_rr1.wav", 69, 22050, 0.7,
                      bright=lift / 2)
        timpani = struck("timpani", "Percussion/Timpani/Timpani2_Hit_v3_rr1_Sum.wav", 47, 11025, 1.2, tune=False)
        crash = struck("cymbals", "VSCO 1 Percussion/varMetal/Cymbals/clash/crash_hit_ff_tight.wav", 60, 22050, 1.2,
                       tune=False)

    tune = m.voices([0, 1, 2, 3])        # the tune and its doubling
    under = m.voices([4, 5, 6, 7])       # what else the right hand has
    low = m.voices([8, 9, 10, 11, 12])   # the left hand
    drum = m.voices([13, 14])
    metal = m.voices([15, 16])

    right, left = read_midi(os.path.join(HERE, "mountain_king.mid"))[1:3]

    def bar_of(start):
        return int(start * 4 // BAR) + 1

    def blown(length):
        """How long a wind or a bow holds a note written staccato: the
        quaver about two thirds of itself, with its tone, a longer note
        nearly whole. The score says e sempre staccato, and marcato."""
        return length * 0.65 if length < 3 else length - 0.75

    # The tune's octave, a four-bar phrase at a time, so that it sits
    # where the instruments that carry it do.
    def lift(bar, pitches):
        if bar < 18:
            return 0
        top = max(pitches)
        return 24 if top <= 62 else 12 if top <= 74 else 0

    phrase = {}
    for start, _, note, _ in right:
        b = bar_of(start)
        if 2 <= b <= 73:
            phrase.setdefault((b - 2) // 4, []).append(note)

    # The right hand: at each moment the top note is the tune.
    moments = {}
    for start, length, note, velocity in right:
        moments.setdefault(round(start * 4 * 6), []).append((note, length * 4, velocity))
    for tick in sorted(moments):
        at = tick / 6
        b = bar_of(at / 4)
        notes = sorted(moments[tick], reverse=True)
        top, length, velocity = notes[0]
        rest = notes[1:]
        if b == 1:
            for note, length, _ in notes:
                under.note(at, length, note, horn, 30)
            continue
        if b >= 85:
            continue  # the ending is written out below
        up = lift(b, phrase[(b - 2) // 4]) if b <= 73 else 0
        grace = length < 0.5
        # The third beat has the accent.
        accent = 6 if at % BAR == 8 and b < 50 else 0
        if b < 18:
            # Bassoons, with the cellos plucked.
            tune.note(at, blown(length), top, bassoon, 34 + accent)
            low.note(at, length, top, cello, 32 + accent)
        elif b < 34:
            # Plucked violins and the oboes.
            tune.note(at, length, top + up, pizz, 40 + accent)
            tune.note(at, blown(length), top + up, oboe, 28 + accent)
        elif b < 42:
            level = 34 + (b - 34) * 2
            tune.note(at, blown(length), top + up, oboe, level + accent)
            tune.note(at, blown(length), top + up, violins, level + accent)
        elif b < 50:
            level = 46 + (b - 42) * 2
            tune.note(at, blown(length), top + up, violins, level)
            tune.note(at, blown(length), top + up - 12, trumpet, level - 6)
        elif b < 74:
            tune.note(at, blown(length), top, violins, 64)
            tune.note(at, blown(length), top - 12, trumpet, 58)
        elif grace:
            # Three in a row, a channel each.
            under.note(at, 0.5, top, violins, 44)
        elif len(notes) > 1 and b not in (76, 77, 80, 81):
            # The ending's chords, everyone.
            tune.note(at, 2.5, top, violins, 64)
            tune.note(at, 2.5, top - 12, trumpet, 64)
            metal.note(at, 4, 60, crash, 52)
            drum.note(at, 2, 47, timpani, 64)
        else:
            tune.note(at, length, top, pizz, 44)
            tune.note(at, blown(length), top, oboe, 36)
        for note, length, _ in rest:
            if b < 50:
                under.note(at, length, note + up, pizz, 26 if b < 34 else 32)
            elif b < 74:
                # The notes held under the tune, for as long as written.
                under.note(at, max(length - 0.5, 1), note - 12, trumpet, 44)
            else:
                under.note(at, 2.5, note - 12, trumpet, 56)

    # The left hand, an octave up: the clock has no bass to speak of.
    for start, length, note, velocity in left:
        at = round(start * 4 * 6) / 6
        length *= 4
        b = bar_of(start)
        if b >= 85:
            continue
        together = [n for s, _, n, _ in left if s == start]
        if b < 18:
            low.note(at, length, note + 12, bass, 40)
        elif b < 42:
            low.note(at, length, note + 12, bass if note < 45 else cello, 40 + (b - 18) // 2)
        elif b < 50:
            if at % 4 == 0:
                low.note(at, length, note + 12, cello, 52)
            if note == min(together):
                low.note(at, blown(length), note + 12, trombone, 40 + (b - 42) * 2)
        elif b < 74:
            low.note(at, blown(length), note + 12, trombone, 60)
        elif len(together) >= 3:
            low.note(at, 2.5, note, trombone, 60)
        else:
            low.note(at, blown(length), note + 12, trombone, 56)
            low.note(at, length, note + 12, cello, 56)

    # Drums from where everyone plays: the kettledrum on the beats, the
    # cymbals at each phrase and then each bar.
    for b in range(50, 74):
        row = (b - 1) * BAR
        for beat in (0, 8):
            drum.note(row + beat, 2, 47 if beat == 0 else 42, timpani, 56)
        if (b - 50) % 4 == 0 or b >= 66:
            metal.note(row, 4, 60, crash, 48)

    # The ending from bar 85, where the file's times are a played
    # ritardando: four chords growing, the roll, the last two strokes.
    def chord(at, level, length=2.5, cymbal=True):
        for note in (95, 86):
            tune.note(at, length, note, violins, level)
        for note in (71, 68):
            under.note(at, length, note, trumpet, level)
        for note in (65, 62, 59):
            low.note(at, length, note, trombone, level)
        if cymbal:
            metal.note(at, 4, 60, crash, level - 12)
        drum.note(at, 2, 47, timpani, level)

    row = 84 * BAR
    for beat in range(4):
        chord(row + beat * 4, 40 + beat * 6, cymbal=beat % 2 == 0)
    row = 85 * BAR
    for i in range(24):  # the kettledrum's roll, two bars less two beats
        drum.note(row + 8 + i, 1, 47, timpani, 20 + i * 44 // 23)
    row = 87 * BAR
    drum.note(row, 2, 47, timpani, 64)
    low.note(row, 3, 47, trombone, 64)
    low.note(row, 3, 47, cello, 64)
    for i, note in enumerate((92, 93, 94)):
        under.note(row + 3 + (i + 3) / 6, 0.5, note, violins, 50)
    chord(row + 4, 64, length=8)
    m.length(88 * BAR)
    return m

# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Edvard Grieg, Morning (Morgenstimmung; Peer Gynt, suite No. 1,
Op. 46 No. 1), for `scripts/mkxm.py`.

The notes were read off the composer's own arrangement for piano four
hands, Edition Peters 2432 (plate 7191), in the Sibley Music Library's
scan, hdl.handle.net/1802/16617: the Primo's two staves and the
Secondo's two, given here to a flute, an oboe, a horn and strings after
his orchestra. It is shortened: bars 1 to 31, to the height of the
sunrise, and then bars 56 to 76, where the tune comes back, the birds
call and a bar's rest ends it; the middle, bars 32 to 55, and the last
eleven bars after that rest are left out. A bar here is
twelve rows, a row a sixteenth.

Read by eye, with the note heads' heights measured where the print is
dense. A chord of more notes than there are channels is thinned.
"""

from mkxm import Module, Sample, pitch_of, pulse, saw

BAR = 12
STRINGS = "Strings/"


def tune(kit="bright"):
    assert kit in ("bright", "chip"), kit
    # Twelve ticks a row, so that a note's release has steps enough.
    m = Module("Morning Mood" + (" Chip" if kit == "chip" else ""), channels=19, speed=12, bpm=180, pattern_rows=48)
    # The tempo is set at the top for when the tune is played round.
    m.tempo(0, 180)

    def held(name, path, note, rate, **how):
        sample = {k: how.pop(k) for k in ("attack", "loop", "skip", "fade", "bright") if k in how}
        return m.instrument(Sample(name, path, note, rate, **sample), **how)

    def wave(name, cycle, note, gain, **how):
        return m.instrument(Sample.wave(name, cycle, note, gain=gain), **how)

    if kit == "chip":
        # Plain waves, as a chiptune has them: a square for the flute,
        # a narrow pulse for the oboe, a rounded saw for the horn, saws
        # for the strings.
        flute = wave("flute", pulse(0.5), 81, 0.45, release=8, rise=1, vibrato=(24, 8, 18))
        oboe = wave("oboe", pulse(0.125), 74, 0.5, release=8, rise=1, vibrato=(24, 6, 18))
        horn = wave("horn", saw(64, 5), 60, 0.6, release=12, rise=3)
        high = wave("violins", saw(), 83, 0.4, release=16, rise=5, vibrato=(36, 6, 16))
        mid = wave("violins low", saw(), 69, 0.4, release=16, rise=5, vibrato=(36, 6, 16))
        low = wave("cellos", saw(64, 12), 57, 0.5, release=16, rise=5)
        deep = wave("basses", saw(64, 8), 40, 0.6, release=16, rise=5)
    else:
        # VSCO 2 CE: its loud layers, which have more of their upper
        # partials than the soft ones, and 10 dB more of what is over
        # 2 kHz. The soft ones as they are were dull (the user's ear).
        lift = 10.0
        v = "v3"
        flute = held("flute", f"Woodwinds/Flute/susNV/LDFlute_susNV_A4_{v}_1.wav", 81, 32000,
                     attack=0.25, loop=0.3, release=10, vibrato=(24, 8, 18), bright=lift)
        oboe = held("oboe", f"Woodwinds/Oboe/Sus/Oboe_Sus_D4_{v}_Main.wav", 74, 32000,
                    attack=0.25, loop=0.3, release=10, vibrato=(24, 6, 18), bright=lift)
        horn = held("horn", f"Brass/F Horn/sus/MOHorn_sus_C3_{v}_1.wav", 60, 22050,
                    attack=0.3, loop=0.4, release=14, bright=lift)
        high = held("violins", STRINGS + "Violin Section/susVib/VlnEns_susVib_B4_v2.wav", 83, 32000,
                    skip=0.8, attack=0.25, loop=0.7, release=20, rise=4, bright=lift)
        mid = held("violins low", STRINGS + "Violin Section/susVib/VlnEns_susVib_A3_v2.wav", 69, 22050,
                   skip=0.8, attack=0.25, loop=0.7, release=20, rise=4, bright=lift)
        low = held("cellos", STRINGS + "Cello Section/susvib/susvib_A2_v3_1.wav", 57, 16000,
                   skip=0.6, attack=0.25, loop=0.7, release=20, rise=4, bright=lift)
        deep = held("basses", STRINGS + "Cello Section/susvib/susvib_E1_v3_1.wav", 40, 11025,
                    skip=0.6, attack=0.25, loop=0.7, release=20, rise=4, bright=lift / 2)

    def strings(pitch):
        return deep if pitch < 47 else low if pitch < 62 else mid if pitch < 78 else high

    # Channels enough for a note to die away before the next is given
    # its channel: the player starts a note with no ramp.
    fl, ob = m.voices([0, 1, 2]), m.voices([3, 4])
    v1, v2 = m.voices([5, 6]), m.voices([7, 8])
    harmony = m.voices([9, 10, 11, 12, 13, 14])
    bass, extra = m.voices([15, 16]), m.voices([17, 18])

    def bar(n):
        return (n - 1) * BAR

    def chord(at, length, names, vol):
        for name in names.split():
            harmony.note(at, length, pitch_of(name), strings(pitch_of(name)), vol, legato=True)

    def low_notes(at, length, names, vol):
        """The bass: its lowest note on the bass line, the rest with
        the harmony."""
        pitches = sorted(pitch_of(n) for n in names.split())
        bass.note(at, length, pitches[0], strings(pitches[0]), vol)
        for p in pitches[1:]:
            harmony.note(at, length, p, strings(p), vol, legato=True)

    # Bars 1 to 20: the flute and the oboe, turn about. --------------

    p = 30
    fl.phrase(bar(1), "B5/2 G#5/2 F#5/2 E5/2 F#5/2 G#5/2"
              " B5/2 (G#5 A5) G#5/2 F#5/2 E5/2 F#5/1 G#5/1 F#5/1 G#5/1"
              " (G#5 A5) B5/2 G#5/2 v34 B5/2 C#6/2 G#5/2 C#6/2"
              " v30 B5/2 G#5/2 F#5/2 E5/4", flute, p, graces=extra)
    chord(bar(1), 12, "E3 B3 E4 G#4", 16)
    chord(bar(2), 12, "E3 B3 E4 G#4", 16)
    chord(bar(3), 6, "E3 B3 E4 G#4", 18)
    chord(bar(3) + 6, 6, "C#3 G#3 E4 G#4", 20)
    chord(bar(4), 10, "E3 E4 G#4", 16)
    bass.phrase(bar(4), "B3/6 B3/2 G#3/2 F#3/2", low, 22)

    ob.phrase(bar(5), "B4/2 G#4/2 F#4/2 E4/2 F#4/2 G#4/2"
              " B4/2 (G#4 A4) G#4/2 F#4/2 E4/2 F#4/1 G#4/1 F#4/1 G#4/1"
              " (G#4 A4) B4/2 G#4/2 v34 B4/2 C#5/2 G#4/2 C#5/2"
              " v30 D#5/2 B#4/2 A#4/2 G#4/4", oboe, p, graces=extra)
    for b in (5, 6):
        chord(bar(b), 12, "E3 G#3 B2", 16)
        bass.note(bar(b), 12, pitch_of("E2"), deep, 22)
    chord(bar(7), 6, "E3 G#3 B2", 18)
    bass.note(bar(7), 6, pitch_of("E2"), deep, 24)
    chord(bar(7) + 6, 6, "E3 G#3 C#3", 20)
    bass.note(bar(7) + 6, 6, pitch_of("C#2"), deep, 24)
    chord(bar(8), 6, "G#3 B#3 D#3", 18)
    bass.note(bar(8), 10, pitch_of("G#2"), deep, 22)
    extra.phrase(bar(8) + 6, "D#4/2 B#3/2 A#3/2", low, 22)

    fl.phrase(bar(9), "D#6/2 B#5/2 A#5/2 G#5/2 A#5/2 B#5/2"
              " D#6/2 (B#5 C#6) B#5/2 A#5/2 G#5/2 A#5/1 B#5/1 A#5/1 B#5/1"
              " (B#5 C#6) D#6/2 B#5/2 v34 D#6/2 E6/2 B#5/2 E6/2"
              " v30 D#6/2 B#5/2 A#5/2 G#5/4", flute, p, graces=extra)
    chord(bar(9), 12, "G#3 D#4 G#4 B#4", 16)
    chord(bar(10), 12, "G#3 D#4 G#4 B#4", 16)
    chord(bar(11), 6, "G#3 D#4 G#4 B#4", 18)
    chord(bar(11) + 6, 6, "E3 B#3 E4 G#4", 20)
    chord(bar(12), 10, "G#3 G#4 B#4", 16)
    bass.phrase(bar(12), "D#4/6 D#4/2 B#3/2 A#3/2", low, 22)

    ob.phrase(bar(13), "D#5/2 B#4/2 A#4/2 G#4/2 A#4/2 B#4/2"
              " D#5/2 (B#4 C#5) B#4/2 A#4/2 G#4/2 A#4/1 B#4/1 A#4/1 B#4/1"
              " (B#4 C#5) D#5/2 B#4/2 v34 D#5/2 E5/2 C#5/2 E5/2"
              " v32 F#5/2 D#5/2 C#5/2 B4/4", oboe, p, graces=extra)
    for b in (13, 14):
        chord(bar(b), 12, "G#3 B#3 D#3", 16)
        bass.note(bar(b), 12, pitch_of("G#2"), deep, 22)
    chord(bar(15), 6, "G#3 B#3 D#3", 18)
    bass.note(bar(15), 6, pitch_of("G#2"), deep, 24)
    chord(bar(15) + 6, 6, "E3 A3 C#4", 20)
    bass.note(bar(15) + 6, 6, pitch_of("C#3"), low, 24)
    chord(bar(16), 6, "F#3 A3 B3", 20)
    bass.note(bar(16), 10, pitch_of("B2"), low, 24)
    extra.phrase(bar(16) + 6, "F#4/2 D#4/2 C#4/2", mid, 22)

    fl.phrase(bar(17), "F#6/2 D#6/2 C#6/2 B5/2 C#6/1 D#6/1 C#6/1 D#6/1", flute, 34)
    chord(bar(17), 12, "B3 F#4 B4 D#5", 20)
    ob.phrase(bar(18), "F#5/2 D#5/2 C#5/2 B4/2 C#5/1 D#5/1 C#5/1 D#5/1", oboe, 34)
    chord(bar(18), 12, "B3 D#4 F#3", 20)
    bass.note(bar(18), 12, pitch_of("B2"), low, 26)
    for b, vol, seventh in ((19, 38, "B3 D#4"), (20, 44, "A3 B3 D#4")):
        fl.phrase(bar(b), "F#6/2 D#6/2 B5/2", flute, vol)
        ob.phrase(bar(b) + 6, "F#5/2 D#5/2 B4/2", oboe, vol)
        chord(bar(b), 6, "B3 F#4 B4 D#5", vol - 14)
        chord(bar(b) + 6, 6, seventh + " F#3", vol - 12)
        bass.note(bar(b) + 6, 6, pitch_of("B2"), low, vol - 8)

    # Bars 21 to 29: everyone, the tune in the violins in octaves. ---

    def tutti(b, top, inner, vol):
        # `top` as the upper violins have it; the lower play it an
        # octave down and the flute goes with the upper, its grace
        # notes with it.
        v1.phrase(bar(b), strip(top), high, vol)
        v2.phrase(bar(b), down(strip(top)), mid, vol - 6)
        fl.phrase(bar(b), top, flute, vol - 10, graces=extra)
        ob.phrase(bar(b), inner, oboe, vol - 16, legato=False)

    def strip(notes):
        """A phrase without its grace notes."""
        out, skip = [], False
        for token in notes.replace("(", " ( ").replace(")", " ) ").split():
            if token == "(":
                skip = True
            elif token == ")":
                skip = False
            elif not skip:
                out.append(token)
        return " ".join(out)

    def down(notes):
        """A phrase an octave lower."""
        out = []
        for token in notes.split():
            if "/" in token and token[0] != "r":
                name, length = token.split("/")
                token = f"{name[:-1]}{int(name[-1]) - 1}/{length}"
            out.append(token)
        return " ".join(out)

    f = 50
    tutti(21, "B5/2 G#5/2 F#5/2 E5/2 F#5/2 G#5/2", "E5/12", f)
    tutti(22, "B5/2 (A5) G#5/2 F#5/2 E5/2 F#5/1 G#5/1 F#5/1 G#5/1", "E5/12", f)
    tutti(23, "(A5) B5/2 G#5/2 B5/2 C#6/2 G#5/2 C#6/2", "E5/6 E5/6", f)
    tutti(24, "C#6/2 A5/2 G#5/2 F#5/2", "E5/8", f)
    tutti(25, "C#6/2 A5/2 G#5/2 F#5/2 G#5/2 A5/2", "E5/12", f)
    tutti(26, "C#6/2 (B5) A5/2 G#5/2 F#5/2 G#5/1 A5/1 G#5/1 A5/1", "E5/12", f)
    tutti(27, "(B5) C#6/2 A5/2 C#6/2 D#6/2 A5/2 D#6/2", "E5/6 F#5/6", 54)
    tutti(28, "(C#6) D#6/2 B5/2 D#6/2 E6/2 B5/2 E6/2", "F#5/6 G#5/6", 58)
    tutti(29, "(D#6) E6/2 C#6/2 E6/2 F#6/2 C#6/2 F#6/2", "G#5/6 A5/6", 62)

    def under(b, at, length, names, low_name, vol):
        for name in names.split():
            pitch = pitch_of(name)
            harmony.note(bar(b) + at, length, pitch, horn if 52 <= pitch <= 64 else strings(pitch), vol, legato=True)
        bass.note(bar(b) + at, length, pitch_of(low_name), strings(pitch_of(low_name)), vol + 4)

    under(21, 0, 12, "B2 E3 G#3 B3", "E2", 36)
    under(22, 0, 12, "B2 E3 G#3 B3", "E2", 36)
    under(23, 0, 6, "B2 E3 G#3 B3", "E2", 36)
    under(23, 6, 6, "C#3 E3 G#3 C#4", "E2", 38)
    under(24, 0, 8, "C#3 F#3 A3 C#4", "F#2", 38)
    extra.phrase(bar(24) + 6, "A2/2 F#2/2 C#2/2", low, 40, legato=False)
    under(25, 0, 12, "C#3 F#3 A3 C#4", "F#2", 38)
    under(26, 0, 12, "C#3 F#3 A3 C#4", "F#2", 38)
    under(27, 0, 6, "C#3 E3 F#3 A3", "F#2", 42)
    under(27, 6, 6, "D#3 F#3 A3 D#4", "F#2", 44)
    under(28, 0, 6, "D#3 F#3 G#3 B3", "G#2", 46)
    under(28, 6, 6, "E3 G#3 B3 E4", "G#2", 48)
    under(29, 0, 6, "E3 G#3 A3 C#4", "A2", 50)
    under(29, 6, 6, "F#3 A3 C#4 F#4", "A2", 52)

    # Bars 30 and 31: the sun is up. ---------------------------------

    v1.phrase(bar(30), "G#6/1 D#6/1 B#5/1 G#5/1 B#5/1 D#6/1 D#6/1 B#5/1 G#5/1 D#5/1 G#5/1 B#5/1"
              " v56 B#5/1 G#5/1 D#5/1 B#4/1 D#5/1 G#5/1 v46 G#5/1 D#5/1 B#4/1 G#4/1 v38 B#4/1 D#5/1",
              high, 64)
    fl.phrase(bar(30), "G#6/6 D#6/6 v50 B#5/6 v40 G#5/6", flute, 56)
    ob.phrase(bar(30), "G#5/6", oboe, 50)
    v2.phrase(bar(30) + 6, "G#5/1 D#5/1 B#4/1 G#4/1 B#4/1 D#5/1"
              " v52 D#5/1 B#4/1 G#4/1 D#4/1 G#4/1 B#4/1 v42 B#4/1 G#4/1 D#4/1 B#3/1 v34 D#4/1 G#4/1",
              mid, 60)
    under(30, 0, 16, "D#3 G#3 B#3 D#4", "G#2", 56)
    extra.note(bar(30), 6, pitch_of("G#4"), mid, 56)

    # Bars 56 to 62 (here 32 to 38): the horn has the tune, very
    # softly, under ripples. -----------------------------------------

    def r(n):
        return bar(31 + n)

    e_up, e_down = "B5/1 G#5/1 E5/1 B4/1 E5/1 G#5/1", "E5/1 B4/1 G#4/1 E4/1 G#4/1 B4/1"
    c_up, c_down = "C#6/1 G#5/1 E5/1 C#5/1 E5/1 G#5/1", "E5/1 C#5/1 G#4/1 E4/1 G#4/1 C#5/1"
    ripples = {1: "ee", 2: "ee", 3: "ec", 4: "ee", 5: "ce", 6: "ce", 7: "cc"}
    for n, halves in ripples.items():
        for i, half in enumerate(halves):
            fl.phrase(r(n) + i * 6, e_up if half == "e" else c_up, flute, 16)
            v2.phrase(r(n) + i * 6, e_down if half == "e" else c_down, mid, 12)
    extra.phrase(r(1), "B3/2 G#3/2 F#3/2 E3/2 F#3/2 G#3/2"
                 " B3/2 G#3/2 F#3/2 E3/2 F#3/1 G#3/1 F#3/1 G#3/1"
                 " B3/2 G#3/2 B3/2 C#4/2 G#3/2 C#4/2 B3/10 r/2"
                 " C#4/2 G#3/2 C#4/2 B3/4 r/2 C#4/2 G#3/2 C#4/2 B3/4 r/2"
                 " C#4/10 r/2 G#3/6 C#4/6", horn, 34)
    for n in (1, 2):
        low_notes(r(n), 12, "E2 B2", 16)
    low_notes(r(3), 6, "E2 E3 G#3", 16)
    low_notes(r(3) + 6, 6, "C#2 E3 G#3", 16)
    low_notes(r(4), 10, "E2 E3 G#3", 16)
    for n in (5, 6):
        low_notes(r(n), 6, "C#2 E3 G#3", 16)
        low_notes(r(n) + 6, 4, "E2 E3 G#3", 16)
    low_notes(r(7), 10, "C#2 E3 G#3", 16)

    # Bars 64 to 67 (here 40 to 43): the oboe, and a trill. ----------

    trill = "D#5/1 E5/1 D#5/1 E5/1 D#5/1 E5/1 D#5/1 E5/1 D#5/1 E5/1 C#5/1 D#5/1"
    pp = 24
    ob.phrase(r(9), "B4/2 G#4/2 F#4/2 E4/2 F#4/2 G#4/2"
              " B4/2 G#4/2 F#4/2 E4/2 F#4/1 G#4/1 F#4/1 G#4/1"
              " G#4/2 E4/2 G#4/2 A4/2 Fx4/2 A4/2 " + trill, oboe, pp)
    extra.phrase(r(9), "B3/6 A3/6 G#3/6 C#4/6 B3/6 A#3/6", horn, 22)
    chord(r(9), 6, "G#3 E4", 14)
    chord(r(9) + 6, 6, "E4", 14)
    bass.phrase(r(9), "B2/6 C#3/6 B2/6 A2/6", low, 20)
    chord(r(10), 6, "E4", 14)
    chord(r(10) + 6, 4, "E3 E4", 14)
    chord(r(11), 12, "E4 B2", 16)
    bass.phrase(r(11), "G#2/6 G2/6", deep, 20)
    low_notes(r(12), 12, "F#2 B2 A3 D#4", 16)
    harmony.note(r(12), 12, pitch_of("F#4"), mid, 14)

    # Bars 68 to 76 (here 44 to 52): birds. --------------------------

    def bird(at, vol):
        """A trilled quaver with its turn, over the flute's channels and
        the violins', which are resting."""
        fl.note(at, 0.5, pitch_of("B5"), flute, vol)
        v1.note(at + 0.5, 0.5, pitch_of("C#6"), flute, vol - 6)
        fl.note(at + 1, 0.5, pitch_of("B5"), flute, vol - 4)
        v1.note(at + 1.5, 0.5, pitch_of("A#5"), flute, vol - 8)
        fl.note(at + 2, 1, pitch_of("B5"), flute, vol - 6)

    def calls(at, vol):
        """The Secondo's three quavers, the oboe on top."""
        ob.phrase(at, "G#4/2 F#4/2 G#4/2", oboe, vol, legato=False, detach=0.5)
        harmony.note(at, 1.5, pitch_of("G#3"), low, vol - 8)
        harmony.note(at, 1.5, pitch_of("E4"), mid, vol - 8)
        harmony.note(at + 2, 1.5, pitch_of("B3"), low, vol - 8)
        harmony.note(at + 4, 1.5, pitch_of("E4"), mid, vol - 8)

    def birds(n, both, vol):
        bird(r(n), vol)
        calls(r(n), vol - 2)
        if both:
            bird(r(n) + 6, vol)
            calls(r(n) + 6, vol - 2)

    birds(13, True, 30)
    v2.phrase(r(13), "E5/6 E5/6", mid, 20, legato=False)
    bass.note(r(13), 4, pitch_of("E2"), deep, 20)
    extra.note(r(13) + 6, 6, pitch_of("B4"), horn, 24)
    birds(14, True, 30)
    v2.phrase(r(14), "E5/6 E5/6", mid, 20, legato=False)
    extra.phrase(r(14), "B4/6 B4/6", horn, 24, legato=False)

    ob.phrase(r(15), "B4/2 G#4/2 B4/2 C#5/2 A#4/2 C#5/2 " + trill, oboe, pp)
    extra.phrase(r(15), "B3/6 A#3/6", horn, 22)
    chord(r(15), 12, "E4 B2", 16)
    bass.phrase(r(15), "G#2/6 G2/6", deep, 20)
    low_notes(r(16), 12, "F#2 B2 A3 D#4", 16)
    harmony.note(r(16), 12, pitch_of("F#4"), mid, 14)

    birds(17, True, 26)
    v2.phrase(r(17), "E5/6 E5/6", mid, 18, legato=False)
    bass.note(r(17), 4, pitch_of("E2"), deep, 18)
    extra.note(r(17) + 6, 6, pitch_of("B4"), horn, 22)
    ob.phrase(r(18), trill, oboe, 22)
    low_notes(r(18), 12, "F#2 B2 A3 D#4", 14)
    for n, vol in ((19, 22), (20, 18)):
        birds(n, False, vol)
        v2.note(r(n), 4, pitch_of("E5"), mid, vol - 6)
        bass.note(r(n), 4, pitch_of("E2"), deep, vol - 4)
        ob.phrase(r(n) + 6, "D#5/1 E5/1 D#5/1 E5/1 C#5/1 D#5/1", oboe, vol - 2)
        chord(r(n) + 6, 6, "B2 A3 D#4", vol - 8)
        extra.note(r(n) + 6, 6, pitch_of("F#2"), deep, vol - 4)
    # Bar 76 is silent in the score, and the tune ends here, on that
    # rest, without the last eleven bars (the flute once more, the
    # cellos, the last chords). The user, 2026-11-19: "The rest likely
    # sounds good with orchestra, not so good in mod". The silent bar
    # is what a ringing alarm has before it starts over.
    m.length(bar(53))
    return m

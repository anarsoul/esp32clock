#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Write a FastTracker 2 module from a tune written down as Python.

    scripts/mkxm.py mods/src/morning.py mods/grieg-morning-chip.xm chip

The tune's file defines `tune()`, which returns a `Module`: instruments
made of recorded notes, and lines of notes on a grid of rows. Only what
the clock's player has (`core/src/xm.rs`) is written: notes, the volume
column's set, the volume envelope with its sustain point and the fade,
an instrument's own vibrato, and the effects 3 (a slur, as a portamento
too fast to hear), ED (a note late in its row), K (a key-off late in
its row) and F (the tempo). That player starts a note with no ramp,
so the notes are given channels where nothing is sounding (`Voices`).

The recorded notes are Versilian Studios' VSCO 2 Community Edition,
which is CC0. They are fetched by name from its repository at a pinned
commit into `xm/vsco/` (ignored by git) and cut down here: mono, a new
rate, a loop found and crossfaded for a note that is held, 8 bits.
A piano's note is a struck string (`Sample(struck=)`,
`Instrument(damp=)`): its decay is the instrument's envelope, with no
sustain point, and its key-off the damper.

Needs numpy and scipy.
"""

import importlib.util
import math
import os
import struct
import sys
import urllib.parse
import urllib.request

import numpy as np
from scipy.signal import lfilter, resample_poly

VSCO = "https://raw.githubusercontent.com/sgossner/VSCO-2-CE/440300901dfe9275fd84e0b7763af1f8443ae62e/"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CACHE = os.path.join(ROOT, "xm", "vsco")

# FastTracker's C-4 plays a sample at this rate, and is note 49 of 1..96.
C4_HZ = 8363.0
KEY_OFF = 97


def fetch(path):
    """The local copy of a file of VSCO 2 CE, or of a file anywhere by
    its URL (kept in `xm/` under its host's name), fetched if it is
    not there."""
    if "://" in path:
        parts = urllib.parse.urlsplit(path)
        local = os.path.join(ROOT, "xm", parts.netloc, os.path.basename(parts.path))
        url = urllib.parse.urlunsplit(parts._replace(path=urllib.parse.quote(parts.path)))
    else:
        local = os.path.join(CACHE, path)
        url = VSCO + urllib.parse.quote(path)
    if not os.path.exists(local):
        os.makedirs(os.path.dirname(local), exist_ok=True)
        print(f"fetching {path}", file=sys.stderr)
        with urllib.request.urlopen(url) as r:
            data = r.read()
        with open(local + ".part", "wb") as f:
            f.write(data)
        os.rename(local + ".part", local)
    return local


def read_aiff(path, channel=None):
    """An AIFF file of 16 bits as mono floats in -1..1 and its rate."""
    d = open(path, "rb").read()
    assert d[:4] == b"FORM" and d[8:12] == b"AIFF", path
    at, channels, rate, x = 12, 0, 0, None
    while at + 8 <= len(d):
        tag, size = d[at:at + 4], struct.unpack(">I", d[at + 4:at + 8])[0]
        body = d[at + 8:at + 8 + size]
        if tag == b"COMM":
            channels, _, bits = struct.unpack(">hIh", body[:8])
            assert bits == 16, f"{path}: {bits} bits"
            # The rate is an 80-bit float.
            exponent, mantissa = struct.unpack(">HQ", body[8:18])
            rate = round(mantissa * 2.0 ** ((exponent & 0x7FFF) - 16383 - 63))
        elif tag == b"SSND":
            offset = struct.unpack(">I", body[:4])[0]
            x = np.frombuffer(body[8 + offset:8 + offset + (len(body) - 8 - offset) // 2 * 2], ">i2")
        at += 8 + size + (size & 1)
    assert channels and x is not None, path
    x = x[:len(x) // channels * channels].reshape(-1, channels).astype(np.float64) / 32768
    return (x.mean(axis=1) if channel is None else x[:, channel]), rate


def read_wav(path, channel=None):
    """A WAV file as mono floats in -1..1 and its rate; an AIFF file
    too. `channel` is the one to take, where it is not all of them."""
    d = open(path, "rb").read()
    if d[:4] == b"FORM":
        return read_aiff(path, channel)
    assert d[:4] == b"RIFF" and d[8:12] == b"WAVE", path
    at = 12
    fmt = None
    while at + 8 <= len(d):
        tag, size = d[at:at + 4], struct.unpack("<I", d[at + 4:at + 8])[0]
        body = d[at + 8:at + 8 + size]
        if tag == b"fmt ":
            kind, channels, rate, _, _, bits = struct.unpack("<HHIIHH", body[:16])
            fmt = (kind, channels, rate, bits)
        elif tag == b"data":
            assert fmt, path
            kind, channels, rate, bits = fmt
            if bits == 16:
                x = np.frombuffer(body[:len(body) // 2 * 2], "<i2").astype(np.float64) / 32768
            elif bits == 24:
                b = np.frombuffer(body[:len(body) // 3 * 3], np.uint8).reshape(-1, 3).astype(np.int32)
                v = b[:, 0] | b[:, 1] << 8 | b[:, 2] << 16
                x = (v - ((v & 0x800000) << 1)).astype(np.float64) / 8388608
            elif bits == 32 and kind == 3:
                x = np.frombuffer(body[:len(body) // 4 * 4], "<f4").astype(np.float64)
            else:
                raise ValueError(f"{path}: {bits} bits, format {kind}")
            x = x[:len(x) // channels * channels].reshape(-1, channels)
            return (x.mean(axis=1) if channel is None else x[:, channel]), rate
        at += 8 + size + (size & 1)
    raise ValueError(f"{path}: no data")


def pitch_hz(x, rate, near):
    """The pitch of `x` by autocorrelation, looked for within three
    semitones of `near`."""
    x = x - x.mean()
    lo = int(rate / (near * 2 ** (3 / 12)))
    hi = int(rate / (near * 2 ** (-3 / 12))) + 2
    n = len(x) - hi
    best, lag = -1.0, lo
    scores = []
    for k in range(lo, hi):
        a, b = x[:n], x[k:k + n]
        c = float(a @ b) / (math.sqrt(float(a @ a) * float(b @ b)) + 1e-12)
        scores.append(c)
        if c > best:
            best, lag = c, k
    i = lag - lo
    if 0 < i < len(scores) - 1:
        # A parabola through the peak and its neighbours.
        y0, y1, y2 = scores[i - 1], scores[i], scores[i + 1]
        lag += 0.5 * (y0 - y2) / (y0 - 2 * y1 + y2 + 1e-12)
    return rate / lag


def midi_hz(note):
    return 440.0 * 2 ** ((note - 69) / 12)


def read_midi(path, pedal=False, grid=None):
    """A MIDI file's notes: for each track a sorted list of (start,
    length, note, velocity), the times in quarter notes. With `pedal`
    a note let go while the sustain pedal is down (controller 64 at 64
    or more) lasts until the pedal comes up or its key is struck
    again, as a piano's does. `grid` puts a played file on the grid:
    (a, b) in quarter notes, each note's start to the nearest `a` and
    its end and the pedal's moves to the nearest `b`."""
    d = open(path, "rb").read()
    assert d[:4] == b"MThd", path
    _, count, division = struct.unpack(">HHH", d[8:14])
    at, tracks = 14, []
    for _ in range(count):
        assert d[at:at + 4] == b"MTrk"
        size = struct.unpack(">I", d[at + 4:at + 8])[0]
        t = d[at + 8:at + 8 + size]
        at += 8 + size
        i, tick, status, held, notes = 0, 0, 0, {}, []
        down, ringing = False, {}

        def ended(key, start, velocity):
            notes.append((start / division, (tick - start) / division, key, velocity))

        def number():
            nonlocal i
            v = 0
            while True:
                b = t[i]
                i += 1
                v = v << 7 | b & 127
                if b < 128:
                    return v

        played = 0
        while i < len(t):
            played += number()
            tick = played
            b = t[i]
            if grid:
                on = b & 0xF0 == 0x90 and t[i + 2] or not b & 0x80 and status & 0xF0 == 0x90 and t[i + 1]
                step = (grid[0] if on else grid[1]) * division
                tick = round(round(played / step) * step)
            if b == 0xFF:
                i += 2
                size = number()
                i += size
            elif b in (0xF0, 0xF7):
                i += 1
                size = number()
                i += size
            else:
                if b & 0x80:
                    status = b
                    i += 1
                kind = status & 0xF0
                if kind in (0xC0, 0xD0):
                    i += 1
                    continue
                key, value = t[i], t[i + 1]
                i += 2
                if kind == 0xB0 and key == 64 and pedal:
                    down = value >= 64
                    if not down:
                        for k in sorted(ringing):
                            ended(k, *ringing.pop(k))
                elif kind == 0x90 and value:
                    if key in ringing:
                        ended(key, *ringing.pop(key))
                    held.setdefault(key, []).append((tick, value))
                elif kind in (0x80, 0x90) and held.get(key):
                    start, velocity = held[key].pop(0)
                    if grid:
                        tick = max(tick, start + round(grid[1] * division))
                    if down:
                        ringing[key] = (start, velocity)
                    else:
                        ended(key, start, velocity)
        for k in sorted(ringing):
            ended(k, *ringing.pop(k))
        tracks.append(sorted(notes))
    return tracks


STEPS = {"C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11}


def pitch_of(name):
    """A note's MIDI number: `C#4`, `Bb3`, `Fx4` (a double sharp); C4
    is 60."""
    semis = STEPS[name[0]]
    rest = name[1:]
    while rest[0] in "#bx":
        semis += {"#": 1, "b": -1, "x": 2}[rest[0]]
        rest = rest[1:]
    return semis + 12 * (int(rest) + 1)


SOUNDFONTS = {}


def read_soundfont(path):
    """A SoundFont 2 file's samples, and its sample headers by name."""
    if path in SOUNDFONTS:
        return SOUNDFONTS[path]
    d = np.memmap(path, dtype=np.uint8, mode="r")

    def chunks(at, end):
        while at + 8 <= end:
            size = struct.unpack("<I", bytes(d[at + 4:at + 8]))[0]
            yield bytes(d[at:at + 4]), at + 8, size
            at += 8 + size + (size & 1)

    found = {}
    for tag, at, size in chunks(12, len(d)):
        if tag == b"LIST":
            for inner, a, n in chunks(at + 4, at + size):
                found[inner] = (a, n)
    at, size = found[b"smpl"]
    samples = np.frombuffer(d, dtype="<i2", count=size // 2, offset=at)
    at, size = found[b"shdr"]
    headers = {}
    for i in range(size // 46 - 1):
        r = bytes(d[at + 46 * i:at + 46 * i + 46])
        start, end, loop_start, loop_end, rate, pitch, correction = struct.unpack("<IIIIIBb", r[20:42])
        headers[r[:20].split(b"\0")[0].decode("latin1")] = dict(
            start=start, end=end, loop_start=loop_start, loop_end=loop_end, rate=rate, pitch=pitch, correction=correction)
    SOUNDFONTS[path] = (samples, headers)
    return SOUNDFONTS[path]


def pulse(duty, n=64):
    """One period of a pulse wave, high for `duty` of it: 0.5 is a
    square, hollow; narrower is thinner and more nasal."""
    return np.where(np.arange(n) < duty * n, 1.0, -1.0)


def saw(n=64, harmonics=None):
    """One period of a sawtooth, or of its first `harmonics` partials
    only, which is rounder."""
    t = np.arange(n) / n
    if harmonics is None:
        return 1.0 - 2.0 * t
    x = sum(np.sin(2 * np.pi * k * t) / k for k in range(1, harmonics + 1))
    return x / np.abs(x).max()


def triangle(n=64):
    t = np.arange(n) / n
    return 1.0 - 4.0 * np.abs(t - 0.5)


class Sample:
    """One recorded note, cut for the module.

    `note` is the MIDI note it holds (the files' names are an octave
    off as often as not, so the pitch is measured and must agree).
    `rate` is the rate it is kept at. `loop` is the length in seconds of
    the stretch that is played round while the note is held, after
    `attack` seconds, with the last `fade` of it crossfaded into what
    is before the loop; without it the note is `length` seconds, faded
    out over its last third. `skip` starts later in the file than its
    first sound, which is where it first passes `onset` of its peak:
    more for a quiet recording with a noise before the note.

    `struck` is for a string struck and left to ring, a piano's:
    (the module's tick in seconds, a depth in dB). The recording's own
    fall up to `attack` is divided out of it, down to `depth` at most,
    and kept as `shape`, points of a volume envelope in ticks, which
    goes on down after the loop's start at the rate the recording
    falls at. So the loop is stored near full scale, where 8 bits are
    enough, and the envelope is the decay (`Instrument(damp=)`).
    """

    def __init__(self, name, path, note, rate, attack=0.25, loop=None, fade=None, length=None, skip=0.0, gain=1.0,
                 tune=True, bright=0.0, made=None, struck=None, channel=None, onset=0.02):
        self.name = name
        self.note = note
        self.loop_match = 1.0
        self.shape = None
        if made is not None:
            # Not a recording to be cut: the samples, their rate, their
            # pitch and, where they have one, their loop (`soundfont`,
            # `wave`).
            x, src, self.hz, given = made
        else:
            x, src = read_wav(fetch(path), channel)
            given = None
            peak = np.abs(x).max()
            first = int(np.argmax(np.abs(x) > peak * onset))
            x = x[max(0, first - int(0.004 * src)) + int(skip * src):]
            # The pitch, where the note has settled.
            a = int((attack if loop else 0.03) * src)
            window = x[a:a + int(0.25 * src)]
            self.hz = pitch_hz(window, src, midi_hz(note)) if tune else midi_hz(note)
            cents = 1200 * math.log2(self.hz / midi_hz(note))
            if abs(cents) > 60:
                raise ValueError(f"{path}: {self.hz:.1f} Hz is {cents:+.0f} cents from note {note}")
        if bright:
            # More of what is over 2 kHz, by so many dB: the clock's
            # buzzer and its small speaker have little under it.
            # Or (dB, Hz), for another corner than 2 kHz.
            lift, corner = bright if isinstance(bright, tuple) else (bright, 2000)
            k = math.exp(-2 * math.pi * corner / src)
            under = lfilter([1 - k], [1, -k], x)
            x = under + (x - under) * 10 ** (lift / 20)
        if struck:
            self.whole = (x, src)
            x = self.flatten(x, src, attack, *struck)
        if rate != src:
            g = math.gcd(int(rate), int(src))
            x = resample_poly(x, rate // g, src // g)
        self.rate = rate
        if given:
            # The loop as its maker set it, at the new rate; the pitch
            # goes with what rounding does to its length.
            start, end = (round(v * rate / src) for v in given)
            self.hz *= (given[1] - given[0]) * rate / src / (end - start)
            x = x[:end]
            self.loop_start, self.loop_len = start, end - start
        elif loop:
            x, self.loop_start = self.make_loop(x, rate, attack, loop, fade or min(loop / 2, 0.25))
            self.loop_len = len(x) - self.loop_start
            if struck:
                self.ring_on(x)
        elif length is None:
            self.loop_start = self.loop_len = 0
        else:
            n = min(len(x), int(length * rate))
            x = x[:n].copy()
            tail = n // 3
            x[n - tail:] *= np.linspace(1, 0, tail) ** 2
            self.loop_start = self.loop_len = 0
        x = x / np.abs(x).max() * 0.98 * gain
        # How long it is heard: to where it has fallen 20 dB under its
        # loudest, 10 ms at a time. A note cut after that is cut under
        # the attack of the note that cuts it.
        hop = max(1, rate // 100)
        level = np.array([np.abs(x[i:i + hop]).max() for i in range(0, len(x), hop)])
        loud = np.where(level > level.max() * 10 ** (-20 / 20))[0]
        self.ring = (int(loud[-1]) + 1) * hop / rate
        self.rms = float(np.sqrt((x * x).mean()))
        self.data = np.clip(np.round(x * 127), -127, 127).astype(np.int8)
        # What the instrument's relative note and finetune must add up
        # to, in semitones, for the note asked for to be the note heard.
        self.offset = 12 * math.log2(rate / C4_HZ) - (12 * math.log2(self.hz / 440.0) + 69 - 60)
        if made is not None and path is None and self.hz < 20:
            # A single period (`wave`): its rate is its length times
            # its note, whatever that comes to.
            self.hz = midi_hz(note)
            self.rate = round(len(x) * self.hz)
            self.offset = 12 * math.log2(len(x) * self.hz / C4_HZ) - (note - 60)

    @classmethod
    def soundfont(cls, name, file, sample, rate, **how):
        """A sample of a SoundFont by its name there, with its loop if it
        has one; at its own rate if `rate` is None."""
        samples, headers = read_soundfont(file)
        h = headers[sample]
        x = samples[h["start"]:h["end"]].astype(np.float64) / 32768
        hz = midi_hz(h["pitch"]) * 2 ** (-h["correction"] / 1200)
        loop = (h["loop_start"] - h["start"], h["loop_end"] - h["start"])
        if loop[1] - loop[0] < 8:
            loop = None
            how.setdefault("length", len(x) / h["rate"])
        return cls(name, None, h["pitch"], rate or h["rate"], made=(x, h["rate"], hz, loop), **how)

    @classmethod
    def wave(cls, name, cycle, note, **how):
        """One period of a wave, played round: `cycle` is its samples,
        and the rate is whatever makes that period `note`."""
        x = np.asarray(cycle, dtype=np.float64)
        return cls(name, None, note, len(x), made=(x, len(x), 1.0, (0, len(x))), **how)

    @classmethod
    def noise(cls, name, seconds, rate, decay, note=60, seed=1):
        """Noise that dies away, to a thousandth in `decay` seconds: a
        cymbal for a tune of plain waves. It plays as made at `note`."""
        n = int(seconds * rate)
        x = np.random.default_rng(seed).uniform(-1, 1, n) * np.exp(-6.9 * np.arange(n) / rate / decay)
        return cls(name, None, note, rate, made=(x, rate, midi_hz(note), None))

    @classmethod
    def pluck(cls, name, cycle, note, seconds, decay, rate):
        """A wave struck and left to die away, to a thousandth in
        `decay` seconds: a plucked string or a drum of plain waves."""
        cycle = np.asarray(cycle, dtype=np.float64)
        n = int(seconds * rate)
        phase = (np.arange(n) * midi_hz(note) / rate) % 1.0 * len(cycle)
        x = np.interp(phase, np.arange(len(cycle) + 1), np.append(cycle, cycle[0]))
        x *= np.exp(-6.9 * np.arange(n) / rate / decay)
        return cls(name, None, note, rate, made=(x, rate, midi_hz(note), None))

    def flatten(self, x, rate, attack, tick, depth):
        """Take the recording's fall to `attack` out of it and into
        `shape`; see the class. What follows the loop's start is
        `ring_on`'s, once there is a loop to measure."""
        hop = max(1, int(0.005 * rate))
        half = int(0.015 * rate)
        power = np.concatenate(([0.0], np.cumsum(x * x)))
        centres = np.arange(0, len(x), hop)
        lo, hi = np.maximum(centres - half, 0), np.minimum(centres + half, len(x))
        db = 10 * np.log10((power[hi] - power[lo]) / (hi - lo) + 1e-12)
        top = int(np.argmax(db[:int(0.2 * rate / hop)]))
        # The level, 5 ms at a time, from its top and never rising: a
        # piano's strings beat, and an envelope cannot follow that.
        loudest = float(db[top])
        db = np.minimum.accumulate(np.where(np.arange(len(db)) < top, loudest, db)) - loudest

        def at(seconds):
            return float(db[min(len(db) - 1, int(seconds * rate / hop))])

        # How fast it dies once it has settled, in dB a second.
        a, b = int(attack * rate / hop), min(len(db), int((attack + 2.0) * rate / hop))
        slope = -float(np.polyfit(np.arange(a, b) * hop / rate, db[a:b], 1)[0])
        self.fall = min(max(slope, 3.0), 20.0)
        # The share of the fall the envelope takes while the recording
        # lasts: all of it, if that is no deeper than `depth`.
        share = min(1.0, depth / max(-at(attack), 1e-6))
        first = max(1, round(top * hop / rate / tick))
        last = round(attack / tick)
        ticks = sorted({first + round((last - first) * f) for f in (0.0, 0.1, 0.22, 0.4, 0.65, 1.0)})
        points = [(0, 64)]
        for t in ticks:
            level = max(1, min(points[-1][1], round(64 * 10 ** (share * at(t * tick) / 20))))
            points.append((t, level))
        gain = np.interp(np.arange(len(x)) / rate / tick, [p[0] for p in points], [p[1] / 64 for p in points])
        self.shape, self.tick = points, tick
        self.decay = (db, hop / rate / tick, loudest, float(db[int(0.2 * rate / hop):].min()) + 10)
        return x / gain

    def ring_on(self, x):
        """The envelope after the loop's start: the level the recording
        has at each moment, over the level the loop is kept at. Its
        points are where that halves, by the recording for as far as it
        is above its noise and then at the rate it was falling at."""
        db, ticks, loudest, floor = self.decay
        loop = 10 * math.log10(float((x[self.loop_start:] ** 2).mean()) + 1e-12) - loudest
        points = self.shape
        t = float(points[-1][0])
        was = float(db[min(len(db) - 1, int(t / ticks))])
        # A couple of ticks for the step from what the envelope was at
        # to what the loop asks for: the loop is levelled, and is not
        # quite as loud as the stretch before it.
        level = max(1, min(64, round(64 * 10 ** ((was - loop) / 20))))
        t += 3
        points.append((round(t), level))
        while len(points) < 12:
            level = level // 2 if len(points) < 11 else 0
            want = loop + 20 * math.log10(max(level, 0.5) / 64)
            under = np.nonzero(db <= want)[0]
            if want > floor and len(under) and under[0] * ticks > t:
                t = under[0] * ticks
            else:
                t += max(1.0, (was - want) / self.fall / self.tick)
            was = want
            points.append((round(t), level))
            if not level:
                break

    def make_loop(self, x, rate, attack, loop, fade):
        s = int(attack * rate)
        f = int(fade * rate)
        assert s >= f, f"{self.name}: the attack is shorter than the crossfade"
        want = s + int(loop * rate)
        period = rate / self.hz
        reach = int(max(period * 4, 0.02 * rate))
        if want + reach + 256 > len(x):
            raise ValueError(f"{self.name}: the file is too short for this loop")
        # The end that continues best into the start: the stretch
        # before each candidate against the stretch before the start.
        w = int(max(period * 8, 0.02 * rate))
        w = min(w, s)
        ref = x[s - w:s]
        best, end = -2.0, want
        for e in range(want - reach, want + reach):
            seg = x[e - w:e]
            c = float(ref @ seg) / (math.sqrt(float(ref @ ref) * float(seg @ seg)) + 1e-12)
            if c > best:
                best, end = c, e
        self.loop_match = best
        y = x[:end].copy()
        # Level the loop, so that going round is not a step in loudness.
        def level(at):
            seg = x[at - w:at]
            return math.sqrt(float(seg @ seg) / len(seg))
        ratio = level(s) / (level(end) + 1e-12)
        ratio = min(max(ratio, 0.5), 2.0)
        y[s:end] *= np.linspace(1.0, ratio, end - s)
        ramp = np.linspace(0.0, 1.0, f)
        # Equal power where the two stretches are not alike, equal gain
        # where they are.
        if best > 0.9:
            y[end - f:end] = y[end - f:end] * (1 - ramp) + x[s - f:s] * ramp
        else:
            y[end - f:end] = y[end - f:end] * np.cos(ramp * math.pi / 2) + x[s - f:s] * np.sin(ramp * math.pi / 2)
        return y, s


class Instrument:
    """A sample with how it is let go: `release` is the ticks a held
    note takes to die away after its key-off (0 for a note that is
    never held: it rings to its end), `rise` the ticks it takes to
    come up, for a sample cut from the middle of a note. `vibrato` is the instrument's own,
    (sweep, depth, rate) as FastTracker has them. `damp` is for a
    `struck` sample: its envelope is the sample's `shape`, which runs
    to nothing whether the key is held or not, and a key-off is the
    damper, the ticks the note takes to fade."""

    def __init__(self, sample, release=0, rise=0, volume=64, vibrato=None, transpose=0, fall=None, damp=0):
        assert not damp or (sample.shape and not release), f"{sample.name}: damp is for a struck sample"
        self.sample = sample
        self.release = release
        self.damp = damp
        self.rise = rise
        # (ticks, level): a held note falls to `level` of 64 over
        # `ticks` and stays there, as a plucked or struck one would.
        self.fall = fall
        self.volume = volume
        self.vibrato = vibrato or (0, 0, 0)
        self.transpose = transpose
        self.number = 0

    def tuning(self):
        semis = self.sample.offset + self.transpose
        rel = math.floor(semis + 0.5)
        fine = round((semis - rel) * 128)
        if fine > 127:
            rel, fine = rel + 1, fine - 128
        return rel, fine


class Module:
    """The tune: rows of cells for `channels` channels, at `speed` ticks
    a row and `bpm` (a tick is 2.5 / bpm seconds)."""

    def __init__(self, title, channels, speed=6, bpm=125, pattern_rows=64):
        self.title = title
        self.channels = channels + 1  # the last is the tempo's
        self.speed = speed
        self.bpm = bpm
        self.pattern_rows = pattern_rows
        self.instruments = []
        self.cells = {}  # (row, channel) -> [note, instrument, volume, effect, param]
        self.rows = 0
        self.polys = []
        self.tempos = []

    def instrument(self, sample, **how):
        ins = Instrument(sample, **how)
        self.instruments.append(ins)
        ins.number = len(self.instruments)
        return ins

    def cell(self, row, channel):
        self.rows = max(self.rows, row + 1)
        return self.cells.setdefault((row, channel), [0, 0, 0, 0, 0])

    def tempo(self, row, bpm):
        c = self.cell(int(row), self.channels - 1)
        assert c[3] == 0, f"two tempos on row {row}"
        c[3], c[4] = 0xF, max(32, min(255, round(bpm)))
        self.tempos.append((int(row), c[4]))
        self.tempos.sort()

    def voices(self, channels):
        assert all(0 <= c < self.channels - 1 for c in channels)
        v = Voices(self, channels)
        self.polys.append(v)
        return v

    def row_seconds(self, row):
        """How long a row is at `row`, by the tempos set so far: set
        the tempo before the notes."""
        bpm = self.bpm
        for at, value in self.tempos:
            if at <= row:
                bpm = value
        return self.speed * 2.5 / bpm

    def length(self, rows):
        self.rows = max(self.rows, int(math.ceil(rows)))

    # The file.

    def write(self, path):
        # The last pattern is as long as what is left of the tune.
        patterns, orders, seen = [], [], {}
        for first in range(0, self.rows, self.pattern_rows):
            count = min(self.pattern_rows, self.rows - first)
            data = bytearray()
            for row in range(first, first + count):
                for ch in range(self.channels):
                    c = self.cells.get((row, ch))
                    if not c or not any(c):
                        data.append(0x80)
                        continue
                    flags = 0x80 | (1 if c[0] else 0) | (2 if c[1] else 0) | (4 if c[2] else 0) | (8 if c[3] else 0) | (16 if c[3] or c[4] else 0)
                    data.append(flags)
                    if c[0]:
                        data.append(c[0])
                    if c[1]:
                        data.append(c[1])
                    if c[2]:
                        data.append(c[2])
                    if c[3]:
                        data.append(c[3])
                    if c[3] or c[4]:
                        data.append(c[4])
            data = bytes(data)
            if (count, data) not in seen:
                seen[count, data] = len(patterns)
                patterns.append((count, data))
            orders.append(seen[count, data])
        assert len(orders) <= 256 and len(patterns) <= 256
        out = bytearray()
        out += b"Extended Module: " + self.title.encode("ascii")[:20].ljust(20) + b"\x1a"
        out += b"onebitdawn mkxm.py".ljust(20) + struct.pack("<H", 0x0104)
        out += struct.pack("<IHHHHHHHH", 276, len(orders), 0, self.channels, len(patterns), len(self.instruments), 1, self.speed, self.bpm)
        out += bytes(orders).ljust(256, b"\0")
        for count, data in patterns:
            out += struct.pack("<IBHH", 9, 0, count, len(data)) + data
        for ins in self.instruments:
            s = ins.sample
            rel, fine = ins.tuning()
            assert -96 <= rel <= 95, f"{s.name}: relative note {rel}"
            head = bytearray()
            head += struct.pack("<I", 263) + s.name.encode("ascii")[:22].ljust(22) + b"\0" + struct.pack("<H", 1)
            head += struct.pack("<I", 40) + bytes(96)
            if ins.damp:
                # No sustain point: the envelope is the string dying
                # away, and the key-off's fade the damper.
                points, sustain, kind, fadeout = s.shape, 0, 1, min(0xFFF, round(32768 / ins.damp))
            elif ins.release:
                # Held at full level; the key-off lets the line down.
                points = [(0, 0), (ins.rise, 64)] if ins.rise else [(0, 64)]
                if ins.fall:
                    points.append((points[-1][0] + ins.fall[0], ins.fall[1]))
                sustain = len(points) - 1
                points.append((points[-1][0] + ins.release, 0))
                kind, fadeout = 3, 0
            else:
                points, sustain, kind, fadeout = [(0, 64), (1, 64)], 0, 0, 0
            env = b"".join(struct.pack("<HH", *p) for p in points).ljust(48, b"\0")
            head += env + bytes(48)
            head += bytes([len(points), 0, sustain, 0, 0, 0, 0, 0, kind, 0])
            sweep, depth, rate = ins.vibrato
            head += bytes([0, sweep, depth, rate]) + struct.pack("<HH", fadeout, 0)
            out += head.ljust(263, b"\0")
            looped = 1 if s.loop_len else 0
            out += struct.pack("<IIIBbBBbB", len(s.data), s.loop_start, s.loop_len, ins.volume, fine, looped, 128, rel, 0)
            out += s.name.encode("ascii")[:22].ljust(22)
            delta = np.diff(s.data.astype(np.int16), prepend=0).astype(np.int8)
            out += delta.tobytes()
        with open(path, "wb") as f:
            f.write(out)
        return len(out), len(patterns), len(orders)

    def seconds(self):
        """How long the tune is, by its rows and tempos."""
        bpm, total = self.bpm, 0.0
        for row in range(self.rows):
            c = self.cells.get((row, self.channels - 1))
            if c and c[3] == 0xF:
                bpm = c[4]
            total += self.speed * 2.5 / bpm
        return total


class Line:
    """One channel's cells. A row is `speed` ticks, and positions and
    lengths in rows may be fractions."""

    def __init__(self, module, channel):
        self.m = module
        self.channel = channel
        self.started = -1

    def split(self, at):
        tick = round(at * self.m.speed)
        return tick // self.m.speed, tick % self.m.speed

    def note(self, at, length, pitch, ins, vol, slur):
        row, tick = self.split(at)
        c = self.m.cell(row, self.channel)
        if c[0] == KEY_OFF and c[3] == 0:
            c[0] = 0
        assert c[0] == 0 and c[3] == 0, f"channel {self.channel}, row {row}: two notes"
        note = pitch - 11
        assert 1 <= note <= 96, f"note {pitch} out of range"
        c[0] = note
        c[2] = 0x10 + max(1, min(64, round(vol)))
        if slur:
            assert tick == 0, "a slurred note starts on its row"
            c[3], c[4] = 3, 0xFF
        else:
            c[1] = ins.number
            if tick:
                c[3], c[4] = 0xE, 0xD0 | tick
        self.started = row
        if ins.release or ins.damp:
            self.off(at + length)

    def off(self, at):
        row, tick = self.split(at)
        c = self.m.cell(row, self.channel)
        if row == self.started:
            if c[3] or not tick:
                # The note began late in this row and its cell has that
                # to say: it is let go on the next.
                row, tick = row + 1, 0
                c = self.m.cell(row, self.channel)
            else:
                # Let go in the row it began on, by its own cell.
                c[3], c[4] = 0x14, tick
                return
        if c[0] or c[3]:
            return  # a note is already here, the tie's or the next
        if tick:
            c[3], c[4] = 0x14, tick
        else:
            c[0] = KEY_OFF

    def vol(self, at, vol):
        row, _ = self.split(at)
        c = self.m.cell(row, self.channel)
        if c[2] == 0 and c[0] != KEY_OFF:
            c[2] = 0x10 + max(1, min(64, round(vol)))

    def taken(self, at):
        c = self.m.cells.get((self.split(at)[0], self.channel))
        return bool(c and (c[0] not in (0, KEY_OFF) or c[3]))


class Voices:
    """A few channels that share notes. The player has no ramp: a note
    that starts on a channel cuts what was sounding there, with a
    click. So a note goes to a channel that has gone quiet, the one
    quiet longest; failing that it is slurred on from a note of its
    instrument that ends as it starts; failing that it cuts the note
    nearest its end, and the module's report counts it."""

    def __init__(self, module, channels):
        self.m = module
        self.lines = [Line(module, c) for c in channels]
        # For each channel its notes: (start, the held note's end, the
        # end of its release or of a struck note's ring, instrument).
        self.notes = [[] for _ in channels]
        self.last = 0
        self.dropped = []
        self.cuts = 0
        self.cut_at = []

    def span(self, at, length, pitch, ins):
        rows = self.m.row_seconds(at)
        if ins.release or ins.damp:
            # The channel is the note's to its end, and to the row
            # after if it is let go in the middle of one.
            end = math.ceil(round((at + length) * self.m.speed) / self.m.speed - 1e-9)
            return end, end + (ins.release or ins.damp) / self.m.speed
        ring = ins.sample.ring * midi_hz(ins.sample.note) / midi_hz(pitch + ins.transpose)
        return at + min(length, 1.0), at + ring / rows

    def before(self, i, at):
        """The channel's note that starts last at or before `at`."""
        found = None
        for note in self.notes[i]:
            if note[0] <= at + 1e-9 and (found is None or note[0] > found[0]):
                found = note
        return found

    def note(self, at, length, pitch, ins, vol=48, slur=False, legato=False):
        """`pitch` is a MIDI note. `slur` moves this voice's last note
        to this pitch with no new attack; `legato` lets the note be
        slurred on from any note of its instrument that ends here, when
        no channel is quiet."""
        at = round(at * self.m.speed) / self.m.speed
        whole = at == math.floor(at)
        end, quiet = self.span(at, length, pitch, ins)

        def ends_here(i):
            prev = self.before(i, at)
            return whole and prev is not None and prev[3] is ins and ins.release and abs(prev[1] - at) < 1e-9

        def free(i):
            if self.lines[i].taken(at):
                return False
            return all(not (n[0] < end - 1e-9 and n[1] > at + 1e-9) for n in self.notes[i])

        def quiet_at(i):
            prev = self.before(i, at)
            return prev[2] if prev else -1.0

        if slur and ends_here(self.last) and free(self.last):
            return self.put(self.last, at, length, pitch, ins, vol, True, end, quiet)
        can = [i for i in range(len(self.lines)) if free(i)]
        if not can:
            self.dropped.append((at, pitch, ins.sample.name))
            return False
        silent = [i for i in can if quiet_at(i) <= at + 1e-9]
        if silent:
            return self.put(min(silent, key=quiet_at), at, length, pitch, ins, vol, False, end, quiet)
        if legato:
            for i in can:
                if ends_here(i):
                    return self.put(i, at, length, pitch, ins, vol, True, end, quiet)
        self.cuts += 1
        self.cut_at.append((at, ins.sample.name))
        return self.put(min(can, key=quiet_at), at, length, pitch, ins, vol, False, end, quiet)

    def put(self, i, at, length, pitch, ins, vol, slur, end, quiet):
        self.lines[i].note(at, length, pitch, ins, vol, slur)
        self.notes[i].append((at, end, quiet, ins))
        self.last = i
        return True

    def chord(self, at, length, notes, ins, vol=48):
        for name in notes.split():
            self.note(at, length, pitch_of(name), ins, vol, legato=True)

    def vol(self, at, vol):
        """The level of what is sounding, from this row."""
        for line in self.lines:
            line.vol(at, vol)

    def phrase(self, at, notes, ins, vol=48, legato=True, graces=None, detach=0.0):
        """Notes one after another from row `at`: `B5/2` is a note of
        two rows, `r/4` a rest, `(G#5 A5)` grace notes before the note
        that follows, a third of a row each and taken from the time
        before it, `v40` the level from here on. With `legato` a note
        that follows a note is slurred to it. Grace notes are played by
        `graces`, other voices, or by these. `detach` shortens each
        note by that many rows. Returns the row after the last."""
        pending, tied = [], False
        for token in notes.replace("(", " ( ").replace(")", " ) ").split():
            if token == "(":
                pending = [None]
            elif token == ")":
                pending = pending[1:]
            elif pending and pending[0] is None:
                pending.append(pitch_of(token))
            elif token[0] == "v" and token[1:].isdigit():
                vol = int(token[1:])
            else:
                name, length = token.split("/")
                length = float(length)
                if name == "r":
                    tied = False
                else:
                    last = self.last
                    for i, grace in enumerate(pending):
                        (graces or self).note(at - (len(pending) - i) / 3, 1 / 3, grace, ins, vol * 3 // 4)
                    if not graces:
                        self.last = last
                    self.note(at, length - detach, pitch_of(name), ins, vol, slur=legato and tied)
                    tied = legato
                    pending = []
                at += length
        return at


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    spec = importlib.util.spec_from_file_location("tune", sys.argv[1])
    assert spec and spec.loader
    tune = importlib.util.module_from_spec(spec)
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    sys.modules.setdefault("mkxm", sys.modules[__name__])
    spec.loader.exec_module(tune)
    m = tune.tune(*sys.argv[3:])
    size, patterns, orders = m.write(sys.argv[2])
    samples = sum(len(i.sample.data) for i in m.instruments)
    print(f"{sys.argv[2]}: {size} bytes, {samples} of samples in {len(m.instruments)} instruments, "
          f"{patterns} patterns in {orders} orders, {m.channels} channels, {m.seconds():.1f} s")
    for p in m.polys:
        channels = ",".join(str(line.channel) for line in p.lines)
        if p.dropped:
            where = ", ".join(f"{name} {pitch} at row {at:g}" for at, pitch, name in p.dropped[:8])
            print(f"  channels {channels}: {len(p.dropped)} notes had no channel: {where}")
        if p.cuts:
            where = ", ".join(f"{name} at row {at:g}" for at, name in p.cut_at[:6])
            print(f"  channels {channels}: {p.cuts} notes cut one still sounding: {where}")
    for i in m.instruments:
        s = i.sample
        rel, fine = i.tuning()
        loop = f"loop {s.loop_len} ({s.loop_match:.3f})" if s.loop_len else "no loop"
        print(f"  {i.number:2} {s.name:12} {len(s.data):6} at {s.rate}, {s.hz:7.1f} Hz, {loop}, rms {s.rms:.2f}, rel {rel} fine {fine}")
        if s.shape:
            print("     falls " + " ".join(f"{t}:{level}" for t, level in s.shape) + f", then {s.fall:.1f} dB a second")


if __name__ == "__main__":
    main()

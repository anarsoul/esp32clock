#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Convert a ProTracker module (.mod) into a FastTracker 2 one (.xm),
which scripts/xm2clip.py takes.

    scripts/mod2xm.py tune.mod tune.xm ['Title of 20 bytes']

A sample becomes an instrument of one sample under its own name, the
finetune kept; a period becomes the nearest note, 428 being C-4 and
the pitch a PAL Amiga's, 16 cents under FT2's. The
module is written on the linear frequency table, which is what
core/src/xm.rs plays, so the slides, which a MOD counts in periods,
are converted at the period the channel has when each is played, the
song followed once in its order: 1, 2, 3 and 5, and E1 and E2. The
effects 0, 4, 6, 7, 9, A, B, C, D, F and the other E's are the same in
both; E0, the Amiga's filter, is left out and named.
Not converted: the panning, a module being mono on the clock, and a
pattern loop's or a delay's part in the order the slides are read in.
"""
import math, struct, sys

if len(sys.argv) not in (3, 4):
    sys.exit(__doc__)
d = open(sys.argv[1], 'rb').read()
tag = d[1080:1084]
if tag in (b'M.K.', b'M!K!', b'FLT4', b'4CHN'):
    nch = 4
elif tag[1:] == b'CHN' and tag[:1].isdigit():
    nch = int(tag[:1])
elif tag[2:] == b'CH' and tag[:2].isdigit():
    nch = int(tag[:2])
else:
    sys.exit('not a module of 31 samples (%r)' % tag)
title = d[:20].split(b'\0')[0]
if len(sys.argv) > 3:
    title = sys.argv[3].encode()
orders = list(d[952:952 + d[950]])
patn = max(d[952:1080]) + 1

# patterns as [row][channel] = [period, ins, fx, param]
pats = []
for p in range(patn):
    rows = []
    for r in range(64):
        row = []
        for c in range(nch):
            a, b, e, par = d[1084 + ((p * 64 + r) * nch + c) * 4:][:4]
            row.append([(a & 15) << 8 | b, (a & 0xf0) | e >> 4, e & 15, par])
        rows.append(row)
    pats.append(rows)

# The slides, in sixteenths of a semitone a tick for periods a tick,
# at the period the channel is at; a pattern played again keeps what
# its first time gave it.
def linear(period, to, ticks):
    """A slide from one period to another over that many ticks."""
    if ticks <= 0 or period == to:
        return 0
    return max(1, min(255, round(abs(12 * 16 * math.log2(period / to)) / ticks)))

speed = 6
period = [0] * nch
target = [0] * nch
porta = [0] * nch
seen = set()
fixed = {}
o = 0
row0 = 0
visited = set()
while o < len(orders) and (o, row0) not in visited:
    visited.add((o, row0))
    p = orders[o]
    first = p not in seen
    seen.add(p)
    jump = None
    for r in range(row0, 64):
        for c, cell in enumerate(pats[p][r]):
            per, ins, fx, par = cell
            if fx == 0xF and 0 < par < 0x20:
                speed = par
        for c, cell in enumerate(pats[p][r]):
            per, ins, fx, par = cell
            ticks = speed - 1
            new = None
            if fx in (3, 5):
                if per: target[c] = per
                if fx == 3 and par: porta[c] = par
                if period[c] and target[c]:
                    step = porta[c] * ticks
                    to = max(target[c], period[c] - step) if target[c] < period[c] else min(target[c], period[c] + step)
                    if fx == 3: new = linear(period[c], to, ticks) if par else 0
                    period[c] = to
            else:
                if per: period[c] = target[c] = per
                if fx in (1, 2) and period[c]:
                    to = period[c] - par * ticks if fx == 1 else period[c] + par * ticks
                    to = max(113, min(856, to))
                    new = linear(period[c], to, ticks)
                    period[c] = to
                elif fx == 0xE and par >> 4 in (1, 2) and period[c]:
                    to = period[c] - (par & 15) if par >> 4 == 1 else period[c] + (par & 15)
                    to = max(113, min(856, to))
                    new = par & 0xf0 | min(15, linear(period[c], to, 1))
                    period[c] = to
            if new is not None and first:
                fixed[(p, r, c)] = new
            if fx == 0xB: jump = (par, 0)
            elif fx == 0xD: jump = (jump[0] if jump else o + 1, (par >> 4) * 10 + (par & 15))
        if jump: break
    o, row0 = jump if jump else (o + 1, 0)
    if row0 > 63: row0 = 0

# FT2 plays C-4 at 8363 Hz, a PAL Amiga period 428 at 7093789.2 / 856:
# 16 cents under, in the finetune's 128ths of a semitone.
PAL = round(12 * 128 * math.log2(7093789.2 / 856 / 8363))

unhandled = set()
def convert(p, r, c, cell):
    per, ins, fx, par = cell
    xn = round(12 * math.log2(428 / per)) + 49 if per else 0
    if (p, r, c) in fixed:
        par = fixed[(p, r, c)]
    if fx == 0xE and par >> 4 == 0:
        unhandled.add('E0'); fx = par = 0
    elif fx == 0xC:
        par = min(par, 64)
    elif fx == 0xF and par == 0:
        fx = 0                                  # F00 stops a MOD; FT2 has no such
    return xn, ins, 0, fx, par

def pack(p, rows):
    out = bytearray()
    for r, row in enumerate(rows):
        for c, cell in enumerate(row):
            v = convert(p, r, c, cell)
            m = sum(1 << i for i in range(5) if v[i])
            if m == 31: out += bytes(v)
            else:
                out.append(0x80 | m)
                out += bytes(x for x in v if x)
    return bytes(out)

restart = d[951] if d[951] < len(orders) else 0
out = bytearray()
out += b'Extended Module: ' + title[:20].ljust(20) + b'\x1a' + b'mod2xm.py'.ljust(20)
out += struct.pack('<HI8H', 0x0104, 276, len(orders), restart, nch, patn, 31, 1, 6, 125)  # linear table: it is what xm.rs plays
out += bytes(orders).ljust(256, b'\0')
for p, rows in enumerate(pats):
    data = pack(p, rows)
    out += struct.pack('<IBHH', 9, 0, 64, len(data)) + data

at = 1084 + patn * 64 * nch * 4
for i in range(31):
    h = 20 + 30 * i
    name = d[h:h + 22].split(b'\0')[0].ljust(22)
    words, fine, vol, lb, ll = struct.unpack_from('>HBBHH', d, h + 22)
    ln, lb, ll = words * 2, lb * 2, ll * 2
    pcm = d[at:at + ln]
    at += ln
    if len(pcm) < 4:
        out += struct.pack('<I22sBH', 29, name, 0, 0)
        continue
    loop = ll > 2 and lb < len(pcm)
    if loop: pcm = pcm[:lb + ll]
    delta = bytearray(); prev = 0
    for b in pcm:
        delta.append((b - prev) & 255); prev = b
    fine = ((fine & 15) ^ 8) - 8                # a signed nibble, eighths of a semitone
    out += struct.pack('<I22sBH', 263, name, 0, 1)
    out += struct.pack('<I', 40) + bytes(96) + bytes(96) + bytes(2 + 6 + 2 + 4 + 2 + 22)
    out += struct.pack('<3IBbBBbB22s', len(pcm), lb if loop else 0, len(pcm) - lb if loop else 0,
                       min(vol, 64), fine * 16 + PAL, 1 if loop else 0, 128, 0, 0, name)
    out += delta
    print('%2d len %5d loop %s vol %2d fine %+d' % (i + 1, len(pcm), (lb, len(pcm)) if loop else '-', vol, fine))

open(sys.argv[2], 'wb').write(out)
print('channels', nch, 'orders', len(orders), 'patterns', patn, 'slides converted', len(fixed), 'bytes', len(out))
if unhandled: print('NOT CONVERTED effects:', sorted(unhandled))

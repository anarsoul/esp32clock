#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Convert a ScreamTracker 3 module (.s3m) into a FastTracker 2 one (.xm),
which scripts/xm2clip.py takes.

    scripts/s3m2xm.py tune.s3m tune.xm ['Title of 20 bytes']

A sample becomes an instrument of one sample, its C2Spd the relative
note and the finetune; the module is written on the linear frequency
table, which is what core/src/xm.rs plays. The effects converted are
A, B, C, D, O and T (D00's memory resolved in playing order, since FT2
keeps the fine slides' apart); any other is left out and named.
Not converted: AdLib instruments, 16-bit, stereo and packed samples,
fast volume slides (refused), and the panning, a module being mono on
the clock.
"""
import math, struct, sys

if len(sys.argv) not in (3, 4):
    sys.exit(__doc__)
d = open(sys.argv[1], 'rb').read()
if d[0x2c:0x30] != b'SCRM':
    sys.exit('not an S3M module')
title = d[:28].split(b'\0')[0]
if len(sys.argv) > 3:
    title = sys.argv[3].encode()
ordn, insn, patn, flags, cwt, ffi = struct.unpack_from('<6H', d, 0x20)
gv, speed, tempo, mv = struct.unpack_from('<4B', d, 0x30)
chset = d[0x40:0x60]
nch = max(i for i, c in enumerate(chset) if c < 16) + 1   # PCM channels only
nch += nch & 1
orders = [o for o in d[0x60:0x60 + ordn] if o < 254]
ip = struct.unpack_from('<%dH' % insn, d, 0x60 + ordn)
pp = struct.unpack_from('<%dH' % patn, d, 0x60 + ordn + insn * 2)
fast_slides = cwt == 0x1300 or flags & 0x40
if fast_slides:
    sys.exit('fast volume slides: not handled')

# patterns as [row][channel] = [note, ins, vol, fx, param] in S3M terms
pats = []
for p in pp:
    rows = [[[255, 0, 255, 0, 0] for _ in range(nch)] for _ in range(64)]
    if p:
        q = p * 16 + 2
        r = 0
        while r < 64:
            b = d[q]; q += 1
            if b == 0:
                r += 1; continue
            c = b & 31
            cell = [255, 0, 255, 0, 0]
            if b & 32: cell[0], cell[1] = d[q], d[q + 1]; q += 2
            if b & 64: cell[2] = d[q]; q += 1
            if b & 128: cell[3], cell[4] = d[q], d[q + 1]; q += 2
            if c < nch: rows[r][c] = cell
    pats.append(rows)

# D00 repeats the channel's last slide: resolve it in playing order
mem = [0] * nch
seen = set()
for o in orders:
    first = o not in seen
    seen.add(o)
    for row in pats[o]:
        for c, cell in enumerate(row):
            if cell[3] == 4:
                if cell[4]: mem[c] = cell[4]
                elif first: cell[4] = mem[c]

unhandled = set()
def convert(cell):
    note, ins, vol, fx, par = cell
    xn = 0
    if note == 254: xn = 97
    elif note < 254: xn = (note >> 4) * 12 + (note & 15) + 1
    xv = 0x10 + min(vol, 64) if vol != 255 else 0
    xf = xp = 0
    L = chr(64 + fx) if fx else ''
    if L == 'A': xf, xp = 0xF, min(par, 0x1F)
    elif L == 'T': xf, xp = 0xF, max(par, 0x20)
    elif L == 'B': xf, xp = 0xB, par
    elif L == 'C': xf, xp = 0xD, par
    elif L == 'O': xf, xp = 0x9, par
    elif L == 'D':
        x, y = par >> 4, par & 15
        if y == 0: xf, xp = 0xA, par            # up
        elif x == 0: xf, xp = 0xA, par          # down
        elif y == 15: xf, xp = 0xE, 0xA0 | x    # fine up
        elif x == 15: xf, xp = 0xE, 0xB0 | y    # fine down
        else: xf, xp = 0xA, y                   # ST3 takes the low nibble
    elif L: unhandled.add(L)
    return xn, ins, xv, xf, xp

def pack(rows):
    out = bytearray()
    for row in rows:
        for cell in row:
            v = convert(cell)
            m = sum(1 << i for i in range(5) if v[i])
            if m == 31: out += bytes(v)
            else:
                out.append(0x80 | m)
                out += bytes(x for x in v if x)
    return bytes(out)

out = bytearray()
out += b'Extended Module: ' + title[:20].ljust(20) + b'\x1a' + b's3m2xm.py'.ljust(20)
out += struct.pack('<HI8H', 0x0104, 276, len(orders), 0, nch, patn, insn, 1, speed, tempo)  # linear table: no slides, and it is what xm.rs plays
out += bytes(orders).ljust(256, b'\0')
for rows in pats:
    data = pack(rows)
    out += struct.pack('<IBHH', 9, 0, 64, len(data)) + data

for i, p in enumerate(ip):
    o = p * 16
    typ = d[o]
    name = d[o + 0x30:o + 0x4c].split(b'\0')[0][:22].ljust(22)
    ln, lb, le = struct.unpack_from('<3I', d, o + 0x10)
    vol, _, packed, fl = struct.unpack_from('<4B', d, o + 0x1c)
    c2, = struct.unpack_from('<I', d, o + 0x20)
    if typ != 1 or ln == 0:
        out += struct.pack('<I22sBH', 29, name, 0, 0)
        continue
    if packed or fl & 6:
        sys.exit('sample %d is packed, stereo or 16-bit' % (i + 1))
    seg = ((d[o + 0x0d] << 16) | struct.unpack_from('<H', d, o + 0x0e)[0]) * 16
    pcm = d[seg:seg + ln]
    if ffi == 2: pcm = bytes(b ^ 0x80 for b in pcm)
    loop = fl & 1 and le > lb
    if loop: pcm = pcm[:le]
    delta = bytearray(); prev = 0
    for b in pcm:
        delta.append((b - prev) & 255); prev = b
    st = round(12 * 128 * math.log2(c2 / 8363))
    rel = (st + 64) >> 7
    fine = st - rel * 128
    out += struct.pack('<I22sBH', 263, name, 0, 1)
    out += struct.pack('<I', 40) + bytes(96) + bytes(96) + bytes(2 + 6 + 2 + 4 + 2 + 22)
    out += struct.pack('<3IBbBBbB22s', len(pcm), lb if loop else 0, le - lb if loop else 0,
                       min(vol, 64), fine, 1 if loop else 0, 128, rel, 0, name)
    out += delta
    print('%2d len %5d loop %s vol %2d c2spd %5d -> rel %+d fine %+d' % (i + 1, len(pcm), (lb, le) if loop else '-', vol, c2, rel, fine))

open(sys.argv[2], 'wb').write(out)
print('channels', nch, 'orders', len(orders), 'patterns', patn, 'instruments', insn, 'bytes', len(out))
if unhandled: print('NOT CONVERTED effects:', sorted(unhandled))

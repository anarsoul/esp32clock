# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Pack a clip that is a stream of items: a stretch that has been played
before is written as a reference to it (core/src/refs.rs).

scripts/zxbeeper.py (a beeper clip's runs) and scripts/sid2clip.py (a
SID tune's register writes and waits) use it, each with its own tokens.
"""
from collections import defaultdict

DEPTH = 16  # refs::DEPTH
CANDIDATES = 256  # earlier places tried for each item


def leb128(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        out.append(byte | (0x80 if value else 0))
        if not value:
            return bytes(out)


def read_leb128(data, at):
    value = shift = 0
    while True:
        byte = data[at]
        at += 1
        value |= (byte & 0x7F) << shift
        shift += 7
        if not byte & 0x80:
            return value, at


def pack(items, literal, reference, min_ref, apart=()):
    """The items as tokens, and where each item of `apart` landed.

    `literal(item)` is an item's own token and `reference(count, back)`
    the token for `count` items decoded from the token `back` bytes
    before it, `min_ref` items at least. An item of `apart` (by its
    index) starts a token outside any reference, for a place the clip
    is entered at, as where a tune starts over.

    A reference is to a token's start, so the places an item can refer
    to are those where this packer itself began a token; the newest
    `CANDIDATES` that start with the same `min_ref` items are tried,
    and the one that saves most bytes is taken. An item is left as it
    is if the reference from the next one saves more.
    """
    n = len(items)
    out = bytearray()
    cost = [0] * (n + 1)  # bytes of the items before each, unpacked
    for k, item in enumerate(items):
        cost[k + 1] = cost[k] + len(literal(item))
    starts = defaultdict(list)  # min_ref items -> where a token starts with them
    offset = {}  # the item a token starts at -> the token's place in `out`
    depth = [0] * n  # references to go through to reach each item
    apart = set(apart)

    def best(i):
        """(bytes saved, items, the item referred to) for a reference at item i."""
        found = (0, 0, 0)
        for j in reversed(starts[tuple(items[i : i + min_ref])][-CANDIDATES:]):
            length = 0
            # Past item i the reference reads its own output, as the clock does.
            while (
                i + length < n
                and items[j + length] == items[i + length]
                and (j + length >= i or depth[j + length] < DEPTH)
                and (length == 0 or i + length not in apart)
            ):
                length += 1
            if length >= min_ref:
                saved = cost[i + length] - cost[i] - len(reference(length, len(out) - offset[j]))
                if saved > found[0]:
                    found = (saved, length, j)
        return found

    i = 0
    while i < n:
        key = tuple(items[i : i + min_ref])
        offset[i] = len(out)
        saved, length, j = best(i)
        if saved > 0 and i + 1 < n and i + 1 not in apart:
            # What the next item could save if this one stays as it is.
            starts[key].append(i)
            out += literal(items[i])
            if best(i + 1)[0] > saved + len(literal(items[i])):
                saved = 0
            del out[offset[i] :]
            starts[key].pop()
        if saved > 0:
            out += reference(length, len(out) - offset[j])
            deepest = 1 + max(depth[j : min(j + length, i)])
            depth[i : i + length] = [deepest] * length
        else:
            out += literal(items[i])
            length = 1
        if len(key) == min_ref:
            starts[key].append(i)
        i += length
    return bytes(out), {k: offset[k] for k in apart if k in offset}


class Refs:
    """The references being read, as `refs::Refs` keeps them."""

    def __init__(self):
        self.frames = []  # [where to go on, items still to give]

    def leave(self, at):
        while self.frames and self.frames[-1][1] == 0:
            at = self.frames.pop()[0]
        return at

    def enter(self, at, start, count, back):
        """The place referred to, by a token at `start` that ends at `at`."""
        if not 0 < back <= start:
            raise SystemExit("packed clip: a reference to nowhere")
        if self.frames:
            count = min(count, self.frames[-1][1])
            self.frames[-1][1] -= count
            if self.frames[-1][1] == 0:
                at = self.frames.pop()[0]
        self.frames.append([at, count])
        if len(self.frames) > DEPTH:
            raise SystemExit("packed clip: references too deep")
        return start - back

    def gave(self):
        if self.frames:
            self.frames[-1][1] -= 1

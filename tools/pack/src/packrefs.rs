// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The packer of a clip whose items repeat: a stretch that has been
//! played before becomes a reference to it, which the clock reads with
//! `onebitdawn_core::refs`.

use std::collections::HashMap;

use onebitdawn_core::refs::DEPTH;

/// Earlier places tried for each item.
const CANDIDATES: usize = 256;

pub fn leb128(mut value: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        out.push(byte | if value != 0 { 0x80 } else { 0 });
        if value == 0 {
            return out;
        }
    }
}

/// What `pack` is told of the tokens: an item's own, and the one for
/// `count` items decoded from the token `back` bytes before it.
pub trait Tokens<T> {
    fn literal(&self, item: &T) -> Vec<u8>;
    fn reference(&self, count: usize, back: usize) -> Vec<u8>;
}

struct Packer<'a, T, K: Tokens<T>> {
    items: &'a [T],
    tokens: &'a K,
    min_ref: usize,
    apart: Option<usize>,
    out: Vec<u8>,
    /// Bytes of the items before each, unpacked.
    cost: Vec<usize>,
    /// `min_ref` items -> where a token starts with them.
    starts: HashMap<&'a [T], Vec<usize>>,
    /// The item a token starts at -> the token's place in `out`.
    offset: Vec<usize>,
    /// References to go through to reach each item.
    depth: Vec<usize>,
}

impl<'a, T: Eq + core::hash::Hash, K: Tokens<T>> Packer<'a, T, K> {
    fn key(&self, i: usize) -> &'a [T] {
        &self.items[i..(i + self.min_ref).min(self.items.len())]
    }

    /// (bytes saved, items, the item referred to) for a reference at
    /// item `i`.
    fn best(&self, i: usize) -> (usize, usize, usize) {
        let (items, n) = (self.items, self.items.len());
        let mut found = (0, 0, 0);
        let Some(starts) = self.starts.get(self.key(i)) else {
            return found;
        };
        for &j in starts.iter().rev().take(CANDIDATES) {
            let mut length = 0;
            // Past item i the reference reads its own output, as the
            // clock does.
            while i + length < n
                && items[j + length] == items[i + length]
                && (j + length >= i || self.depth[j + length] < DEPTH)
                && (length == 0 || Some(i + length) != self.apart)
            {
                length += 1;
            }
            if length >= self.min_ref {
                let reference = self.tokens.reference(length, self.out.len() - self.offset[j]).len();
                let unpacked = self.cost[i + length] - self.cost[i];
                if unpacked > reference && unpacked - reference > found.0 {
                    found = (unpacked - reference, length, j);
                }
            }
        }
        found
    }
}

/// The items as tokens, and where the item `apart` landed.
///
/// A reference is `min_ref` items at least. The item `apart` (by its
/// index) starts a token outside any reference, for a place the clip
/// is entered at, as where a tune starts over.
///
/// A reference is to a token's start, so the places an item can refer
/// to are those where this packer itself began a token; the newest
/// `CANDIDATES` that start with the same `min_ref` items are tried,
/// and the one that saves most bytes is taken. An item is left as it
/// is if the reference from the next one saves more.
pub fn pack<T: Eq + core::hash::Hash, K: Tokens<T>>(items: &[T], tokens: &K, min_ref: usize, apart: usize) -> (Vec<u8>, Option<usize>) {
    let n = items.len();
    let mut cost = vec![0; n + 1];
    for (k, item) in items.iter().enumerate() {
        cost[k + 1] = cost[k] + tokens.literal(item).len();
    }
    let mut p = Packer { items, tokens, min_ref, apart: Some(apart), out: Vec::new(), cost, starts: HashMap::new(), offset: vec![usize::MAX; n], depth: vec![0; n] };
    let mut i = 0;
    while i < n {
        let key = p.key(i);
        p.offset[i] = p.out.len();
        let (mut saved, mut length, j) = p.best(i);
        let literal = tokens.literal(&items[i]);
        if saved > 0 && i + 1 < n && i + 1 != apart {
            // What the next item could save if this one stays as it is.
            p.starts.entry(key).or_default().push(i);
            p.out.extend(&literal);
            if p.best(i + 1).0 > saved + literal.len() {
                saved = 0;
            }
            p.out.truncate(p.offset[i]);
            p.starts.entry(key).or_default().pop();
        }
        if saved > 0 {
            let reference = tokens.reference(length, p.out.len() - p.offset[j]);
            p.out.extend(reference);
            let deepest = 1 + p.depth[j..(j + length).min(i)].iter().max().copied().unwrap_or(0);
            p.depth[i..i + length].fill(deepest);
        } else {
            p.out.extend(literal);
            length = 1;
        }
        if key.len() == min_ref {
            p.starts.entry(key).or_default().push(i);
        }
        i += length;
    }
    let landed = p.offset.get(apart).copied().filter(|&o| o != usize::MAX);
    (p.out, landed)
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! References in a packed clip: "the next so many items are those
//! decoded from the token that starts so many bytes back".
//!
//! A clip that is a stream read from front to back (a beeper clip's
//! runs, a SID tune's register writes) says a stretch it has had
//! before by such a reference and not again. The reference is to the
//! clip's bytes, not to what was played: the clip is read in place
//! from the flash and nothing played is kept, so going back is a place
//! to return to and a count, which is what `Refs` holds. What is
//! referred to may have references in it, `DEPTH` deep. A reference
//! that takes all its parent still wants takes the parent's place, so
//! a chain of them is no depth, and a reference may reach itself:
//! what lies between is then given round and round until the count
//! is up, a few items many times over.
//!
//! The reader of a format keeps the place and knows the tokens; it
//! calls `leave` before it reads a token, `enter` for a reference and
//! `gave` for each item. `tools/pack/src/packrefs.rs` is the packer, and
//! `scripts/packrefs.py` the beeper converter's.

/// How deep references may nest.
pub const DEPTH: usize = 16;
/// Tokens a reader may read for one item before the clip is taken for
/// a bad one, whose references give nothing: a good clip reads `DEPTH`
/// references at most.
pub const IDLE_TOKENS: usize = 4 * DEPTH;

/// The references being read.
pub struct Refs {
    /// Innermost last: where to go on from when one is done, and the
    /// items it still stands for.
    frames: [(u32, u32); DEPTH],
    depth: usize,
}

impl Refs {
    pub const fn new() -> Self {
        Self { frames: [(0, 0); DEPTH], depth: 0 }
    }

    /// Out of them all, for a clip that starts over.
    pub fn reset(&mut self) {
        self.depth = 0;
    }

    /// Whether the place is in no reference.
    pub fn idle(&self) -> bool {
        self.depth == 0
    }

    /// Out of the references that have given all their items: `at`
    /// goes on from where the outermost of those was read.
    pub fn leave(&mut self, at: &mut usize) {
        while self.depth > 0 && self.frames[self.depth - 1].1 == 0 {
            self.depth -= 1;
            *at = self.frames[self.depth].0 as usize;
        }
    }

    /// Into a reference to `count` items, `back` bytes before `start`,
    /// where its token began; `at` is the place after the token and
    /// becomes the place referred to. `false` for one that points
    /// nowhere or is too deep, which is the clip's end.
    pub fn enter(&mut self, at: &mut usize, start: usize, mut count: u32, back: usize) -> bool {
        if back == 0 || back > start {
            return false;
        }
        let mut resume = *at as u32;
        if self.depth > 0 {
            // No more than the reference this one is read for still
            // wants; one that takes all of it takes its place too.
            let outer = &mut self.frames[self.depth - 1];
            count = count.min(outer.1);
            outer.1 -= count;
            if outer.1 == 0 {
                resume = outer.0;
                self.depth -= 1;
            }
        }
        let Some(frame) = self.frames.get_mut(self.depth) else {
            return false;
        };
        *frame = (resume, count);
        self.depth += 1;
        *at = start - back;
        true
    }

    /// An item was read.
    pub fn gave(&mut self) {
        if self.depth > 0 {
            let left = &mut self.frames[self.depth - 1].1;
            *left = left.saturating_sub(1);
        }
    }
}

impl Default for Refs {
    fn default() -> Self {
        Self::new()
    }
}

/// A LEB128 number at `*at`, which moves past it; `None` if the bytes
/// end first.
pub fn leb128(data: &[u8], at: &mut usize) -> Option<u32> {
    let mut value = 0u32;
    let mut shift = 0;
    loop {
        let byte = *data.get(*at)?;
        *at += 1;
        if shift < 32 {
            value |= ((byte & 0x7F) as u32) << shift;
        }
        shift += 7;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! Reading a file that may be cut short or lie about itself: a field
//! past the end is an error, a run of bytes past the end is as much of
//! it as there is.

pub const SHORT: &str = "the file is cut short";

#[derive(Clone, Copy)]
pub struct Data<'a>(pub &'a [u8]);

impl<'a> Data<'a> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The bytes from `from` to `to`, or what there is of them.
    pub fn slice(&self, from: usize, to: usize) -> &'a [u8] {
        let to = to.min(self.0.len());
        &self.0[from.min(to)..to]
    }

    /// `len` bytes at `at`, all of them.
    pub fn bytes(&self, at: usize, len: usize) -> Result<&'a [u8], String> {
        at.checked_add(len).and_then(|end| self.0.get(at..end)).ok_or_else(|| SHORT.to_string())
    }

    pub fn u8(&self, at: usize) -> Result<u8, String> {
        self.0.get(at).copied().ok_or_else(|| SHORT.to_string())
    }

    pub fn i8(&self, at: usize) -> Result<i8, String> {
        Ok(self.u8(at)? as i8)
    }

    pub fn u16(&self, at: usize) -> Result<u16, String> {
        let b = self.bytes(at, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u16be(&self, at: usize) -> Result<u16, String> {
        let b = self.bytes(at, 2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u32(&self, at: usize) -> Result<u32, String> {
        let b = self.bytes(at, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A `u32` as an offset or a length.
    pub fn size(&self, at: usize) -> Result<usize, String> {
        Ok(self.u32(at)? as usize)
    }
}

/// A text field: up to its first NUL, as Latin-1.
pub fn text(field: &[u8]) -> String {
    field.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect()
}

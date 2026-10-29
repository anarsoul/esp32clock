//! What an OTA image looks like, and the pieces of the upload that need
//! no flash: the header and descriptor offsets, the slot names, the
//! appended-hash bookkeeping and the version string.

use sha2::{Digest, Sha256};

/// Image header magic and the app descriptor's, which sits at 0x20 in
/// every esp-idf style image (24-byte image header, 8-byte segment
/// header, then the descriptor as the first thing in the first segment).
pub const IMAGE_MAGIC: u8 = 0xE9;
pub const DESC_OFFSET: usize = 0x20;
pub const DESC_MAGIC: u32 = 0xABCD_5432;
/// The 32-byte version string, NUL padded, inside the descriptor.
pub const DESC_VERSION: core::ops::Range<usize> = DESC_OFFSET + 16..DESC_OFFSET + 48;
/// Image header byte that says a SHA-256 of the rest of the image is its
/// last 32 bytes. espflash always sets it; an image without it is
/// refused, since nothing else can tell a truncated file from a whole one.
pub const HASH_APPENDED: usize = 23;
pub const HASH_LEN: usize = 32;

/// An app slot, as the boot found it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Slot {
    Unknown = 0,
    Factory = 1,
    Ota0 = 2,
    Ota1 = 3,
}

impl Slot {
    pub fn name(self) -> &'static str {
        match self {
            Slot::Unknown => "?",
            Slot::Factory => "factory",
            Slot::Ota0 => "ota_0",
            Slot::Ota1 => "ota_1",
        }
    }

    pub fn from_raw(raw: u8) -> Slot {
        match raw {
            1 => Slot::Factory,
            2 => Slot::Ota0,
            3 => Slot::Ota1,
            _ => Slot::Unknown,
        }
    }
}

/// Feed the chunk at `at` of an image `total` bytes long to the hasher,
/// except for the last `HASH_LEN` bytes of the image, which go into
/// `tail` instead.
pub fn hash_chunk(hasher: &mut Sha256, tail: &mut [u8; HASH_LEN], chunk: &[u8], at: usize, total: usize) {
    let hashed_end = total - HASH_LEN;
    let split = hashed_end.saturating_sub(at).min(chunk.len());
    hasher.update(&chunk[..split]);
    for (i, &b) in chunk[split..].iter().enumerate() {
        tail[at + split + i - hashed_end] = b;
    }
}

/// The descriptor's version field as text: up to the first NUL, printable
/// ASCII only.
pub fn version_from(field: &[u8]) -> heapless::String<32> {
    let mut s = heapless::String::new();
    for &b in field.iter().take_while(|&&b| b != 0) {
        let c = if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' };
        if s.push(c).is_err() {
            break;
        }
    }
    s
}


#[cfg(test)]
mod tests {
    use super::*;

    const SECTOR: usize = 4096;

    /// An image of `total` bytes whose last 32 are the SHA-256 of the rest,
    /// as espflash appends it.
    fn image(total: usize) -> Vec<u8> {
        let mut img: Vec<u8> = (0..total - HASH_LEN).map(|i| (i * 7 + i / 300) as u8).collect();
        let hash = Sha256::digest(&img);
        img.extend_from_slice(&hash);
        img
    }

    fn check(total: usize, chunk: usize) {
        let img = image(total);
        let mut hasher = Sha256::new();
        let mut tail = [0u8; HASH_LEN];
        let mut at = 0;
        while at < total {
            let n = chunk.min(total - at);
            hash_chunk(&mut hasher, &mut tail, &img[at..at + n], at, total);
            at += n;
        }
        assert_eq!(hasher.finalize()[..], tail[..], "total {total}, chunks of {chunk}");
        assert_eq!(tail[..], img[total - HASH_LEN..]);
    }

    #[test]
    fn appended_hash_is_split_off_wherever_it_falls() {
        for total in [3 * SECTOR, 3 * SECTOR + 1001, 3 * SECTOR + 3, 3 * SECTOR + 31, 3 * SECTOR + 32, 3 * SECTOR + 33, SECTOR] {
            check(total, SECTOR);
        }
        // Chunks that are not sectors, including one byte at a time across the boundary.
        check(2 * SECTOR + 17, 1000);
        check(2 * SECTOR + 17, 1);
    }

    #[test]
    fn version_field() {
        let mut field = [0u8; 32];
        field[..10].copy_from_slice(b"v1.2-dirty");
        assert_eq!(version_from(&field).as_str(), "v1.2-dirty");
        field[3] = 0x01;
        field[4] = 0xFF;
        assert_eq!(version_from(&field).as_str(), "v1.??dirty");
        assert_eq!(version_from(&[0; 32]).as_str(), "");
        assert_eq!(version_from(&[b'x'; 40]).as_str(), "x".repeat(32));
        assert_eq!(DESC_VERSION, 0x30..0x50);
    }

    #[test]
    fn slots() {
        for (raw, slot, name) in [(0, Slot::Unknown, "?"), (1, Slot::Factory, "factory"), (2, Slot::Ota0, "ota_0"), (3, Slot::Ota1, "ota_1"), (9, Slot::Unknown, "?")] {
            assert_eq!(Slot::from_raw(raw), slot);
            assert_eq!(slot.name(), name);
            if raw < 4 {
                assert_eq!(slot as u8, raw);
            }
        }
    }
}

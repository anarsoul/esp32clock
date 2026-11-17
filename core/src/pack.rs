// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! A clip pack: the clips of the `clips` partition, which a clock takes
//! in place of the ones built into its firmware. Pure: the layout and
//! the checks; `src/pack.rs` finds the partition, maps it and takes an
//! upload. `scripts/mkpack.py` makes one.
//!
//! | at | what |
//! | --- | --- |
//! | 0 | `CLPK` |
//! | 4 | the version, 1 |
//! | 5 | the number of clips, 1 to `MAX_CLIPS` |
//! | 6 | two zero bytes |
//! | 8 | the length of the whole pack, the hash included, little endian |
//! | 12 | an entry of `ENTRY_LEN` bytes a clip |
//! | after | the clips' data |
//! | end | SHA-256 of everything before it |
//!
//! An entry: the offset of the clip's data in the pack and its length
//! (`u32` each, little endian), the flags (bit 0: a ringing alarm plays
//! it round and round with no gap), three zero bytes, the API name
//! (`NAME_MAX` bytes, NUL padded) and the label (`LABEL_MAX` bytes, NUL
//! padded). A clip's data is what `ALARM_CLIP` takes: raw PCM, a beeper
//! clip, a tracker module, a SID tune or a PT3 module.
//!
//! Version 1 is the first public release's, and every firmware after
//! it must go on reading it: a clock's pack outlives its firmware. A
//! firmware refuses a pack of another version, and one with any of the
//! zero bytes or an entry's other flag bits set, and then runs with the
//! note melodies. So whatever a later pack has that this parser would
//! get wrong must set one of those: above all a clip of a new kind,
//! whose magic an older firmware does not know and would play as PCM
//! with no header, as noise (`clip::is_unsupported` knows only the
//! kinds there are, and a new version of one of them).

use sha2::{Digest, Sha256};

pub const MAGIC: &[u8; 4] = b"CLPK";
pub const VERSION: u8 = 1;
/// The most clips a pack holds: what the clock page and the discovery
/// payload have room to list, which their tests measure.
pub const MAX_CLIPS: usize = 16;
pub const HEADER_LEN: usize = 12;
pub const ENTRY_LEN: usize = 64;
pub const NAME_MAX: usize = 20;
pub const LABEL_MAX: usize = 32;
pub const HASH_LEN: usize = 32;

const ENTRY_NAME: usize = 12;
const ENTRY_LABEL: usize = ENTRY_NAME + NAME_MAX;
const FLAG_LOOPS: u8 = 1;

/// The shortest clip, as for one built in.
const CLIP_MIN_MS: u32 = 1000;

/// Why a pack is not one. The number is the clip's, from 0.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PackError {
    /// Shorter than a header and a hash, or than its own table.
    Short,
    Magic,
    Version,
    /// No clips, or more than `MAX_CLIPS`.
    Count,
    /// The length in the header is not the length in hand.
    Length,
    /// An entry's data is outside the pack, or its reserved bits are
    /// set: a later firmware's pack.
    Entry(usize),
    /// A name that is empty, has other than `a-z` and `_`, is another
    /// clip's, a note melody's or `random`.
    Name(usize),
    /// A label that is empty, not UTF-8, or has a control character or
    /// one of `< > & "`.
    Label(usize),
    /// Less than a second of sound, or a clip of a format's version
    /// the players do not read, an old one or a newer converter's
    /// (`clip::is_unsupported`).
    Clip(usize),
    Hash,
}

impl PackError {
    /// The refusal, for an upload's answer.
    pub const fn text(self) -> &'static str {
        match self {
            PackError::Short => "not a clip pack: too short",
            PackError::Magic => "not a clip pack",
            PackError::Version => "a clip pack of a version this firmware does not read",
            PackError::Count => "a clip pack holds 1 to 16 clips",
            PackError::Length => "clip pack length mismatch: truncated?",
            PackError::Entry(_) => "clip pack: an entry points outside the pack, or the pack is for a newer firmware",
            PackError::Name(_) => "clip pack: a clip's name is taken or not a-z and _",
            PackError::Label(_) => "clip pack: a clip's label is empty or has characters a page cannot show",
            PackError::Clip(_) => "clip pack: a clip is shorter than a second or of a version this firmware does not play",
            PackError::Hash => "clip pack hash mismatch: truncated or corrupt",
        }
    }
}

/// The length a pack's header declares, from its first `HEADER_LEN`
/// bytes or more: what to read, or to expect of an upload, before the
/// rest is in hand.
pub fn declared_len(head: &[u8]) -> Result<usize, PackError> {
    let head = head.get(..HEADER_LEN).ok_or(PackError::Short)?;
    if head[..4] != MAGIC[..] {
        return Err(PackError::Magic);
    }
    if head[4] != VERSION {
        return Err(PackError::Version);
    }
    let total = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
    if total < HEADER_LEN + ENTRY_LEN + HASH_LEN {
        return Err(PackError::Short);
    }
    Ok(total)
}

/// A NUL-padded field as text: up to the first NUL, nothing but NULs
/// after it.
fn field(bytes: &[u8]) -> Option<&str> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    if bytes[end..].iter().any(|&b| b != 0) {
        return None;
    }
    core::str::from_utf8(&bytes[..end]).ok()
}

fn u32_at(bytes: &[u8], at: usize) -> usize {
    bytes.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// One clip of a pack.
#[derive(Clone, Copy, Debug)]
pub struct Clip<'a> {
    pub name: &'a str,
    pub label: &'a str,
    pub data: &'a [u8],
    /// Made to go round: a ringing alarm plays it with no gap.
    pub loops: bool,
}

/// A pack that passed `parse`.
#[derive(Clone, Copy, Debug)]
pub struct Pack<'a> {
    data: &'a [u8],
}

impl<'a> Pack<'a> {
    /// The pack in `data`, which is all of it and nothing more, if it is
    /// one: see `PackError`.
    pub fn parse(data: &'a [u8]) -> Result<Self, PackError> {
        if declared_len(data)? != data.len() {
            return Err(PackError::Length);
        }
        let count = data[5] as usize;
        if count == 0 || count > MAX_CLIPS || data[6] != 0 || data[7] != 0 {
            return Err(PackError::Count);
        }
        let table_end = HEADER_LEN + count * ENTRY_LEN;
        let body_end = data.len() - HASH_LEN;
        if table_end > body_end {
            return Err(PackError::Short);
        }
        let pack = Pack { data };
        for k in 0..count {
            let e = &data[HEADER_LEN + k * ENTRY_LEN..][..ENTRY_LEN];
            let (at, len) = (u32_at(e, 0), u32_at(e, 4));
            let inside = at >= table_end && at.checked_add(len).is_some_and(|end| end <= body_end);
            if !inside || e[8] & !FLAG_LOOPS != 0 || e[9..ENTRY_NAME] != [0; 3] {
                return Err(PackError::Entry(k));
            }
            let name = field(&e[ENTRY_NAME..ENTRY_LABEL]).ok_or(PackError::Name(k))?;
            let taken = (0..k).any(|j| pack.clip(j).is_some_and(|c| c.name == name));
            if name.is_empty()
                || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                || taken
                || crate::melody::is_reserved(name)
            {
                return Err(PackError::Name(k));
            }
            let label = field(&e[ENTRY_LABEL..]).ok_or(PackError::Label(k))?;
            if label.is_empty() || label.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '&' | '"')) {
                return Err(PackError::Label(k));
            }
        }
        if Sha256::digest(&data[..body_end])[..] != data[body_end..] {
            return Err(PackError::Hash);
        }
        // What is asked of a clip built in (`melody::BUILT_IN`).
        for k in 0..count {
            let clip = pack.clip(k).ok_or(PackError::Entry(k))?.data;
            if crate::clip::duration_ms(clip) < CLIP_MIN_MS || crate::clip::is_unsupported(clip) {
                return Err(PackError::Clip(k));
            }
        }
        Ok(pack)
    }

    /// The number of clips.
    pub fn count(&self) -> usize {
        self.data.get(5).map_or(0, |&n| n as usize)
    }

    /// The pack's length in bytes, the hash included.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }

    /// Clip `k`, from 0; `None` past the last.
    pub fn clip(&self, k: usize) -> Option<Clip<'a>> {
        if k >= self.count() {
            return None;
        }
        let e = self.data.get(HEADER_LEN + k * ENTRY_LEN..)?.get(..ENTRY_LEN)?;
        let at = u32_at(e, 0);
        Some(Clip {
            name: field(&e[ENTRY_NAME..ENTRY_LABEL])?,
            label: field(&e[ENTRY_LABEL..])?,
            data: self.data.get(at..at.checked_add(u32_at(e, 4))?)?,
            loops: e[8] & FLAG_LOOPS != 0,
        })
    }
}

/// A pack of these clips (name, label, loops, data), as
/// `scripts/mkpack.py` lays one out: for the tests here and in the
/// modules that list the melodies.
#[cfg(test)]
pub(crate) fn build(clips: &[(&str, &str, bool, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, clips.len() as u8, 0, 0, 0, 0, 0, 0]);
    let mut at = HEADER_LEN + clips.len() * ENTRY_LEN;
    for (name, label, loops, data) in clips {
        out.extend_from_slice(&(at as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&[*loops as u8, 0, 0, 0]);
        let mut text = [0u8; NAME_MAX + LABEL_MAX];
        text[..name.len()].copy_from_slice(name.as_bytes());
        text[NAME_MAX..NAME_MAX + label.len()].copy_from_slice(label.as_bytes());
        out.extend_from_slice(&text);
        at += data.len();
    }
    for (_, _, _, data) in clips {
        out.extend_from_slice(data);
    }
    seal(&mut out);
    out
}

/// Write the length and append the hash, over whatever `out` holds.
#[cfg(test)]
pub(crate) fn seal(out: &mut Vec<u8>) {
    let total = (out.len() + HASH_LEN) as u32;
    out[8..12].copy_from_slice(&total.to_le_bytes());
    let hash = Sha256::digest(&out[..]);
    out.extend_from_slice(&hash);
}

/// The largest pack by what it lists: `MAX_CLIPS` clips with names and
/// labels at their limits, a second of PCM each, leaked so it can be
/// installed. For the page and discovery size tests.
#[cfg(test)]
pub(crate) fn fullest() -> Pack<'static> {
    let names: Vec<(String, String)> = (0..MAX_CLIPS)
        .map(|k| {
            let letter = (b'a' + k as u8) as char;
            (letter.to_string().repeat(NAME_MAX), letter.to_ascii_uppercase().to_string().repeat(LABEL_MAX))
        })
        .collect();
    let clips: Vec<(&str, &str, bool, &[u8])> = names.iter().map(|(n, l)| (n.as_str(), l.as_str(), true, &tests::SECOND[..])).collect();
    Pack::parse(Vec::leak(build(&clips))).unwrap()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A second of silence as raw PCM at 8 kHz.
    pub(crate) const SECOND: [u8; 8000] = [128; 8000];

    fn two() -> Vec<u8> {
        build(&[("first", "The First", true, &SECOND), ("second_one", "Zweite Größe", false, &[127; 9000])])
    }

    /// The pack with one edit, sealed again so only the edit is wrong.
    fn with(edit: impl FnOnce(&mut Vec<u8>)) -> Result<(), PackError> {
        let mut data = two();
        data.truncate(data.len() - HASH_LEN);
        edit(&mut data);
        seal(&mut data);
        Pack::parse(&data).map(|_| ())
    }

    #[test]
    fn a_pack_round_trips() {
        let data = two();
        let pack = Pack::parse(&data).unwrap();
        assert_eq!((pack.count(), pack.len(), pack.is_empty()), (2, data.len(), false));
        let (a, b) = (pack.clip(0).unwrap(), pack.clip(1).unwrap());
        assert_eq!((a.name, a.label, a.loops, a.data), ("first", "The First", true, &SECOND[..]));
        assert_eq!((b.name, b.label, b.loops, b.data), ("second_one", "Zweite Größe", false, &[127u8; 9000][..]));
        assert!(pack.clip(2).is_none());
        assert_eq!(declared_len(&data[..HEADER_LEN]), Ok(data.len()));
        assert_eq!(with(|_| ()), Ok(()));
    }

    #[test]
    fn what_is_not_a_pack_is_refused() {
        let data = two();
        assert_eq!(Pack::parse(&data[..8]).unwrap_err(), PackError::Short);
        assert_eq!(Pack::parse(&[0xFF; 4096]).unwrap_err(), PackError::Magic, "an erased partition");
        assert_eq!(Pack::parse(&data[..data.len() - 1]).unwrap_err(), PackError::Length);
        let mut longer = data.clone();
        longer.push(0);
        assert_eq!(Pack::parse(&longer).unwrap_err(), PackError::Length);
        let mut flipped = data.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert_eq!(Pack::parse(&flipped).unwrap_err(), PackError::Hash);
        let mut inside = data.clone();
        inside[HEADER_LEN + 2 * ENTRY_LEN + 100] ^= 0x10;
        assert_eq!(Pack::parse(&inside).unwrap_err(), PackError::Hash, "a bit of a clip's data");

        assert_eq!(with(|d| d[4] = 2), Err(PackError::Version));
        assert_eq!(with(|d| d[5] = 0), Err(PackError::Count));
        assert_eq!(with(|d| d[5] = MAX_CLIPS as u8 + 1), Err(PackError::Count));
        assert_eq!(with(|d| d[6] = 1), Err(PackError::Count));
        // An entry: its data before the table's end, past the pack's, a length that wraps, a reserved flag.
        let e = HEADER_LEN + ENTRY_LEN;
        assert_eq!(with(|d| d[e..e + 4].copy_from_slice(&12u32.to_le_bytes())), Err(PackError::Entry(1)));
        assert_eq!(with(|d| d[e + 4..e + 8].copy_from_slice(&9001u32.to_le_bytes())), Err(PackError::Entry(1)));
        assert_eq!(with(|d| d[e + 4..e + 8].copy_from_slice(&u32::MAX.to_le_bytes())), Err(PackError::Entry(1)));
        assert_eq!(with(|d| d[e + 8] = 2), Err(PackError::Entry(1)));
        assert_eq!(with(|d| d[e + 9] = 1), Err(PackError::Entry(1)));
        // A count the table has no room for.
        let mut crowded = build(&[("a", "A", true, &[])]);
        crowded.truncate(crowded.len() - HASH_LEN);
        crowded[5] = 2;
        seal(&mut crowded);
        assert_eq!(Pack::parse(&crowded).unwrap_err(), PackError::Short);
    }

    #[test]
    fn names_and_labels_are_held_to_the_page() {
        let name = HEADER_LEN + ENTRY_LEN + ENTRY_NAME;
        let label = name + NAME_MAX;
        let put = |d: &mut Vec<u8>, at: usize, len: usize, s: &[u8]| {
            d[at..at + len].fill(0);
            d[at..at + s.len()].copy_from_slice(s);
        };
        for bad in [&b""[..], b"Second", b"two words", b"first", b"random", b"beeps", b"a\0b", b"nine9"] {
            assert_eq!(with(|d| put(d, name, NAME_MAX, bad)), Err(PackError::Name(1)), "{:?}", core::str::from_utf8(bad));
        }
        assert_eq!(with(|d| put(d, name, NAME_MAX, &[b'z'; NAME_MAX])), Ok(()), "a name may fill its field");
        for bad in [&b""[..], b"a<b", b"a>b", b"a&b", b"a\"b", b"a\nb", b"\xff\xfe", b"a\0b"] {
            assert_eq!(with(|d| put(d, label, LABEL_MAX, bad)), Err(PackError::Label(1)), "{bad:?}");
        }
        assert_eq!(with(|d| put(d, label, LABEL_MAX, "Frère Jacques' 2nd (loud!)".as_bytes())), Ok(()));
    }

    #[test]
    fn clips_are_held_to_what_a_built_in_one_is() {
        assert_eq!(Pack::parse(&build(&[("short", "Short", false, &[128; 7999])])).unwrap_err(), PackError::Clip(0));
        assert_eq!(Pack::parse(&build(&[("ok", "Ok", false, &SECOND), ("old", "Old", true, b"BPR1\0\0\0\0")])).unwrap_err(), PackError::Clip(1));
        assert_eq!(Pack::parse(&build(&[("old", "Old", true, b"XMC1....")])).unwrap_err(), PackError::Clip(0));
        // A version from a newer converter, behind a second of anything.
        for magic in [b"PCM2", b"ADP2", b"BPR4", b"XMC3", b"SID3", b"PT32"] {
            let mut clip = magic.to_vec();
            clip.extend_from_slice(&SECOND);
            assert_eq!(Pack::parse(&build(&[("new", "New", false, &clip)])).unwrap_err(), PackError::Clip(0), "{:?}", core::str::from_utf8(magic));
        }
    }

    #[test]
    fn the_fullest_pack_is_a_pack() {
        let pack = fullest();
        assert_eq!(pack.count(), MAX_CLIPS);
        let last = pack.clip(MAX_CLIPS - 1).unwrap();
        assert_eq!((last.name.len(), last.label.len()), (NAME_MAX, LABEL_MAX));
    }

    /// With `CLIP_PACK_FILE=file`, the pack `scripts/mkpack.py` made is
    /// read by the parser the clock reads it with, and each clip is
    /// started. Passes with nothing to check when the variable is unset.
    /// (The variable is not in `build.rs`'s list: change the file and
    /// run the test again, nothing is rebuilt.)
    #[test]
    fn the_pack_file_parses() {
        let Ok(path) = std::env::var("CLIP_PACK_FILE") else { return };
        // A relative path is from the workspace root, where cargo is
        // run; the test itself runs in `core/`.
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(&path);
        let data = std::fs::read(&file).unwrap_or_else(|e| panic!("{path}: {e}"));
        let pack = Pack::parse(&data).unwrap_or_else(|e| panic!("{path}: {e:?}: {}", e.text()));
        println!("{path}: {} clips, {} bytes", pack.count(), pack.len());
        for k in 0..pack.count() {
            let c = pack.clip(k).unwrap();
            let ms = crate::clip::duration_ms(c.data);
            println!("  {:<20} {:<32} {:>7} bytes {:>4} s{}", c.name, c.label, c.data.len(), ms / 1000, if c.loops { ", loops" } else { "" });
            let mut stream = crate::clip::Stream::new(Vec::leak(c.data.to_vec()), false);
            let mut chunk = [0u8; crate::clip::CHUNK];
            stream.fill(&mut chunk);
            assert!(!stream.done(), "{}: over within a chunk", c.name);
        }
    }
}

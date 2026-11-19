// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The pack writer. The layout is `onebitdawn_core::pack`'s, and what
//! is written here is read back by its parser before it is given out.

use onebitdawn_core::melody;
use onebitdawn_core::pack::{ENTRY_LEN, HASH_LEN, HEADER_LEN, LABEL_MAX, MAGIC, MAX_CLIPS, NAME_MAX, Pack, ROOM, VERSION};
use sha2::{Digest, Sha256};

use crate::Clip;

/// The shortest clip the clock takes, `core::pack`'s.
pub const CLIP_MIN_MS: u32 = 1000;

/// What `Pack::parse` asks of a name and a label, said of the file.
pub fn check_names(file: &str, name: &str, label: &str) -> Result<(), String> {
    if name.len() > NAME_MAX {
        return Err(format!("{file}: the name {name} is longer than {NAME_MAX} characters; give it another, {file}:name=…"));
    }
    if melody::is_reserved(name) {
        return Err(format!("{file}: the name {name} is a melody of the firmware's; give it another, {file}:name=…"));
    }
    if label.len() > LABEL_MAX {
        return Err(format!("{file}: the label {label:?} is longer than {LABEL_MAX} bytes"));
    }
    if label.trim().is_empty() || label.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '&' | '"')) {
        return Err(format!("{file}: the label {label:?} is empty or has one of < > & \" or a control character"));
    }
    Ok(())
}

/// The pack's length with these clips in it.
pub fn total(clips: &[Clip]) -> usize {
    HEADER_LEN + clips.len() * ENTRY_LEN + clips.iter().map(|c| c.data.len()).sum::<usize>() + HASH_LEN
}

/// A pack of these clips, in this order.
pub fn build(clips: &[Clip]) -> Result<Vec<u8>, String> {
    if clips.is_empty() {
        return Err("no clips; a pack holds at least one".into());
    }
    if clips.len() > MAX_CLIPS {
        return Err(format!("{} clips; a pack holds {MAX_CLIPS}", clips.len()));
    }
    for (k, clip) in clips.iter().enumerate() {
        if clips[..k].iter().any(|c| c.name == clip.name) {
            return Err(format!("two clips named {}; give one another, file:name=…", clip.name));
        }
    }
    let total = total(clips);
    if total > ROOM {
        return Err(format!("{total} bytes; the clips partition holds {ROOM}"));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, clips.len() as u8, 0, 0]);
    out.extend_from_slice(&(total as u32).to_le_bytes());
    let mut at = HEADER_LEN + clips.len() * ENTRY_LEN;
    for clip in clips {
        out.extend_from_slice(&(at as u32).to_le_bytes());
        out.extend_from_slice(&(clip.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&[clip.loops as u8, 0, 0, 0]);
        let mut text = [0u8; NAME_MAX + LABEL_MAX];
        // `check_names` is `convert`'s; a clip from elsewhere is caught
        // here and not by a panic.
        if clip.name.len() > NAME_MAX || clip.label.len() > LABEL_MAX {
            return Err(format!("{}: the name or the label is too long", clip.name));
        }
        text[..clip.name.len()].copy_from_slice(clip.name.as_bytes());
        text[NAME_MAX..NAME_MAX + clip.label.len()].copy_from_slice(clip.label.as_bytes());
        out.extend_from_slice(&text);
        at += clip.data.len();
    }
    for clip in clips {
        out.extend_from_slice(&clip.data);
    }
    let hash = Sha256::digest(&out);
    out.extend_from_slice(&hash);
    // The clock's own reading of it.
    match Pack::parse(&out) {
        Ok(_) => Ok(out),
        Err(e) => Err(format!("the pack does not pass the clock's check: {}", e.text())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(name: &str, loops: bool) -> Clip {
        let mut data = b"PCM1\x80\x3e\0\0".to_vec();
        data.extend([128u8; 16000]);
        Clip { name: name.into(), label: name.to_uppercase(), loops, data, notes: Vec::new() }
    }

    #[test]
    fn a_pack_is_what_the_clock_reads() {
        let made = build(&[clip("one", false), clip("two", true)]).unwrap();
        let pack = Pack::parse(&made).unwrap();
        assert_eq!(pack.count(), 2);
        let two = pack.clip(1).unwrap();
        assert!(two.name == "two" && two.label == "TWO" && two.loops && two.data.len() == 16008);
        assert!(!pack.clip(0).unwrap().loops);
        assert_eq!(made.len(), total(&[clip("one", false), clip("two", true)]));
    }

    #[test]
    fn what_the_clock_would_refuse_is_refused_here() {
        assert!(build(&[]).is_err());
        assert!(build(&[clip("one", false), clip("one", false)]).unwrap_err().contains("two clips named one"));
        let many: Vec<Clip> = (0..17).map(|k| clip(&format!("c{}", (b'a' + k) as char), false)).collect();
        assert!(build(&many).unwrap_err().contains("17 clips"));
        let mut long = clip("long", false);
        long.data.extend(vec![128u8; ROOM]);
        assert!(build(&[long]).unwrap_err().contains("the clips partition holds"));
        // A name the parser refuses and `check_names` was not asked about.
        assert!(build(&[clip("beeps", false)]).unwrap_err().contains("clock's check"));
        assert!(check_names("f", "beeps", "Beeps").is_err());
        assert!(check_names("f", "ok", "a<b").is_err());
        assert!(check_names("f", "ok", "  ").is_err());
        assert!(check_names("f", "abcdefghijklmnopqrstu", "x").is_err());
        assert!(check_names("f", "ok", "Fine").is_ok());
    }
}

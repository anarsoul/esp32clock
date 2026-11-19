// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! What a file is: by its bytes where they say, by its name where they
//! do not.

use onebitdawn_core::{clip, pt3, sid, xm};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A clip made already, of any kind the clock plays.
    Clip,
    Xm,
    Mod,
    S3m,
    Sid,
    Pt3,
    Wav,
}

/// Whether a clip is a tune, made to go round, and not a recording.
pub fn is_tune(data: &[u8]) -> bool {
    clip::is_beeper(data) || xm::is_tune(data) || sid::is_tune(data) || pt3::is_tune(data)
}

/// The clips' magics less the version: a file that starts with one is a
/// clip, of a version the clock plays or not.
const FAMILIES: [&[u8; 3]; 6] = [b"PCM", b"ADP", b"BPR", b"XMC", b"SID", b"PT3"];

fn extension(file: &str) -> String {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    base.rsplit_once('.').map_or(String::new(), |(_, e)| e.to_ascii_lowercase())
}

/// The channel count a ProTracker module's tag names, as
/// `modxm::channels` reads it.
pub fn mod_channels(data: &[u8]) -> Option<usize> {
    let tag = data.get(1080..1084)?;
    match tag {
        b"M.K." | b"M!K!" | b"FLT4" | b"4CHN" => Some(4),
        [n @ b'1'..=b'9', b'C', b'H', b'N'] => Some((n - b'0') as usize),
        [a @ b'1'..=b'9', b @ b'0'..=b'9', b'C', b'H'] => Some(((a - b'0') * 10 + (b - b'0')) as usize),
        _ => None,
    }
}

impl Kind {
    pub fn of(file: &str, data: &[u8]) -> Option<Kind> {
        let ext = extension(file);
        // A clip's own extension first: a PT3 clip's magic is a PT3
        // module's first bytes too ("PT3" against "ProTracker 3").
        if matches!(ext.as_str(), "xmc" | "sidc" | "pt3c" | "bpr" | "adp" | "pcm") {
            return Some(Kind::Clip);
        }
        if data.starts_with(b"Extended Module: ") {
            Some(Kind::Xm)
        } else if data.starts_with(b"PSID") || data.starts_with(b"RSID") {
            Some(Kind::Sid)
        } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WAVE") {
            Some(Kind::Wav)
        } else if data.get(44..48) == Some(b"SCRM") {
            Some(Kind::S3m)
        } else if data.starts_with(b"ProTracker 3") || data.starts_with(b"Vortex Tracker II") || ext == "pt3" {
            Some(Kind::Pt3)
        } else if mod_channels(data).is_some() {
            Some(Kind::Mod)
        } else if data.len() >= 4 && FAMILIES.iter().any(|f| data.starts_with(&f[..])) {
            Some(Kind::Clip)
        } else {
            None
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Kind::Clip => "a clip made already",
            Kind::Xm => "a FastTracker module",
            Kind::Mod => "a ProTracker module",
            Kind::S3m => "a ScreamTracker module",
            Kind::Sid => "a Commodore 64 tune",
            Kind::Pt3 => "a Pro Tracker 3 module",
            Kind::Wav => "a sound file",
        }
    }

    /// A word for the page.
    pub fn id(self) -> &'static str {
        match self {
            Kind::Clip => "clip",
            Kind::Xm => "xm",
            Kind::Mod => "mod",
            Kind::S3m => "s3m",
            Kind::Sid => "sid",
            Kind::Pt3 => "pt3",
            Kind::Wav => "wav",
        }
    }

    /// The options a file of this kind takes, besides `loop` and `once`.
    pub fn options(self) -> &'static [&'static str] {
        match self {
            Kind::Clip => &[],
            Kind::Xm | Kind::Mod | Kind::S3m => crate::xmc::OPTIONS,
            Kind::Pt3 => crate::pt3c::OPTIONS,
            Kind::Sid => crate::sidc::OPTIONS,
            Kind::Wav => crate::wav::OPTIONS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_known_by_its_bytes() {
        assert_eq!(Kind::of("a.bin", b"Extended Module: tune"), Some(Kind::Xm));
        assert_eq!(Kind::of("a", b"PSID\0\x02"), Some(Kind::Sid));
        assert_eq!(Kind::of("a", b"RIFF\x24\0\0\0WAVEfmt "), Some(Kind::Wav));
        assert_eq!(Kind::of("tune.pt3", b"ProTracker 3.6 compilation of "), Some(Kind::Pt3));
        assert_eq!(Kind::of("tune.pt3c", b"PT31...."), Some(Kind::Clip));
        assert_eq!(Kind::of("tune", b"XMC2...."), Some(Kind::Clip));
        assert_eq!(Kind::of("ring.pcm", &[128; 64]), Some(Kind::Clip));
        assert_eq!(Kind::of("ring.mp3", &[128; 64]), None);
        let mut module = vec![0u8; 1084];
        module[1080..].copy_from_slice(b"M.K.");
        assert_eq!(Kind::of("a", &module), Some(Kind::Mod));
        module[1080..].copy_from_slice(b"12CH");
        assert_eq!(mod_channels(&module), Some(12));
        module[44..48].copy_from_slice(b"SCRM");
        assert_eq!(Kind::of("a", &module), Some(Kind::S3m));
    }
}

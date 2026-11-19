// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The pack maker: files in, a clip pack for the clock's `clips`
//! partition out. A file is a tracker module (`.xm`, `.mod`, `.s3m`),
//! a Commodore 64 tune (`.sid`), a ZX Spectrum one (`.pt3`), a sound
//! file (`.wav`), or a clip made already, which goes in as it is.
//!
//! The command (`main.rs`) and the web page (`wasm.rs`) are two faces
//! of `convert` and `build`. The clips' formats, the pack's layout and
//! its limits are `onebitdawn-core`'s, and so is the parser every pack
//! made here is read back with, the one the clock reads it with.

pub mod adpcm;
pub mod bytes;
pub mod kind;
pub mod modxm;
pub mod pack;
pub mod packrefs;
pub mod pt3c;
pub mod s3mxm;
pub mod shape;
pub mod sidc;
pub mod spec;
pub mod wav;
#[cfg(target_arch = "wasm32")]
mod wasm;
pub mod web;
pub mod xmc;

pub use kind::Kind;
pub use pack::build;
pub use spec::Spec;

/// What the clock's sound goes out of, which is what a clip is shaped
/// for: the buzzer has no bass and wants the level pushed hard into
/// the limiter; a speaker plays the bass and gives the push away as
/// overdrive.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Output {
    #[default]
    Buzzer,
    Speaker,
}

impl Output {
    /// By the name the command takes; `line` is the setting's name for
    /// the pin an amplifier and its speaker are on.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "buzzer" => Some(Output::Buzzer),
            "speaker" | "line" => Some(Output::Speaker),
            _ => None,
        }
    }
}

/// A file to make a clip of.
#[derive(Clone, Debug, Default)]
pub struct Source {
    /// The file's name, a path or not: the clip's name comes of it, and
    /// its kind where the bytes do not say.
    pub file: String,
    pub data: Vec<u8>,
    /// `key` or `key=value`, as `Spec` splits them.
    pub options: Vec<(String, String)>,
    /// The label, if it is not to be the file's name in words.
    pub label: Option<String>,
}

/// A clip made, ready for a pack.
#[derive(Clone, Debug)]
pub struct Clip {
    /// The API name: `a-z` and `_`.
    pub name: String,
    pub label: String,
    /// A ringing alarm plays it round and round with no gap.
    pub loops: bool,
    pub data: Vec<u8>,
    /// What the converter has to say: what a module uses that the
    /// player lacks, where a tune was cut.
    pub notes: Vec<String>,
}

/// The file's name without its directories and its last extension.
fn stem(file: &str) -> &str {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    match base.rfind('.') {
        Some(dot) if dot > 0 => &base[..dot],
        _ => base,
    }
}

/// A clip's API name and its label of a file's name: the letters, in
/// lower case with `_` between the words, and the words capitalised.
pub fn names(file: &str) -> Option<(String, String)> {
    words(stem(file))
}

/// The same of any text.
fn words(text: &str) -> Option<(String, String)> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if words.is_empty() {
        return None;
    }
    let label: Vec<String> = words.iter().map(|w| w[..1].to_ascii_uppercase() + &w[1..]).collect();
    Some((words.join("_"), label.join(" ")))
}

/// The title a tune carries in its file, as a label: what the clock's
/// pages can show of it (no control character, none of `< > & "`), in
/// `LABEL_MAX` bytes. `None` for a kind with no title, or a title with
/// no letter or digit in it.
pub fn title(kind: Kind, data: &[u8]) -> Option<String> {
    let field = match kind {
        Kind::Xm => data.get(17..37),
        Kind::Mod => data.get(..20),
        Kind::S3m => data.get(..28),
        Kind::Sid => data.get(0x16..0x36),
        Kind::Pt3 => data.get(30..62),
        Kind::Wav | Kind::Clip => None,
    }?;
    // The files are older than UTF-8: Latin-1 is the nearest guess.
    let text: String = bytes::text(field).chars().map(|c| if c.is_control() || matches!(c, '<' | '>' | '&' | '"') { ' ' } else { c }).collect();
    let mut label = String::new();
    for word in text.split_whitespace() {
        let more = if label.is_empty() { word.len() } else { word.len() + 1 };
        if label.len() + more > onebitdawn_core::pack::LABEL_MAX {
            break;
        }
        if !label.is_empty() {
            label.push(' ');
        }
        label.push_str(word);
    }
    label.chars().any(char::is_alphanumeric).then_some(label)
}

/// The name and the label a file's clip has when nobody gives it any:
/// the tune's title where it has one, and the file's name where it has
/// not; the name is the file's too when the title's would not do (no
/// letters, too long, a melody's). The name may still be too long.
pub fn natural(file: &str, data: &[u8]) -> Option<(String, String)> {
    let Some(label) = Kind::of(file, data).and_then(|kind| title(kind, data)) else {
        return names(file);
    };
    let fits = |name: &String| name.len() <= onebitdawn_core::pack::NAME_MAX && !onebitdawn_core::melody::is_reserved(name);
    let name = words(&label).map(|w| w.0).filter(fits).or_else(|| names(file).map(|n| n.0))?;
    Some((name, label))
}

/// The options a converter takes, each asked for by name; what is left
/// at the end is a mistake of the user's.
pub struct Options {
    left: Vec<(String, String)>,
}

impl Options {
    fn new(source: &Source) -> Self {
        Options { left: source.options.clone() }
    }

    fn take(&mut self, key: &str) -> Option<String> {
        let at = self.left.iter().position(|(k, _)| k == key)?;
        Some(self.left.remove(at).1)
    }

    /// An option with no value: there or not.
    pub fn flag(&mut self, key: &str) -> Result<bool, String> {
        match self.take(key) {
            None => Ok(false),
            Some(v) if v.is_empty() => Ok(true),
            Some(_) => Err(format!("the option {key} takes no value")),
        }
    }

    /// An option's value as text.
    pub fn text(&mut self, key: &str) -> Result<Option<String>, String> {
        match self.take(key) {
            Some(v) if v.is_empty() => Err(format!("the option {key} needs a value, {key}=…")),
            other => Ok(other),
        }
    }

    /// An option's value as a number, or `default`.
    pub fn number<T: core::str::FromStr>(&mut self, key: &str, default: T) -> Result<T, String> {
        match self.text(key)? {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("the option {key}={v} is not a number it can be")),
        }
    }

    /// An option's value as a number, if it was given.
    pub fn maybe<T: core::str::FromStr>(&mut self, key: &str) -> Result<Option<T>, String> {
        match self.text(key)? {
            None => Ok(None),
            Some(v) => v.parse().map(Some).map_err(|_| format!("the option {key}={v} is not a number it can be")),
        }
    }

    /// Nothing may be left: `known` is what this kind of file takes.
    fn finish(self, kind: Kind) -> Result<(), String> {
        match self.left.first() {
            None => Ok(()),
            Some((key, _)) => {
                let known = kind.options();
                let takes = if known.is_empty() { "name, loop and once".into() } else { format!("name, loop, once, {}", known.join(", ")) };
                Err(format!("no option {key} for {}; it takes {takes}", kind.text()))
            }
        }
    }
}

/// The clip's bytes, and what the options say of its looping.
fn made(source: &Source, kind: Kind, output: Output, notes: &mut Vec<String>) -> Result<(Vec<u8>, Option<bool>), String> {
    let mut options = Options::new(source);
    options.take("name");
    let asked = match (options.flag("loop")?, options.flag("once")?) {
        (true, true) => return Err("loop or once, not both".into()),
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    };
    let data = match kind {
        Kind::Clip => Ok(source.data.clone()),
        Kind::Xm | Kind::Mod | Kind::S3m => xmc::Shape::options(output, &mut options).and_then(|shape| {
            let module = match kind {
                Kind::Mod => modxm::convert(&source.data, notes)?,
                Kind::S3m => s3mxm::convert(&source.data, notes)?,
                _ => source.data.clone(),
            };
            xmc::convert(&module, &shape, notes)
        }),
        Kind::Pt3 => pt3c::Shape::options(output, &mut options).and_then(|shape| pt3c::convert(&source.data, &shape, notes)),
        Kind::Sid => sidc::Shape::options(output, &mut options).and_then(|shape| sidc::convert(&source.data, &shape, notes)),
        Kind::Wav => wav::Shape::options(output, &mut options).and_then(|shape| wav::convert(&source.data, &shape, notes)),
    }?;
    options.finish(kind)?;
    Ok((data, asked))
}

/// Make a clip of a file.
pub fn convert(source: &Source, output: Output) -> Result<Clip, String> {
    let file = &source.file;
    // `name=` is for a clip whose own name is too long or taken; the
    // label is then that name's, unless there is one. Otherwise both
    // are the tune's title's, or the file's name's.
    let asked = source.options.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone());
    let (name, words) = match asked {
        Some(name) => {
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                return Err(format!("{file}: name={name}: a name is a-z and _"));
            }
            let words = names(&name).map_or(name.clone(), |n| n.1);
            (name, words)
        }
        None => natural(file, &source.data).ok_or_else(|| format!("{file}: the file's name needs letters, they are the clip's name; or give it one, name=…"))?,
    };
    let label = source.label.clone().unwrap_or(words);
    pack::check_names(file, &name, &label)?;
    let kind = Kind::of(file, &source.data).ok_or_else(|| {
        format!("{file}: not a file this knows: .xm, .mod, .s3m, .sid, .pt3, .wav, or a clip made already (.xmc, .sidc, .pt3c, .bpr, .adp, .pcm)")
    })?;
    let mut notes = Vec::new();
    let (data, asked) = made(source, kind, output, &mut notes).map_err(|e| format!("{file}: {e}"))?;
    if onebitdawn_core::clip::is_unsupported(&data) {
        let magic = String::from_utf8_lossy(&data[..data.len().min(4)]).into_owned();
        return Err(format!("{file}: a clip of the version {magic}, which the firmware does not play; convert it again"));
    }
    if onebitdawn_core::clip::duration_ms(&data) < pack::CLIP_MIN_MS {
        return Err(format!("{file}: less than a second of sound, which the clock takes for no clip"));
    }
    let loops = asked.unwrap_or_else(|| kind::is_tune(&data));
    Ok(Clip { name, label, loops, data, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_the_files_letters() {
        assert_eq!(names("mods/Orion - Nero Burning ROM v5.5.7.8kg.xm").unwrap().0, "orion_nero_burning_rom_v_kg");
        assert_eq!(names("clips/trantor.bpr").unwrap(), ("trantor".into(), "Trantor".into()));
        assert_eq!(names("C:\\tunes\\nq-synchronization.PT3").unwrap(), ("nq_synchronization".into(), "Nq Synchronization".into()));
        assert_eq!(names("ORiON.xm").unwrap().1, "Orion");
        assert!(names("1234.wav").is_none());
    }

    #[test]
    fn a_tune_is_named_by_its_title() {
        let mut module = b"Extended Module: keygentune2003 orn  \x1a".to_vec();
        module.resize(400, 0);
        assert_eq!(natural("Orion-Nero6.3.0.2.xm", &module).unwrap(), ("keygentune_orn".into(), "keygentune2003 orn".into()));
        // No title, or none to speak of: the file's name.
        module[17..37].fill(b' ');
        assert_eq!(natural("a tune.xm", &module).unwrap(), ("a_tune".into(), "A Tune".into()));
        module[17..37].copy_from_slice(b"-=*  <<<\0>>>  *=-   ");
        assert_eq!(natural("a tune.xm", &module).unwrap().1, "A Tune");
        // A title a page cannot show as it is, cut by its field of 32
        // bytes, and too long for a name.
        let mut sid = b"PSID".to_vec();
        sid.resize(0x16, 0);
        sid.extend(b"Caf\xe9 <Noir> & the \"Long\" Goodbye!\0");
        sid.resize(0x7c, 0);
        assert_eq!(natural("cafe.sid", &sid).unwrap(), ("cafe".into(), "Caf\u{e9} Noir the Long Goodbye".into()));
        // A melody's name is not a clip's.
        let mut module = b"beeps".to_vec();
        module.resize(1084, 0);
        module[1080..].copy_from_slice(b"M.K.");
        assert_eq!(natural("tune.mod", &module).unwrap(), ("tune".into(), "beeps".into()));
        assert_eq!(natural("ring.wav", b"RIFF\0\0\0\0WAVEfmt ").unwrap().1, "Ring");
    }

    #[test]
    fn an_option_nobody_takes_is_refused() {
        let mut pcm = b"PCM1\x80\x3e\0\0".to_vec();
        pcm.extend([128u8; 16000]);
        let mut source = Source { file: "ring.pcm".into(), data: pcm, ..Default::default() };
        let clip = convert(&source, Output::Buzzer).unwrap();
        assert!(!clip.loops && clip.name == "ring" && clip.label == "Ring");
        source.options = vec![("loop".into(), String::new())];
        assert!(convert(&source, Output::Buzzer).unwrap().loops);
        source.options = vec![("song".into(), "2".into())];
        assert!(convert(&source, Output::Buzzer).unwrap_err().contains("no option song"));
        source.options.clear();
        source.data.truncate(4000);
        assert!(convert(&source, Output::Buzzer).unwrap_err().contains("less than a second"));
        source.data[3] = b'7';
        assert!(convert(&source, Output::Buzzer).unwrap_err().contains("PCM7"));
    }
}

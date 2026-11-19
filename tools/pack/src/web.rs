// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! What the web page asks of the library, in text: the page gives
//! files and gets clips, names them and gets a pack, and hears a clip
//! through the players the clock has. `wasm.rs` is these functions
//! over a C ABI; they are here, with no pointers, for the host's tests.

use onebitdawn_core::clip::{self, Stream};
use onebitdawn_core::{adpcm, pt3, sid, xm};
use onebitdawn_core::pack::{LABEL_MAX, MAX_CLIPS, NAME_MAX, ROOM};

use crate::{Clip, Kind, Output, Source, pack, spec};

/// Samples a second a beeper clip is heard at: its line's bits, sixteen
/// to a sample.
const BEEPER_HZ: u32 = clip::BEEPER_BIT_RATE / 16;

/// What gives a clip's samples: the player the clock has for its kind.
enum Voice {
    Module(Box<xm::Tune<'static>>),
    Sid(Box<sid::Tune<'static>>),
    Ay(Box<pt3::Tune<'static>>),
    Adpcm(adpcm::Decoder<'static>),
    /// 8-bit samples and the next of them.
    Pcm(&'static [u8], usize),
    /// A beeper clip: the line's bits, sixteen to a sample.
    Bits(Box<Stream<'static>>),
}

/// A clip being heard.
struct Preview {
    // Before `data`, which it borrows: dropped first.
    voice: Voice,
    _data: Box<[u8]>,
}

/// A page's clips.
#[derive(Default)]
pub struct Session {
    clips: Vec<Option<Clip>>,
    preview: Option<Preview>,
}

/// A string as JSON.
fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The pack's limits, for the page to show and to hold its fields to.
pub fn limits() -> String {
    format!("{{\"room\":{ROOM},\"clips\":{MAX_CLIPS},\"name\":{NAME_MAX},\"label\":{LABEL_MAX},\"header\":{},\"entry\":{}}}", pack::total(&[]), onebitdawn_core::pack::ENTRY_LEN)
}

impl Session {
    /// A clip of a file, kept under the number in the JSON that
    /// describes it. `options` is what follows the colon in the
    /// command's argument; `speaker` is `--sound-output speaker`.
    pub fn convert(&mut self, file: &str, data: &[u8], options: &str, speaker: bool) -> Result<String, String> {
        let options = spec::parse_options(options).ok_or_else(|| format!("{file}: the options are key=value, with commas between them"))?;
        let mut source = Source { file: file.to_string(), data: data.to_vec(), options, label: None };
        // A name too long for a clip's is cut to fit and not refused:
        // the page has fields to put it right in.
        if let Some((name, label)) = crate::natural(file, data)
            && name.len() > NAME_MAX
            && !source.options.iter().any(|(k, _)| k == "name")
        {
            source.options.push(("name".into(), name[..NAME_MAX].trim_end_matches('_').to_string()));
            // The label is not that name's for it.
            let end = (0..=LABEL_MAX.min(label.len())).rev().find(|&k| label.is_char_boundary(k)).unwrap_or(0);
            source.label = Some(label[..end].trim_end().to_string());
        }
        let kind = Kind::of(file, data);
        let made = crate::convert(&source, if speaker { Output::Speaker } else { Output::Buzzer })?;
        let notes: Vec<String> = made.notes.iter().map(|n| quoted(n)).collect();
        let takes: Vec<String> = kind.map_or(&[][..], Kind::options).iter().map(|o| quoted(o)).collect();
        let free = self.clips.iter().position(Option::is_none).unwrap_or(self.clips.len());
        let json = format!(
            "{{\"id\":{free},\"name\":{},\"label\":{},\"loops\":{},\"bytes\":{},\"ms\":{},\"kind\":{},\"options\":[{}],\"notes\":[{}]}}",
            quoted(&made.name),
            quoted(&made.label),
            made.loops,
            made.data.len(),
            clip::duration_ms(&made.data),
            quoted(kind.map_or("", Kind::id)),
            takes.join(","),
            notes.join(",")
        );
        if free == self.clips.len() {
            self.clips.push(None);
        }
        self.clips[free] = Some(made);
        Ok(json)
    }

    /// Forget a clip.
    pub fn forget(&mut self, id: usize) {
        if let Some(slot) = self.clips.get_mut(id) {
            *slot = None;
        }
    }

    /// The pack of the clips a line each names, in that order:
    /// `id<TAB>name<TAB>label<TAB>1 or 0`, the last for whether a
    /// ringing alarm plays it round and round.
    pub fn build(&self, lines: &str) -> Result<Vec<u8>, String> {
        let mut clips = Vec::new();
        for line in lines.lines().filter(|l| !l.is_empty()) {
            let fields: Vec<&str> = line.split('\t').collect();
            let &[id, name, label, loops] = &fields[..] else {
                return Err("a line of the pack is not a clip".into());
            };
            let made = id.parse::<usize>().ok().and_then(|id| self.clips.get(id)).and_then(Option::as_ref).ok_or("a clip that is not there any more")?;
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                return Err(format!("{label}: the name {name:?} is not a-z and _"));
            }
            pack::check_names(label, name, label)?;
            clips.push(Clip { name: name.into(), label: label.into(), loops: loops == "1", data: made.data.clone(), notes: Vec::new() });
        }
        pack::build(&clips)
    }

    /// Start hearing a clip, once through: the samples a second
    /// `samples` will give, 0 if there is no such clip.
    pub fn preview(&mut self, id: usize) -> u32 {
        self.preview = None;
        let Some(made) = self.clips.get(id).and_then(Option::as_ref) else {
            return 0;
        };
        let data: Box<[u8]> = made.data.clone().into_boxed_slice();
        // SAFETY: the stream reads `data`'s heap, which does not move
        // when the box does and is freed only after the stream, the
        // field before it in `Preview`; neither leaves the struct.
        let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(data.as_ptr(), data.len()) };
        let (voice, rate) = if clip::is_beeper(bytes) {
            (Voice::Bits(Box::new(Stream::new(bytes, false))), BEEPER_HZ)
        } else if adpcm::is_clip(bytes) {
            (Voice::Adpcm(adpcm::Decoder::new(bytes)), adpcm::rate(bytes))
        } else if xm::is_tune(bytes) {
            let tune = Box::new(xm::Tune::new(bytes));
            let rate = tune.rate();
            (Voice::Module(tune), rate)
        } else if sid::is_tune(bytes) {
            let tune = Box::new(sid::Tune::new(bytes));
            let rate = tune.rate();
            (Voice::Sid(tune), rate)
        } else if pt3::is_tune(bytes) {
            let tune = Box::new(pt3::Tune::new(bytes));
            let rate = tune.rate();
            (Voice::Ay(tune), rate)
        } else {
            (Voice::Pcm(clip::pcm_samples(bytes), 0), clip::pcm_rate(bytes))
        };
        self.preview = Some(Preview { voice, _data: data });
        rate
    }

    /// The next samples of the clip being heard, between -1 and 1; how
    /// many, fewer than asked for at its end and 0 after. Ask for a
    /// multiple of 64.
    pub fn samples(&mut self, out: &mut [f32]) -> usize {
        let Some(preview) = &mut self.preview else {
            return 0;
        };
        // A player's sample is in `xm::STEP`ths of PCM's 8-bit step.
        let scale = |s: Option<i16>| s.map(|s| s as f32 / (128 * xm::STEP) as f32);
        let mut given = 0;
        if let Voice::Bits(stream) = &mut preview.voice {
            if !stream.done() {
                let mut raw = vec![0u8; out.len() * 2];
                stream.fill(&mut raw);
                for (o, pair) in out.iter_mut().zip(raw.as_chunks::<2>().0) {
                    // The share of the bits that are ones, about nought.
                    *o = (pair[0].count_ones() + pair[1].count_ones()) as f32 / 16.0 - 0.5;
                }
                given = out.len();
            }
        } else {
            for o in out.iter_mut() {
                let sample = match &mut preview.voice {
                    Voice::Module(tune) => scale(tune.next_sample()),
                    Voice::Sid(tune) => scale(tune.next_sample()),
                    Voice::Ay(tune) => scale(tune.next_sample()),
                    Voice::Adpcm(decoder) => scale(decoder.next_sample()),
                    Voice::Pcm(samples, next) => samples.get(*next).map(|&s| {
                        *next += 1;
                        (s as f32 - 128.0) / 128.0
                    }),
                    Voice::Bits(_) => None,
                };
                let Some(sample) = sample else { break };
                *o = sample.clamp(-1.0, 1.0);
                given += 1;
            }
        }
        if given == 0 {
            self.preview = None;
        }
        given
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm() -> Vec<u8> {
        let mut data = b"PCM1\x80\x3e\0\0".to_vec();
        data.extend((0..16000).map(|k| if k / 16 % 2 == 0 { 200u8 } else { 56 }));
        data
    }

    #[test]
    fn the_page_makes_a_pack() {
        let mut session = Session::default();
        let json = session.convert("My \"Ring\".pcm", &pcm(), "loop", false).unwrap();
        assert_eq!(json, r#"{"id":0,"name":"my_ring","label":"My Ring","loops":true,"bytes":16008,"ms":1000,"kind":"clip","options":[],"notes":[]}"#);
        assert!(session.convert("b.pcm", &pcm(), "", true).unwrap().starts_with(r#"{"id":1,"name":"b","#));
        let long = session.convert("Orion - Nero Burning ROM v5.5.7.8kg and a great deal more.pcm", &pcm(), "", false).unwrap();
        assert!(long.contains(r#""name":"orion_nero_burning_r","label":"Orion Nero Burning Rom V Kg And","#), "{long}");
        session.forget(2);
        assert!(session.convert("b.pcm", &pcm(), "song=", false).unwrap_err().contains("key=value"));
        assert!(session.convert("b.txt", b"hello, world", "", false).is_err());
        let made = session.build("1\tsecond\tThe Second\t0\n0\tfirst\tThe First\t1\n").unwrap();
        let pack = onebitdawn_core::pack::Pack::parse(&made).unwrap();
        let (a, b) = (pack.clip(0).unwrap(), pack.clip(1).unwrap());
        assert!(a.name == "second" && a.label == "The Second" && !a.loops && b.name == "first" && b.loops);
        assert!(session.build("0\tbeeps\tBeeps\t0\n").unwrap_err().contains("melody"));
        assert!(session.build("0\tBad Name\tx\t0\n").unwrap_err().contains("a-z"));
        assert!(session.build("0\ta\tA\t0\n0\ta\tB\t0\n").unwrap_err().contains("two clips"));
        session.forget(1);
        assert!(session.build("1\tb\tB\t0\n").unwrap_err().contains("not there"));
        // The number is given again.
        assert!(session.convert("c.pcm", &pcm(), "", false).unwrap().starts_with(r#"{"id":1,"#));
        assert!(limits().starts_with("{\"room\":1179648,\"clips\":16,\"name\":20,\"label\":32,\"header\":44,\"entry\":64"));
    }

    #[test]
    fn a_clip_is_heard_to_its_end() {
        let mut session = Session::default();
        session.convert("a.pcm", &pcm(), "", false).unwrap();
        assert_eq!(session.preview(7), 0);
        assert_eq!(session.preview(0), 16000);
        let (mut heard, mut loud) = (0, 0.0f32);
        let mut buf = [0f32; 1024];
        loop {
            let n = session.samples(&mut buf);
            if n == 0 {
                break;
            }
            heard += n;
            loud = buf[..n].iter().fold(loud, |m, s| m.max(s.abs()));
        }
        // The second of sound, a square wave 72 steps of 128 high.
        assert!(heard == 16000 && loud == 0.5625, "{heard} samples, {loud}");
        assert_eq!(session.samples(&mut buf), 0);
    }
}

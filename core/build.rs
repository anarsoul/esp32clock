// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    // `config.rs` reads these with `option_env!`, so this crate is what must rebuild when they change.
    for var in ["WIFI_SSID", "WIFI_PASSWORD", "TZ_OFFSET_MINUTES", "TZ_RULE", "API_TOKEN", "MAX_CURRENT_STEP", "AP_PASSWORD", "ESP_RADIO_CONFIG_WIFI_MTU", "MQTT_HOST", "MQTT_PORT", "MQTT_USER", "MQTT_PASSWORD", "CLIP_KBPS", "CLIP_CARRIER_KHZ", "CLIP_MODULATOR", "ALARM_CLIP", "ALARM_CLIP_LABEL"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    clips();
}

/// The clips a build has when `ALARM_CLIP` is not set: the ones in the
/// repository. Set it to nothing for a build with none.
const DEFAULT_CLIPS: &str = "clips/trantor.bpr,clips/halftime.xmc,clips/keygen.xmc";

/// `ALARM_CLIP` names the clips to build in as the last melodies, files
/// separated by commas: raw PCM (`scripts/wav2pcm.sh`), a beeper clip
/// (`scripts/zxbeeper.py`) or a tracker module (`scripts/xm2clip.py`).
/// A relative path is taken from the workspace root, where cargo is run.
/// A clip's API name is its file's name without the extension, in lower
/// case with everything but letters turned into `_`; its label is the
/// same in words, or the entry of `ALARM_CLIP_LABEL`, also separated by
/// commas, at the same place.
///
/// A ringing alarm plays a clip round and round with no gap if the clip
/// is made to go round, and with a pause between passes, as the note
/// melodies have, if it is not. A beeper clip and a module are taken to
/// be, PCM not; `:loop` or `:once` after a file's name says otherwise.
/// `melody.rs` includes what this writes.
fn clips() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent");
    let files = std::env::var("ALARM_CLIP").unwrap_or_else(|_| DEFAULT_CLIPS.to_string());
    let labels = std::env::var("ALARM_CLIP_LABEL").unwrap_or_default();
    let labels: Vec<&str> = labels.split(',').map(str::trim).collect();
    let mut names = String::new();
    let mut data = String::new();
    let mut loops = String::new();
    let mut count = 0;
    for (i, file) in files.split(',').map(str::trim).filter(|f| !f.is_empty()).enumerate() {
        let (file, asked) = match file.rsplit_once(':') {
            Some((file, "loop")) => (file, Some(true)),
            Some((file, "once")) => (file, Some(false)),
            _ => (file, None),
        };
        let mut path = PathBuf::from(file);
        if path.is_relative() {
            path = root.join(path);
        }
        let path = path.canonicalize().unwrap_or_else(|e| panic!("ALARM_CLIP {}: {e}", path.display()));
        println!("cargo:rerun-if-changed={}", path.display());
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let words: Vec<String> = stem
            .split(|c: char| !c.is_ascii_alphabetic())
            .filter(|w| !w.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();
        assert!(!words.is_empty(), "ALARM_CLIP {file}: the file's name needs letters, they are the melody's name");
        let name = words.join("_");
        let label = match labels.get(i) {
            Some(label) if !label.is_empty() => label.to_string(),
            _ => words.iter().map(|w| w[..1].to_ascii_uppercase() + &w[1..]).collect::<Vec<_>>().join(" "),
        };
        writeln!(names, "    ({name:?}, {label:?}),").unwrap();
        writeln!(data, "    include_bytes!({:?}),", path.display().to_string()).unwrap();
        // The clips with a header are the ones made from music written
        // to loop: `clip::BEEPER_MAGIC` and `xm::MAGIC`.
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("ALARM_CLIP {}: {e}", path.display()));
        let made_to = bytes.starts_with(b"BPR") || bytes.starts_with(b"XMC");
        writeln!(loops, "    {},", asked.unwrap_or(made_to)).unwrap();
        count += 1;
    }
    let out = format!(
        "pub const CLIP_COUNT: usize = {count};\n\
         pub const CLIP_NAMES: [(&str, &str); CLIP_COUNT] = [\n{names}];\n\
         pub static CLIP_DATA: [&[u8]; CLIP_COUNT] = [\n{data}];\n\
         pub const CLIP_LOOPS: [bool; CLIP_COUNT] = [\n{loops}];\n"
    );
    let dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    std::fs::write(Path::new(&dir).join("clips.rs"), out).expect("write clips.rs");
}

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

/// `ALARM_CLIP` names the clips to build in as the last melodies, files
/// separated by commas: raw PCM (`scripts/wav2pcm.sh`), a beeper clip
/// (`scripts/zxbeeper.py`) or a two-channel tune (`scripts/wham2clip.py`).
/// A relative path is taken from the workspace root, where cargo is run.
/// A clip's API name is its file's name without the extension, in lower
/// case with everything but letters turned into `_`; its label is the
/// same in words, or the entry of `ALARM_CLIP_LABEL`, also separated by
/// commas, at the same place. `melody.rs` includes what this writes.
fn clips() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent");
    let files = std::env::var("ALARM_CLIP").unwrap_or_default();
    let labels = std::env::var("ALARM_CLIP_LABEL").unwrap_or_default();
    let labels: Vec<&str> = labels.split(',').map(str::trim).collect();
    let mut names = String::new();
    let mut data = String::new();
    let mut count = 0;
    for (i, file) in files.split(',').map(str::trim).filter(|f| !f.is_empty()).enumerate() {
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
        count += 1;
    }
    let out = format!(
        "pub const CLIP_COUNT: usize = {count};\n\
         pub const CLIP_NAMES: [(&str, &str); CLIP_COUNT] = [\n{names}];\n\
         pub static CLIP_DATA: [&[u8]; CLIP_COUNT] = [\n{data}];\n"
    );
    let dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    std::fs::write(Path::new(&dir).join("clips.rs"), out).expect("write clips.rs");
}

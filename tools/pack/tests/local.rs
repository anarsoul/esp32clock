// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Vasily Khoruzhick

//! The converters over whatever music the machine has: the ignored
//! folders of the repository, where the clips that may not be
//! redistributed are kept, hold files of every kind. None is named
//! here and none need be there; the CI has only `mods/` and `clips/`.
//!
//! There is nothing to compare a clip with, so what is asked is what
//! the clock asks: the file converts or is refused in words (never a
//! panic), the clip makes a pack the clock's parser takes, and the
//! clock's player plays it.

use std::path::Path;

use onebitdawn_core::pack::Pack;
use onebitdawn_pack::web::Session;
use onebitdawn_pack::Kind;

const FOLDERS: [&str; 7] = ["mods", "clips", "xm", "pt3", "sid", "wav", "tap"];

#[test]
fn every_file_at_hand_converts_and_plays() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (mut made, mut refused) = (0, 0);
    for folder in FOLDERS {
        let Ok(entries) = std::fs::read_dir(root.join(folder)) else {
            continue;
        };
        let mut files: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        for path in files {
            let data = std::fs::read(&path).unwrap();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            let Some(kind) = Kind::of(&file, &data) else {
                continue;
            };
            // Under a name of its own: a file's may be too long, or a
            // melody's, and that is not what is tried here. A quarter
            // of a minute of a sound file is as good as all of it.
            let options = if kind == Kind::Wav { "name=local,seconds=15" } else { "name=local" };
            for speaker in [false, true] {
                let mut session = Session::default();
                if let Err(why) = session.convert(&file, &data, options, speaker) {
                    assert!(why.starts_with(&file), "{why}");
                    println!("  refused: {why}");
                    refused += 1;
                    break;
                }
                // Sampled sound may be more than the partition holds.
                match session.build("0\tlocal\tLocal\t0") {
                    Ok(pack) => {
                        let pack = Pack::parse(&pack).unwrap_or_else(|e| panic!("{file}: {}", e.text()));
                        assert!(pack.count() == 1 && pack.clip(0).unwrap().name == "local");
                    }
                    Err(why) => assert!(why.contains("the clips partition holds"), "{file}: {why}"),
                }
                // Ten seconds of it through the page's preview, which
                // is the clock's player for the kind.
                let rate = session.preview(0);
                assert!((8000..=32000).contains(&rate), "{file}: {rate} Hz");
                let (mut heard, mut loud) = (0, 0f32);
                let mut buf = [0f32; 4096];
                while heard < 10 * rate as usize {
                    let n = session.samples(&mut buf);
                    if n == 0 {
                        break;
                    }
                    heard += n;
                    loud = buf[..n].iter().fold(loud, |m, s| m.max(s.abs()));
                }
                assert!(heard >= rate as usize && loud > 0.01 && loud <= 1.0, "{file}: {heard} samples, the loudest {loud}");
                made += 1;
            }
        }
    }
    println!("{made} clips made and played, {refused} files refused");
    // The repository's own are always there.
    assert!(made >= 14, "{made}");
}

# Clip tooling reference

The pack maker that makes clips and packs, the beeper converter and
the build-time variables of the sound path, with the settings the
default pack's clips were made with and who chose them by ear. The
players are in `agents/modules.md`; the buzzer stage is in
`agents/hardware.md`.

## The pack maker (`tools/pack`, `onebitdawn-pack`)

```
cargo pack -o packs/mine.bin [--sound-output buzzer|speaker] file[:option,…][=Label] …
cargo pack --clip clips/tune.xmc tune.xm        # one file's clip and no pack
cargo pack --help                               # every kind's options
cargo pack-test                                 # its tests
cargo pack-wasm && cp target/wasm32-unknown-unknown/tool/onebitdawn_pack.wasm docs/pack/
(cd docs && python3 -m http.server)             # the page, at /pack/
```

One library, twice: the command (`src/main.rs`) and, as WebAssembly
with a C ABI written by hand (`src/wasm.rs` over `src/web.rs`; no
binding generator, no npm), the page `docs/pack/index.html`, which the
release workflow deploys beside the installer with the `.wasm` it
builds (the file is ignored by git). The page previews a clip through
`core`'s players (`web::Session::preview`) and gives the pack as a
download; it does not post to a clock, which `http::admit` would
refuse from a foreign `Origin`.

It was ported from the Python converters the clips of 2026 were made
with (`scripts/xm2clip.py`, `mod2xm.py`, `s3m2xm.py`, `pt32clip.py`,
`sid2clip.py`, `adpcm.py`, `mkpack.py`, gone 2026-11-19; they are in
the history), byte for byte: its tests hold it to `clips/` and to
`tools/pack/tests/data/`, the Python's output for three small files,
to the Python 6502's state after four thousand random programs and to
the Python ADPCM encoder's codes. Keep them passing: a clip a clock
has must come out the same.

- A file's kind is by its bytes, then its extension (`kind.rs`); a
  clip made already goes in as it is. A clip's label is the tune's
  title where its file has one (XM, MOD, S3M, SID, PT3; `title` and
  `natural` in `lib.rs`), cleaned to what a page shows, and its name
  the title's letters; with no title, or one that makes no name, the
  file's name as `ALARM_CLIP`'s. `name=` and `=Label` give others
  (the user asked for the titles, 2026-11-19).
- `--sound-output` chooses each kind's defaults, `Shape::new` in its
  module: `buzzer` is what the default pack's clips were made with,
  `speaker` (or `line`) what `scripts/speaker-clips.sh` made, so that
  script is now the default pack's sources through it.
- Every pack is read back by `core::pack::Pack::parse` before it is
  given out, and the limits are `core`'s constants.

### Sound files (`wav.rs`, `adpcm.rs`)

- The steps are those of the sox line the PCM clips were made with
  (`scripts/wav2pcm.sh`, gone with the rest): mono, the cut in time,
  the high-pass, the new rate (a Kaiser-windowed sinc), the peak to
  -0.5 dB, the drive into a limiter with no memory (sox's `gain -l`,
  `g x / (1 + (g - 1)|x|)`, measured against sox to seven places), the
  peak again, a fade of 1.5 s if it was cut. Not sox's bytes: against
  sox's output for the same file the difference is 28 to 34 dB under
  the signal at 8 bits (2026-11-19), dither and the resampler's band
  edge. Not yet heard on a clock.
- The buzzer's defaults are 900 Hz twice and 20 dB of drive; a
  speaker's 100 Hz once and 10 dB (`CLIP_HIGHPASS=100 CLIP_DRIVE=10`
  as it was).
- ADPCM by default (`ADP1`, four bits a sample, 8 KB a second at
  16 kHz), encoded with the four best paths kept; `format=pcm` is
  8 bits with dither, behind a `PCM1` header, or bare at 8 kHz. The
  user's ear on white-clock, 2026-11-16, of ADPCM against PCM: "not
  bad", a difference heard, "way better than PCM 8k", which is the
  same flash; and of the clock's own decoder on a speaker: "audible,
  but acceptable".

### Tracker modules (`xmc.rs`, `modxm.rs`, `s3mxm.rs`)

- A FastTracker 2 module for the clock's own player, of up to twenty
  channels; the notes list what the module uses that the player
  lacks. A ProTracker module (31 samples, any channel count the tag
  names) becomes an XM first: the pitch a PAL Amiga's, the slides
  (1, 2, 3, 5, E1, E2) converted from periods to the linear table at
  the period each is played at, E0 (the filter) left out and named. A
  ScreamTracker 3 module the same: samples, notes, volumes and the
  effects A, B, C, D, O and T, any other named; no panning.
- The bass cut, an instrument's octave (`raise=`) and the limiter are
  the clip's. `knee` is the level the limiter starts at, in 1024s of
  the mix where one voice is 32. The default is 20 dB of drive, for
  the buzzer; a knee above the tune's peak is the limiter off.
- The rate: the clip is at `clip::FAST_HZ`, 31 250 samples a second,
  here and in the two kinds below, which the chip's converter at 256
  bits a sample sends at 8 Mbit/s exactly. It is the default since
  2026-11-13, heard on the breadboard stage and on white-clock. 32 kHz
  is not among the converter's whole dividers, and at
  `CLIP_PDM_BITS=384` such a clip is refused. `rate=16000` is the rate
  before it, half a module's share of the processor, and 8000 a
  module's third.
- A speaker's defaults are a 100 Hz high-pass and the limiter's
  release at 128 ms (`release=10`); heard and kept by the user
  2026-11-15, after a pack with the limiter off was too quiet.

### SID tunes (`sidc.rs`)

- A Commodore 64 tune (PSID) for the clock's own SID. It runs the
  tune's 6502 code on the host and keeps each frame's register
  writes, to the point where the tune loops (the machine's state is
  hashed after every call), and says what the tune does that the
  clock will not play.
- `mute=` keeps the voices' gates closed for the first frames, for a
  routine that clicks as it starts. `system=` plays it as the other
  machine would, whatever the file says: a PAL tune on an NTSC C64 is
  59.83 calls a second for 50.12, the pitch 3.8 % up.
- `flat`, for a speaker: no limiter, `drive` then a plain gain with
  the three voices at their top as full scale (flags bit 1; a firmware
  before 2026-11-15 plays such a clip limited).
- The record is written packed (`SID2`: writes and waits the tune has
  had before are references to them, `packrefs.rs`, a fifth of the
  bytes for a long tune); `plain` for `SID1`.
- A speaker's defaults are `flat` and the high-pass at 150 Hz twice
  (`highpass=150,passes=2`): the user's ear on white-clock's speaker,
  2026-11-19. A module's (the limiter with `release=10`, 100 Hz once)
  were tried first and heard as clipping, then flat with 100 Hz once
  as "almost good, but a bit bass-heavy". `limiter` asks for the
  limiter all the same.

### PT3 modules (`pt3c.rs`)

- A ZX Spectrum tune (a Pro Tracker 3 or Vortex Tracker II module)
  for the clock's own player and AY-3-8910. The clip is the module as
  it is behind a header with the machine's clock and frame rate (by
  the module's tone table unless told), the length and where it goes
  round. `restart` is for a module whose loop is only its ending.
- The defaults are the default pack's Synchronization: the limiter,
  the noise at 16, a 100 Hz high-pass once, `release=10`, since
  2026-11-16 (the user, after hearing the two on a speaker: "beeper
  should not be flat for sure"). Not heard on a buzzer yet. A
  speaker's add a gentler limiter, `knee=37` and `drive=150` (the
  user's ear, 2026-11-16, on test-clock's speaker; needs that day's
  firmware, an older one limits hard).
- `flat` is the chip as it is but for the high-pass, at a plain gain
  3 dB over three channels at full scale (`drive` 180): the user's
  choice of 2026-11-13, on headphones and on white-clock, over the
  limiter, and the converter's default until the pack's clip changed.
- `knee` is kept in the flags' top four bits as one of fifteen steps
  2 dB apart (`core::pt3::KNEES`).

## Beeper (`scripts/zxbeeper.py`)

```
~/.local/pipx/venvs/skoolkit/bin/python scripts/zxbeeper.py game.tap tap/tune.bpr --seconds 60 [--stretch 4] [--settle 20 --start 64000]
scripts/zxbeeper.py old.bpr new.bpr
```

- The first is what a ZX Spectrum program plays on its beeper, as a
  clip. It needs SkoolKit; `--start` is for a loader that waits for a
  key. The clip is written packed (`BPR3`, about half the bytes, the
  same runs); `--plain` for `BPR2`.
- The second packs a beeper clip again, or with `--plain` unpacks it;
  no SkoolKit.

## Packs

```
CLIP_PACK_FILE=packs/mine.bin cargo host-test the_pack_file_parses -- --nocapture
API_TOKEN='…' scripts/clips.sh <ip> [pack]
espflash write-bin 0x2e0000 packs/mine.bin && espflash reset
scripts/speaker-clips.sh
```

- `the_pack_file_parses` puts a pack through the parser the clock
  reads it with, as the pack maker does itself.
- `scripts/clips.sh` posts the pack to `/clips` (the default one, made
  of `clips/`, if none is named) and the clock restarts; with no
  arguments it just makes `packs/clips-default.bin`.
- `espflash write-bin` is the pack over USB.
- `scripts/speaker-clips.sh` makes the default pack for a clock with a
  speaker in the buzzer's place (`speaker-mod/`),
  `packs/clips-default-speaker.bin`, under the default pack's names.

## Build variables

```
ALARM_CLIP=clips/trantor.bpr,wav/tune.pcm cargo build --release
CLIP_MODULATOR=sd CLIP_KBPS=256 cargo build --release
CLIP_MODULATOR=pwm cargo build --release
CLIP_PDM_BITS=128 cargo build --release
```

- `ALARM_CLIP` is the older way, clips built into the firmware, for a
  clock whose partition table has no `clips` partition; a pack takes
  their place where there is one. Any kind (PCM, ADPCM, beeper,
  tracker module, SID tune or PT3 module), as the last melodies, named
  after their files; `ALARM_CLIP_LABEL='A,B'` for other labels. A
  ringing alarm loops a beeper clip, a module, a SID tune or a PT3
  module with no gap and repeats PCM after a pause, unless the file is
  given as `file:once` or `file:loop`.
- `CLIP_MODULATOR=sd` sends a PCM clip or a module through the
  software sigma-delta and not the chip's PDM converter (the default,
  `pdm`, 256 bits a sample), for a buzzer circuit that the converter's
  rate is too fast for. `CLIP_KBPS` is 128, 256 or 512 (the default);
  a 16 kHz clip runs at twice the figure.
- `CLIP_MODULATOR=pwm` is the other software modulator: a pulse a
  carrier period (`CLIP_CARRIER_KHZ`, default 16).
- `CLIP_PDM_BITS=128` is the PDM converter at 128 bits a sample, 2.048
  Mbit/s for a 16 kHz clip, as the clocks' buzzers have heard it. 256
  (4.096 Mbit/s) is the default and 384 the third. More bits are less
  of the converter's own noise and shorter pulses for the transistor.

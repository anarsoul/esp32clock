# Clip tooling reference

The converters that make clips, the pack tools and the build-time
variables of the sound path, with the settings the default pack's
clips were made with and who chose them by ear. The players are in
`agents/modules.md`; the buzzer stage is in `agents/hardware.md`.

## PCM and ADPCM

```
scripts/wav2pcm.sh in.wav wav/tune.pcm [seconds] [skip] [rate]
scripts/wav2pcm.sh in.wav wav/tune.adp [seconds] [skip] [rate]
scripts/adpcm.py wav/tune.pcm wav/tune.adp [--search 4]
CLIP_HIGHPASS=100 CLIP_DRIVE=10 scripts/wav2pcm.sh in.wav wav/tune.pcm
```

- `.pcm`: a sound file as the clip's raw PCM (sox): mono, 8 bits
  unsigned, 16 kHz behind a `PCM1` header, or bare at 8 kHz for half
  the flash, or 31250 for twice it.
- `.adp`: the same clip as IMA ADPCM (`ADP1`), four bits a sample:
  half the flash, 8 KB a second at 16 kHz, encoded from sox's 16-bit
  samples by `scripts/adpcm.py`. The user's ear on white-clock,
  2026-11-16, of a round trip stored as PCM: "not bad", a difference
  heard, "way better than PCM 8k", which is the same flash; and of the
  clock's own decoder on a speaker: "audible, but acceptable".
- `scripts/adpcm.py` makes ADPCM of a PCM clip there is already;
  `--decode` gives the samples back as raw 16-bit. The body after the
  8-byte header is sox's raw `ima`.
- `CLIP_HIGHPASS=100 CLIP_DRIVE=10` is the same for a speaker: the
  high-pass at that corner once and not at 900 Hz twice, 10 dB into
  the limiter and not 20.

## Beeper

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

## Tracker modules (XM)

```
scripts/xm2clip.py tune.xm mods/tune.xmc [--highpass 900 --passes 2] [--raise 1:2] [--drive 160 --release 5] [--knee 100] [--rate 16000] [--bits 8]
```

A FastTracker 2 module for the clock's own player, of up to twenty
channels. It lists what the module uses that the player lacks.

`scripts/s3m2xm.py tune.s3m tune.xm ['Title']` makes an XM of a
ScreamTracker 3 module first: samples, notes, volumes and the effects
A, B, C, D, O and T, naming any other it leaves out; no panning.

- The bass cut, an instrument's octave and the limiter are the clip's.
- `--knee` is the level the limiter starts at, in 1024s of the mix
  where one voice is 32. The default is 20 dB of drive, for the
  buzzer; a knee above the tune's peak is the limiter off, for a
  speaker.
- The rate: the clip is at `clip::FAST_HZ`, 31 250 samples a second,
  here and in the two converters below, which the chip's converter at
  256 bits a sample sends at 8 Mbit/s exactly. It is the default since
  2026-11-13, heard on the breadboard stage and on white-clock. 32 kHz
  is not among the converter's whole dividers, and at
  `CLIP_PDM_BITS=384` such a clip is refused. `--rate 16000` is the
  rate before it, half a module's share of the processor, and 8000 a
  module's third.

## SID tunes

```
scripts/sid2clip.py tune.sid sid/tune.sidc [--song 2] [--seconds 90] [--mute 2] [--model 8580] [--system ntsc] [--steps 4] [--rate 16000] [--highpass 300 --passes 1]
```

A Commodore 64 tune (PSID) for the clock's own SID. It runs the tune's
6502 code on the host and keeps each frame's register writes, to the
point where the tune loops, and says what the tune does that the clock
will not play.

- `--mute` keeps the voices' gates closed for the first frames, for a
  routine that clicks as it starts.
- `--system` plays it as the other machine would, whatever the file
  says: a PAL tune on an NTSC C64 is 59.83 calls a second for 50.12,
  the pitch 3.8 % up.
- `--flat`, for a speaker: no limiter, `--drive` then a plain gain
  with the three voices at their top as full scale (flags bit 1; a
  firmware before 2026-11-15 plays such a clip limited).
- The record is written packed (`SID2`: writes and waits the tune has
  had before are references to them, a fifth of the bytes for a long
  tune); `--plain` for `SID1`. A `.sidc` in the `.sid`'s place is
  packed again, or with `--plain` unpacked.

## PT3 modules

```
scripts/pt32clip.py tune.pt3 pt3/tune.pt3c [--machine pentagon|spectrum] [--clock 1773400] [--chip ym] [--restart] [--highpass 100 --passes 1] [--drive 180] [--rate 16000] [--limiter --highpass 300 --noise 11] [--knee 37] [--noise-lowpass 3000]
```

A ZX Spectrum tune (a Pro Tracker 3 or Vortex Tracker II module) for
the clock's own player and AY-3-8910. The clip is the module as it is
behind a header with the machine's clock and frame rate (by the
module's tone table unless told), the length and where it goes round.

- `--restart` is for a module whose loop is only its ending.
- By default the clip is the chip as it is but for a 100 Hz high-pass,
  at a plain gain 3 dB over three channels at full scale: the user's
  choice of 2026-11-13, on headphones and on white-clock, over the
  limiter.
- `--limiter` brings the limiter back, as the first PT3 clips had it.
- `--knee` is the level the limiter starts at, as `xm2clip.py`'s, kept
  in the flags' top four bits as one of fifteen steps 2 dB apart. A
  firmware before 2026-11-16 limits hard whatever it says.
- The default pack's Synchronization is `--limiter --noise 16
  --highpass 100 --passes 1 --release 10` since 2026-11-16 (the user,
  after hearing the two on a speaker: "beeper should not be flat for
  sure"). Not heard on a buzzer yet.

## Packs

```
scripts/mkpack.py packs/mine.bin clips/trantor.bpr wav/tune.pcm:loop=My\ Tune
CLIP_PACK_FILE=packs/mine.bin cargo host-test the_pack_file_parses -- --nocapture
API_TOKEN='…' scripts/clips.sh <ip> [pack]
espflash write-bin 0x2e0000 packs/mine.bin && espflash reset
scripts/speaker-clips.sh
```

- `scripts/mkpack.py` makes a clip pack for the `clips` partition: up
  to 16 clips, named and looped as `ALARM_CLIP`'s are, `=Label` for
  another label.
- `the_pack_file_parses` puts the pack through the parser the clock
  reads it with.
- `scripts/clips.sh` posts the pack to `/clips` (the default one, made
  of `clips/`, if none is named) and the clock restarts; with no
  arguments it just makes `packs/clips-default.bin`.
- `espflash write-bin` is the pack over USB.
- `scripts/speaker-clips.sh` makes the default pack for a clock with a
  speaker in the buzzer's place (`speaker-mod/`),
  `packs/clips-default-speaker.bin`, under the default pack's names:
  - the two XM modules made again with a 100 Hz high-pass and the
    limiter's release at 128 ms (`--release 10`); heard and kept by
    the user 2026-11-15, after a pack with the limiter off was too
    quiet;
  - Synchronization with `--limiter --noise 16`, the same two and a
    gentler limiter, `--knee 37 --drive 150` (the user's ear,
    2026-11-16, on test-clock's speaker; needs that day's firmware, an
    older one limits hard).

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

#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
# Convert a sound file to the raw PCM the firmware builds in as the
# sampled alarm clip: mono, 16 kHz, 8 bits unsigned. The
# buzzer is a resonator near 2 kHz and all but silent under 1 kHz, so
# everything below 900 Hz is cut (twice, 24 dB an octave), which leaves
# the line's range to what the buzzer can play, and the level is then
# pushed 20 dB into sox's limiter. Thin, but as loud as it gets.
# Needs sox. 16 KB of flash per second; see AGENTS.md.
# Usage: scripts/wav2pcm.sh <in.wav> <out.pcm> [seconds] [skip] [rate]
# keeps `seconds` of sound (all of it for "") from `skip` seconds in,
# the last second and a half faded out when it is cut short. `rate` is
# 16000, the default, 8000, or 31250 (8 Mbit/s through the chip's
# converter, 31 KB of flash a second). At 16000 the clip carries what lies
# between 4 and 8 kHz, which the user's ear found "way better" on the
# buzzer (2026-11-12), and gets a header saying so (PCM1, then the
# rate) for the firmware to read, as it does at 31250; 8000 is half
# the flash, bare samples.
# For a speaker in the buzzer's place, which plays the bass and on
# which 20 dB of drive is heard as hiss and overdrive, two variables:
# CLIP_HIGHPASS, the corner in Hz, then applied once (12 dB an octave),
# and CLIP_DRIVE, the dB pushed into the limiter (0 for none):
#   CLIP_HIGHPASS=100 CLIP_DRIVE=10 scripts/wav2pcm.sh in.wav out.pcm
# An output named `.adp` is written as IMA ADPCM, four bits a sample
# and half the flash of the same clip as PCM (scripts/adpcm.py, which
# encodes sox's samples at 16 bits; python3): heard as close to the PCM
# and far better than PCM at half the rate (2026-11-16).
#   scripts/wav2pcm.sh in.wav wav/tune.adp
# Then:  ALARM_CLIP=<out.pcm> ALARM_CLIP_LABEL='…' cargo build --release
set -eu

usage='usage: scripts/wav2pcm.sh <in.wav> <out.pcm> [seconds] [skip] [rate]'
in=${1:?$usage}
out=${2:?$usage}
seconds=${3:-}
skip=${4:-0}
rate=${5:-16000}
if [ -n "${CLIP_HIGHPASS:-}" ]; then cut="highpass $CLIP_HIGHPASS"; else cut='highpass 900 highpass 900'; fi
drive=${CLIP_DRIVE:-20}
case $rate in 8000 | 16000 | 31250) ;; *) echo "rate must be 8000, 16000 or 31250" >&2; exit 1 ;; esac

case $out in *.adp) format='-e signed-integer -b 16 -L' ;; *) format='-e unsigned-integer -b 8' ;; esac
sox "$in" -t raw $format "$out.raw" \
    remix - trim "$skip" ${seconds:+"$seconds"} $cut rate -h "$rate" \
    norm -0.5 gain -l "$drive" norm -0.5 ${seconds:+fade t 0 "$seconds" 1.5}
if [ "$format" != '-e unsigned-integer -b 8' ]; then
    # It says the clip's size and length itself.
    "$(dirname "$0")/adpcm.py" --s16 --rate "$rate" "$out.raw" "$out"
    rm "$out.raw"
    exit 0
elif [ "$rate" != 8000 ]; then
    # clip::PCM_MAGIC and the rate, little-endian.
    case $rate in 16000) header='PCM1\200\076\000\000' ;; *) header='PCM1\022\172\000\000' ;; esac
    printf "$header" | cat - "$out.raw" > "$out"
    rm "$out.raw"
else
    mv "$out.raw" "$out"
fi
echo "$out: $(stat -c %s "$out") bytes, $(($(stat -c %s "$out") / rate)) s at $rate Hz"

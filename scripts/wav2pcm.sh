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
# 16000, the default, or 8000. At 16000 the clip carries what lies
# between 4 and 8 kHz, which the user's ear found "way better" on the
# buzzer (2026-11-12), and gets a header saying so (PCM1, then the
# rate) for the firmware to read; 8000 is half the flash, bare samples.
# Then:  ALARM_CLIP=<out.pcm> ALARM_CLIP_LABEL='…' cargo build --release
set -eu

usage='usage: scripts/wav2pcm.sh <in.wav> <out.pcm> [seconds] [skip] [rate]'
in=${1:?$usage}
out=${2:?$usage}
seconds=${3:-}
skip=${4:-0}
rate=${5:-16000}
case $rate in 8000 | 16000) ;; *) echo "rate must be 8000 or 16000" >&2; exit 1 ;; esac

sox "$in" -t raw -e unsigned-integer -b 8 "$out.raw" \
    remix - trim "$skip" ${seconds:+"$seconds"} highpass 900 highpass 900 rate -h "$rate" \
    norm -0.5 gain -l 20 norm -0.5 ${seconds:+fade t 0 "$seconds" 1.5}
if [ "$rate" = 16000 ]; then
    # clip::PCM_MAGIC and the rate, little-endian.
    printf 'PCM1\200\076\000\000' | cat - "$out.raw" > "$out"
    rm "$out.raw"
else
    mv "$out.raw" "$out"
fi
echo "$out: $(stat -c %s "$out") bytes, $(($(stat -c %s "$out") / rate)) s at $rate Hz"

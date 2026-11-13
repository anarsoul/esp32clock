#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
# Convert a sound file to the raw PCM the firmware builds in as the
# sampled alarm clip: mono, 8 kHz, 8 bits unsigned, no header. The
# buzzer is a resonator near 2 kHz and all but silent under 1 kHz, so
# everything below 900 Hz is cut (twice, 24 dB an octave), which leaves
# the line's range to what the buzzer can play, and the level is then
# pushed 20 dB into sox's limiter. Thin, but as loud as it gets.
# Needs sox. 8 KB of flash per second; see AGENTS.md.
# Usage: scripts/wav2pcm.sh <in.wav> <out.pcm> [seconds] [skip]
# keeps `seconds` of sound from `skip` seconds in, the last second and a
# half faded out when it is cut short.
# Then:  ALARM_CLIP=<out.pcm> ALARM_CLIP_LABEL='…' cargo build --release
set -eu

usage='usage: scripts/wav2pcm.sh <in.wav> <out.pcm> [seconds] [skip]'
in=${1:?$usage}
out=${2:?$usage}
seconds=${3:-}
skip=${4:-0}

sox "$in" -t raw -e unsigned-integer -b 8 "$out" \
    remix - trim "$skip" ${seconds:+"$seconds"} highpass 900 highpass 900 rate -h 8000 \
    norm -0.5 gain -l 20 norm -0.5 ${seconds:+fade t 0 "$seconds" 1.5}
echo "$out: $(stat -c %s "$out") bytes, $(($(stat -c %s "$out") / 8000)) s"

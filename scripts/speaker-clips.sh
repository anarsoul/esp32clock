#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
# Make the default clip pack for a clock with a speaker in the buzzer's
# place (speaker-mod/README.md): the three clips of clips/, the two
# modules converted again from mods/ with the bass left in (a high-pass
# at 100 Hz, once) and the limiter letting go over 128 ms and not 16:
# as hard a drive as the buzzer's clips have, but a gain that follows
# the tune's level and not its waveform, which on a speaker is heard
# as overdrive. Some 2 dB under the buzzer's clips; with no limiter at
# all (--knee) they were 6 dB under and too quiet (2026-11-15).
# Trantor is the Spectrum's speaker bit and is the same for either.
# The clips go to packs/speaker/ and the pack to
# packs/clips-default-speaker.bin, under the default pack's names, so
# a clock's melody setting means the same tune after it.
# Usage: scripts/speaker-clips.sh
#        API_TOKEN=… scripts/clips.sh <ip> packs/clips-default-speaker.bin
set -eu

cd "$(dirname "$0")/.."
mkdir -p packs/speaker
scripts/xm2clip.py "mods/Orion - Nero Burning ROM v5.5.7.8kg.xm" packs/speaker/halftime.xmc \
    --highpass 100 --passes 1 --release 10
scripts/xm2clip.py mods/Orion-Nero6.3.0.2.xm packs/speaker/keygen.xmc \
    --highpass 100 --passes 1 --release 10
pack=packs/clips-default-speaker.bin
scripts/mkpack.py "$pack" clips/trantor.bpr packs/speaker/halftime.xmc packs/speaker/keygen.xmc
printf '%s (%s bytes)\n' "$pack" "$(stat -c %s "$pack")"

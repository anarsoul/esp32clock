#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
# Make the default clip pack for a clock with a speaker in the buzzer's
# place (speaker-mod/README.md): the eight clips of clips/, the
# XM modules converted again from mods/ with the bass left in (a high-pass
# at 100 Hz, once) and the limiter letting go over 128 ms and not 16:
# as hard a drive as the buzzer's clips have, but a gain that follows
# the tune's level and not its waveform, which on a speaker is heard
# as overdrive. Some 2 dB under the buzzer's clips; with no limiter at
# all (--knee) they were 6 dB under and too quiet (2026-11-15).
# Trantor is the Spectrum's speaker bit and is the same for either.
# Synchronization has the noise at full level, the high-pass at
# 100 Hz, the release at 128 ms, and a gentler limiter than the
# modules': its knee at 37 (22 dB over the player's own, so the tune's
# quiet parts are at a plain gain) and its peaks aimed 1.4 dB over
# full scale, the user's choice by ear of 2026-11-16 over the hard
# limiter, which the buzzer's pack has. The knee wants a firmware of
# that day or later; an older one limits hard.
# Suspended Seconds is a piano, whose attacks a limiter that follows
# the waveform flattens into clicks: its limiter lets go over a
# second (release=13) from a low knee, so it rides a passage's level
# and not a note's (the user's choice, 2026-11-19; the buzzer's clip
# has release=10).
# The pack maker's `--sound-output speaker` is these settings, so the
# pack is the default one's sources through it: packs/clips-default-speaker.bin,
# under the default pack's names, so a clock's melody setting means the
# same tune after it.
# Usage: scripts/speaker-clips.sh
#        API_TOKEN=… scripts/clips.sh <ip> packs/clips-default-speaker.bin
set -eu

cd "$(dirname "$0")/.."
pack=packs/clips-default-speaker.bin
cargo pack -o "$pack" --sound-output speaker clips/trantor.bpr \
    "mods/Orion - Nero Burning ROM v5.5.7.8kg.xm:name=halftime" mods/Orion-Nero6.3.0.2.xm:name=keygen \
    mods/nq-synchronization.pt3:restart,name=synchronization \
    mods/grieg-morning-bright.xm mods/grieg-morning-chip.xm mods/grieg-mountain-king-bright.xm \
    mods/suspended-seconds.xm:knee=12,release=13

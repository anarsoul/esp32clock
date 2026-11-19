#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
# Post a clip pack to the clock's /clips, which writes it to the `clips`
# partition and restarts. Needs curl, and API_TOKEN in the environment
# (the clock's token).
# Usage: API_TOKEN=… scripts/clips.sh <ip-or-hostname> [pack.bin]
#        scripts/clips.sh            make the default pack only and print
#                                    its path, for the clock's Clips page
#
# Without a pack named, the default one is made of the eight clips in
# clips/ (the pack maker, tools/pack; cargo) as packs/clips-default.bin. The
# clock answers once the pack is written and checked, then reboots; the
# panel is dark meanwhile. A megabyte takes about half a minute.
set -eu

host=${1:--}
pack=${2:-}
cd "$(dirname "$0")/.."

if [ -z "$pack" ]; then
    pack=packs/clips-default.bin
    cargo pack -o "$pack" clips/trantor.bpr clips/halftime.xmc clips/keygen.xmc \
        clips/synchronization.pt3c clips/morning_mood.xmc clips/morning_mood_chip.xmc \
        clips/mountain_king.xmc clips/suspended_seconds.xmc
fi
if [ "$host" = - ]; then
    printf '%s (%s bytes)\n' "$pack" "$(stat -c %s "$pack")"
    exit 0
fi
: "${API_TOKEN:?set API_TOKEN to the token of the clock}"
printf 'posting %s (%s bytes) to http://%s/clips\n' "$pack" "$(stat -c %s "$pack")" "$host"
curl -sS --max-time 300 -H "Authorization: Bearer $API_TOKEN" -H 'Expect:' \
    --data-binary "@$pack" -w 'HTTP %{http_code}, %{time_total}s\n' "http://$host/clips"

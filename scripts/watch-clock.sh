#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
# Host-side view of the clock's reachability: every PERIOD seconds, one
# ping of the largest size the clock answers (498 bytes of payload, the
# firmware's gateway probe at the 540-byte Wi-Fi MTU; a larger ping gets
# no reply, since the clock cannot send one) and one GET /api, each on a
# line with the host time. Run it next to the serial monitor so a
# failure can be read from both sides.
# Usage: watch-clock.sh <ip-or-hostname> [period-seconds] | tee clock-watch.log
set -u

host=${1:?usage: $0 <ip-or-hostname> [period-seconds]}
period=${2:-30}

while :; do
    stamp=$(date '+%F %T')
    if ping -c1 -W2 -s 498 "$host" >/dev/null 2>&1; then ping_result=ok; else ping_result=FAIL; fi
    http_start=$(date +%s%N)
    if curl -sS --max-time 5 -o /dev/null "http://$host/api" 2>/dev/null; then
        http_result="ok $(( ($(date +%s%N) - http_start) / 1000000 )) ms"
    else
        http_result=FAIL
    fi
    printf '%s ping %s http %s\n' "$stamp" "$ping_result" "$http_result"
    sleep "$period"
done

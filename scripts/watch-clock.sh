#!/usr/bin/env bash
# Host-side view of the clock's reachability: every PERIOD seconds, one
# full-size ping (the same 1400-byte payload as the firmware's gateway
# probe, so short frames cannot pass while page-sized ones fail) and one
# GET /api, each on a line with the host time. Run it next to the serial
# monitor so a failure can be read from both sides.
# Usage: watch-clock.sh <ip-or-hostname> [period-seconds] | tee clock-watch.log
set -u

host=${1:?usage: $0 <ip-or-hostname> [period-seconds]}
period=${2:-30}

while :; do
    stamp=$(date '+%F %T')
    if ping -c1 -W2 -s 1400 "$host" >/dev/null 2>&1; then ping_result=ok; else ping_result=FAIL; fi
    http_start=$(date +%s%N)
    if curl -sS --max-time 5 -o /dev/null "http://$host/api" 2>/dev/null; then
        http_result="ok $(( ($(date +%s%N) - http_start) / 1000000 )) ms"
    else
        http_result=FAIL
    fi
    printf '%s ping %s http %s\n' "$stamp" "$ping_result" "$http_result"
    sleep "$period"
done

#!/usr/bin/env bash
# Configure the clock over the LAN: orange at 10 %, the "beeps" melody,
# alarm 07:40 armed on weekdays, no skip, 24-hour display. Timezone and
# hostname are not sent, so they stay as they are. Needs curl.
# Usage: scripts/configure-clock.sh <ip-or-hostname>
#
# One POST to /api applies everything or nothing: the clock parses the
# body on a copy and answers 400 with the reason if any pair is bad. The
# API does not clear booleans that are left out, so skip and twelve_hour
# are sent explicitly as 0. The reply is the settings as the clock now
# holds them, printed for checking.
set -eu

host=${1:?usage: $0 <ip-or-hostname>}

body='color=orange'
body+='&brightness=10'
body+='&melody=beeps'
body+='&alarm=07:40'
body+='&armed=1'
body+='&days=weekdays'
body+='&skip=0'
body+='&twelve_hour=0'

printf 'posting settings to http://%s/api\n' "$host"
response=$(curl -sS --fail-with-body --max-time 15 \
    -H 'Content-Type: application/x-www-form-urlencoded' \
    --data "$body" "http://$host/api") || {
    printf 'clock refused the settings: %s\n' "$response" >&2
    exit 1
}
printf '%s\n' "$response"

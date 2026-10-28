#!/usr/bin/env bash
# Reset the clock the deliberate way and wait for it to come back.
# POST /reload blanks the panel, carries the time and the alarm state
# across the reset and does not count as a self-heal; see AGENTS.md.
# Needs curl and OTA_TOKEN (the value the firmware was built with).
# Usage: OTA_TOKEN=… scripts/reload.sh <ip-or-hostname>
set -eu

host=${1:?usage: OTA_TOKEN=… scripts/reload.sh <ip-or-hostname>}
: "${OTA_TOKEN:?set OTA_TOKEN to the token the firmware was built with}"

printf 'before: %s\n' "$(curl -sS --max-time 5 "http://$host/api")"
curl -sS --max-time 10 -X POST -H "Authorization: Bearer $OTA_TOKEN" \
    -w 'HTTP %{http_code}\n' "http://$host/reload"
# The link, the lease and the listener take about six seconds.
for i in $(seq 1 30); do
    sleep 1
    if after=$(curl -s --max-time 2 "http://$host/api"); then
        printf 'back after %s s: %s\n' "$i" "$after"
        exit 0
    fi
done
echo "the clock has not come back within 30 s" >&2
exit 1

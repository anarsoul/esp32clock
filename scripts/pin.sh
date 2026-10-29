#!/usr/bin/env bash
# Ask the clock for a PIN and read it back from the serial log, for a
# bench run without eyes on the panel. POST /pin puts four digits on the
# panel and in the log (never in the response); this prints them, so
#     PIN=$(scripts/pin.sh <ip> <serial-log>)
#     curl -H "Authorization: PIN $PIN" -d token=… http://<ip>/token
# The monitor must already be writing the log, e.g.
#     espflash monitor --port /dev/ttyACM0 --non-interactive > clock-serial.log
# Usage: scripts/pin.sh <ip-or-hostname> <serial-log>
set -eu

host=${1:?usage: scripts/pin.sh <ip-or-hostname> <serial-log>}
log=${2:?usage: scripts/pin.sh <ip-or-hostname> <serial-log>}

before=$(grep -c 'auth: PIN' "$log" || true)
curl -sS --max-time 5 -X POST -o /dev/null -w 'HTTP %{http_code}\n' "http://$host/pin" >&2
for _ in $(seq 1 20); do
    if [ "$(grep -c 'auth: PIN' "$log" || true)" -gt "$before" ]; then
        grep 'auth: PIN' "$log" | tail -1 | sed -E 's/.*auth: PIN ([0-9]{4}).*/\1/'
        exit 0
    fi
    sleep 0.25
done
echo "no PIN line in $log within 5 s; is the monitor running, and was the clock locked out?" >&2
exit 1

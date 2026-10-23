#!/usr/bin/env bash
# Build the firmware, turn it into an application image and post it to
# the clock's /ota. Needs espflash and curl, and OTA_TOKEN in the
# environment (the same value the firmware was built with).
# Usage: OTA_TOKEN=… scripts/ota.sh <ip-or-hostname> [app.bin]
#        scripts/ota.sh              build the image file only and print
#                                    its path, for the clock's /update page
#
# With an image file given, the build is skipped and that file is sent.
# The clock answers once the whole image is written and verified, then
# reboots; the panel is dark meanwhile. Expect the request to take tens
# of seconds.
set -eu

host=${1:--}
image=${2:-}
cd "$(dirname "$0")/.."

if [ -z "$image" ]; then
    image=target/esp32clock-ota.bin
    cargo build --release
    espflash save-image --chip esp32c3 --partition-table partitions.csv \
        target/riscv32imc-unknown-none-elf/release/esp32clock "$image"
fi
if [ "$host" = - ]; then
    printf '%s (%s bytes)\n' "$image" "$(stat -c %s "$image")"
    exit 0
fi
: "${OTA_TOKEN:?set OTA_TOKEN to the token the firmware was built with}"
printf 'posting %s (%s bytes) to http://%s/ota\n' "$image" "$(stat -c %s "$image")" "$host"
curl -sS --max-time 300 -H "Authorization: Bearer $OTA_TOKEN" -H 'Expect:' \
    --data-binary "@$image" -w 'HTTP %{http_code}, %{time_total}s\n' "http://$host/ota"

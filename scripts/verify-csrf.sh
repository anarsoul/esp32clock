#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
# Checks that the clock refuses what another site's page could make a
# browser send: a POST with a foreign Origin, any request under a Host
# that is not the clock's, and that its own pages and a script still
# get through. Needs curl. Usage: verify-csrf.sh <ip>
#
# Give the address, not the name: the redirect is checked against it.
# The colour is stepped once and put back.
set -u

host=${1:?usage: $0 <ip>}
base="http://$host"
fail=0

pass() { printf '  ok    %s\n' "$*"; }
bad()  { printf '  FAIL  %s\n' "$*"; fail=1; }
# code <curl args…>  -> the HTTP status
code() { curl -sS --max-time 5 -o /dev/null -w '%{http_code}' "$@"; }
# expect <status> <what> <curl args…>
expect() {
    local want=$1 what=$2 got
    shift 2
    got=$(code "$@")
    if [ "$got" = "$want" ]; then pass "$what: $got"; else bad "$what: $got, expected $want"; fi
}
color() { curl -sS --max-time 5 "$base/api" | sed -nE 's/.*"color":"([^"]*)".*/\1/p'; }

original=$(color)
[ -n "$original" ] || { echo "cannot read $base/api"; exit 2; }
other=red
[ "$original" = red ] && other=green
trap 'curl -sS --max-time 5 -o /dev/null -d "color=$original" "$base/api"' EXIT

echo "Another site's page"
for origin in http://evil.example https://evil.example null "http://$host.evil.example"; do
    expect 403 "POST /api, Origin $origin" -d "color=$other" -H "Origin: $origin" "$base/api"
    expect 403 "POST /, Origin $origin" -d "color=$other" -H "Origin: $origin" "$base/"
done
expect 403 "POST /play, foreign Origin" -d melody=random -H 'Origin: http://evil.example' "$base/play"
expect 403 "POST /stop, foreign Origin" -X POST -H 'Origin: http://evil.example' "$base/stop"
expect 403 "POST /pin, foreign Origin" -X POST -H 'Origin: http://evil.example' "$base/pin"
[ "$(color)" = "$original" ] && pass "nothing changed" || bad "the colour changed to $(color)"

echo "A name that is not the clock's"
expect 302 "GET /, Host evil.example" -H 'Host: evil.example' "$base/"
expect 302 "GET /api, Host evil.example" -H 'Host: evil.example' "$base/api"
where=$(curl -sS --max-time 5 -o /dev/null -w '%{redirect_url}' -H 'Host: evil.example' "$base/system")
[ "$where" = "$base/" ] && pass "sent to $where" || bad "sent to '$where', expected $base/"
expect 403 "POST /api, Host evil.example" -d "color=$other" -H 'Host: evil.example' "$base/api"
expect 403 "POST /api, Host and Origin evil.example (rebinding)" -d "color=$other" \
    -H 'Host: evil.example' -H 'Origin: http://evil.example' "$base/api"
expect 403 "POST /ota, Host evil.example" --data-binary 'not an image' -H 'Host: evil.example' "$base/ota"
[ "$(color)" = "$original" ] && pass "nothing changed" || bad "the colour changed to $(color)"

echo "The clock's own pages and a script"
expect 200 "GET /api" "$base/api"
expect 200 "GET /system" "$base/system"
expect 200 "POST /api, Origin $base" -d "color=$other" -H "Origin: $base" "$base/api"
[ "$(color)" = "$other" ] && pass "the colour is $other" || bad "the colour is $(color), expected $other"
expect 200 "POST /api, no Origin" -d "color=$original" "$base/api"
expect 303 "POST /, Origin $base" -d "color=$original" -H "Origin: $base" "$base/"
frame=$(curl -sS --max-time 5 -D - -o /dev/null "$base/" | tr -d '\r' | grep -i '^x-frame-options:')
[ "$frame" = "X-Frame-Options: DENY" ] && pass "$frame" || bad "no X-Frame-Options on the page: '$frame'"

[ "$fail" = 0 ] && echo "all checks passed" || echo "FAILED"
exit "$fail"

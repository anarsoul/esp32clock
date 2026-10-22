#!/usr/bin/env bash
# Checks that a refused form post on `/` saves nothing and reports why,
# and that a good one sticks. Needs curl. Usage: verify-form-post.sh <ip>
#
# The clock's settings are read first and put back at the end, whatever
# happens in between. The alarm is armed for the test, since the bug this
# guards against disarmed it.
set -u

host=${1:?usage: $0 <ip-or-hostname>}
base="http://$host"
fail=0

say()  { printf '%s\n' "$*"; }
pass() { say "  ok    $*"; }
bad()  { say "  FAIL  $*"; fail=1; }

api()  { curl -sS --max-time 5 "$base/api"; }
# Form post to /, returns the Location header (empty if none).
post_page() {
    curl -sS --max-time 5 -i -d "$1" "$base/" | tr -d '\r' | awk 'tolower($1)=="location:" {print $2}'
}
field() { # field <json> <key>  -> string or bare value
    printf '%s' "$1" | sed -nE "s/.*\"$2\":\"?([^\",}]*)\"?.*/\\1/p"
}
# Build a full form body from a JSON record so the page's checkbox rule
# (absent = off) leaves every field as the record says.
form_from() {
    local j=$1 body
    body="color=$(field "$j" color)&brightness=$(field "$j" brightness)"
    body="$body&alarm=$(field "$j" alarm)&days=$(field "$j" days)"
    body="$body&melody=$(field "$j" melody)&tz=$(field "$j" tz)"
    body="$body&hostname=$(field "$j" hostname)"
    [ "$(field "$j" armed)" = true ] && body="$body&armed=on"
    [ "$(field "$j" twelve_hour)" = true ] && body="$body&twelve_hour=on"
    [ "$(field "$j" skip)" = true ] && body="$body&skip=on"
    printf '%s' "$body"
}
# Pick a value from a list that differs from the current one.
other() { local cur=$1; shift; for v in "$@"; do [ "$v" != "$cur" ] && { printf '%s' "$v"; return; }; done; }

say "Reading settings from $base"
original=$(api) || { say "cannot reach $base/api"; exit 2; }
say "  $original"
restore() {
    say "Restoring original settings"
    loc=$(post_page "$(form_from "$original")")
    now=$(api)
    if [ "$now" = "$original" ]; then pass "settings restored"; else bad "restore left: $now"; fi
}
trap restore EXIT

# Arm the alarm for the test so a half-applied post would show as disarmed.
if [ "$(field "$original" armed)" != true ]; then
    curl -sS --max-time 5 -d armed=on "$base/api" >/dev/null
fi
baseline=$(api)
[ "$(field "$baseline" armed)" = true ] || { bad "could not arm the alarm: $baseline"; exit 1; }

color=$(other "$(field "$baseline" color)" red orange yellow green cyan blue magenta white)
bright=$(other "$(field "$baseline" brightness)" 10 25 50 75 100)
melody=$(other "$(field "$baseline" melody)" beeps twinkle ode frere)
say "Baseline (armed): $baseline"
say "Test values: color=$color brightness=$bright melody=$melody"

say "1. Form with the alarm time cleared"
loc=$(post_page "color=$color&brightness=$bright&alarm=&melody=$melody&armed=on")
[ "$loc" = "/?error=value" ] && pass "redirected to $loc" || bad "Location was '$loc', wanted /?error=value"
now=$(api)
[ "$now" = "$baseline" ] && pass "settings unchanged" || bad "settings changed to: $now"
notice=$(curl -sS --max-time 5 "$base/?error=value" | grep -o '<p class=err>[^<]*</p>')
case $notice in *"bad value"*) pass "page shows: $notice";; *) bad "page notice missing or wrong: '$notice'";; esac

say "2. Form with an unknown key"
loc=$(post_page "color=$color&bogus=1")
[ "$loc" = "/?error=key" ] && pass "redirected to $loc" || bad "Location was '$loc', wanted /?error=key"
now=$(api)
[ "$now" = "$baseline" ] && pass "settings unchanged" || bad "settings changed to: $now"
notice=$(curl -sS --max-time 5 "$base/?error=key" | grep -o '<p class=err>[^<]*</p>')
case $notice in *"unknown field"*) pass "page shows: $notice";; *) bad "page notice missing or wrong: '$notice'";; esac

say "3. Good form"
loc=$(post_page "color=$color&brightness=$bright&alarm=07:30&days=all&melody=$melody&armed=on")
[ "$loc" = "/" ] && pass "redirected to / with no error" || bad "Location was '$loc', wanted /"
now=$(api)
want_color=$(field "$now" color); want_b=$(field "$now" brightness); want_m=$(field "$now" melody)
if [ "$want_color" = "$color" ] && [ "$want_b" = "$bright" ] && [ "$want_m" = "$melody" ] \
   && [ "$(field "$now" alarm)" = "07:30" ] && [ "$(field "$now" armed)" = true ]; then
    pass "new values stuck: $now"
else
    bad "values did not stick: $now"
fi
notice=$(curl -sS --max-time 5 "$base/" | grep -o '<p class=err>[^<]*</p>')
[ -z "$notice" ] && pass "page shows no notice" || bad "page still shows: $notice"

trap - EXIT
restore
say
if [ "$fail" = 0 ]; then say "ALL PASSED"; else say "SOME CHECKS FAILED"; fi
exit $fail

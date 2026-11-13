# esp32clock — guide for agents

Firmware for the HU-058 / HU-058D LED clock panel driven by an ESP32-C3,
written in Rust with esp-hal and Embassy on esp-rtos. The display's two
AiP33628 drivers are fed by SPI2 in quad mode used as a four-wire pattern
generator, on one DMA segmented transfer the hardware loops for ever;
see `PLAN.md` for the full design and its history.
The repo is a two-member workspace: the firmware is the root package,
and `core/` (`esp32clock-core`) is the hardware-independent half, a
`no_std` library the firmware re-exports and the host runs the tests
of. `CLAUDE.md` is a symlink to this file; edit this one.

## Workflow

- Plan before implementing. `PLAN.md` is the design record: each milestone
  has its design, what was measured, and what was learned. Add to it before
  starting work and mark milestones verified when the user confirms on the
  panel. It ends with a review backlog of known, undone findings; read it
  before proposing work and strike items you finish.
- Nothing here can be verified without the hardware. Build, run clippy
  and `cargo host-test`, and put new pure logic under test in `core/`;
  then hand the user a flash command and say exactly what to look for.
  Do not claim hardware results.
- Commit only when asked. Commit messages end with
  `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>` (the user asked
  for this trailer in this project).
- `3rdparty/` is the reverse-engineering reference (public domain), ignored
  by git. Read it; do not modify it.
- `clips/` holds clips that are in the repository; they are other
  people's music, not under the project's licence, and
  `clips/README.md` says whose each is. Nothing builds them in unless
  `ALARM_CLIP` names them. A clip nobody may redistribute stays in the
  ignored `wav/` and `tap/`.
- The project is licensed MIT OR Apache-2.0 (`LICENSE-MIT`,
  `LICENSE-APACHE`, the `license` field of both manifests). Every `.rs`
  and `.sh` file starts with an `SPDX-License-Identifier` line and the
  copyright line; give a new file both. The reference in `3rdparty/` is CC0 and needs nothing.

## Build and flash

```
cargo build --release
cargo clippy --release
cargo host-test                          # the core crate's tests on the host (alias in .cargo/config.toml)
cargo clippy -p esp32clock-core --target host-tuple --tests
WIFI_SSID='…' WIFI_PASSWORD='…' TZ_RULE='<-07>7' API_TOKEN='…' cargo run --release
TZ_OFFSET_MINUTES=-420 cargo build --release   # the older variable: minutes east of UTC, stored as the fixed rule <-07>7
node scripts/tz-check.mjs                 # the page's browser-zone script (src/tz.js) against zones with known rules
MAX_CURRENT_STEP=8 cargo build --release   # optional: raise the drive-current ceiling (default 5)
MQTT_HOST=10.1.1.5 MQTT_USER=clock MQTT_PASSWORD='…' cargo build --release   # optional: the Home Assistant broker (MQTT_PORT defaults to 1883)
API_TOKEN='…' scripts/ota.sh <ip>      # later updates, over the LAN
scripts/ota.sh                          # just the image file, for http://<ip>/update
API_TOKEN='…' scripts/reload.sh <ip>   # software reset with the carry record, waits for the clock to come back
scripts/pin.sh <ip> <serial-log>        # POST /pin, then the four digits read back from the monitor's log
AP_PASSWORD='…' cargo build --release   # optional: the clock's own access point's password (default esp32clock)
scripts/wav2pcm.sh in.wav wav/tune.pcm [seconds]   # a sound file as the clip's raw PCM (sox): mono, 8 kHz, 8 bits unsigned
~/.local/pipx/venvs/skoolkit/bin/python scripts/zxbeeper.py game.tap tap/tune.bpr --seconds 60 [--stretch 4]   # what a ZX Spectrum program plays on its beeper, as a clip (needs SkoolKit)
ALARM_CLIP=clips/trantor.bpr,wav/tune.pcm cargo build --release   # optional: clips (PCM or beeper) as the last melodies, named after their files; ALARM_CLIP_LABEL='A,B' for other labels
CLIP_MODULATOR=sd CLIP_KBPS=256 ALARM_CLIP=… cargo build --release   # a PCM clip as a sigma-delta stream, a decision a bit, instead of the default pulse a period (pwm) (CLIP_KBPS=128|256|512, default 512; CLIP_CARRIER_KHZ=8|16|32, default 16)
```

Releases: `CHANGELOG.md` gets the version's section first (the release body points at it), then an annotated tag `v*` (`git tag -a`; `build.rs` runs `git
describe` without `--tags`, so a lightweight tag would not reach the
version) runs `.github/workflows/release.yml`, which builds with the
CI's prebuilt settings, tests, and attaches `esp32clock-ota-<tag>.bin`
(the `/update` image) and `esp32clock-<tag>.bin` (`save-image
--merge`: the whole flash from 0, contiguous, so it covers `nvs` with
erased bytes and clears the settings; `write-bin` then needs an
`espflash reset`) to the GitHub release, then deploys `docs/` to the
`gh-pages` branch with the release under `<tag>/` (a manifest for ESP
Web Tools with four parts, the bootloader and partition table cut
from the merged image, an erased `otadata` and the OTA image at
0x20000, so `nvs` is left alone and the settings survive unless the
erase is chosen) and `latest/`, keeping earlier tags. `docs/index.html` is the
web installer at `https://anarsoul.github.io/esp32clock/`: it lists
the releases from the GitHub API, newest first, and points the install
button at the chosen tag's manifest; without the API it uses `latest/`.
Nothing in `docs/` but the page is in the repository.

`cargo run` flashes with espflash and opens the monitor; `.cargo/config.toml`
sets the runner and `ESP_LOG=info`. The runner passes `partitions.csv`
(two app slots, `ota_0` and `ota_1`, plus `otadata`) and erases
`otadata` so a USB flash always boots `ota_0`, where espflash put the
image. `scripts/ota.sh` builds, makes the application image with
`espflash save-image` and posts it to `/ota` with the token; the clock
writes the other slot, selects it and reboots. The token is
`API_TOKEN` at build time and a setting after that (`src/auth.rs`): a
build without one has no script path (a `Bearer` request is `403`)
until one is set from `/token` with the PIN or by `POST /token`, and
the boot line says `api token: stored`, `built in`, `cleared` or
`none`. The Wi-Fi credentials are seeds the same way, and `WIFI_SSID`
may be empty: whatever the record lacks is filled from the build at
boot, the next save carries it, and from then on the record wins over
a later build's constants (`settings::Settings::seed`). A token
cleared with `POST /token` stays cleared: the record's flags byte
(`token_cleared`, version 7) tells a cleared token from one never set,
and only the latter is filled from the build. The MQTT broker
(`MQTT_HOST`, `MQTT_PORT`, `MQTT_USER`, `MQTT_PASSWORD`) is a seed the
same way, with bit 1 of the flags (`mqtt_cleared`) for a host cleared
on the page. For a network debugging run set
`ESP_LOG='info,embassy_net=debug,esp_radio=debug'` on the command line
(it overrides the config, and esp-println compiles the filter in); the
extra lines are DHCP state and driver events, not packets. Every log
line carries the uptime in ms. `scripts/watch-clock.sh <ip>` sends a
498-byte ping (the largest the clock answers, see the MTU below) and
fetches `/api` from the host every 30 s for the other side of the story.

Credentials, the default timezone and the API token are read at build
time by `core/src/config.rs`; `core/build.rs` reruns that crate when
they change, and the root `build.rs` exports `git describe --always
--dirty` as `FIRMWARE_VERSION`, which
goes into the app descriptor, the boot line, the page and `/api`. The timezone (`tz`: a POSIX TZ rule such as `PST8PDT,M3.2.0,M11.1.0`, parsed by `core/src/tz.rs` and evaluated on the chip from the NTP time, so daylight saving needs nothing from the network; the key also takes minutes east of UTC, stored as the fixed rule `<-07>7`; the default is `TZ_RULE` from the build, checked by a `const` assert so a bad rule fails the build, else `TZ_OFFSET_MINUTES` as a fixed rule, the CI's `<-07>7` being Vancouver without daylight saving; the page's `Use this browser's zone` button fills it in through `/tz.js`, see `src/tz.js`), 12/24 hour, colour, brightness, alarm, alarm days,
alarm melody, thermometer offset (`temp_offset`, tenths of a degree
Celsius added to the NTC's reading, ±10.0, for the warm air at the
sensor's spot in the case; the default is −2.5, `config::DEFAULT_TEMP_OFFSET_DC`,
what the user's clocks typically need, for a clock with no record or one older than
version 8, while a stored value, 0 included, stays), the
temperature on the panel (`temp_show`: `off`, `c` or `f`, default
`c`; with a reading in hand the UI shows ` 23C` with the degree mark
for seconds 30 to 34 of every minute in clock mode, `ui::TEMP_SHOW_AT`
and `TEMP_SHOW_SECS`), DHCP
hostname, Wi-Fi network, API token and MQTT broker (`mqtt_host`,
`mqtt_port`, `mqtt_user`, `mqtt_password`; see "Home Assistant" below)
are settings:
saved in the `nvs` partition, editable from the buttons and from
`http://<ip>/` (form) or `/api` (form-encoded keys, JSON back). The
network keys `ssid` and `password` are taken on `/api` only with the
`Bearer` token, refused as `403` on the key alone (never on whether the
value would change, which would make a password oracle), and never on
the settings form; the broker keys `mqtt_*` need the token on `/api`
the same way (a broker of the attacker's choosing would be handed the
stored password) but the settings form takes them (`form::Allow`:
`PAGE`, `ALL`, `NONE`) under one rule, checked in `form::apply` after
the pairs: a form that moves the broker to another host without typing
the password again is refused (`FormError::BrokerPassword`, with its
own notice), so the stored password goes only to the host it was typed
for; the same host keeps it on an empty field, an empty host clears it,
the token is not held to the rule, and `POST /play` applies the form
with `ALL` since nothing is saved; a saved change resets the clock into the new network
(`heal::switch_mode`, with the carry record started over), so a wrong
one costs the LAN until the password is right again. On the access
point every save resets into the station once a network is stored,
whatever changed, so the timezone or hostname posted with the setup
form reaches flash before the clock leaves. The password and
the token are never read back: the JSON has `ssid`, `password_set` and
`token_set`. The page ends with a read-only status
line, and the JSON with the read-only keys `version`, `slot` (the app
slot the firmware runs from), `mode` (`sta` or `ap`), `uptime` (seconds), `heals` (self-heal
resets since power-up), `temp_c` and `temp_f` (the room in tenths, `null`
with no thermometer fitted or before the first reading), `light` (0 dark
to 100, `null` before the first reading), `rssi` (the station's signal
in dBm from its last beacon, read every 10 s by `net::wifi_task` into
an atomic, `null` while not connected and on the access point),
`utc_offset` and `dst` (the minutes east of UTC and whether daylight time is in force right now under the rule, `null` before the first time sync), `mqtt` (`off`, `connecting` or `connected`), `ringing` (whether the alarm rings right now) and `last_heal` (Unix seconds or null); posting them is refused as an unknown key. The settings keys carry
`mqtt_password_set` instead of the password. The page shows the
network and the signal under the firmware line (`Wi-Fi home, -62
dBm.`, or `not connected`, or `no network stored`), the broker on the
next (`MQTT connected 10.1.1.5:1883.`, `MQTT connecting …, last: not
authorized.` or `MQTT off.`), the room on the line after (`Room 23.4 °C (74.1 °F), light 62 %`), the local time the rule gives on the next (`Local time Sat 15:20 PST, UTC-08:00.`, with `, daylight time` appended when it is in force, for checking a rule), and the minute heartbeat logs the filtered millivolts of
both dividers for calibration. `skip_next` (page checkbox, key `skip`) is
the one setting the record does not store: it skips one alarm and is cleared
by it or by a power cut; a software reset (self-heal, panic) carries it over
in RTC RAM, see `src/carry.rs`. `POST /play` previews the melody chosen in the form
without saving; the page posts it by script so the form is not reloaded. `rust-version` must stay at 1.95 or higher or cargo's
MSRV-aware resolver picks an esp-rom-sys that esp-storage cannot build with.

## Controls

| press | in clock mode |
| --- | --- |
| top short | next colour |
| bottom short | next brightness |
| top long | set alarm hour, then bottom short for minutes, bottom short again to leave |
| bottom long | arm or disarm the alarm (degree mark shows armed) |
| both long | show the IP, one octet at a time, twice |
| both held 10 s | reset into the access point; from the access point, back to the station if a network is stored |

Opening `http://<ip>/update` shows a four-digit PIN for a minute, or
until it is used, right or wrong; so does `http://<ip>/token`, the
API-token page, which no page links to.

Provisioning: a clock with no network in its record (and none built
in), or one whose buttons were held 10 s, boots as an access point
instead of a station for that whole boot, since the driver does not do
both at once. The panel shows `AP`; the network is `clock-xxxx` (the
last two bytes of the MAC), WPA2 with `AP_PASSWORD` from the build
(default `esp32clock`), channel 1, the clock at `http://192.168.4.1/`
with its own DHCP server (`src/dhcpd.rs`, eight leases, replies
broadcast) and no time, so no alarm. `GET /` there is the setup form
(network, password, timezone, hostname); saving answers a page and the
clock resets into the network once the record is written, or at once
if nothing changed. Both switches (`heal::switch_mode`) start the carry
record over, keeping only the OTA boot count: no time, heals or alarm
state crosses a mode change, and a panic in AP mode keeps the flag so
the clock comes back as an access point. A station that cannot join
never turns into an access point by itself: a wrong password is a
blank panel, `wrong password?` in the log every attempt, and the hold.
`/update`, `/ota`, `/api` and `/token` work on the access point too;
`/api` takes the network keys there without the token. A captive DNS
(`src/dnsd.rs`) answers every name with the clock's address and any
path that is not a route is a `302` to `http://192.168.4.1/`, so a
phone's connectivity probe opens the setup form in its sign-in sheet
by itself. `POST /ap` with
the `Bearer` token resets a running clock into the access point, for a
bench without hands on the buttons. The access point runs as AP+STA
with a station half that never connects, because the driver refuses a
scan in AP-only mode: it scans once when it comes up and again on
`POST /scan` (the page's Rescan button, which comes back to `/` with
the list; `ap_task` counts the scans it has done and each waiter polls
the count, so two clicks from two tabs are each answered rather than
one waking the other's `Signal`), and the setup form lists what it saw, strongest first, with
an "Other" entry and a text field for a hidden network. The AP stays
up through a scan. A `POST` of `ssid=` (empty) leaves the network
unchanged, since the form always sends both the list's pick and the
typed name; a non-empty `ssid_pick` wins over `ssid` whatever the
order (`form::apply`), and the page also disables the typed field
while a listed network is picked, since a hidden input is still
posted. The form's password field is write-only and always posted:
on the setup form an empty password with the stored network keeps the
stored password unless the scan lists that network as open, and an
empty password for another network the scan lists as secured is
refused with the notice (`form::settle_password`); `/api` takes what
it is given. A setup form that changes nothing resets at once
(`Then::Join`); one that changes anything is committed and the
persist task saves it, then resets.

Any press ends ringing or the IP display, and presses in the second
after it are ignored (`ui::PRESS_QUIET`), so a two-handed slap does not
step a setting with its second event. Set mode times out after 15 s,
ringing after 5 minutes. Self-heal: once the gateway has answered a ping, ten
failed probes in a row (five minutes) with the link still up blank the
panel and reset the chip (`config::PROBE_RESET_FAILURES`). The probe is
a ping of the largest frame the clock sends (`config::PROBE_PAYLOAD_BYTES`,
498 bytes at the 540-byte MTU) because a marginal uplink passes short
frames while full ones fail; so do ten
failed connects in a row or three minutes of them
(`config::CONNECT_RESET_ATTEMPTS`, `CONNECT_RESET_SECS`), because the
driver avoids the access point that last dropped it and only a reset
clears that. The boot line and the heartbeat carry the count since
power-up. The time survives a software reset: the display tick writes
the Unix time to RTC RAM every second, a deliberate reset writes it once
more and pauses `clock::RESET_PAUSE`, and the boot adds that pause back,
so the panel shows the time again before Wi-Fi is up, within a second
(the RTC timer is not used; esp-hal recalibrates it at every boot and an
offset from it drifted with the uptime since power-up). An alarm missed by up to
10 minutes across a power cut rings on the first valid time after boot;
after a software reset the alarm last rung-and-ended or skipped within
that grace is not rung again, while one cut off mid-ring comes back. A
self-heal due while the alarm rings waits for the ring to end, by a press
or its timeout, before blanking and resetting.

`POST /reload` with the `Bearer` token (never the PIN) answers once and
then resets the chip the deliberate way: panel blanked, time and alarm
state carried, not counted as a self-heal and not held for a ringing
alarm. It is the way to exercise the carry record from a script:
`curl -X POST -H "Authorization: Bearer $API_TOKEN" http://<ip>/reload`.

`POST /token` with `token=<new>` (8 to 32 printable characters without
spaces, empty to clear) and either `Bearer <current token>` or `PIN
<digits>` sets the API token. `GET /token` is the page for it: the
new token, the PIN it issues when opened just as `/update` does, and
whether one is set; an empty token asks the browser to confirm before
it clears. Nothing links to that page on purpose (the settings page
links to `/update` and no further, and the `form` tests assert it),
so the token is changed by someone who typed the address; it is
documented here and nowhere on the clock. The body is parsed before the PIN is spent, so a bad
token costs a `400` and not the PIN. `POST /pin` issues a PIN without
the page, on the panel and in the serial log and never in the
response, for a bench script (`scripts/pin.sh`). One PIN is one
action, upload or token.

## Home Assistant

The clock announces itself to Home Assistant over MQTT (M12, built
2026-11-07): one device named after the hostname, with the settings as
entities and the readings as sensors, through MQTT discovery; nothing
is typed into Home Assistant. The broker is the `mqtt_*` settings
above; an empty host is MQTT off, and clearing it on the page clears
the user and password with it. The client is `rust-mqtt` 0.6 (MQTT 5
only, so the broker must speak MQTT 5: Mosquitto 1.6 and later,
including Home Assistant's add-on, EMQX, NanoMQ), no TLS, QoS 0,
clean session, 60 s keepalive. The pure half is `core/src/mqtt.rs`,
the task `src/mqtt.rs`, spawned on the station only.

The id is `clock-` and the last three bytes of the MAC in hex
(`mqtt::DeviceId`), stable across hostname changes. Topics under
`esp32clock/<id>/`: `status` (`online`/`offline`, retained; `offline`
is also the will), `state` (the `/api` JSON byte for byte, retained,
published after connect, on every settings change debounced 300 ms,
on the ringing edges and every 60 s), `set/<key>` (the value as `/api`
takes it; one subscription `set/+`) and `cmd` (`play`, `silence`,
`reload`; its own subscription, which the first bench run found
missing). The keys `set/` takes are `mqtt::SET_KEYS` (colour,
brightness, alarm, armed, days, skip, melody, twelve_hour, temp_show,
temp_offset); anything else (`hostname`, `tz`, the network, the token,
the broker) is logged and dropped, so the bus is no way to move the
clock elsewhere. The alarm parser takes `HH:MM:SS` as Home
Assistant's `time` entity sends it. The discovery payload goes to
`homeassistant/device/<id>/config`, not retained, at every connect, on
a hostname change and when Home Assistant publishes `online` on
`homeassistant/status` (after a random delay under 2 s, as its docs
ask); every entity reads a key of the state JSON through `val_tpl`,
and the host test parses the payload and checks each key against the
JSON, each `set/` key against the allow list and each select option
against the parser. Nineteen entities: selects for colour, brightness,
melody and the panel temperature, a `time` for the alarm, switches for
armed, skip and 12 hour, a text for the days, a number for the
thermometer offset, sensors for temperature, light, signal, uptime and
self-heals, a binary sensor for ringing, buttons for play, silence and
reload.

The task resolves the host (an IPv4 literal or DNS), connects with a
5 s timeout each, and on any failure aborts, logs the reason once and
backs off from 5 s doubling to 60 s; a change to the broker settings
disconnects cleanly and starts over, a cleared host publishes
`offline` and waits. `poll_header` is the one cancel-safe client
future the loop selects on; a header is followed by `poll_body` to
completion, and publishes happen between polls. The three subscriptions are
confirmed by SUBACKs the loop matches by packet identifier: a reason
code of 0x80 or more, or no answer within 10 s, fails the session
with the reason on the page (`subscribe` only sends). A broker or
hostname change that lands during the start is compared after it,
not dropped with its notification. Received packets land
in a 1 KB `BumpBuffer`, reset after each event (the one `unsafe` in
the module). Before a deliberate reset (`heal::reboot`, `heal::reset`,
`heal::switch_mode`, the OTA reboot) `mqtt::goodbye` lets the task
publish `offline` and send DISCONNECT, within 500 ms, so Home
Assistant shows the clock unavailable at once rather than after the
broker's 90 s keepalive timeout; a panic cannot, and the will covers
it late. `cmd` `reload` says goodbye on its own connection and then
reboots. The heartbeat carries an `mqtt:` line (state, connects,
publishes, commands). Verify with `mosquitto_sub -v -t 'esp32clock/#'
-t 'homeassistant/#'` on the host.

Firmware updates: `POST /ota` with the application image as the body
and an `Authorization` header: `Bearer <API_TOKEN>` from `scripts/ota.sh`,
or `PIN <digits>` from the `/update` page. Opening the page makes the
clock draw four random digits, show them on the panel for a minute
(any press ends it; ringing and alarm-set take precedence) and log them,
so the uploader must be able to see the clock or the serial port. A PIN
is good for five minutes and one attempt; a wrong one locks new PINs out
for ten seconds. The attempt, right or wrong, takes the digits off the
panel (`auth::pin_to_show` yields `None`, `ui::hide_pin`). The page posts the chosen file the same way as the
script (raw body plus header, no multipart) and polls `/api` until the
clock is back. The credentials are checked before
the body is read, and a refused upload reads and discards the rest of
the body before answering, or the browser sees a reset instead of the
`403`; then the first sector is checked for the image and
app-descriptor magics and the hash-appended flag, the scan is stopped
(the panel is dark for the whole write, about 11 s for 680 KB), the
other slot is erased, written and read back a sector at a time, the
SHA-256 espflash appends is checked against the streamed body, `otadata`
is switched and the clock reboots with the time carried over. Any
failure, a truncated or corrupt file included, brings the panel back on
the running image with `otadata` untouched. The bootloader only
falls back on a bad checksum, so the carry record counts the boots of
the new image: a software reset before a gateway probe has been
answered or an HTTP request served bumps it, and the third such boot
(`config::OTA_BOOT_LIMIT`) switches back. A power cut clears the count.

## Layout

The pure modules live in `core/src/` and the firmware re-exports them
from `src/lib.rs`, so firmware code says `crate::settings`; inside
`core` the same `crate::` paths point at its own modules. Six modules
are split, the pure half in `core/src/` under the same name and the
hardware half in `src/`: `buttons` (event types / GPIO task), `clock`
(calendar arithmetic / the offset and the carry), `carry` (record
codec / RTC static and reset gate), `http` (head parser and
`RESPONSE_BUF` / sockets), `ota` (image constants, `Slot`,
`hash_chunk`, `version_from` / flash and partitions), `clip` (the
pulse-width `Player` and the ring's `Pacer` / I2S0, its DMA ring and
the pin). Each `core`
module ends with its `#[cfg(test)] mod tests`.

| file | role |
| --- | --- |
| `core/src/lib.rs` | `#![cfg_attr(not(test), no_std)]`; the module list; nothing else |
| `core/src/aip33628.rs` | frame format, bit-reversed current table, quad-nibble waveform encoder; pure `core` |
| `core/src/panel.rs` | COM/SEG map, font, per-position colour as duty `Levels` (no RGB; the palette is written in levels), `Panel::render` → `ScanImage` |
| `src/scan.rs` | cycle/transfer (one lap) builder; the scan task, which builds the SPI2 quad + DMA bus itself (an async esp-hal driver is `!Send`), runs one plain dark transfer so esp-hal programs the registers, starts the ring and from then on only acts on `stop` (the ring ends on its blanking frame within two laps, before a flash write), `resume` after a failed upload and `halt` before a deliberate reset (the stop latched: `resume` is void after it, so an upload failing under the reset's pause cannot restart the scan); `publish` forwards to the ring; `Report` for the heartbeat |
| `src/ring.rs` | the scan as one SPI2 DMA-controlled configurable segmented transfer (TRM 27.5.8.5) on a one-segment descriptor ring: `Ring`, a `DmaTxBuffer` of a conf descriptor (bitmap with magic 0xA and the USER word) and eleven data descriptors of two cycles each, the last pointing at the first, whose `prepare` switches the mode on in the window esp-hal leaves between programming the registers and setting `usr`; `request_stop` clears `usr_conf_nxt` in the conf USER word; `publish` builds into the spare buffer and repoints the data descriptors (`point_at`), holding the buffer it left for two laps before it is written again; `position` for the heartbeat. The one module with register access, through esp-hal's `regs()` |
| `src/display.rs` | UI loop: tick, button or settings change → `ui` → draw → publish; rings the alarm and signals the ringing edges (`ringing_changed`) for the MQTT state; `silence` ends a ring as a press would; shows the IP, or `AP` in place of the sweep on the access point; acts on the hold before the UI sees it; seeds and carries the skip flag and the time the alarm last rang |
| `core/src/ui.rs` | mode state machine, pure logic, one timer for the current mode; edits the settings it is handed; alarm due check and missed-alarm grace, both gated by the carried local second the alarm last rang (local rather than Unix, so the hour a fall-back transition repeats rings once; an alarm minute a spring-forward transition skips rings when the clock lands past it); consumes `skip_next`; `view` takes the room reading and answers the `Temp` face mid-minute in clock mode |
| `core/src/settings.rs` | `Settings`, palette and brightness tables, `Hostname`, `Text`/`Secret` strings, `Credentials`, the token and the MQTT `Broker` with their rules, the build-time `seed`, shared `Watch` with change signal, versioned append-only flash record |
| `src/storage.rs` | the one flash handle (`with_flash`), nvs partition lookup, `sequential-storage` map over it, debounced persist task that compares records, not structs, and resets into station mode after a save that changed the credentials, or after any save on the access point with a network stored |
| `src/heal.rs` | deliberate resets: `reboot` says the MQTT goodbye, blanks the panel through `scan::halt`, carries the time and resets, for `/reload` and the OTA reboot; `reset`, the self-heal, waits out a ringing alarm and counts itself first; `switch_mode`, the reset between station and access point, which starts the carry record over and freezes it |
| `core/src/auth.rs` | the API token (a setting; constant-time compare) and the panel PIN with its TTL and lockout; `token`, `token_or_pin`, `issue_pin`, `pin_to_show`; `Refusal` with its HTTP status; pure rules with the clock and the random number passed in |
| `core/src/form.rs` | key=value parsing, all-or-nothing `apply` on a copy with the `Allow` flags for the guarded keys, JSON, the HTML page with its error notice, its unsaved-changes mark (a script compares the form with the defaults it was served with) and the read-only `Status` lines, the update and token pages, each its own script over the shared `PIN_JS` helpers and `PIN_FIELD`, `JsonStr`/`HtmlStr` escaping for the SSID, `parse_token`; pure logic |
| `core/src/http.rs` | `Method`, `Head` (`content_length` is `None` when the header is absent, and `transfer_encoding` says one was sent), `parse_head`; `RESPONSE_BUF` and `PAGE_MAX`, which the page tests assert against |
| `core/src/mqtt.rs` | Home Assistant over MQTT, pure: `DeviceId` and its topics, `Link` (the state for the page), `Route` and `Command`, `SET_KEYS` and `apply_set` over `form::apply_one`, `write_discovery` (the device payload with every entity) and `DISCOVERY_MAX` |
| `src/mqtt.rs` | the MQTT task: resolve, connect with the will, subscribe, discovery and state publishes, keepalive pings, the select loop over `poll_header`, the back-off, `goodbye` for the reset paths, `link()` and `Report` for the page and the heartbeat |
| `src/http.rs` | HTTP/1.0 server, `LISTENERS` sockets each in its own task: a `POST` without `Content-Length` to a route that reads a body (`/`, `/api`, `/play`, `/token`, `/ota`; `reads_body`) is answered `411` and any `Transfer-Encoding` `400` before a route is looked at (`NoRequest`), since a bodiless post is not an empty form, while the command posts (`/reload`, `/ap`, `/pin`, `/scan`) take a bare `curl -X POST`; `GET`/`POST` on `/` (the settings page, or the setup form on the access point) and `/api`, `POST /play`, `GET /update` and `GET /token` (each issues a PIN), `POST /ota` with the body streamed to `ota::install`, `POST /reload` and `POST /ap` with the token, `POST /token` with the token or the PIN, `POST /pin`, `POST /scan` on the access point, `GET /favicon.ico` and `GET /tz.js` (`src/favicon.ico` and `src/tz.js` embedded, the two cacheable responses; the page loads the script as `/tz.js?v=<version>` so an update is not served the cached one); the settings, update and token pages are titled and headed `Clock: <hostname>`; a refused form redirects to `/?error=…`; per-listener state and counters for the heartbeat |
| `core/src/buttons.rs` | `Button`, `Kind`, `Event` |
| `src/buttons.rs` | debounced short/long press events, the two-button long press, and the 10 s two-button `Hold` |
| `core/src/melody.rs` | alarm tunes as pitch and length data with tempo; `RANDOM` and the pure `pick` that turns it into a tune; `CLIP_COUNT`, `CLIP_NAMES` and `CLIP_DATA` (included from `clips.rs`, which `core/build.rs` writes from `ALARM_CLIP`), `HAS_CLIP`, and one melody a clip (`Melody::clip`) after the nine tunes of `MELODIES`; pure |
| `core/src/carry.rs` | `Carried`, `MARK` with its layout version, `encode`/`decode` |
| `src/carry.rs` | RTC-RAM record used only after a software reset (`CoreSw`/`Cpu0Sw`; anything else zeroes it at boot): self-heal count and time, the skip flag, the local second the alarm last rang, the Unix time last written, boots of an unconfirmed OTA image, the access-point flag for the next boot; the mark carries a layout version; `freeze` for a mode switch |
| `src/buzzer.rs` | LEDC PWM on an open-drain pin; plays a melody on command, looped or once, stops within a note; a clip goes to `clip::play`, which hands it to the clip task on the same pin |
| `core/src/clip.rs` | sampled sound for the 1-bit buzzer line, pure: `Player`, a pulse-width modulator (one pulse per carrier period, centred, its length the level at mid-period with the rounding error fed back as far as the carrier leaves room: `ORDER`; integer, linear interpolation between the 8 kHz samples, a sample of 255 at 0.95 of the rail) that writes a clip's bitstream a buffer at a time, wrapped in 30 ms ramps from and to the released line; `Beeper`, a clip that is the line's level as run lengths (`BPR1`, LEB128 counts of 1/128 000 s, off and on alternately; `scripts/zxbeeper.py`), written out as it is; `Runs`, which writes a `RunSource` (the beeper clip) out as bytes; `Stream`, either kind by its first bytes; `Pacer`, which says which chunks of the ring to write from the time since the start alone (one crystal clocks the line and the timer), counts underruns, and counts as strays the reads of the DMA's descriptor register that are not two slots on from that; `BIT_RATE` and `CARRIER_HZ` from `config::CLIP_KBPS` and `CLIP_CARRIER_KHZ`, `CHUNK` 1024, `SLOTS` 16 |
| `src/clip.rs` | `clip_task`, on the scan's interrupt executor so that Wi-Fi work on the thread executor cannot starve the ring (the buzzer task, whose LEDC driver is not `Send`, stays there and asks with `play`, lending the pin for the clip's length; `main.rs` makes the second pin handle, its one `unsafe`); `Sampler`: I2S0's transmitter (TDM, two 8-bit channels, no clock pins, so the data line is the ring's bytes MSB first at the bit rate) on GDMA channel 1 looping over a sixteen-descriptor ring; `play` writes the clip's bits (`clip::Stream`) from flash into the ring ahead of the DMA, connects GPIO5 as a frozen open-drain signal through the GPIO matrix's inverter once the stream runs, and hands it back released before stopping; reads the DMA's descriptor register through `regs()`, for the pacer's check only |
| `core/src/sensors.rs` | the NTC table (10 kΩ, B 3950, under the adapter's 10 kΩ pull-up from 3V3) and `temperature_dc` by interpolation, `fahrenheit_tenths`, `ldr_ohms` and `light_pct` (the logarithm of the LDR's resistance, 1 kΩ is 100 and 50 kΩ is 0), `face` (the panel's four characters for a temperature), `Tenths`; pure |
| `src/sensors.rs` | `sensor_task`: the async `Adc<ADC1>` with `AdcCalCurve` (the scheme that reads straight on the C3) on GPIO0 (NTC) and GPIO1 (LDR), 8 samples a second each into a 10 s exponential average, published as millivolts in two atomics; `room()` for `http::status`, `Report` for the heartbeat |
| `core/src/tz.rs` | POSIX TZ rules: `parse` (`const`, so a build-time rule is checked by the compiler), `Tz::period_at(unix)` (the offset, the daylight flag and the name in force), `fixed(minutes)` for the old setting, the calendar arithmetic; `TZ_MAX` 48 |
| `src/tz.js` | the page's `Use this browser's zone` button, served as `/tz.js`: the browser's IANA zone as a POSIX rule from next year's two offset changes (so a zone dropping daylight time this year, as Vancouver did in 2026, comes out fixed), each transition's rule checked against the six years after (a midnight change is Saturday 24:00 in Chile and Sunday 00:00 elsewhere), names from the first locale that has letters; throws, and the page shows the message, for a zone with other than none or two changes a year or none of whose candidate rules predicts the six years after (Casablanca's Ramadan hour); `scripts/tz-check.mjs` runs it under Node |
| `core/src/clock.rs` | `LocalTime` (with the `offset_secs` it was computed with and `local_secs`), `weekday_of_days`, `local_time(unix, offset_secs)` |
| `src/clock.rs` | Unix time = uptime + NTP offset; `period_at` and `local_time` under the settings' rule; `carry_time`/`restore_carried` move the time across a software reset as plain Unix time, written every second by the display tick; `RESET_PAUSE` for every deliberate reset |
| `core/src/ota.rs` | image header and descriptor offsets, `Slot` names, `hash_chunk` for the appended SHA-256, `version_from` |
| `src/ota.rs` | firmware upload: slot lookup and the boot-time rollback, sector writer with read-back, activate, reboot; `VERSION` from `build.rs` |
| `src/net.rs` | Wi-Fi station on the stored credentials with all-channel scan and transmit-power ceiling, minute RSSI line, connect failures logged with their meaning (`wrong password?`, `no such network in range`) and a budget that ends in `heal::reset`; DHCP switched on 500 ms after each connect and off at disconnect; link and address monitor, gateway ping probe with the self-heal trigger, SNTP, DHCP renew on hostname change; the boot's `Mode`, the access-point task (AP+STA, scans on request into the network list) and its static stack config |
| `src/dhcpd.rs` | DHCP server for the access point: `parse`, `build` and the lease table pure, the socket task around them; every reply broadcast |
| `src/dnsd.rs` | captive DNS for the access point: `answer` pure (every A question is the clock, anything else an empty answer), the socket task around it |
| `core/src/config.rs` | build-time env vars and compile-time defaults |
| `core/build.rs` | `rerun-if-env-changed` for the build-time variables `config.rs` reads; writes `clips.rs` (count, names, labels and `include_bytes!` of the files in `ALARM_CLIP`) into `OUT_DIR` |
| `src/bin/main.rs` | pin setup, display wires with their pull-downs, executors, task spawning, panic reset hook, log timestamps, reset reason and carried time at boot, OTA rollback check, minute heartbeat log |

## Hardware facts that bite

- **Buzzer (GPIO5)** drives a PNP with its emitter at +5 V. Any driven level
  turns it on, including a 3.3 V high. Only a released open-drain line is
  silent. Never make it push-pull. The sampled clip drives it from
  I2S0 with the GPIO matrix's inverter on, so a 1 bit is the buzzer
  on; a peripheral given the bare pin would make it push-pull, so it
  goes in as an open-drain `Output` turned into a signal, which
  esp-hal freezes. The inverter is part of the pin's connection:
  `clip.rs` takes the pin back as a plain `Output` when it ends,
  which clears it, and a tone's LEDC must never be connected over an
  inverted pin (its released level would be the buzzer on). The
  clip's bits come in long pulses: a per-bit sigma-delta stream at
  256 kbit/s was quieter than one pulse per 16 kHz period
  (2026-11-12). On the scope the pulses at the buzzer are as wide as
  GPIO5's, to the full 5 V, and with no diode on the board the coil
  kicks to −10…−15 V at each turn-off; the clip is still much quieter
  than the beeps, which sit on the buzzer's resonance. The phantom
  presses seen while a clip played were the user's adapter, pins 9
  and 10 of the DIP-16 shorted, not interference. The user's mod, in
  the README as optional: a 1N4148 across the buzzer as a flyback
  diode (cathode to the collector side), which takes the negative
  peak away, and R5 at 5.1 kΩ instead of 10 kΩ, whose doubled base
  current squares the collector's falling edge (a slope with 10 kΩ)
  and makes the buzzer louder. The firmware assumes neither.
- **The GDMA's `out_dscr` is not the descriptor being read.** On the
  C3 it names the one two further on, from the moment a transfer
  starts (traced against the clock, 2026-11-12). The clip's pacer took
  it for the current one and rewrote every chunk as it was played,
  which was heard as skips, crackle and the clip overlapping itself,
  with no underrun counted. Pace a DMA ring by time; use the register
  as a check at most. `ring::position`, the scan heartbeat's figure, is
  off by the same two.
- **Display wires (GPIO6–9)** are SPI2 SIO0–3. The SPI builder resets the
  pull of a bare pin it takes, but leaves a `Flex` alone (it arrives as a
  frozen signal), so `main.rs` sets the idle pull-downs on `Flex` pins and
  the scan task builds the bus from those. No PAC access for the pins;
  `ring.rs` and `clip.rs` are the modules that touch registers, SPI2's
  segmented-mode bits and each DMA channel's current descriptor, through
  esp-hal's `regs()`.
- **Buttons:** GPIO4 is the upper button, GPIO3 the lower. No board
  pull-ups; the internal ones are used.
- **Sensors (GPIO0, GPIO1):** the adapter's 10 kΩ pull-ups from 3V3
  with the clock board's NTC (R4) and LDR (R3) to ground; the board's
  own R1 and R2 pull up to 5 V and must stay off. A pin reads high
  when the part is cold, dark or missing, so only the NTC can report
  "no sensor" (the pull-up alone is colder than −20 °C); a missing
  LDR looks dark. The NTC reads a few degrees above the room on the
  bench and creeps up with uptime (2026-11-01): the board warms it;
  the user's thermometer is the reference, not the reading, and the
  `temp_offset` setting carries the difference (−0.7 on blue-clock). A clock
  built on the adapter with the C3 Zero's chip antenna hears the
  access point 10 to 25 dB weaker than the old bench board did at the
  same spot (the antenna sits inside the adapter's ground fill); a
  U.FL receptacle at the feed and a patch antenna put 25 to 30 dB
  back (2026-11-01: −29 dBm on the near access point at the bench;
  installed in the case, both access points read it at −70). The
  SuperMini adapter with a SuperMini Plus and an external antenna on
  its IPEX jack reads
  −21 to −23 there, against −38 to −54 for the old white-clock board in
  the same case and spot (2026-11-12; the access points' side not yet
  read); the v3 Zero adapter, its chip antenna over a notch at the
  board's edge, reads −52 to −60, level with the old board at the near access point,
  and unlike it always lists the far one, at −86 to −92. ADC1 is the converter that works with Wi-Fi on,
  11 dB attenuation is only calibrated to about 2.5 V, and
  `AdcCalCurve` is the scheme to use: the line fit is percent off.
- **Current:** `IS` comes from brightness alone, capped at
  `MAX_CURRENT_STEP`, a build-time variable, default step 5 (the 50 %
  step, about 15 mA per sink); `settings::BRIGHTNESS_STEPS` is cut at
  the cap, so the button, the page and the API offer 10, 25 and 50 % by
  default and a stored higher step lands on the brightest one offered.
  With the transmit ceiling at 16 dBm the
  board browns out on a quick step to 75 % or 100 % at the old cap of
  11; the supply, not the firmware, is the limit (2026-10-22). Never
  derive the current from how much is lit, and never park the panel
  all-white at high current; the drivers get hot.
- **Wi-Fi power:** `WIFI_MAX_TX_POWER_QDBM` is 68, which the driver
  rounds to its 16 dBm level (the level table is in the constant's
  comment; the boot log prints the setting). The access point hears the
  clock about 20 dB below what the clock hears of it, so the uplink is
  the side that fails, and the clock's own RSSI line looks fine while
  pages and lease renewals are lost. 14 and 16 dBm each gained a full
  step at the access point; 18 and 20 dBm read louder at the near AP
  but quieter at the far one, with slower probes and failed
  associations, which is the SuperMini antenna mismatch. Measure any
  change with the AP's signal figure for the clock, from inside the
  case, not with the clock's RSSI.
- **First frame after connect:** the driver reports the station
  connected before it can pass a frame, so a DHCP discover sent at that
  instant is lost and smoltcp retries only after 10 s. The stack starts
  with no IPv4 config and `net::wifi_task` enables DHCP `LINK_SETTLE`
  after the connect. embassy-net does not wake its runner on a config
  change; `net::poke` (a throwaway UDP bind) does, and must follow any
  `set_config_v4`.
- **Flash writes** run with interrupts off, so the scan stalls: a record
  write is invisible, a sector erase is a brief dark blink. That is why the
  persist task debounces and skips writes that would not change the record,
  and why a firmware upload stops the scan first rather than flicker
  through 200 erases. A save that fails is tried again after 2 s,
  doubling to 60 s, with whatever the settings are by then.
- **One flash handle.** `storage::with_flash` is the only way to the
  flash, one blocking operation at a time, never across an await. A
  second `FlashStorage` does not work: its constructor sizes the flash
  with a chip-ID command over SPI1, and after the first handle has used
  the flash that read comes back 0, so every operation on the second
  handle fails its bounds check (measured 2026-10-22).

## Timing rules in the scan path

- The CPU is not in the scan loop. The transfer is one segment of SPI2's
  configurable segmented transfer on a descriptor ring (`ring.rs`), and
  the hardware runs lap after lap by itself; the gap between laps is the
  CONF state, a fixed 0.6 µs after the blanking frame with the lines idle
  (bench and scope, 2026-11-08). Until then the scan task restarted every
  transfer: 10 to 19 µs each time, and up to 1 ms whenever a critical
  section held the done interrupt off (esp-sync's `RawMutex` masks every
  interrupt on this chip: the Wi-Fi blob's lock, the heap lock on every
  frame buffer, esp-println's lock while a monitor drains the USB FIFO),
  which no priority could fix; the history is in PLAN.md under 2026-11-07
  and 2026-11-08. The scan task keeps its interrupt executor, but nothing
  time-critical runs on it now.
- A frame must fit in one sub-frame unit (`UNIT_BYTES >= FRAME_BYTES`, a
  compile-time assert). Raising `SPI_HZ` or lowering `UNIT_US` changes the
  refresh rate; 20 MHz / 5 µs / three planes gives 7.1 kHz, which is what
  the shimmer tests settled on. Fewer planes shorten the cycle; more planes
  make it visibly shimmer.
- The lap ends with a blanking frame so the CONF state between laps is
  dark. Keep it. A data descriptor holds whole cycles (two, 2800 bytes;
  the last 2847 with the blanking frame; `CHUNK_SIZE` is 4092, asserted),
  so an image swap, which repoints the descriptors one aligned store at
  a time, changes the image at a cycle boundary within one lap.
- A published buffer may still be read for the rest of the lap in flight,
  so `ring::publish` waits `HOLD` (two laps and a bit) after a swap before
  it builds into the buffer it left. One publisher, the display task.
- `stop` takes up to two laps, about 6 ms: the cleared `usr_conf_nxt` in
  the conf buffer is applied at the next CONF state and that lap is the
  last. The ring's data stays the live image, so `resume` shows what was
  showing.
- `Instant - Duration` panics when uptime is shorter than the duration. Use
  `Option<Instant>` for "last time X happened".
- Never reset the chip while a lap is in flight: the row the drivers
  last latched stays lit at full current until the firmware is back. A
  deliberate reset goes through `heal::reset`, which waits for
  `scan::halt` (the ring ends on its blanking frame, and the stop
  is latched against an upload's `resume`) first. The panic
  path in `custom_halt` cannot do this and is the one exception.
- The heartbeat's `scan` field is images published, stops (and stops that
  had to cancel the transfer, which the bench never showed), running or
  halted, and the index of the descriptor the DMA is reading (0 the conf
  descriptor, 1 to 11 the data), which must differ between heartbeats.

## Sizes that bite

- `http::RESPONSE_BUF` (6 KB, one per listener task; raised from 4096
  on 2026-11-02) holds the whole page plus headers, and
  overflow is a 500, not a truncation. The headers are 88 bytes;
  `core::http::PAGE_MAX` (6016) is what a page may take, and the
  `form` tests build each page at its worst case and assert it
  (2026-11-07: settings 5564 in its four fieldsets with the Play and unsaved-changes script, the Wi-Fi, MQTT and local-time lines, a stored SSID of 32 `"`, a 48-byte timezone rule of `<` (four bytes each through `HtmlStr`), a 64-byte broker host twice, a user of 32 `"`, the hostname in the title and heading and the version in the script tag; update 2492 and token 2059 since the split of 2026-11-02; setup with twelve 32-character networks 3193). `cargo host-test` after adding a field
  or label; the test prints the sizes with `-- --nocapture`. The setup
  page's network list stops at `form::NETWORK_LIST_BUDGET` (1900)
  bytes, strongest first, with a "N more not shown" line, because a
  listed SSID of 32 `"` is 417 bytes of `<option>` through `HtmlStr`
  (twice per network) and five of them would pass the buffer; the rest of the page at its worst is 2324, and `provision_page_budget_is_covered`
  locks the sum under `PAGE_MAX`; the budget was set when the page
  had 3968 and stays, since twelve networks is plenty.
- `http::RX_BUF` (4096) is the TCP receive window; 1 KB made an upload
  one segment per round trip. `REQUEST_BUF` (1536) still bounds the
  head; an upload's body stays on the socket.
- An OTA image must fit a slot (`partitions.csv`, 0x1E0000 = 1920 KB);
  the release image is about 650 KB. Static RAM: `ota.rs` holds 11 KB
  of buffers, and what the statics leave of DRAM is the main stack,
  66.9 KB on 2026-11-08 (67.1 on 2026-11-07; the ring's descriptors and
  conf words are 160 bytes, the gap statistics and buffer channels went) (`Settings` is a 385-byte record and sits in several task futures: the 49-byte timezone rule cost 2.2 KB of stack); check `.stack` in `llvm-readelf -S` after adding one, and compare with a build of `HEAD` in a worktree when in doubt: one `Response` per listener loop, since a second one in another arm of the same loop was not overlapped by the compiler and cost 6 KB per listener until it was hoisted (2026-11-07). `src/tz.js` (5.8 KB) is sent from one response buffer and a `const` assert in `src/http.rs` keeps it under `RESPONSE_BUF` with its header; it was trimmed to comments of a line each to fit.
- The sampled clip is 8 KB of flash a second (the 27.6 s test clip is
  221 KB of `.rodata`, held once: `melody::CLIP_DATA` is a `static`; a beeper clip is about 1 KB a second)
  and must leave the image inside its slot; the ring is 16 KB of
  static RAM, sized 0 in a build without a clip (2026-11-12: the main
  stack is 68.4 KB without, 51.5 KB with). A chunk is 16 ms at
  512 kbit/s and the ring holds fourteen ahead of the DMA, so the clip
  task not running for about 220 ms (twice that at 256 kbit/s) is an
  underrun, logged when the clip ends with the count of DMA positions
  the pacer did not believe; it is on the interrupt
  executor for that reason, and a chunk's modulating, a millisecond
  or two, runs there above the Wi-Fi driver.
- `form::apply` decodes each key into 16 bytes and each value into 64
  (`form::VALUE_MAX`, the password); longer input is reported as a bad
  key or value before parsing. The record is `settings::RECORD_LEN` (385) bytes and the map scratch in `storage.rs` 512, which must hold
  the item (record plus 9 bytes, padded to a word).
- The MQTT task's future holds its socket buffers (1 KB receive, 2 KB
  send), the 1 KB `BumpBuffer`, the state JSON (1 KB) and the
  discovery payload (`mqtt::DISCOVERY_MAX`, 4608; the worst case is
  4174, printed by the `mqtt` test), about 15 KB of static RAM in all
  (2026-11-07: the main stack went from 84.5 KB to 69.6 KB).
- `settings::RECEIVERS` is the number of `Watch` receivers (display,
  persist, hostname, MQTT). Add one when a new task subscribes, or `receiver()`
  panics at boot.
- `StackResources<11>` in `main.rs` covers DHCP, DNS, SNTP, the gateway
  probe, the MQTT connection and the HTTP listeners with headroom (on the access point: the
  DHCP and DNS servers and the listeners); adding sockets means raising it.
- `HOSTNAME_MAX` (24) stays under embassy-net's 32-byte DHCP option.
- `ESP_RADIO_CONFIG_WIFI_MTU` (540, in `.cargo/config.toml`'s `[env]`,
  read back as `config::WIFI_MTU`) bounds every frame the clock sends:
  TCP segments carry 486 bytes, the probe 498, and a page is seven
  segments instead of two. It is a transmit-side limit only (smoltcp
  applies it when emitting; the driver passes received frames whole),
  chosen because the access point hears the clock 20 dB below what the
  clock hears of it and short frames pass where full ones fail
  (2026-11-01, the adapter-built clocks at −52 to −65 dBm). Two
  consequences: a TCP peer is told the 486-byte segment size and sends
  an upload in 486-byte pieces, and a ping larger than 498 bytes from
  the LAN gets no reply, since the reply would not fit;
  `scripts/watch-clock.sh` pings with 498. Raising it means the probe
  grows with it; nothing else depends on the number.
- `config::WIFI_RX_QUEUE_FRAMES` (32) is the driver's receive queue;
  esp-radio's default of 5 dropped bursts of LAN broadcasts, visible as
  `RX QUEUE FULL` with `esp_radio=debug`. The stack answers ping only
  because embassy-net's `auto-icmp-echo-reply` feature is on.

## Verifying without a board

`cargo host-test` runs the tests of `core/` on the host (103 on 2026-11-12): the AiP33628 encoder against a behavioural model of the
chip (shift on CLK rise, latch on DIN rise while CLK high), the panel
map, the palette and brightness tables, the hostname, password and
token rules, the record round trip and every older version, the form
parser and the page sizes against `PAGE_MAX`, the head parser, the
appended-hash split, the carry codec, the calendar arithmetic, the POSIX TZ parser with its rejects and the 2026 transitions of both hemispheres, the melodies, the clip's modulator (a sine back out of the bits, one centred pulse a period, the ramps, full-scale stability), the beeper clip's runs bit for bit, and the ring's pacer, run against a model of the DMA (`CLIP_KBPS` and `CLIP_CARRIER_KHZ` change what is tested; `ALARM_CLIP=… cargo host-test` covers the table with the clip in it), the UI's alarm, grace, skip and daylight-saving logic, the PIN and token
header rules, the NTC and LDR tables against their equations, and the
MQTT topics, routes and discovery payload (parsed with `serde_json`, a
host-only dev-dependency, and checked key by key against the `/api`
JSON and the form parser). `cargo clippy -p esp32clock-core --target host-tuple
--tests` lints the tests. `node scripts/tz-check.mjs` checks the browser half of the timezone field, `src/tz.js`, against zones with known rules under Node, whose `Intl` is a browser's, and against Casablanca, which must throw. `.cargo/config.toml` aims every cargo command at the chip, which is why the alias passes `--target host-tuple`; the `[unstable] build-std` line there is ignored by
stable cargo. Not under test: `scan.rs`'s
`build_cycle` (the M1 lit-time check needs it on the host), `ring.rs`
(descriptors and registers; its layout is `const`-asserted), `src/clip.rs` (the I2S transfer and the pin), `dhcpd.rs`
and `dnsd.rs` (their pure halves
use embassy-net's address types), `storage.rs`'s map over the flash,
`mqtt.rs`'s session loop.
`PLAN.md` records the checks of each milestone; new ones are tests.

## Conventions

- `no_std`, no `alloc` use outside what esp-radio needs; the heap exists
  for the Wi-Fi driver.
- The target is riscv32imc: atomics have load and store only, no
  `fetch_add` or `swap`. Counters shared with the heartbeat have one
  writer and are bumped with a load and a store; anything with more than
  one writer goes behind `critical_section`.
- Keep hardware out of `core/`: it has no esp dependency and must
  build for the host. New pure logic goes there with its tests; a
  module that needs a peripheral and has a pure half is split the way
  `clock`, `carry`, `http` and `ota` are.
- Settings change only through `settings::update`; it is what wakes the
  display and the persist task. The HTTP handlers apply a request to a
  copy and commit it with one `update` only if everything parsed; keep
  parsing out of the watch's closure.
- Adding a setting touches, in order: the `Settings` field and `DEFAULT`,
  `is_valid`, `RECORD_VERSION` with the new field appended (never
  reordered: an older image reads a newer record by its prefix, so a
  rollback keeps the settings) and `from_record` still reading every
  older version, `form::apply_one`, `write_json` and `write_page`,
  the `PLAN.md` entry, and the `settings` and `form` tests (the record
  round trip, `apply_sets_every_key`, the page sizes). A setting Home
  Assistant should see gets a component in `mqtt::write_discovery`
  and, if it may be set from there, its key in `mqtt::SET_KEYS`; the
  `mqtt` test ties the three together. A key only the token may set
  goes behind `form::Allow`. A field that
  must not persist, like `skip_next`, is left out of the record on
  purpose. Read-only status goes in `form::Status`, built in
  `http::status`, so `form.rs` stays clockless.
- Tables copied from the reference docs (`BLOCKS`, `SEGMAP`, `COM_SEQ`,
  `IS_WIRE`) are verbatim; cite `3rdparty/HU-058_ESPHome/docs/` when
  changing anything near them.

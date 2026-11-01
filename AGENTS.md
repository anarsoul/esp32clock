# esp32clock — guide for agents

Firmware for the HU-058 / HU-058D LED clock panel driven by an ESP32-C3,
written in Rust with esp-hal and Embassy on esp-rtos. The display's two
AiP33628 drivers are fed by SPI2 in quad mode used as a four-wire pattern
generator with DMA; see `PLAN.md` for the full design and its history.
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
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` (the user asked
  for this trailer in this project).
- `3rdparty/` is the reverse-engineering reference (public domain), ignored
  by git. Read it; do not modify it.
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
WIFI_SSID='…' WIFI_PASSWORD='…' TZ_OFFSET_MINUTES=-420 API_TOKEN='…' cargo run --release
MAX_CURRENT_STEP=8 cargo build --release   # optional: raise the drive-current ceiling (default 5)
API_TOKEN='…' scripts/ota.sh <ip>      # later updates, over the LAN
scripts/ota.sh                          # just the image file, for http://<ip>/update
API_TOKEN='…' scripts/reload.sh <ip>   # software reset with the carry record, waits for the clock to come back
scripts/pin.sh <ip> <serial-log>        # POST /pin, then the four digits read back from the monitor's log
AP_PASSWORD='…' cargo build --release   # optional: the clock's own access point's password (default esp32clock)
```

`cargo run` flashes with espflash and opens the monitor; `.cargo/config.toml`
sets the runner and `ESP_LOG=info`. The runner passes `partitions.csv`
(two app slots, `ota_0` and `ota_1`, plus `otadata`) and erases
`otadata` so a USB flash always boots `ota_0`, where espflash put the
image. `scripts/ota.sh` builds, makes the application image with
`espflash save-image` and posts it to `/ota` with the token; the clock
writes the other slot, selects it and reboots. The token is
`API_TOKEN` at build time and a setting after that (`src/auth.rs`): a
build without one has no script path (a `Bearer` request is `403`)
until one is set from `/update` with the PIN or by `POST /token`, and
the boot line says `api token: stored`, `built in`, `cleared` or
`none`. The Wi-Fi credentials are seeds the same way, and `WIFI_SSID`
may be empty: whatever the record lacks is filled from the build at
boot, the next save carries it, and from then on the record wins over
a later build's constants (`settings::Settings::seed`). A token
cleared with `POST /token` stays cleared: the record's flags byte
(`token_cleared`, version 7) tells a cleared token from one never set,
and only the latter is filled from the build. For a network debugging run set
`ESP_LOG='info,embassy_net=debug,esp_radio=debug'` on the command line
(it overrides the config, and esp-println compiles the filter in); the
extra lines are DHCP state and driver events, not packets. Every log
line carries the uptime in ms. `scripts/watch-clock.sh <ip>` sends a
full-size ping and fetches `/api` from the host every 30 s for the other
side of the story.

Credentials, the default timezone and the API token are read at build
time by `core/src/config.rs`; `core/build.rs` reruns that crate when
they change, and the root `build.rs` exports `git describe --always
--dirty` as `FIRMWARE_VERSION`, which
goes into the app descriptor, the boot line, the page and `/api`. The timezone, 12/24 hour, colour, brightness, alarm, alarm days,
alarm melody, thermometer offset (`temp_offset`, tenths of a degree
Celsius added to the NTC's reading, ±10.0, for the warm air at the
sensor's spot in the case; blue-clock measured 0.7 high), the
temperature on the panel (`temp_show`: `off`, `c` or `f`, default
`c`; with a reading in hand the UI shows ` 23C` with the degree mark
for seconds 30 to 34 of every minute in clock mode, `ui::TEMP_SHOW_AT`
and `TEMP_SHOW_SECS`), DHCP
hostname, Wi-Fi network and API token are settings:
saved in the `nvs` partition, editable from the buttons and from
`http://<ip>/` (form) or `/api` (form-encoded keys, JSON back). The
network keys `ssid` and `password` are taken on `/api` only with the
`Bearer` token, refused as `403` on the key alone (never on whether the
value would change, which would make a password oracle), and never on
the settings form; a saved change resets the clock into the new network
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
to 100, `null` before the first reading) and `last_heal` (Unix seconds
or null); posting them is refused as an unknown key. The page shows the
room on a line under the status (`Room 23.4 °C (74.1 °F), light 62 %`),
and the minute heartbeat logs the filtered millivolts of both dividers
for calibration. `skip_next` (page checkbox, key `skip`) is
the one setting the record does not store: it skips one alarm and is cleared
by it or by a power cut; a software reset (self-heal, panic) carries it over
in RTC RAM, see `src/carry.rs`. `POST /play` previews the melody chosen in the form
without saving. `rust-version` must stay at 1.95 or higher or cargo's
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

Opening `http://<ip>/update` shows a four-digit PIN for a minute.

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
the list), and the setup form lists what it saw, strongest first, with
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

Any press ends ringing or the IP display. Set mode times out after 15 s,
ringing after 5 minutes. Self-heal: once the gateway has answered a ping, ten
failed probes in a row (five minutes) with the link still up blank the
panel and reset the chip (`config::PROBE_RESET_FAILURES`). The probe is
a full-size ping (`config::PROBE_PAYLOAD_BYTES`, 1400 bytes) because a
marginal uplink passes short frames while page-sized ones fail; so do ten
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
<digits>` sets the API token; the update page has a form for it next
to the upload. The body is parsed before the PIN is spent, so a bad
token costs a `400` and not the PIN. `POST /pin` issues a PIN without
the page, on the panel and in the serial log and never in the
response, for a bench script (`scripts/pin.sh`). One PIN is one
action, upload or token.

Firmware updates: `POST /ota` with the application image as the body
and an `Authorization` header: `Bearer <API_TOKEN>` from `scripts/ota.sh`,
or `PIN <digits>` from the `/update` page. Opening the page makes the
clock draw four random digits, show them on the panel for a minute
(any press ends it; ringing and alarm-set take precedence) and log them,
so the uploader must be able to see the clock or the serial port. A PIN
is good for five minutes and one attempt; a wrong one locks new PINs out
for ten seconds. The page posts the chosen file the same way as the
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
`core` the same `crate::` paths point at its own modules. Five modules
are split, the pure half in `core/src/` under the same name and the
hardware half in `src/`: `buttons` (event types / GPIO task), `clock`
(calendar arithmetic / the offset and the carry), `carry` (record
codec / RTC static and reset gate), `http` (head parser and
`RESPONSE_BUF` / sockets), `ota` (image constants, `Slot`,
`hash_chunk`, `version_from` / flash and partitions). Each `core`
module ends with its `#[cfg(test)] mod tests`.

| file | role |
| --- | --- |
| `core/src/lib.rs` | `#![cfg_attr(not(test), no_std)]`; the module list; nothing else |
| `core/src/aip33628.rs` | frame format, bit-reversed current table, quad-nibble waveform encoder; pure `core` |
| `core/src/panel.rs` | COM/SEG map, font, per-position colour as duty `Levels` (no RGB; the palette is written in levels), `Panel::render` → `ScanImage` |
| `src/scan.rs` | cycle/transfer builder, double-buffered DMA scan task that builds the SPI2 quad + DMA bus itself (an async esp-hal driver is `!Send`), restart-gap statistics, `stop` on a blank frame before a deliberate reset or a flash write, `resume` after a failed one |
| `src/display.rs` | UI loop: tick, button or settings change → `ui` → draw → publish; rings the alarm; shows the IP, or `AP` in place of the sweep on the access point; acts on the hold before the UI sees it; seeds and carries the skip flag and the time the alarm last rang |
| `core/src/ui.rs` | mode state machine, pure logic, one timer for the current mode; edits the settings it is handed; alarm due check and missed-alarm grace, both gated by the carried time the alarm last rang; consumes `skip_next`; `view` takes the room reading and answers the `Temp` face mid-minute in clock mode |
| `core/src/settings.rs` | `Settings`, palette and brightness tables, `Hostname`, `Text`/`Secret` strings, `Credentials` and the token with their rules, the build-time `seed`, shared `Watch` with change signal, versioned append-only flash record |
| `src/storage.rs` | the one flash handle (`with_flash`), nvs partition lookup, `sequential-storage` map over it, debounced persist task that compares records, not structs, and resets into station mode after a save that changed the credentials, or after any save on the access point with a network stored |
| `src/heal.rs` | deliberate resets: `reboot` blanks the panel through `scan::stop`, carries the time and resets, for `/reload` and the OTA reboot; `reset`, the self-heal, waits out a ringing alarm and counts itself first; `switch_mode`, the reset between station and access point, which starts the carry record over and freezes it |
| `core/src/auth.rs` | the API token (a setting; constant-time compare) and the panel PIN with its TTL and lockout; `token`, `token_or_pin`, `issue_pin`, `pin_to_show`; `Refusal` with its HTTP status; pure rules with the clock and the random number passed in |
| `core/src/form.rs` | key=value parsing, all-or-nothing `apply` on a copy, JSON, the HTML page with its error notice and the read-only `Status` lines, the update page with its upload and token scripts, `JsonStr`/`HtmlStr` escaping for the SSID, `parse_token`; pure logic |
| `core/src/http.rs` | `Method`, `Head`, `parse_head`; `RESPONSE_BUF` and `PAGE_MAX`, which the page tests assert against |
| `src/http.rs` | HTTP/1.0 server, `LISTENERS` sockets each in its own task: `GET`/`POST` on `/` (the settings page, or the setup form on the access point) and `/api`, `POST /play`, `GET /update`, `POST /ota` with the body streamed to `ota::install`, `POST /reload` and `POST /ap` with the token, `POST /token` with the token or the PIN, `POST /pin`, `POST /scan` on the access point; a refused form redirects to `/?error=…`; per-listener state and counters for the heartbeat |
| `core/src/buttons.rs` | `Button`, `Kind`, `Event` |
| `src/buttons.rs` | debounced short/long press events, the two-button long press, and the 10 s two-button `Hold` |
| `core/src/melody.rs` | alarm tunes as pitch and length data with tempo; `RANDOM` and the pure `pick` that turns it into a tune; pure |
| `core/src/carry.rs` | `Carried`, `MARK` with its layout version, `encode`/`decode` |
| `src/carry.rs` | RTC-RAM record used only after a software reset (`CoreSw`/`Cpu0Sw`; anything else zeroes it at boot): self-heal count and time, the skip flag, the time the alarm last rang, the Unix time last written, boots of an unconfirmed OTA image, the access-point flag for the next boot; the mark carries a layout version; `freeze` for a mode switch |
| `src/buzzer.rs` | LEDC PWM on an open-drain pin; plays a melody on command, looped or once, stops within a note |
| `core/src/sensors.rs` | the NTC table (10 kΩ, B 3950, under the adapter's 10 kΩ pull-up from 3V3) and `temperature_dc` by interpolation, `fahrenheit_tenths`, `ldr_ohms` and `light_pct` (the logarithm of the LDR's resistance, 1 kΩ is 100 and 50 kΩ is 0), `face` (the panel's four characters for a temperature), `Tenths`; pure |
| `src/sensors.rs` | `sensor_task`: the async `Adc<ADC1>` with `AdcCalCurve` (the scheme that reads straight on the C3) on GPIO0 (NTC) and GPIO1 (LDR), 8 samples a second each into a 10 s exponential average, published as millivolts in two atomics; `room()` for `http::status`, `Report` for the heartbeat |
| `core/src/clock.rs` | `LocalTime`, `weekday_of_days`, `local_time(unix, tz_minutes)` |
| `src/clock.rs` | Unix time = uptime + NTP offset; `local_time` with the settings' timezone; `carry_time`/`restore_carried` move the time across a software reset as plain Unix time, written every second by the display tick; `RESET_PAUSE` for every deliberate reset |
| `core/src/ota.rs` | image header and descriptor offsets, `Slot` names, `hash_chunk` for the appended SHA-256, `version_from` |
| `src/ota.rs` | firmware upload: slot lookup and the boot-time rollback, sector writer with read-back, activate, reboot; `VERSION` from `build.rs` |
| `src/net.rs` | Wi-Fi station on the stored credentials with all-channel scan and transmit-power ceiling, minute RSSI line, connect failures logged with their meaning (`wrong password?`, `no such network in range`) and a budget that ends in `heal::reset`; DHCP switched on 500 ms after each connect and off at disconnect; link and address monitor, gateway ping probe with the self-heal trigger, SNTP, DHCP renew on hostname change; the boot's `Mode`, the access-point task (AP+STA, scans on request into the network list) and its static stack config |
| `src/dhcpd.rs` | DHCP server for the access point: `parse`, `build` and the lease table pure, the socket task around them; every reply broadcast |
| `src/dnsd.rs` | captive DNS for the access point: `answer` pure (every A question is the clock, anything else an empty answer), the socket task around it |
| `core/src/config.rs` | build-time env vars and compile-time defaults |
| `core/build.rs` | `rerun-if-env-changed` for the build-time variables `config.rs` reads |
| `src/bin/main.rs` | pin setup, display wires with their pull-downs, executors, task spawning, panic reset hook, log timestamps, reset reason and carried time at boot, OTA rollback check, minute heartbeat log |

## Hardware facts that bite

- **Buzzer (GPIO5)** drives a PNP with its emitter at +5 V. Any driven level
  turns it on, including a 3.3 V high. Only a released open-drain line is
  silent. Never make it push-pull.
- **Display wires (GPIO6–9)** are SPI2 SIO0–3. The SPI builder resets the
  pull of a bare pin it takes, but leaves a `Flex` alone (it arrives as a
  frozen signal), so `main.rs` sets the idle pull-downs on `Flex` pins and
  the scan task builds the bus from those. No PAC access.
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
  `temp_offset` setting carries the difference (−0.7 on blue-clock). The new
  clock also hears the access point 10 to 25 dB weaker than the old
  bench board did at the same spot. ADC1 is the converter that works with Wi-Fi on,
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
  through 200 erases.
- **One flash handle.** `storage::with_flash` is the only way to the
  flash, one blocking operation at a time, never across an await. A
  second `FlashStorage` does not work: its constructor sizes the flash
  with a chip-ID command over SPI1, and after the first handle has used
  the flash that read comes back 0, so every operation on the second
  handle fails its bounds check (measured 2026-10-22).

## Timing rules in the scan path

- The scan task runs on an interrupt executor so Wi-Fi threads cannot delay
  it. Nothing slow may run between one DMA transfer ending and the next
  starting: no logging, no building. Transfers are double buffered; the
  publisher builds in thread mode.
- A frame must fit in one sub-frame unit (`UNIT_BYTES >= FRAME_BYTES`, a
  compile-time assert). Raising `SPI_HZ` or lowering `UNIT_US` changes the
  refresh rate; 20 MHz / 5 µs / three planes gives 7.1 kHz, which is what
  the shimmer tests settled on. Fewer planes shorten the cycle; more planes
  make it visibly shimmer.
- The cycle ends with a blanking frame so the gap between transfers is
  dark. Keep it.
- `Instant - Duration` panics when uptime is shorter than the duration. Use
  `Option<Instant>` for "last time X happened".
- Never reset the chip while a transfer is in flight: the row the drivers
  last latched stays lit at full current until the firmware is back. A
  deliberate reset goes through `heal::reset`, which waits for
  `scan::stop` (the transfer ends on its blanking frame) first. The panic
  path in `custom_halt` cannot do this and is the one exception.

## Sizes that bite

- `http::RESPONSE_BUF` (4096) holds the whole page plus headers, and
  overflow is a 500, not a truncation. The headers are 88 bytes;
  `core::http::PAGE_MAX` (3968) is what a page may take, and the
  `form` tests build each page at its worst case and assert it
  (2026-11-01: settings 3121, update 2989, setup with twelve
  32-character networks 3039). `cargo host-test` after adding a field
  or label; the test prints the sizes with `-- --nocapture`. The setup
  page's network list stops at `form::NETWORK_LIST_BUDGET` (1900)
  bytes, strongest first, with a "N more not shown" line, because a
  listed SSID of 32 `"` is 417 bytes of `<option>` through `HtmlStr`
  (twice per network) and five of them would pass the buffer; the rest
  of the page at its worst is 2006, and `provision_page_budget_is_covered`
  locks the sum under `PAGE_MAX`, with 62 bytes to spare.
- `http::RX_BUF` (4096) is the TCP receive window; 1 KB made an upload
  one segment per round trip. `REQUEST_BUF` (1536) still bounds the
  head; an upload's body stays on the socket.
- An OTA image must fit a slot (`partitions.csv`, 0x1E0000 = 1920 KB);
  the release image is about 650 KB. Static RAM: `ota.rs` holds 11 KB
  of buffers, and what the statics leave of DRAM is the main stack,
  about 96 KB now (`Settings` is 171 bytes of record and sits in several
  task futures); check `.stack` in `llvm-readelf -S` after adding one.
- `form::apply` decodes each key into 16 bytes and each value into 64
  (`form::VALUE_MAX`, the password); longer input is reported as a bad
  key or value before parsing. The record is `settings::RECORD_LEN`
  (171) bytes and the map scratch in `storage.rs` 256, which must hold
  the item (record plus 9 bytes, padded to a word).
- `settings::RECEIVERS` is the number of `Watch` receivers (display,
  persist, hostname). Add one when a new task subscribes, or `receiver()`
  panics at boot.
- `StackResources<10>` in `main.rs` covers DHCP, DNS, SNTP, the gateway
  probe and the HTTP listeners with headroom (on the access point: the
  DHCP and DNS servers and the listeners); adding sockets means raising it.
- `HOSTNAME_MAX` (24) stays under embassy-net's 32-byte DHCP option.
- `config::WIFI_RX_QUEUE_FRAMES` (32) is the driver's receive queue;
  esp-radio's default of 5 dropped bursts of LAN broadcasts, visible as
  `RX QUEUE FULL` with `esp_radio=debug`. The stack answers ping only
  because embassy-net's `auto-icmp-echo-reply` feature is on.

## Verifying without a board

`cargo host-test` runs the tests of `core/` on the host (75 on
2026-11-01): the AiP33628 encoder against a behavioural model of the
chip (shift on CLK rise, latch on DIN rise while CLK high), the panel
map, the palette and brightness tables, the hostname, password and
token rules, the record round trip and every older version, the form
parser and the page sizes against `PAGE_MAX`, the head parser, the
appended-hash split, the carry codec, the calendar arithmetic, the
melodies, the UI's alarm, grace and skip logic, the PIN and token
header rules, and the NTC and LDR tables against their equations. `cargo clippy -p esp32clock-core --target host-tuple
--tests` lints the tests. `.cargo/config.toml` aims every cargo
command at the chip, which is why the alias passes `--target
host-tuple`; the `[unstable] build-std` line there is ignored by
stable cargo. Not under test: `scan.rs`'s `build_cycle` (the M1 lit-time
check needs it on the host), `dhcpd.rs` and `dnsd.rs` (their pure halves
use embassy-net's address types), `storage.rs`'s map over the flash.
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
  round trip, `apply_sets_every_key`, the page sizes). A field that
  must not persist, like `skip_next`, is left out of the record on
  purpose. Read-only status goes in `form::Status`, built in
  `http::status`, so `form.rs` stays clockless.
- Tables copied from the reference docs (`BLOCKS`, `SEGMAP`, `COM_SEQ`,
  `IS_WIRE`) are verbatim; cite `3rdparty/HU-058_ESPHome/docs/` when
  changing anything near them.

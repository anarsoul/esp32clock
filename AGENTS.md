# esp32clock — guide for agents

Firmware for the HU-058 / HU-058D LED clock panel driven by an ESP32-C3,
written in Rust with esp-hal and Embassy on esp-rtos. The display's two
AiP33628 drivers are fed by SPI2 in quad mode used as a four-wire pattern
generator with DMA; see `PLAN.md` for the full design and its history.
`CLAUDE.md` is a symlink to this file; edit this one.

## Workflow

- Plan before implementing. `PLAN.md` is the design record: each milestone
  has its design, what was measured, and what was learned. Add to it before
  starting work and mark milestones verified when the user confirms on the
  panel. It ends with a review backlog of known, undone findings; read it
  before proposing work and strike items you finish.
- Nothing here can be verified without the hardware. Build, run clippy, and
  where logic allows, simulate on the host; then hand the user a flash
  command and say exactly what to look for. Do not claim hardware results.
- Commit only when asked. Commit messages end with
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` (the user asked
  for this trailer in this project).
- `3rdparty/` is the reverse-engineering reference (public domain), ignored
  by git. Read it; do not modify it.

## Build and flash

```
cargo build --release
cargo clippy --release
WIFI_SSID='…' WIFI_PASSWORD='…' TZ_OFFSET_MINUTES=-420 OTA_TOKEN='…' cargo run --release
MAX_CURRENT_STEP=8 cargo build --release   # optional: raise the drive-current ceiling (default 5)
OTA_TOKEN='…' scripts/ota.sh <ip>      # later updates, over the LAN
scripts/ota.sh                          # just the image file, for http://<ip>/update
```

`cargo run` flashes with espflash and opens the monitor; `.cargo/config.toml`
sets the runner and `ESP_LOG=info`. The runner passes `partitions.csv`
(two app slots, `ota_0` and `ota_1`, plus `otadata`) and erases
`otadata` so a USB flash always boots `ota_0`, where espflash put the
image. `scripts/ota.sh` builds, makes the application image with
`espflash save-image` and posts it to `/ota` with the token; the clock
writes the other slot, selects it and reboots. A build without
`OTA_TOKEN` (or with one under `config::OTA_TOKEN_MIN` characters) has
no script path: a `Bearer` upload is `403`, the page's PIN still works,
and the boot line says which paths are open. For a network debugging run set
`ESP_LOG='info,embassy_net=debug,esp_radio=debug'` on the command line
(it overrides the config, and esp-println compiles the filter in); the
extra lines are DHCP state and driver events, not packets. Every log
line carries the uptime in ms. `scripts/watch-clock.sh <ip>` sends a
full-size ping and fetches `/api` from the host every 30 s for the other
side of the story.

Credentials, the default timezone and the OTA token are read at build
time by `src/config.rs`; `build.rs` reruns the build when they change,
and exports `git describe --always --dirty` as `FIRMWARE_VERSION`, which
goes into the app descriptor, the boot line, the page and `/api`. The timezone, 12/24 hour, colour, brightness, alarm, alarm days,
alarm melody and DHCP hostname are settings: saved in the `nvs` partition,
editable from the buttons and from `http://<ip>/` (form) or `/api`
(form-encoded keys, JSON back). The page ends with a read-only status
line, and the JSON with the read-only keys `version`, `slot` (the app
slot the firmware runs from), `uptime` (seconds), `heals` (self-heal
resets since power-up) and `last_heal` (Unix seconds or null); posting
them is refused as an unknown key. `skip_next` (page checkbox, key `skip`) is
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

Opening `http://<ip>/update` shows a four-digit PIN for a minute.

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
power-up. The time survives a software reset: it is carried as an
offset from the RTC timer, which keeps running, so the panel shows the
time again before Wi-Fi is up. An alarm missed by up to
10 minutes across a power cut rings on the first valid time after boot;
after a software reset the alarm last rung-and-ended or skipped within
20 minutes is not rung again, while one cut off mid-ring comes back. A
self-heal due while the alarm rings waits for the ring to end, by a press
or its timeout, before blanking and resetting.

Firmware updates: `POST /ota` with the application image as the body
and an `Authorization` header: `Bearer <OTA_TOKEN>` from `scripts/ota.sh`,
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

| file | role |
| --- | --- |
| `src/aip33628.rs` | frame format, bit-reversed current table, quad-nibble waveform encoder; pure `core` |
| `src/panel.rs` | COM/SEG map, font, per-position colour, gamma and brightness tables, `Panel::render` → `ScanImage` |
| `src/scan.rs` | cycle/transfer builder, double-buffered DMA scan task, restart-gap statistics, `stop` on a blank frame before a deliberate reset or a flash write, `resume` after a failed one |
| `src/display.rs` | UI loop: tick, button or settings change → `ui` → draw → publish; rings the alarm; shows the IP; seeds and records the carried alarm state |
| `src/ui.rs` | mode state machine, pure logic; edits the settings it is handed; alarm due check, missed-alarm grace with `suppress_alarm` seed, consumes `skip_next` |
| `src/settings.rs` | `Settings`, palette and brightness tables, `Hostname`, shared `Watch` with change signal, versioned flash record |
| `src/storage.rs` | the one flash handle (`with_flash`), nvs partition lookup, `sequential-storage` map over it, debounced persist task that compares records, not structs |
| `src/heal.rs` | last-resort reset: waits out a ringing alarm, blanks the panel through `scan::stop`, counts itself and the time in the `carry` record, logs, reboots |
| `src/form.rs` | key=value parsing, all-or-nothing `apply` on a copy, JSON, the HTML page with its error notice and the read-only `Status` lines, the update page with its upload script; pure logic |
| `src/http.rs` | HTTP/1.0 server, `LISTENERS` sockets each in its own task: `GET`/`POST` on `/` and `/api`, `POST /play`, `GET /update`, `POST /ota` with the body streamed to `ota::install`; a refused form redirects to `/?error=…`; per-listener state and counters for the heartbeat |
| `src/buttons.rs` | debounced short/long press events, plus the two-button hold |
| `src/melody.rs` | alarm tunes as pitch and length data with tempo; pure |
| `src/carry.rs` | RTC-RAM record used only after a software reset (`CoreSw`/`Cpu0Sw`; anything else zeroes it at boot): self-heal count and time, the skip flag, the alarm last handled, the clock as an offset from the RTC timer, boots of an unconfirmed OTA image |
| `src/buzzer.rs` | LEDC PWM on an open-drain pin; plays a melody on command, looped or once, stops within a note |
| `src/clock.rs` | Unix time = uptime + NTP offset; local time with weekday; `carry_time`/`restore_carried` move the time across a software reset via the RTC timer, refreshed by the heartbeat |
| `src/ota.rs` | firmware upload: token check, slot lookup and the boot-time rollback, sector writer with read-back, activate, reboot; `VERSION` from `build.rs` |
| `src/net.rs` | Wi-Fi station with all-channel scan and transmit-power ceiling, minute RSSI line, connect failure budget that ends in `heal::reset`; DHCP switched on 500 ms after each connect and off at disconnect; link and address monitor, gateway ping probe with the self-heal trigger, SNTP, DHCP renew on hostname change |
| `src/config.rs` | build-time env vars and compile-time defaults |
| `src/bin/main.rs` | pin setup, SPI2 quad + DMA, executors, task spawning, panic reset hook, log timestamps, reset reason and carried time at boot, OTA rollback check, minute heartbeat log |

## Hardware facts that bite

- **Buzzer (GPIO5)** drives a PNP with its emitter at +5 V. Any driven level
  turns it on, including a 3.3 V high. Only a released open-drain line is
  silent. Never make it push-pull.
- **Display wires (GPIO6–9)** are SPI2 SIO0–3. The SPI builder resets pin
  pulls when it takes a pin, so the idle pull-downs are set afterwards via
  the `esp32c3` PAC in `main.rs`.
- **Buttons:** GPIO4 is the upper button, GPIO3 the lower. No board
  pull-ups; the internal ones are used.
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
  overflow is a 500, not a truncation. The worst-case settings page is
  2644 bytes with headers and the update page 2355; measure on the host
  after adding a field or label.
- `http::RX_BUF` (4096) is the TCP receive window; 1 KB made an upload
  one segment per round trip. `REQUEST_BUF` (1536) still bounds the
  head; an upload's body stays on the socket.
- An OTA image must fit a slot (`partitions.csv`, 0x1E0000 = 1920 KB);
  the release image is about 650 KB. Static RAM: `ota.rs` holds 11 KB
  of buffers, and what the statics leave of DRAM is the main stack,
  about 98 KB now; check `.stack` in `llvm-readelf -S` after adding one.
- `form::apply` decodes each key into 16 bytes and each value into 32;
  longer input is reported as a bad key or value before parsing.
- `settings::RECEIVERS` is the number of `Watch` receivers (display,
  persist, hostname). Add one when a new task subscribes, or `receiver()`
  panics at boot.
- `StackResources<10>` in `main.rs` covers DHCP, DNS, SNTP, the gateway
  probe and the HTTP listeners with headroom; adding sockets means raising it.
- `HOSTNAME_MAX` (24) stays under embassy-net's 32-byte DHCP option.
- `config::WIFI_RX_QUEUE_FRAMES` (32) is the driver's receive queue;
  esp-radio's default of 5 dropped bursts of LAN broadcasts, visible as
  `RX QUEUE FULL` with `esp_radio=debug`. The stack answers ping only
  because embassy-net's `auto-icmp-echo-reply` feature is on.

## Verifying without a board

The waveform encoder and panel map are pure code. A host harness that runs
the byte stream through a behavioural model of the AiP33628 (shift on CLK
rise, latch on DIN rise while CLK high) and checks per-sink lit time against
level × unit caught every encoding mistake so far. It lives outside the
repo; recreate it in a scratch crate by copying `aip33628.rs` and
`panel.rs` and reimplementing `build_cycle` with the current constants.
The same approach covers the other pure modules: a scratch crate with
`form.rs`, `melody.rs`, the record part of `settings.rs`, the head parser
from `http.rs` and `sequential-storage`'s mock flash has checked form
bodies, page size against `RESPONSE_BUF`, record round-trips and
migrations, melody lengths and the weekday arithmetic. `form.rs`,
`settings.rs`, `melody.rs`, `panel.rs`, `aip33628.rs` and `config.rs`
can be pulled in with `#[path]` as they are (embassy-sync builds on
the host); the private head parser, `ota.rs`'s token and sector
functions and `carry.rs`'s record codec are copied in. `PLAN.md` records
each check under its milestone.

## Conventions

- `no_std`, no `alloc` use outside what esp-radio needs; the heap exists
  for the Wi-Fi driver.
- The target is riscv32imc: atomics have load and store only, no
  `fetch_add` or `swap`. Counters shared with the heartbeat have one
  writer and are bumped with a load and a store; anything with more than
  one writer goes behind `critical_section`.
- Keep hardware out of `ui.rs`, `panel.rs`, `form.rs`, `melody.rs`,
  `aip33628.rs` and the record part of `settings.rs` so they stay testable.
- Settings change only through `settings::update`; it is what wakes the
  display and the persist task. The HTTP handlers apply a request to a
  copy and commit it with one `update` only if everything parsed; keep
  parsing out of the watch's closure.
- Adding a setting touches, in order: the `Settings` field and `DEFAULT`,
  `is_valid`, `RECORD_VERSION` with `to_record`/`from_record` still reading
  every older version, `form::apply_one`, `write_json` and `write_page`,
  the `PLAN.md` entry, and a page-size check. A field that must not
  persist, like `skip_next`, is left out of the record on purpose.
  Read-only status goes in `form::Status`, built in `http::status`, so
  `form.rs` stays clockless and the host check still builds.
- Tables copied from the reference docs (`BLOCKS`, `SEGMAP`, `COM_SEQ`,
  `IS_WIRE`) are verbatim; cite `3rdparty/HU-058_ESPHome/docs/` when
  changing anything near them.

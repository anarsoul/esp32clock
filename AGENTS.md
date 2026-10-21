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
WIFI_SSID='…' WIFI_PASSWORD='…' TZ_OFFSET_MINUTES=-420 cargo run --release
```

`cargo run` flashes with espflash and opens the monitor; `.cargo/config.toml`
sets the runner and `ESP_LOG=info`. Credentials and the default timezone are
read at build time by `src/config.rs`; `build.rs` reruns the build when they
change. The timezone, 12/24 hour, colour, brightness, alarm, alarm days,
alarm melody and DHCP hostname are settings: saved in the `nvs` partition,
editable from the buttons and from `http://<ip>/` (form) or `/api`
(form-encoded keys, JSON back). `skip_next` (page checkbox, key `skip`) is
the one setting the record does not store: it skips one alarm and is cleared
by it or by a reboot. `POST /play` previews the melody chosen in the form
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

Any press ends ringing or the IP display. Set mode times out after 15 s,
ringing after 120 s. An alarm missed by up to 10 minutes across a power
cut rings on the first valid time after boot.

## Layout

| file | role |
| --- | --- |
| `src/aip33628.rs` | frame format, bit-reversed current table, quad-nibble waveform encoder; pure `core` |
| `src/panel.rs` | COM/SEG map, font, per-position colour, gamma and brightness tables, `Panel::render` → `ScanImage` |
| `src/scan.rs` | cycle/transfer builder, double-buffered DMA scan task, restart-gap statistics |
| `src/display.rs` | UI loop: tick, button or settings change → `ui` → draw → publish; rings the alarm; shows the IP |
| `src/ui.rs` | mode state machine, pure logic; edits the settings it is handed; alarm due check, missed-alarm grace, consumes `skip_next` |
| `src/settings.rs` | `Settings`, palette and brightness tables, `Hostname`, shared `Watch` with change signal, versioned flash record |
| `src/storage.rs` | nvs partition lookup, `sequential-storage` map over `esp-storage`, debounced persist task that compares records, not structs |
| `src/form.rs` | key=value parsing, all-or-nothing `apply` on a copy, JSON and the HTML page with its error notice; pure logic |
| `src/http.rs` | HTTP/1.0 server, `LISTENERS` sockets each in its own task: `GET`/`POST` on `/` and `/api`, `POST /play`; a refused form redirects to `/?error=…` |
| `src/buttons.rs` | debounced short/long press events, plus the two-button hold |
| `src/melody.rs` | alarm tunes as pitch and length data with tempo; pure |
| `src/buzzer.rs` | LEDC PWM on an open-drain pin; plays a melody on command, looped or once, stops within a note |
| `src/clock.rs` | Unix time = uptime + NTP offset; local time with weekday |
| `src/net.rs` | Wi-Fi station with all-channel scan and transmit-power ceiling, embassy-net, SNTP, DHCP renew on hostname change |
| `src/config.rs` | build-time env vars and compile-time defaults |
| `src/bin/main.rs` | pin setup, SPI2 quad + DMA, executors, task spawning, panic reset hook, minute heartbeat log |

## Hardware facts that bite

- **Buzzer (GPIO5)** drives a PNP with its emitter at +5 V. Any driven level
  turns it on, including a 3.3 V high. Only a released open-drain line is
  silent. Never make it push-pull.
- **Display wires (GPIO6–9)** are SPI2 SIO0–3. The SPI builder resets pin
  pulls when it takes a pin, so the idle pull-downs are set afterwards via
  the `esp32c3` PAC in `main.rs`.
- **Buttons:** GPIO4 is the upper button, GPIO3 the lower. No board
  pull-ups; the internal ones are used.
- **Current:** `IS` comes from brightness alone, capped at step 11
  (`MAX_CURRENT_STEP`). Never derive it from how much is lit, and never park
  the panel all-white at high current; the drivers get hot.
- **Wi-Fi power:** `WIFI_MAX_TX_POWER_QDBM` is 44 (11 dBm) because the
  enclosure costs a few dB and esp-radio's default did not reach the
  access point. The SuperMini antenna is unstable near 20 dBm; raise it in
  small steps and let the user confirm from inside the case.
- **Flash writes** run with interrupts off, so the scan stalls: a record
  write is invisible, a sector erase is a brief dark blink. That is why the
  persist task debounces and skips writes that would not change the record.

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

## Sizes that bite

- `http::RESPONSE_BUF` (2560) holds the whole page plus headers, and
  overflow is a 500, not a truncation. The page is within about 150 bytes
  of it; measure on the host after adding a field or label, or raise it.
- `form::apply` decodes each key into 16 bytes and each value into 32;
  longer input is reported as a bad key or value before parsing.
- `settings::RECEIVERS` is the number of `Watch` receivers (display,
  persist, hostname). Add one when a new task subscribes, or `receiver()`
  panics at boot.
- `StackResources<8>` in `main.rs` covers DNS, SNTP and the HTTP listeners
  with headroom; adding sockets means raising it.
- `HOSTNAME_MAX` (24) stays under embassy-net's 32-byte DHCP option.

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
migrations, melody lengths and the weekday arithmetic. `PLAN.md` records
each check under its milestone.

## Conventions

- `no_std`, no `alloc` use outside what esp-radio needs; the heap exists
  for the Wi-Fi driver.
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
- Tables copied from the reference docs (`BLOCKS`, `SEGMAP`, `COM_SEQ`,
  `IS_WIRE`) are verbatim; cite `3rdparty/HU-058_ESPHome/docs/` when
  changing anything near them.

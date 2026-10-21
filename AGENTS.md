# esp32clock — guide for agents

Firmware for the HU-058 / HU-058D LED clock panel driven by an ESP32-C3,
written in Rust with esp-hal and Embassy on esp-rtos. The display's two
AiP33628 drivers are fed by SPI2 in quad mode used as a four-wire pattern
generator with DMA; see `PLAN.md` for the full design and its history.

## Workflow

- Plan before implementing. `PLAN.md` is the design record: each milestone
  has its design, what was measured, and what was learned. Add to it before
  starting work and mark milestones verified when the user confirms on the
  panel.
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

`cargo run` flashes with espflash and opens the monitor. Credentials and the
default timezone are read at build time by `src/config.rs`; `build.rs` reruns
the build when they change. The timezone, 12/24 hour, colour, brightness,
alarm, alarm days, alarm melody and DHCP hostname are settings: saved in the `nvs` partition, editable
from the buttons and from `http://<ip>/` (form) or `/api` (form-encoded keys,
JSON back).
`rust-version` must stay at 1.95 or higher or cargo's MSRV-aware resolver
picks an esp-rom-sys that esp-storage cannot build with.

## Layout

| file | role |
| --- | --- |
| `src/aip33628.rs` | frame format, bit-reversed current table, quad-nibble waveform encoder; pure `core` |
| `src/panel.rs` | COM/SEG map, font, per-position colour, gamma and brightness tables, `Panel::render` → `ScanImage` |
| `src/scan.rs` | cycle/transfer builder, double-buffered DMA scan task, restart-gap statistics |
| `src/display.rs` | UI loop: tick, button or settings change → `ui` → draw → publish; rings the alarm; shows the IP |
| `src/ui.rs` | mode state machine, pure logic; edits the settings it is handed |
| `src/settings.rs` | `Settings`, palette and brightness tables, shared `Watch` with change signal, flash record format |
| `src/storage.rs` | nvs partition lookup, `sequential-storage` map over `esp-storage`, debounced persist task |
| `src/form.rs` | key=value parsing, JSON and the HTML page; pure logic |
| `src/http.rs` | one-socket HTTP/1.0 server: `GET`/`POST` on `/` and `/api` |
| `src/buttons.rs` | debounced short/long press events, plus the two-button hold |
| `src/melody.rs` | alarm tunes as pitch and length data with tempo; pure |
| `src/buzzer.rs` | LEDC PWM on an open-drain pin; plays a melody on command, looped or once, stops within a note |
| `src/clock.rs` | Unix time = uptime + NTP offset; local time |
| `src/net.rs` | Wi-Fi station, embassy-net, SNTP |
| `src/config.rs` | build-time env vars and compile-time defaults |
| `src/bin/main.rs` | pin setup, SPI2 quad + DMA, executors, task spawning, panic reset hook |

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

## Verifying without a board

The waveform encoder and panel map are pure code. A host harness that runs
the byte stream through a behavioural model of the AiP33628 (shift on CLK
rise, latch on DIN rise while CLK high) and checks per-sink lit time against
level × unit caught every encoding mistake so far. It lives outside the
repo; recreate it in a scratch crate by copying `aip33628.rs` and
`panel.rs` and reimplementing `build_cycle` with the current constants.

## Conventions

- `no_std`, no `alloc` use outside what esp-radio needs; the heap exists
  for the Wi-Fi driver.
- Keep hardware out of `ui.rs`, `panel.rs`, `form.rs`, `melody.rs` and the
  record part of `settings.rs` so they stay testable.
- Settings change only through `settings::update`; it is what wakes the
  display and the persist task. Bump the record version in `settings.rs`
  when the layout changes and keep reading the old one.
- Tables copied from the reference docs (`BLOCKS`, `SEGMAP`, `COM_SEQ`,
  `IS_WIRE`) are verbatim; cite `3rdparty/HU-058_ESPHome/docs/` when
  changing anything near them.

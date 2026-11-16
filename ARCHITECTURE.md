# Architecture

How the firmware is put together: the parts, who owns what, and how
they talk. `PLAN.md` is the journal of how it got here and what was
measured; `AGENTS.md` has the per-file table, the hardware facts and
the sizes to keep. This file names constants rather than repeating
their values.

## The shape of it

An ESP32-C3 replaces the 8051 of an HU-058 LED clock. It drives the
panel's two AiP33628 LED drivers, a buzzer, two buttons and two
analogue sensors, and it is on Wi-Fi for the time (SNTP), its own web
pages, Home Assistant (MQTT) and firmware updates.

The firmware is Rust, `no_std`, on esp-hal with Embassy tasks under
esp-rtos. There is no heap use outside the Wi-Fi driver. Three ideas
shape everything else:

1. **Hardware plays the time-critical streams, not the CPU.** The
   display scan and the sampled sound are DMA rings that a peripheral
   loops over; tasks only swap or refill what the ring points at.
2. **Logic is pure and tested on the host.** Whatever can be written
   without a peripheral lives in a separate crate and runs under
   `cargo host-test`.
3. **One shared settings value.** Buttons, pages, `/api` and MQTT all
   edit the same `Settings`; a change wakes whoever depends on it.

## Two crates

| crate | what it holds |
| --- | --- |
| `core/` (`esp32clock-core`) | hardware-independent logic: the driver's wire format, panel map and font, UI state machine, settings and their flash record, form parser and HTML pages, HTTP head parser, auth rules, timezone rules, MQTT topics and discovery payload, the sound players (PCM, beeper, XM, SID, AY/PT3), sensor tables. Builds for the host; each module ends with its tests |
| root (`esp32clock`) | the tasks and drivers around it: `src/*.rs` and `src/bin/main.rs`. Re-exports `core`'s modules from `src/lib.rs`, so both halves are `crate::…` |

A module with both halves has the same name in each crate (`clock`,
`carry`, `http`, `ota`, `pack`, `buttons`, `clip`, `mqtt`, `sensors`): types
and rules in `core/src/`, the peripheral and the task in `src/`.
Build-time configuration (credentials, timezone, token, clips) is read
by `core/src/config.rs` and `core/build.rs`.

## Boot

`src/bin/main.rs`, in order:

1. Release the buzzer pin (open drain, high) before anything else.
2. Open the flash; map the `clips` partition, if the table has one,
   and put the clip pack it holds in the melody table (`pack::init`);
   load the settings record, fill what it lacks from
   the build's seeds, publish it (`settings::init`).
3. Read the reset reason and the RTC-RAM carry record (`carry::init`);
   check the OTA slot and roll back an image that never confirmed
   itself (`ota::init`); restore a carried time.
4. Choose the mode for this whole boot: **station**, or **access
   point** if no network is stored or the carry record asks for it.
   The driver does not do both.
5. Build the Wi-Fi controller and the embassy-net stack, start the two
   executors and spawn the tasks.
6. Stay as the minute heartbeat log.

## Executors and tasks

Two executors. The **interrupt executor** (priority 2) pre-empts
everything on the thread executor, Wi-Fi work included; the **thread
executor** runs the rest cooperatively.

| executor | task | owns | job |
| --- | --- | --- | --- |
| interrupt | `scan::scan_task` | SPI2, DMA channel 0, GPIO6–9 | starts the display ring; afterwards only stop, resume, halt |
| interrupt | `clip::clip_task` | I2S0, DMA channel 1, the buzzer pin while a clip plays | keeps the sound ring filled ahead of the DMA |
| thread | `display::display_task` | the UI state | tick, button or settings change → `ui` → render → publish; rings the alarm |
| thread | `buttons::button_task` | GPIO3, GPIO4 | debounce into press events |
| thread | `buzzer::buzzer_task` | LEDC, the buzzer pin | note melodies; hands clips to the clip task |
| thread | `sensors::sensor_task` | ADC1, GPIO0, GPIO1 | filtered NTC and LDR readings |
| thread | `storage::persist_task` | the `nvs` map | debounced save of changed settings |
| thread | `net::net_task` | the stack's runner | embassy-net |
| thread | `http::http_task` × `LISTENERS` | a socket each | pages, `/api`, commands, OTA upload |
| thread, station | `net::wifi_task` | the Wi-Fi controller | scan, connect, DHCP on and off, RSSI, the transmit power setting |
| thread, station | `net::ntp_task`, `hostname_task`, `monitor_task`, `probe_task` | sockets | time sync, DHCP hostname, link log, gateway ping and the self-heal trigger |
| thread, station | `mqtt::mqtt_task` | a socket | Home Assistant session |
| thread, access point | `net::ap_task`, `dhcpd::dhcpd_task`, `dnsd::dnsd_task` | the controller, sockets | the setup network, its leases, captive DNS |

Tasks talk through a small set of statics, each defined in the module
that owns the receiving side and wrapped in functions:

- `settings::WATCH` (a `Watch` with `RECEIVERS` subscribers: display,
  persist, hostname, MQTT). All writes go through `settings::update`.
- `Signal`s for commands: play/stop to the buzzer, start/stop/done to
  the clip task, stop/resume to the scan, silence and ringing edges at
  the display, goodbye to MQTT, rescan to the access point.
- One `Channel` of button events.
- Atomics for readings and counters (sensor millivolts, RSSI, probe
  and MQTT counts, the clip task's flag for a clip on the PDM
  converter, which the sensor task holds the thermometer by) with a single writer each; the target has no
  atomic read-modify-write, so anything with two writers is behind a
  critical section.

## Display path

```
Settings + time + readings
        │  core::ui      mode state machine → what to show
        ▼
   core::panel           font, COM/SEG map, colour → ScanImage (duty levels)
        ▼
   scan::build_cycle     one lap of AiP33628 frames (core::aip33628),
        │                binary-weighted sub-frames, ending in a blanking frame
        ▼
   ring::publish         build into the spare buffer, repoint the descriptors
        ▼
   SPI2 quad + DMA       loops over the descriptor ring by itself
        ▼
   GPIO6–9 → two AiP33628 → LEDs
```

SPI2 in quad mode is used as a four-wire pattern generator: its four
data lines are the two drivers' clock and data. The transfer is one
segment of the peripheral's DMA-controlled segmented mode on a
descriptor ring whose last descriptor points at the first, so the
hardware scans for ever with no interrupt and no CPU in the loop.
`ring.rs` is the one module that touches SPI2's registers.

The display task is the only publisher. A new image goes into the
buffer the ring is not reading, the descriptors are repointed, and the
old buffer is left alone for two laps. Brightness is the drivers'
current setting; colour is duty per LED.

The scan stops in three cases, always on the blanking frame: a
firmware upload (flash writes hold interrupts off), a deliberate reset
(`scan::halt`, latched), and a panic, which cannot be graceful.

## Sound path

One pin, two sources. The buzzer's transistor is on at any driven
level, so the pin is open drain throughout and released means silent.

- **Note melodies**: `buzzer_task` plays pitch-and-length tables from
  `core::melody` as LEDC PWM.
- **Clips**: the files of the clip pack (`core::pack`), a partition
  of its own that `src/pack.rs` maps through the flash MMU so they are
  read in place, checked by a SHA-256 at boot; or, with no pack, files
  built into the image (`ALARM_CLIP`). The melody table is the note
  melodies and then the clips of whichever is in force
  (`melody::get`). A clip is one of five kinds told apart
  by their first bytes (`clip::Stream`): raw PCM, a ZX Spectrum beeper
  bitstream, a FastTracker module (`xm`), a Commodore 64 register log
  played through a software SID (`sid`), a Pro Tracker 3 module played
  through a software AY-3-8910 (`pt3`, `ay`). The players are pure and
  produce samples; `shape::Shaper` is the high-pass and limiter they
  end in.

For a clip the buzzer task lends the pin to `clip_task`, which runs
I2S0 over a sixteen-slot DMA ring: by default 16-bit samples into the
chip's PCM-to-PDM converter, or raw line bits for a beeper clip and
the software modulators. The task fills chunks ahead of the DMA, paced
by elapsed time (`clip::Pacer`), which is why it sits on the interrupt
executor.

## Network services

- **HTTP** (`src/http.rs` over `core::http` and `core::form`): an
  HTTP/1.0 server, one request per connection, a fixed response buffer
  per listener that holds a whole page. Five pages: `/`, `/settings`
  and `/ha`, each a partial-update form, `/clips` and `/system`, and `/api` with the
  same keys as form-encoded in, JSON out. `form::apply` works on a copy
  of the settings and commits all or nothing. Every request passes
  `http::admit` before a route is looked at: a `Host` header,
  if sent, must be the clock's address or its own name, and an
  `Origin` on a `POST` must be one of those, so another site's page cannot
  drive the clock through a browser on the LAN (cross-site posts, DNS
  rebinding). Responses forbid framing.
- **Auth** (`core::auth`): script access by a `Bearer` API token;
  browser access to the guarded actions by a four-digit PIN shown on
  the panel, one PIN per action, issued by a `POST` (the system page's
  script asks when an action's button is pressed), never by a `GET`. Ordinary settings need neither on the
  LAN; network credentials, the broker over `/api`, OTA, reboot and
  the token itself do.
- **MQTT** (`src/mqtt.rs` over `core::mqtt`): Home Assistant discovery
  for one device, a retained state topic carrying the `/api` JSON, and
  `set/<key>` and `cmd` topics restricted to an allow list. Reconnects
  with back-off; says goodbye before a deliberate reset.
- **Clip pack upload** (`src/pack.rs`): `POST /clips`, streamed into
  the `clips` partition with the OTA's sector writer and hash check;
  the pack leaves the melody table first, what was written is parsed
  in place, and the clock reboots into it.
- **OTA** (`src/ota.rs`): the image is streamed from the socket into
  the other app slot a sector at a time with read-back and a SHA-256
  check, then `otadata` is switched. A new image must answer a gateway
  probe or an HTTP request within `OTA_BOOT_LIMIT` boots or the boot
  code switches back.
- **Access-point mode**: the same HTTP server with the setup form at
  `/`, a small DHCP server and a captive DNS that answers every name
  with the clock, so a phone opens the form by itself.

## Time

The clock is `uptime + offset` (`src/clock.rs`), the offset set by
SNTP. Local time comes from a POSIX TZ rule in the settings, evaluated
on the chip (`core::tz`), so daylight saving needs nothing from the
network. All alarm decisions are in `core::ui` and take the time as an
argument.

## Persistent state

| where | what | survives |
| --- | --- | --- |
| `nvs` partition | the settings record (`core::settings`): versioned, append-only, so an older image reads a newer record by its prefix | everything |
| RTC RAM | the carry record (`carry`): Unix time, alarm bookkeeping, the skip flag, self-heal count, OTA boot count, the access-point flag | software resets only |
| `otadata` | which app slot boots | everything |
| `clips` partition | the clip pack (`core::pack`); absent on a clock flashed before it existed, which the firmware allows for | everything but a web install, which writes the default pack |

All flash access goes through `storage::with_flash`, one blocking
operation at a time; there is exactly one flash handle.

## Resets

A reset is a normal event here, not a failure path. Every deliberate
one goes through `src/heal.rs`, which says the MQTT goodbye, halts the
scan on a dark frame and writes the time to the carry record first:

- `heal::reboot`: `/reload`, after an OTA.
- `heal::reset`: the self-heal, when the gateway stops answering or
  connects keep failing; waits out a ringing alarm.
- `heal::switch_mode`: between station and access point; starts the
  carry record over.

After a software reset the panel shows the time again before Wi-Fi is
up. A panic resets through `custom_halt` in `main.rs` without the
graceful steps; the carry record's per-second time write covers it.

## Where to change things

- A new setting: the checklist under "Conventions" in `AGENTS.md`.
- New logic: in `core/`, with tests; the firmware half only wires it
  to a peripheral or a socket.
- Anything near the scan or the buzzer pin: read "Hardware facts that
  bite" and "Timing rules in the scan path" in `AGENTS.md` first.

# onebitdawn — guide for agents

Firmware for the HU-058 / HU-058D LED clock panel driven by an ESP32-C3,
written in Rust with esp-hal and Embassy on esp-rtos. The display's two
AiP33628 drivers are fed by SPI2 in quad mode used as a four-wire pattern
generator, on one DMA segmented transfer the hardware loops for ever;
see `HISTORY.md` for the full design and its history; `PLAN.md` is
what is still to do.
`ARCHITECTURE.md` is the brief overview (crates, tasks, data paths,
resets).
`README.md` is short on purpose (the user, 2026-11-16): what the
project is, how it differs from the original, how to build the clock,
what the clips are. Everything else a user needs (building from
source, updates, clip packs, the buttons, the pages, `/api`, Home
Assistant) is `MANUAL.md`; document a user-visible change there.
The repo is a three-member workspace: the firmware is the root package,
`core/` (`onebitdawn-core`) is the hardware-independent half, a
`no_std` library the firmware re-exports and the host runs the tests
of, and `tools/pack/` (`onebitdawn-pack`) is the pack maker, for the
host and the browser, over `core`. `CLAUDE.md` is a symlink to this file; edit this one.

## Workflow

- Plan before implementing. `PLAN.md` is what is not done: the work
  planned, the checks a built change still owes, and the review backlog
  of known, undone findings. Give a piece of work its section there
  before starting it, with its design, then what was measured and what
  was learned; read the backlog before proposing work and strike items
  you finish. `HISTORY.md` is the design record of every milestone so
  far. When a section's work is built and the user has confirmed it on
  the panel, mark it verified and move it to the end of `HISTORY.md`
  as it stands; a section built and moved before its check leaves a
  line under `PLAN.md`'s "Checks still owed". Nothing in `HISTORY.md`
  is rewritten: a later finding is a new section or a dated note.
- Update `ARCHITECTURE.md` in the same change as any major change to
  the architecture: a task or executor added, removed or moved, a new
  peripheral or DMA path, a new way tasks talk to each other, a new
  network service or page, a new persistent store or reset path, a
  module moved between `core/` and `src/`. It stays brief and names
  constants rather than their values; tuning, measurements and
  history go in the work's section (`PLAN.md`, then `HISTORY.md`), not
  there.
- Nothing here can be verified without the hardware. Build, run clippy
  and `cargo host-test`, and put new pure logic under test in `core/`;
  then hand the user a flash command and say exactly what to look for.
  Do not claim hardware results.
- Commit only when asked. Commit messages end with
  `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>` (the user asked
  for this trailer in this project).
- `3rdparty/` is the reverse-engineering reference (public domain), ignored
  by git. Read it; do not modify it.
- `clips/` holds the clips of the default clip pack
  (`scripts/clips.sh`, the CI and the release make it): Trantor's title
  music, two of Dualtrax's modules (written for ORiON's key
  generators, hence the file names) and n1k-o's "synchronization", a
  PT3 module that is here with his permission (the user asked him,
  2026-11-16); the modules' sources are in `mods/`. They
  are other people's music, not under the project's licence;
  `clips/README.md` credits each and must go on doing so.
  "Suspended Seconds" is the user's own piano tune and not under the
  project's licence either: his copyright, not to be distributed
  apart from onebitdawn's clip pack. The other
  three are ours, Grieg's Morning (twice) and In the Hall of the
  Mountain King as modules `scripts/mkxm.py` writes from `mods/src/`,
  with CC0 recordings in them (`agents/clips.md`). A clip
  nobody may redistribute stays in the ignored `wav/`, `tap/`, `sid/`, `pt3/` and `xm/`,
  and a pack made of such in the ignored `packs/`.
- The project is licensed MIT OR Apache-2.0 (`LICENSE-MIT`,
  `LICENSE-APACHE`, the `license` field of both manifests). Every `.rs`
  and `.sh` file starts with an `SPDX-License-Identifier` line and the
  copyright line; give a new file both. The reference in `3rdparty/` is CC0 and needs nothing.

## Reference files

This file is the rules. What the firmware does, what each module
holds and what was measured are in `agents/`, read when the work
touches them:

| before touching | read |
| --- | --- |
| a page, a form, `/api`, the token or the PIN, provisioning, self-heal, MQTT, the upload, a release | `agents/behaviour.md` |
| a module you do not know, or to see what the host tests cover | `agents/modules.md` |
| a clip converter, a pack, the sound path's build variables | `agents/clips.md` |
| the buzzer pin, a DMA ring, the sensors, the current, Wi-Fi power, the flash | `agents/hardware.md` |
| a buffer, a page's size, the record, a partition, a new static | `agents/sizes.md` |

Keep them current in the same change as the code. A date, a
measurement or the story of how something came to be goes in the
work's section (`PLAN.md`, then `HISTORY.md`) or one of those files,
never here; this file gets a line only when
there is a new rule.

## Build and flash

```
cargo build --release
cargo clippy --release
cargo host-test                          # the core crate's tests on the host; plain `cargo test` aims at the chip
cargo clippy -p onebitdawn-core --target host-tuple --tests
cargo pack-test                          # the pack maker's tests (tools/pack)
cargo clippy -p onebitdawn-pack --target host-tuple --all-targets
cargo pack -o packs/mine.bin tune.xm ring.wav   # a clip pack of your files; --help for the options
cargo pack-wasm                          # the pack maker for its page, docs/pack/ (agents/clips.md)
node scripts/tz-check.mjs                # src/tz.js against zones with known rules
WIFI_SSID='…' WIFI_PASSWORD='…' TZ_RULE='<-07>7' API_TOKEN='…' cargo run --release   # flash over USB, open the monitor
API_TOKEN='…' scripts/ota.sh <ip>        # update over the LAN; with no arguments just the image file
API_TOKEN='…' scripts/reload.sh <ip>     # software reset with the carry record
scripts/pin.sh <ip> <serial-log>         # POST /pin, the digits from the monitor's log
scripts/verify-csrf.sh <ip>              # a foreign Origin or Host is refused
API_TOKEN='…' scripts/clips.sh <ip> [pack]   # write a clip pack; with no arguments make packs/clips-default.bin
```

The build-time variables, all optional, are read by
`core/src/config.rs` (`core/build.rs` reruns the crate when one
changes): `WIFI_SSID`, `WIFI_PASSWORD`, `TZ_RULE` (or the older
`TZ_OFFSET_MINUTES`), `API_TOKEN`, `AP_PASSWORD`, `MAX_CURRENT_STEP`,
`WIFI_TX_POWER_QDBM`, `MQTT_HOST`, `MQTT_PORT`, `MQTT_USER`,
`MQTT_PASSWORD`, and for the sound path `ALARM_CLIP`,
`ALARM_CLIP_LABEL`, `CLIP_MODULATOR`, `CLIP_KBPS`, `CLIP_CARRIER_KHZ`
and `CLIP_PDM_BITS` (`agents/clips.md`). The network, the token, the
timezone and the broker are seeds: a stored record wins over a later
build's constants. `ESP_LOG` on the command line overrides the
config's `info`. The `bench-hang` feature (never in a release) adds
`POST /hang/<kind>` and reads `BENCH_HANG=boot` in `src/watchdog.rs`,
for trying the watchdog on the bench (`agents/modules.md`).

`rust-version` must stay at 1.95 or higher or cargo's MSRV-aware
resolver picks an esp-rom-sys that esp-storage cannot build with.

A release is `CHANGELOG.md`'s section first, then an annotated tag
`v*` (`git tag -a`; a lightweight tag does not reach the version);
the rest is `agents/behaviour.md`, "Releases".

## Controls

| press | in clock mode |
| --- | --- |
| top short | next colour |
| bottom short | next brightness |
| top long | set alarm hour, then bottom short for minutes, bottom short again to leave |
| bottom long | arm or disarm the alarm (degree mark shows armed) |
| both long | show the IP, one octet at a time, twice |
| both held 10 s | reset into the access point; from the access point, back to the station if a network is stored |

Any press ends ringing or the IP display. The pages are `/`,
`/settings`, `/ha`, `/clips` and `/system`; on the access point `/` is
the setup form.

## Behaviour rules

The description is `agents/behaviour.md`; these are the rules a change
must keep.

- No `GET` has a side effect (an `<img>` on any site sends one). A
  route that changes anything is a `POST`, and every request goes
  through `core::http::admit` before a route is looked at.
- A password or the token is never read back: the JSON has
  `password_set`, `token_set`, `mqtt_password_set`.
- `ssid` and `password` are taken on `/api` only with the `Bearer`
  token (or on the access point), refused on the key alone, never on
  whether the value would change: that would be a password oracle.
  The `mqtt_*` keys need the token on `/api`; the form takes them, but
  a broker moved to another host without the password typed again is
  refused (`FormError::BrokerPassword`). A key only the token may set
  goes behind `form::Allow`.
- A form is a partial update, as `/api` is: only the keys sent change.
  A checkbox goes through `form::Check`.
- The pages say nothing of `/api`; that is `MANUAL.md`'s.
- One PIN is one action. Credentials are checked before a body is
  read, a body is parsed before the PIN is spent, and a refused upload
  reads the rest of the body before it answers.
- MQTT sets only `mqtt::SET_KEYS`; the bus is no way to move the clock
  to another network, broker or name.
- A deliberate reset goes through `heal` (`reboot`, `reset`,
  `switch_mode`): the MQTT goodbye, the panel blanked by `scan::halt`,
  the time carried.
- A station that cannot join never turns into an access point by
  itself.
- The RTC watchdog is fed by `watchdog_task` alone, on the thread
  executor, never from an interrupt or the interrupt executor: a feed
  must prove everything above it gave the CPU back. A task checks in
  (`watchdog::Party`) only if every wait in its loop is bounded well
  under `DEADLINE_MS`; the network's health is `heal`'s, not the
  watchdog's.
- A failed upload, firmware or pack, leaves what was running in use
  wherever it can; check before anything is erased.

## Layout

The pure modules live in `core/src/` and the firmware re-exports them
from `src/lib.rs`, so firmware code says `crate::settings`. Ten
modules are split, the pure half in `core/src/` and the hardware half
in `src/` under the same name: `buttons`, `clock`, `carry`, `http`,
`ota`, `pack`, `clip`, `mqtt`, `sensors`, `watchdog`. Each `core` module ends with
its `#[cfg(test)] mod tests`. The full account of each file is
`agents/modules.md`.

| file | role |
| --- | --- |
| `core/src/aip33628.rs` | the drivers' frame format and waveform encoder |
| `core/src/panel.rs` | COM/SEG map, font, colours as duty levels, `Panel::render` |
| `core/src/ui.rs` | the mode state machine, alarm due and grace, the faces |
| `core/src/settings.rs` | `Settings`, its tables and rules, the `Watch`, the flash record |
| `core/src/form.rs` | key=value parsing, `apply`, the JSON, the five pages and their scripts |
| `core/src/auth.rs` | the API token and the panel PIN |
| `core/src/http.rs` | the head parser, `admit`, `RESPONSE_BUF`, `PAGE_MAX` |
| `core/src/mqtt.rs` | topics, routes, `SET_KEYS`, the discovery payload |
| `core/src/melody.rs` | the note melodies and the clip table at run time |
| `core/src/pack.rs` | the clip pack's layout and checks |
| `core/src/clip.rs` | `Player`, `Beeper`, `Stream`, the ring's `Pacer` |
| `core/src/adpcm.rs`, `xm.rs`, `sid.rs`, `ay.rs`, `pt3.rs` | the clip kinds' decoders and players |
| `core/src/refs.rs`, `shape.rs` | references in a packed clip; the high-pass and limiter |
| `core/src/sensors.rs` | the NTC and LDR tables, the thermometer's `Hold` |
| `core/src/tz.rs`, `clock.rs` | POSIX TZ rules; calendar arithmetic |
| `core/src/carry.rs`, `ota.rs`, `buttons.rs` | the carry codec; image constants; event types |
| `core/src/config.rs`, `core/build.rs` | build-time variables and defaults; `clips.rs` from `ALARM_CLIP` |
| `src/bin/main.rs` | pins, executors, task spawning, the panic hook, the heartbeat |
| `src/scan.rs`, `src/ring.rs` | the scan task; the SPI2 segmented transfer on its descriptor ring |
| `src/display.rs` | the UI loop: tick, button or change, draw, publish; rings the alarm |
| `src/buzzer.rs`, `src/clip.rs` | LEDC tones; the clip task, I2S0 and its DMA ring, the pin |
| `src/http.rs` | the HTTP/1.0 server and every route |
| `src/net.rs`, `dhcpd.rs`, `dnsd.rs` | station and access point, the probe, SNTP; the access point's servers |
| `src/mqtt.rs` | the MQTT session task |
| `src/storage.rs` | the one flash handle, the settings map, the persist task |
| `src/heal.rs` | the deliberate resets |
| `core/src/watchdog.rs`, `src/watchdog.rs` | the deadlines, parties and the carry word; the RTC watchdog and its feeder task |
| `src/carry.rs`, `src/clock.rs` | the RTC-RAM record; the time and its carry |
| `src/ota.rs`, `src/pack.rs` | the firmware upload and rollback; the `clips` partition and its upload |
| `src/sensors.rs`, `src/buttons.rs` | the ADC task; the GPIO task |
| `src/tz.js` | the page's browser-zone button, served as `/tz.js` |
| `tools/pack/src/` | the pack maker: a converter a kind (`xmc`, `modxm`, `s3mxm`, `sidc`, `pt3c`, `wav`, `adpcm`), `pack` the writer, `main` the command, `web` and `wasm` the page's side |
| `docs/index.html`, `docs/pack/index.html` | the web installer; the pack maker's page |

## Hardware facts that bite

Each with its measurements in `agents/hardware.md`.

- **Buzzer (GPIO5)** drives a PNP with its emitter at +5 V. Any driven
  level turns it on, including a 3.3 V high; only a released
  open-drain line is silent. Never make it push-pull. A clip drives it
  from I2S0 through the GPIO matrix's inverter, as an open-drain
  `Output` turned into a signal; `clip.rs` takes the pin back as a
  plain `Output` when it ends, which clears the inverter, and a tone's
  LEDC must never be connected over an inverted pin. The firmware
  assumes neither of the user's buzzer mods.
- **Line output (GPIO20)** is the other pin a sound can go out of,
  for a clock with an amplifier: push-pull, low at rest, no inverter.
  The `output` setting chooses, read at each tone and each clip, as
  `AudioOut::tones()` and `AudioOut::clips()`: `split` is the notes on
  the buzzer and the clips here. Every
  pin configuration for sound goes through `buzzer::Pins`, so GPIO5 is
  never built push-pull by a path meant for GPIO20; a tone takes the
  last tone's pin back to its rest before LEDC goes to the other.
- **Amplifier shutdown (GPIO10)** is high only from before a sound on
  the line output to after it; the buzzer task brackets a melody with
  it and `Pins` is its only way. Never driven while the output is
  `buzzer`, and low at boot before GPIO20 is driven.
- **The GDMA's `out_dscr` is not the descriptor being read**; it names
  the one two further on. Pace a DMA ring by time and use the register
  as a check at most.
- **I2S0 runs at one sample width**, 16 bits, for the converter and
  for a stream of bytes alike. After a run at 8 bits the transmitter
  now and then takes one byte a sample for every 16-bit clip that
  follows, at half the rate, whatever its registers say.
- **Display wires (GPIO6–9)** are SPI2 SIO0–3. The SPI builder resets
  the pull of a bare pin but leaves a `Flex` alone, so `main.rs` sets
  the idle pull-downs on `Flex` pins. No PAC access; `ring.rs`,
  `clip.rs` and `pack.rs` are the modules that touch registers,
  through esp-hal's `regs()`.
- **Buttons:** GPIO4 is the upper, GPIO3 the lower, on the internal
  pull-ups.
- **Sensors:** GPIO0 the NTC, GPIO1 the LDR, on ADC1 (the converter
  that works with Wi-Fi on) with `AdcCalCurve`. Only the NTC can
  report "no sensor"; a missing LDR looks dark. The board warms the
  NTC (`temp_offset`), and the buzzer's transistor heats it while a
  clip plays through the PDM converter, which is what `sensors::Hold`
  is for.
- **Current:** `IS` comes from brightness alone, capped at
  `MAX_CURRENT_STEP`. Never derive it from how much is lit, and never
  park the panel all-white at high current.
- **Wi-Fi power:** `tx_power` is the station's only, given to the
  driver before every connect and every 10 s; the access point always
  transmits at `config::AP_TX_POWER_QDBM`. Not an MQTT `set/` key. The
  uplink is the side that fails: measure a change with the access
  point's figure for the clock, from inside the case, not with the
  clock's RSSI.
- **First frame after connect:** DHCP is enabled `LINK_SETTLE` after
  the connect, and `net::poke` must follow any `set_config_v4`.
- **Flash writes** run with interrupts off, so the scan stalls. The
  persist task debounces and skips a write that would not change the
  record; an upload stops the scan first.
- **One flash handle.** `storage::with_flash` is the only way to the
  flash, one blocking operation at a time, never across an await. A
  second `FlashStorage` does not work.

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
  which no priority could fix; the history is in HISTORY.md under 2026-11-07
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
  path in `custom_halt` and the RTC watchdog's own reset cannot do
  this and are the exceptions.
- The heartbeat's `scan` field is images published, stops (and stops that
  had to cancel the transfer, which the bench never showed), running or
  halted, and the index of the descriptor the DMA is reading (0 the conf
  descriptor, 1 to 11 the data), which must differ between heartbeats.

## Sizes that bite

Each with its figures in `agents/sizes.md`.

- A page and its headers must fit `http::RESPONSE_BUF` (one per
  listener); overflow is a 500, not a truncation. The `form` tests
  build each page at its worst case against `http::PAGE_MAX`: run
  `cargo host-test` after adding a field or a label (`-- --nocapture`
  prints the sizes). The setup page's network list stops at
  `form::NETWORK_LIST_BUDGET`. `src/tz.js` is held under the buffer by
  a `const` assert.
- `http::REQUEST_BUF` bounds a request's head; a body stays on the
  socket. A form's body must fit `http::BODY_MAX`, which the `form`
  tests measure each form against: run them after adding a field.
  `form::apply` decodes a key into 16 bytes and a value into
  `form::VALUE_MAX`.
- An OTA image must fit a slot and a pack the `clips` partition
  (`partitions.csv`).
- What the statics leave of DRAM is the main stack. After adding a
  static, or a large local to a task (`Settings` sits in several
  futures), check `.stack` in `llvm-readelf -S`, against a build of
  `HEAD` when in doubt. One `Response` per listener loop.
- The settings map's scratch in `storage.rs` must hold the item, and
  no item may pass `storage::ITEM_MAX` (see "Compatibility").
- `settings::RECEIVERS` is the number of `Watch` receivers; add one
  when a new task subscribes, or `receiver()` panics at boot.
- `StackResources<11>` in `main.rs`: adding a socket means raising it.
- `HOSTNAME_MAX` stays under embassy-net's 32-byte DHCP option.
- `config::WIFI_MTU` bounds every frame the clock sends and the probe
  is sized from it; a ping larger than the probe gets no reply.
- The discovery payload must fit `mqtt::DISCOVERY_MAX`; the `mqtt`
  test prints the worst case. The JSON of `/api`, which is also the
  MQTT state, must fit `form::JSON_MAX`; the `form` test builds the
  worst case, so run it after adding a key.
- The clip ring is ahead of the DMA by a fraction of a second, which
  is why the clip task is on the interrupt executor; keep it there.

## Verifying without a board

`cargo host-test` runs the tests of `core/` on the host, and
`cargo clippy -p onebitdawn-core --target host-tuple --tests` lints
them; `node scripts/tz-check.mjs` checks `src/tz.js`;
`cargo pack-test` holds the pack maker to the clips there are, byte
for byte, and runs it over whatever is in the ignored music folders. The players'
tests over real clips run only with `ALARM_CLIP` naming some, which
the CI does in a second run. Not under test: `scan.rs`'s
`build_cycle`, `ring.rs` (its layout is `const`-asserted),
`src/clip.rs`, `dhcpd.rs` and `dnsd.rs`,
`storage.rs`'s map over the flash, `mqtt.rs`'s session loop. What is
covered, and the dump commands for comparing a player with a
reference, are in `agents/modules.md`. `HISTORY.md` records the checks of
each milestone; new ones are tests.

## Compatibility

The first public release (the one after v0.4) fixes what every later
firmware must go on doing, since a clock is updated over the air and
may be rolled back (by hand, or by the third unconfirmed boot) to any
release from that one on, with the newer one's data in flash:

- **The partition table and the bootloader** are what the clock was
  flashed with over USB; an update never changes them. A later image
  fits `ota_0` (0x160000) and a pack `clips`.
- **The settings record** is append-only, and an older image reads a
  newer record by its prefix: a new field goes at the end with a new
  `RECORD_VERSION`. `Settings::from_record` gives up only on a record
  shorter than its version says; a value it does not know in a field
  it has (a new enum value, a level, a rule its parser refuses) is
  that field's default, with the rest kept, the network above all.
  Widening a field's values is therefore allowed, and costs a
  rolled-back clock that setting.
- **No item in the settings map is longer than `storage::ITEM_MAX`**
  (1000 bytes, key included; the record is 408). `sequential-storage`
  reads every item it passes into the scratch, so one item too long
  for an older image fails its read of all of them: it then runs on
  the defaults and erases the map at its first save (seen on
  test-clock, 2026-11-17). A record that outgrows the limit goes on
  under a second key. A new major version of `sequential-storage`
  must read the map as 8.0 wrote it.
- **The carry record** is append-only from layout 4: a later layout
  adds words after the nine and bumps the low byte of `MARK` (layout 5
  added the watchdog's word, read only under a mark that has it);
  `carry::readable` takes this layout or any later one and reads the
  words it knows, `ota_boots` among them, so an update or a rollback
  across a layout change still counts its boots. An image writes the
  mark back with its own version.
- **A clip pack** of version 1 and every clip format in it today are
  read by every later firmware. A firmware refuses a pack of another
  version or with a reserved byte or flag bit set, and a known kind's
  clip of a version it does not play; a clip of a *new* kind it would
  play as PCM with no header, so a pack that holds one must set a
  reserved bit or the version (`core/src/pack.rs`'s header).
- **`/api`** keys, the `set/` keys and the MQTT topics, the device id
  and the entities' unique ids stay; additions only.
- **The prebuilt images have no API token** (since the same release:
  a token printed in the manual would be every clock's, and `seed`
  stores it at the first save, for good). `API_TOKEN` stays empty in
  `ci.yml` and `release.yml`.

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
  older version, `form::apply_one`, `write_json` and the page it belongs on (`write_page`, `write_settings_page` or `write_ha_page`; a checkbox through `form::Check`),
  the `PLAN.md` entry, and the `settings` and `form` tests (the record
  round trip, `apply_sets_every_key`, `each_page_has_its_own_fields`, the page sizes). A setting Home
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

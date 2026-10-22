# ESP32-C3 clock for the HU-058 panel — implementation plan

Target: show a static time (`12:34`) on the HU-058/HU-058D LED panel from an
ESP32-C3, written in Rust with esp-hal + Embassy, with the display scan
offloaded to the SPI2 peripheral in quad mode plus DMA.

Reference material: `3rdparty/HU-058_ESPHome/docs/` (protocol, display map)
and `3rdparty/HU-058_ESPHome/firmware/` (bit-bang reference, ESPHome component).

## 1. What the panel needs (from the reference)

- Two AiP33628 drivers, one per two-wire bus (CLK/DATA, CLK_1/DATA_1).
  Driver 1 = hour digits, driver 2 = minute digits.
- Each driver is a 30-bit shift register + latch, no strobe, no command set.
  Frame, LSB first: `SS[15:0]` (SEG sinks, 1 = on), `CS[7:0]` (COM anodes,
  1 = on), `IS[3:0]` (current, **bit-reversed on the wire**), 2 reserved zeros.
- Shift on CLK rising edge. Data must only change while CLK is low.
- Latch = DATA rising edge while CLK is held high after bit 29. Then DATA low,
  then CLK low. Reference sequence (`panel-test/src/main.cpp:sendFrame`):
  bits 0..29 clocked, CLK left high after bit 29, then DATA 0 → 1 → 0, CLK 0.
- Multiplex scan is done by the MCU: four COM pairs (`0x30, 0x0C, 0x03, 0xC0`),
  ~600 µs each, ~417 Hz refresh. CS and IS are identical for both drivers in a
  slot; only SS differs.
- Display map: per block (digit) `com_lo`/`com_hi`, LEDn = SEG(3n-2..3n),
  channel = SEG index mod 3 (1 blue, 2 green, 0 red). Segment table and
  BLOCKS table copied verbatim from `display-map.md` / `aip33628.cpp`.
- Current: `IS` from brightness only, never from lit count. Keep ≤ step ~11
  for normal use; the board is rated 250 mA.
- After power-up, ignore the first 200 µs (we wait ~10 ms anyway).

## 2. Hardware offload design: SPI2 quad mode as a 4-channel pattern generator

The two buses have four signals total. Rather than use SPI's own SCK (which
cannot produce the "DATA edge while CLK is high" latch), we drive **all four
lines as quad-SPI data lines** and never connect SCK to anything. Each SPI
clock period outputs one nibble, and each nibble is the complete state of the
four wires:

| nibble bit | SPI signal | GPIO | panel net          |
| ---------- | ---------- | ---- | ------------------ |
| 0          | SIO0       | 9    | DATA   (driver 1)  |
| 1          | SIO1       | 8    | CLK    (driver 1)  |
| 2          | SIO2       | 7    | DATA_1 (driver 2)  |
| 3          | SIO3       | 6    | CLK_1  (driver 2)  |

(Pin ↔ signal routing is free via the GPIO matrix. The nibble-bit ↔ SIOn
mapping and high-nibble-first byte order were confirmed on hardware on
2026-10-21: the panel shows `12:34` with this table as written.)

Consequences:

- A full frame to **both drivers at once** is a fixed 47-byte byte string.
- The dwell between frames is just zero padding (all lines low = hold).
- One DMA transfer of 2447 bytes carries an entire 2400 µs scan cycle. The
  CPU restarts the transfer when it ends; the latch holds across the gap.
- Timing is cycle-exact from the SPI clock, immune to task/ISR jitter, which
  is exactly what the later sub-frame colour dithering needs (the ESPHome
  component had to move to a gptimer ISR for that; here it is free).

### Frame encoding (per 30-bit frame, both drivers)

SPI clock 2 MHz → 500 ns per nibble. Per bit `i` with data bits `d1`, `d2`:

1. `[d1, CLK=0, d2, CLK_1=0]` — set data while clocks low
2. `[d1, CLK=1, d2, CLK_1=1]` — rising edge shifts the bit
3. `[d1, CLK=0, d2, CLK_1=0]` — falling edge with data still stable

Three nibbles per bit, not two, so DATA never changes in the same nibble as a
CLK falling edge (a DATA rise a few ns before CLK falls would be read as a
latch). Bit 29 skips step 3 so CLK stays high, then the latch tail:

4. `[0,1,0,1]` DATA low, CLK high
5. `[1,1,1,1]` DATA rising edge while CLK high → **latch**
6. `[0,1,0,1]` DATA low
7. `[0,0,0,0]` CLK low, idle

Total 93 nibbles = 47 bytes = 47 µs. Each nibble phase is
500 ns, far above the chip's 16 ns minimum and slower than the reference's
~100 ns bit-bang, leaving margin for the 3.3 V-into-5 V-logic drive.

### Scan cycle buffer

```
slot k (k = 0..3), 600 bytes each = 600 µs:
  [47-byte frame: SS1[k], SS2[k], CS = COM_SEQ[k], IS]  [553 bytes of 0x00]
blanking frame, 47 bytes:
  [47-byte frame: SS1 = 0, SS2 = 0, CS = COM_SEQ[3], IS]
cycle = 4 slots + blank = 2447 bytes, ~409 Hz
```

`COM_SEQ = [0x30, 0x0C, 0x03, 0xC0]` (stock order). Both drivers get the same
CS/IS. Fits one `DmaTxBuf` (one descriptor; CHUNK_SIZE is 4092).

**Why the blanking frame.** The AiP33628 latch holds the last pattern
indefinitely; the chip has no blanking of its own. Without the extra frame the
fourth COM pair would stay lit through the gap between one DMA transfer ending
and the next starting, so that row of segments would be brighter than the
other three, and would pulse with gap jitter (small under Embassy alone,
worse once Wi-Fi runs). With `SS = 0` latched last, every lit slot runs
latch-to-latch for exactly 600 µs and the restart gap is dark. Gap jitter
then only wobbles the refresh period by tens of microseconds, and the
brightness loss is gap / cycle, well under one percent and uniform. The same
rule carries into M3: the last thing latched before any gap must be dark.

### Idle-line safety

Between transfers the quad lines may be released by the peripheral. The frame
and padding end with all four lines low, so configure each of the four pins as
a `Flex` with `Pull::Down`, output-enabled and low, before handing them to
`with_sio0..3`. A weak pull-down keeps the CMOS inputs at a defined low either
way, so no spurious edges reach the drivers.

### esp-hal API (verified against esp-hal 1.2.2 source)

- `Spi::new(SPI2, Config::default().with_frequency(Rate::from_mhz(2)).with_mode(Mode::_0))`
  — `with_sck` is not called (defaults to `NoPin`), `with_cs` not called.
- `.with_sio0(..).with_sio1(..).with_sio2(..).with_sio3(..)` take
  `PeripheralInput + PeripheralOutput`; `Flex` satisfies both (`Output` does
  not), hence Flex above.
- `.with_dma(peripherals.DMA_CH0).into_async()` → `SpiDma<Async>`.
- `dma_tx_buffer!(2447)` → `DmaTxBuf`; fill via `as_mut_slice()`.
- `spi.half_duplex_write_buffer(DataMode::Quad, Command::None, Address::None, 0, 2447, buf)`
  → `SpiDmaTransfer`; `transfer.wait_for_done().await`; `transfer.wait()`
  returns `(spi, buf)` for the next cycle.
- All of the above are behind the `unstable` feature.

## 3. Project layout (Rust, project root = this directory)

Scaffold with `esp-generate` (`-c esp32c3 -o embassy -o unstable-hal
-o esp-backtrace -o log`), then adjust. The current template uses
`esp-rtos` as the Embassy runtime (`#[esp_rtos::main]`,
`esp_rtos::start(timg0.timer0, sw_interrupt0)`).

```
Cargo.toml               esp-hal (esp32c3, unstable), esp-rtos (embassy), embassy-executor,
                         embassy-time, embassy-sync, esp-println, esp-backtrace, log,
                         esp-bootloader-esp-idf, static_cell, critical-section
.cargo/config.toml       target riscv32imc-unknown-none-elf, runner = espflash flash --monitor
rust-toolchain.toml      stable, rust-src, riscv32imc target
build.rs                 -Tlinkall.x (from template)
src/lib.rs               no_std crate root: pub mod aip33628, panel, scan
src/aip33628.rs          protocol: Frame {ss, cs, is}, IS_WIRE table, IS_MA table,
                         encode_pair(f1_ss, f2_ss, cs, is, &mut [u8; 48]) nibble encoder
src/panel.rs             display map: BLOCKS, SEGMAP, COM_SEQ, 7-seg font,
                         Panel { on: [[bool; 9]; 4] } with set_digit/set_colon/set_am/...,
                         render() -> ScanImage { ss: [[u16; 4]; 2], is: u8 }
src/scan.rs              build_cycle(&ScanImage, &mut [u8; 2447]) incl. trailing blanking
                         frame; Embassy task that owns
                         the SpiDma + DmaTxBuf, loops DMA cycles, picks up new ScanImage
                         from a Signal between cycles
src/bin/main.rs          init, pin setup, spawn scan task, write "12:34" once
PLAN.md                  this file
3rdparty/                untouched reference (public domain)
```

Versions: template pins esp-hal ~1.1; bump to the current set (esp-hal 1.2.2,
esp-rtos 0.4.0, esp-bootloader-esp-idf 0.6.0, esp-println 0.18, esp-backtrace
0.20, embassy-executor 0.10, embassy-time 0.5) if they build together;
otherwise keep the template's pinned set. Updating `esp-generate` to 1.4.0
(`cargo install esp-generate`) gives the newer set directly.

## 4. Other pins (configured in milestone 1 so nothing misbehaves at boot)

| GPIO | function | config |
| ---- | -------- | ------ |
| 5    | buzzer (PNP base via 10 k, emitter at +5 V) | **open-drain output, released (high-Z) at boot**. A push-pull 3.3 V high still turns the PNP on. Low = buzzer on. |
| 4    | upper button (switch to GND, no board pull-up) | input, internal pull-up |
| 3    | lower button (switch to GND, no board pull-up) | input, internal pull-up |

Strapping notes for ESP32-C3: GPIO8 and GPIO9 are strapping pins (GPIO9 is the
BOOT button, GPIO8 must be high for download mode). Both are driven by us only
after boot and the AiP33628 inputs do not load them, so flashing is unaffected;
the display lines sit low/undriven during reset, which the drivers ignore.

## 5. Milestones

### M1 — static `12:34` — DONE, verified on the panel 2026-10-21

1. `git init`; scaffold with esp-generate into the project root; keep
   `3rdparty/` as-is; `.gitignore` gets `target/`.
2. `aip33628.rs`: frame struct, IS wire table, nibble encoder. Pure `core`
   code, no HAL dependency.
3. `panel.rs`: tables from `display-map.md`, 7-segment font (digits, `-`,
   blank), `Panel` state → `ScanImage` (SS word per driver per COM slot).
4. `scan.rs`: cycle builder (four lit slots plus the all-off blanking frame
   at the end, so the inter-transfer gap is dark) + scan task (SPI2 quad DMA
   loop, double-buffered via `Signal<CriticalSectionRawMutex, ScanImage>`
   consumed between cycles).
5. `main.rs`: clocks, pins (Flex + pull-downs for the 4 display lines,
   open-drain buzzer released, buttons pulled up), SPI2 quad + DMA, spawn scan
   task, then: `panel.set_time(12, 34)`, colon on, single saturated colour
   (red, like stock), `IS` step 5 (15 mA) as a conservative first-light
   ceiling, raise later. Log heartbeat once a second.
6. Build: `cargo build --release`. Flash + monitor: `cargo run --release`.
7. Hardware verification order:
   - Logic analyser or scope on GPIO8/9 (optional): confirm nibble→line
     mapping and the latch tail shape. Otherwise go straight to the panel.
   - Panel shows `12:34` with colon. If digits are garbled/mirrored, swap the
     nibble-bit table (one line). If one driver works and the other does not,
     the SIO2/SIO3 pins are swapped or floating → check pull-down config.
   - Segments A, B, C, D of the hour-ones and minute-ones digits (the `0xC0`
     slot, last in the cycle) must not look brighter than the rest. If they
     do, the blanking frame is missing or not being latched.
   - Optional 1 s all-red lamp test at boot (as stock does) as a wiring check.

### M2 — real time over NTP — DONE, verified on the panel 2026-10-21

Crate set (verified against sources): esp-radio 1.0.0-beta.1 (needs esp-hal
~1.2, esp-rtos with the `esp-radio` and `esp-alloc` features, a heap from
esp-alloc 0.11), embassy-net 0.9.1 (dhcpv4, dns, udp), sntpc 0.11 with the
`sntpc-net-embassy` 0.11 socket adapter and `sntpc-time-embassy` 0.6
timestamp generator.

Design:

- `src/config.rs`: `WIFI_SSID`, `WIFI_PASSWORD` and `TZ_OFFSET_MINUTES` come
  from environment variables at build time (`option_env!`), so no secret is
  ever in the tree. Missing credentials still build; the clock then shows
  `--:--` and logs why. `TWELVE_HOUR` is a const. `NTP_SERVER` is
  `pool.ntp.org`, resync every hour, retry every 30 s after a failure.
- `src/clock.rs`: Unix time = Embassy uptime (systimer, crystal derived) plus
  an offset set from each SNTP result. `now()` returns local hour, minute,
  second or `None` before the first sync. No DST handling; the offset is a
  fixed number of minutes.
- `src/net.rs`: three tasks. `wifi_task` configures the station, connects,
  waits for a disconnect, reconnects after a delay. `net_task` runs the
  embassy-net runner. `ntp_task` waits for DHCP, resolves the server, sends
  one SNTP query through a UDP socket with a timeout, stores the offset.
- `src/display.rs`: a task that reads the clock, draws `HH:MM` with the
  leading zero suppressed, blinks the colon (1 s on, 1 s off), lights the AM
  mark in 12 hour mode before noon, draws `--:--` with the colon off until
  the first sync, and publishes a new image only when it changed. It sleeps
  until the next whole second so the colon edge is sharp.
- `main.rs`: heap regions, Wi-Fi controller and station interface,
  embassy-net stack with DHCP and a hardware-random seed, spawn everything.

Steps: Cargo.toml and build.rs (`rerun-if-env-changed` for the three
variables), the four modules, main wiring, build, flash with
`WIFI_SSID=... WIFI_PASSWORD=... TZ_OFFSET_MINUTES=... cargo run --release`,
confirm the log shows a sync and the panel shows the current time.

Risks: heap too small for the Wi-Fi driver (follow the esp-radio recommended
64 KiB reclaimed + 36 KiB), binary size versus espflash's default app
partition (check with `espflash` output), the DMA scan continuing unaffected
while Wi-Fi runs (DMA needs no CPU, so it should).

### M2b — buzzer — DONE, verified 2026-10-21; reserved for the alarm

The buzzer is a magnetic (passive) type behind an S8550 PNP with its emitter
at +5 V and a 10 k base resistor from GPIO5. Any driven level turns it on
(3.3 V high still leaves 1.7 V across base-emitter), so **only high
impedance is off**. The pin must be open-drain: low = energised, released =
off.

- `src/buzzer.rs`: LEDC low-speed timer 0 at the tone frequency, channel 0
  on GPIO5 in open-drain mode. Idle duty is 100 % (output high = released,
  silent). A beep sets 50 % duty for the on time, then back to 100 %.
  Requests arrive through a channel as `Beep { freq_hz, on_ms, off_ms, count }`
  so callers never touch the hardware.
- The pin is handed to LEDC as an `Output` already high and open-drain, so
  there is no low blip while the channel is being wired up.
- Verified with a boot beep and a melody, both since removed. The firmware
  makes no sound until the alarm milestone uses `beep` / `play`.
- Tone: 2.5 kHz as a starting point; magnetic buzzers resonate around
  2 to 3 kHz. Adjust by ear. Verified 2026-10-21: beeps work.
- Melody: a `Note { freq_hz, ms }` slice (`freq_hz == 0` is a rest) played
  note by note with a short articulation gap, through the same request
  channel as beeps. `ODE_TO_JOY` is kept as an alarm tune candidate.

### M3 — colour and brightness — DONE, verified on the panel 2026-10-21

Colour is time: each RGB LED is three SEG sinks, and a channel's duty inside
the 600 µs COM slot sets its level. Brightness is the driver current `IS`,
set from the user's brightness alone and never from what is lit.

- **Sub-frames.** Each slot is split into four binary-weighted regions of
  40, 80, 160 and 320 µs. A channel at level L (0..=15) is lit in region k
  when bit k of L is set, so all sixteen levels are reachable. Every region
  starts with a frame to both drivers, so the latch lands at the same offset
  in every region and the visible duration equals the region length.
- **SPI clock 4 MHz.** At 2 MHz a frame is 47 µs, longer than the 40 µs
  region. At 4 MHz it is 23.5 µs and fits. CLK high time is 250 ns, still
  far above the 16 ns minimum and slower than the reference bit-bang. One
  unit (40 µs) is 80 bytes; a slot is 1200 bytes; the cycle with its
  blanking frame is 4847 bytes, two DMA descriptors.
- **No merging.** The reference merges identical neighbouring sub-frames to
  save interrupts. With DMA a frame costs the CPU nothing, so every slot
  always carries four frames. Re-latching identical data is invisible.
- **Panel model.** `Panel` keeps a level triple per position (4 blocks × 9
  positions × 3 channels) plus an on/off bit, set through a whole-panel
  colour, a per-digit colour, or a per-position colour. `render` produces a
  `ScanImage` of SS words per driver, per slot, per bit plane.
- **Gamma.** 8-bit colour components are perceptual; duty is linear light.
  A 256-entry table (gamma 2.2) maps a component to a level 0..=15.
  Brightness percent maps to a current step through a 101-entry table built
  like the reference's `current_for`: perceived output interpolated between
  the 2.5 mA floor and 40.4 mA with gamma 2.8, nearest step. A
  `MAX_CURRENT_STEP` ceiling (11, stock "normal", 30 mA) caps it for heat.
- **Config.** `PANEL_COLOR` and `BRIGHTNESS_PCT` consts until the buttons
  milestone adds a way to change them at runtime.
- **Scan rate, after first light.** Orange showed visible shimmer at a
  2400 µs cycle: its green component at level 2 is one 80 µs pulse per
  cycle, a 417 Hz strobe at 3 % duty. That is above flicker fusion for a
  steady light but short low-duty pulses are seen as a phantom array
  whenever the eyes move. Since the DMA engine costs the CPU nothing per
  frame, the cycle just gets shorter: SPI at 16 MHz (62.5 ns phases, about
  what the reference's register-store bit-bang runs at 3.3 V), unit 6 µs
  (48 bytes, one byte over the 47-byte frame), slot 90 µs, cycle 360 µs,
  2.8 kHz refresh. Fallback if the panel misreads at 16 MHz: 8 MHz with a
  12 µs unit, 1.4 kHz refresh. Both are two constants in `scan.rs`.
- **Several cycles per transfer.** With a 360 µs cycle the restart gap
  between DMA transfers would be a large fraction of each cycle and would
  show Wi-Fi scheduling jitter. Each transfer carries eight cycles back to
  back (about 2.9 ms, 23 KB) and one blanking frame at the end, so the gap
  occurs once per transfer and the cycles inside it are seamless.
- **Still shimmering at 2.8 kHz when the eyes move.** The phantom array is
  visible well into the kilohertz for short low-duty pulses. Two changes:
  three bit planes instead of four (8 levels per channel, 512 colours), so
  a slot is 7 units and the cycle 168 µs, about 6 kHz; and plane-major
  frame order (for each plane, each COM slot) so a digit is lit three times
  per cycle instead of once and the longest dark stretch per digit falls
  from 21 to 12 units. Transfers are sized to the 32 KB DMA limit (23
  cycles, 3.9 ms) and the scan task logs the largest restart gap it sees,
  to tell whether Wi-Fi scheduling jitter contributes.
- **Measured restart gap** at 16 MHz / three planes: average 3 µs, maximum
  644 µs over 2567 transfers. The maximum is a Wi-Fi thread holding the
  thread-mode executor. The scan task now runs on an esp-rtos
  `InterruptExecutor` (software interrupt 1, priority 2), which threads
  cannot preempt. SPI raised to 20 MHz (50 ns phases), unit 5 µs, cycle
  140 µs, 7.1 kHz; 22 cycles per transfer.
- **The 649 µs maximum gap was the rebuild**, not Wi-Fi: it did not move
  on the interrupt executor. Rebuilding a 31 KB transfer between two DMA
  transfers takes that long. Now double buffered: `publish` runs in the
  caller's thread-mode task, fills the spare buffer and queues it; the scan
  task swaps buffers at the boundary in microseconds.
- **Measured after double buffering**, with reporting moved out of the scan
  task (the serial write of the report line had itself been the 624 µs
  maximum): restart gap average 1 µs, maximum 49 µs over 3222 transfers,
  about 1.6 % dark once in a while. Shimmer judged fine from the 6 kHz
  build onward; the clock runs at 7.1 kHz with 8 levels per channel.
- **Verification.** Host simulation runs a full transfer through the chip model
  and checks that every channel's lit time per slot equals level × 40 µs and
  that the blanking frame still ends the cycle. Then on the panel: a colour
  such as orange (255, 128, 0) should look orange, and the four digits can
  be given four colours to confirm per-digit mixing.

### M4 — buttons and alarm — DONE, verified on the panel 2026-10-21

Two buttons, top (GPIO4) and bottom (GPIO3), both switch to ground with the
internal pull-up. The buzzer is used for the alarm only.

**Controls**

| button | press | clock mode | alarm-set mode |
| --- | --- | --- | --- |
| top | short | next colour in the palette | increment the field (hour +1, minute +5) |
| top | long | enter alarm-set mode | — |
| bottom | short | next brightness step | next field; from minutes: save and leave |
| bottom | long | arm / disarm the alarm | — |
| any | short | stops the alarm while it rings (press consumed) | — |

Palette: red, orange, yellow, green, cyan, blue, magenta, white. Brightness
steps: 10, 25, 50, 75, 100 %. The degree mark is lit while the alarm is
armed. Alarm-set mode shows the alarm time with the field being edited
blinking at 2 Hz and leaves on its own after 15 s without a press. The
alarm fires when the local time reaches HH:MM with the alarm armed, rings
a beep pattern every 1.5 s until a press or for two minutes, and does not
fire again in the same minute.

**Code**

- `src/buttons.rs`: one task polls both pins every 10 ms, debounces, and
  emits `Event { button, kind: Short | Long }` on a channel. Long fires at
  800 ms while still held, so the user gets feedback without releasing.
- `src/ui.rs`: `Settings` (colour index, brightness index, alarm hour,
  minute, armed) and the mode state machine (`Clock`, `SetHour`,
  `SetMinute`, `Ringing`). Pure logic: takes events and the time, returns a
  `View` describing what to draw. No hardware, so host-testable.
- `src/display.rs`: a loop that waits on whichever comes first, the next
  tick or a button event, feeds the UI, draws the view, and drives the
  buzzer while ringing. Tick is the next second in clock mode, 62 ms while
  blinking or sweeping.
- `embassy-futures` for the select.

Lessons: an `Instant` minus a `Duration` panics when the uptime is still
shorter than the duration, so "last happened" timestamps are `Option`s;
and the physical top button turned out to be GPIO4, not GPIO3.

Settings lived in RAM until M5; a reboot returned to the compile-time defaults.

### M5 — persistence — DONE, verified on the panel 2026-10-21

Colour, brightness and the alarm survive a power cut. Settings also leave
the display task so that M6 can edit them from the network.

**Shared settings.** `src/settings.rs` owns `Settings` and a
`embassy_sync::watch::Watch` holding the current value. `settings::get()`
reads it, `settings::update(|s| ...)` applies a change and publishes only
if something actually changed, and the display and the persist task each
hold a receiver. `Ui` no longer owns the settings: `handle`, `tick` and
`view` take them as a parameter, so the display loop runs button events as
`settings::update(|s| ui.handle(s, ev, now))`. The display redraws on a
tick, a button, or a settings change from elsewhere (nothing else writes
yet; M6 will).

**Flash layout.** The espflash default partition table has a 24 KB `nvs`
data partition at 0x9000 that nothing else uses. At boot
`esp-bootloader-esp-idf` reads the table and the persist task takes the
`nvs` entry's range; if there is no such partition it logs an error and
settings stay in RAM. `sequential-storage`'s map over `esp-storage`'s
`FlashStorage` keeps one key (0) whose value is an 8-byte record:
version, colour index, brightness index, alarm hour, alarm minute, armed,
two spare bytes. A record with the wrong version or an out-of-range field
is ignored and the defaults apply. Six 4 KB pages at about 20 bytes an
item means a few hundred changes between sector erases.

**Writing.** The persist task waits for a settings change, then waits until
2 s have passed without another one, and writes if the value differs from
the last one written. Cycling through the palette costs one write, not
seven. `esp-storage` runs each flash operation with interrupts off and the
cache disabled, so the scan task cannot restart a DMA transfer meanwhile:
a 32-byte write is well under a millisecond and invisible, a sector erase
is tens of milliseconds and a brief dark blink, once per few hundred
changes. `esp-storage`'s blocking `NorFlash` is wrapped in a 30-line async
adapter in `storage.rs` because `sequential-storage` only speaks
`embedded-storage-async`.

**Boot.** `main` loads the settings before spawning the display so the
first frame already has the saved colour, then hands the flash handle to
the persist task.

**Checked on the host** (scratch crate with `sequential-storage`'s mock
flash, 6 pages of 4 KB, the real key and record types): the record
round-trips, records with another version or an out-of-range field are
rejected, and 2000 saves cost 5 page erases and 20 bytes written each.
The firmware image grew by 33 KB (`sequential-storage` with its repair
paths, `esp-storage`, and the partition-table parser).

**Verify on the panel.** Change colour and brightness, arm the alarm, wait
a few seconds, pull the plug. On power-up the first sweep frame uses the
saved colour and brightness and the degree mark is lit. The log shows
`settings: loaded ...` at boot and `settings: saved` 2 s after the last
change.

### M6 — settings over Wi-Fi — DONE, verified on the panel 2026-10-21

A small HTTP page served by the clock on its LAN address, and the same
values as a scriptable API. No TLS, no authentication: anyone on the LAN
can set the alarm, which is the right trade for a bedside clock.

**Settings.** Timezone offset and 12/24 hour stop being build-time
constants and join the record, which grows to version 2, 12 bytes:
version, colour, brightness, alarm hour, alarm minute, armed, twelve-hour,
timezone offset as a little-endian i16 in minutes, three spare bytes. A
version 1 record is still read and the two new fields take the build-time
defaults, so nobody loses their colour on the upgrade. `TZ_OFFSET_MINUTES`
at build time is now just the default. `clock::now` and the hour drawing
read the live settings.

**Server.** `src/http.rs`, three identical tasks in thread mode, each on
its own embassy-net TCP socket, HTTP/1.0 style: read the request head
into a 1.5 KB buffer, read a body of at most 512 bytes if
`Content-Length` says so, write the whole response from a 2.5 KB buffer,
close. One request per connection, which is all a form needs. Three
listeners rather than one because a browser opens more connections than
it uses: a speculative one alongside the page, one for the favicon, a
fresh one for the redirect after a form post. With one socket, each of
those was "connection refused" for the next real request (found on the
panel). A client that connects but sends nothing is dropped after 2 s so
an idle speculative connection frees its socket. No framework: `picoserve` was considered and is more
than four routes on one socket deserve, and it would tie the project to
whichever embassy-net version it tracks. The parsing is in
`src/form.rs`, pure code with no sockets, so it is host-testable.

| route | does |
| --- | --- |
| `GET /` | the page: a form with the current values |
| `POST /` | applies the form, absent checkboxes mean off, redirects to `/` |
| `GET /api` | JSON of the current settings |
| `POST /api` | applies the given keys only, returns the JSON; 400 on a bad key or value |
| `POST /play` | previews the melody chosen in the form once; saves nothing (M8) |

Keys, as `application/x-www-form-urlencoded` on both routes so that
`curl -d` and Home Assistant's `rest_command` can drive it without JSON:

| key | value |
| --- | --- |
| `color` | palette name: `red`, `orange`, `yellow`, `green`, `cyan`, `blue`, `magenta`, `white` |
| `brightness` | percent from the step list: 10, 25, 50, 75, 100 |
| `alarm` | `HH:MM` |
| `armed` | `on`, `1`, `true`, or anything else for off |
| `twelve_hour` | same |
| `tz` | minutes east of UTC, −840 to 840 |
| `hostname` | 1 to 24 of `a-z`, `0-9`, `-`; sent to the DHCP server |
| `melody` | alarm sound: `beeps`, `twinkle`, `ode`, `frere` (M8) |

Changes go through `settings::update`, so the display redraws and the
persist task writes exactly as for a button press.

**Finding the clock.** The IP is logged at boot. On the panel, holding
both buttons for 0.8 s shows it: four octets in turn, two seconds each,
the first digit the octet's number and the other three its value, so
192.168.1.42 reads `1192`, `2168`, `3  1`, `4 42`. Two rounds, or any
press, and it is back to the clock. Before DHCP has an address it shows
`no1P`. The button task reports the two-button hold as its own event and
holds back the single long presses while both are down. mDNS is left out:
embassy-net has no responder and the IP display covers it.

**Resources.** Per listener: socket buffers 1 KB each way, request
1.5 KB, response 2.5 KB, all in the task's static frame; 18 KB for the
three. `StackResources` goes from 4 to 8 sockets (DNS, SNTP, three TCP,
headroom). The server never touches the scan
path.

**Hostname.** The clock tells the DHCP server its name (option 12,
embassy-net's `dhcpv4-hostname` feature), so routers that publish DHCP
names in local DNS resolve `clock` or `clock.lan` without mDNS. The name
is a setting: record version 3 appends a length byte and 24 bytes of
name to version 2, and versions 1 and 2 still load with the default name
from `config::DEFAULT_HOSTNAME`. Valid names are 1 to 24 characters of
`a-z`, `0-9` and `-`, not starting or ending with `-`; the form and the
API lower-case what they get and reject the rest. `net::hostname_task`
holds a settings receiver and, when the name changes, calls
`Stack::set_config_v4` with a fresh DHCP config, which re-sends the
option and restarts the lease; the address normally comes back the
same. The display shows `no1P` for the second or so DHCP takes. The
renewal waits until the name has been stable for 3 s: done at once, it
dropped the address while the page's response and the browser's redirect
were still in flight, and the browser reported a connection reset (found
on the panel). Saving to flash needs no such care; the persist task
already waits 2 s and the write does not touch the network.

**Roaming.** Found while testing: with two access points on one SSID the
clock sometimes connected to the far one at -82 dBm and failed with
`AuthenticationExpired`. esp-radio's default station scan method is
`Fast`, which takes the first matching beacon on any channel.
`ScanMethod::AllChannels` scans everything first and connects to the
strongest, since esp-radio sets the driver's sort order to by-signal.
One builder call in `net::station_config`; connecting takes a second or
two longer.

**Checked on the host** (scratch crate with `form.rs`, the pure part of
`settings.rs` and the head parser from `http.rs`): a version 3 record
round-trips and version 1 and 2 records migrate with the defaults for the
fields they lack; a record with a hostname too long or holding a bad
character is rejected; hostnames are lower-cased and the empty, leading
or trailing dash, space and 25-character cases are refused; the form
parser applies every key, turns absent checkboxes off only for the page,
rejects an unknown key, a colour not in the palette, a brightness off the
step list, `24:00`, a timezone past the limit and a malformed escape, and
accepts an empty body; the page is 1628 bytes, within the 2 KB response
buffer; the head parser finds the method, path and body length.

**Verify on the panel.** Open `http://<ip>/` from a phone: the form shows
the current colour, brightness, alarm, armed, 12/24 hour and timezone.
Change the colour and submit; the panel changes at once and `settings:
saved` follows 2 s later. `curl http://<ip>/api` prints JSON;
`curl -d armed=on http://<ip>/api` lights the degree mark. Hold both
buttons: the octets cycle. Set the timezone by 60 and the hour moves.
Change the hostname: the log shows the DHCP renewal and the router's
client list shows the new name.

### M7 — alarm days — DONE, verified on the panel 2026-10-21

The alarm rings only on chosen days, so a weekday alarm stays armed
over the weekend without ringing.

- **Setting.** `alarm_days`, a 7-bit mask with bit 0 Monday and bit 6
  Sunday, default all seven. Record version 4 stores it in the byte at
  offset 34 that version 3 left spare; a version 3 record loads with every
  day set. A mask of zero is rejected as a value: it would be a disarmed
  alarm that looks armed.
- **Clock.** `LocalTime` gains `weekday` (0 Monday to 6 Sunday), from the
  local day count since 1970-01-01, a Thursday, so `(days + 3) mod 7`.
- **Trigger.** `Ui::tick` fires only when armed and today's bit is set.
  The degree mark still means armed, whatever the day, so a glance on
  Sunday evening says Monday's alarm is on.
- **Page and API.** Key `days`: `all`, `weekdays`, `weekend`, or a
  comma-separated list of `mon` to `sun`. The page offers the three
  presets in a select, plus `custom` showing the list when the API set
  something else. JSON reports the preset name when one matches and the
  list otherwise.
- **Buttons.** Unchanged; the long press on the bottom button still arms
  and disarms.

**Checked on the host:** the weekday of 1970-01-01, 1969-12-31,
2000-01-01 and 2026-10-21; a version 3 record loads with every day set
and a version 4 record round-trips; masks of zero and with bit 7 are
rejected; the parser and printer agree on the presets and on lists, and
refuse the empty string, `none` and a trailing comma; the page selects
the preset or shows the custom list. The page is now 1842 bytes, so the
response buffer grew from 2 KB to 2.5 KB.

**Verify on the panel.** Set `days=weekend` from the page on a weekday,
set the alarm one minute ahead, arm it: nothing rings, the degree mark is
lit. Set `days=all`: it rings. `curl -d days=mon,wed http://<ip>/api`
echoes `"days":"mon,wed"`; `days=none` and `days=` get a 400.

### M8 — alarm melodies — DONE, verified on the panel 2026-10-21

The alarm sound is a setting: the four beeps as before, or a tune.

- **Data.** `src/melody.rs`, pure: a `Note` is a pitch in Hz (0 for a
  rest) and a length in sixteenths; a `Melody` has a name for the API, a
  label for the page, a tempo in beats per minute, the notes, and the
  pause before it repeats. `Note::ms(bpm)` is the only arithmetic. A note
  sounds for its length minus an articulation gap of one eighth of the
  note, clamped to 15 to 60 ms, so repeated pitches stay distinct and the
  beat is not stretched; rests are written out. Four melodies: `beeps`
  (four 100 ms tones at 2.5 kHz, 100 ms apart, 800 ms pause, the M4
  pattern), `twinkle` (Twinkle Twinkle Little Star, full verse, 120 bpm),
  `ode` (Ode to Joy, first two phrases, 120 bpm), `frere` (Frère Jacques,
  one round, 120 bpm). All public domain. The first cut put the tunes in
  C with a top note of A6 (1760 Hz), and that note came out far louder
  than the rest: the buzzer resonates near 2 kHz. Twinkle and Frère are
  now in G, top note E6 (1319 Hz), Ode stays in C with G6 (1568 Hz) as
  its top. If a pitch still stands out, the next step is a per-pitch duty
  table in the buzzer, which lowers the drive near the resonance.
- **Buzzer.** The request queue goes; a `Signal` carries one of `Ring
  (melody)`, `Preview(melody)` or `Stop`. The task plays note by note and
  between notes polls the signal, so a stop lands within one note. `Ring`
  repeats with the melody's pause until stopped or replaced; `Preview`
  plays once.
- **Display.** Sends `Ring` when the UI enters `Ringing` and `Stop` when
  it leaves, instead of re-queuing a beep every 1.5 s.
- **Setting.** `melody`, an index into the melody table, default `beeps`.
  Record version 5 stores it in the last spare byte at offset 35; a
  version 4 record loads with `beeps`.
- **Page and API.** Key `melody` with the melody name. The page gets an
  "Alarm sound" select and a second submit button, "Play", which posts
  the form to `/play`: the melody chosen in the select previews once and
  the page reloads with nothing saved, so a sound can be auditioned
  before Save commits the whole page. (First cut saved as well; the user
  wanted the two separate.)

**Checked on the host** (the melody module as is, plus the usual record
and form harness): every note has a length, every tone sounds for its
length minus 15 to 60 ms, a quarter at 120 bpm is 500 ms sounding 440,
an eighth 250 sounding 219, the beep pattern is 700 ms as in M4, the
tunes run 24 s (Twinkle), 16 s (Ode) and 16 s (Frère); a version 4
record loads with `beeps`, an index past the table is rejected; the API
name round-trips and an unknown one is refused; the page is 2127 bytes.

**Verify on the panel.** Pick a tune on the page and press "Play": it
plays once, in time, with audible separation between repeated notes, and
the saved sound is unchanged until Save. Set the alarm a minute ahead: it rings the tune, repeats after the
pause, and a press stops it mid-tune. `curl -d melody=ode http://<ip>/api`
echoes `"melody":"ode"`; `melody=jingle` gets a 400.

Ode to Joy was later transposed from C to G: its top note G6
(1568 Hz) sat near the buzzer's resonance and rang out louder than the
rest. Down a fourth it spans G5 to D6, in the same key as the other
tunes. Verified on the panel 2026-10-21.

### Wi-Fi reach (after M8) — DONE, verified on the panel 2026-10-21

A far access point at -78 dBm kept failing with `AuthenticationExpired`
even after the all-channel scan. esp-radio starts the radio at a
transmit ceiling of 5 dBm; the AP at 20 dBm is heard at -78, so the
clock's reply lands at the AP near -93 dBm, at the noise floor, and the
authentication request is never answered. The ceiling is now
`config::WIFI_MAX_TX_POWER_QDBM`, default 34 (8.5 dBm): the value the
user found to keep ESP32-C3 SuperMini boards stable in another project,
since their antenna makes higher power worse, not better. Before each
connect the station now scans for its SSID and logs every candidate with
channel and RSSI, so a failure can be read against the choice it had.
The first cut used the default scan dwell of 10 to 20 ms per channel
and saw nothing from where the far AP is, while the driver's own scan
found the other AP; the dwell is now 120 to 200 ms, about two seconds a
scan. The failing BSSID turned out to be the AP that reads -35 dBm from
the other room, so the two locations simply have different nearest APs.
At 8.5 dBm both connected first time, and from the far room the scan now
lists both APs (-71 and -83 dBm) with the driver picking the stronger.
Inside the enclosure 8.5 dBm was not enough; the ceiling went to 44
(11 dBm), which connects and holds with the clock in its case.

### Missed alarm after a power cut (after M8) — DONE, verified on the panel 2026-10-21

The alarm fires in `Ui::tick` on the tick where the local minute equals
the alarm minute. A power cut across that minute loses it: the clock
boots, waits for Wi-Fi and NTP, and by the time it has a valid time the
minute has passed. The safeguard: on the first tick that has a valid
time, if the alarm is armed for the day it last came round on and that
was no more than `MISSED_ALARM_GRACE` (10) minutes ago, ring as if it
had just fired. The age is computed modulo the day, so an alarm at 23:55
seen at 00:03 is eight minutes old and is checked against the previous
weekday. The check runs once per boot (`Ui::had_time`); the normal
on-the-minute path is unchanged and still rings once per alarm minute.
The first valid time arrives in clock mode unless someone is pressing
buttons within seconds of power-up, in which case the grace check is
skipped: whoever is at the buttons is awake. No settings or record
change; the grace is a constant in `ui.rs`.

### Skip the next alarm (after M8) — DONE, verified on the panel 2026-10-21

A volatile flag for holidays: `Settings::skip_next`, set from a checkbox
on the page (`skip` in the form and the API, `"skip"` in the JSON). It
lives in `Settings` so it rides the same `Watch`, wakes the display and
is edited only through `settings::update`, but `to_record` does not
write it and `from_record` leaves it false, so a reboot clears it and it
never costs a flash write: the persist task now compares records rather
than structs before saving. When the alarm minute arrives and the alarm
would ring, `Ui::tick` clears the flag instead of ringing, once; the
flag is not consumed by a minute on which the alarm would not have rung
anyway (disarmed, or a day that is off). `Ui::tick` therefore takes the
settings mutably and the display loop runs it through
`settings::update`, which only signals when something changed. The
missed-alarm check on boot goes through the same gate, where the flag is
always false. No panel indication of the flag yet.

### Form posts are all or nothing (after M8) — DONE, verified on the panel 2026-10-21

A code review found that `POST /` ran `form::apply` on the live
settings inside `settings::update`. `apply` turns the three checkbox
fields off before it reads any pair, and stops at the first bad pair, so
a form the browser could submit, such as one with the alarm time
cleared (`alarm=`), left colour, brightness and melody applied and the
alarm disarmed and 12-hour mode off, then answered with the usual
redirect and no word of it. Checked on the host before the fix: that
body returns `BadValue` with `armed` and `twelve_hour` both false.

The fix follows `/api`: the body is applied to a copy of the settings
and committed with one `update` only when every pair parsed, which also
takes the parsing out of the watch's critical section. On an error the
redirect goes to `/?error=value` or `/?error=key`; the request parser
now keeps the query string, and the page renders one line above the
form saying that nothing was saved and why. `/play` keeps previewing
whatever it could parse, since it saves nothing.

**Checked on the host** (the scratch crate of the pure modules): the
cleared-alarm body leaves the copy untouched apart from the reported
error; the page is 2244 bytes plain and 2306 with the notice, 2409 with
headers, of the 2560-byte response buffer.

**Verify on the panel.** Open the page, clear the alarm time field, press
Save: the page comes back with the notice, and the alarm stays armed and
the clock in the mode it was. Correct the time and Save: the notice is
gone and the values stick.
`scripts/verify-form-post.sh <ip>` runs the same checks over curl, plus
the unknown-key path, and puts the settings back afterwards; it passed
against a host mock of the server and flagged a mock with the old bug,
then passed on the panel.

### Wi-Fi drops out after a while (after M8) — debugging plan, 2026-10-22

The clock loses the network at some point: the page stops opening, and
it is not known whether it still answers ping. Nothing in the log says
why, because the log only covers connect and disconnect, and the IP is
printed once an hour at the NTP sync. The plan is to instrument a
spare ESP32-C3 on USB, leave it running until it fails, and read the
failure against a host-side probe log. No fix is attempted until the
logs say which of the hypotheses below it is.

**What the stack does today, as read from the sources.** esp-radio
1.0.0-beta.1 starts the station with power save `None`, so modem
sleep and its beacon-timeout bugs are out. A disconnect reaches
`wifi_task` as an event with a reason code and it reconnects after
5 s; the embassy-net link follows the driver's connected state, and the
stack restarts DHCP itself on link down and up. What nobody watches:
the IP after a reconnect, a DHCP renewal that fails with the link up,
and a station the driver still calls connected while no traffic
passes.

**Hypotheses, in the order the logs will separate them.**

1. *Link drops and comes back with another address.* The log shows
   `disconnected` and `connected`; the browser is on the old IP.
2. *Link drops and the reconnect keeps failing.* Repeated
   `connect failed` with a reason; RSSI before the drop tells whether
   it is range.
3. *DHCP lease lost with the link up.* The renewal gets no answer, the
   stack drops its config and sits in discover; link up, config down.
4. *Connected but deaf.* The driver reports connected, the AP has long
   dropped the station or the receive path stalled; no beacon timeout
   fires. Only an active probe can see this.
5. *Network fine, HTTP wedged.* Ping and the gateway probe pass, all
   three listeners are stuck in some socket state, accept never
   returns.
6. *Reboot.* Panic, watchdog or brownout; the uptime in the heartbeat
   restarts and the boot banner shows the reset reason.
7. *Thread executor starved.* A task spins and `net_task` never
   polls. The panel keeps scanning (interrupt executor) but the minute
   heartbeat stops.

**Observed so far.** When it failed, the router's lease table still
showed the clock's old lease, not refreshed. That rules out 1, and says
no DHCP exchange happened around the failure: a reboot (6) or a clean
reconnect would both have renewed it. 2 stays only as a reconnect that
never completes.

**Device-side traces, in place since 2026-10-22.** All at `info` so the
normal build keeps them; none in the scan path.

- Timestamps on every log line: esp-println's `timestamp` feature,
  with `_esp_println_timestamp` defined in `main.rs` as the embassy
  uptime in ms. Turns the capture into a timeline by itself.
- Boot: `esp_hal::system::reset_reason()` next to `settings:`.
- `wifi_task`: instead of waiting on the disconnect alone, `select`
  it against a minute timer and log `wifi: up <s>, rssi <dBm>`
  from `controller.rssi()`, with the BSSID and channel from
  `ap_info()` so a roam between the two APs is visible. The
  disconnect line keeps the reason; the `connected` line gains the
  BSSID.
- `net::monitor_task`: logs every transition. `wait_link_up` →
  `net: link up`; `wait_config_up` → `net: ip <addr> gw <gw>
  dns <dns>`; then `select(wait_link_down, wait_config_down)` and log
  which one fired. Separates hypotheses 1 to 3 at a glance.
- `net::probe_task`: every 30 s with the config up, ping the gateway
  once with embassy-net's `PingManager` (`icmp` feature, one more
  socket in `StackResources`, raise it to 10). Log only failures,
  recoveries and a running count in the heartbeat. A probe that fails
  while `wifi` still reports connected is hypothesis 4.
- `http_task`: three atomics per listener (accepted, served, head
  timeouts) and a state byte (waiting, reading, writing, closing),
  printed in the heartbeat as `http: L0 waiting 12/11/1 …`. Three
  listeners stuck in `closing` or `writing` is hypothesis 5 and says
  where. `accept` errors and head timeouts logged at `info` for the
  debug run.
- Heartbeat: add uptime seconds, so a reboot shows even if the boot
  banner scrolled off, and the probe and HTTP counters above.
- Log filter for the debug flash: `ESP_LOG=info,embassy_net=debug,
  esp_radio=debug` on the `cargo run` line; esp-println parses
  per-target levels at build time. embassy-net's `log` feature then
  prints DHCP state changes and esp-radio prints every event it
  receives. If the esp-radio events do not explain a disconnect, the
  `print-logs-from-driver` feature prints the closed driver's own
  messages; it is noisy, so only as a second round.

**Host-side capture.** `scripts/watch-clock.sh <ip>`: every 30 s, one
`ping -c1 -W2` and one `curl --max-time 5 /api`, each result on a line
with the host time. The serial monitor goes through `tee` into a file
on the same machine, so the device uptime stamps and the host clock
line up at the first `alive` line. Run both until the page fails, then:

| device log says | host log says | it is |
| --- | --- | --- |
| `disconnected`, `connected`, `net: ip` with a new address | ping to the old IP fails | 1, ruled out by the router's lease table |
| `disconnected`, `connect failed …` repeating | both fail | 2 |
| `net: config down`, link still up, DHCP discover in the debug lines | both fail | 3 |
| `wifi: up` lines continue, `probe: gateway unreachable` | both fail | 4 |
| probe fine, `http:` listeners stuck in one state | ping ok, curl times out | 5 |
| uptime restarts, boot banner with a reset reason | both fail briefly | 6 |
| `alive` lines stop, panel still scanning | both fail | 7 |

Also worth doing once it has failed, before power-cycling: check the
router's client list for the clock, and `arping` it from the host; a
station the AP has forgotten answers neither.

**Not in this round.** Any self-healing: a forced reconnect after N
probe failures, a reboot after the config has been down for ten
minutes. Those are the fixes for 4 and 3 and belong after the logs
have picked one. The `StackResources` bump and the `icmp` feature cost
about 1 KB of RAM; the probe socket is reused, not reopened.

**First capture, 2026-10-22.** The spare board, ten minutes in, found
two things before any failure:

- *Ping never worked.* The host's ping failed from the first probe
  while `/api` answered in 30 ms. smoltcp answers echo requests only
  with its `auto-icmp-echo-reply` feature, which embassy-net exposes
  under the same name and does not enable by default. It is on now,
  so ping is a usable check from the host and the router.
- *The driver's receive queue overflowed in bursts.* With
  `esp_radio=debug` the driver printed `RX QUEUE FULL` eight times in
  3 ms shortly after connecting, then in pairs now and then. The
  queue is esp-radio's `ControllerConfig::rx_queue_size`, 5 frames by
  default; the driver's receive thread delivers a burst of LAN
  broadcasts faster than the thread-mode executor gets to run the
  stack, and everything past the fifth frame is dropped. The DHCP
  offer was one casualty: the link came up at 5 s and the address
  arrived at 15 s, smoltcp's discover retry. The queue is now
  `config::WIFI_RX_QUEUE_FRAMES`, 32, the same as the driver's dynamic
  receive buffers. Whether it is also the long-run failure is open: a
  queue that fills and stays full would be hypothesis 7 with a cause,
  and the debug line's timestamps will show it.
- *The first DHCP discover is lost whatever the queue.* The user knows
  this one from another project on the same driver: the station
  reports connected before it can pass a frame, the stack sends its
  discover at that instant, and smoltcp waits its 10 s
  `discover_timeout` before the next. The workaround there was to
  build the stack only after the connect plus 500 ms. Here every task
  takes the stack at boot, so the same delay is applied the other way
  round: the stack starts with no IPv4 configuration, `wifi_task`
  switches DHCP on `LINK_SETTLE` (500 ms) after each connect and off at
  each disconnect. That also covers reconnects, where embassy-net
  would otherwise restart DHCP at the link-up instant. embassy-net
  does not wake its runner for a configuration change, so a throwaway
  UDP bind follows the switch (`net::poke`); the hostname renewal uses
  the same path and skips the renew while the link is down.

Both verified on the spare board 2026-10-22: DHCP switched on at
5.39 s after boot and the address was up at 5.40 s, 20 ms later;
no `RX QUEUE FULL` in the first minutes; ping and `/api` answer from
the host; gateway probes pass; RSSI -42 dBm on the near AP. The
long-run failure is still to be captured.

**Review finding, 2026-10-22: a way for the stack to go deaf while
connected.** The user reported that during the outage the clock kept
ticking and the buttons worked, so the thread executor was alive and the
stack's runner was being polled; the question was what could leave it
asleep. Reading esp-radio 1.0.0-beta.1 (`src/wifi/mod.rs`, unchanged on
upstream main as of today, no changelog entry):

- `WifiTxToken::consume_token` increments `WIFI_TX_INFLIGHT`, then
  `esp_wifi_send_data` returns early, *without* the decrement, when the
  station state is no longer `Connected` at that instant. The token was
  handed out a few microseconds earlier on `link_state() == Up`, so a
  disconnect event landing in that window leaks one slot.
- The counter is never reset: not on `StationDisconnected`, not in
  `stop`, not in `WifiController::new`. The only decrements are the
  driver's tx-done callback and an `esp_wifi_internal_tx` error.
- `can_send()` is `inflight < tx_queue_size` (3), and the embassy-net
  driver's `receive` goes through `rx_token()`, which returns `None`
  unless `can_send()`. After three leaks neither direction works.
- embassy-net then sees `transmit` return `None` every poll, marks
  `tx_exhausted` and does not arm smoltcp's timer. The runner wakes on
  each incoming frame, cannot take it, and sleeps again; the queue
  fills and every further frame is a dropped `RX QUEUE FULL`. The
  driver still reports connected, DHCP renewals never go out, the lease
  on the router simply ages. A reconnect does not help, since the
  counter survives it; only a reset does.

Whether a frame the driver accepted and then dropped at a disconnect
gets its tx-done callback is a property of the closed blob and could
not be checked; if not, every disconnect with traffic pending is
another leak and three disconnects are enough.

What the capture would show: `wifi: disconnected` lines earlier in the
log, then `probe: gateway unreachable` every 30 s with `wifi: up` lines
continuing, and with `esp_radio=debug` a continuous `RX QUEUE FULL`.

Proposed fix, in two layers, not yet done:

1. Patch esp-radio through `[patch.crates-io]` (a local copy or a fork):
   decrement on the early return in `esp_wifi_send_data`, reset the
   counter on `StationDisconnected` and in `stop`, and stop gating
   `rx_token` on `can_send` (upstream's newer `xarxa` driver path
   already does not). Offer it upstream.
2. In the firmware, a self-heal that does not depend on knowing the
   cause: the gateway probe failing for five minutes while the driver
   reports connected logs the fact and resets. Settings are in flash
   and the time is back within seconds of the reboot. Hypotheses 3 and
   4 both end there. **Done 2026-10-22, awaiting a bench check:**
   - `probe_task` arms itself on the first gateway answer since boot
     (`probe: gateway answers …; self-heal armed`), so a router that
     never answers ping cannot reboot-loop the clock. Then
     `config::PROBE_RESET_FAILURES` (10, five minutes) consecutive
     failures with `stack.is_link_up()` call `heal::reset`. Link down
     is left to the Wi-Fi task's reconnect.
   - `heal::reset` logs `heal: <reason>; self-heal reset N since
     power-up: blanking the panel and rebooting`, then waits for
     `scan::stop`: the scan task raises a flag check after each
     transfer's `wait()`, and the transfer ends on the blanking frame,
     so the panel is dark and the wires rest on their pull-downs when
     the task parks itself. The user asked for this: a reset
     mid-transfer leaves the row the drivers last latched lit at full
     current until the firmware is back. Then the count goes to RTC
     fast RAM (`#[ram(unstable(rtc_fast, persistent))]`, with a mark
     word against a skipped zeroing), 200 ms for the serial port, and
     `software_reset`.
   - The boot line reads `boot: reset reason Some(CoreSw), self-heal
     resets since power-up N` and the heartbeat ends with `self-heal
     resets N`, so a heal that happened with nobody on the serial port
     is still on record until the next power cut.

   Bench check, on the spare board with the monitor attached: drop the
   router's echo replies to the board for six minutes. On OpenWrt
   (fw4): `nft insert rule inet fw4 output ip daddr 10.1.1.31 icmp type
   echo-reply drop`, or with iptables `iptables -I OUTPUT -d 10.1.1.31
   -p icmp --icmp-type echo-reply -j DROP`. Expect `probe: gateway
   unreachable … (1 in a row)` every 30 s, at the tenth the `heal:`
   line, the panel dark with no lit row during the reboot, then the
   boot line with `CoreSw` and count 1, and the heartbeat carrying 1.
   Remove the rule; the count stays until a power cut. A panic reset
   still goes through `custom_halt` and does not blank first. Done and
   passed, see the bench test entry below.

**Self-heal bench test, 2026-10-22, passed on the spare board** (the
user confirmed the panel goes dark through the reboot with no row left
lit, so the blank-first path is verified) with the
router dropping its echo replies (`nft` table `healtest`): probe failures
every 32 s (30 s period plus the 2 s timeout), the tenth at 325.8 s of
uptime followed at once by `heal: gateway 10.1.1.1 unreachable for 10
probes (300 s) with the link up; self-heal reset 1 since power-up:
blanking the panel and rebooting`, then a boot with `reset reason
Some(CoreSw), software reset: carried over Carried { heals: 1,
last_heal_unix: Some(1792693762), skip_next: true, handled: None }`
and `display: skip-next alarm carried over the reset`; the skip box had
been ticked before the test. The two flasher resets before it read
`CoreUsbUart, RTC carry-over discarded`, so the hard-reset gate holds.
Heap flat at 54080 bytes throughout. A second round on the same board,
rule removed for 14 minutes (28 probes passed) then re-added, healed
again at 1135.8 s of that boot, and the next boot read `heals: 2,
last_heal_unix: Some(1792694898), skip_next: true`, so the record
accumulates across repeated software resets. With the rule left in
place the next boot's first probe fails and the watchdog stays
disarmed, as designed: the real clock (`martin-clock`, 10.1.1.60)
showed exactly that, `heals: 1` from its own test and no second
reboot while its rule stayed. One thing the host-side watch log
shows that is not a fault: the first ping after every boot fails while
`/api` answers, because smoltcp has no neighbour entry for the host
yet, drops the echo reply and sends an ARP request instead of queueing
the reply; the next ping, 30 s later, succeeds. Linux would queue the
packet, smoltcp by design does not.

**Alarm state across a self-heal, 2026-10-22.** The user asked whether
the skip-next flag survives a self-heal. It did not: it was RAM only,
and a reboot also re-arms the missed-alarm grace, which would have rung
an alarm dismissed or skipped minutes before the reset. Now `carry.rs`
owns one RTC-RAM record, `[mark, heals, last heal, skip, handled unix,
handled minute]`, and `carry::init` at boot keeps it only when the
reset reason is a software reset (`CoreSw` or `Cpu0Sw`) and the mark
is present; power-on, brown-out, watchdog and the flasher's reset zero
it, and the boot line says `software reset: carried over …` or
`RTC carry-over discarded` (the user asked for the hard-reset gate
explicitly). The display task puts the carried skip flag back into the
settings at start, writes the flag whenever it changes, and records
the alarm as handled when ringing ends or the skip flag is used up;
on the first valid time after boot, if that was within 15 minutes, it
seeds `Ui::suppress_alarm`, and the grace check now also respects the
seeded minute. A reset while still ringing records nothing on
purpose, so the grace brings that alarm back. Checked on the host with
a scratch crate of `ui.rs`: the grace rings an unhandled alarm, skips a
handled one, the alarm minute itself stays quiet when handled, a seed
for another minute does not suppress, a carried skip flag is used up by
the grace, the next day's alarm still rings, and the normal path is
unchanged. Verify on the panel: tick "skip the next alarm", run the
heal bench test, and the page should show the box still ticked after
the reboot; dismiss a ringing alarm and heal within ten minutes, and it
must stay quiet.

**Status on the page, 2026-10-22.** The clock in its case has no serial
port, so the counter is shown where it can be read: the page ends with
`Up 3 d 4 h 12 min. Self-heal resets since power-up: 1, last Wed 03:14
(5 h ago).` and the JSON gains `uptime` (seconds), `heals` and
`last_heal` (Unix seconds or null). The RTC record is now
`[mark, count, unix time of the last reset]`, 0 for the time when the
clock was not synced. `form::Status` carries values the HTTP layer has
already converted (`clock::local_time` is the new pure conversion
`now` also uses), so `form.rs` stays free of the clock and the host
check still builds. A posted `heals` key is refused as unknown.
`RESPONSE_BUF` went to 4096 as the backlog asked. Checked on the host
with the scratch crate: worst case (24-char hostname, custom day list,
every checkbox, the notice, a 46296-day uptime and a last heal) is
2426 bytes, 2529 with headers; the plain page 2364; the JSON 243.
Verify on the panel: the page shows the line with the uptime counting,
`curl /api` ends with `"uptime":…,"heals":0,"last_heal":null`, and
after a self-heal both show 1 and the time.

**Running it.** Flash the spare board with the debug filter and keep
the monitor in a file; start the watch script against the address the
board logs:

```
ESP_LOG='info,embassy_net=debug,esp_radio=debug' WIFI_SSID='…' WIFI_PASSWORD='…' \
  TZ_OFFSET_MINUTES=-420 cargo run --release 2>&1 | tee clock-serial.log
scripts/watch-clock.sh <ip> | tee clock-watch.log
```

The first minute should show `boot: reset reason`, the candidates, a
`connected to <bssid>` line, `net: link up`, `net: ip … gateway …`, and
then every minute a `wifi: up … rssi …` line, an `alive … gateway
probes …` line and an `http: L0 waiting …` line. The address should
now arrive about half a second after `wifi: connected`, and `RX QUEUE
FULL` should be rare or gone. The gateway probes
should count up with no failures; if the router never answers ping,
every probe fails from the start, which is the time to swap the probe
target for another box on the LAN. The riscv32imc target has no atomic
add, so the counters are plain load-and-store with one writer each.

**Done when.** The user has a capture that falls in one row of the
table above, and the entry below this one records what it was.

### What the capture showed (after M8) — DONE, verified on the panel 2026-10-22

Eleven hours of the spare board on the bench produced no fault of its
own: no disconnect, no `RX QUEUE FULL`, flat heap, every probe answered
except the ones the router rule dropped. Kicking the station from the
near access point three times with hostapd's `del_client` did:

- After each kick the driver tried only the far AP for about a minute,
  although its own all-channel scan listed the near one at -38 dBm and
  the far one at -77. ESP-IDF avoids the AP that just disconnected it
  for a few attempts. Those attempts died with
  `DisassociatedDueToInactivity`, `AuthenticationExpired` and
  `Class3FrameFromNonAssociatedStation`, which all mean the far AP did
  not hear the board. Once it did succeed and the board sat on the far
  AP for 223 s, reported connected at -77 dBm, with DHCP taking 30 s,
  one gateway answer in seven probes, five connections accepted and none
  served, and the host unable to ping it. That is the original symptom
  reproduced: connected and dead, with no firmware state machine at
  fault. Five probes had failed when it was kicked off; the self-heal
  would have reset it after ten and the fresh scan would have picked the
  near AP.
- The AP side explains it. Luci on the near AP shows the spare board at
  -59 dBm against -38 heard the other way: the board's 11 dBm ceiling
  and antenna put its uplink 21 dB below the downlink. The far AP, heard
  at -77, therefore hears the board around -98, under its -90 noise
  floor; it can never hold these boards. The real clock reads -84/-91
  on the near AP: a 7 dB margin, which is the decoding limit of the
  6 Mbit/s rate that OpenWrt's default (`legacy_rates 0`) makes the
  lowest one. Downlink fine, uplink lossy in bursts: lease renewals and
  page replies vanish while the driver reports connected. That is the
  primary cause on the real clock; the self-heal is the backstop.

**Next steps, in order.**

1. Transmit ceiling up one step, 44 to 56 (14 dBm), on the spare board
   first. Measure: the Luci signal figure for the board on the near AP
   (was -59), the board's own `rssi` line (was -38 to -43, should not
   move), and whether the connect and the probes stay clean. A gain of
   less than 3 dB at the AP means the antenna is saturating and the
   next step is not worth taking; a full step earns a try at 68
   (17 dBm). Then the same on the real clock, read from inside the case.
   Measured: 56 read -55 at the AP (a full step from -59), board RSSI
   still -38, connect and probes clean. 68 read -52, again a full step,
   board RSSI -39, connect clean; after a kick the far AP, which could
   not hear the board at 44, heard it at -70 and held it, slow (gateway
   108 ms, a page in 1.2 s) and unstable, with DHCP taking 135 s there
   against 20 ms on the near AP. 80 (20 dBm) read -46 at the near AP,
   two steps' worth, and the link got worse everywhere: the first
   connect after a clean boot tried the far AP twice and took 24 s, the
   gateway probe on the near AP went from 41 to 86 ms, and after a kick
   the far AP, which hears the board louder than at 68, failed 16
   associations in a row with `Class3FrameFromNonAssociatedStation` and
   `HandshakeTimeout`: frames arriving but not decoding. That is the
   SuperMini antenna mismatch at full power. Correction from the
   esp-wifi-sys doc table: the driver rounds the setting down to a
   level, and 68 is 16 dBm, not 17; 72 to 79 are 18 dBm, 80 is 20. So
   the series was 11, 14, 16, 20 dBm, and 72 (18 dBm) is the one
   untested level between the last good one and the bad one. Measured:
   the near AP read -47 jumping to -52, the far AP -73 against -70 at
   68, the first probe 70 ms against 41, the first connect fine but the
   far AP took five tries after a kick, DHCP 80 s there, pages in 1.0
   to 1.4 s. More drive, a quieter far AP: past the knee. 68 (16 dBm) is
   the ceiling, verified by the user on the AP side; the guide's
   hardware section says so. On the real clock, in its case, 68 moved
   the near AP's reading from -84/-91 to -76/-90 with dips to -78: an
   8 dB gain, 14 dB above the noise floor against 7 before, clear of
   the 6 Mbit/s decoding limit with margin. The smoltcp DHCP lines never appeared:
   smoltcp logs only with its own `log` feature, which embassy-net does
   not enable; turning it on is the next thing to do before another
   far-AP session, since DHCP there took 30 to 135 s at every power.

   Probe size. On the far AP at 18 dBm the probe passed every time while
   pages loaded in chunks over 1 to 1.4 s: a 10-byte ping on a marginal
   uplink says nothing about 1.5 KB frames, and a page is two of those.
   The probe payload is now `config::PROBE_PAYLOAD_BYTES`, 1400 bytes
   (1428 with headers, under the 1492 MTU), the ICMP buffers hold one
   such packet each (a reply that does not fit is dropped as truncated),
   the payload is a static so the task's future stays small, and the
   armed line names the size. `scripts/watch-clock.sh` pings with the
   same payload. The 2 s timeout stays: a page segment that takes longer
   than that is a failed page in practice. Verify: `probe: gateway
   10.1.1.1 answers a 1400-byte ping in N ms` on the near AP with N near
   the old 12 to 40 ms; on the far AP expect failures to appear where
   the small ping passed and the page crawled.
2. On the AP: `legacy_rates '1'` lets the clock fall back to 1 Mbit/s,
   about 10 dB more uplink margin than 6 Mbit/s OFDM, at some airtime
   cost. No firmware change.
3. Pick the AP ourselves: `net::wifi_task` already scans before each
   connect; pin the strongest candidate's BSSID in `StationConfig`
   (`with_bssid`) and fall through to the next on failure. Removes the
   driver's avoidance from the choice, so a kick or an AP reboot does
   not strand the clock on an AP that cannot hear it.
4. Re-select after three consecutive probe failures with the link up:
   scan while connected, reconnect to a candidate at least 10 dB
   stronger if there is one. Turns the five-minute reset into a one to
   two minute reconnect; the reset stays for the case no AP can hear
   the board, where it also rescans every five minutes until the near
   AP returns.

The esp-radio `WIFI_TX_INFLIGHT` leak noted above remains unproven on
hardware and stays in the backlog. BSSID pinning (items 3 and 4) was
set aside by the user in favour of the two changes below.

### Heal when the station cannot connect, and keep the time across a heal (after M8) — DONE, verified on the panel 2026-10-22

**Problem.** The self-heal lives in the probe task, which waits for an
address and checks `is_link_up`, so a station that is not associated is
never healed: the reconnect loop in `net::wifi_task` had no failure
budget. The 20 dBm run showed the case it misses: after a kick the
driver tried the far AP sixteen times in 200 s and never fell back to
the near one at -38 dBm. The driver avoids the AP that last dropped it,
that state lives in the radio library, and a reset is what clears it.

A reset also costs the time. The clock is Embassy uptime plus an NTP
offset in RAM, so after a software reset the panel sweeps until the
next sync, which with no Wi-Fi may be a long time. The RTC timer is
the fix: esp-hal documents that any reset other than power-up leaves it
running, and it is read through `Rtc::time_since_power_up` with the
slow-clock calibration applied.

**Design.**

- `config::CONNECT_RESET_ATTEMPTS` (10) and `CONNECT_RESET_SECS` (180).
  `wifi_task` counts consecutive failed `connect_async` calls and notes
  the time of the first failure in the run; when either limit is
  reached it calls `heal::reset` with the count and the span in the
  reason. A successful connect clears both. The user chose this over a
  candidate test: the heal fires whether or not the scan showed an AP,
  so with every AP down the clock resets every three minutes until one
  is back. With the time carried that costs a dark blink and nothing
  else; before the first sync after a power-up the panel was sweeping
  anyway.
- `carry::Carried` gains `time_offset_us: Option<i64>`, two more words
  in the record: Unix microseconds minus RTC microseconds at the moment
  it was written. `clock::init` takes the `Rtc` built from
  `peripherals.RTC_TIMER` in `main`; `clock::carry_time` writes the
  offset, `clock::restore_carried` sets the clock from it at boot and
  returns whether it did. `heal::reset` writes the offset just before
  the reset, and the minute heartbeat in `main` refreshes it so a panic
  reset, which cannot run async code, also carries the time to within a
  minute of RTC drift. The record keeps its software-reset-only policy,
  so a flasher or power reset still starts without a time.
- Between the write and the restore the elapsed time comes from the RTC
  slow clock, about 5 s of reboot; after the restore the clock runs on
  the crystal again, and the next NTP sync replaces the offset as
  always. Error across a heal: well under a second.

**Verify.** Flash, let it sync, then kick the station from the near AP
and block it on the far one (or power the far one off and ban the board
on the near one): expect `wifi: connect failed … (attempt N of 10)`
lines and, at the tenth or at 180 s, `heal: no connection after N
attempts over S s …`, the panel going dark, and a boot line with
`carried over Carried { …, time_offset_us: Some(…) }` followed by
`clock: time carried over the reset, local time HH:MM:SS` before any
Wi-Fi line, with the panel showing the time at once. `/api` then shows
`heals` one higher. For the panic path, nothing to do by hand; the
heartbeat line keeps the offset fresh.

**Verified 2026-10-22** on the spare board in one run: a kick sent the
station to the far AP after five failed attempts, counted as `attempt 5
of 10, 35 s of 180` without firing; on the far AP all ten 1400-byte
probes failed while the host's small `/api` fetches were served, the
heal fired at 300 s, the boot line read `CoreSw` with `heals: 1` and a
time offset, the clock printed 16:51:04 at 260 ms into the boot and NTP
said 16:51:09 5.2 s later, and the fresh scan took the near AP first
try. The connect budget's own reset is exercised by the same code path
and was not forced to fire; the arithmetic in the log lines is right.

### Review backlog (2026-10-21)

Found in the same review, not yet done, in rough order of value:

- **Alarm lost while in another mode.** `Ui::tick` only checks the alarm
  in clock mode. If the alarm minute passes entirely while the user is
  in alarm-set mode or the IP display (both run about 15 s and the set
  mode is extended by every press), it never rings; the missed-alarm
  grace covers only the first valid time after boot. Evaluate the due
  check in every mode and switch to ringing from any of them, or on the
  return to clock mode ring if the alarm came round within the grace.
- **Spaced day lists.** `parse_days` trims each name, but the 32-byte
  value buffer in `form::apply` is one byte short of
  `mon, tue, wed, thu, fri, sat, sun`, so the API answers "bad value"
  before the trim runs. Raise the buffer to 40 or so.
- **First wake of a fresh watch receiver.** embassy-sync starts a
  receiver at id 0 and `settings::init` bumps the watch to 1, so the
  first `changed()` returns at once. `persist_task` swallows that with
  `try_changed`; the display and hostname tasks do not, costing one
  extra redraw and one no-op renew check. A `try_changed` after
  `receiver()` in those two, or a helper in `settings`, makes them
  consistent.
- **Build-time timezone range.** `TZ_OFFSET_MINUTES` is cast to i16 in
  `config.rs` without a range check, so a wild value wraps silently and
  `Settings::DEFAULT` is never run through `is_valid`. A const assert
  against `TZ_LIMIT_MINUTES` catches it at compile time.
- **Unused public API.** `Panel::set_time`, `set_segment`,
  `set_position_color`, `set_dash`, `Levels::OFF`, `clock::is_synced`,
  `aip33628::IS_MA_TENTHS` and `scan::SLOT_US` have no callers; the lib
  crate hides the warnings. `Panel::lamp_test` is also unused but worth
  keeping as a hardware aid.
- **Gap statistics on a DMA start failure.** The retry path in
  `scan_task` leaves `ended_at` set, so each retry adds its delay to the
  restart-gap statistics. Statistics only.

## 6. Risks and how they are handled

| risk | mitigation |
| ---- | ---------- |
| quad nibble bit ↔ SIO line order differs from assumption | confirmed correct on hardware (M1) |
| quad lines float between DMA transfers | pull-downs on all four pins; frames end all-low |
| DATA/CLK simultaneous edge read as latch | 3 nibbles per bit, data never changes when CLK falls |
| last COM slot stays lit across the DMA restart gap | cycle ends with an all-off blanking frame; gap is dark, slots are equal |
| `half_duplex_write_buffer` rejects 2447-byte transfer | limit is 32 KB on C3; buffer is one descriptor; also tested with blocking `half_duplex_write` fallback |
| esp-rtos / esp-hal version mismatch | start from esp-generate pinned set, bump only if it builds |
| thermal: too many sinks at high IS | IS fixed from brightness (step 5 initially), never all-white for long |

## 7. Roadmap (2026-10-22)

Next work, in priority order: over-the-air updates first, then
Wi-Fi provisioning from an access point, then the two sensors the board
already carries. Each entry is the design as planned; it gets the
measured facts and the "verified" mark the way the milestones above
did, and the review backlog stays separate. The hardware facts came
from `3rdparty/HU-058_ESPHome/docs/hardware.md` (socket pinout), the
adapter board's schematic in `adapter/adapter/adapter.kicad_sch` (an
ESP32-C3-Zero on a 2×8 header into the 8051 socket) and the esp-hal,
esp-radio and esp-bootloader-esp-idf sources in the registry.

### M9 — OTA updates — planned

**Why first.** The clock lives in its case with no USB reachable, and
every fix since M8 has meant opening it. An update over the LAN turns
that into a `curl`.

**Flash layout.** The 4 MB flash today carries espflash's default
table: `nvs` at 0x9000 (24 KB), `phy_init` at 0xF000, one `factory`
app at 0x10000 of 4032 KB. The release image is about 830 KB (text
820 KB, data 10 KB), so two slots fit with room to double. New
`partitions.csv`, checked in and passed to espflash from the runner
line in `.cargo/config.toml` (`--partition-table partitions.csv`):

| label | type | offset | size |
| --- | --- | --- | --- |
| nvs | data/nvs | 0x9000 | 0x6000 |
| phy_init | data/phy | 0xF000 | 0x1000 |
| otadata | data/ota | 0x10000 | 0x2000 |
| ota_0 | app/ota_0 | 0x20000 | 0x1E0000 |
| ota_1 | app/ota_1 | 0x200000 | 0x1E0000 |

`nvs` keeps its offset so saved settings survive the repartition. The
one-time USB flash with the new table erases `otadata`
(`--erase-parts otadata`); all-ones `otadata` with no `factory` boots
`ota_0`, which is where espflash puts the image. The bootloader is
espflash's prebuilt one, and the crate docs warn it "might not"
support OTA. ESP-IDF bootloaders have always booted from `otadata`,
so the expectation is that it works; it is the first thing to verify,
and if the boot log keeps loading from 0x20000 after a slot switch,
the fallback is building the bootloader from ESP-IDF with
`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE` and pointing espflash at it
with `--bootloader`.

**Upload.** `POST /ota` with the raw application binary as the body
(`espflash save-image --chip esp32c3 … app.bin`, then
`curl --data-binary @app.bin http://<ip>/ota`). The request head is
parsed as today; the body is streamed, not buffered: the handler
reads the socket into a 4 KB sector buffer and, per full sector,
erases and writes it into the slot that `OtaUpdater::next_partition`
hands back (`esp_bootloader_esp_idf::ota_updater`, over the same
`esp-storage` flash the settings use). `Content-Length` is required
and must be at most the slot size; the first sector is checked for
the image magic (0xE9) and the app descriptor magic before anything
is erased, and a body that ends short leaves the slot half written
and `otadata` untouched, so the running image stays the boot image.
On the last sector the handler calls `activate_next_partition`,
answers `200` with the slot name and byte count, and resets through
`heal::reset` after the response is flushed. One upload at a time: a
static busy flag makes a second listener answer `503`.

**The scan during the write.** Flash writes run with interrupts off,
and 830 KB is about 200 sector erases of tens of ms each: the panel
would blink dark for the whole upload and the last latched row could
sit lit at full current between sectors. The upload therefore starts
by stopping the scan through `scan::stop` (the transfer ends on its
blanking frame, same as the heal path), so the panel is dark from
the first byte to the reboot. That is the honest display for "do not
unplug". Expected wall time: the TCP side with 1 KB socket buffers
over Wi-Fi is the limit, somewhere between ten and forty seconds;
measure it. Wi-Fi frames dropped while interrupts are off are
covered by TCP retransmission.

**Rollback without bootloader support.** The bootloader falls back
to the other slot on its own only if the selected image fails its
checksum. A new image that boots and then panics or loses the network
is on the firmware. The `carry` record gains an `ota_pending`
counter: set to 1 by the upload, bumped on every software reset while
non-zero, cleared when the new image reaches "good" (defined as the
first gateway probe answered, or the first HTTP request served, so a
board with a dead router can still be updated). If it reaches 3 the
boot path switches `otadata` back to the previous slot and resets.
A power cut zeroes the carry record, which is the right default: the
user pulled the plug, the user decides.

**Visible state.** `build.rs` exports `git describe --always --dirty`
as `FIRMWARE_VERSION`; `/api` gains the read-only keys `version` and
`slot` (`ota_0`/`ota_1`), the page status line shows both, and the
boot line logs them. The page-size check on the host runs again
(worst case was 2529 bytes with headers against the 4096 buffer).
`scripts/ota.sh <ip> [app.bin]` builds the image and posts it.

**Security.** The upload is as open as the settings form: anyone on
the LAN can post. That is the existing trust boundary and stays so;
a build-time `OTA_TOKEN` checked as a query string is a one-line
addition if the LAN ever becomes shared.

**Touches.** `partitions.csv`, `.cargo/config.toml`, `build.rs`,
`src/ota.rs` (slot lookup, sector writer, activate, the pending
counter), `src/http.rs` (`POST /ota`, streaming body, busy flag),
`src/carry.rs`, `src/form.rs` (`Status` version and slot),
`src/bin/main.rs` (boot-time rollback check, log line), `scripts/`.

**Host checks.** The head parser with a `Content-Length` of a few
hundred KB; the sector writer over `sequential-storage`'s mock flash
with a body that ends short, one that is exactly a sector multiple
and one that is not; the carry record round trip with the counter.

**Verify on the panel.** Flash once over USB with the new table and
read the boot log: `otadata` listed, app loaded from 0x20000. Post an
image with a changed version string: the panel goes dark, the curl
returns the slot and size, the clock reboots within a minute and
`/api` shows the new version and `ota_1`. Post again: back to
`ota_0`. Post a truncated file: `4xx`, the clock keeps running the
old image after the dark period, and `/api` still shows it. Flash an
image that panics after boot: three resets later the boot log shows
the previous slot and the version in `/api` is the old one.

### M10 — Wi-Fi provisioning from an access point — planned

**Scope.** Credentials become a stored setting instead of a build-time
constant, and a clock that has none raises its own network so the
user can give it one from a phone. While it is an access point
there is no internet, so no time and no alarm: the panel says so and
waits. The build-time `WIFI_SSID`/`WIFI_PASSWORD` stay as the seed
for a board whose record has no credentials yet, and may be empty.

**Mode decision at boot, not at run time.** esp-radio
1.0.0-beta.1 accepts `Config::AccessPointStation` but its receive
path has a comment saying AP+STA is not supported yet, so the clock is
either a station or an access point for one boot. `main.rs` decides
before it builds the stack: access point if the record has no SSID or
if the carry record's `boot_ap` flag is set by the button hold below;
station otherwise. One
`StackResources` serves whichever interface runs, so a second stack
costs no RAM. The HTTP listeners are spawned on the stack that exists.

**Getting into and out of it.** Exactly two things start the access
point: a record with no SSID, and both buttons held for 10 seconds
(the existing both-button hold, extended; the 2 s IP display still
fires first and is harmless), which sets `boot_ap` in the carry
record and resets through `heal::reset`. Nothing else does: a station
that cannot associate keeps retrying forever, as today, so a router
that is off for an hour or a wrong password never flips a clock on
the wall into an access point by itself. A wrong password is
therefore a blank clock until the user holds the buttons and enters
it again; the station log line says `wrong password` or `no such
network` from the disconnect reason to make that diagnosable over
USB. The access point has no timeout: it stays up until credentials
are saved, which clears the flag and resets into station mode, or
until power is cut, which zeroes the carry record and so boots the
station again if the record has an SSID.

**The access point.** SSID `esp32clock-<last two MAC bytes>`, WPA2
with the build-time `AP_PASSWORD` (default `esp32clock`, documented),
channel 1, address 192.168.4.1/24 static. embassy-net has no DHCP
server, and a phone will not talk to an access point that does not
give it an address, so the clock answers DHCP itself: a UDP task that
offers and acknowledges one of eight addresses in 192.168.4.2–.9
with a one-hour lease, discover/request only, about 150 lines in
`src/dhcpd.rs`, with `edge-dhcp` as the alternative if the hand-written
one proves fiddly. A captive-portal DNS (every A query answered with
192.168.4.1, so the phone opens the page by itself) is the second
step once the plain flow works; until then the address is in the
docs and on the panel.

**The page in this mode.** `GET /` serves the provisioning form:
SSID (32 bytes), password (64 bytes), timezone and hostname, the
rest of the settings hidden since there is nothing to show for them.
A scan list is not possible without AP+STA; the user types the SSID.
`POST /` applies through the existing all-or-nothing path and
answers with a page that says "rebooting into <ssid>"; the password
is write-only, never in the page or JSON, and `/api` reports only
whether one is set (`wifi_configured`). `form::apply` decodes values
into 32 bytes today, so the value buffer grows to 64 for the password;
the page-size check runs again.

**Record.** `RECORD_VERSION` 6 adds `ssid` (32 bytes plus length) and
`password` (64 plus length), `RECORD_LEN` about 134; the storage
scratch buffer is 128 and grows with it. Version 5 records read as
"no credentials" and fall back to the build-time seed, so an updated
board keeps joining its network with no visit.

**Panel.** Clock mode is unreachable without time. A new `Ui` mode
`Provision` shows `AP` on the left pair and the hour digits blank,
colon off, and both-long shows the fixed address as the IP display
does; any other press is ignored. A station that is waiting the 3
minutes shows the usual blank-until-synced face, so the only new
face is `AP`.

**Touches.** `settings.rs` (fields, validity, record v6, two
`RECEIVERS` unchanged), `storage.rs` (scratch size), `form.rs`
(keys, the provisioning page, write-only password), `http.rs` (serve
the provisioning page when in AP mode), `net.rs` (AP config, the
disconnect-reason log line, the DHCP server task), `dhcpd.rs`, `carry.rs`
(`boot_ap`), `ui.rs`/`display.rs` (`Provision` mode), `buttons.rs`
(10 s hold), `main.rs` (the boot decision), `config.rs`
(`AP_PASSWORD`), `StackResources` (+2 UDP for DHCP and
DNS), CLAUDE.md controls table.

**Host checks.** Record v5 → v6 migration and round trip with the
longest credentials; the provisioning page size; the DHCP
discover/offer/request/ack byte layout against a captured phone
exchange (any laptop `tcpdump` on a hotspot gives one); `Ui` in
`Provision` ignoring presses except the hold.

**Verify on the panel.** Flash with empty credentials: the panel
shows `AP`, a phone sees `esp32clock-xxxx`, gets an address, opens
192.168.4.1, saves the home network, and the clock reboots and syncs.
Save a wrong password: the clock stays blank, the USB log says why,
and it never shows `AP` on its own. Power the router off with the
clock running: it reconnects when the router returns without ever
showing `AP`. Hold both buttons 10 s on a working clock: `AP`; cut
the power: back to the clock.

### M11 — NTC thermometer and light sensor — planned

**Hardware.** The adapter schematic already wires both sensors, and
the pull-ups live on the adapter, not on the clock board:

| adapter net | socket pin | clock part | adapter part | C3 pin |
| --- | --- | --- | --- | --- |
| R_T | J1 pin 3 | NTC R4 to ground | R2 10 kΩ to 3V3 | GPIO0 (ADC1 ch 0) |
| R_P | J1 pin 4 | LDR R3 to ground | R1 10 kΩ to 3V3 | GPIO1 (ADC1 ch 1) |

The clock board's own R1 and R2 pull up to **+5 V** and are left off;
only the NTC at R4 and the LDR at R3 are fitted. That matters because
the C3's ADC reads reliably only up to about 2.5 V at 11 dB
attenuation and the pin's absolute maximum is 3.6 V: from 5 V a
10 kΩ NTC would already sit at 2.5 V at room temperature. From 3.3 V
it gives 1.65 V at 25 °C, 2.5 V near 0 °C and 1.1 V at 40 °C, so a
room never clips, and a dark LDR clipping at the top is harmless.
GPIO0 and GPIO1 are also the right pins for another reason: only
ADC1 (GPIO0–4) works with Wi-Fi on, GPIO3 and 4 are the buttons,
GPIO5 is the buzzer on ADC2, and GPIO2 is a strapping pin that a
divider at mid-rail could pull the wrong way at reset. Neither GPIO0
nor GPIO1 is a strapping pin on the C3. Section 4's pin table and
CLAUDE.md's pin facts gain both lines.

The NTC's part number is not in the reference; 10 kΩ at 25 °C with
B = 3950 is what these kits ship, and the conversion gets a
`NTC_BETA` build-time override plus a one-point offset setting if
the user's thermometer disagrees.

**Sampling.** `sensors.rs` holds the pure part: millivolts to tenths
of a degree by the B equation in fixed point (a 64-entry table over
the usable range with linear interpolation, no floats), and
millivolts to a 0–100 light index. One thread-mode `sensor_task`
owns the `Adc<ADC1>` converter and both `AdcPin`s, because
`read_oneshot` borrows the converter mutably and the pins are typed
to it; ADC1 is a single converter behind a five-way input mux, so
the two channels are read one after the other, each conversion a
few tens of microseconds. The task reads both once a second through
esp-hal's oneshot ADC with the line calibration scheme (`AdcCalLine`,
supported on the C3), averages 8 samples per reading, keeps an
exponential average of about 10 s, and publishes temperature, light
and the dark flag into shared cells with this task as the single
writer (load-and-store like the counters); the display task and
`http::status` only read them. The scan is untouched: ADC reads are
microseconds in thread mode.

**Temperature on the panel.** A setting `temp_show` (off, every 30 s,
every 60 s) shows the temperature for 3 s between clock faces: two
digits, the degree mark and `C` or `F` (`temp_unit`) in the last
digit, matching what the stock firmware did with the same mark. The
degree mark doubles as the armed indicator in the clock face; that
stays, the temperature face is distinct enough. Below −9 °C the face
shows `-9`; the status keys carry the real value. The `Ui` gets a
`Temp` face driven by the tick, not a mode, so presses keep their
clock-mode meaning during it.

**Light on the brightness.** One boolean setting, `dim_in_dark`,
labelled "Dim in the dark" on the page (key `dim`). When it is on
and the room is dark, the panel runs at the lowest brightness step;
otherwise it runs at the brightness the user chose. Dark is the
10 s light average below `DARK_BELOW` and it ends when the average
rises above `DARK_ABOVE`, two constants in `config.rs` a comfortable
gap apart (a room light switching on has to clear the gap, so a
candle or a passing car does not flip it), with the first numbers
guessed and tuned on the panel from the heartbeat millivolts. The
user's brightness step is untouched by all this: the bottom short
press still cycles it, the record stores it, and the display task
substitutes the lowest step at draw time while dark is set. A user
who has chosen the lowest step sees no difference. The current step
`IS` still comes from the step actually drawn, cap and all.

**Exposed.** Read-only `temp_c` (tenths, signed), `temp_f`, `light`
(0–100) and `dark` (true while the panel is dimmed) in `/api`, and
the same on the web page: the status line grows a second line,
`temperature 23.4 °C (74.1 °F), light 62 %, dimmed`,
with `no sensor` for a reading pinned at either rail (an unfitted
NTC reads the pull-up, an unfitted LDR reads ground), so a board
without the parts says so instead of showing nonsense. They go
through `form::Status` like the uptime, so `form.rs` stays free of
hardware. The heartbeat logs the raw millivolts for calibration.
Page-size check again.

**Record.** `RECORD_VERSION` 7 (or 6 if M10 slips): `temp_show`,
`temp_unit`, `temp_offset`, `dim_in_dark`.

**Touches.** `sensors.rs`, a `sensor_task` in `display.rs` or its own
file, `main.rs` (ADC1 and the two pins), `settings.rs`, `form.rs`,
`ui.rs` (the face), `display.rs` (the dark substitution), `panel.rs` (the `A`, `P`, `C` and `F`
glyphs are already in the font), CLAUDE.md pins table.

**Host checks.** The B-equation table at 0, 25 and 40 °C against a
calculator; the dark flag never chatters on a noisy light level
hovering at either threshold; the `Temp`
face layout through `Panel::render`.

**Verify on the panel.** Heartbeat millivolts near 1650 at room
temperature with the NTC in free air; the status line within 1 °C of
a thermometer after the offset; with the setting on, a hand over the
LDR drops the panel to the lowest step within about 10 s and
uncovering restores the chosen step; lights off in the evening dims
it once and it stays dim until morning, with no flicker at dusk.

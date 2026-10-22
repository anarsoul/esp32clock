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
- **Page buffer headroom.** The worst-case page plus headers is 2409 of
  the 2560-byte `RESPONSE_BUF`. One more select or a longer label tips
  it into the 500 path. Raise it to 4096, about 4.5 KB more across the
  three listeners.
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

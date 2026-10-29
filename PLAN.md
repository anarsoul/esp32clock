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
| `melody` | alarm sound: `beeps`, `twinkle`, `ode`, `frere` (M8), `morning`, `westminster`, `reveille`, `elise`, `jingle`, or `random` for a different tune each time |

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
   the 6 Mbit/s decoding limit with margin. Confirmed 2026-10-23 on the
   real clock in its case after 24 h on `881d786`: the AP reads it at
   -74/-76 dBm (two chains), 0 heals, a 1400-byte ping from the host
   5 to 12 ms with none lost, the page in 28 ms. The clock's uplink sits
   at 6 Mbit/s when idle and the rate control moves it to 13 Mbit/s
   (MCS 1) under a 100 packets/s flood of 1400-byte pings, the second
   chain dipping to -81 meanwhile; the AP sends to the clock at MCS 5.
   The smoltcp DHCP lines never appeared:
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

### Review backlog, second pass (2026-10-23)

A whole-tree review of the code as of commit 0e85ee9, read in four
slices (net/http/ota/heal/main; ui/display/clock/carry/buttons/buzzer/
melody; settings/storage/form/config; scan/panel/aip33628) against
the registry sources where a claim depended on them (esp-hal 1.2.2,
embassy-net 0.9.1, smoltcp 0.12, embassy-sync 0.7.2, sequential-storage
8.0.2, esp-bootloader-esp-idf 0.6.0, sntpc 0.11). Every item below was
confirmed by reading the code path; none is fixed yet. What checked out
and is not listed: the frame and transfer arithmetic and the two-buffer
hand-off in `scan.rs`; the panel tables against the reference docs and
every glyph in use; the record decode of every version; the flash handle
never held across an await; the HTTP head parser on partial reads and
odd headers; the alarm-age arithmetic across midnight and the 12-hour
display; the carry record's sign and 2106 limits; the buzzer's
open-drain path on every exit; the rollback count; the NTP validation
in sntpc; the library's refusal to pick the booted slot for an upload.

Bugs, in rough order of value:

- **Self-heal is unreachable once the lease expires on a dead uplink.**
  `probe_task` starts every iteration with `stack.wait_config_up()`
  (`net.rs:328`). When the uplink dies with less lease time left than
  the five-minute budget, smoltcp's DHCP client resets at `expires_at`
  and embassy-net clears the address (`lib.rs:907-909`); from then on
  no probe is sent, the streak is frozen below ten, the link is still
  up so `wifi_task` sees nothing, and the connect budget needs connect
  failures that never come. The clock sits with the carried time and
  no page until power is cut. With a one-hour lease that is about one
  uplink death in twelve. Fix: bound the wait (`with_timeout(PROBE_PERIOD,
  wait_config_up())`) and, once armed, count a period with the link up
  and no address as a failed probe.
- **The sweep's trailing dash is dark and blinks the armed mark.**
  `display::dimmed` scales components by 64/256, so none exceeds 63,
  and `GAMMA[0..=76]` is 0: `Levels::from_rgb(dimmed(c))` is (0,0,0)
  for every palette colour. `set_digit_color(tail, …)` therefore sets
  the whole tail block to level 0, annunciator included: the dimmer
  dash of the M4 design has never been visible, and when the tail is
  block 3 the degree mark lit for an armed alarm goes dark one step in
  four, a visible blink before the first sync. Fix: dim in level space
  (halve the head's `Levels`, floor 1) and apply it to the dash
  segments only, not the block.
- **`POST /play` during an alarm silences it.** `buzzer_task`'s `Ring`
  loop breaks on any command and the `Preview` arm plays once and then
  waits for the next command; the display re-issues `ring` only on the
  ringing edge. One preview, from a browser tab left on the page, say,
  leaves the panel flashing in silence for the rest of the 120 s. Fix:
  ignore `Preview` while ringing, or go back into the ring loop after
  it.
- **A `POST /` without `Content-Length` disarms the alarm.**
  `parse_head` defaults the length to 0 and nothing records whether the
  header was present, so a chunked or bodiless post is an empty form;
  `form::apply` with `checkboxes_absent_off` then clears `alarm_armed`,
  `twelve_hour` and `skip_next` and returns `Ok`, which is committed and
  answered with a 303. Not reachable from the clock's own page;
  reachable by anything on the LAN by accident. Fix: refuse a `POST`
  without `Content-Length` (411) or with `Transfer-Encoding` (400).

Gaps:

- **No watchdog.** `esp_hal::init` disables the SWD, RWDT and both
  timer-group watchdogs and nothing re-enables one. A hang that is not a
  panic (a lockup in the Wi-Fi driver, a critical section waiting on
  something, an infinite loop in a freshly uploaded image) leaves the
  panel dark until someone opens the case, and the OTA rollback counts
  only software resets, so a new image that hangs is never rolled back,
  contrary to `ota.rs`'s header. Fix: enable the RWDT with a few-second
  timeout fed from the heartbeat or the display tick, and keep
  `ota_boots` in `carry::init` on a watchdog reset reason.
- **A heal reset can land mid-transfer during a failing upload.**
  `ota::install` has stopped the scan; a probe or connect budget then
  calls `heal::reset`, whose `scan::stop` returns at once (already
  halted) and which waits 200 ms before `software_reset`. If the
  upload's body read fails in that window (likely, since the same dead
  link is what trips both), `install` calls `scan::resume` and the
  reset lands with a transfer in flight, the exact case the stop
  exists for. One `STOP` flag cannot represent two stoppers. Fix: a
  latched reset-pending flag that `resume` honours, or a stop count.
- ~~**A record from a newer image throws every setting away on rollback.**~~
  Done as M10 step 0, 2026-10-29. `from_record` returned `None` for `version > RECORD_VERSION`, and
  `is_valid` fails the whole record for a `color` or `melody` index this
  build does not know; only `brightness` is clamped. An OTA image with
  record version 6 (M10 plans exactly that) or one more melody saves a
  record, the `OTA_BOOT_LIMIT` rollback or a manual re-flash boots the
  older image, which comes up with hostname `clock`, timezone 0 and the
  alarm disarmed, and its first change writes a version-5 record over
  the newer fields for good. Fix: versions are append-only, so accept a
  newer version's known prefix and clamp `color` and `melody` the way
  `brightness` is.
- **The carry record has a mark but no layout.** `decode` trusts word
  positions behind `MARK` alone, and `ota::reboot` writes the record in
  firmware A for firmware B to read after `CoreSw`. The record has grown
  twice and M10 adds `boot_ap`; a reorder or insertion makes B misread
  a stale word as `ota_boots`, `handled` or the time offset. Fix: fold
  `WORDS` or a version into the mark and treat a mismatch like a missing
  mark.
- **A PIN issued while ringing or in alarm-set is dropped, not deferred.**
  `ui.show_pin` acts only in the clock, IP and PIN modes, but the
  `Signal` is consumed either way and `ota.rs:218` logs "shown on the
  panel" regardless; the user at the update page has nothing to type
  until they reload. Fix: keep the pending PIN in `Ui` and show it when
  the mode returns to clock within `PIN_TTL_MS`, or log that it was not
  shown.
- **The missed-alarm grace is used up by the first time in any mode.**
  `had_time |= time.is_some()` sits outside the mode match while the
  grace check runs only in clock mode. A sync that lands during the IP
  display or a PIN at boot marks the boot as had-time and the alarm
  that fell in the outage never rings. Same fix as the known
  "alarm lost while in another mode" item: evaluate the due check, or
  at least `had_time`, in every mode.
- **The connect budget is cleared by an association that does not
  survive.** `failures` and `first_failure` reset on any `connect_async`
  success. An access point that associates and drops the station before
  DHCP finishes (MAC filter, client isolation, a flaky AP) gives a loop
  of connect-ok, disconnect, retry in which neither budget fires and the
  probe never arms. Fix: clear the budget only once DHCP has an address
  or the connection has lasted some tens of seconds.
- **A failed save is never retried.** `persist_task` logs an `Err` from
  `store_item` and goes back to waiting for the next change, so the
  unsaved settings live only in RAM until something else changes. Fix:
  re-arm the settle timer on an error.
- **A reset inside the settle window loses the last change.** Changes
  reach flash two seconds after the last one; `heal::reboot` carries
  the time and alarm state but not the settings, so a colour or arming
  change made just before the probe budget fires is gone after the
  reboot with no indication. Reproduced 2026-10-28: `POST /api
  color=green` then `scripts/reload.sh` at once comes back with the
  old colour. Fix: have `heal::reboot` flush the persist task (or save
  synchronously through `with_flash` after `scan::stop`).
- **The upload does not check the chip id.** The first sector is checked
  for the image magic, the descriptor magic and the hash flag, not for
  the ESP32-C3 id at bytes 12–13 of the extended header. An image built
  with the wrong `--chip` is written, selected and booted; the
  bootloader falls back, and the old image then logs that it is itself
  on probation. Recovery works; the first failure is confusing. Fix:
  refuse unless `sector[12..14] == [0x05, 0x00]`.

Nits:

- **The display task freezes during an upload.** With the scan halted
  the scan task does not swap buffers, so the second `publish` after
  `stop` blocks in `FREE.receive()` for the whole 11 s write: no tick,
  no button handling, a ringing alarm cannot be silenced, and after a
  failed upload the first transfer shows the image queued at the stop.
  Fix: have `publish` drop or overwrite the image while halted.
- **`scan::stop` is not honoured on the DMA start-failure path.** The
  `Err` branch sleeps and `continue`s past the `STOP` check, so if the
  start kept failing `heal::reset` and `ota::install` would spin for
  ever. Unreachable with esp-hal 1.2.2's constant error conditions;
  a `STOP` check before the retry keeps the invariant.
- **Oversized or unparseable requests get a TCP reset, not a status.**
  `read_request` returns `None` for a head over `REQUEST_BUF`, a body
  over `BODY_MAX` or a bad request line, and the socket is closed with
  bytes pending. Answer 413/400 after a bounded drain, as `/ota` does.
- **First change on an empty flash writes the default record.** With
  `stored == None` the compare in `persist_task` is skipped, so ticking
  "skip the next alarm" on a fresh board costs a flash write for a field
  the record does not hold. Compare against the defaults when nothing
  was loaded.
- **A persistent `set_config` failure loops outside the connect budget.**
  The error path at `net.rs:162-166` sleeps and retries without touching
  `failures`. Static errors are caught earlier, so unlikely to matter.
- **`FIRMWARE_VERSION` goes into JSON and HTML unescaped.** It is
  `git describe`, so a tag with `"` or `<` breaks `/api` and the page.
  Sanitise in `build.rs`.
- **Silent fallbacks in the panel tables.** `com_index` returns slot 0
  for a value not in `COM_SEQ`, and nothing ties `GAMMA[255]` to
  `MAX_LEVEL`. Two `const` asserts lock in what is verbatim today.
- **In the ringing off-phase the degree mark stays lit.** `set_degree`
  runs before the match, so the flash is digits-off, mark-on. Cosmetic.
- **Stale names.** `ota.rs:12` and `carry.rs:49` refer to
  `ota::check_rollback`, which is `ota::init`; `http.rs:29` says an
  upload is 800 KB where the image is about 680 KB; the M10 text said
  two `RECEIVERS` where there are three (fixed in place).

### Review backlog, third pass (2026-10-27): more than a clock needs

The brief for this pass was over-engineering. The panel shows HH:MM
and a minute of drift is acceptable, so anything that works below a
second, and anything that was instrumentation for a problem since
solved, is a candidate. Read against the tree at commit 7372f3d, every
file. Nothing here is a bug; each item says what goes and what, if
anything, is lost. None is done yet. What checked out and is not
listed: the self-heal and connect budgets (each one measured in the
dropout campaign), the persist debounce (flash blink), the scan stop
for a reset or an upload, the upload drain (the `403` in the browser),
the `RefCell` in `settings::update` (`send_if_modified` wants a `Fn`),
and the buzzer's per-note timer (the LEDC API).

In rough order of value:

- **Sub-second time on a minute display.** The clock is an `i64` of
  microseconds; `query_ntp` adds the NTP fraction, half the round trip
  and the microseconds since the reply landed (measured in the same
  function, so zero); `restore_carried` adds `RESET_PAUSE` back, which
  makes "every deliberate reset path must wait exactly this" an
  invariant across `heal`, `ota::reboot` and `ota::init`; the carry
  record spends two words on it. All of it is well under a second. Fix:
  keep the offset in whole seconds (`unix_s - uptime_s`), set it from
  `result.seconds` alone, carry one `u32` word of seconds (good to
  2106, like `last_heal_unix`), drop the pause add-back (a reset then
  costs the pause, 200 ms, once) and key `until_next_second` to the
  uptime, which is where the whole seconds tick anyway. About 25 lines
  and one carry word go; the remaining error per sync or reset is under
  a second.
- **Colour through 8-bit RGB and two lookup tables.** The palette is
  eight `Rgb` values whose components are 0, 96 or 255; `GAMMA[256]`
  turns those three inputs into levels 0, 1 and 7; `BRIGHTNESS_STEP[101]`
  turns five percentages into steps 1, 2, 5, 8 and 15. The sweep's
  `dimmed` scales in RGB space, which is why its tail has never been
  visible (second-pass bug). Fix: make `PALETTE` an array of `Levels`
  (red is `(0, 0, 7)`, orange `(0, 1, 7)`, in blue-green-red order),
  make `BRIGHTNESS_CHOICES` pairs of percent and step, delete `Rgb`,
  `GAMMA`, `Levels::from_rgb`, `BRIGHTNESS_STEP` and
  `current_step_for`, and dim the tail by halving levels with a floor
  of 1, which fixes the sweep bug by construction. About 60 lines go;
  `Panel::set_color` takes a `Levels`. The page keeps its colour names.
- **Instrumentation from the dropout campaign.** Four pieces measured a
  problem that is now understood and healed: the per-listener `State`,
  `Listener`, `LISTENER` and `log_status` in `http.rs` (70 lines in the
  heartbeat, "the shape of a wedged server"); `net::monitor_task` (link
  and address transitions, which the Wi-Fi task and DHCP already log);
  `net::log_candidates`, a second two-second scan before every connect
  purely for a log line, on top of the one `ScanMethod::AllChannels`
  makes; and `scan::GapStats` with its critical section after every
  transfer, which settled the shimmer question in M3 and has read a few
  microseconds since. Fix: delete them, keep the probe counters (they
  drive the self-heal and the heartbeat still says "failing now") and
  the minute RSSI line (the documented way to read the link). About 140
  lines, one task, one `StackResources` slot and two seconds per
  connect go. Whether the serial log is still being watched for these
  is the user's call; `scripts/watch-clock.sh` covers the other side.
- **Dead state.** `ota::SELECTED` is stored at `init` and never read
  (the log line uses the local); `carry::SOFT` and `soft_reset` have no
  caller; `clock::is_synced` neither (first-pass item). With the panel
  methods of the first pass (`set_time`, `set_segment`,
  `set_position_color`, `set_dash`, `Levels::OFF`, `IS_MA_TENTHS`,
  `SLOT_US`) that is about 50 lines the lib crate hides warnings for.
- **Nits.** `PinState::check` compares a one-attempt, four-digit PIN in
  constant time; `presented.parse::<u16>() == Some(pin)` with a length
  check is the whole rule. The three `try_changed` calls in
  `display_task` save one loop iteration whose render equals `last` and
  publishes nothing; deleting them, rather than adding one to the
  hostname task as the first pass suggested, is the consistent fix.
  `Hostname::DEFAULT` spends fifteen lines validating the literal
  `"clock"` at compile time; `Hostname::new` at `settings::init` with
  an `expect` does the same. `http::Counting` exists so a refusal knows
  what to drain; `ota::install` could return the bytes it consumed in
  the `Refusal` instead.

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

### M9 — OTA updates — DONE, verified on the panel 2026-10-22

**Why first.** The clock lives in its case with no USB reachable, and
every fix since M8 has meant opening it. An update over the LAN turns
that into a `curl`.

**Flash layout.** The 4 MB flash today carries espflash's default
table: `nvs` at 0x9000 (24 KB), `phy_init` at 0xF000, one `factory`
app at 0x10000 of 4032 KB. The release application image is 652 KB
(`espflash save-image`; the ELF's text is 816 KB before the flash
segments are packed), so two slots fit with room to double. New
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
runner line also carries `--erase-parts otadata` on every USB flash:
all-ones `otadata` with no `factory` boots `ota_0`, which is where
espflash puts the image, so a USB flash always boots what was just
flashed even when the last OTA had switched the clock to `ota_1`.
The bootloader is espflash's prebuilt one, and the crate docs warn it
"might not" support OTA. Its binary (the first 32 KB of a
`save-image --merge`) carries the strings of ESP-IDF's `otadata`
selection: "No factory image, trying OTA 0", "Set actual ota_seq=%lu
in otadata[0]", "Loaded app from partition at offset 0x%lx", so the
slot switch should work. It is still the first thing to verify on the
panel; if the boot log keeps loading from 0x20000 after a switch, the
fallback is building the bootloader from ESP-IDF and pointing espflash
at it with `--bootloader`.

**Token.** The upload is the one request that can brick the clock or
replace its firmware with someone else's, so it is not as open as the
settings form. A token comes in at build time, `OTA_TOKEN`, like the
Wi-Fi credentials, and `POST /ota` must carry it as
`Authorization: Bearer <token>`; anything else is `403` before a byte
of the body is read or the flash touched. The comparison is constant
time (a fold over the XOR of both strings, lengths included) so a
timing side channel cannot leak it. A firmware built without
`OTA_TOKEN`, or with one shorter than `config::OTA_TOKEN_MIN` (8)
characters, has no OTA at all: every `/ota` is `403 ota disabled`, and
the boot line says so, so a forgotten variable is noticed at the first
flash and not at the first update. The token is compiled in as a
string; anyone with the image file can read it out, which is the same
standing as the Wi-Fi password.

**Upload.** `POST /ota` with the raw application binary as the body
(`espflash save-image --chip esp32c3 --partition-table partitions.csv
<elf> app.bin`, then `curl -H 'Authorization: Bearer …'
--data-binary @app.bin http://<ip>/ota`). The head is parsed as today
plus the `Authorization` header; the body is streamed, not buffered.
`read_request` returns the body as `Body::Inline` for the small
requests and as `Body::Stream` (whatever followed the head in the
request buffer, plus the total) for `/ota`, and the listener reads the
rest of the socket in chunks into `ota::Update`, which fills a 4 KB
sector buffer and, per full sector, erases, writes and reads back the
sector in the slot that `OtaUpdater::next_partition` hands back
(`esp_bootloader_esp_idf::ota_updater` chooses the slot and switches
`otadata`; the sectors are written with `esp-storage` directly at the
slot's absolute offset). The flash is the one handle in the firmware,
`storage::with_flash`, taken for one blocking operation at a time and
never across an await, so the settings map and the updater cannot
interleave inside an operation. The first attempt gave the updater a
second `FlashStorage` on a stolen `FLASH` peripheral, and on the panel
it could not read the partition table: `FlashStorage::new` sizes the
flash with a chip-ID command over SPI1, and once the first handle has
used the flash that read returns 0 (logged `capacity 0 bytes`), so
every bounds check on the second handle fails.
`Content-Length` is required, at least a sector and at most the slot
size; the first sector is checked for the image magic (0xE9), the
app descriptor magic (0xABCD5432 at offset 0x20) and the
hash-appended flag (byte 23) before anything is erased, and the
descriptor's version string is logged and returned. The body is
hashed as it streams (`sha2`, software) and the last 32 bytes, which
espflash makes the SHA-256 of the rest, must match: that is the only
thing that tells a truncated file sent with its own honest length
from a whole one, and it catches a corrupt byte too. A body that ends
short, a hash mismatch, a read-back mismatch or a flash error leaves
the slot half written and `otadata` untouched, so the running image
stays the boot image, and the answer is `4xx`/`5xx`. On the last
sector the handler calls `activate_next_partition`, answers `200`
with the slot name, byte count and version, closes the socket, and
reboots through `ota::reboot`: the time is carried as for a heal, the
carry record's `ota_boots` is set to 1 and the chip resets. One
upload at a time: the updater sits in an `embassy_sync` mutex and a
second listener that cannot `try_lock` it answers `503`.

**The scan during the write.** Flash writes run with interrupts off,
and 830 KB is about 200 sector erases of tens of ms each: with the
scan running the panel would flicker dark at a few hertz for the
whole upload. The upload therefore starts, once the head and the
first sector have passed every check, by stopping the scan through
`scan::stop` (the transfer ends on its blanking frame, same as the
heal path), so the panel is dark from the first erase to the reboot.
That is the honest display for "do not unplug". A failed upload
calls `scan::resume`, new in this milestone: the scan task no longer
pends forever after stopping but waits for the resume signal, and
`stop` is idempotent (it polls a halted flag rather than a one-shot
signal), so a self-heal firing during an upload cannot hang on the
second stop. Expected wall time: the TCP side over Wi-Fi is the
limit; `http::RX_BUF` goes from 1 KB to 4 KB on all listeners (9 KB
of static RAM, taken from the 120 KB main stack) because it is the
receive window. Measure it. Wi-Fi frames arriving while interrupts
are off wait in the radio's receive ring; TCP covers any that spill.

**Rollback without bootloader support.** The bootloader falls back
to the other slot on its own only if the selected image fails its
checksum. A new image that boots and then panics or loses the network
is on the firmware. The `carry` record gains an `ota_boots` word:
set to 1 by the upload, read at boot by `ota::check_rollback` before
anything else is spawned: zero means nothing pending; at or above
`config::OTA_BOOT_LIMIT` (3) the previous slot is activated again
with `activate_next_partition` (two slots, so "next" is "previous"),
the word is cleared and the chip resets, with the scan not yet
started so no transfer is in flight; otherwise the word is bumped.
`ota::confirm` clears it when the new image reaches "good": the first
gateway probe answered, or the first HTTP request served, so a board
with a dead router can still be updated. A power cut zeroes the carry
record, which is the right default: the user pulled the plug, the
user decides. So: two software resets of an unconfirmed image and the
third boot runs the old one.

**Visible state.** `build.rs` exports `git describe --always --dirty`
as `FIRMWARE_VERSION` (truncated to the descriptor's 31 characters)
and `main.rs` feeds it to `esp_app_desc!`, so the image file carries
it too. `/api` gains the read-only keys `version` and `slot`
(`ota_0`/`ota_1`, or `factory`/`?` on an old table), the page status
line shows both, and the boot line logs them with the selected slot,
which differs from the booted one only after a bootloader fallback.
`ota::init` reads the partition table once at boot for the slot name
and whether OTA is possible. The page-size check on the host runs
again. `scripts/ota.sh <ip> [app.bin]` builds the image and posts it
with `OTA_TOKEN` from the environment.

**Touches.** `partitions.csv`, `.cargo/config.toml`, `build.rs`,
`src/config.rs` (`OTA_TOKEN`, `OTA_TOKEN_MIN`, `OTA_BOOT_LIMIT`),
`src/ota.rs` (boot-time slot lookup and rollback, token check, the
sector writer, activate, reboot, confirm), `src/http.rs` (the
`Authorization` header, `Body::Stream`, the `/ota` handler, `RX_BUF`),
`src/scan.rs` (`resume`, idempotent `stop`), `src/carry.rs`
(`ota_boots`), `src/net.rs` (confirm on the first probe answer),
`src/form.rs` (`Status` version and slot), `src/bin/main.rs`
(rollback check, descriptor version, boot line, the second flash
handle), `scripts/ota.sh`, this file and `AGENTS.md`.

**Host checks** (scratch crate, 2026-10-22; `form.rs`, `settings.rs`,
`melody.rs`, `panel.rs`, `aip33628.rs` and `config.rs` included by
path, the head parser, the token and sector functions and the carry
codec copied in): the head parser takes a `Content-Length` of 831488
and the `Authorization` header in either case, strips the query,
refuses a non-numeric length and does not treat `GET /ota` or
`POST /otax` as uploads; the token matches only itself (not empty, a
prefix, one byte longer, first or last byte off, `Basic`, or bare);
the sector loop reproduces an image that is a sector multiple, one
with 1001 bytes over, one three bytes over (padded to a word with
0xFF), a single sector, and one where the request buffer already held
more than a sector, which caught a bug: `fill` dropped what was left
of the prefix past the first sector; a body that ends short or is
empty is refused. The carry record round-trips with `ota_boots`, and a
zero word means nothing pending. The worst-case page (31-character
version, `factory`, 24-character hostname, every notice) is 2487
bytes, 2590 with headers, of 4096. Sizes: the application image is
652 KB, 33 % of a slot, with the version in its descriptor at 0x30
and the hash-appended flag set; static RAM grew by 11 KB for the upload buffers and 9 KB for
the receive windows, leaving a 98 KB main stack; the buffers sit in
a `StaticCell` so they are zeroed RAM, not image.

**Seen on the bench** (board on `/dev/ttyACM0`, 10.1.1.31, AP
`esptest`, 2026-10-22). The USB flash with the new table: the
bootloader lists `otadata`, `ota_0`, `ota_1` and loads from 0x20000;
the boot line says `booted from ota_0, otadata selects ota_0, updates
enabled`; saved settings survived the repartition. The first attempt
with a second flash handle failed as described above and was
replaced by the shared one. Then, in order:

| post | answer | time | after |
| --- | --- | --- | --- |
| no header, wrong token | `403 bad token` | 25 ms | nothing in the log but the refusal |
| `GET /ota` | `404` | | |
| whole image, 672544 bytes | `200 … written to ota_1` | 10.4 s | reboot, `Loaded app from partition at offset 0x200000`, time carried, `confirmed good` at the first probe 5.6 s up, `/api` says `ota_1` |
| whole image again | `200 … written to ota_0` | 10.8 s | back on `ota_0` |
| 300 KB of the file with the full length claimed | reset by peer | 9.7 s | `body read failed` after the 5 s socket timeout; the timed-out socket cannot carry the `400`; running image stays, scan resumed (the heartbeat's transfer count dipped for that minute and recovered) |
| 8 KB of `/dev/urandom` | `400 not an application image` | 35 ms | |
| two uploads at once | second gets `503 an upload is in progress` | 0.1 s | |
| 300 KB of the file with its own length | `400 image hash mismatch` | 5.1 s | written to the other slot, not selected |
| the file with one byte flipped | `400 image hash mismatch` | 10.8 s | same |
| an image that panics 1 s after start | `200`, reboot | | `boot 1 of 2`, panic, `boot 2 of 2`, panic, `not confirmed after 3 boots, switching back`, `Loaded app from partition at offset 0x200000`, old image `confirmed good` |

So an upload runs at about 65 KB/s with the 4 KB window, flash
writes included; the hash costs nothing visible. The user confirmed
both paths on the panel, from the browser and from the script. One gateway probe
timed out during an upload (interrupts are off for each erase) and
was answered next time; at ten failures in a row for a heal, an
upload cannot come close. A reset reason of `CoreSw` carries the
record through the panic and the rollback alike.

### M9b — upload from the page — DONE, verified on the panel 2026-10-22

**Scope.** `GET /update` serves a page with a file input, a token
field, a "remember the token in this browser" checkbox, an Upload
button, a progress bar and a status line. Its script posts the file
itself as the body of `POST /ota` with the `Authorization: Bearer`
header through `XMLHttpRequest`, so the server side is the one the
curl path already uses: no multipart, no new parser. A plain HTML form
would wrap the file in multipart boundaries and need the image length
dug out of the wrapper on the device; the script avoids all of that.
`XMLHttpRequest` rather than `fetch` because it reports upload
progress. Same origin, so no CORS preflight; the browser's request
head fits `REQUEST_BUF`. On `200` the page waits 5 s and polls `/api`
every 2 s, up to 20 times, until the clock answers again, then shows
the version and slot it came back with; a refusal shows the status
and text. The token is kept in `localStorage` only when the box is
ticked, in clear, same standing as the shell history of a curl.

**Status line.** The firmware line moves out of the uptime line onto
its own: `Firmware <version> in <slot>. Update.` with the link, then
`Up …. Self-heal resets …` as before. `/api` is unchanged.

**Script.** `scripts/ota.sh` with no argument builds the image file
and stops, printing its path, for the browser path (`-` as the host
does the same).

**Touches.** `src/form.rs` (`write_update_page`, the split status),
`src/http.rs` (`GET /update`), `scripts/ota.sh`, `AGENTS.md`.

**Host checks.** Both pages' sizes against `RESPONSE_BUF`; the page's
script syntax-checked with `node`.

**Host checks** (2026-10-22). Pages with the longest version, slot and
hostname and every notice: settings page 2541 bytes, 2644 with
headers; update page 2277 bytes, 2380 with headers; both against the
4096-byte buffer. The page's script passed `node --check` and a run
under node with a mocked DOM, `XMLHttpRequest`, `fetch` and
`localStorage`: the request is `POST /ota` with the bearer header and
the file as the body, the button locks and the bar shows, progress
updates, a `200` leads to the polling and "Back up: firmware v2 in
ota_1", a `403` shows `403: bad token` in red, no file is refused
before any request, and the token is stored only with the box ticked.

**Seen from a browser** (headless Chromium driven over the DevTools
protocol against the clock, file set through `DOM.setFileInputFiles`):
the bar went from 0.19 at 1 s to 1.0 at 10 s, the line read
`updated: … written to ota_0, 680704 bytes; rebooting Waiting for the
clock to come back.` at 11 s and `Back up: firmware … in ota_0.` at
18 s, with `/api` agreeing and the token in `localStorage`. With a
wrong token the browser got the `403` and the page said
`403: bad token` within a second, even though the server closes
without reading the body. The trailing newline of the server's text
showed before "Waiting" and is trimmed now.

### M9c — a PIN on the panel for the update page — DONE, verified on the bench 2026-10-22 (serial log, curl and a browser; the panel itself not watched)

**Why.** The owner of a clock does not build firmware: they get an
image file and have a browser, on Windows or a phone. A token baked
into the image is public once the image is, and one they must be told
and type is a nuisance. What every owner has is the panel. So the
page proves possession of the clock: opening `/update` makes the
clock draw four random digits, show them on the panel and log them,
and the page asks for them. The script keeps the build-time token,
for the bench and for updates out of sight of the panel; a build
without `OTA_TOKEN` refuses the script and still serves the page.

**The PIN.** `GET /update` calls `ota::issue_pin`: four digits from
the hardware RNG (`Rng::new().random() % 10000`), remembered with the
time of issue, sent to the display task over a signal and logged
(`ota: PIN 1234 shown on the panel`; the serial port is physical
access, and a headless board has no panel to read). The panel shows
the digits in one frame, colon off, for `ui::PIN_SHOW` (60 s) or
until any press; ringing and alarm-set take precedence, the PIN
display replaces the clock and the IP display. The upload sends
`Authorization: PIN 1234`. One attempt consumes the PIN, right or
wrong: a wrong one also starts `PIN_LOCKOUT` (10 s) during which the
page is served with a notice and no PIN; a PIN older than `PIN_TTL`
(5 min) is refused as expired. That caps guessing at one try per ten
seconds against 10,000 values, with the panel flashing a fresh PIN
every time, which is plenty for a LAN. The PIN is never in the page,
so a cross-site page learns nothing and still cannot send the header
without a preflight the clock does not answer. The PIN logic is a
pure `PinState` with a millisecond clock passed in, for the host
check; `ota.rs` wraps it with `Instant` and the RNG.

**The two schemes.** `authorize` accepts `Bearer <token>` only when
`OTA_TOKEN` was built in, and `PIN <digits>` whenever one is pending.
The boot line says which paths are open: "page with a PIN and the
script with the token" or "page with a PIN only (no OTA_TOKEN)".

**The page** loses the token field and the remember box and gains a
numeric PIN field; the text says to read the digits off the clock. A
page served during the lockout says so and that reloading gets a new
PIN after it.

**Touches.** `src/ota.rs` (`PinState`, `issue_pin`, `pin_to_show`,
the two-scheme `authorize`), `src/http.rs` (`GET /update` issues),
`src/ui.rs` (`Mode::ShowPin`, `View::Pin`, `show_pin`, timeout),
`src/display.rs` (draw, the signal in the select), `src/form.rs`
(the page), `AGENTS.md`.

**Host checks.** `PinState`: issue, match, consume, wrong attempt
and lockout, expiry, reload during the lockout; `authorize` with each
scheme; the page sizes.

**Host checks** (2026-10-22). `PinState` copied verbatim: nothing
pending, four digits from the random (leading zeros kept), a wrong
or short attempt uses the PIN up and locks, the lockout is reported
and ends, success uses it up, an old PIN is expired without locking,
a reissue replaces. `authorize_with` with each scheme and each PIN
error. The update page is 2252 bytes (2355 with headers) in both its
normal and lockout forms; the page script passed the node mock with
the PIN header and the four-digit check.

**Seen on the bench.** Boot line: `updates from the page with a PIN
and from the script with the token`. No header: `403 no
credentials`. `PIN 1234` before any page: `403 no PIN issued`. After
`GET /update` the log read `ota: PIN 3838 shown on the panel`; the
wrong PIN got `403 wrong PIN`, the page then said "Reload this page
in 10 s", the right PIN after that got `no PIN issued` (used up),
and a fresh PIN after the lockout uploaded 683 KB in 11.6 s with the
reboot and confirmation as before. From headless Chromium: the page
opened with the notice, the PIN read from the serial log went in,
the bar filled in 11 s, "Back up: firmware c5f7e81-dirty in ota_1"
at 20 s; a wrong PIN showed `403: wrong PIN` within a second and the
reloaded page carried the lockout notice with the seconds left. The
script's token path is unchanged.

**A refusal drains the body first** (found by the user in a real
browser, 2026-10-22). The clock used to answer `403` and close while
the browser was still sending the 680 KB body; the reset on the
leftover data reached the browser before the response, and the page
said "connection lost" where headless Chromium, on a faster path, had
seen the `403`. Now `http::upload` counts what `ota::install` took
from the socket and, on any refusal but `503` (another upload is
running and needs the air), reads and discards the rest, capped at
2 MB and 40 s, using the response buffer as scratch, before answering.
Without flash writes the body streams at about 200 KB/s, so a wrong
PIN costs 2 to 4 s and then shows as `403: wrong PIN` in the browser
and in curl alike; the right PIN went through as before. A client
that claims more length than it sends still ends in the socket
timeout and a reset, as recorded under M9.

### Brightness ceiling (after M9c) — DONE, verified on the bench 2026-10-22

Quick presses on the brightness button reset the board. A read of the
whole path (button channel, `settings::update`, the two-buffer DMA
hand-off, the debounced flash write under esp-storage's
interrupt-disabling lock) found nothing that could panic, and the
serial logs held no panic text. Capping the current step at 5 made it
rock stable; 75 % and 100 % (steps 8 and 15 before the cap) reset it.
The firmware had run at a ceiling of 11 since M3, and the resets began
when the transmit ceiling went to 16 dBm, so the board's supply is the
limit: a step from 10 % to 100 % is about 2.5 to 40 mA on every lit
sink at once, a transient of the order of an ampere on the 5 V rail,
with the radio now taking its share. `MAX_CURRENT_STEP` is a build-time
variable (`build.rs` reruns on it, a const assert holds it to 0..=15)
with the default at 5, which the user finds bright enough; a board with
a stiffer supply can be built with a higher one. The page then still
offered 75 % and 100 %, which drew the same as 50 %, so
`settings::BRIGHTNESS_STEPS` is now the prefix of the five choices
whose current step is within the ceiling, computed at compile time
(three by default); the button cycle, the page's select and the API's
accepted values follow it, and `from_record` moves a stored step the
build does not offer to the brightest one rather than dropping the
record. The record layout is unchanged. Not done, and worth
doing if the ceiling ever goes back up: slew the step one at a time
over a few frames so the brightness button changes the load gradually,
and log the reset reason prominently so a `SysBrownOut` is told apart
from a panic at a glance.

### Hold the self-heal while the alarm rings, and ring for five minutes (after the brightness ceiling) — built, not yet verified on the panel

The user asked whether a self-heal reset can land while the alarm is
ringing. It could: neither the probe task nor the connect loop asked
the UI anything, so a heal mid-ring blanked the panel, reset the chip
with the buzzer still driven until the reset itself, and relied on the
missed-alarm grace to bring the alarm back a few seconds later for a
fresh timeout, for up to four minutes of ringing in two pieces. Also
120 s of ringing is too short for the user.

Design. `display.rs` owns a `RINGING` atomic with the display task as
its one writer, set on the ringing edge before the buzzer is told to
ring and cleared after `record_handled` on the ending edge, so a reset
that goes ahead the moment the flag clears finds the alarm already
recorded as handled. The hold is in `heal::reset`, the one way to a
deliberate reset, so both triggers and any future one get it: while the
flag is set it logs once, polls the flag every second and goes on only
when ringing has ended, or after `ui::RING_TIMEOUT` plus 30 s, a bound
that keeps a flag stuck for any reason from blocking a heal for good.
The panic path (`custom_halt`) cannot wait and is unchanged. The probe
and connect tasks sit inside the hold, so no probes are sent and no
connects are attempted while an alarm rings on a dead network; the
reset follows anyway, as before, since the network state that earned
it is not re-examined. `RING_TIMEOUT` goes from 120 s to 300 s, and
`display::HANDLED_WINDOW_SECS` from 15 to 20 minutes to keep its margin
over the grace plus a full ring (10 + 5). `MISSED_ALARM_GRACE` stays at
10 minutes: an alarm cut off by a reset is at most five minutes plus a
boot old, well inside it. The `POST /play` item in the second-pass
backlog is unchanged by this, except that the silent flashing it leaves
now lasts up to five minutes.

Checked on the host: builds, clippy clean. The UI scratch crate from
the 2026-10-22 alarm-state check needs no change beyond the constant.

Verify on the panel: set the alarm a minute ahead, let it ring, and pull
the uplink (block the clock at the AP or power the AP down) so the probe
streak reaches ten while it rings; the log must show `heal: ... alarm
ringing, holding the reset` and the panel must keep flashing and
sounding until a press or five minutes, then `heal: ... self-heal reset`
and the reboot with no second ring. Then the plain case: an undismissed
alarm must stop on its own after five minutes.

### Carry the time across a reset as plain Unix time (after the ring hold) — verified on the bench 2026-10-28 at short uptime (five software resets, serial log); the hour-long soak still owed

**Problem.** The user saw the clock jump back nine minutes after a
self-heal. The carried time was Unix time minus
`Rtc::time_since_power_up`, and esp-hal converts the RTC tick count to
microseconds with a slow-clock period it measures afresh at every boot
(`esp_hal::init` → `calibrate_rtc_slow_clock`, 16 cycles of the
~136 kHz RC oscillator, kept in `STORE1`). The offset was written with
the old period and read back with the new one on the whole count since
power-up, so the error was `ticks_since_power_up × (P_new − P_old)`:
0.3 % after two days is nine minutes, either sign. The 2026-10-22
check ran at minutes of uptime, where it is under a second. NTP
corrected it seconds after Wi-Fi returned, but the alarm logic saw the
wrong time meanwhile (a ring twice or not at all around the step, a
wrong `unix` in the handled-window test). This was the first item of
the second-pass backlog.

**Design.** The user chose the simpler of two fixes. The raw tick count
could be carried and only the ticks of the reboot converted (exact to
milliseconds, three PAC register reads, since esp-hal's raw read is
`pub(crate)`); instead the record carries plain Unix microseconds and
nothing measures the reset. Error per reset is the bootloader's share
of the uptime at restore, well under a second, and repeated heals with
no NTP to correct them are not a scenario the user cares about.

- `carry::Carried.time_offset_us` becomes `unix_us`, same two words.
  The mark is now `"CAR" + layout version` (`0x4341_5202`): a record
  from the version-1 layout, left by `ota::reboot` on the firmware
  before this one, is discarded as if the mark were missing, so the
  offset cannot be read as a time. That one update therefore boots
  with `ota_boots` 0 and is not covered by the boot-count rollback, and
  loses the skip flag and the handled alarm once. This is half of the
  "mark but no layout" backlog item; the item stays for the word count.
- `clock::carry_time` writes `unix_us`; `clock::restore_carried` sets
  the offset to the carried time plus `clock::RESET_PAUSE` (200 ms), so
  the carried time is taken as the time at uptime zero less the pause
  every deliberate reset makes for its last log line. `heal::reset`,
  `ota::reboot` and the rollback in `ota::init` all wait exactly that
  constant. `clock::init` and the `Rtc` are gone; the rollback path's
  `carry_time`, a no-op before because it ran ahead of `clock::init`,
  now works, which strikes that backlog item.
- The display loop calls `carry_time` at the top of every iteration,
  at least once a second in clock mode, instead of the minute heartbeat.
  A panic reset is therefore at most a second plus the reboot stale,
  and the time learned from NTP is in the record within a second of the
  sync, which strikes the "not carried until the next heartbeat" item.
  The write is nine words of RTC RAM.

Checked on the host: builds, clippy clean. The record codec is unchanged
apart from the mark value and the field's meaning, so the 2026-10-22
scratch-crate round-trip still applies with the new mark.

**Verify.** Flash over USB (the carry is discarded on that reset
anyway), let it sync, leave it an hour or more, then force a heal (pull
the uplink, or block the clock at the AP); the boot line must read
`carried over Carried { …, unix_us: Some(…) }` and
`clock: time carried over the reset, local time HH:MM:SS` within a
second of the wall clock, and the first `ntp:` line after it must not
move the display. For the one OTA from the previous firmware, expect
`RTC carry-over discarded` on the `CoreSw` boot and no `ota: image …
boot 1 of 2` line; later OTAs carry as before.

### Three simplifications from the third review pass (after the carry) — DONE, verified on the bench 2026-10-28 (serial log and curl; the panel and buttons not watched)

Three items of the third-pass backlog, chosen by the user. The first
two change no behaviour; the third changes the carry layout and one
edge of the alarm.

- **The request head parsed once, into borrows.** `http::parse_head`
  used to return byte spans (`span_in`, pointer subtraction) so that
  the borrow of the head ended before the body was read into the same
  buffer, and `read_request` rebuilt the `&str`s through a closure. Now
  `parse_head` returns a `Head<'a>` of plain borrows: the method, path,
  query, `Authorization` value and `Content-Length`. `read_request`
  calls it twice: once before the body read, keeping only the two
  `Copy` facts that decide the read (is it `POST /ota`, how long is the
  body), and once after it on the head half of `buf.split_at`, to fill
  the `Request`. The second parse cannot fail, since it is the same
  bytes; the cost is one extra scan of a few hundred bytes. `span_in`,
  `is_upload` and the `field` closure are gone.
- **One timer in `Ui`.** `last_press`, `ring_started`, `ip_started`
  and `pin_started` were each the instant the current mode began. They
  are one field, `since`, set by `Ui::enter`, the one way `mode` is
  assigned outside `new`; `handle` also sets it on a press that stays
  in alarm-set, which is what that timeout wants. The IP view's octet
  index counts from the same instant it did.
- **The handled alarm is the time it last rang, carried.** Not ringing
  an alarm again after a software reset took `carry.handled: (unix,
  minute)`, `display::HANDLED_WINDOW_SECS` (20 minutes),
  `record_handled` at the ring's end and at a skip, the `seed_handled`
  block in the display loop, `Ui::suppress_alarm`, and `Ui::fired_minute`
  with its own clearing rule for the current minute. The user asked for
  the plain fact instead: `Ui::last_alarm` is the Unix time the alarm
  last rang or was used up by the skip flag, nothing clears it, and an
  occurrence is handled when that time is at or after the moment the
  alarm last came round (`t.unix` less the minutes behind and the
  seconds into the minute; `LocalTime` now carries its `unix`). Both
  due checks test that one comparison, so the alarm fires once per
  occurrence, a moved alarm time is a new occurrence, and the grace
  does not repeat a ring. The distance is computed whether or not the
  alarm is armed, so a disarm and re-arm inside the alarm minute does
  not ring twice, as before. The display task carries the value
  whenever it changes, like the skip flag, as `None` while ringing: a
  reset that cuts a ring off still brings it back through the grace,
  and one after the ring ended or the skip was used does not ring it
  again, now within the grace's ten minutes rather than a window of
  twenty. The carry record is eight words with the time as a `u32` of
  seconds like `last_heal_unix`, layout version 3; the one OTA from the
  previous firmware discards the record as the version-2 bump did.
  `alarm_age` is split into `minutes_since_alarm`, the distance and the
  weekday it fell on, and the armed test. About 40 lines and one carry
  word go.

Checked on the host: builds, clippy clean. The head parser is the one
the second pass read on partial reads and odd headers; its inputs and
outputs are the same, only the borrow shape changed. The alarm logic
was walked through on paper for: the alarm minute (rings once, a press
ends it, no second ring that minute); a skip (used up, not rung,
carried so a reset does not ring it either); a power cut over the
alarm (grace rings it on the first time after boot); a software reset
one to ten minutes after a ring ended (not rung again); a reset
mid-ring (rung again); the alarm moved to a later minute within the
grace (rings at the new minute); the next day (cleared hours before).

**Verify.** Flash over USB, then: open `http://<ip>/` and `/api`, post
the form once, open `/update` (the PIN shows for a minute, a press
ends it), hold both buttons for the IP (eight octets over 16 s), hold
the top button for alarm-set and let it time out after 15 s. For the
alarm: arm it a minute ahead, let it ring, press to stop, then force a
heal within ten minutes (block the clock at the AP); the boot line
must show `last_alarm_unix: Some(…)` and the alarm must not ring
again. Then arm it again a minute ahead and pull power during the
ring; after the reboot and sync it must ring (grace), and the serial
log must not show the "skip-next" or a stale `last_alarm_unix`.

**Result (2026-10-28, bench board over USB, commit 4d2dcbe).** The
parser: `GET /`, `/api`, `/?error=value` (notice shown), `/api?x=1`,
`/update` (PIN logged), `/nope` (404); `POST /api` with a good key
(200, JSON), a bad key (400), `POST /` with a bad value (303 to
`/?error=value`) and the full form (303 to `/`); `POST /ota` with a
wrong token (403 "bad token"), no header (403) and the logged PIN with
a ten-byte body (400 "body too short"), so the `Authorization` value
and the upload path are parsed as before; a `PUT` closes the socket.
The alarm: armed two minutes ahead with the skip flag, it was used up
at 13:39:00 with no ring; an OTA of the same image thirty seconds
later (a `CoreSw` reset) booted with `last_alarm_unix:
Some(1793219940)`, the display logged it as carried, the time came
over at 13:39:30 and NTP moved it by nothing visible, and nothing
rang. Armed again two minutes ahead and uploaded during the ring, the
boot carried `last_alarm_unix: None` and rang again 352 ms after
boot, from the carried time, before the link was up. Not driven:
the IP display, alarm-set and the PIN timeouts, which need the
buttons; the one `since` timer is the same code path for all of them.

### `POST /reload`: a deliberate reset on demand (after the third-pass items) — DONE, verified on the bench 2026-10-28

**Why.** The carry record can only be exercised by a software reset,
and the only ways to get one were a self-heal (pull the uplink, wait
five minutes) or an OTA of the same image (twenty seconds of dark
panel and a slot switch). The user asked for a request that does it.

**Design.**

- `POST /reload` with `Authorization: Bearer <OTA_TOKEN>`, the token
  only: the PIN is for the page, and a reset is a script's tool. A
  build without a token refuses it like `/ota`. The check is
  `ota::authorize_token`, split out of `authorize_with`, which now
  calls it for a `Bearer` header.
- The answer is `200` with one line, then after the socket is flushed
  the listener calls `heal::reboot`, which is the tail `heal::reset`
  and `ota::reboot` used to carry each: `scan::stop` (a no-op when the
  upload has stopped it already), `clock::carry_time`, the
  `RESET_PAUSE` wait and `software_reset`. `heal::reset` keeps its hold
  for a ringing alarm and its count; `/reload` has neither, so a reset
  mid-ring can be tested with it. The `RESET_PAUSE` invariant now lives
  in one function plus the synchronous rollback in `ota::init`.
- `http::respond` and `http::upload` both return what to do after the
  response, `Then::{Close, NewImage, Reload}`, in place of the `bool`
  the upload returned; the listener matches on it after the flush.

**Verify.** `curl -X POST -H 'Authorization: Bearer …' http://<ip>/reload`
answers `rebooting with the time carried over`; the boot line reads
`reset reason Some(CoreSw), software reset: carried over Carried { … }`
with `heals` unchanged, the time is back within a second, and
`last_heal` on the page stays as it was. Without the header, `403`.

**Result (2026-10-28).** No header, a wrong token and a `PIN` header
each answered `403` with the reason; the token answered `200` and the
board reset at once: `reset reason Some(CoreSw), software reset:
carried over Carried { heals: 0, …, unix_us: Some(…), ota_boots: 0 }`,
the time back at 13:46:43 against a request at 13:46:42, NTP five
seconds later moving nothing visible, `/api` with `heals` 0 and
`last_heal` null. The image had been uploaded over the air a minute
before and confirmed by the probe, which is why `ota_boots` was 0 by
the reload.

`scripts/reload.sh <ip>` wraps the request: it prints `/api` before,
posts the reload, and polls `/api` until the clock answers again,
about three seconds after the request on the bench. Three more
reloads with it, at uptimes under half a minute because the serial
monitor resets the board when it connects:

- The skip flag set just before: carried (`skip_next: true` in the
  boot line, the display's "carried over the reset" line, `skip` true
  on `/api` after).
- A colour change posted two seconds before the reload came back as
  the old colour, with no `settings: saved` between: the second-pass
  backlog item "a reset inside the settle window loses the last
  change", now reproducible in one line.
- Time: the request left the host at 13:50:04.687, the record carried
  13:50:05.002 (the response, the flush and the 200 ms pause), and the
  sync five seconds later moved nothing visible. The same held on the
  two reloads before it. That is the carry-as-plain-Unix-time design
  checked at short uptime; it has no uptime term, unlike the RTC
  scheme it replaced, so the hour-long soak is a formality still owed.
- The rollback count cannot be tested this way: serving the reload
  request confirms the image before the reset.

### More tunes, and a random pick (after `/reload`) — random verified on the bench 2026-10-28; the tunes retuned after two listens, a third owed

**Why.** The user asked for a few more melodies and a setting that
picks one at random for the alarm.

**Design.**

- **Five more tunes in `melody.rs`**, all public domain, written from
  memory of the standard versions and kept in G like the first three so
  they sit between D5 and E6, clear of the buzzer's resonance:
  - `morning`, Morning Mood from Grieg's Peer Gynt (1875): the first
    four bars of the pentatonic theme and the held note that follows,
    in 6/8 at 90 bpm. The one tune made for waking up.
  - `westminster`, the Westminster Quarters: the four changes of the
    hour sequence, each four notes and a pause, at 72 bpm. The clock
    chime; the lowest note is D5.
  - `reveille`, the US bugle call's first strain, twice as it is
    played: a pickup and three bars of the dotted figure on the bugle
    notes (D5, G5, B5), at 120 bpm.
  - `elise`, the opening of Beethoven's Für Elise (1810): the first
    eight bars of the melody line with the left hand's arpeggios where
    the right hand rests, at 100 bpm. It needs the pitches C5, E5, G#5
    and D#6, which are added; C5 at 523 Hz is the lowest note any tune
    uses and will be the quietest.
  - `jingle`, the chorus of Jingle Bells (1857), both endings, at
    120 bpm.
  Nine melodies in all; the table order is the page order and the
  stored index, so the new ones go after `frere`.
- **Random is a value of the `melody` setting**, not a second field:
  `melody::RANDOM` is the index one past the table, named `random` on
  the API and "Random tune" on the page. The buzzer task resolves it
  when a `Ring` or a `Preview` arrives, from the hardware RNG, so the
  display and the `/play` handler pass the setting through unchanged
  and the log line `buzzer: ring <name> (random)` says what was
  drawn. `melody::pick` is the pure part: a fixed index is itself;
  `RANDOM` is one of the tunes, every melody but the beeps at index 0,
  since the beeps are the plain alarm and "random tune" promises a
  tune; and it is never the one picked last when there is another,
  so two mornings in a row do not sound the same (the last pick is a
  local of the buzzer task, so it is forgotten at a reset, which is
  fine).
- **Record.** `RANDOM` is stored as `0xFF` in the melody byte at
  offset 35, so the index of a real tune and the layout do not change
  and `RECORD_VERSION` stays at 5. A record from this build holding
  `0xFF` or an index from 4 up fails `is_valid` on the M8 image and
  that image falls back to the defaults; that is the second-pass
  backlog item on clamping `color` and `melody`, not new here, and it
  only bites on a rollback to an older image.
- **Page and API.** The select gets the five tunes and "Random tune"
  as its last option; `melody=random` on the API, `"melody":"random"`
  back. `POST /play` with Random chosen previews a random tune, so
  the button still answers.

**Checked on the host** (scratch crate with `form.rs`, `settings.rs`,
`melody.rs`, `panel.rs`, `aip33628.rs` and `config.rs` by `#[path]`):
every note of every tune has a length and every tone sounds; the
tunes run 9.0 s (`morning`, E5 to D6, shortest tone 292 ms), 19.2 s
(`westminster`, D5 to B5, 773 ms), 8.2 s (`reveille`, D5 to B5, the
sixteenth sounds 110 ms), 7.5 s (`elise`, C5 to E6, 132 ms) and 32.0 s
(`jingle`, G5 to D6, 219 ms); every name round-trips through
`index_of`, `random` is index 9 and `9` is not a name; `pick` returns a
fixed index as is, and for `RANDOM` over a thousand draws reaches every
tune, never the beeps, never the avoided one, and wraps from the last
tune to the first; a record with `RANDOM` stores `0xff` and loads back,
each tune's index stores as itself, a raw 9 or `0xfe` in the byte is
refused, and a version 4 record still loads with the beeps;
`melody=random` and `melody=westminster` apply, `melody=9` is a bad
value; the JSON says `"melody":"random"`; the worst-case settings page
(24-byte hostname, −840, every checkbox on, an odd day list, Random
selected, the error notice, a 400-day uptime with the fullest heal
line) is 2730 bytes, 2833 with headers, of the 4096 buffer; the update
page is 2349 with headers.

**Verify on the panel.** On the page, each new tune plays once from
"Play" and is recognisable; "Random tune" plays a tune, and pressing
"Play" again plays a different one. Save with "Random tune" and reload
the page: it stays selected, `/api` says `"melody":"random"`, and the
boot line after `scripts/reload.sh` prints `melody: 9`. Set the alarm
a minute ahead with Random: it rings a tune and the log says
`buzzer: ring <name> (random)`. `curl -d melody=westminster
http://<ip>/api` echoes it; `melody=random` too; `melody=9` is a 400.

**Result (2026-10-28, bench board over the air, then the serial
monitor).** The page lists the nine sounds and "Random tune" in order;
each of the ten names posted to `/api` is echoed back, `melody=9` and
`melody=jingle+bells` are `400`. With `random` saved, `scripts/reload.sh`
brought it back from flash (`"melody":"random"` before and after) and
the boot line after the monitor's USB reset read `melody: 9` from the
record. Six `POST /play` with `melody=random`, four seconds apart, drew
`morning`, `elise`, `jingle`, `reveille`, `frere`, `twinkle`, each
logged as `buzzer: preview <name> (random)`: six different tunes, never
the beeps. The five new tunes previewed by name each logged without the
suffix. The alarm armed a minute ahead with Random rang at 14:26:00
with `buzzer: ring westminster (random)`, a tune other than the last
pick. Not checked: how the tunes sound, which needs an ear at the
bench; they are transcribed from memory and Reveille's rhythm is the
least certain. The board was left as found: alarm 14:17 armed, beeps.

**First listen (2026-10-28, the user at the bench).** Morning Mood
did not sound right and its fourth note rang out; the Westminster
Quarters rang out on a note of each change; Reveille rang out on its
first note; Für Elise rang out somewhere, less badly; Jingle Bells was
too slow. The loud notes are exactly the pitches below G5 the new
tunes introduced: E5 (Morning Mood's fourth note), D5 (the held note
of the chime, Reveille's pickup) and C5 and E5 (the Für Elise
arpeggios, sixteenths, so less of it). A 50 % square wave carries a
third harmonic a third as strong as the fundamental, and for C5 to F5
it lands between 1.6 and 2.1 kHz, where G6 (1568 Hz) and A6 already
rang out in M8: the buzzer's resonance, reached from below. Two
changes, both still to be heard:

- The three tunes that could move were transposed so their lowest
  note is G5: Morning Mood to B flat (G5, Bb5, C6, D6, F6; a new
  `AS5`), the Westminster Quarters and Reveille to C (G5, C6, D6, E6).
  Morning Mood is also slowed from 90 to 75 bpm (an eighth 400 ms, a
  dotted quarter 50 to the minute, nearer the recordings), since the
  pace may be part of what sounded wrong; the notes are the pentatonic
  theme as remembered (mi re do la do re, twice; mi re mi sol re sol;
  mi re do la do re; mi), and if it still sounds wrong the notes are
  the next suspect. Jingle Bells goes from 120 to 160 bpm, 24 s a
  pass.
- Für Elise cannot go up (its phrase sits on E6) and keeps C5 and E5,
  as Frère Jacques keeps its D5, so the buzzer now takes its duty from
  the pitch: `melody::duty_pct` gives 33 % below 700 Hz and 50 %
  above. A one-third duty cycle has no third harmonic at all and keeps
  87 % of the fundamental, which is the per-pitch duty table M8 held
  in reserve, reduced to one threshold. If the low notes still ring
  out, the resonance is not the harmonic but something near 600 Hz
  (the case, say), and the answer is a lower duty for those pitches
  or dropping the arpeggios' low notes.

Uploaded to the bench board at 15:07; the host harness checked the
ranges (every tune G5 to F6 except Frère and Für Elise, the only two
with a note below 700 Hz) and the lengths (Morning Mood 10.8 s, Jingle
Bells 24.0 s). To hear: "Play" on each of the five, listening for an
even loudness across the notes, Morning Mood's tune and pace, and
Jingle Bells' pace.

**Second listen (2026-10-28).** The Westminster Quarters, Reveille
and Jingle Bells are fine now, which bears out the third-harmonic
reading for D5 and E5. Morning Mood had wrong notes: the user supplied
a score (`morning-mood-notes.png`, an arrangement in C, 3/4 with the
6/8 eighths as quarters), which reads G E D | C D E | G E D | C D-E
D-E | G E G | A E A | G E D | C held: sol mi re do re mi, the second
time with a turn of sixteenths, then sol mi sol la mi la, sol mi re,
do (the held note was first misread as E and corrected by the user).
The first cut's figure (mi re do la do re) had the leap at the
wrong end, a third too low throughout. Rewritten from the score, in G
(G5, A5, B5, D6, E6; `AS5` and the B flat key go), 75 bpm, 9.6 s.
Für Elise rang out on its long notes, the A5, B5 and C6 eighths and
the final A5, pitches that are fine in every other tune, so the drive
is not the difference; the tune was fast (a sixteenth 150 ms, 132
sounding, blips next to 263 ms eighths). Slowed from 100 to 75 bpm:
a sixteenth 200 ms, five notes a second, the usual pace, 10 s a pass.
If the long notes still stand out, the next step is dropping the
arpeggios so there is nothing quiet to contrast with. Uploaded to the
bench at 15:20.

**Third listen (2026-10-28).** Morning Mood's held note was C in the
score, not E; fixed. The D (D6, 1175 Hz) of Morning Mood rings
weakly; Twinkle holds the same D6, so it is the pitch. Either the
fundamental is on the lower slope of the resonance (G6 at 1568 Hz
rang out in M8), which no duty can help, or the transistor's slow
turn-off stretches the low, sounding, phase past 50 % and lets a
second harmonic through at 2350 Hz, which one global correction
would fix for every pitch. The user chose to try the correction by
ear, so the buzzer's square wave high time is now a runtime value,
`melody::SQUARE_PCT` (50) until `POST /play?duty=NN` (first 40 to
60, then 25 to 75 at the user's request) moved it; the one-third
duty of the low pitches shifted by the same amount. Result: 50 is
the cleanest; below 45 and above 55 every note sounds dirtier, which
is the second harmonic a duty off 50 brings in. So the turn-off is
not stretching the pulse, the D is its fundamental on the resonance
slope, firmware cannot help it, and the hook is removed again (the
`query_value` helper it needed stays, `error_from_query` now uses
it). Final image on the bench at 15:30. What is left is acoustic: a
filter before the buzzer can only take harmonics out, not the
fundamental, and the one that would help the D would take the top
notes with it; damping the buzzer's port lowers the resonance's Q
and is the thing to try if the D matters.

**Acoustic fix (2026-10-28, the user at the bench).** Masking tape
across the buzzer's sound port made it louder, not softer: tape is
airtight, so it seals the cavity and becomes a membrane with a
resonance of its own rather than damping anything. Tape with a small
hole pricked in it is what the user kept: a sealed cavity with a tiny
port is a Helmholtz resonator tuned low, under the tunes, and it
lifts the fundamentals without touching the harmonics, so the notes
come through better at about the same loudness. The hole's size sets
where the lift sits (bigger is higher). Porous damping, felt or
cotton over the port, was the textbook answer and was not needed.
With the tape on, the one-third duty for the low pitches was compared
with the square wave through a second temporary hook
(`POST /play?low=NN`, Für Elise and Frère Jacques): little difference
to the ear now, so 33 % stays and the hook was dropped uncommitted.

### Fewer `unsafe` blocks (after the tunes) — DONE, verified on the panel 2026-10-29

**Why.** The user asked for every `unsafe` in the tree to be looked at
and made safe where the API allows. There were seven: two
`unsafe impl Send` in `scan.rs`, the volatile read and write of the RTC
record in `carry.rs`, the PAC pointer that sets the display pull-downs
in `main.rs`, and the two `#[unsafe(no_mangle)]` hooks there.

**What was found.**

- `TransferBuf`'s `Send` impl was redundant: `DmaTxBuf` is `Send` on
  its own (removing both impls and compiling left one error, and it
  was the SPI driver's).
- `ScanHardware`'s `Send` impl covered `SpiDma<'static, Async>`, which
  esp-hal makes `!Send` on purpose: an async driver installs its
  interrupt handler on the core it is created on, so the driver must
  be created where it is used. The impl asserted exactly what the
  marker exists to deny. Harmless on one core, but the fix is to build
  the driver in the task that uses it.
- The RTC record was a `static mut` because esp-hal's docs show
  `#[ram(persistent)]` that way. The macro accepts any `static` whose
  type is `Persistable`, and `portable_atomic::AtomicU32` is one, so an
  array of atomics gives the same memory with a shared reference and
  no `unsafe`.
- The pull-downs went through the PAC because the SPI builder resets a
  pin's pull when it connects it. esp-hal's documented way around that
  is a frozen signal: the builder's `apply_output_config` and enables
  are no-ops on it while the matrix connection still happens, and a
  `Flex` handed to the builder is converted through that frozen path.
  So the pull can be set on the `Flex` before the bus is built.
- The two `#[unsafe(no_mangle)]` stay. Edition 2024 spells the
  attribute that way, and esp-backtrace (`custom-halt`) and esp-println
  (`timestamp`) resolve those exact symbol names. The functions are
  safe; only the export is marked.

**Design.**

- `scan.rs`: `ScanHardware` carries the `SPI2` and `DMA_CH0`
  singletons, the four wires as `Flex` pins with their pull-downs set,
  and the first buffer, all `Send`. `scan_task` builds the bus
  (`Spi::new`, `with_sio0..3`, `with_dma`, `into_async`) before its
  first transfer, on the interrupt executor it runs on. `TransferBuf`
  goes; the channels carry `DmaTxBuf`.
- `main.rs`: `display_wire(pin)` makes a `Flex`, applies an
  `OutputConfig` with `Pull::Down`, enables the input buffer as the
  builder would have, and returns it. `pull_down_display_wires`, the
  GPIO number table and the `esp32c3` dependency go.
- `carry.rs`: `RECORD` is `[AtomicU32; WORDS]` from `portable-atomic`
  (a direct dependency now; esp-hal already pulled it in), read and
  written word by word with `Relaxed`, as the volatile array accesses
  were. One task in thread mode touches it, as before.

**Verify.** Build and clippy on the host; `grep -rn unsafe src` shows
the two attributes only. On the bench: the panel comes up and scans as
before (the bus is now built inside the scan task) and the heartbeat's
restart gap is unchanged; `/reload` still carries the time, the heal
count and the skip flag (the record still round-trips); `/update`'s
dark panel and the moment after a deliberate reset show no stray lit
row (the pull-downs are still set).

**Measured (bench, 2026-10-29).** Uploaded over the air onto a board
running the previous firmware. The new image booted from the record
the old firmware had written with `write_volatile` and read it through
the atomics at the same address (`.rtc_fast.persistent` at
0x50000000, 32 bytes, unchanged): time carried, `ota_boots: 1`,
confirmed good at the first probe. The scan task built the bus and
printed its line at 338 ms, as before. Two `POST /reload`s: the time
came back each time (within a second of the NTP sync that followed),
and the skip flag set through `/api` before the second one came back
as `skip_next: true`, so the new write path round-trips a non-default
field; the flag was then cleared. The first heartbeat read a restart
gap average of 2 µs and a maximum of 99 µs over 19269 transfers, the
range the old firmware logged (77 to 81 µs maxima over 19264 to 19270
transfers). The user watched the panel afterwards: it goes dark
and nothing is faintly lit, so the pull-downs set on the `Flex` pins
survive the builder as the esp-hal source said they would (a `Flex`
is converted through `into_peripheral_output`, which freezes the
signal, and the builder's `apply_output_config` is a no-op on a
frozen one).

### M10 — Wi-Fi provisioning from an access point — steps 0 and 1 verified on the bench 2026-10-29; step 2 built and bench-checked the same day, the phone and the buttons owed

**Scope.** Credentials become a stored setting instead of a build-time
constant, and a clock that has none raises its own network so the
user can give it one from a phone. While it is an access point there
is no internet, so no time and no alarm: the panel says `AP` and
waits. The build-time `WIFI_SSID`/`WIFI_PASSWORD` stay as the seed
for a board whose record has no credentials yet, and may now be empty.
The sketch of 2026-10-22 is reworked below against the tree at
5170871; where this plan departs from it the paragraph says so.

**Facts checked in the sources (esp-radio 1.0.0-beta.1, embassy-net
0.9.1, smoltcp 0.13.1, sequential-storage 8.0.2).**

- `Config::AccessPoint(AccessPointConfig)` exists; `set_config` stops
  the driver, sets the mode and calls `esp_wifi_start`, which raises
  `AccessPointStart` before it returns, so the AP interface's link is
  up when `set_config` returns and embassy-net sees it at once. No
  `connect` step. The receive path registers one callback for both
  interfaces "until we support APSTA", so AP+STA stays off the table:
  one mode per boot.
- `AccessPointConfig::default()` is open, channel 1, DTIM 2, 255
  connections; WEP and an empty WPA2 password are refused by
  `validate`. `Ssid` is up to 32 bytes, `Password` up to 64.
  `Interface::access_point()` builds the driver half and
  `mac_address()` gives the AP MAC from the efuse before the driver
  is configured.
- `connect_async` fails with `ConnectionError::Failed(DisconnectedInfo)`
  whose `reason` is the driver's code: `NoAccessPointFound` (201),
  `FourWayHandshakeTimeout` (15), `HandshakeTimeout` (204),
  `AssociationFailed` (203). The first means no such network in
  range, the next two a wrong password.
- `embassy_net::Config::ipv4_static(StaticConfigV4 { address, gateway,
  dns_servers })` is the static address. smoltcp accepts IPv4 packets
  to the broadcast address and sends to `255.255.255.255` with the
  broadcast MAC and no ARP (`lookup_hardware_addr`), so a DHCP server
  on a `UdpSocket` can both hear a discover and answer it broadcast.
  embassy-net has no DHCP server; `edge-dhcp` is not in the registry
  here and is not needed.
- A map item costs 8 bytes of header plus the key and the data padded
  to a word; a 134-byte record is a 144-byte item, so the 128-byte
  scratch has to grow.
- `wait_for_access_point_connected_event_async` reports stations
  joining and leaving by MAC, for the log.

**The carry record starts over at a mode switch (2026-10-29).** Every
word in the record belongs to the network mode that wrote it: the
heal count and time describe the old network and the access point
heals nothing, the carried Unix time is valid only within one running
clock and after an access-point session of any length it would be
shown as the time, drift and all, until SNTP lands, and the skip flag
and the time the alarm last rang are keyed to that time. So both
switch resets, the 10 s hold into the access point and the credential
save out of it, write a fresh record instead of carrying one:
`Carried::default()` with `ota_boots` kept, because the rollback
counts boots of an unconfirmed image in either mode and the new image
may be the one that cannot join the network, and `boot_ap` set or
cleared. Neither writes the time, so they go through a second
deliberate-reset path beside `heal::reboot` (which writes the time
after the caller's update): `scan::stop`, the record, the pause, the
reset. The panic path is untouched on purpose: a panic in access-point
mode keeps the record, so the flag holds and the clock comes back as
an access point rather than booting the station with nothing to join.
The cost is one blank panel after a provisioning save until SNTP
answers, where a `/reload` shows the time within a second; there is
no time to carry out of the access point anyway.

**Step 0, first and on its own: a record from a newer image survives
a rollback.** The second-pass backlog item: `from_record` returns
`None` for `version > RECORD_VERSION` and `is_valid` rejects a `color`
or `melody` index this build does not know, so the version 6 record
this milestone writes would reset every setting when the rollback or
a USB flash boots a version 5 image, and that image's first change
would overwrite the credentials for good. Fix: `from_record` reads the
prefix it knows of any newer version (the versions are append-only;
the comment says so) and clamps `color` and `melody` the way
`brightness` is clamped. One commit, flashed over the air before the
rest, so the image the rollback lands on already tolerates the new
record. Host check: a version 6 record with trailing bytes read by the
version 5 parser gives the version 5 fields.

*Done 2026-10-29, host-checked and verified on the bench the same day:
the image went over the air into `ota_1`, booted with `settings:
loaded from flash` and the same settings line as before (yellow, 10 %,
alarm 14:17 armed, `test-clock`), was confirmed good and synced.* The `version > RECORD_VERSION` early return is gone, `color`
lands on `PALETTE.len() - 1` and a melody byte past the table on the
last tune (`0xff` stays random); the brightness rule is as it was,
clamped within the five choices and rejected past them. The scratch
crate (`config.rs`, `melody.rs`, `panel.rs`, `aip33628.rs` and
`settings.rs` by `#[path]`, embassy-sync 0.7 from the registry) checked:
a version 5 round trip; the same bytes marked version 6 with 131 bytes
appended, and version 9 with 40 more, read as the version 5 fields;
colour 9 lands on 7, melody 200 on the last tune, brightness 4 on the
brightest offered and 5 is rejected; an hour of 30, no alarm days, day
bit 7, a hostname length past its field and a truncated record are
rejected; a version 1 prefix still reads. Bench verification is a
flash of this image, then the M10 image over it later and a rollback
to see the settings survive, which is step 1's check.

**Step 1: credentials as a setting, and the switch reset.** After this
step the clock still always boots the station, but it joins the
network in its record, and a credential change over `/api` resets it
into the new one. Verifiable on the bench without a phone.

- `settings.rs`: a `Text<const N: usize>` (length byte plus `[u8; N]`,
  UTF-8 in, `as_str` out, `new(&str) -> Option<Self>` with a length
  range) and `Credentials { ssid: Text<32>, password: Text<64> }` with
  `is_valid`: SSID 1 to 32 bytes, password empty (open network) or 8
  to 64 bytes (`validate` in esp-radio refuses a WPA2 password under
  8). `Credentials::NONE` is the empty SSID and means "none". The
  `Settings` field is `wifi: Credentials`; `Settings` keeps `Copy` and
  `PartialEq`. `Credentials` gets a hand-written `Debug` that prints
  the SSID and `password: <set>` or `<none>`, because the boot line
  prints the settings and the password must not reach the serial
  port. `Hostname` could become `Text<24>` with the extra character
  rule on top; not in this milestone, it works as it is.
- The token is a setting too (user's call, 2026-10-29: a build with
  a blank token must be able to get one later, from the clock, and
  the PIN already proves what an upload rests on, that the person can
  see the panel). `token: Text<32>` in `Settings`, valid empty (none)
  or 8 to 32 bytes of printable ASCII without spaces, so it survives
  a shell and a header. `auth::token_ok` reads `settings::get().token`
  instead of the constant, still in constant time, and `token_enabled`
  is "not empty". It is not a `form::apply` key, so it cannot arrive
  through `/api` or the settings form, and it is never in the page,
  the JSON (`"token_set":true` is all) or the `Debug` output. The
  boot line says `api token: stored`, `built in` or `none`.
- Record version 6 appends `ssid_len` at 36, the SSID at 37..69,
  `password_len` at 69, the password at 70..134, `token_len` at 134
  and the token at 135..167: `RECORD_LEN` 167. Version 5 and older
  read as `Credentials::NONE` and no token. `storage::Scratch` grows
  to 256 bytes (the item is 176) and stays word aligned.
- `POST /token`, form-encoded `token=…`, with either `Authorization:
  PIN dddd` from the update page or `Bearer <current token>` from a
  script, so a token can be rotated without the panel. A second form
  on the `/update` page, "Set API token", with a `type=password` field
  and the PIN field, posted by the same script as the upload with the
  PIN in the header; the page says one PIN is one action, upload or
  token, and a reload gives a new one. An empty token clears it and
  closes the script paths again; `token_set` in `/api` is the way to
  confirm it. The change goes through `settings::update` and the
  persist task like any setting. The update page grows by about 500
  bytes from 2349; measured on the host against `RESPONSE_BUF`.
- `POST /pin`, no body and no header, for the bench (user's call,
  2026-10-29): it issues a PIN exactly as opening `/update` does, so
  the panel shows it and the log prints `PIN dddd` as it does today,
  and answers plain text, `200 PIN issued, on the panel and the
  serial log for 5 minutes` or `403 locked out for N s` after a wrong
  attempt. The PIN is never in the response. `scripts/pin.sh <ip>
  <serial-log>` posts it, waits for the new PIN line in the log the
  monitor is writing and prints the four digits, so a bench run goes
  `PIN=$(scripts/pin.sh 10.1.1.31 clock-serial.log)` and then posts
  `/token` or `/ota` with `Authorization: PIN $PIN`; the monitor must
  be running before the post, as the bench notes already say.
- The seed: `main.rs` runs `settings::seed` on the loaded value
  before `settings::init`, which fills `wifi` from
  `config::WIFI_SSID`/`WIFI_PASSWORD` and `token` from
  `config::API_TOKEN` where the record has none and the build has
  them; the persist task writes them on the next change and the
  board then carries them in its record, after which the record wins
  over a later build's constants, which the boot line makes visible.
  A build with no credentials leaves `NONE`, and the boot line says
  `wifi: no credentials stored or built in`.
- `net.rs`: `station_config` and `log_candidates` take a
  `&Credentials` from `settings::get().wifi` instead of `config`.
  `wifi_task` with `NONE` stays offline as it does today with an empty
  `WIFI_SSID` (step 2 replaces this with the access point). The
  connect failure line adds the reason's meaning: `no such network in
  range` for `NoAccessPointFound`, `wrong password?` for
  `FourWayHandshakeTimeout` and `HandshakeTimeout`, the raw reason
  otherwise, so a mistyped password is diagnosable over USB without
  ever flipping the clock into an access point by itself.
- The token becomes the API token (user's call, 2026-10-29: the
  credentials are the one setting that can take the clock offline, so
  writing them from the LAN needs the same proof as a firmware
  upload). `OTA_TOKEN` is renamed `API_TOKEN` everywhere: the build
  variable and `build.rs`'s rerun list, `config::API_TOKEN` and
  `API_TOKEN_MIN`, the boot line, `scripts/ota.sh` and
  `scripts/reload.sh`, AGENTS.md. The token and PIN code moves out of
  `ota.rs` into `src/auth.rs` in the same commit (user's question,
  2026-10-29: not `http.rs`, which is the caller and already the
  largest module, and not `net.rs`, which is the link and never
  touches a header): `token_enabled`, the constant-time `token_ok`,
  `PinState` with its TTL and lockout, `issue_pin` and `pin_to_show`
  (the display task's half), `Refusal` and the `authorize*`
  functions, about 120 lines with no hardware in them, so the host
  harness pulls the file in with `#[path]` instead of copying
  functions out (`token_ok` reads the settings, which the harness has). `ota.rs` keeps the slots, the sector writer and the
  rollback, imports `Refusal`, and its upload-specific refusal texts
  stay with `install`. `http.rs` calls `auth::token` for `/reload`
  and the credentials on `/api`, `auth::token_or_pin` for `/ota` and
  `/token`, and `auth::issue_pin` for `/update` and `/pin`. A
  build without a token has no script path for uploads, `/reload` or
  credentials, as it has none for uploads today.
- `form.rs`: keys `ssid` and `password`, both plain strings; the value
  buffer in `apply` grows from 32 to 64 bytes for the password. The
  key absent means unchanged, as for every key; the key present and
  empty means an open network for `password` and a bad value for
  `ssid`. `apply` takes a `credentials: bool`; when it is false either
  key is `FormError::Forbidden`, which stops the request before
  anything is applied, like the other errors. The check is on the
  key, never on whether the value would change: a rule that refused
  only a changed password would answer `200` to a guessed one, an
  oracle for anyone on the LAN. `write_json` adds `"ssid":"…"` and
  `"password_set":true`, never the password. An SSID may hold `"`, `\`, `<` or `&`, which
  the hostname's character rule rules out today, so `form.rs` gains
  `JsonStr` and `HtmlAttr` display wrappers (about 25 lines) that
  escape them, and uses them for the SSID in the JSON and in the
  page's `value=`. The settings page itself does not show the
  credentials: a typo there would cost the user the network, and the
  page is for things the panel can show. `http.rs`: `POST /api` passes
  `credentials` as `auth::token(req.authorization).is_ok()`,
  so `curl -H "Authorization: Bearer $API_TOKEN" -d ssid=… -d
  password=…` is the way to move a clock to another network from the
  LAN and a request without the header gets `403 token required`
  (`Forbidden` maps to that status; `BadValue` and `UnknownKey` stay
  `400`). `POST /` on the settings page passes false: the form has no
  credential fields and a crafted post of them is refused the same
  way. In AP mode (step 2) both paths pass true: the access point's
  own WPA2 password is the gate there, and the phone cannot be
  expected to carry the token. `scripts/configure-clock.sh` is
  unchanged; the keys and the header are documented in AGENTS.md.
- `carry.rs`: `LAYOUT` 4, `WORDS` 9, word 8 is `boot_ap`. An old
  image's record is discarded by the mark, as the layout version was
  made for.
- `heal.rs`: `switch_mode(boot_ap: bool) -> !`, the second
  deliberate-reset path from the carry paragraph above: log the
  reason, `scan::stop`, write `Carried { ota_boots: carry::get()
  .ota_boots, boot_ap, ..Carried::default() }`, pause
  `clock::RESET_PAUSE`, reset. Not counted as a self-heal, and no
  `carry_time`: the time is not carried across a mode switch.
- The trigger for the AP-to-station switch, and for a re-provisioning
  from the LAN, is one rule in `storage::persist_task`: after a save,
  if the saved credentials differ from the ones this boot started
  with (`settings::get().wifi` at task start, not `stored`, which is
  `None` on a first boot with the seed), `heal::switch_mode(false)`.
  The 2 s settle gives the HTTP response and the browser's redirect
  time to finish, which is the same reason `RENEW_SETTLE` exists, and
  the flash write is done before the reset by construction. A
  credential change over `/api` in station mode therefore means "join
  this network now", with the old network's record gone if the new
  one is wrong; the hold in step 2 is the way back, and the token is
  the gate. A board without
  an `nvs` partition has no persist task and cannot be provisioned
  from the page; its boot line already says settings will not persist.

Bench check for step 1: `curl -d ssid=esptest -d password=…
http://10.1.1.31/api` without the header answers `403 token
required` and changes nothing; with `-H "Authorization: Bearer
$API_TOKEN"` and the current network's own credentials it
answers the JSON, the clock resets within 3 s with `boot: reset
reason CoreSw, software reset: carried over Carried { heals: 0, …,
unix_us: None, ota_boots: 0, boot_ap: false }`, the panel is blank
until SNTP and the log says `settings: loaded from flash` with the
SSID in the settings line and no password or token. A build without
`WIFI_SSID` flashed over a record that has the SSID joins it anyway.
A build without `API_TOKEN`: the boot line says `api token: none`,
`scripts/ota.sh` gets `403`; `scripts/pin.sh` prints the PIN from
the serial log, `curl -H "Authorization: PIN $PIN" -d token=…
http://10.1.1.31/token` sets it, `token_set` turns true in `/api`, the script works with
it, and after a power cycle the boot line says `stored`; a wrong PIN
locks the form out like the upload. Post an empty token with the
`Bearer` header: `token_set` false, the script gets `403` again.

*Step 1 done 2026-10-29, host-checked and verified on the bench the
same day; not committed at the time of writing.* What was built
matches the bullets above, with these particulars:

- `auth.rs` takes the random number for a PIN as a parameter
  (`issue_pin(random)`, the caller in `http.rs` reads the RNG), so the
  file has no hardware in it and the harness pulls it in whole. The
  PIN log line now says `auth: PIN dddd shown on the panel, good for 5
  min`, which `scripts/pin.sh` greps for.
- `settings.rs`: `Text<N>`, `Secret<N>` (prints `<set>`/`<none>`),
  `Credentials` with `NONE`, `valid_password`, `valid_token` (const,
  so the build-time constants are checked at compile time: a short
  `WIFI_PASSWORD` or a token with a space is a build error, where a
  short `OTA_TOKEN` used to count as none), `seed`. Record version 6
  as laid out above, `RECORD_LEN` 167, the layout const-asserted.
- The main stack went from 98.2 KB to 96.4 KB: `Settings` grew by 131
  bytes and sits in several task futures, plus the 128 bytes of
  scratch.
- `form::Pairs` iterates a body once for `apply` and `parse_token`;
  `HtmlStr` exists for step 2's page. The settings form gets a third
  error notice, "the network is set with the token", on `?error=token`.
- The `/api` refusal for a bad `Bearer` reads the same as for a
  missing one ("token required"): `apply` only knows whether the
  token checked out. Good enough; a wrong token on `/reload` or
  `/token` does say `bad token`.
- **Bug found on the bench and fixed:** the first switch reset carried
  `unix_us`. The display tick writes the time into the record every
  loop, and it ran during the 200 ms pause between the record reset and
  the chip reset. `carry::freeze` writes the switch's record and drops
  every later `update`; `switch_mode` uses it. The second run carried
  `unix_us: None`.

Host checks (the scratch crate, now with `form.rs` and `auth.rs` too,
embassy-time and critical-section on their `std` features): version 6
round trip with a 32-byte SSID, a 64-byte password and a 32-byte
token; version 5 reads as none; version 7 with trailing bytes reads
the version 6 fields; SSID length 33, password 7 and 65, token 7, a
token with a space and a non-UTF-8 SSID are rejected; the token rule
at the edges; `Debug` shows the SSID and `<set>`, never the values;
`seed` fills an empty record and leaves a full one; `apply` with
`ssid`/`password` through `+` and `%26`, `Forbidden` with nothing
applied when `credentials` is false whatever else the body holds, the
empty SSID, the short and the 65-byte password, `token` as an unknown
key, the settings form leaving the network alone; `parse_token` on a
good, an empty, a short, a spaced and a wrong-key body; the escapes;
the JSON with a quoted SSID; the settings page at 2821 bytes with
headers and the update page at 3059 against 4096; `auth::token` with
a good, trimmed, short, long, PIN-shaped and missing header;
`with_pin` through issue, use, reuse, wrong, lockout, expiry and the
`Bearer` short-cut; the cleared token never matching.

Bench (10.1.1.31, over the air onto a version 5 record): boot line
`network built in, api token built in`, the old carry record discarded
by the layout mark, the settings line with `password: <set>, token:
<set>`; `ssid` without the token `403` and `color=red&ssid=x` leaving
the colour alone; a bad `Bearer` `403`; the same credentials with the
token `200` and no reset; `password=short` `400`; `token=` on `/api`
`400 unknown key`; two ordinary saves without a reset. `scripts/pin.sh`
read `9841` from the log; `/token` with it set `bench-token-1234`, the
old token was then `403` and the new one `200` on the credentials; the
used-up PIN `403 no PIN issued`; a wrong PIN `403`, then `/pin` `403
locked out for 10 s`; a token with a space `400`; clearing with the
`Bearer` gave `token_set: false` and `/reload` `403 no token set`; a
new PIN set the original back; `GET /update` 2966 bytes. The switch:
`password=wrongpass1` with the token answered `200`, `settings: saved`
2 s later, `heal: resetting into station mode, carry record started
over`, boot line `network stored, api token stored` with `carried over
Carried { heals: 0, …, unix_us: None, ota_boots: 0, boot_ap: false }`,
then `connect failed: … FourWayHandshakeTimeout …, wrong password?`
every 15 s. Recovery each time: USB flash with `--erase-parts
otadata,nvs`, `settings: nothing stored`, the seeds in the boot line,
and the settings put back with one `/api` post (yellow, 10 %, 14:17
armed, every day, beeps, -420, `test-clock`).

**Step 2: the access point.**

- Mode decision in `main.rs`, after `settings::init`, the seed and
  `carry::init`, before the stack is built: access point if
  `carry::get().boot_ap` or `settings::get().wifi` is `NONE`, station
  otherwise. `net::set_mode(Mode)` stores it in an `AtomicBool`;
  `net::mode()` is read by the HTTP handler and the display task. The
  boot line says which. The interface is `Interface::access_point()`
  or `Interface::station()`, the same type, so one `embassy_net::new`
  call takes either; in AP mode its config is
  `ipv4_static(192.168.4.1/24, gateway 192.168.4.1, dns [192.168.4.1])`
  and in station mode `default()` with DHCP switched on later, as now.
  `StackResources<10>` stays: the access point runs the DHCP server's
  socket and the three listeners and none of the station's five.
- `net::ap_task(controller, stack)`: `set_max_tx_power` as the
  station does, `set_config(&Config::AccessPoint(…))` with SSID
  `clock-xxxx` from the last two AP MAC bytes in hex, WPA2 with
  `config::AP_PASSWORD` (build-time, default `esp32clock`, const
  assert of 8 to 64 bytes), channel 1, the rest default; log `wifi:
  access point clock-xxxx up, 192.168.4.1`, then loop on
  `wait_for_access_point_connected_event_async` logging each station's
  MAC joining or leaving. Spawned in AP mode instead of `wifi_task`,
  `ntp_task`, `hostname_task`, `monitor_task` and `probe_task`.
  `net_task` and the HTTP listeners are spawned in both modes.
- `src/dhcpd.rs`: the pure part is `parse(&[u8]) -> Option<Request>`
  (op 1, htype 1, hlen 6, the magic cookie, option 53 discover or
  request, option 50 requested address, option 54 server id, the xid,
  flags and chaddr) and `build(&mut [u8], &Request, Reply) -> usize`
  (op 2, the xid and flags copied, `yiaddr`, `siaddr` 192.168.4.1,
  chaddr copied, options 53 offer/ack/nak, 54 server id, 51 lease
  3600 s, 1 mask /24, 3 router and 6 DNS both 192.168.4.1, 255), plus
  `Leases`: eight slots of `Option<([u8; 6], Instant)>` for
  192.168.4.2 to .9, a MAC keeps its slot, an expired slot is reused,
  a ninth client is ignored. The task binds a `UdpSocket` on port 67
  with two 600-byte buffers, answers a discover with an offer, a
  request for the client's own slot with an ack, any other request
  with a nak (the phone then discovers again), and sends every reply
  to `255.255.255.255:68`, which every DHCP client accepts and which
  needs no ARP for an address the client does not have yet. About
  150 lines; the parse and build halves have no hardware in them.
- `http.rs`: in AP mode `GET /` serves `form::write_provision_page`:
  SSID (text, `maxlength=32`), password (`type=password`,
  `maxlength=64`, "leave empty for an open network"), timezone and
  hostname, with the firmware line and the `/update` link, so a
  clock with a wrong password can still be updated over its own
  network. `POST /` applies through `form::apply` with
  `checkboxes_absent_off` false (the form has none) on a copy; on a
  bad field it redirects to `/?error=…` as the settings page does;
  on success it answers `200` with a short page, "Saved. The clock
  restarts now and joins `<ssid>`. Join that network yourself and
  open http://`<hostname>`/ or the address the clock shows on a
  both-button press." When the posted credentials differ from the
  stored ones the handler commits them with `settings::update` and
  returns `Then::Close`: the persist task writes the record and makes
  the switch, as in step 1, after the response is long gone. When they
  equal the stored ones (a clock put into AP mode by the hold, given
  its own network again) nothing changes and the persist task would
  not fire, so the handler returns `Then::Join` and the listener
  itself calls `heal::switch_mode(false)` once the response is
  flushed; there is nothing to write first. (Departure from the
  sketch, which had no answer to the unchanged case.) The provisioning
  form posts without a token, see step 1; `/api` in this mode takes
  credentials without one too, so the two paths agree. `/api` works in both modes; `write_json` adds
  `"mode":"ap"` or `"sta"` as a read-only key. The page's `<p>` with
  "Scriptable" and the status line are shared with the settings page.
- `display.rs`: no new `Ui` mode (departure from the sketch, which
  had `Provision`). The display task knows the mode and draws the
  `AP` face, `A` and `P` in the first two digits, colon off, in place
  of the sweep; `View::Sweep` is what `Ui` produces without a time,
  so `ui.rs` is untouched and presses keep their meanings: colour
  and brightness still cycle, the alarm can still be set, and the
  both-button long press shows 192.168.4.1 through the existing IP
  view since `stack.config_v4()` is the static address. The alarm
  cannot ring without a time.
- `buttons.rs`: a third `Kind::Hold`, emitted once for `Button::Both`
  when both have been down for `HOLD_PRESS` (10 s), tracked in
  `button_task` as the instant both became pressed; the 800 ms `Both
  Long` fires first and starts the IP display, which is harmless.
  `display_task` takes the event before `ui.handle`: in station mode
  `heal::switch_mode(true)`, in AP mode `heal::switch_mode(false)` if
  the record has credentials (the way back without a power cut; the
  sketch only had the power cut) and nothing otherwise. The hold is
  the only thing besides an empty record that starts the access
  point; a station that cannot connect retries forever, as today.
- `config.rs`: `AP_PASSWORD` and `AP_SSID_PREFIX` (`clock-`), and
  `WIFI_SSID` documented as optional. `build.rs` adds `AP_PASSWORD`
  to the rerun list.

*Step 2 built 2026-10-29, host-checked, and on the bench as far as a
bench without a phone goes; the phone and the buttons are owed.* What
differs from the bullets above:

- `dhcpd.rs` as planned, 290 lines with the task and the log lines;
  `parse`, `build` and `Leases` take no hardware (`Leases::handle`
  takes the clock in seconds). An offer holds its slot 60 s, a lease
  an hour; a request names the address in option 50 or, renewing, in
  `ciaddr`; a request with another server's id is ignored, one for an
  address that is not ours or is someone else's is a nak. Replies are
  padded to 300 bytes, the BOOTP minimum.
- `net::ap_task` takes the SSID, built in `main` from
  `Interface::access_point().mac_address()`, so `main` decides the
  mode and both interface and config come from it; `net::Mode` is an
  `AtomicBool` behind `set_mode`/`mode`. `AP_SSID_PREFIX` is `clock-`,
  not `esp32clock-`: four hex digits after it fit the panel's IP view
  of the name better and the prefix says what it is.
- The hold toggles, as decided: from the access point it goes back to
  the station when a network is stored and logs "hold ignored"
  otherwise. The display task acts on `Kind::Hold` and never hands it
  to `Ui`, whose clock-mode match gains an empty arm for it.
- On the access point `/api` takes the network keys without the
  token, so the page and the API agree; everything else is as in
  station mode, `/update` and `/ota` included, so a stranded clock can
  still be updated over its own network.
- The `Status` has `mode` (`sta`/`ap`) for the JSON.

Host checks (the scratch crate with `dhcpd.rs` by `#[path]`,
embassy-net 0.9.1 on `udp`, `proto-ipv4` and `medium-ethernet` builds
on the host): a discover built as a phone sends it (BOOTP head, cookie,
options 53, 61, 55, padded to 300) parses to the expected `Request`;
a wrong op, a missing cookie, an inform, 239 bytes and an option past
the end are rejected; padding inside the options is skipped and option
50 is read. The offer for it is 300 bytes with op 2, the xid and the
broadcast flag echoed, `ciaddr` zero, `yiaddr` .2, `siaddr` and
`chaddr` right, the cookie, and the options in order: type 2, server,
lease 3600, mask /24, router and DNS. A request for the offered
address from our server is an ack with type 5; one meant for another
server is silence; one for an address that is not ours is a nak with
only type and server and `yiaddr` zero; one for an address another
client holds is a nak. A second client gets .3 and the first keeps .2
across a new discover; a renewal with `ciaddr` and no option 50 is an
ack with `ciaddr` echoed. Eight offers fill the pool, a ninth is
ignored at 10 s and gets the first slot at 61 s, its request is acked,
the old holder of .2 then gets .3, and a renewal at the hour is acked
when nobody took the slot. The provisioning page with a quoted SSID and
every field at its longest is 1424 bytes with headers (the SSID
escaped in `value=`, the password absent), the saved page 998; the
JSON carries `"mode":"sta"` after `slot`. `Kind::Hold` is not host
checked: `buttons.rs` takes the GPIO type, and the logic is six lines.

Bench: a build with `WIFI_SSID` empty, flashed over USB with `nvs`
erased, booted with `settings: nothing stored`, `boot: network none,
api token built in`, `boot: access point mode, no network to join`,
`http: serving on port 80` at 345 ms (the static config is up before
the radio), `wifi: access point clock-f044 up on channel 1, page at
http://192.168.4.1/` at 1.2 s and `dhcpd: serving 192.168.4.2-9 for
3600 s`. The host's Wi-Fi scan (`nmcli device wifi list`) showed
`clock-f044`, channel 1, WPA2, signal 89. Joining it from the host was
not an option: the host's only link is that Wi-Fi, and the session
runs over it. The station build went back over USB with `nvs` erased
and the settings restored by one `/api` post; it reports `"mode":"sta"`.

Owed on the panel, by the user with a phone: `AP` on the panel; the
phone joining `clock-f044` with `esp32clock`, getting 192.168.4.2 (the
log shows the MAC joining, the discover and the offer, the request and
the ack), opening 192.168.4.1 and getting the setup form; saving the
home network: the saved page, `settings: saved`, `heal: resetting into
station mode`, the clock joining and syncing with `heals` 0. The hold:
both buttons 10 s on the working clock (the IP display starts at
0.8 s and is harmless): `AP`; the hold again: back to the clock, time
from SNTP. A `/reload` in AP mode comes back as `AP`. `heap free` in
the heartbeat in AP mode against the station's figure.

**Step 2b (user's call after the phone test, 2026-10-29): a way into
the access point from a script, and a network list on the page.**

- `POST /ap` with the `Bearer` token answers once and resets into the
  access point through `heal::switch_mode(true)`, the same shape as
  `/reload`; the PIN does not do, like `/reload`. It is how a bench
  without hands gets there, and the way back is the phone's save or
  the hold.
- The page lists the networks in range instead of a bare text field.
  The driver refuses a scan in access-point-only mode, so the access
  point is raised as `Config::AccessPointStation` with a default
  (empty-SSID) station half that is never connected: the config
  validates (the station half checks only its beacon timeout) and
  `scan_async` is allowed whenever a station half exists. The driver's
  "APSTA not supported" note is about its data path, which the station
  half never uses here; the bench says whether the AP stays up through
  a scan, with the fallback being a station-mode scan before the AP
  comes up and a rescan that bounces it.
- `net::ap_task` scans once after the AP is up (all channels, 120 to
  200 ms dwell, about 3 s) and again on `net::request_scan`, and keeps
  up to `NETWORKS_MAX` (12) of what it saw in a static: SSID, RSSI and
  whether open, hidden SSIDs skipped, one entry per SSID keeping the
  strongest, sorted by RSSI. `form::Network` holds one so `form.rs`
  stays free of the driver.
- The setup form: `<select name=ssid_pick>` with one option per
  network, `name (-45 dBm)` or `name (-45 dBm, open)`, the stored one
  selected, then an "Other, typed below" option with an empty value
  and a text field `name=ssid` for a hidden network, prefilled when
  the stored SSID is not in the list; a "Rescan" button is a second
  submit with `formaction=/scan formnovalidate`. `POST /scan` on the
  access point asks the AP task for a scan, waits for it (10 s cap)
  and redirects to `/`, so the page comes back with the new list and
  no script; elsewhere it is a 404. Both `ssid` keys set the SSID when
  non-empty and are ignored when empty, since the form always sends
  both; `ssid=` on `/api` is therefore a no-op now, not a 400. A save
  that leaves no network at all is refused with the bad-value notice.

*Step 2b built 2026-10-29 and on the bench the same day.* AP+STA
works as hoped: with `Config::AccessPointStation(StationConfig::
default(), ap)` the access point came up at 1.2 s as before, the scan
right after it saw six networks by 3.5 s (the bench AP and the house's
at -40 dBm, two neighbours at -71 and -88), and the host's Wi-Fi scan
still showed `clock-f044` afterwards, so the AP survives a scan. `POST
/ap` without the token is `403`, with it `200` and the reset with
`boot_ap: true`. Host checks: `ssid=` is unchanged, `ssid_pick` sets
and needs the token, the typed name wins over an empty pick; the setup
page with twelve 32-byte SSIDs holding `<`, `&` and `>` is 3209 bytes
with headers (the typed-name field hidden by a line of script unless
"Other" is chosen, user's call; without script it stays visible), the stored SSID selected when listed and "Other"
selected with the name in the text field when not, and the empty-list
note.

Phone, 2026-10-29 (the user, with the log read here): the list came up
with `esptest` listed; the typed-name field needed a `[hidden]
{display:none}` rule in the page's stylesheet, since the author rule
`label{display:block}` outranks the browser's own `[hidden]` rule. A
save from the list with a mistyped password reset the clock into the
station, which failed every connect with `FourWayHandshakeTimeout,
wrong password?` (and twice `Class3FrameFromNonAssociatedStation`) at
-39 dBm, as designed; the hold from there reset into the access point
with the flag, the phone rejoined with a reboot-style request for its
old address (acked, no discover), the page came back with `esptest`
preselected, and the save with the right password brought the clock
back on the network in 2.9 s, synced at 5.8 s, `heals` 0.

**Buttons held across the reset, a precaution:** the colour and
brightness were found changed after the holds, which the user said
were their own short presses, so nothing was observed wrong. But the
chip resets at the 10 s mark with both buttons still down, the new
boot sees them released, and a release within 800 ms of the boot would
count as two short presses. `Tracker::new` now takes whether the
button is down at start and begins stable-down with nothing pending,
so the release of a press that began before the reset emits nothing.
Three lines; not verified, since nothing was seen to fix.

**Step 3, after the plain flow works: captive DNS.** A UDP task on
port 53 that answers every A query with 192.168.4.1, so a phone's
portal check opens the page by itself. Needs one more `StackResources`
slot and about 60 lines; left out until a phone has been seen joining
and provisioning through the plain flow.

**Touches, in order.** `auth.rs` (new, out of `ota.rs`, with the
rename), `settings.rs` (Step 0 tolerance, `Text`,
`Credentials`, record 6, seed), `storage.rs` (scratch, the switch
trigger), `form.rs` (keys, escaping, JSON, the two pages), `carry.rs`
(`boot_ap`, layout 4), `heal.rs` (`switch_mode`), `net.rs`
(credentials from the settings, the reason hints, `Mode`, `ap_task`),
`dhcpd.rs`, `http.rs` (the mode, the provisioning page, `Then::Join`),
`display.rs` (`AP` face, the hold), `buttons.rs` (`Kind::Hold`),
`main.rs` (the seed, the mode decision, the two spawn lists),
`config.rs` and `build.rs` (`API_TOKEN`), `scripts/ota.sh`,
`scripts/reload.sh` and the new `scripts/pin.sh`, `lib.rs` (`dhcpd`), AGENTS.md (build section
with the renamed variable, controls table, layout table, the sizes
list: `RECORD_LEN` 167, scratch 256, value buffer 64; the token
section: set from the update page, never shown).

**Host checks.** Record round trip at version 6 with a 32-byte SSID,
a 64-byte password and a 32-byte token; version 5 read as `NONE` and
no token; the token rule at 7, 8, 32 and 33 bytes and with a space; version 6 read by
the Step 0 parser as a version 5 prefix. `Credentials::is_valid` at
the edges (SSID 0 and 33 bytes, password 7, 8, 64 and 65).
`form::apply` with `ssid`/`password` through percent-decoding of a
space and a `%26`, and refused with `Forbidden` and nothing applied
when `credentials` is false, whatever else the body holds; `JsonStr` and `HtmlAttr` on `a"b\c<d&e`. The
provisioning and the saved pages, and the update page with its second
form, against `RESPONSE_BUF`. `dhcpd`:
`parse` and `build` against a discover/offer/request/ack exchange
captured with `tcpdump -i wlan0 -w` on a laptop hotspot with a phone
joining; the lease table keeping a MAC on its slot, reusing an
expired one and ignoring a ninth. `Kind::Hold` timing in a scratch
copy of `buttons.rs` with the inputs stubbed.

**Verify on the bench and the panel.** Step 1 as above. Step 2:
flash a build without `WIFI_SSID` over an erased `nvs` (`espflash
erase-region 0x9000 0x6000`): the boot line says `access point`, the
panel shows `AP`, a phone sees `clock-xxxx`, joins with the password,
gets 192.168.4.2 (the log shows the MAC joining and the DHCP
exchange), opens 192.168.4.1 and gets the provisioning form; saving
the bench network's credentials answers the saved page, the log shows
`settings: saved` then the switch reset with `boot_ap: false`, the
boot line says `station`, the clock joins and syncs, and the record
is not carried. Save a wrong password instead: the clock stays blank,
the log says `wrong password?` every attempt, and it never shows `AP`
on its own; hold both buttons 10 s: `AP` again. Power the bench AP off
with the clock running: it reconnects when the AP returns without
showing `AP`. Hold both buttons 10 s on a working clock: `AP`; hold
again: back to the clock, time from SNTP, `heals` 0 in the boot line
whatever the count was before. A `/reload` in AP mode comes back as
`AP`. Watch `heap free` in the heartbeat in AP mode against the
station's figure; the softAP has its own driver buffers.

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

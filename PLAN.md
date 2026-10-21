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

Settings live in RAM; a reboot returns to the compile-time defaults.

### M5 — persistence (next)
- Keep colour, brightness and the alarm across power cuts in flash (an
  NVS-style key-value store on a dedicated partition, e.g. via
  `sequential-storage` over `esp-storage`), written only when a setting
  changes and read once at boot.
- Settings move out of the display task into a shared store with a change
  signal, so both the buttons and M6 can edit them and the display redraws
  on either.

### M6 — settings over Wi-Fi

A small HTTP page served by the clock on its LAN address.

- **Server.** `picoserve` on embassy-net TCP (it targets exactly this
  stack), one or two sockets, no TLS. A single HTML page with a form:
  colour (palette choice or a colour picker mapped to `Rgb`), brightness,
  alarm time and armed, 12/24 hour, timezone offset. `GET /` renders the
  current values, `POST /` applies them and redirects back. A `GET /api`
  JSON view and `POST /api` make it scriptable from Home Assistant or a
  shell.
- **Shared settings.** The store from M5 is the single source of truth; the
  page and the buttons both write through it and the display picks up the
  change signal. Timezone offset and 12/24 hour become settings rather than
  build-time constants.
- **Finding it.** Log the IP at boot (already done) and show it on the panel
  on a long press of both buttons, four digits at a time, so no serial
  console is needed. mDNS (`clock.local`) is possible with embassy-net's
  `mdns` feature and worth trying once the page works.
- **Out of scope for M6.** Wi-Fi credentials stay build-time. Provisioning
  through an access-point captive portal is its own milestone, since it
  needs AP+STA mode, DHCP server and a DNS catch-all.
- **Risks.** Flash and RAM: the HTTP server and page are small, but
  embassy-net TCP buffers and another socket add a few KB; heap is at
  about 54 KB free with Wi-Fi up. Keep the page static and tiny, no
  JavaScript needed. The server task runs in thread mode and never touches
  the scan path.

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

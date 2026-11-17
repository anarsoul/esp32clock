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
| 0    | NTC divider R_T (adapter 10 k to 3V3, board NTC R4 to GND) | ADC1 channel 0, 11 dB, `AdcCalCurve` (M11a, 2026-11-01) |
| 1    | LDR divider R_P (adapter 10 k to 3V3, board LDR R3 to GND) | ADC1 channel 1, same |

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

- ~~**Alarm lost while in another mode.**~~ Not a bug, decided 2026-11-02:
  whoever is in alarm-set or watching the IP or the PIN has just
  pressed the buttons and is awake; the clock has nothing to wake.
  `Ui::tick` only checks the alarm
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
- ~~**First wake of a fresh watch receiver.**~~ Moot since the third pass took
  the `try_changed` calls out of the display task (2026-10-29); `hostname_task`'s
  first wake sees the boot hostname and does nothing. Checked 2026-10-30. embassy-sync starts a
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
- ~~**Unused public API.**~~ Done after M10 (colour in levels, dead state and nits), 2026-10-29; `set_position_color` went too once the trail was dropped. `Panel::set_time`, `set_segment`,
  `set_position_color`, `set_dash`, `Levels::OFF`, `clock::is_synced`,
  `aip33628::IS_MA_TENTHS` and `scan::SLOT_US` have no callers; the lib
  crate hides the warnings. `Panel::lamp_test` is also unused but worth
  keeping as a hardware aid.
- ~~**Gap statistics on a DMA start failure.**~~ Done with the gap
  measurement change of 2026-11-07: the retry resets the reference
  start. The retry path in
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
  and no address as a failed probe. Left as is, 2026-11-02: a corner case
  that needs a link to die in the last minutes of its lease, while a
  marginal link trips the probe budget well before that. The fix, if
  it is ever wanted, is the bounded wait above with the no-address
  period scored like a lost ping, and the streak logic moved into
  `core` under test.
- ~~**The sweep's trailing dash is dark and blinks the armed mark.**~~
  Done after M10 (colour in levels, dead state and nits), 2026-10-29.
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
- ~~**A `POST /` without `Content-Length` disarms the alarm.**~~ Done
  2026-11-07 (the codex review's item 12): `411`, and `400` for a
  `Transfer-Encoding`.
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
- ~~**A heal reset can land mid-transfer during a failing upload.**~~
  Done 2026-11-07 (codex item 7): `scan::halt` latches the stop for
  the deliberate resets and `resume` honours the latch.
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
- ~~**The carry record has a mark but no layout.**~~ Fixed with M10: `MARK`
  carries `LAYOUT` in its low byte and a mismatch is no mark (checked
  2026-10-30). `decode` trusts word
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
- ~~**The missed-alarm grace is used up by the first time in any mode.**~~
  Not a bug, decided 2026-11-02, for the same reason as the first
  pass's "alarm lost while in another mode": a boot that is in the IP
  or PIN display when the first time lands has someone at the clock
  or at its update page, awake.
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
- ~~**A failed save is never retried.**~~ Done 2026-11-07 (codex item
  10): tried again after 2 s, doubling to 60 s. `persist_task` logs an `Err` from
  `store_item` and goes back to waiting for the next change, so the
  unsaved settings live only in RAM until something else changes. Fix:
  re-arm the settle timer on an error.
- ~~**A reset inside the settle window loses the last change.**~~ Not a
  bug, decided 2026-11-07: a reload a second after a save is the
  operator's own doing, and the save follows two quiet seconds. Changes
  reach flash two seconds after the last one; `heal::reboot` carries
  the time and alarm state but not the settings, so a colour or arming
  change made just before the probe budget fires is gone after the
  reboot with no indication. Reproduced 2026-10-28: `POST /api
  color=green` then `scripts/reload.sh` at once comes back with the
  old colour. Fix: have `heal::reboot` flush the persist task (or save
  synchronously through `with_flash` after `scan::stop`).
- ~~**The upload does not check the chip id.**~~ Not worth a check, decided
  2026-11-02: the bootloader refuses the image and falls back to the
  old firmware, which is the recovery the item itself describes.
  The first sector is checked
  for the image magic, the descriptor magic and the hash flag, not for
  the ESP32-C3 id at bytes 12–13 of the extended header. An image built
  with the wrong `--chip` is written, selected and booted; the
  bootloader falls back, and the old image then logs that it is itself
  on probation. Recovery works; the first failure is confusing. Fix:
  refuse unless `sector[12..14] == [0x05, 0x00]`.

Nits:

- ~~**The display task freezes during an upload.**~~ Not a bug, decided
  2026-11-07: the panel is dark for the write anyway. With the scan halted
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

- ~~**Sub-second time on a minute display.**~~
  Dropped by the user 2026-10-29: kept as it is. The clock is an `i64` of
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
- ~~**Colour through 8-bit RGB and two lookup tables.**~~
  Done after M10 (colour in levels, dead state and nits), 2026-10-29; 75 % is step 9, not 8. The palette is
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
- ~~**Instrumentation from the dropout campaign.**~~
  Dropped by the user 2026-10-29: kept as it is. Four pieces measured a
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
- ~~**Dead state.**~~ Done after M10 (colour in levels, dead state and nits), 2026-10-29. `ota::SELECTED` is stored at `init` and never read
  (the log line uses the local); `carry::SOFT` and `soft_reset` have no
  caller; `clock::is_synced` neither (first-pass item). With the panel
  methods of the first pass (`set_time`, `set_segment`,
  `set_position_color`, `set_dash`, `Levels::OFF`, `IS_MA_TENTHS`,
  `SLOT_US`) that is about 50 lines the lib crate hides warnings for.
- ~~**Nits.**~~ Done after M10 (colour in levels, dead state and nits), 2026-10-29, except `Counting`, kept. `PinState::check` compares a one-attempt, four-digit PIN in
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

### Review backlog, fourth pass (2026-10-30): the code since the second pass

A bug review of the whole tree at commit aa6d0de, read in the same
four slices as the second pass (net/dhcpd/dnsd/main/heal;
http/ota/auth/form/scripts; settings/storage/config/carry/clock;
ui/display/buttons/buzzer/melody/scan/panel/aip33628) by four
independent readers, each item then confirmed in the code path by a
fifth, against the locked sources where a claim depended on them
(esp-hal 1.2.2, esp-radio 1.0.0-beta.1, embassy-net 0.9.1, smoltcp
0.13.1, embassy-sync 0.7.2, sequential-storage 8.0.2,
esp-bootloader-esp-idf 0.6.0). Everything since 0e85ee9 had only had
the over-engineering pass: `/reload`, the tunes, the credentials and
the token as settings, the newer-record prefix read, the access point
with its DHCP and DNS servers, the captive redirect, colour in levels,
the core split and the setup page budget. Those are where the findings
are: the setup form has three ways to lose the network or a field, and
the seed undoes a cleared token. Nothing in the scan path, the encoder,
the carry codec, the record decode, the auth rules or the upload is
listed. Clippy is clean and the 63 host tests pass. The four
setup-form and token bugs and the PIN gap were built the same day,
see the milestone after the setup page budget; the rest is open.

A second, independent review of the same tree by another model
(`codex-review.patch`, 2026-10-30) was read against this one and
agrees with it on every point it makes; nothing in it contradicts a
finding here or adds one. Its nine items are six earlier backlog items
re-confirmed still open (the self-heal lost once the lease expires,
the alarm lost in another mode together with the grace used up in any
mode, the heal reset landing mid-transfer during a failing upload, the
preview silencing a ring, the display freeze during an upload, and the
`POST` without `Content-Length`) and three of the bugs below (the
hidden "other network" field, the join path losing the other fields,
and the cleared token). It ranks the lease-expiry self-heal, the alarm
lost in another mode and the upload-reset race as the three highest
priorities, above the setup-form bugs, which is a fair order for a
clock that is already provisioned. Its evidence is worth keeping:
host reproducers outside the repository confirmed four of the items by
assertion, the hidden-SSID overwrite (`ssid_pick=new&ssid=old&
password=…` saves `old` with the new password), the token restored
after a clear (through `parse_token`, the record round trip and the
boot seed with a test `API_TOKEN`), the grace consumed by a first time
at 07:05 during a PIN display for an armed 07:00 alarm, and a bodiless
`POST /` clearing armed, twelve-hour and skip; those four are the
regression tests to write with the fixes. One toolchain note from it:
on Rust 1.99.1 `cargo clippy --release` flagged `chunks_exact_mut` at
`scan.rs:144`; the toolchain here was bumped to 1.99.1 the same day
and the loop now uses `as_chunks_mut::<CYCLE_BYTES>()`, which is the
same copy (the remainder is empty by the transfer arithmetic). Clippy
is clean again.

Of the earlier passes, two items turned out done and are struck in
place: the carry mark now carries a layout version (second pass), and
the first wake of a fresh watch receiver is moot since the third pass
took the `try_changed` out of the display task (first pass). Every
other open item was re-checked and is still open; two of them gained a
case here (the settle window and the failed save, on the setup form).

Bugs, in rough order of value:

- ~~**Re-saving the setup form with the stored network wipes its
  password.**~~ Done 2026-10-30, verified from a phone (`form::settle_password`). The form lists the stored network pre-selected and its
  password field empty (`form::write_provision_page`, the field is
  write-only), and `apply_one` takes `password=` as a valid open-network
  password and stores it. A clock held into the access point to change
  the timezone or the hostname, saved with the network left as listed,
  posts an empty password: `trial.wifi != current.wifi` in `http.rs`'s
  `POST /`, the record is saved, the persist task resets into the
  station, and the WPA2 network refuses it. Blank panel, `wrong
  password?` every attempt, another 10 s hold and a retyped password to
  recover. Found by two readers. Fix: on the setup form an empty
  password with the SSID unchanged keeps the stored one; a listed
  network the scan says is secured (`Network::open` is false) can refuse
  an empty password outright.
- ~~**The hidden "other network" field overrides the list's pick.**~~ Done 2026-10-30, verified from a phone (the pick wins in `form::apply`, the field is disabled). Also found by the other review, with a host reproducer. When
  the stored network is not in the scan list, the page selects "Other"
  and puts the stored name in the typed field; the page script hides
  that field when a listed network is picked, but a hidden input is
  still submitted, and `apply_one` applies `ssid_pick` and `ssid` in
  body order with "non-empty sets the SSID", so the typed name wins. A
  clock provisioned at home and held into the access point elsewhere:
  the user picks the new network from the list, types its password,
  saves; the clock stores the old name with the new password, says it
  joins the new one, resets and never joins, and the field that did it
  is invisible on the next try too. Fix: disable the typed input when a
  listed network is picked (a disabled control is not submitted), and
  let a non-empty `ssid_pick` win over `ssid` server-side.
- ~~**Timezone and hostname changes on the setup form are lost when the
  network is unchanged.**~~ Done 2026-10-30, verified from a phone (every save on the access point leaves it). Also found by the other review. The `Then::Join` branch commits `trial` with
  `settings::update` and `http_task` calls `heal::switch_mode(false)`
  right after the flush, inside the persist task's 2 s settle, so
  anything else the form carried never reaches flash and the station
  boots with the old values after a page that said they were kept. The
  changed-network branch is fine, since the save itself triggers the
  reset. The second pass's "reset inside the settle window" on the
  ordinary provisioning path; `POST /ap` and the hold are two more
  unflushed resets. Fix: have the persist task do the reset whenever the
  record differs from the boot value, not only `wifi`, or save
  synchronously through `with_flash` before any `switch_mode` and in
  `heal::reboot`.
- ~~**Clearing the API token does not survive a reboot on a build with
  `API_TOKEN`.**~~ Done 2026-10-30, verified on the bench (record version 7, `token_cleared`). Also found by the other review, with a host reproducer. `POST /token` with an empty value stores an empty
  token, but `Settings::seed` refills any empty token from the build
  constant and `main.rs` runs it at every boot over whatever was
  loaded, with no notion of whether the record ever held the field. The
  user closes the script paths, the clock reboots (self-heal, `/reload`,
  power cut), the boot line says `api token: built in`, and the next
  colour change writes the built-in token back into the record for
  good. Fix: seed only when the loaded record predates version 6 or
  nothing was loaded (`from_record` can report the version); a stored
  empty token then means none.
- ~~**A two-button slap to silence the alarm changes a setting.**~~ Done
  2026-11-02, see "Quiet after the press that ends a ring" at the end:
  a one-second quiet window in `Ui` after the press. The
  button task emits one `Short` per button on release and there is no
  `Both`/`Short` event, so two buttons released a few ms apart are two
  events: `Ui::handle` ends `Ringing` on the first and the second lands
  in `Clock`, where a top short steps the colour and a bottom short the
  brightness, and the persist task writes it. The same for a hurried
  double press of one button and for a press that ends the IP or PIN
  display. Fix: a `quiet_until` in `Ui`, about 500 ms after a press
  ends `Ringing`, `ShowIp` or `ShowPin`, dropping events before it; or
  fold two shorts within 100 ms into `Both`/`Short`, which the `Clock`
  arm already ignores.
- **`POST /scan` can only answer within the socket's 5 s idle
  timeout.** `rescan` waits up to `SCAN_WAIT` (10 s) for the AP task's
  scan, during which the browser sends nothing, and smoltcp's socket
  timeout is an idle timeout: `remote_last_ts` moves on every received
  segment and `dispatch` closes the socket at `remote_last_ts +
  timeout` with no regard to pending transmit data. A scan slower than
  5 s (13 channels at up to 200 ms is about 2.6 s today) answers into a
  closed socket and the phone shows a reset instead of the page. Fix:
  raise the timeout for that request, or cut `SCAN_WAIT` under
  `TIMEOUT` and say so in the comment.

Gaps:

- **A record the map scratch cannot hold wipes every setting on
  rollback.** `Store::load` erases the whole map on any `Err` from
  `fetch_item`, but sequential-storage returns `BufferTooSmall` before
  anything is corrupt when an item does not fit (`Corrupted` is already
  repaired inside `fetch_item`). The scratch is 256 bytes and a
  version-6 item is 176 (8-byte header, 1-byte key, 167 of record; the
  9-byte claim holds); the record grew 99 bytes in the last version, so
  one more set of fields of that order (a second network) takes a
  version-7 item past 256. An image with that record saves it, the
  `OTA_BOOT_LIMIT` rollback boots this one, `load` logs "cannot read the
  map, erasing it", and the clock comes up with hostname `clock`,
  timezone 0 and no network, the exact case the newer-prefix read was
  built to prevent. Fix: erase only on `Corrupted`, treat
  `BufferTooSmall` as "not understood, using defaults" without erasing,
  and size the scratch to a page.
- **A string-rule change in a newer image throws the record away on
  rollback.** `from_record` clamps `color`, `melody` and `brightness`
  for exactly this case, but the version-6 strings go through
  `is_valid`, so a password of 1 to 7 bytes or a token outside
  `valid_token`, both legal in a later image that relaxed the rule,
  make this image discard the whole record. Fix: a bad string field
  falls back to its default (`Credentials::NONE`, empty token) and the
  rest is kept.
- ~~**The raw record, password and token included, goes into the serial
  log.**~~ Done 2026-11-07 (codex item 14): the version and the length. `Store::load` logs `stored record {record:?}` when a record is
  not understood; the bytes hold the Wi-Fi password and the API token
  in the clear, unlike `Settings`'s `Debug`, which prints `<set>`. Fix:
  log the version byte and the length.
- ~~**Disarming from the page or `/api` does not stop a ring.**~~ Not a
  bug, decided 2026-11-07: whoever disarms it from the page is awake,
  and MQTT has `silence`. In
  `Mode::Ringing` `tick` looks only at `RING_TIMEOUT`; `alarm_armed`,
  which a form post or `/api alarm=off` clears, is read only in the
  `Clock` arm's due check. The display task wakes on the change, ticks,
  and keeps ringing for up to five minutes until a press. Fix: in the
  `Ringing` arm, `enter(Clock)` when `!settings.alarm_armed`;
  `last_alarm` is already set, so the grace does not ring it again.
- ~~**Two `/scan` posts at once thrash the executor and leave one waiter
  for the full timeout.**~~ Done 2026-11-07 (codex item 13): a scan
  count each waiter polls. `net::SCAN_DONE` is an embassy-sync `Signal`
  and `rescan` is reachable from every listener. `Signal::poll_wait`
  with a second, different waker replaces the first and wakes it, so
  two listeners waiting on `scan_done()` wake each other in a loop
  until the scan signals (about 3 s of thread-mode spinning; the panel
  is on the interrupt executor and is fine); the value is then taken by
  one waiter and the other sits until `SCAN_WAIT` and redirects. A
  double click on Rescan, or a phone's captive sheet and a browser tab,
  is enough. Fix: a scan generation counter with a `Watch`, which any
  number of waiters can wait on.
- ~~**A bad token body burns the PIN.**~~ Done 2026-10-30, verified on the bench (the body is parsed first). `POST /token` spends the PIN in
  `token_or_pin` before `parse_token` looks at the body; a 7-character
  token answers 400 and the page's second try answers "no PIN issued:
  open the update page first". Fix: parse the body first (a parse
  changes nothing), then spend the PIN; the page script can also check
  the length.
- **`/api` on the access point saves a password with no network.**
  Since 2026-10-30 the persist task resets off the access point only
  with a network stored, so this is a pointless save and no reset;
  the guard on `/api` is still missing. The
  `trial.wifi.is_none()` guard is only on the form path; `POST /api
  password=…` on an unprovisioned access point saves, differs from
  `boot_wifi`, and the persist task resets into station mode with an
  empty SSID, which the boot turns back into an access point. A
  harmless loop, but the script sees "saved" and a reset for nothing.
  Fix: the same guard on `/api` in access-point mode.
- **The OTA rollback carries a stale time.** `ota::init` runs before
  `clock::restore_carried` in `main.rs`, so its `carry_time()` is a
  no-op (`unix_micros` is `None`) and the record keeps the time written
  at the end of the previous boot; the restore then adds `RESET_PAUSE`
  to a value older by that boot's span up to `ota::init` plus its own
  pause, about a second, shown until NTP. Fix: `restore_carried` before
  `ota::init`.
- **Without a usable `nvs` partition a provisioning save never leaves
  the access point.** The reset into station mode lives in
  `persist_task`, spawned only when `Store::open` succeeded; with no
  store the setup page still says the clock will reset and nothing
  does, while the unchanged-credentials path does reset. A bad
  partition table only; answer differently or reset from the listener
  when there is no store. The same shape as the open "failed save is
  never retried": on the access point a failed save shows the saved
  page and stays an access point until the next change.
- **The access point's transmit ceiling is set before the driver is
  restarted.** `ap_task` calls `set_max_tx_power` and then
  `set_config(AccessPointStation)`; `WifiController::new` has already
  started the driver as a station, so that `set_config` is a mode
  change, which stops and restarts the driver (`esp_wifi_stop`,
  `esp_wifi_set_mode`, `esp_wifi_start` in esp-radio). ESP-IDF
  documents the power call as "after WiFi start"; whether the value
  survives the restart is inside the blob and was not verified. The
  station path has no restart. Fix: move the call after `set_config`,
  so the boot line says what is in force.

Nits:

- **The display loop wakes every 64 ms for nothing on the access point
  and while a PIN shows.** The redraw wait is `BLINK_STEP / 4` for every
  view but `Time` and `Ip`, right for the blink and the sweep, but
  `View::Sweep` on the access point is a static `AP` for the whole boot
  and `View::Pin` is static for its minute. Each wake runs
  `carry_time`, a `settings::update` under the watch's critical section
  (which holds off the interrupt executor for its duration), a render
  and the image compare. Fix: wait `PIN_SHOW` for the PIN and a second
  for the sweep when `net::mode()` is the access point.
- **`Store::open` logs inside the flash critical section.** The
  `error!` lines for a missing partition table or `nvs` entry run
  inside `with_flash`'s `critical_section::with`, so the serial write
  stalls everything. Boot-only, before the scan starts; move the match
  outside the closure.
- **A discover from a client that already holds a lease cuts the lease
  to the offer hold.** `Leases::handle` writes `now + OFFER_HOLD_SECS`
  (60 s) over an existing slot when `find` hits. A phone that
  re-discovers and then does not request keeps using the address while
  the slot is free again after a minute and can be offered to the next
  client. Fix: on a discover, keep the later of the two expiries.
- **`dnsd::answer` indexes `out` by the query's length.** `out[HEADER..n]`
  with `n` up to `query.len()` is only checked against `PACKET`; a
  caller passing a query longer than `out` panics. The task's buffers
  are both 512 so it cannot happen on the air; a host test feeding a
  long query would hit it. Fix: `get_mut(HEADER..n)?` like the record
  copy below it.
- **`REQUEST_BUF` is tight for the setup form from a phone.** The head
  and the inline body share 1536 bytes; the setup form at its worst is
  about 457 bytes encoded (two 32-byte SSIDs and a 64-byte password,
  percent-escaped), leaving about 1080 for the head, and a mobile
  browser's POST head to a captive page is about 750 bytes before any
  cookie another gadget at 192.168.4.1 may have set. Over the limit is
  the silent close of the open "oversized requests" item, on the one
  page whose user cannot see the log.
- **No error from `ap_task` is ever healed.** A failed `set_config`
  logs once and sleeps for ever; the panel shows `AP` with no network
  and only a power cycle or a hold helps. Unlikely with the
  compile-time-checked SSID and password; a log line is probably all
  that is worth doing.
- **Stale comments.** `net::Mode`'s doc says the driver does not do
  AP+STA, which `ap_task` does for the scan; `http.rs` still says an
  upload is 800 KB where the image is about 650 KB (the second pass's
  `check_rollback` half is fixed).

What checked out and is not listed: the frame and transfer arithmetic
(`UNIT_BYTES` 50 over `FRAME_BYTES` 47, 22 cycles plus the blank frame
in 30847 of 32000 bytes, every region's latch at the same offset); the
encoder against the model test and the reserved bits; the two-buffer
hand-off; nothing slow between transfers but the kept `GapStats`
section; the buzzer's open-drain line on every exit (esp-hal 1.2.2 has
no `Drop` on `Output` or `Flex`) and the `Signal` command slot; the
melodies (no zero length, `pick` never yields 0 or `avoid`, no
overflow); `Instant - Instant` always non-negative in `tick` and the
trackers; the tracker after a hold reset and the `Both`/`Long`
suppression; 00:xx and 12:xx in 12-hour mode; `came_round`, the
midnight and weekday carry, `alarm_on` against `weekday_of_days`, the
grace and skip logic, the heal hold's bound; `dhcpd::parse` and
`dnsd::answer` on hostile input (every read through `get`, pointers
refused), the DHCP state machine against RFC 2131 and the broadcast
reply's route; `StackResources<10>` against at most eight sockets;
`RECEIVERS` against three receivers; the atomics as load/store with one
writer; the flash handle never across an await; `set_config` on a
reconnect not restarting the driver; `freeze` against `update` in the
switch pause; the reset-reason gate and the mark; `partitions.csv` and
the runner's `otadata` erase; `read_request`'s two-pass parse and the
inline body bound; `parse_head` on partial reads and bad lengths; the
per-listener `Response` buffer (no cross-listener race) and the clean
500 on overflow; `drain` and the 503 exception; `install`'s `total <
SECTOR` guard, slot bound, `hash_chunk` at every boundary, padding,
one `with_flash` per sector, `otadata` untouched and `resume` on every
failure, `next_ota_part` skipping the booted slot by the MMU entry,
`confirm` before `reboot`; `token_ok` visiting the whole secret with an
empty token never matching, the PIN's one attempt, TTL and lockout; the
network keys refused on the key alone and the JSON's `password_set`
and `token_set`; `JsonStr`/`HtmlStr` on the SSID; the update page's
upload, poll and token scripts; the page sizes under the tests;
`version_from` and the 31-byte version; the scripts; `to_record` and
`from_record` for every version and the newer prefix, an empty slice,
non-UTF-8 strings (rejected), `RECORD_LEN` against `store_item`'s
buffer; `Secret`'s `Debug` everywhere a `Settings` is formatted; the
const asserts on the build-time strings; `seed` with an empty
`WIFI_SSID` (not seeded, the boot is an access point);
`send_if_modified` on embassy-sync 0.7.2; the persist compare and its
`boot_wifi` test in both modes; every `carry::update` on the thread
executor with no await inside; the codec's zero-means-none and 2106;
`local_time` and `weekday_of_days` on negative times and the `i16`
timezone extremes; `OFFSET_US`'s two writers behind `critical_section`;
`parse_i32`.

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
Wi-Fi provisioning from an access point, then (added 2026-10-29) a
`hosttest/` crate so the scratch-crate checks become `cargo test`,
then the two sensors the board already carries. Each entry is the design as planned; it gets the
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

### Carry the time across a reset as plain Unix time (after the ring hold) — verified on the bench 2026-10-28 at short uptime (five software resets, serial log); the carry at 4.5 h of uptime checked in the soak of 2026-11-08

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
  scheme it replaced, so the hour-long soak was a formality. Paid on
  2026-11-08: the OTA reboot into v0.3 at 4h31m of uptime carried the
  time to the second, and the NTP sync 6 s later kept it (the soak
  section at the end).
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
  - `reveille`, the US bugle call, from the user's score
    (`reveille.png`, 2026-10-30): eight bars of cut time on the bugle
    notes, at 160 bpm in quarters (the score's half note of 80).
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

**Reveille from the score (2026-10-30).** The user found the notes
wrong and supplied a trombone score (`reveille.png`, bass clef, cut
time, half note 80). It reads G | C E C G E | C E C G E | C E C G C |
E (half, slurred) C G | C E C G E | C E C G E | C E C G G | C (whole):
quarters with one pair of eighths in each bar, and no dotted figure
anywhere; the first cut had the dotted-eighth and sixteenth of the
other bugle calls, a wrong fourth bar and only three bars repeated.
Rewritten from the score on the same three pitches (G5, C6, E6, the
range is a sixth, so nothing new for the buzzer) at 160 bpm in
quarters, the score's tempo; an eighth is 188 ms and a pass 12.4 s.
The bar with the two G quarters in a row is where the articulation gap
in `Note::sounding` is heard. Heard on the bench 2026-10-30: better.

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

### M10 — Wi-Fi provisioning from an access point — DONE, verified on the bench and with a phone 2026-10-29

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

**Step 3: captive DNS (planned 2026-10-29, after the phone test).**
`src/dnsd.rs`: a UDP task on port 53, access-point mode only, that
answers every query for an A record with 192.168.4.1 and a 60 s TTL,
and any other type with an empty NOERROR answer (so an AAAA lookup
does not make the phone wait for IPv6). The pure part, `answer(query,
out) -> Option<usize>`, checks the header (a query, opcode 0, one
question), walks the labels to the end of the question, copies the
question back, and writes the answer with a name pointer to it; 512
bytes of buffer each way, about 80 lines with the task. The DHCP
options already name the clock as the DNS server, and the stack's
static config does too, so nothing else changes on the network side.
The phone's connectivity probe (`connectivitycheck.gstatic.com`,
`captive.apple.com`) then reaches the clock's HTTP server, which must
not answer the probe's path with a `404`: in access-point mode every
path that is not a route answers `302 Location:
http://192.168.4.1/`, which is what both Android and iOS take as a
captive portal and open in their sign-in sheet, with the setup form
in it. Station mode keeps its `404`. `StackResources<10>` holds: the
access point uses the DHCP and DNS sockets and the three listeners,
fewer than the station's. Host check: a query for `captive.apple.com`
A answers with the ID, the flags `0x8180`, the question copied, one
answer with the pointer, type A, class IN, TTL 60 and the address;
AAAA answers with no records; a response, a truncated question and a
two-question packet are ignored. Bench: `POST /ap`, then the phone
joining should pop the sign-in sheet by itself with the setup page in
it, and the log shows the queries answered.

*Step 3 done and verified on the panel with a phone 2026-10-29.* Built
as planned (`dnsd.rs` 150 lines with the task and the name for the
log; the `302` in `http::respond` for any non-route in access-point
mode). Host check as listed, plus RD clear copied and a short output
buffer refused. Bench, with the user's Android phone: 0.3 s after its
lease it looked up `www.google.com` and `connectivitycheck.gstatic.com`
and got the clock; its `GET /generate_204` got the redirect; 5 s later
it fetched `/`, which is the sign-in sheet opening with the setup
page, with `/generate_204` re-probed while the sheet was up and a
`content-autofill.googleapis.com` lookup from the form; the save
posted `esptest` unchanged and the clock was back on the network and
synced within 6 s with `heals` 0. The requests logged after the
"resetting into station mode" line were served by the other listeners
in the 200 ms before the reset fired.

M10 is complete: steps 0 to 3 verified on the bench and, for the
access point, on the panel with a phone, and the user confirmed the
whole flow on the real clock the same day. Still to read once: the
heartbeat's `heap free` in access-point mode (the minute `alive` line
in `main.rs`, which logs in either mode with the probe counts at zero),
since the soft-AP has its own driver buffers in the same heap. Read
2026-10-29 on the bench, five minutes in access-point mode with no
phone attached: `heap free` 49820 bytes at every heartbeat from 60 s to
300 s, against 54084 as a connected station, so the soft-AP costs
about 4.3 KB and nothing grows while it idles.

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

### Colour in levels, dead state and nits from the third review pass (after M10) — DONE, verified on the panel 2026-10-29 (colours, brightness, then the single-dash sweep; the PIN not re-checked)

Items 2, 4 and 5 of the third-pass backlog, chosen by the user; items
1 (sub-second time) and 3 (dropout instrumentation) were dropped by the
user and struck from the backlog as "keep". Only the first changes what
the panel shows, and only in the sweep's tail.

- **Colour in duty levels, not 8-bit RGB.** `settings::PALETTE` is
  eight `panel::Levels` written in the panel's blue-green-red order:
  red `(0, 0, 7)`, orange `(0, 1, 7)`, yellow `(0, 7, 7)`, green
  `(0, 7, 0)`, cyan `(7, 7, 0)`, blue `(7, 0, 0)`, magenta `(7, 0, 7)`,
  white `(7, 7, 7)`: exactly what `GAMMA` made of the old `Rgb` values
  (255 → 7, 96 → 1), so the panel's colours do not change. `Rgb`,
  `GAMMA`, `Levels::from_rgb` and `Panel::set_digit_color` are gone;
  `Panel::set_color` and `set_position_color` take a `Levels`.
  Brightness: `BRIGHTNESS_CHOICES` is five `(percent, step)` pairs,
  `(10, 1)`, `(25, 2)`, `(50, 5)`, `(75, 9)`, `(100, 15)`, read off
  the old `BRIGHTNESS_STEP[101]` table (the backlog said 8 for 75 %;
  the table says 9). `brightness_count` compares the step directly;
  `Settings::current_step` returns it; `BRIGHTNESS_STEP` and
  `current_step_for` are gone. The display no longer clamps the step
  to `MAX_CURRENT_STEP`: every offered choice is within the ceiling by
  construction of `BRIGHTNESS_STEPS`, and the stored index is reduced
  modulo its length. The sweep's tail is `Levels::dimmed`, each
  channel halved rounding up (7 → 4, 1 → 1, 0 → 0), applied with
  `set_position_color` to the tail's dash segment only. That is the
  second-pass fix for "the trailing dash is dark and blinks the armed
  mark": the old `dimmed` scaled RGB by a quarter and `GAMMA` of
  anything under 77 is 0, so the tail was never lit and the degree
  mark went dark one step in four. Both halves struck. **Seen on the
  panel 2026-10-29, the user asked for the single dash instead:** the
  tail, `Levels::dimmed` and `set_position_color` are gone and
  `draw_sweep` lights one dash per step. The degree mark is steady by
  construction now, since nothing recolours a block during the sweep.
- **Dead state.** `ota::SELECTED` (stored, never read), `carry::SOFT`
  and `soft_reset`, `clock::is_synced`, `Panel::set_time` with its
  `digit` helper, `set_segment`, `set_dash`, `Levels::OFF`,
  `aip33628::IS_MA_TENTHS` and `scan::SLOT_US` (folded into
  `CYCLE_US`) are deleted. `set_position_color` stays, now with a
  caller. `Panel::lamp_test` stays as the hardware aid the first pass
  kept it for. `settings::update` no longer returns whether it
  changed anything; the display was the only reader.
- **Nits.** `PinState::check` compares the digits as a number:
  `presented.len() == 4 && presented.parse::<u16>() == Ok(pin)`.
  A one-attempt, four-digit PIN with a lockout gains nothing from a
  constant-time compare; `bytes_eq` stays for the token. The three
  `try_changed` calls in `display_task` are gone: the loop's own
  update wakes its receiver once more, renders the same image and
  publishes nothing. `Hostname::new` is a `const fn` (a `while` loop
  over the bytes instead of `zip`) and `Hostname::DEFAULT` is
  `new(DEFAULT_HOSTNAME).expect(…)`, evaluated at compile time, so the
  fifteen-line duplicate of the rule is gone and a bad
  `DEFAULT_HOSTNAME` is still a build error. `http::Counting` was
  looked at and kept: returning the consumed count from `ota::install`
  would mean a wider error type on every `?` in `install` and `fill`,
  or a counting reader inside `ota.rs`, which is `Counting` moved, not
  removed. Backlog item struck as "kept".
- ~~**The setup page can outgrow the buffer on hostile SSIDs**~~ (found by
  the host tests, 2026-10-29). Done the same day, "Setup page within the
  buffer on hostile SSIDs". `write_provision_page` escapes each
  listed SSID twice; twelve SSIDs of 32 `"` or `&` are about 4.4 KB
  of entities and the page is a `500` while they are in range. Cap the
  escaped length or list the label only.

Checked on the host: builds, clippy clean. The old tables were read
out of git by a script (`GAMMA[96]` is 1, `GAMMA[255]` 7; the steps at
10, 25, 50, 75 and 100 % are 1, 2, 5, 9 and 15). A scratch crate with
`config.rs`, `melody.rs`, `panel.rs` and `settings.rs` pulled in by
`#[path]` checked `Hostname::new` on the old cases (empty, leading or
trailing dash, `_` and `.`, 24 and 25 characters, upper case lowered),
`Hostname::DEFAULT` against `DEFAULT_HOSTNAME`, every palette entry
against the old gamma mapping, that `dimmed` keeps every lit channel
lit (7 → 4, 1 → 1) and that the default build offers 10, 25 and 50 %
as steps 1, 2 and 5. (`parse::<u16>` would also take `+123` for PIN
`0123`: another spelling of the same secret, not a second guess.)

**Verify.** Flash over USB. The colours and brightnesses must look as
before through the top and bottom buttons (red, orange, yellow, green,
cyan, blue, magenta, white; 10, 25, 50 %). Before the first sync, or
after `/reload` with the uplink pulled, the sweep must show a dimmer
dash one step behind the bright one, and an armed alarm's degree mark
must stay steady through the sweep. Open `/update`: the PIN still
works once and a wrong one still locks out for ten seconds.

### Host tests: a library crate the host can `cargo test` — DONE, verified on the panel 2026-10-29

**Problem.** Every milestone since M5 was checked on the host in a
scratch crate that lived outside the repo and was recreated each time
(the records above: the mock-flash record round-trips, the form parser
and page sizes, the head parser, the token and sector functions, the
melody lengths, the UI grace, the DHCP and DNS servers, the `Hostname`
rule and the palette). Nothing of it survived a session, so each pass
started from the PLAN.md prose and the checks drifted from the code.
The firmware crate cannot host `cargo test`: the lib depends on
esp-hal and esp-radio, and `.cargo/config.toml` sets the riscv target
for every cargo subcommand under the repo.

**What the ecosystem does (read 2026-10-29).** Cargo has no answer for
the target: `build.target` applies to `test` as much as to `build`,
and the request for a build-only default (rust-lang/cargo#6784) was
closed "not planned" on 2025-11-06. What was added is `--target
host-tuple`, stable since Rust 1.91, so the host triple need not be
spelled out. With that, the practice Ferrous Systems ("Testing an
embedded application"), Veecle ("Your rust embedded unit test runs on
the host") and Embassy's own CI share is: the hardware-independent
code is a library crate with no HAL dependency, marked
`#![cfg_attr(not(test), no_std)]`, with its tests in `#[cfg(test)]`
modules next to the code, and it is tested with `cargo test -p <lib>
--target <host>`; Ferrous puts the host crates in an outer workspace
with no `build.target` and the firmware in an inner one, Veecle and
Embassy pass the flag. Espressif's Rust book says only "test as much
as possible on your host machine" and then covers on-target
`embedded-test`. The first draft of this entry had a `hosttest/` crate
pulling the firmware's files in by `#[path]`; that is the hack, not
the target flag, and it was dropped for the split below.

**Design.** The repo becomes a two-member workspace. The root package
stays the firmware, so `cargo build --release`, `clippy` and `run` at
the root build only it, as before. `core/` is `esp32clock-core`, a
`#![cfg_attr(not(test), no_std)]` library with no esp dependency,
which the firmware depends on by path and re-exports from `lib.rs`
(`pub use esp32clock_core::{aip33628, auth, config, form, melody,
panel, settings, ui}`), so every `crate::settings` in the firmware
still resolves and the moved files keep their `crate::` paths, which
now point inside `core`. The tests run with
`cargo test -p esp32clock-core --target host-tuple`, aliased as
`cargo host-test` in `.cargo/config.toml`.

- **Moved whole:** `aip33628.rs`, `panel.rs`, `melody.rs`,
  `config.rs`, `settings.rs`, `form.rs`, `auth.rs`, `ui.rs`. `config`
  reads `option_env!` when `core` compiles, so `core/build.rs` carries
  the `rerun-if-env-changed` lines and the root `build.rs` keeps the
  `git describe` and the linker script.
- **Split, the pure half into `core` under the same module name:**
  `buttons` (the `Button`, `Kind` and `Event` types; the GPIO task
  stays and re-exports them), `clock` (`LocalTime`, `weekday_of_days`
  and `local_time(unix, tz_minutes)`; the firmware's `local_time`
  passes the settings' timezone), `carry` (`Carried`, `MARK`, `WORDS`,
  `encode`, `decode`; the RTC static, `init`, `get`, `update` and
  `freeze` stay), `http` (`Method`, `Head`, `parse_head` and
  `RESPONSE_BUF`, so the page-size tests assert against the real
  number; the sockets stay), `ota` (the image constants, `Slot` with
  `name` and `from_raw`, `hash_chunk`, `version_from`; the flash,
  the partition table and `slot_from_subtype` stay). Each is a move,
  not a change; the firmware image is the same code.
- **Not moved yet:** `dhcpd.rs` and `dnsd.rs` (their pure halves use
  embassy-net's address types; moving them means embassy-net in
  `core`'s dependencies), `storage.rs` (esp-storage; the mock-flash
  check of M5 is not reproduced, the record codec is tested directly),
  `scan.rs` (the AiP33628 behavioural model of M1 would need
  `build_cycle` on the host). Three candidates for the next pass.
- **Dependencies of `core`:** `embassy-sync`, `embassy-time`,
  `critical-section`, `log`, `heapless`, `sha2` without default
  features; as dev-dependencies, `embassy-time` and `critical-section`
  on their `std` features, so the host has a time driver and a
  critical section. Versions as the root lockfile had them.

**Tests** (`#[cfg(test)] mod tests` at the end of each `core` module),
ported from the records above: `settings` (the `Hostname` rule and
`DEFAULT`, the record round trip and the older versions read with
defaults, a newer version read by its prefix, the rejections, `seed`,
`Debug` hiding the secrets, the brightness choices within the
ceiling, `is_valid` at the edges); `panel` (the palette against the
old gamma mapping, `render` putting a level's bits in the right
planes, every glyph within seven segments); `aip33628` (frame word
layout, `IS_WIRE` its own inverse, the encoder's step count and the
latch tail); `form` (every key, absent checkboxes, the refusals, the
credentials gate, percent decoding, the page, update page and setup
page sizes under `RESPONSE_BUF`, the JSON hiding the secrets);
`http` (the head parser cases of M9); `ota` (`hash_chunk` on a
sector multiple, 1001 over and 3 over, `version_from`); `carry`
(round trip, a wrong mark, the 2106 edge); `clock` (midnight, a
negative timezone, the weekday); `melody` (lengths, names, `pick`);
`ui` (the grace and skip cases of 2026-10-22, the single timer);
`auth` (`bytes_eq`, the PIN's one attempt, TTL, lockout and `+123`).
Sizes recorded in PLAN.md become assertions, so a change shows up as
a failing test, not a prose edit.

**Workflow after it lands.** CLAUDE.md's "Verifying without a board"
says `cargo host-test` instead of "recreate a scratch crate", the
Build section lists the command, and the layout table gains `core/`.
Later milestones add their host checks as tests and PLAN.md records
"host tests pass" with the new tests' names.

**Done, 2026-10-29.** 61 tests pass; `cargo build --release`,
`cargo clippy --release` and `cargo clippy -p esp32clock-core --target
host-tuple --tests` are clean. `llvm-size` against the previous
commit, built from a worktree into a separate target directory: text
879398 against 878570 (+828 bytes, 0.1 %), data 10480 against 10496,
bss 335516 against 335500; the crate boundary costs a little even
under fat LTO (the `local_time` wrapper, the re-exported statics).
Deviations from the plan above: the page tests assert the bound
(`PAGE_MAX`, 3968) and print the sizes (settings 2769, update 2989,
setup with twelve 32-character networks 2945; without headers, so
not comparable to the 2821/3059/3209 above, which counted 88 bytes
of headers) rather than the exact
numbers, so a label edit is not a failing test; and `tz_minutes` is
an `i16` in `clock::local_time`, as the setting is. Two tests found
facts, not bugs: the beeps are 700 ms on 2500 Hz on purpose (the
tune bounds exempt index 0), and 2026-10-29 is a Thursday.

**Finding, for the backlog.** The setup page lists each network twice
through `HtmlStr` (the option's value and its label); an SSID of 32
`"` or `&` is 160 to 192 bytes each way, and twelve of them would
pass `RESPONSE_BUF` and turn the setup page into a 500 while that
network is in range. Adversarial, not seen in the wild; a cap on the
escaped length, or listing the label only, would close it.

**Verify on the panel.** One flash: the clock boots, shows the time,
`http://<ip>/` loads and a setting saved from the page sticks. The
move touched no logic beyond the `local_time` wrapper.


### Setup page within the buffer on hostile SSIDs (after the host tests) — DONE on the host 2026-10-29

**Problem.** Found by the host tests. `write_provision_page` lists
each scanned network as `<option value="…">… (-67 dBm)</option>`,
the SSID escaped twice through `HtmlStr`. A 32-byte SSID of plain
characters costs about 104 bytes a network and twelve fit in 1248;
one of 32 `"` costs 6 bytes a character twice over, about 424 a
network, so five such networks in range push the page past
`RESPONSE_BUF` and the setup page is a `500` for as long as they are
heard. The stored SSID goes through `HtmlStr` once more in the
"Other" field. Nothing a passer-by cannot do with a phone hotspot.

**Design.** A byte budget for the list, spent strongest first, exact
by construction.

- `form::NETWORK_LIST_BUDGET`, the bytes the `<option>` lines may
  take. `write_provision_page` formats each option into a counting
  `Write` first (a `Counter` that adds up `write_str` lengths; no
  buffer, no duplicated arithmetic), and writes it for real only if
  it fits what is left of the budget; the first one that does not
  ends the list, since the list is sorted strongest first and the
  weaker ones are the ones to lose. `listed` (whether the stored
  network appears, which decides the "Other" option's `selected` and
  the typed field's value) is taken over the networks written, so a
  stored network dropped from the list lands in the field. When
  anything was dropped the page says so under the list: "N more not
  shown; type the name."
- The budget is locked by a test, not hand-tuned: the page with no
  networks and every other string at its worst (24-byte hostname, a
  stored SSID of 32 `"` in the field, the notice, the "more not
  shown" line) plus `NETWORK_LIST_BUDGET` must be at most `PAGE_MAX`;
  twelve plain 32-byte SSIDs must all be listed; twelve of 32 `"`
  must give a page within `PAGE_MAX` with some listed and the rest
  counted in the note; a stored network that is dropped must appear
  in the field.
- Nothing changes on the wire for ordinary networks: twelve plain
  32-byte SSIDs are well within the budget, and the `/api` JSON does
  not list networks. `net.rs`'s scan filter and the `ssid_pick`
  handling are untouched; a dropped network is still joinable by
  typing its name.

**Done, 2026-10-29.** `NETWORK_LIST_BUDGET` is 1900. Measured by the
tests: a hostile option (`x` and 31 `"`) is 417 bytes, so four are
listed and the fifth starts the "more not shown" line; the page with
no list and everything else at its worst (24-byte hostname, a stored
SSID of 32 `"` in the field, the notice, the line) is 1915, and
1915 + 1900 = 3815 of `PAGE_MAX` 3968. Twelve plain 32-byte SSIDs
are 2945 as before, all listed, no line; twelve hostile ones plus a
stored one past the budget give 3484, the stored one typed into the
field with Other selected. 63 host tests, clippy clean on both
crates. Nothing to see on the panel; a bench hotspot named 32 `"`
would show four listed at most and the line.

### Setup form and cleared token, from the fourth pass (after the setup page budget) — DONE, verified on the bench 2026-10-30 (the token half from the host, the setup form from a phone with the serial log read)

Four bugs of the fourth-pass backlog, the three on the setup form and
the cleared API token, plus one gap in the same route (the PIN spent
on a bad token body). Each has a pure rule in `core/` under test and a
thin change in the firmware. The fifth is included because it is three
lines in the handler the fourth touches; strike it if not wanted.

**1. An empty password keeps the stored one (the password wipe).**
Rule, in `form.rs` as a pure function the handler calls after
`apply`, `settle_password(trial: &mut Settings, stored: &Credentials,
networks: &[Network]) -> Result<(), FormError>`:

- the posted SSID is the stored one and the password is empty: keep
  the stored password, unless the scan lists that network as open, in
  which case empty is what the network wants;
- the posted SSID is another network, the scan lists it as secured
  and the password is empty: `BadValue`, since the join would fail
  for certain and the page can say so now;
- anything else is as posted (a new network not in the list with an
  empty password is taken on trust, as today: it may be open).

The password label says "empty to keep the stored one" when a network
is stored, "empty for an open network" otherwise, and the `BadValue`
notice gains "or a secured network was given no password". `/api` is
not touched: with the token it sets what it is given, and a script
that wants the same SSID open posts `password=` as before.
Test: the three branches, and that a stored password is never
replaced by an empty one when the SSID is unchanged and unlisted.
Built as planned (`settle_password_keeps_or_refuses`); the handler
runs `settle_password` on the applied copy before the no-network
check. The setup page's fixed part grew from 1915 to 2006 bytes for
the notice, the label and the script, 62 bytes under the budget's
sum.

**2. The list's pick wins over the typed name (the hidden field).**
Both sides. The page's script disables the typed input, not only
hides it, when a listed network is picked (`o.hidden=i.disabled=n.value!=''`,
the input gets `id=i`); a disabled control is not submitted, so a
browser never sends the stale name. The server stops trusting the
order: `form::apply` remembers a non-empty `ssid_pick` and writes it
over `ssid` after the loop, so `ssid_pick=B&ssid=A` and
`ssid=A&ssid_pick=B` both pick `B`. `apply_one` keeps `ssid_pick` as
an alias of `ssid` for a body with only one of them. Test:
`apply_sets_every_key` for the alias, a new test for both orders, and
the page tests re-assert the sizes (`provision_page_fits_the_response_buffer`
prints them; the script grows by about 15 bytes).
Built as planned (`the_pick_wins_over_the_typed_name`); `apply` keeps
the SSID `apply_one` set for a non-empty `ssid_pick` and writes it
back after the loop, so the pick is still validated like any SSID.

**3. Every save on the access point leaves it (the lost fields).**
The reset after a save moves from "the credentials changed" to a rule
per mode in `persist_task`: on the station, a save that changed the
credentials resets into the new network, as now; on the access point,
any save resets into the station when a network is stored, since the
only reason to be on the access point is to be configured. The
handler's own reset (`Then::Join`) is kept only for a form whose
settings are exactly the current ones (`trial == current`), where
there is nothing to save; a form that changed anything, network or
not, is committed with `settings::update` and the persist task saves
it, then resets. The timezone and hostname posted with the stored
network thus reach flash before the reset, and `/api tz=` on the
access point joins the network too (documented). The gap "`/api` on
the access point saves a password with no network" shrinks to a
pointless save with no reset, since the rule checks for a stored
network. The saved page's "restarts now" is two seconds late at most.
`storage.rs` reads `net::mode()`; a failed save still leaves the
clock on the access point (the open "failed save" item, unchanged).
Not testable on the host; the bench check is below. Built as planned.

**4. A cleared token stays cleared (the seed).** The record cannot
tell a token never set from one cleared on purpose, and both must
behave differently at boot: a build with `API_TOKEN` should fill the
first (a bench board re-flashed with a token) and leave the second
alone. So the record says which: version 7 appends one `flags` byte
at offset 167 (`RECORD_LEN` 168), bit 0 `TOKEN_CLEARED`, the other
bits reserved and ignored on read. `Settings` gains `token_cleared:
bool`, `DEFAULT` false; `POST /token` with an empty value sets it and
a non-empty value clears it; `seed` fills the token only when it is
empty and not cleared. The field is not a form or API key (posting it
is an unknown key), not in the JSON (`token_set` already says what
matters), and `write_page` does not show it. The boot line gains a
fourth state, `api token: cleared`. The map item grows to 177 bytes,
well inside the 256-byte scratch. An older image rolled back reads
the version-7 record by its prefix and re-seeds the token, which is
the old behaviour, not a loss. Tests: the record round trip with the
flag, the older-version loop to 7, the newer-prefix test at 8, and
`seed` with a cleared token left empty.
Built as planned; the newer-prefix test also sets every reserved flag
bit and expects them ignored, and the older-version loop now checks
the version-6 network and token come through.

**5. Parse the token body before spending the PIN.** In the `/token`
handler, `form::parse_token(body)` runs first; a bad body answers 400
with the PIN still good, and only a parsed token goes through
`auth::token_or_pin`. Nothing changes between the two, so the order is
free. The `Bearer` path is unaffected. Built: a bad body answers
before the header is looked at, so neither a PIN nor a lockout is
touched.

Order of work: 4 (record and seed, all in `core/` with its tests),
then 1 and 2 (`form.rs` with its tests, then the handler), then 3
(`storage.rs` and the handler), then 5. `cargo host-test`, both
clippy runs, `cargo build --release`; then `AGENTS.md` for the three
rule changes (the empty password, the pick precedence, the save on
the access point, the token flag and the boot line).

Bench check, in order, on the bench board with `scripts/pin.sh` not
needed:

1. `POST /token` with `token=` and the `Bearer` token, wait for
   `settings: saved`, `scripts/reload.sh` without a token (it needs
   one; use `curl -X POST -H "Authorization: Bearer $OLD" /reload`
   expecting `403`), then power-cycle or hold: the boot line must say
   `api token: cleared` and `/api` must show `token_set: false`. Then
   `POST /token` with a new token and the PIN from `/update`: the
   boot line after a reload says `stored`.
2. Hold both buttons 10 s; on `http://192.168.4.1/` leave the network
   as listed, type nothing in the password, change the hostname, save:
   the clock must come back on the LAN under the new hostname with the
   old password. The log line for the save is followed by the switch.
3. On the access point again with a network not in range (type one
   under Other, save, hold again): pick the real network from the
   list, type its password, save: the clock joins it, and the log's
   `network … saved` line names the picked one.
4. On the access point, pick a listed secured network and leave the
   password empty: the page must come back with the notice and
   nothing saved.
5. `curl -X POST -d token=short -H "Authorization: PIN 1234" /token`
   with a fresh PIN: `400`, and the same PIN then sets a good token.

Bench run 2026-10-30, image 044a226-dirty over the air onto `ota_1`
with the monitor on `/dev/ttyACM0`: the version-6 record was read
intact (`settings: loaded from flash`, every field as before,
`token_cleared: false`). Check 1: `POST /token` with `token=` and the
`Bearer` token answered 200, `settings: saved` followed two seconds
later, `/api` said `token_set: false` and `/reload` with the old token
was `403`; after a USB reset (the monitor reconnecting, since no token
was left for a software reset) `/api` still said `token_set: false`
on a build with `API_TOKEN`, which the old seed would have refilled.
The boot line itself was not captured: the monitor loses the first two
seconds after the reset it makes. Check 5: a PIN from `POST /pin`,
`token=short` with it answered `400` and the same PIN then set the
token (`200`, `settings: saved`); `scripts/reload.sh` with it came
back in 3 s with `boot: network stored, api token stored` and the
settings line from the version-7 record. A brightness change and back
over `/api` on the station saved without a reset (uptime continuous,
`heals` 0). Checks 2 to 4 were done from a phone
the same day with the monitor on, every expected line in the log, in
this order: the hold (`heal: resetting into access point mode`),
`clock-f044` up and the phone's lease; check 4, a listed secured
network with no password, `setup form rejected: BadValue` and the
clock still an access point; check 2, `esptest` left as listed, no
password, the hostname changed to `test-clock2`: `network esptest
saved, leaving the access point once it is written`, `settings: saved`
two seconds later, then the station boot with the new hostname in the
settings line and `connected to` the network on the kept password;
check 3, `nowhere` typed under Other and saved, four `no such network
in range` attempts, the hold again, then the pick: `network esptest
saved`, not `nowhere`, and the join. The page's own behaviour (the
notice, "Other" selected with `nowhere` typed in) was seen on the
phone. A settings-page save of the hostname back to `test-clock` from
the LAN closed the run; `/api` then read hostname `test-clock`, ssid
`esptest`, password set, token set, mode `sta`, heals 0.

### M11 — NTC thermometer and light sensor — the readings built 2026-11-01 (M11a, below), the face and the dimming still planned

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

#### M11a — read both sensors and show them on the page and in `/api` — built 2026-11-01; the NTC verified on the bench the same day, the LDR read the rail (see the end)

The parts arrived and the adapter is on the bench (2026-11-01). The
user asked for the readings on the page and in the API first; the
temperature face, the dim-in-the-dark setting and the record change
above stay planned, so this step adds no setting and leaves
`RECORD_VERSION` alone.

**Correction to the design above.** An unfitted LDR does not read
ground: the pull-up is on the adapter, so a missing LDR leaves the
pin at 3V3, exactly what a pitch-dark LDR does. Only the NTC can
say "no sensor" (the pin at the pull-up, above any room
temperature); a missing LDR is reported as 0 % light. The ADC's
calibrated range at 11 dB ends near 2.5 V and the raw code pins at
about 3.1 V, so the NTC's cold end is imprecise below 0 °C and
unreadable below −20 °C; a room is nowhere near either.

**Pure half, `core/src/sensors.rs`.** A 21-entry table of the
divider's millivolts at −20, −15, … 80 °C for a 10 kΩ, B = 3950
NTC under a 10 kΩ pull-up from 3300 mV, computed once on the host
(`ntc.py`, the B equation) and pasted; `temperature_dc(mv)` finds
the segment and interpolates to tenths of a degree, which is within
0.17 °C of the equation over the whole span, rounding included (checked by a test that
evaluates the equation in `f64`). Above the −20 °C entry it answers
`None` (no sensor, or colder than the ADC can tell); below the 80 °C
entry, `Some(800)`. `fahrenheit_tenths` for the second unit, and
`light_pct(mv)`: first the LDR's resistance from the divider
(`ldr_ohms`), then the logarithm of it between `LIGHT_FULL_OHMS`
(1 kΩ, 100 %) and `LIGHT_DARK_OHMS` (50 kΩ, 0 %) through a 21-entry
table at 5 % steps, because that is how both the eye and a GL55-type
LDR work: about a kilohm in daylight, tens of kilohms at dusk, more
than the ADC can tell from the pull-up alone (about 70 kΩ, where the
raw code pins near 2.9 V) in the dark. The first cut was linear in
the voltage and squeezed the bench's lighting into 5–21 %, which
read as broken. A `Tenths` formatter writes `23.4` and `-0.5` for
the JSON and the page.

**Hardware half, `src/sensors.rs`.** One `sensor_task` in thread
mode owns `Adc<ADC1>` and both `AdcPin`s (GPIO0 the NTC, GPIO1 the
LDR, 11 dB, `AdcCalCurve`, which on the C3 corrects with the eFuse
two-point data and returns millivolts). Every second it takes 8
blocking `read_oneshot` samples of each, 50 µs apart (a conversion
is microseconds; the scan is on the interrupt executor and does
not see it), averages them, and folds the result into an
exponential average with a weight of 1/8 (about 10 s), which it
publishes to two `AtomicU32` cells as the single writer; the first
reading seeds the average so there is no ramp from zero. `readings()`
hands `http::status` and the heartbeat the filtered millivolts,
`None` until the first second has passed.

**Exposed.** `form::Status` grows `temp_dc: Option<i16>` and
`light: Option<u8>`, filled by `http::status`; `write_json` adds
`temp_c`, `temp_f` (tenths, or `null`) and `light` (0–100, or
`null`) before `last_heal`, and `write_status` a line
`Room 23.4 °C (74.1 °F), light 62 %.` or `Room: no thermometer,
light 62 %.`, with `not read yet` for the first second. The heartbeat
logs the raw filtered millivolts of both for calibration. The page
sizes are re-measured below.

**Host checks.** The table against the equation; the rails and the
range ends; the light index at its ends; the `Tenths` formatter on
negatives; the JSON keys with and without a reading; the page size.

**Verify on the bench.** The heartbeat's millivolts: about 1650 for
the NTC at 25 °C in free air and the LDR well under 1000 in a lit
room; `/api` shows `temp_c` within a degree of a thermometer and
`light` dropping toward 0 with a hand over the LDR within about
10 s; the page's new line says the same.

**Bench, 2026-11-01.** Flashed over USB to the sensor-fitted board
(it joined the home network as `clock`, 10.1.1.156). The first
heartbeat said `sensors ntc 1657 mV, ldr 2894 mV, room 24.8 C
(76.6 F), light 0 %`; `/api` carried `"temp_c":24.8,"temp_f":76.6,
"light":0` and the page the line `Room 24.8 °C (76.6 °F), light
0 %.` The NTC's 1657 mV is the 25 °C point of the table, so the
divider, the calibration scheme and the table agree with the room.
The LDR at 2894 mV is the pull-up alone as the curve reads it near
its top (about 3.1 V raw pinned): either the bench is dark for the
LDR, or it is not fitted or not making contact at R3 / J1 pin 4.
The settings page is 2830 bytes (was 2769); the setup page
3039 (the guide said 3036, which was stale).

Second run, after a reflash (the first image carried a stale version
string, `a01e48c`, from cargo reusing an old build-script output;
`touch build.rs` and a rebuild gave `4bd6882-dirty`). The LDR does
work: over three minutes it read 2894, 2260, 1959 and 2372 mV, that
is from the rail to 14.6 kΩ and back to 25.6 kΩ as the bench's light
changed, so the first run's rail was the bench being dark for it,
not a missing part. Those are 0, 9, 21 and 5 % on the linear scale,
0, 31, 17 % on the logarithmic one, which is why the index was
changed. The NTC crept from 1657 to 1646 to 1629 mV (24.8, 25.1,
25.6 °C) over the run and 25.9 °C at the end, while the user put
the room at about 21 °C: the drift says the sensor is warmed by the
board (the case open, the panel at 10 %), and a constant part of the
gap could be the NTC's R25 or the 3V3 rail. Owed: a thermometer
beside the clock now and after an hour, and a meter on R4 or on the
3V3 pin, to split the two; then the planned `temp_offset`
(constant) or a corrected R25 (part). Not changed yet.

Settled the same afternoon. The clock had been lying on its face
with the NTC's hole against the bench, so the case held its heat;
stood upright it fell from 24.5 to 23.0 °C in twelve minutes and
then tracked a digital thermometer beside it: 23.0 against 22.3 °C
at 12:25, 23.4 against 22.6 °C at 13:08, a constant 0.7 to 0.8 °C
with the room warming under both. The adapter's pull-ups measured
9.96 and 9.99 kΩ (another adapter, same reel), which moves the
reading 0.1 °C the other way, so the gap is the case: the hole is at
the top, where the air inside is warmest. The conversion is sound.
The fix, when wanted, is the planned `temp_offset` setting at about
−0.8 °C; a second day's pair of readings before trusting the tenths.
Minute-to-minute noise is about ±0.25 °C on the 10 s average, partly
convection through the hole; a longer filter if a face ever shows
tenths. A second re-worked clock (`martin-clock`, 10.1.1.58) flashed
the same day read 24.7 °C at a minute's uptime and the same access
point at −53 dBm, so the weaker reception is the adapter build, not
one board.

Wi-Fi at the same spot as the old bench board, same access point
(80:69:1a:1f:25:cc, channel 9): the new clock's minute line read
−64, −52 and −65 dBm and its scan −51; the old board's logs from
that spot read −38 to −42 dBm on the minute line and −38/−39 at the
scan, and the far access point on channel 3 went from −76 to −83.
That is 10 to 25 dB less at the clock, more than the spread between
two boards; the probe (50 ms for 1400 bytes) and the page (30 to
80 ms) are still fine, so the link works, but the margin before the
self-heal is smaller. The adapter puts the C3 Zero in a different
place and orientation in the case, next to the ground plane of the
clock board; the guide's rule applies, measure at the access point's
figure for the clock, from inside the case, before changing the
transmit power.

Antenna rework, the evening of 2026-11-01: the user fitted a Hirose
U.FL receptacle at the C3 Zero's chip-antenna feed on blue-clock and
a patch antenna on it. Same spot, same access point, lid open as
before: the scan reads the near access point at −29 dBm (was −51 to
−53 with the chip antenna, −38/−39 on the old bench board) and the
far one on channel 3 at −58 to −61 (was −82/−83, −76 on the old
board); the minute line on channel 9 reads −29 (was −52 to −65).
That is 25 to 30 dB back at the clock, 10 dB past the old board.
Ten 498-byte pings from the host answered in 3 to 11 ms, no loss;
the page in 38 to 39 ms; the boot probe in 13 ms. One oddity in two
boots: the first connect after the rework went to the far access
point at −58 although the candidate scan listed the near one at −29
and the config sorts by signal; the reload after it joined the near
one. Watch for it; a repeat would be worth a `with_bssid` pin or a
look at the driver's own scan. The reworked board then went into
the case and the real clock's spot as `martin-clock` (10.1.1.156, on
`5d0e373`): both access points read it at about −70 dBm, equal, so
that spot is as far from one as from the other. The old board, a
C3 Zero as well but placed differently in the case, read −74/−76 at
the near access point at 16 dBm from that spot and was out of the
far one's reach; the adapter build with the chip antenna would have
been 10 to 25 dB below that at the access point and was never
installed. From the host, 20 pings of 498 bytes
answered in 4 to 57 ms with none lost and the page in 35 to 46 ms,
0 heals at 24 min. With both access points equal the driver's pick
is a coin toss, so the first-boot oddity above may show up as a
spread in the minute line's BSSID rather than a fault. The light
reads 0 there, which is right: the room was dark. The −0.7 offset is
the open-lid figure; the closed-case offset (about −2.0 on the chip
antenna board the same day) is still to be measured there. Still
owed: the same rework on the second adapter board, and the user
plans a second adapter for the ESP32-C3 SuperMini Plus footprint
(two rows of eight), so a module with its own IPEX antenna jack can
go in without the receptacle rework.

The SuperMini adapter on the bench, 2026-11-12: the user built a
clock on the `adapter-esp32c3-supermini` board with an ESP32-C3
SuperMini Plus and an external antenna on its IPEX jack (hostname
`clock`, 10.1.1.11, id `clock-1d32d8`, `v0.4-1-g253a63e`) and put it
in the case with the lid open, on USB, then swapped the
old white-clock board (`white-clock`, 10.1.1.58, id `clock-6a1bec`,
`v0.4`) into the same case and spot. Each got the same run from the
session: three minutes connected to the near access point
(`80:69:1a:1f:25:cc`, channel 9) with `/api` read every 10 s, a
498-byte ping every second and the settings page fetched every 10 s,
then four boot scans (the monitor's USB reset each time).

| measure | SuperMini adapter | old white-clock board |
| --- | --- | --- |
| RSSI connected, near AP | −21 to −23, steady | two levels, −38/−39 and −52 to −54 |
| boot scans, near AP | −31, −21, −21, −23 | −53, −56, −38, −39 |
| boot scans, far AP (channel 3) | −74, −64, −62, −63 | −83, −82, −84, −86 |
| 498-byte pings, 180 | 180 answered, 3.0/4.6/76 ms | 179 answered, 3.1/5.9/25 ms |
| settings page | 40 to 52 ms | 42 to 62 ms, one of 117 |
| boot probe | 10 ms | 57 ms |

So the new adapter hears the near access point 16 to 30 dB better
than the old board and the far one about 20 dB better, and 6 to 8 dB
better at the near one than the U.FL rework did on 2026-11-01 (−29),
with the far one about level with it (−58 to −61 then); the spot on
the bench may not be the same as that day's. Two things not
explained: the new board's first boot scanned both access points
10 dB lower than its next three, while its connected figure on that
boot was already −22; and the old board flipped between two levels
14 dB apart from one 10 s sample to the next, in the scans as well,
which the new board never did. All of this is the clock's side.
Still owed: the access points' figure for each clock from the real
spot with the lid closed, which the user reads from the router, and
the same run on the v3 Zero adapter.

The v3 Zero adapter, the same afternoon: a clock on the
`adapter-esp32c3-zero` board (the chip antenna over the notch at the
board's edge; hostname `clock`, 10.1.1.65, id `clock-6a1bf4`,
`v0.4-3-gb744b03`) on the same port, the same run. Connected to the
near access point it read −54 to −57, steady, over the three minutes;
the four boot scans read the near one at −56 every time and the far
one at −87, −86, −86, −86. 180 of 180 pings of 498 bytes answered,
3.2/8.1/116 ms; the settings page in 46 to 73 ms; the boot probe in
70 ms. So the notch did not bring the chip antenna up to the old
board: the v3 Zero adapter sits at the old board's lower level (−52
to −54) without its upper one (−38/−39), inside the range the first
adapter's chip antenna had on 2026-11-01 (−52 to −65), and 33 dB
below the SuperMini Plus with its external antenna. It passes
traffic at the bench all the same. Whether the board was in the same
case and spot as the morning's two was not said.

Standing upright, later that afternoon: the user stood the case
bottom down, as the clock stands in use, and the session repeated
the run on the v3 Zero adapter and then, with the adapter swapped,
on the old white-clock board in the same case.

| measure | v3 Zero adapter, upright | old white-clock board, upright |
| --- | --- | --- |
| RSSI connected, near AP | −53 to −60, drifting down | −53 to −57, steady |
| boot scans, near AP | −59 all four | −54, −53, −54, −54 |
| boot scans, far AP | −90, −89, −92, −90 | −87 once, not listed in three |
| 498-byte pings, 180 | 180 answered, 3.1/10.0/100 ms | 180 answered, 3.0/6.1/36 ms |
| settings page | 45 to 69 ms, two of 0.46 and 0.48 s | 41 to 73 ms |
| boot probe | 48, 74, 54, 49 ms | 50, 44, 49, 67 ms |

Upright the two chip-antenna boards are within 3 to 5 dB of each
other, the old one ahead, and the old board's upper level of the
morning (−38/−39) is gone: that was the case lying down, not the
board. The v3 Zero adapter lost about 3 dB against its own run lying
down. Neither hears the far access point usefully. The SuperMini
Plus was not measured upright.

The user's own readings of the two, taken by hand the same day: the
v3 Zero adapter always lists the far access point and reads the near
one between −52 and −58; the old board often does not list the far
one at all and reads the near one closer to −58. The two sets agree
on the far access point: the session's scans listed it in all eight
boots of the v3 Zero adapter, lying down and upright, and in one of
the old board's four upright. On the near one they differ by a few
dB in opposite directions (the session had the old board 3 to 5 dB
ahead upright, the user has it behind), which is the spread of the
placement, so the near access point does not separate the two
boards. What the notch bought is the far access point being heard at
all, at −86 to −92, too weak to rely on. The user's verdict: the
v3 Zero adapter is only marginally better than the old board, and
the external antenna, the SuperMini Plus adapter, is the one to
build.

#### Thermometer offset (after M11a) — DONE, verified on the bench 2026-11-01

The afternoon's comparison above gave a constant 0.6 to 0.8 °C
between the NTC and a thermometer beside the clock, the warm air
at the top of the case. A setting `temp_offset`, tenths of a degree
Celsius added to the reading, from −10.0 to 10.0, default 0,
applied in the firmware's `sensors::room` before both units, so the
page, `/api`, the heartbeat and later the panel face all say the
corrected figure. Key `temp_offset` on the form and `/api` as a
decimal with at most one place (`-0.7`, `1`, `-10.0`); the page has a
number field with step 0.1 under the timezone. `RECORD_VERSION` 8
appends the two bytes after the flags (`RECORD_LEN` 170); a version
7 record reads as offset 0 and a rollback to the previous image
reads version 8 by its prefix. `form::parse_tenths` is the parser,
in `form.rs` with the other parsers and under test: a sign, digits
and one optional decimal, nothing else. `Settings::is_valid`
bounds it. Page size re-measured below.

**Verify on the bench.** `POST /api` with `temp_offset=-0.7` on
blue-clock: the JSON echoes `-0.7` and `temp_c` drops by 0.7
against the heartbeat's raw millivolts; the page shows the field
with the value; a reload keeps it; a `temp_offset=10.1` is a 400.

**Bench, 2026-11-01.** blue-clock flashed over USB with its version 7
record in place: `/api` came up with `temp_offset` 0.0 and 23.5 °C
(the old record read with the default). `POST /api temp_offset=-0.7`
echoed −0.7 and 22.8 °C in the same answer; `10.1` and `abc` were
400s; the page showed the field at −0.7 with `min=-10.0 max=10.0
step=0.1` and the room line at 22.8 °C; `scripts/reload.sh` brought
it back in 3 s with −0.7 still set and 22.7 °C, so the version 8
record was written and read. The settings page is 2943 bytes.
martin-clock was unplugged before its flash and still runs the
image without the setting.

#### M11b, first half — the temperature on the panel each minute — DONE, verified on the panel 2026-11-01

The user asked for the temperature for 5 s in the middle of each
minute, with a setting to turn it off. One setting does that and
picks the unit: `temp_show`, `off`, `c` or `f` (page select
"Temperature on the panel": Off, Celsius, Fahrenheit; default
Celsius), `RECORD_VERSION` 9 with one byte appended (`RECORD_LEN`
171; a version 8 record reads as the default, a byte past 2 is a
bad record).

The face is the UI's, not a mode: in `Mode::Clock`, with the
setting on and a reading in hand, `Ui::view` answers `View::Temp`
for seconds 30 to 34 (`TEMP_SHOW_AT`, `TEMP_SHOW_SECS`) and the time
otherwise, so a press keeps its clock-mode meaning and ringing,
alarm-set, the IP and the PIN take precedence as before. `view`
gains the reading as an argument (`Option<i16>` tenths, with the
offset already in), which the display task takes from
`sensors::room`; no reading, no face. `sensors::face(tenths, unit)`
in the core crate lays it out: the whole degrees, rounded, right
aligned in the first three positions, the unit letter in the
fourth, so `23°C`, `-5°C` and `100°F` all read, clamped to −99 and
999; the display lights the degree mark for it whatever the alarm.
The display's wake for the face is the next second, like the time.

**Host checks.** The record round trip and the older versions; the
form key and the JSON; the page size; `face` at 23.4, −4.5, 99.5,
−100 and 1000; `view` on, off, without a reading, at seconds 29,
30, 34 and 35, and in Fahrenheit.

**Verify on the panel.** At :30 the panel shows ` 23C` with the
degree mark for five seconds and the time is back at :35 with the
colon blinking; Fahrenheit shows ` 73F`; Off shows the time all
minute; the alarm-set and the IP display win over it.

**Bench, 2026-11-01.** blue-clock flashed over USB on its version 8
record: `/api` came up with `temp_show` `c` and the offset still
−0.7; `f` was taken and echoed, `k` was a 400, the page has the
select, and `c` put it back. The settings page is 3121 bytes. The
user watched the face on the panel: works as expected.

#### Short frames on the uplink (after the temperature face) — DONE, verified on the bench 2026-11-01

The adapter-built clocks hear the access point 10 to 25 dB weaker
than the old bench board (M11a), the access point hears them 20 dB
below that again, and a marginal uplink passes short frames long
after full ones fail. The clock sends only short frames (the antenna
rework landed the same evening and put 25 to 30 dB back, see M11a;
the MTU stays, since nothing but the probe depends on it and a
clock built without the rework needs it): `ESP_RADIO_CONFIG_WIFI_MTU` 540 in
`.cargo/config.toml`'s `[env]` (esp-radio's compile-time key; the
default is 1492), which smoltcp turns into a 486-byte TCP payload
and the largest ICMP payload of 498. The probe follows it:
`config::WIFI_MTU` reads the same variable back and
`PROBE_PAYLOAD_BYTES` is `WIFI_MTU - 42`, with a compile-time
assert on the range, so the probe stays the largest frame the clock
sends; `core/build.rs` reruns on the variable. `scripts/watch-clock.sh`
pings with 498.

It is a transmit-side limit only. smoltcp applies the MTU in
`dispatch_ip` and the TCP segment size (smoltcp 0.14, checked), never
on ingress, and esp-radio hands received frames to the stack whole
from the driver's own buffers, so a DHCP offer, a DNS answer or a
ping from the LAN of any size still arrives. What cannot be kept is
the segment size the clock advertises in the TCP handshake, which a
conforming peer obeys: an OTA upload arrives in 486-byte pieces,
three times the segments through the 4 KB window. The one visible
change on the LAN: a ping larger than 498 bytes gets no reply, since
embassy-net is built without IPv4 fragmentation and the reply would
not fit. The access point's DHCP server pads its replies to 300
bytes with a few options on top, well under the limit.

**Verify on the bench.** The probe line says a 498-byte ping; the
page and `/api` still come back in under 100 ms; `ping -s 498` from
the host is answered and `-s 1400` is not; an OTA upload still
completes; the heartbeat's failed-probe count stays at 0 over the
evening.

**Bench, 2026-11-01.** blue-clock over USB, esp-radio rebuilt for the
key: the probe line says `answers a 498-byte ping in 79 ms` (46 ms
after the next boot); from the host `ping -s 498` is answered (two
of three in the first burst after boot, then clean), `-s 1400` gets
nothing, the plain ping all three; the page's 2876 bytes come back
in 32 to 43 ms; `scripts/ota.sh` put the 714 KB image through the
486-byte segments in 7.4 s, no slower than before (the flash write
is the bound), and the clock rebooted into `ota_1` with the record
and the carry intact. The evening's failed-probe count is still
owed.

#### Settings page in three groups (after the short frames) — DONE, verified in a browser 2026-11-01

The settings form was one run of twelve labels. It is now three
`<fieldset>`s with a legend each, in the order the user gave (and
reordered once at his request after the first look): Alarm (armed,
time, days, skip the next, sound, Play), Display (colour, brightness,
temperature on the panel, 12 hour clock) and Other (timezone,
thermometer offset, hostname). Save stays below the last group. Play
sits under the melody select and no longer reloads
the page: a four-line script posts `melody=<picked>` to `/play` by
`fetch` (no redirect followed), so the browser keeps every unsaved
field, the picked tune first of all; without script the button still
has `formaction=/play` and works the old way, with the reload. The
handler is unchanged: it applies the body to a scratch copy, and a
body with the melody alone is a valid form. The fieldset rule (a thin border, rounded,
the labels' own margins inside) is a second `<style>` written by
`write_page` alone and not in the shared `HEAD`, because the setup
page's worst case had 62 bytes to spare under `PAGE_MAX` and the rule
is 77: in `HEAD` it put `provision_page_budget_is_covered` over by 15
bytes. The settings page is 3629 bytes (was 3121), the other two
unchanged. The field names and `/api` are untouched, so a script sees
no difference.

**Verify on the bench.** The page shows the three groups with the
fields in that order; a save from it still takes every field; the
Play button plays the picked tune and leaves the form as it was. The
reorder was flashed and fetched the same way: 3387 bytes, the
legends and fields in the new order, `POST /play` still a `303`.

**Bench, 2026-11-01.** test-clock (10.1.1.30, the old bench board)
flashed over USB: the page comes back at 3382 bytes with the three
legends and the fields in the given order, Play after the melody
select; a form post of the current values is a `303` to `/` and the
record reads back unchanged; `POST /play` with `melody=beeps` alone
is a `303`. The buttons in a browser are for the user to try.

#### Yellow was pale green (after the settings groups) — DONE, verified on the panel 2026-11-01

The user: yellow on the panel "doesn't look yellow, more like very
light green". The palette had yellow as red and green at equal full
duty, `(0, 7, 7)`, which is what the old gamma made of RGB (255, 255,
0); on this panel's LEDs the green die puts out far more light per
unit of current than the red, so equal duty is a greenish yellow.
`settings::PALETTE` now has yellow at `(0, 4, 7)`: green at a little
over half duty, red full. Nothing else moves; orange stays `(0, 1,
7)`, so the three warm colours are red, a touch of green, and about
half green. The palette test
(`palette_is_the_old_gamma_of_the_old_rgb_but_for_yellow`) locks the
other seven to the old gamma and yellow to this, with green between
orange's and red's. White `(7, 7, 7)` is presumably greenish for the
same reason and is left alone until someone says so.

For the comparison the old mix stays in the palette as a ninth
colour, `lime` `(0, 7, 7)`, placed right after yellow so one press
of the top button flips between the two; the page's select and `/api`
take `color=lime`. It went in as a bench colour to be removed once
yellow was settled; the user kept it as a ninth colour ("I like it as
an extra color", 2026-11-01), so it stays. The insertion shifts the stored index of green and the
colours after it by one: a clock saved on green comes up lime after
the update, and an image from before this one reading a record that
says white (index 8) lands on its last entry, white, by the clamp in
`from_record`. The settings page grows by one `<option>`, 3663 bytes.

**Panel, 2026-11-01, first look.** Better: yellow is yellow at 50 %,
the top step offered, but still has a green tint at 10 %. Lime is
lime and stays. The green die keeps its efficiency at a low drive
current while the red one loses it, so no one mix holds across the
brightness steps. The first try was a ramp: `Settings::color`
trimmed yellow's green by the brightness step, `[2, 3, 4, 4, 4]` for
10, 25, 50, 75 and 100 %, with the palette entry the 50 % mix.

**Panel, 2026-11-01, second look.** Good at 10 %, green-ish at 25 and
50 %; the user asked for level 2 flat. Done: the palette's yellow is
`(0, 2, 7)` at every brightness, the `YELLOW_GREEN` ramp and the
brightness-dependent `Settings::color` are gone again, and the
palette test locks in the 2. One press of the top button from yellow
is lime, kept as a colour of its own.

#### Wi-Fi status line on the page (after yellow) — DONE, verified on the bench 2026-11-01

The user asked for the network and the signal on the settings page.
The signal is only in the serial log today: `net::log_status` reads
`controller.rssi()` once a minute inside `wifi_task`, and the
controller belongs to that task, so the HTTP handler cannot ask the
driver itself. The same pattern as the sensors: the Wi-Fi task
publishes the reading in an atomic (`net::RSSI`, one writer, with a
sentinel for "not connected") and `net::rssi()` reads it. It is
written on connect, cleared on disconnect, and refreshed every
`RSSI_PERIOD` (10 s) by the inner loop, which keeps logging its
minute line every sixth sample so the log is as it was. The RSSI is
the last beacon's, so a station that stopped hearing its AP shows the
stale figure until the disconnect lands, same as the log line.

`form::Status` gains `rssi: Option<i8>`; the SSID is the stored one
(`Settings::wifi.ssid`), which is the only network the station ever
joins, so `write_status` takes the settings' SSID rather than a copy
in `Status`. The page shows `Wi-Fi <b>home</b>, −62 dBm.` under the
firmware line, `Wi-Fi <b>home</b>: not connected.` without a reading,
and `Wi-Fi: no network stored.` with an empty SSID. `/api` gains the
read-only key `rssi` (dBm, or `null` when not connected and on the
access point, where the setup form has no status line). The page
test's worst case now stores a hostile SSID of 32 `"` (192 bytes
escaped) and a four-character `-100`: 3899 bytes of the 3968, 69 to
spare, which is the settings page's new ceiling to watch. The yellow
entry's 3663 was without the hostile SSID; the escaped line alone is
the growth. The minus on the page is ASCII, same as the log.

**Verify on the bench.** `curl http://<ip>/` shows the line with the
stored network and a figure near the minute log's `rssi`; `/api` has
`"rssi":-NN`; after `scripts/reload.sh` the first page is "not
connected" only if fetched before the connect, then the figure.

**Bench, 2026-11-01.** The user flashed and checked it; from the
session, test-clock (10.1.1.30, on `esptest`) answers `/api` with
`"rssi":-41` after `"light"` and the page with `Wi-Fi <b>esptest</b>,
-41 dBm.` under the firmware line, 172 s after the flash.

#### Unsaved-changes mark on the settings page (after the Wi-Fi line) — DONE, verified on the bench 2026-11-02 with headless Chromium against the clock

The user asked for an indication on the settings page when the form
has been edited but not saved. The page is a plain form posted whole
by the Save button, so "modified but not saved" is a browser-side
state: nothing on the clock knows about an edit until the post. A
script on the page does it, the same way the Play button already
posts by script.

Design: `<b id=u hidden>Changes not saved yet.</b>` after the Save
button, and the page's one script (the Play handler merged into it)
sets `f.oninput` on the form to compare every named control with the
defaults the page was served with: a checkbox against
`defaultChecked`, a select against its option's `defaultSelected`, a
field against `defaultValue`. The mark shows while any differs and
hides again when the user puts the field back, so it tracks
"modified", not "touched". `input` bubbles from every control,
selects and checkboxes included, and the Play button changes no
control, so it does not trip the mark. A save reloads the page from
the record, with fresh defaults, so the mark is gone after a save or
a refused one (the form then shows what is stored, under the error
notice). Every `<select>` writes `selected` on its stored option,
which the comparison relies on; the `DayOptions`, `TempShowOptions`
and the three inline loops all do. A change made on the buttons while
the page is open is not seen by it (the page is stale either way) and
not in scope. No warning on leaving the page: that would have to be
undone for the Save button's own navigation, and the user asked for a
mark, not a gate.

Size: the settings page was 69 bytes under `PAGE_MAX` at its worst
and the mark with its script is 308, so `RESPONSE_BUF` had to grow.
The user chose 6 KB (`6 * 1024`, `PAGE_MAX` 6016) over the minimum,
for the next feature's sake. The buffer is in each of the three
listener tasks' futures, so static RAM grows 6 KB and the main stack
shrinks by the same: `.stack` went from 90664 to 84520 bytes (`llvm-readelf -S`), about 82.5 KB. The settings
page is 4207 at its worst, 1809 to spare; the setup page's list
budget `NETWORK_LIST_BUDGET` stays at 1900 (the list is capped for
the page's sake, not the buffer's, and twelve networks is plenty), so
its spare grows to 2110. The `form` size tests measure the new worst
case, and a new test checks the mark and the comparison are on the
page. `AGENTS.md` carries the new sizes.

**Verify in a browser.** Open `http://<ip>/`: no mark. Change the
colour: "Changes not saved yet." appears after the Save button. Put
the colour back: it goes. Tick "Skip the next alarm", press Play: the
mark stays and the tune plays. Save: the page comes back without the
mark and with the box ticked.

**Bench, 2026-11-02.** test-clock (10.1.1.30) took the build by
`scripts/ota.sh` (715168 bytes, 13.9 s, back on `ota_0` within a
second of the reboot). The page is 3776 bytes as served, with the
mark and the one script. The browser side was driven from the
session: headless Chromium (`chromium --headless=new
--remote-debugging-port`) with a Node script over the DevTools
protocol, the checkbox and the buttons by mouse events at the
elements' positions, the colour select by setting its value and
dispatching the `input` event a pick fires, the mark read back from
`u.hidden` and every request the page made logged. Served: hidden.
Colour changed: shown, "Changes not saved yet."; put back: hidden.
Skip ticked by a click: shown. Play clicked: `POST /play
melody=beeps` and the mark stays with the box ticked; Chrome aborts
the 303's `GET /` itself (`net::ERR_ABORTED`, `redirect: 'manual'`
doing its job), so the clock serves no second page for a preview.
Save clicked: the post carries `skip=on`, the page comes back with
the mark hidden and the box ticked, `/api` says `"skip":true`. The
box unticked: shown; saved: hidden, `"skip":false`. The settings
were left as found.

Two things from the run. The default headless window is 493 px high
and the Save button sits at 899, so the first run's clicks on it
landed nowhere; the script now refuses a target outside the viewport
and the window is set to 1400 (`Emulation.setDeviceMetricsOverride`).
And between the OTA and the end of that first run the stored melody
went from beeps to ode: the browser's requests, logged on a replay of
the same clicks, hold nothing that sets a melody, the replay does not
reproduce it, and the firmware writes the melody only from `POST /`
and `POST /api`; most likely a save from another browser at the
bench. It was put back with `POST /api melody=beeps`.

#### Favicon and the hostname in the title (after the unsaved-changes mark) — DONE, verified in a browser 2026-11-02

Every page load cost a `GET /favicon.ico` answered 404 and the tab
showed the browser's blank glyph. `src/favicon.ico` (337 bytes) is
the panel as pixel art, a dark rounded face with the time in orange
seven-segment digits: a 32 px `12:34` and a 16 px `1:23`, two PNG
entries in one ICO so the tab is crisp at either density. `src/http.rs`
embeds it with `include_bytes!` and serves `GET /favicon.ico` with
`image/x-icon` and `Cache-Control: max-age=86400`, on the access
point too, ahead of the captive-portal redirect. The pages carry no
`<link>` tag, since `/favicon.ico` is the path browsers ask for by
themselves, so they do not grow. Drawn once with a throwaway script;
the file is the source.

**Verify in a browser.** Open `http://<ip>/`: the tab shows the dark
face with orange digits, and `http://<ip>/favicon.ico` the icon
itself. One `GET /favicon.ico` in the log for the first page, none
for a reload.

**Bench, 2026-11-02.** test-clock (10.1.1.30) flashed over USB. `curl
http://10.1.1.30/favicon.ico` answers `200`, `image/x-icon`,
`Cache-Control: max-age=86400`, and the body is byte-identical to
`src/favicon.ico`; the page still serves (3770 bytes). The tab itself
not yet looked at in a browser.

**Title, 2026-11-02.** The user asked for the tab to say which clock
it is: the settings and update pages are titled `Clock: <hostname>`
(`form::write_page` and `write_update_page`, the latter now given the
hostname), the setup pages keep `Clock setup` since there the
hostname is usually still the default. The hostname is letters,
digits and dashes, so nothing to escape. Worst-case sizes: settings
4233, update 3008 of `PAGE_MAX` 6016. Then the user found the heading
still said `Clock`: the `<h1>` of both pages is `Clock: <hostname>`
as well (settings 4259, update 3027). On test-clock over USB; both
read back `Clock: test-clock`.

#### Quiet after the press that ends a ring (after the favicon) — DONE, verified on the panel 2026-11-02

The fourth pass's "two-button slap to silence the alarm changes a
setting". The button task reports one event per button, so a slap
with both hands is two events: the first ends `Ringing` and the
second lands in `Clock`, where a top short steps the colour, a bottom
short the brightness, a bottom long toggles the alarm and a top long
opens alarm-set, and the persist task writes the change. The same
for a press that ends the IP or the PIN display. The second event
can trail the first by up to 800 ms, not a few: when one button of
the pair is let go early its `Short` ends the ring at its release,
and the other, still held, sends its `Long` at the long-press
threshold, which the button task had held back only while both were
down.

**Fix, in `core/src/ui.rs`.** `Ui` gains `quiet_until:
Option<Instant>`. The press that ends `Ringing`, `ShowIp` or
`ShowPin` sets it to `now + PRESS_QUIET` (one second, past the 800 ms
a trailing `Long` can take), and `handle` drops any event before that
instant. The window is only started by a press, never by a timeout,
and only by one that ends one of those three modes; a deliberate
press one second after silencing the alarm works as before. A
hurried double press of one button in clock mode stays two steps,
since two presses are two presses. The `Hold` never reaches `Ui`
during a ring anyway: the display task acts on it first. Nothing
changes in the button task or the display task.

**Host checks.** A `Short` ends a ring and a second `Short` 20 ms
later leaves the colour alone; a `Long` of the other button 790 ms
after leaves the alarm armed and the mode in `Clock`; a `Short` after
the window steps the colour. The same shape for the PIN display, and
a timeout that ends the IP display starts no window.

**Verify on the panel.** Ring the alarm (set it a minute ahead, or
wait for it) and slap both buttons: the ring stops and the colour,
brightness and armed mark are what they were, with no `settings:
saved` in the log. Slap with one hand lingering: the same. Then a
single short press a second or more later still steps the colour.

**Panel, 2026-11-02.** The user rang the alarm and slapped both
buttons, with and without a lingering hand: the ring stopped and
nothing else changed; a press a second later worked as before.

#### Two pages for the two PIN actions (after the press quiet) — DONE, verified on the panel and in a browser 2026-11-02

The update page did two things with one PIN field: a firmware upload
and the API token form under it, with nothing saying which to do or
that the one PIN serves either. The user asked for two pages, each
issuing its own PIN, and for the token page to be reachable by its
address only: the settings page keeps its `Update` link, and nothing
links to `/token`, so a visitor who follows links can at most upload
an image, and the token is a thing one changes on purpose, from the
documentation.

**`GET /update`** is the upload alone: the firmware line, a sentence
saying what to do (choose the image, type the four digits the clock
is showing, press Upload), the file input first, the PIN under it,
the button and the progress bar. It issues a PIN as before.

**`GET /token`** is new: the same heading, whether a token is set,
the same PIN sentence, the new-token field (`type=password`), the PIN
and a `Set token` button. It issues a PIN as before (the same
`auth::issue_pin`, so opening either page replaces an unused PIN and
the lockout notice is the same). It posts `POST /token` with the PIN
in the header exactly as the old form did; the route is unchanged, and
the script path with `Bearer` is unchanged. An empty token still
clears, since that is how the script paths are closed, but the page
asks the browser's `confirm()` first, because an empty field and a
press is otherwise one slip away from closing `ota.sh`. The page
links back to the settings; nothing links to it.

**A spent PIN leaves the panel.** Found by the user on the first try:
a wrong PIN answered `403: wrong PIN`, the next click `403: no PIN
issued: open the page first`, and the panel kept showing the digits
for the rest of the minute. The display learned of a PIN only from
`issue_pin`; now `auth::SHOW_PIN` carries an `Option<u16>`, and
`token_or_pin` signals `None` after any attempt that spent the PIN
(right, wrong or expired; not the attempt with nothing pending, which
has nothing on the panel to end). `ui::hide_pin` goes back to the
clock from `ShowPin` only, without the press quiet window, since no
press happened; `pin_leaves_the_panel_when_spent` covers both. A
successful upload spends the PIN too, so the panel goes to the clock
before the scan stops for the write. The Upload button, which came
back after an error for a retry that could only fail (the PIN is
spent), now stays disabled after any answer as the token page's does,
and both pages' status lines end with "Reload this page for a new
PIN." after an error.

**Shared script.** The `$`, `say` and `pin` helpers both pages use
are one constant, `form::PIN_JS`, so the two scripts are only their
own request.

**Texts.** The refusals in `auth` that named the update page name
neither: "no PIN issued: open the page first", "PIN expired: reload
the page", "no credentials: the PIN the clock shows or the token", and
"no token set: build one in or set one on /token". The doc comments
in `auth`, `config`, `ui` and `display` that said "the update page's
PIN" say the page's PIN, either page.

**Tests.** `update_page_fits_the_response_buffer` loses the token
argument; `token_page_fits_the_response_buffer` is new; and
`nothing_links_to_the_token_page` builds the settings, setup and update
pages and asserts none contains `/token`, so the rule outlives this
entry, and that the update page no longer links to itself from its
firmware line. The pages at their worst: update 2476 (was 3027),
token 2043, of `PAGE_MAX` 6016; both scripts were checked with `node
--check` from the served page. `AGENTS.md` describes the token page
under `POST /token` and carries the new sizes.

**Verify in a browser.** Open `http://<ip>/update`: one sentence of
instructions, a file input, a PIN field, Upload, no token form; the
panel shows a PIN. Open `http://<ip>/token`: the token form, whether
one is set, a new PIN on the panel; set a token with it, `/api` says
`token_set: true`; reload, leave the field empty, press Set token: the
browser asks, Cancel changes nothing and no request is made (the PIN
is still good); OK clears it and `token_set` turns false. On either
page, a wrong PIN: `403: wrong PIN` and the panel back on the clock at
once, the button greyed and the line saying to reload for a new PIN;
reload and the panel shows a new one. Search the
settings page's source for `/token`: nothing.

**Panel and browser, 2026-11-02.** The user found the first cut's gap
on the update page (a wrong PIN left the digits on the panel, and the
Upload button invited a retry that could only fail); with the spent
PIN signalled to the display and both buttons staying disabled, the
user verified the pages and the panel: the wrong PIN takes the digits
off at once and the line says to reload for a new one.

### M12 — Home Assistant over MQTT — DONE, verified in Home Assistant by the user and on the bench 2026-11-07

**Goal.** The clock shows up in Home Assistant as one device with its
settings as entities (colour, brightness, alarm time, armed, skip,
days, melody, 12 hour, temperature on the panel) and its readings as
sensors (room temperature, light, signal, uptime, self-heals, whether
the alarm is ringing), plus three buttons (play the melody, silence,
reload). Nothing is typed into Home Assistant: the clock announces
itself with MQTT discovery. The broker is a setting on the page, with
build-time defaults like the Wi-Fi credentials and the token.

**What Home Assistant expects** (its MQTT integration page, read
2026-11-07). Discovery is a JSON payload on
`<prefix>/<component>/<object_id>/config`, prefix `homeassistant`.
Since 2024.11 one payload per *device* is the recommended form:
component `device`, root keys `dev` (device: `ids`, `name`, `mf`,
`mdl`, `sw`, `cu` for the configuration URL), `o` (origin: `name`,
`sw`, `url`), `cmps` (components, each with `p` = platform and
`uniq_id`), and shared `stat_t`, `avty_t`, `qos` that every component
inherits. Keys have documented abbreviations (`cmd_t`, `val_tpl`,
`cmd_tpl`, `dev_cla`, `stat_cla`, `unit_of_meas`, `ent_cat`, `ic`,
`ops`, `pl_on`, `pl_off`, `pl_prs`). The payload need not be
retained: the recommended way is to subscribe to Home Assistant's
birth message, `homeassistant/status` = `online`, and resend
discovery when it arrives, after a short random delay. An empty
retained payload on the config topic removes the device. State topics
should be retained so an entity has a state right after a restart.
The `time` platform takes a parsable time string as state and sends
`HH:MM:SS` as the command; `select` takes `ops` and echoes the chosen
option; `switch` and `binary_sensor` take `pl_on`/`pl_off`; a sensor
whose template renders `None` goes `unknown`.

**Client crate: `rust-mqtt` 0.6.0** (the user's pick, and it fits:
released 2026-09-22 as the rewrite of obabec's crate, `no_std`, async
over `embedded-io-async` 0.7 and `heapless` 0.9, both the versions
this firmware already uses; MIT OR Apache-2.0). It does the protocol
and nothing else: no reconnect, no keepalive loop, no background
task, which is what this firmware wants, since every other network
task here owns its own loop. Features: `default-features = false,
features = ["v5", "bump", "log", "log-level-warn"]`, so no `alloc`;
received topics and payloads land in a `BumpBuffer` over a 1 KB
static, reset after each event is handled (the one `unsafe` call in
the module: the event has been dropped by then). The client's const
parameters are small: one QoS 1 flight at most, two subscriptions,
no topic aliases, no user properties. Checked 2026-11-07: with those features the crate
builds for `riscv32imc-unknown-none-elf` on stable with no `alloc`,
pulling in only `const_fn` beyond what the firmware already has.
Everything is published at
QoS 0: the broker is on the LAN and TCP delivers to it, and QoS 0
keeps the session stateless, so `clean start` on every connect.
`poll_header` is documented cancel-safe and `poll_body` is run to
completion, which is what the select loop below relies on. The one
constraint it imposes: **the broker must speak MQTT 5**. Mosquitto
1.6 and later does (Home Assistant's Mosquitto add-on ships 2.x), so
do EMQX and NanoMQ. No TLS: `embedded-tls` or esp-mbedtls would cost
on the order of 100 KB of flash and tens of KB of RAM, the add-on's
default listener is plain 1883, and the LAN is the trust boundary the
rest of the clock already assumes (the page and `/api` are plain
HTTP). Give the clock its own broker user, with an ACL on
`esp32clock/#` and `homeassistant/#` if the broker has ACLs.

**Settings.** A `Broker` struct in the settings, keys `mqtt_host`,
`mqtt_port`, `mqtt_user`, `mqtt_password`:

| field | type | rule |
| --- | --- | --- |
| `host` | `Text<64>` | an IPv4 literal or a DNS name: `[A-Za-z0-9.-]`, no spaces; empty means MQTT off |
| `port` | `u16` | 1 to 65535, default 1883 |
| `user` | `Text<32>` | printable, no spaces; empty for an anonymous broker |
| `password` | `Secret<64>` | any printable; never read back, `mqtt_password_set` in the JSON |

Record version 10 appends them after `temp_show`: a length byte and
64 for the host, two bytes for the port, a length byte and 32 for the
user, a length byte and 64 for the password, so `RECORD_LEN` goes
from 171 to 336 and `storage.rs`'s scratch from 256 to 512 (the item
is the record plus 9, padded to a word). A version 9 record reads as
MQTT off, as the append-only rule says. The flags byte gains bit 1,
`FLAG_MQTT_CLEARED`, with the token's meaning: a host cleared on the
page stays cleared across a boot of a build that has `MQTT_HOST`
built in; a host never set is filled from the build.

Build-time defaults, read by `core/src/config.rs` with
`rerun-if-env-changed` in `core/build.rs`: `MQTT_HOST`, `MQTT_PORT`
(1883 when unset), `MQTT_USER`, `MQTT_PASSWORD`. `Settings::seed`
fills the whole broker from the build when the record's host is empty
and not cleared, the same shape as the Wi-Fi seed; the CI build sets
none, so its image has MQTT off until the page turns it on.

**On the page and the API.** The settings page gets a fourth
fieldset, "Home Assistant", with Broker, Port, User and Password.
The password field is write-only and always posted: empty keeps the
stored one, as the Wi-Fi password does on the setup form. Clearing
the host turns MQTT off and clears the user and password with it, so
the record carries no stale secret; that is also the way to drop a
password without a new one. The form takes the four keys without the
token, as the user asked (the page already changes the hostname and
the alarm from the LAN). `/api` takes them with the `Bearer` token
only, like the network keys, since pointing the clock at another
broker would hand that broker the stored password; refused on the
key alone, never on the value. The setup form on the access point
does not carry them: it is for the network, and the page is a click
away once the clock is on it. The JSON gains `mqtt_host`,
`mqtt_port`, `mqtt_user`, `mqtt_password_set`, and the read-only
`mqtt`: `"off"`, `"connecting"` or `"connected"`, plus `ringing`
(bool), which the discovery below uses. The status line under the
form says `MQTT connected to 10.1.1.5:1883`, `MQTT connecting to …,
last: connection refused`, or `MQTT off`. The fieldset is about 550
bytes of page; the settings page at its worst was 4259 of
`PAGE_MAX` 6016, so it should fit, and the page test will say. If it
does not, `RESPONSE_BUF` goes to 8 KB (6 KB of RAM across the three
listeners), not the page.

**Topics.** The device id is `clock-` and the last three bytes of
the MAC in hex, `clock-a1b2c3`: stable across hostname changes, so no
retained message goes stale when the user renames the clock, and
unique on a LAN (the access point's `clock-xxxx` uses two bytes; it
is a name people type, this one is not). The base is
`esp32clock/clock-a1b2c3/`:

| topic | way | payload |
| --- | --- | --- |
| `…/status` | out, retained; also the will | `online` or `offline` |
| `…/state` | out, retained | the `/api` JSON, byte for byte |
| `…/set/<key>` | in, one subscription `set/+` | the value exactly as `/api` takes it |
| `…/cmd` | in | `play`, `silence`, `reload` |
| `homeassistant/status` | in | `online` resends discovery after 0 to 2 s |
| `homeassistant/device/clock-a1b2c3/config` | out, not retained | the device discovery payload |

The state is the existing `form::write_json` output, so there is no
second serialisation to keep in step: every entity's `val_tpl` reads
a key of that JSON. A command is `form::apply_one` on a copy with the
key from the topic and the payload as the value, committed with one
`settings::update` if it parsed, exactly the `/api` path; the keys
allowed are the clock's own, `color`, `brightness`, `alarm`,
`armed`, `days`, `skip`, `melody`, `twelve_hour`, `temp_show`,
`temp_offset` and `tz`, and anything else (`hostname`, `ssid`,
`password`, `token`, `mqtt_*`, unknown) is logged and dropped: the
broker is not a way to move the clock to another network or broker.
The state is published after connect, on every settings change
(debounced 300 ms like the persist task, so a run of button presses
is one message), on the ringing edges, and every 60 s for the
sensors, the signal and the uptime. `cmd` `play` is
`buzzer::preview` of the stored melody, `silence` ends a ring the way
a press does, `reload` is `heal::reboot` after the goodbye below.

**Discovery payload.** One device, named after the hostname
(renamed on the next resend, which a hostname change triggers),
manufacturer `esp32clock`, model `HU-058 on ESP32-C3`, `sw` the
firmware version, `cu` `http://<ip>/`; origin `esp32clock` with the
version and the repository URL; root `stat_t` the state topic,
`avty_t` the status topic, `qos` 0. Components, each `uniq_id`
`clock-a1b2c3_<name>`:

| component | platform | keys |
| --- | --- | --- |
| Colour | `select` | `ops` the palette names, `val_tpl` `{{ value_json.color }}`, `cmd_t` `set/color` |
| Brightness | `select` | `ops` the offered steps as `10`, `25`, `50`, `val_tpl` `{{ value_json.brightness }}`, `cmd_t` `set/brightness` |
| Alarm | `time` | `val_tpl` `{{ value_json.alarm }}`, `cmd_t` `set/alarm`, `cmd_tpl` `{{ value[:5] }}` (HA sends `HH:MM:SS`, the parser takes `HH:MM`) |
| Alarm armed | `switch` | `val_tpl` `{{ 'on' if value_json.armed else 'off' }}`, `pl_on` `on`, `pl_off` `off`, `cmd_t` `set/armed` |
| Skip next alarm | `switch` | the same on `skip` |
| Alarm days | `text` | `val_tpl` `{{ value_json.days }}`, `cmd_t` `set/days`: `all`, `weekdays`, `weekend` or `mon,tue,…`, the API's own format |
| Melody | `select` | `ops` the tune names and `random` |
| 12 hour clock | `switch` | on `twelve_hour`, `ent_cat` `config` |
| Temperature on panel | `select` | `ops` `off`, `c`, `f`, `ent_cat` `config` |
| Temperature | `sensor` | `dev_cla` `temperature`, `unit_of_meas` `°C`, `stat_cla` `measurement`, `val_tpl` `{{ value_json.temp_c }}` (`null` renders `None`, which HA reads as unknown) |
| Light | `sensor` | `unit_of_meas` `%`, `ic` `mdi:brightness-5`, `val_tpl` `{{ value_json.light }}` |
| Signal | `sensor` | `dev_cla` `signal_strength`, `unit_of_meas` `dBm`, `ent_cat` `diagnostic` |
| Uptime | `sensor` | `dev_cla` `duration`, `unit_of_meas` `s`, `ent_cat` `diagnostic` |
| Self-heals | `sensor` | `ic` `mdi:restart-alert`, `ent_cat` `diagnostic` |
| Ringing | `binary_sensor` | `val_tpl` on `ringing`, `pl_on` `on`, `pl_off` `off`, `ic` `mdi:alarm-light` |
| Play melody | `button` | `cmd_t` `cmd`, `pl_prs` `play` |
| Silence | `button` | `pl_prs` `silence` |
| Reload | `button` | `dev_cla` `restart`, `pl_prs` `reload`, `ent_cat` `diagnostic` |

Seventeen components come to about 3 KB with the abbreviations,
built with `core::fmt::Write` into a 4 KB static (`mqtt::DISCOVERY_MAX`);
a host test builds it at the worst hostname and asserts the size,
printing it as the page tests do. A `light` entity for colour and
brightness was considered and dropped: a light has an off state the
panel does not have, and the palette is nine named duty triples, not
RGB, so two selects say what the clock does.

**The task.** `src/mqtt.rs`, `mqtt_task(stack)`, spawned on the
station only (the access point has no LAN). Its loop:

1. Host empty: wait for a settings change. Otherwise wait for the
   stack's address, resolve the host (an IPv4 literal is parsed, a
   name goes to `dns_query`, 5 s timeout), connect TCP (5 s timeout;
   receive buffer 1 KB, send buffer 2 KB), then `connect` with a 60 s
   keepalive, the user and password, the will on `status` =
   `offline` retained, client id the device id.
2. Publish `status` = `online` retained, subscribe `set/+` and
   `homeassistant/status`, publish discovery, publish the state.
3. Select over `poll_header`, the settings receiver, the ringing
   edge (a `Signal` the display task sets on the edges), the 60 s
   state timer, the keepalive timer (ping after 45 s without a send,
   drop the connection if no `Pingresp` within 10 s), and the
   goodbye signal. A header is followed by `poll_body` without
   cancellation, then the event is dispatched and the bump buffer
   reset. Publishes happen between polls from the same task, so no
   two client futures ever exist at once.
4. Any error: `abort`, close the socket, `connecting`, log the reason
   once (`connection refused`, `not authorized`, `dns failed`, …),
   back off 5 s doubling to 60 s, and from step 1. A change to the
   broker settings disconnects cleanly and starts over with the new
   ones; a cleared host publishes `offline` retained, disconnects and
   goes to the off wait, so Home Assistant shows the device
   unavailable rather than a stale state.

**Goodbye before a reset.** `heal::reboot`, `heal::reset`,
`heal::switch_mode` and the OTA reboot already pause `RESET_PAUSE`
after `scan::stop`; they first set a `GOODBYE` signal and wait up to
500 ms for the task to publish `offline` retained and send
DISCONNECT, so Home Assistant shows the clock unavailable within a
second instead of after the broker's 90 s keepalive timeout. A panic
cannot do this, as with the scan; the will covers it late.

**Sizes.** One TCP socket: `StackResources` 10 to 11. One more
`Watch` receiver: `RECEIVERS` 3 to 4. Static RAM: socket buffers 3 KB,
bump buffer 1 KB, discovery 4 KB, the state JSON about 1 KB, so about
9 KB; check `.stack` in `llvm-readelf -S` after. The minute heartbeat
gains an `mqtt:` line: state, connects, publishes, commands, last
error.

**Pure half, `core/src/mqtt.rs`**: `DeviceId` from the MAC and its
topics; `write_discovery`; `command_key` (the key after `set/`, or
the `cmd` payload as a `Command`); `apply_command` with the allow
list over `form::apply_one`. Tests: the discovery payload fits
`DISCOVERY_MAX` at the worst hostname and parses as JSON (`serde_json`
as a `core` dev-dependency, host only), every `value_json.<key>` it
references is a key of `write_json`'s output, the keys every `set/`
component commands are accepted by `apply_command` and the refused
keys are refused, `command_key` on good and bad topics, the broker
rules, the record round trip for version 10 and the read of version
9, `apply_sets_every_key` with the four new keys, the page sizes, and
`seed` with the cleared flag. The task itself is not under test, like
the other socket tasks.

**Files.** New: `core/src/mqtt.rs`, `src/mqtt.rs`. Touched:
`core/src/settings.rs` (the struct, record, seed, flags),
`core/src/config.rs` and `core/build.rs` (the four variables),
`core/src/form.rs` (`apply_one`, `write_json`, the fieldset, the
status line, `Status.mqtt` and `Status.ringing`), `src/http.rs`
(`status()`, the `Bearer` rule for `mqtt_*` on `/api`),
`src/display.rs` (the ringing signal), `src/heal.rs` and `src/ota.rs`
(the goodbye), `src/storage.rs` (scratch), `src/bin/main.rs` (spawn,
`StackResources`, the heartbeat line), `Cargo.toml` (`rust-mqtt`,
`serde_json` for the host), `AGENTS.md` (settings, layout rows,
sizes, env vars) and `README.md` (a Home Assistant section under
"Using the clock").

**Verify on the bench.** Build with `MQTT_HOST=<broker>
MQTT_USER=clock MQTT_PASSWORD=…` or type them into the page; the log
says `mqtt: connected to <host>:1883 as clock-xxxxxx`. On the host,
`mosquitto_sub -v -t 'esp32clock/#' -t 'homeassistant/#'` shows
`status online`, the state JSON and the discovery payload. Home
Assistant's MQTT integration shows a device named after the hostname
with the entities above. Change the colour from Home Assistant: the
panel follows within a second and the state topic carries the new
colour. Press the top button: the Colour select follows. `mosquitto_pub
-t esp32clock/clock-xxxxxx/set/alarm -m 06:45` sets the alarm;
`-t …/set/hostname -m x` is logged as refused and changes nothing.
Set the alarm a minute ahead: Ringing turns on, Silence ends it.
Restart Home Assistant: the entities come back without a clock
restart (the birth resend). `POST /reload`: the device goes
unavailable within a second and comes back. Pull the clock's power:
unavailable after about 90 s (the will). A wrong password: `mqtt:
connect refused: not authorized`, the status line says so, the
back-off grows to 60 s. Clear the host on the page: `MQTT off`, the
device unavailable. The page still fits: `cargo host-test -- --nocapture`
prints the sizes.

**Built 2026-11-07, on the host.** As planned, with these differences.
The `tz` key is not on the bus and the thermometer offset is: the
test that ties the discovery payload to `SET_KEYS` found both without
an entity, and a `number` for the offset (−10 to 10, step 0.1, `mode`
box, `cmd_tpl` `{{ '%.1f' % value }}` since Home Assistant sends a
float) is useful for calibrating from the dashboard, while the
timezone is the page's. The alarm's `cmd_tpl` was not needed: the form
parser takes `HH:MM:SS` and checks the seconds, so `/api` does too.
Nineteen entities, 4174 bytes at the worst hostname; `DISCOVERY_MAX`
is 4608. The guarded-key flag `credentials: bool` became
`form::Allow { network, broker }` with `NONE`, `PAGE` and `ALL`, since
the page may set the broker but not the network. The settings page is
5149 of 6016 at its worst (the broker host appears twice, in the field
and the status line), so `RESPONSE_BUF` stays. The record is version
10, 336 bytes, the scratch 512. `rust-mqtt` builds with `v5`, `bump`
and `log` only, no `alloc`. The main stack went from 84.5 KB to
69.6 KB: the task's future is about 15 KB (socket buffers 3 KB,
bump 1 KB, state 1 KB, discovery 4.6 KB and the client), more than the
estimate but with room. `serde_json` is a host-only dev-dependency
(`[target.'cfg(not(target_os = "none"))'.dev-dependencies]`) so a
check aimed at the chip does not try to build it. 86 host tests pass,
clippy is clean on both crates; nothing has run on the panel yet.

**Bench, 2026-11-07.** The user flashed the clocks and checked the
device page in Home Assistant first (three clocks on the bus). Then the
bench board (`test-clock`, the old one without a thermometer), with the
serial monitor and a paho subscriber on the host: after the monitor's
USB reset, Wi-Fi at 5 s, the broker at 7 s of uptime, the will
(`offline`, the old session taken over) and `online` within the same
second, the discovery payload 4112 bytes for a ten-letter hostname.
The OTA to the committed build published `offline` through the
goodbye at the reboot and was back `online` 7 s later; `/reload` the
same. `set/color` changed the panel and the state and `/api` within a
second; `set/hostname`, `set/tz` and a bad colour were logged and
dropped. A birth message on `homeassistant/status` brought the
discovery again within 2 s. An alarm set a minute ahead over the bus
rang on the minute, the state said `ringing: true`, `cmd silence`
ended it and the next state said false. `mqtt_*` on `/api` without the
token is a `403`. A wrong password: `broker setting changed,
disconnecting`, then `not authorized` (what Mosquitto says for a bad
password) with the back-off 5, 10, 20 s, the JSON `connecting` and the
page's line carrying the reason; the right password cut the wait
short and reconnected. A cleared host published `offline` retained and
logged `off`, the page said `MQTT off.`; the host restored, back
`online` with discovery in 2 s. The heartbeat line reads
`mqtt connected (1 connects, 7 published, 7 commands)`.

Two findings, fixed in the same session: the task never subscribed
to the `cmd` topic, so `play`, `silence` and `reload` over the bus did
nothing until the second OTA (`SUBSCRIBE_MAXIMUM` is 3 now); and the
state went out twice at every connect, because a settings change
pending in the receiver from before the connect was published after
the connect's own state, so the receiver is drained once the state is
out. One more, not fixed: the root `build.rs` runs `git describe` only
when `.git/HEAD` or the index changes, so an image built from a dirty
tree right after a commit still says the clean hash (`f4d705f` for the
build with the `cmd` fix).

### Web flasher and releases — planned 2026-11-07

**Goal.** A clock is flashed the first time from a browser, with no
toolchain: a page on GitHub Pages with an ESP Web Tools install button
(Web Serial, Chrome or Edge) that offers the firmware of any GitHub
release, newest first. Tagging `v*` builds the release, attaches the
images and publishes the page; `v0.1` is the first.

**Reference.** The user's `esp32-bthidbridge`: `docs/index.html` with
`<esp-web-install-button manifest=…>` from unpkg's `esp-web-tools`, a
`release.yml` on `v*` tags that builds, creates the release with
`softprops/action-gh-release`, writes the manifest with `jq` and
deploys `docs/` to the `gh-pages` branch with
`peaceiris/actions-gh-pages`; Pages serves that branch. The same
shape here, with two differences: one merged image instead of three
parts, and every release kept on the page.

**The image.** `espflash save-image --merge --skip-padding -s 4mb`
with the partition table gives one file to flash at offset 0: the
bootloader at 0, the table at 0x8000, `otadata` erased (so `ota_0`
boots, where the app is at 0x20000), 971 KB for the current firmware.
`nvs` is not in the file, so a reflash keeps the settings, and ESP Web
Tools' erase prompt (`new_install_prompt_erase`) is the way to start
over. `new_install_improv_wait_time` is 0: the clock has no Improv
serial, and the default makes the dialog wait 10 s for it. Built with
the CI's prebuilt settings: no network (the clock boots as its access
point `clock-xxxx`, password `esp32clock`, the setup form at
`http://192.168.4.1/`), Vancouver time, API token `esp32clock`.

**Releases.** `.github/workflows/release.yml`, on `push` of tags
`v*`, `permissions: contents: write`. Job `build`: the firmware,
clippy and the host tests as in `ci.yml` (a tag never publishes a
build the CI would have failed), then two images, `esp32clock-ota-
<tag>.bin` (`save-image`, for `/update`) and `esp32clock-<tag>.bin`
(merged, for the flasher), as one artifact. Job `release`: the two
files on the GitHub release the tag names, with a body pointing at
the flasher and `/update`. Job `pages`: `docs/<tag>/manifest.json`
(name, the version from the tag, the erase prompt, no Improv wait,
one `ESP32-C3` build with the merged image at offset 0) next to a
copy of both images, the same again under `docs/latest/`, and
`docs/` deployed to `gh-pages` with `keep_files: true`, so earlier
tags stay on the site. The version in the firmware comes from `git
describe`, which sees annotated tags only (`build.rs` runs it
without `--tags`), so a release tag is `git tag -a`. Pages is
enabled once the branch exists (`gh api -X POST …/pages` with the
branch, or the repository settings) and serves
`https://anarsoul.github.io/esp32clock/`.

**The page.** `docs/index.html`, the one file in the repository
under `docs/`: the clock in a paragraph, what the browser needs
(Chrome or Edge on a desktop, the clock on USB; if the C3 does not
enter its bootloader on its own, hold BOOT while plugging it in), a
select of releases filled from the GitHub API
(`/repos/anarsoul/esp32clock/releases`, public, CORS allowed, 60
requests an hour unauthenticated), newest first and the newest
selected, which sets the button's `manifest` to `<tag>/manifest.json`;
when the API cannot be reached the select shows "latest" and the
button uses `latest/manifest.json`. Below the button: what the image
is configured with (the README's table), the first boot through the
access point, the token note, and that later updates are the
`esp32clock-ota-<tag>.bin` of the release through `/update`. Links to
the repository and the releases. The script from unpkg as in the
reference, so both projects' pages age the same way.

**Docs.** The README's "Prebuilt image" section becomes "Prebuilt
images": the flasher first, the release files second, the Actions
artifact of every push third. `AGENTS.md`'s build section names the
release workflow, the tag rule and `docs/`.

**Verify.** Push, tag `v0.1`, watch the run: a release with two
files, a `gh-pages` branch with `v0.1/` and `latest/`, Pages enabled
and the manifest reachable. The flasher itself needs a browser with
Web Serial, which the session has not; the merged image is verified
on the bench by `espflash write-bin 0x0` of the file downloaded from
the page: the board boots `v0.1` on `ota_0` with its settings kept.

**Built 2026-11-07.** The first `v0.1` tag ran green (three jobs, the
release with both images, the `gh-pages` branch with `v0.1/` and
`latest/`), and the bench then showed the plan's premise wrong: the
merged image is contiguous from 0, so its bytes 0x9000 to 0xF000 are
erased `nvs`, and `espflash write-bin 0x0` of it left the bench board
with no settings, scanning as an access point. Two more facts from
the same run: `write-bin` leaves the chip in the bootloader (an
`espflash reset` after it), and the boot after the wipe seeds from the
build, so the board was put back with a USB flash seeded with its
network and token and one `/api` post for the rest. The fix: the
installer flashes four parts (the bootloader and the partition table
cut out of the merged image with `dd`, an `otadata` of 0xFF, and the
OTA image at 0x20000) and never touches `nvs`, so the erase prompt
means what it says; the merged file stays on the release for
`write-bin`, documented as clearing the settings. The `v0.1` tag was
moved to the fixed commit minutes after the first push, and the
release re-made.

### Timezone rules: POSIX TZ strings and daylight saving — planned 2026-11-07

**Goal.** The clock follows daylight saving by itself. The timezone
setting becomes a POSIX TZ rule (`PST8PDT,M3.2.0,M11.1.0`,
`CET-1CEST,M3.5.0,M10.5.0/3`, `<+0545>-5:45`), the form ESPHome and
every libc take, evaluated on the chip from the NTP time; no broker,
no Home Assistant automation, nothing on the LAN involved. The page
sets it without the user knowing the notation: one button takes the
browser's own zone and turns it into the rule. The user asked
2026-11-07 after the question whether Home Assistant could push the
offset over MQTT (it cannot without an automation on its side; its
MQTT integration publishes nothing about itself but the birth
message, and its API gives an IANA name the chip has no table for).

**The rule, `core/src/tz.rs`.** `Tz<'a>`: the standard zone (name,
offset in seconds east of UTC) and an optional daylight zone with its
two transitions. `parse(&str) -> Option<Tz>` is a `const fn` over the
bytes, so a bad rule in the build fails the build with a message, as
a bad `TZ_OFFSET_MINUTES` does: a name of three or more letters, or
`<…>` of three or more letters, digits, `+` and `-`; an offset
`[+-]h[h][:mm[:ss]]`, positive west as POSIX has it (stored negated,
east positive, as the rest of the clock counts); the daylight name,
its offset or the default of one hour ahead; then `,start[/time],end
[/time]` with `Mm.w.d` (month, week 1 to 5 where 5 is the last, day 0
Sunday), `Jn` (1 to 365, no leap day) or `n` (0 to 365 with it), and
a time `[+-]h[h][:mm[:ss]]` from −167 to 167 hours, default 02:00.
Nothing else: no daylight zone without rules (the TZ-database
default rules are not knowable here), no trailing junk, at most
`TZ_MAX` (48) bytes. `Tz::period_at(unix) -> Period { offset_secs,
dst, name }`: the civil year of the standard-time instant (days to
year, month and day by Howard Hinnant's algorithm), both transitions
of that year as Unix instants (the start is given in standard time,
the end in daylight time), then daylight iff between them, or outside
them when the end comes first in the year (the southern hemisphere).
`Tz::std_offset_minutes()` for the record's old field. `fixed(minutes)
-> Text<TZ_MAX>`, `const`, writes `<-07>7` or `<+0545>-5:45` for a
plain offset, so the old setting and the old build variable still
mean something. `clock::local_time(unix, offset_secs: i32)` replaces
the minutes argument, and `LocalTime` gains `offset_secs`, which the
alarm uses below.

**The setting.** `Settings.tz: Text<TZ_MAX>` replaces
`tz_offset_minutes`; `is_valid` parses it. The record goes to version
11 with the rule appended (a length byte and 48 bytes, `RECORD_LEN`
336 to 385; the 512-byte map scratch still holds it) and keeps writing
the standard offset in minutes at bytes 7 and 8, so a rollback to an
older image shows standard time; a record of version 10 or lower is
read as `fixed(minutes)`. The build variable `TZ_RULE` seeds the rule;
`TZ_OFFSET_MINUTES` still works and becomes `fixed(…)`, so the CI and
the README's command stay right (`TZ_RULE` wins if both are set).
`TZ` itself is not read: shells export it as an IANA name, which
would break every build.

**The form and the API.** The `tz` key takes the rule, or a plain
integer of minutes east of UTC for the old scripts, which lands as
`fixed(…)`; the JSON answers the rule as a string (a type change for
a reader of `tz`, noted in the README; nothing on the clock reads it
back). Two read-only keys: `utc_offset`, the minutes east of UTC in
effect right now, and `dst`, whether daylight time is in effect, both
`null` before the first time sync, computed in `http::status` so
`form.rs` stays clockless; the status block gets a line `Local time
Fri 14:32 PST, UTC−08:00.` for checking a rule. `set/tz` on the bus
stays refused. The HTML value and the JSON string go through
`HtmlStr` and `JsonStr`, since a quoted name has `<` and `>`.

**The page.** In Other, `Timezone, POSIX rule` is a text field with
the rule, `maxlength=48`, and beside it a button `Use this browser's
zone` with a `<small>` that then says which zone that was
(`America/Vancouver`). The button runs `posixTz(zone)` from
`/tz.js`, a static script embedded like the favicon and sent with
its `max-age`, so the page itself grows by the field and the tag only
(the settings page is at 5149 of 6016). The script gets the browser's
IANA zone from `Intl.DateTimeFormat().resolvedOptions().timeZone`,
finds the UTC offset the zone has on 1 January and 1 July of this
year through `formatToParts`; equal offsets are a fixed rule; otherwise
it bisects each half-year to the minute for the instant the offset
changes, and writes each transition as `Mm.w.d/time` in the local
time in force before it, with week 5 when the day is the last of its
kind in the month (so the EU's last Sunday stays right in a year
where it is the fourth). The names are `Intl`'s short zone names when
they are letters (`PST`, `CET`) and `<+hhmm>` otherwise; the daylight
offset is written only when it is not one hour ahead. The result is
put in the field and the unsaved-changes mark raised. It is a
heuristic from one year's two transitions, which is what the rule
notation is for; a zone with no daylight time or a new rule next year
is one click again. The setup page on the access point gets the same
field and button, since the phone doing the setup knows its zone.
`scripts/tz-check.mjs` runs the same function under Node against a
list of zones and their known rules, the one check of the script the
host can do (`Intl` is the browser's and Node's alike).

**The alarm across a transition.** Two cases the alarm minute logic
gets wrong today. Spring forward skips an hour: an alarm at 02:30 is
never "behind == 0" and does not ring. The fix: `Ui` remembers the
last tick's local minute of the day, and the alarm is also due when
that minute was before the alarm time and this one is at or past it
(the crossing), within the same local day, so an alarm in the skipped
hour rings at 03:00. Fall back repeats an hour: an alarm at 01:30
would ring at 01:30 PDT and again at 01:30 PST, since the second is
an hour later in Unix time. The fix: the time the alarm last rang is
kept in local seconds (`unix + offset`), so the repeat compares equal
and counts as handled; the 07:00 to 07:30 case (the user moves the
alarm after it rang) still rings, since 07:30 is after 07:00 in local
seconds too. The carry record's field is renamed `last_alarm_local`
with the same codec and layout version; the first boot after this
update reads the old Unix value as local, off by the offset for one
boot, which matters only to the ten-minute boot grace on that boot.

**Docs.** `AGENTS.md` (the settings list, the build command,
`TZ_RULE`, the `tz` key, the two read-only keys, the route, the
sizes, the test count), the README (features, variables table, the
settings paragraph, the `tz` type change), and the `tz.js` check in
`scripts/`.

**Tests.** `tz`: the examples above parse to the expected offsets and
rules, the rejects (`TZ`, `PST`, `PST8PDT`, `PST8PDT,M3.2.0`, `M13`,
week 0 or 6, day 7, a 49-byte rule, trailing junk); `period_at` at
the second before and the second of each 2026 transition for the US,
the EU, Sydney, Lord Howe's half hour, Nuuk's negative time and the
`J` and `n` forms; `fixed` round-trips through `parse`;
`DEFAULT_TZ` parses. `settings`: the record round trip at version 11,
a version 10 record read as the fixed rule, the standard offset at
bytes 7 and 8. `form`: `tz` as a rule, as minutes and refused; the
JSON `tz` string and the two read-only keys; the page sizes with a
48-byte rule of `<` and `>`. `ui`: the skipped hour rings at 03:00,
the repeated hour rings once, the moved alarm still rings.
`scripts/tz-check.mjs` under Node for the browser half.

**Verify on the bench.** Flash, open the page: the field shows
`<-07>7` (the old record's minutes), the status line `Local time …
UTC−07:00`; click the button, see `America/Vancouver` and
`PST8PDT,M3.2.0,M11.1.0` in the field, save, see the same time (PDT
until November, and Vancouver no longer changes). Then `tz=CET-
1CEST,M3.5.0,M10.5.0/3` by `/api`: the panel moves nine hours, the
line says `CET, UTC+01:00`, `dst` false (November); put the rule back
by the button. The alarm cases are host tests only.

**Built 2026-11-07, verified on the bench the same day** (test-clock at
10.1.1.30, updated over the air three times by the PIN path, since the
board's token is the release image's and not the bench one; the
monitor's log supplied the digits). As planned, with these findings:

- The version 10 record came up as `<-07>7` on the first boot, the
  time carried, the image confirmed good; `/api` answers `"tz":"<-07>7",
  "utc_offset":-420,"dst":false`. `tz=CET-1CEST,M3.5.0,M10.5.0/3` moved
  the page's line to `Sun 00:20 CET, UTC+01:00`, the Pacific rule to
  `Sat 15:20 PST, UTC-08:00`, Sydney's to `Sun 10:20 AEDT, UTC+11:00,
  daylight time` (November is its summer), `America/Vancouver` is a
  `400`, and `tz=-420` put `<-07>7` back.
- The browser script, driven by headless Chromium through CDP against
  the real page: the click filled `PDT7` for `America/Vancouver`, named
  the zone and raised the unsaved mark. That answer took two fixes the
  host check alone did not find. First, Chromium's time zone data has
  Vancouver on PST in January 2026 and PDT from March with no return,
  so bisecting for the second change of this year found a nonsense
  instant (`M12.5.4/17`); the script now reads the rule from next year,
  when a zone dropping daylight time is already fixed, and checks each
  transition's candidate rules against the six years after. Second,
  a midnight transition is `Saturday 24:00` in Chile and `Sunday 00:00`
  elsewhere, and Nuuk's is `Sunday -1:00`; one year's data cannot tell
  them apart, the later years can, which is what the candidates are
  for. Names come from the first locale that has letters (en-US for
  PST, en-GB for CET, en-AU for AEST and Lord Howe's LHST, en-CA for
  NST, en-IN for IST, en-ZA for SAST); Moscow and Tokyo stay `<+03>`
  and `<+09>`. `scripts/tz-check.mjs` holds seventeen zones.
- A browser keeps `/tz.js` for a day (`max-age=86400`, like the
  favicon), so the second bench run clicked with the old script after
  the update. The pages load it as `/tz.js?v=<firmware version>`.
- The script is 5.7 KB and goes out of one response buffer; a `const`
  assert in `src/http.rs` keeps it under `RESPONSE_BUF` with its
  header. It passed 6 KB once with its comments and was trimmed.
- The alarm's "jumped over" rule was narrowed while the tests were
  written: it applies only when the UTC offset changed between two
  ticks, since a plain gap in the ticks (a time step) is what the
  ten-minute boot grace is for, and an existing test rightly refuses
  to ring an alarm an hour late.
- Sizes: `RECORD_LEN` 385, the settings page 5525 of 6016 (the rule
  field, the button, the local-time line and the versioned script
  tag), the setup page 3177 with twelve networks and 2324 without the
  list, the main stack 67.4 KB (from 69.6), 94 host tests (from 86).
  The Node check and the Chromium run are not in `cargo host-test`;
  the first is a command in `AGENTS.md`, the second a scratchpad
  script after the pattern of 2026-11-02.
- Not verified: the panel itself (the time moved nine hours in the
  JSON, not watched on the LEDs), the alarm across a real transition
  (host tests only), and the setup form's button on a phone.

### Codex review of 2026-11-07: eleven findings fixed — built 2026-11-07, released as v0.3 and soaked 11 h on the bench 2026-11-08; the fixes' own checks still owed

A review by another agent (`codex-review.md`, at commit 0a26b1d) listed
fourteen findings, six new and eight from this file's backlog. The user
set three aside as not bugs: disarming an alarm does not silence it
(the fourth-pass item; whoever disarms it from the page is awake and can
press a button, and MQTT has `silence`), the display task freezing
during an upload (the second-pass nit; the panel is dark for the write
anyway) and a deliberate reset discarding a change still inside the
save debounce (the second-pass item; a reload a second after a save is
the operator's doing). The eleven left, in the order of the work, with
the design of each fix:

1. **The settings page hands the MQTT password to a new host** (new,
   P1). `Allow::PAGE` lets the unauthenticated `POST /` change
   `mqtt_host` and the stored password goes along to whatever answers
   there. Fix in `form::apply`, after the pairs: on `Allow::PAGE` (the
   broker allowed without the network, so the token did not vouch for
   the request), a host that changed to another non-empty host while
   the body posted no non-empty `mqtt_password` and a password is
   stored is refused as `FormError::BrokerPassword`, a new variant with
   its own notice ("Nothing was saved: a new broker needs its password
   typed again."). The same host keeps its password on an empty field
   as before; clearing the host still clears the credentials; the token
   (`Allow::ALL`) is not subject to the rule. `POST /play` applies the
   form to a scratch copy and nothing is saved, so it uses `Allow::ALL`
   and a form with a changed host still previews. The page's password
   label says "empty keeps the stored one for the same broker". Tests:
   refused with the stored password, accepted with a typed one, with
   the same host, with an empty host, with no stored password, and
   under `Allow::ALL`.
2. **A rule with an offset past 14 hours fails the record round trip**
   (new). `tz::parse` takes offsets to 24 hours (POSIX), `from_record`
   checks the version-2 minute field against `TZ_LIMIT_MINUTES` for
   every version, and `to_record` writes the rule's standard offset
   there: `tz=XXX-15` is saved and the next boot throws the record
   away, network and token with it. Fix: `to_record` clamps the legacy
   field to the limit (an older image reads a fixed rule within its
   range) and `from_record` checks that field only for versions before
   11, where it is the timezone. Test: the round trip of `XXX-15` and
   `<-14>14`, and a version-10 read of the record.
3. **MQTT startup swallows a broker or hostname change** (new). The
   `try_changed` after the first publishes drops whatever came in
   during resolve, connect and the publishes without looking. Fix:
   consume the notification just before the state is built (so the
   state published is the newest), keep the value, and after the start
   compare it: a different broker says farewell and returns
   `Outcome::Settings`; a different hostname from the one the discovery
   was built with schedules a resend.
4. **A refused subscription looks like success** (new). `subscribe`
   returns when SUBSCRIBE is sent; the SUBACK comes as an event the
   loop ignored. Fix: keep the three packet identifiers, handle
   `Event::Suback` in the loop (a reason code at or above 0x80 fails
   the session with "subscribe to <topic> refused"), and fail it too
   when any is still unanswered after `SUBACK_TIMEOUT` (10 s). The link
   is reported connected as before; a refusal takes it back to
   connecting with the reason on the page.
5. **The browser script invents a fixed rule** (new). It compares the
   January and July offsets and calls the zone fixed when they match
   (Casablanca's Ramadan dip is lost), and returns the first candidate
   when none predicts the later years. Fix: `changes(yr)` samples the
   offset at every month's start and bisects each month that differs,
   so it finds every change of the year; none is a fixed zone, two are
   a rule, anything else throws; `rule` throws when no candidate
   validates. The page shows the error's message in place of the zone
   name ("no rule for Africa/Casablanca"). `scripts/tz-check.mjs` gets
   Casablanca (a throw), Tehran and São Paulo (fixed since 2022 and
   2019) and prints a thrown message as the result.
6. **A transition displaced across New Year is evaluated in the wrong
   year** (new). `period_at` takes the two transitions of the instant's
   own year only; a time of `-1` or `25` or more can put one in the
   neighbouring year. Fix: evaluate the transitions of the year before,
   the year and the year after, and take the latest that is not after
   the instant: a start means daylight time, an end standard time. The
   southern hemisphere needs no special case then. Tests: the review's
   `STD0DST,J1/-1,J180/2` at both sides of midnight, and the existing
   transitions unchanged.
7. **A failed upload can resume the scan under a reset** (second pass,
   P1). `scan::STOP` has two owners. Fix: `scan::halt`, for the three
   deliberate resets in `heal.rs`, latches `FINAL` and sets `STOP`
   inside one critical section; `resume` clears `STOP` and signals
   only when `FINAL` is not set, in a critical section too, so the
   two cannot interleave; the scan task, woken by `RESUME`, goes back
   to waiting while `STOP` is still set. `stop` stays for the upload.
8. **A failed save is never retried** (second pass). Fix: after an
   `Err` the persist task waits 2 s, doubling to 60 s, and saves again
   with whatever the settings are then (a change during the wait is
   taken up); the warning carries the wait.
9. **A `POST` without `Content-Length` is an empty form** (second
   pass). Fix: `Head::content_length` becomes `Option<usize>`, a
   `Transfer-Encoding` header is noted, and `read_request` answers
   `411 Length Required` to a `POST` without a length and `400 Bad
   Request` to any request with a transfer encoding, through a new
   `Reading` result the listener writes before closing. A `GET`
   without a length is zero, as before.
10. **Two rescans share one `Signal`** (fourth pass). Fix: a scan
    generation counter written by `ap_task`; `request_scan` returns
    the generation it saw and `scan_done(gen)` polls every 100 ms
    until it moves. Any number of waiters, each released by the scan
    after its request.
11. **A bad record is logged with its secrets** (fourth pass). Fix:
    the warning prints the version byte and the length.

Each fix strikes its backlog item; the three set aside are struck as
"not a bug, decided 2026-11-07" with the reason above. Verification:
`cargo host-test` with the new tests, clippy on both targets, `node
scripts/tz-check.mjs`; on the bench, a page save with a changed broker
host and no password (the notice), `curl -X POST http://<ip>/` without
a body (`411`), and the Rescan button pressed twice on the access
point. The MQTT and scan interleavings are by construction.

**Built 2026-11-07, host and build only.** As planned, with these
findings:

- The broker rule lives in `form::apply` after the pairs, so the order
  of `mqtt_host` and `mqtt_password` in the body does not matter; the
  test posts them both ways and without the field. `Allow` is compared
  whole (`== Allow::PAGE`), so the token's `ALL` and the setup form's
  `ALL` are both exempt. The page's label grew by 20 bytes: settings
  5564 of 6016.
- The record keeps its layout: version 11 still writes the standard
  offset in bytes 7 and 8, clamped to ±14 hours, and ignores them on
  the way back in. A version 10 image reading a record saved with
  `<+24>-24` gets `<+14>-14`, which is the nearest thing it can hold.
- `period_at` is six candidate transitions and a maximum; the
  southern-hemisphere branch went. All the existing transition tests
  pass unchanged.
- The SUBACK match is by packet identifier against a three-entry
  `heapless::Vec` the start fills; the reason strings are static so
  `Outcome::Failed` can carry them. The link reads connected from the
  start as before, and a refusal takes it back to connecting with
  `subscribe to set/+ refused` on the page.
- `scan::halt` and `resume` each take a critical section for their two
  flags; the scan task, woken, checks `STOP` again before it runs. The
  three deliberate resets call `halt`; the upload keeps `stop`.
- The first version of the `411`/`400` path built a second `Response`
  in another arm of the listener loop, and the compiler did not overlap
  it with the first: `.bss` grew 17.5 KB (6 KB per listener) and the
  main stack fell to 50.3 KB. Found by building `HEAD` in a worktree
  and comparing `llvm-readelf -S`. Hoisted to one buffer per loop:
  `.bss` is 304 bytes over `HEAD`, the stack 67.1 KB (from 67.4).
- `src/tz.js` passed the 6016-byte ceiling by 105 bytes with its new
  comments and is 5766 after trimming them to a line each; the why is
  here. The Node check has twenty zones, Casablanca expected to throw
  `no rule for Africa/Casablanca`; Tehran and São Paulo come out fixed
  from a year with no change, which the January-July comparison would
  also have given, but now by the same path as every other zone.
- 95 host tests (from 94: the TZ test is new, the others grew),
  clippy clean on both targets, the release build 0.3 KB larger in
  static RAM.

Not verified on the panel or the bench: the fixes themselves. The
image ran 11 h on the bench without a fault (the soak of 2026-11-08 at
the end of this file), which says the fixes broke nothing on the paths
a running clock takes, not that each fix works. To check them, in
order: `curl -X POST http://<ip>/` with no body is `411 Length
Required` and the alarm stays armed; the settings page with the broker
host changed and the password empty comes back with the notice, and
with the password typed connects to the new host; `GET /api` after a
reboot still shows the network with `tz=<+15>-15` saved; a reload
during an upload that then fails (a `curl` killed mid-body) leaves the
panel dark until the clock is back, never a lit row; the setup page's
Rescan from two tabs at once returns both; and the MQTT page line after
pointing the clock at a broker whose ACL denies `set/+`.

### Scan restart gap: measure the whole dark time — built 2026-11-07, not yet verified on the bench

The heartbeat's restart gap read an average of 1 to 2 µs and a maximum
of 49 to 99 µs per minute, and the question was whether Wi-Fi caused
the maximum and whether it could be cut. Read against the sources of
esp-hal 1.2.2, esp-rtos 0.4.0, esp-radio 1.0.0-beta.1, esp-alloc 0.11
and esp-println 0.18:

- Nothing preempts the scan executor by priority. It runs at interrupt
  priority 2; the Wi-Fi MAC and power interrupts and the esp-rtos tick
  are priority 1, the driver's threads are thread mode. What holds the
  restart off is a critical section held anywhere: esp-sync's
  `RawMutex` is `csrrci mstatus, 8` on the C3, which masks every
  interrupt until the holder leaves. The long ones are the Wi-Fi
  blob's lock (`wifi_int_disable`, on every frame), esp-alloc's lock
  around `linked_list_allocator` (the blob allocates and frees a
  buffer per frame; a walk of a fragmented free list is tens of
  microseconds), and esp-println's lock, held for the whole write
  while it busy-waits on a full USB-JTAG FIFO, up to 50 000 iterations,
  when a monitor is attached (with no host it drops output instead).
  So the maximum is Wi-Fi activity, through interrupts-off stretches
  rather than the Wi-Fi interrupt, and a higher executor priority
  would change nothing.
- The statistic read low. `ended_at` was taken after `transfer.wait()`
  returned and the gap recorded before the restart call, so it covered
  only the slice between the task waking and the restart: not the done
  interrupt's latency, which is where a critical section lands, and
  not the setup of the next transfer.
- It is invisible either way: 72 µs once in a 3085 µs transfer is a
  2.3 % dip in one lap, and a digit goes dark for 60 µs in every cycle
  by design (twelve units between its planes).

**Built:** the gap is now the interval between two consecutive starts
less the transfer's length on the wire, `scan::TRANSFER_NS` (two
nibbles per byte at 20 MHz, 3 084 700 ns), so it covers the latency,
the wake, the swap and the setup. The start is stamped before
`half_duplex_write_buffer` and recorded after it returns, with the DMA
already streaming, so the statistics no longer run in the dark. A DMA
start failure and a stop both clear the reference start, which closes
the backlog item about retries inflating the figure. `GapStats` also
keeps a histogram, ten buckets of 10 µs with the last open (90 µs and
up), printed as the heartbeat's next line, `scan gap histogram, 10 us
buckets, last open: n0 n1 … n9`, first bucket first. The shape tells
the causes apart where the maximum alone cannot: a spike of a few
hits in the last bucket against thousands in the first is a long
critical section such as esp-println's FIFO wait, a smear across the
middle buckets is the heap lock's walk varying with fragmentation,
and a second hump at a fixed place is a periodic holder.

**Expect** the first heartbeats to read higher than before: the
average a few microseconds up (the interrupt latency and the setup
are now in it) and the maximum above the old 49 to 99 µs, more so with
the monitor attached. A number that moves with Wi-Fi load (a page
load, an upload) is the critical sections; one that does not is the
fixed cost of the restart path. For a window without esp-println's
FIFO wait in it, attach the monitor after the clock has run a while:
the first heartbeat line it prints covers a minute spent mostly with
no host, when the printer drops output instead of waiting.

**First bench numbers, 2026-11-07, monitor attached, the clock at
−31 dBm on the near access point.** Three minutes, the first from
boot: average 28, 24, 24 µs; maximum 910, 1078, 976 µs; histogram
(10 µs buckets, last open) `0 14602 1645 670 653 368 214 109 81 929`,
then `0 16164 766 589 521 361 199 85 51 561` and `0 16176 733 633
582 353 171 83 36 531`. A minute with eight settings-page loads: `0
16010 699 612 514 356 193 98 58 740`, maximum 657. Read:

- No gap under 10 µs, three quarters in 10 to 19 µs: the fixed cost
  of the restart path (done interrupt, executor wake, esp-hal's setup
  with its descriptor walk and `update` wait, the DMA start). The old
  statistic's 1 to 2 µs was the middle slice of it.
- A steady smear from 20 to 89 µs, about 3600 a minute, that page
  loads do not move at all. Not traffic, then.
- Some 530 to 560 gaps a minute of 90 µs and up when idle, close to
  the access point's beacon rate (one per 102.4 ms, 586 a minute),
  930 in the boot minute, 740 with the page loads: the long holds
  follow Wi-Fi work. Power save is not involved: esp-radio sets
  `PowerSaveMode::None` itself and the firmware leaves it. A 1 ms
  gap is a 30 % dip of one 3 ms lap, below what the eye picks up.
- The smear has a second explanation besides critical sections, found
  in esp-hal: the SPI done handler is bound with `#[handler]` at its
  default priority 1, the level of the Wi-Fi MAC interrupt and the
  esp-rtos tick, and same-level interrupts do not preempt each other.
  A transfer ending while the Wi-Fi ISR runs waits for it to return
  before the scan executor can be woken at all; its priority 2 never
  came into play. The driver's ISR runs for every received frame,
  beacons included, which fits a smear at a rate the LAN does not
  change.

**Experiment, built 2026-11-07:** `esp_hal::interrupt::enable(
Interrupt::SPI2, Priority::Priority3)` after `into_async()` in
`scan_task`. `enable` remaps the peripheral interrupt to the CPU
interrupt of that level and leaves the bound handler alone, so
esp-hal's own handler runs above the Wi-Fi ISRs and pends the
executor at once; the handler clears the interrupt and wakes the
task, nothing else, so preempting another driver's ISR with it is
safe. Expected: if the 20 to 89 µs buckets collapse into the 10 to 19
one, the smear was ISR queueing and what remains in the open bucket
is critical sections; if the smear stays, it is short critical
sections at a high rate (the blob's own lock), and either way only
the segmented transfer below removes the rest.

**Result, 2026-11-07, same bench:** no measurable change. Idle minute
before `0 16164 766 589 521 361 199 85 51 561`, after `0 16071 960
792 496 247 94 54 28 553`: the middle buckets sum to 2572 and 2671,
the open bucket 561 and 553, and a minute with ten page loads adds
about 200 to the open bucket both times and nothing to the middle.
The maximum stayed at 0.7 to 1.1 ms. So the smear is not the done
interrupt queueing behind the Wi-Fi ISR; it is short critical
sections at a high rate, the driver blob's own lock being the
likeliest since it is taken on every operation, and the open bucket
is its longer holds plus the heap lock on traffic. No priority
reaches either. The remap stays, since it is right in principle and
costs nothing, but it is not a fix. Only the segmented transfer below
takes the CPU out of the loop.

**Planned next, if the gap is to go away rather than be measured:**
the C3's SPI2 has a DMA-controlled segmented transfer. Each segment
begins with a 15-word conf buffer the SPI reads from the DMA stream
(a bitmap word, `0x7FFF` with the magic `0x2` in bits 31:28, then
ADDR, CTRL, CLOCK, USER, USER1, USER2, MS_DLEN, MISC, DIN_MODE,
DIN_NUM, DOUT_MODE, DMA_CONF, DMA_INT_ENA, DMA_INT_CLR), followed by
that segment's data; `usr_conf_nxt` in the USER word says another
segment follows, `slave.usr_conf` enables the mode,
`slave.dma_seg_magic_value` sets the magic and `cmd.conf_bitlen` the
conf state's length (esp-idf v5.4 `components/hal/esp32c3/include/
hal/spi_ll.h`, section "Segmented-Configure-Transfer"; esp-hal's
metadata lists `spi_master.has_dma_segmented_transfer` for the C3
but the driver does not expose it). A descriptor ring of one segment
whose conf word says "next" runs the scan forever with no CPU in the
loop: the only gap is the conf state, a fixed microsecond or two
after the blanking frame. Design: a `DmaTxBuffer` of one conf
descriptor and eleven data descriptors of two cycles each (2800
bytes, the last plus the blanking frame), the last pointing at the
first, owner check off as in esp-hal's `DmaLoopBuf`; bring the bus
up as today, run one ordinary dark transfer, copy the fourteen
registers from `SPI2::regs()` into the conf words with `usr_conf_nxt`
and the segment's MS_DLEN set, and start the ring through
`half_duplex_write_buffer`, whose `prepare()` call on the buffer
lands after esp-hal zeroes the SLAVE register and before it sets
`usr`, which is where `usr_conf`, the magic and `conf_bitlen` go.
Publishing rewrites the eleven data descriptors' buffer pointers
(single aligned stores the DMA reads per lap; whole cycles per
descriptor, so a mid-lap swap changes the image at a cycle boundary
as today) and frees the old buffer after one lap time. `stop`/`halt`
clear `usr_conf_nxt` in the conf word; the chain ends within two
segments, under 7 ms, on the blanking frame. Caveat: Espressif never
enabled its segmented-transfer driver on the C3 (no
`SOC_SPI_SCT_SUPPORTED` in its soc_caps, though the low-level code is
there), so the first step is a bench spike that only proves the ring
runs. Not planned: trimming the critical sections (esp-println's lock
is a feature, the heap lock would need another allocator).

### Scan ring spike: the segmented transfer on the bench — built 2026-11-08, not yet run

The spike, `src/ring.rs` behind the cargo feature `ring-spike`,
answers one question before anything depends on it: does the C3's
SPI2 run a looping DMA-controlled configurable segmented transfer in
quad mode from our descriptor chain? It runs on a bare board; the
user has a scope for the wires.

**What the manual says** (ESP32-C3 TRM v1.4, 27.5.8.5, read from the
PDF): each segment's CONF state reads a conf buffer from the TX DMA
stream, `SPI_BIT_MAP_WORD` first, with the magic in bits 31:28
compared against `SPI_DMA_SEG_MAGIC_VALUE` (the example uses 0xA; a
mismatch ends the transfer with `SPI_SEG_MAGIC_ERR`) and bits 0 to 13
naming the registers whose new values follow, one word each, in
order: ADDR, CTRL, CLOCK, USER, USER1, USER2, MS_DLEN, MISC,
DIN_MODE, DIN_NUM, DOUT_MODE, DMA_CONF, DMA_INT_ENA, DMA_INT_CLR
(esp-idf's `spi_ll.h` sets 0x7FFF, a fifteenth bit the table does
not list; the spike names only what it needs). Only registers that
change need a word. `SPI_USR_CONF` goes on before `SPI_USR`;
`SPI_USR_CONF_NXT` (USER bit 15) set means another segment follows,
clear means the transfer ends after this one; `SPI_CONF_BITLEN` sets
the CONF state's length, (value + 5) APB clocks; the CONF and data
buffers should have their own descriptors; `SPI_DMA_SEG_TRANS_DONE`
fires once, at the end of the whole transfer, and `SPI_TRANS_DONE` at
the end of each bus transfer. Half-duplex MOSI is listed as supported
for the mode, with the CPU-controlled variant not.

**Design.** `Ring` implements esp-hal's `DmaTxBuffer`: a conf
descriptor of 8 bytes (the bitmap with magic 0xA and bit 3, then the
USER word) and eleven data descriptors of two cycles each (2800
bytes; the last 2847 with the blanking frame; `CHUNK_SIZE` is 4092),
the last pointing back at the first, owner check off and no
write-back as in esp-hal's `DmaLoopBuf`. The data is the existing
transfer buffer, `DmaTxBuf::split`. Startup runs one ordinary quad
transfer so esp-hal programs every register, then starts the ring
through `half_duplex_write_buffer`; esp-hal calls the buffer's
`prepare` after it has zeroed SLAVE and programmed USER and before
it sets `usr`, so `prepare` sets `usr_conf`, the magic,
`conf_bitlen` 0 and `usr_conf_nxt`, and copies the USER register
back into the conf buffer's USER word with `usr_conf_nxt` set: the
CONF state then rewrites exactly the configuration in force. The
stop is the manual's: clear `usr_conf_nxt` in the conf buffer's USER
word (a volatile store and a fence; the C3 has no data cache), the
next CONF state applies it, and the transfer ends within two
segments on the blanking frame. The spike loop runs the ring for 60
s, counting laps by polling `trans_done` every 500 µs and clearing
it, watching `seg_magic_err` and an early `dma_seg_trans_done`; then
stops it, waits up to 20 ms for `dma_seg_trans_done`, logs the run
(laps, gap per lap as elapsed over laps less `TRANSFER_NS`, the stop
latency, whether `usr` cleared), reclaims the ring with
`transfer.wait()` (which only checks `usr`), pauses 3 s and starts
again, for ever. The heartbeat's `ring:` line has the totals, the
DMA channel's current and last-EOF descriptor addresses, the raw SPI
interrupt bits and `usr`. The test image lights every segment in the
shortest plane at the lowest current step, so the data lines carry
ones for the scope. The normal scan loop is `scan_loop`, compiled
out when the feature is on; the clock's build is unchanged.

**Verification on the bench board**, `cargo run --release --features
ring-spike -- --port /dev/ttyACM0`:

1. The boot prints `ring: 12 descriptors: conf 8 bytes, data 2800
   bytes each, last 2847 bytes; 3084 us per lap on the wire`.
2. A minute later `ring: ran 60000 ms, N laps, G us gap per lap; stop:
   seg_trans_done true after S us, usr clear`. N near 19 450 and G of
   a few microseconds is the result hoped for; a G that is the
   current 24 to 28 µs would mean the mode is not chaining at all
   (it would also have stopped after one segment, so N would be 1).
   S under 7000.
3. The heartbeat's `ring:` line: runs climbing by one a minute, laps
   by about 19 450, magic errors and early ends 0, stops ok equal
   to runs, the descriptor address changing between heartbeats.
4. On the scope, GPIO8 (CLK of driver 1) against GPIO9 (DATA): bursts
   of 47-byte frames every 5 µs unit, and at the segment boundary
   the blanking frame followed by a silent stretch of the CONF state
   (a conf-word fetch plus 5 APB clocks, a microsecond or two) and
   then the next frame. The lines must sit idle through the CONF
   state; any clock edge there is the risk the panel would see.
   `SPI_HZ` can be dropped to 2 MHz in `scan.rs` for a slower scope,
   the mechanism does not care.

**First run, 2026-11-08, bench board on ttyACM0:** `ring: ran 60000
ms, 0 laps, 0 us gap per lap; stop: seg_trans_done true after 3742
us, usr clear`. The chain loops: a stop request lands mid-segment,
the next CONF state applies it and that segment is the last, so a
running ring stops in 3085 to 6170 µs, which 3742 is; a chain that
had ended after one segment would have had `seg_trans_done` set
already and a stop time near zero. `trans_done` is not raised per
segment in CONF mode, hence 0 laps; the lap counter moved to the
GDMA's `out_eof`, with `suc_eof` set on the last data descriptor (as
esp-hal sets it on the last descriptor of every SPI transfer), polled
and cleared on the channel's interrupt registers.

**Second run, 2026-11-08:** `ring: ran 60000 ms, 19447 laps, 0 us gap
per lap; stop: seg_trans_done true after 3756 us, usr clear`, the
heartbeat at descriptor 6 with last EOF 11, magic errors and early
ends 0. 60.000 s over 19 447 laps is 3085.3 µs a lap against 3084.7
on the wire: the gap is about 0.6 µs, some 48 APB clocks for the
conf descriptor and its two words, the five-clock CONF state and the
first data descriptor's fetch, and it is fixed, since no CPU is in
the loop. The spike prints it in nanoseconds from here on. The mode
works on the C3 in quad mode from our ring; what remains before the
scan path is rebuilt on it is the scope's view of the lines through
the CONF state.

**Scope aids, 2026-11-08.** The user's scope triggers on edges only,
and the lap boundary's 0.6 µs CONF state is hidden among the planes'
padding (the cycle's frames sit 0.3, 5.3 and 15.3 µs apart in planes
0, 1 and 2: 47-byte frames in regions of 50, 100 and 200 bytes). Two
aids in the spike build: `CONF_BITLEN` 3995 stretches the CONF state
to 50 µs, the one gap in a lap longer than any padding (the scan
wants 0), and SPI2's chip select is routed to GPIO10, high through
the CONF state and low during a segment, so its rising edge is the
lap boundary. Trigger on GPIO10 rising, watch GPIO8 (CLK) and GPIO9
(DATA): the blanking frame just before the edge, then 50 µs with
both lines flat, then the next lap's dense plane-0 frames. Any edge
in the flat stretch is what the drivers would shift in.

**Scope, 2026-11-08, bench board, CS on GPIO10 as the trigger:** the
lap boundary shows the blanking frame, then the 50 µs CONF state with
CLK and DATA flat low, then the next lap's plane-0 frames. No edge in
the stretch on either line, CS clean. The high level of CLK and DATA
droops a little on the unloaded pins through the probe, which the
panel's inputs will not show. The spike has answered its question:
the C3 chains our segments in quad mode, stops on request within two
segments, idles its lines through the CONF state, and the gap is a
fixed 0.6 µs. The scan path is rebuilt on it next (below).

**Outcomes.** Laps counting and the descriptor moving: the mode works
and steps 2 to 5 of the plan above turn the spike into the scan
path. Laps 1 and early ends 1 a run: the hardware does not chain,
the conf word is wrong or the first segment reads its registers from
the register file only; look at `seg_magic_err` first. `trans_done`
never set but the descriptor address moving: the flag is not per
segment; the gap is then measured by other means (the scope, or the
GDMA EOF counter with `suc_eof` on the last descriptor).

### Soak of v0.2 and v0.3 on the bench — 2026-11-07/08, passed

A long run of the released images on the bench clock (`test-clock.lan`,
the C3 on the adapter, no sensors fitted, -40 to -48 dBm on the near
access point), to catch what the short checks cannot: a slow heap
leak, an MQTT session that drops and recovers, a self-heal with no
cause, a lost ping that the TCP retries hide, the time drifting.
Tooling, all host side: `espflash monitor` into a serial log (its
attach reset the board, so the run starts from a clean boot),
`scripts/watch-clock.sh` every 30 s (one 498-byte ping and one `GET
/api`), a watch on both logs for `WARN`, `ERROR`, panic, reset, failed
probe or lost request, and a summary script read every 30 minutes:
uptime, heals, RSSI, the heap-free histogram from the heartbeats, the
ping and `/api` tallies, the page's local time against the host, the
heartbeat's `mqtt` counts. The rule was the user's `PDT7` throughout,
the alarm disarmed, the broker the home one.

**v0.2, 4h28m** (17:54 to 22:25 local, MQTT saved from the page at
5 min, which did not reset the clock): no reset, no heal, no warning;
540 of 541 pings and 541 of 541 `/api` fetches answered (the miss was
12 s after boot, with HTTP answering at the same instant, and never
again); one MQTT session, 269 publishes, a publish a minute. Heap free 54080 bytes
on every heartbeat for 3h40m, then one step down of 188 bytes to
53892 that held to the end, plus two one-minute dips (1704 and 680
bytes) that recovered the next minute. Nothing in the log at the step.

**Update to v0.3** by `POST /ota` with a panel PIN from the monitor
log, the release's `esp32clock-ota-v0.3.bin`: written to `ota_1` in
23 s, reboot with the time carried (`local time 22:25:58` from the
record, NTP at 6 s said 22:26:03), every setting kept, Wi-Fi up at
5 s, MQTT connected at 7 s, `ota: image v0.2 confirmed good` logged
by the old image on its way out.

**v0.3, 10h59m** (22:26 to 09:25 local, overnight):

| | |
| --- | --- |
| resets, self-heals, panics, WARN, ERROR | 0 |
| pings from the host, every 30 s | 1315 of 1315 |
| `/api` fetches, every 30 s | 1315 of 1315, slowest 256 ms |
| gateway probes | all answered |
| MQTT | one session, 661 publishes, no reconnect |
| local time against the host | matched at every checkpoint, through midnight |
| heap free | 54032 bytes on 655 of 659 heartbeats |
| RSSI | -40 to -43 dBm |

The four other heartbeats were one-minute dips of 1704, 2908, 3408
and 8520 bytes, each back to 54032 the next minute, with nothing but
the watcher's `/api` in the log around them. Only the Wi-Fi driver
uses the heap (the firmware does not allocate), and 8.5 KB is a
handful of received frames held in its queue at the instant of the
heartbeat, so these are LAN bursts, not a leak. The 188-byte step of
v0.2 did not recur in 11 h; one step in 15 h of running is noted, not
a finding. NTP corrected 80 to 90 ms every hour in the same
direction: the crystal runs about 24 ppm fast, the hourly sync keeps
the panel within a tenth of a second, nothing to do. The soaked image
carries the old restart-gap statistic (the measured-whole-dark-time
rework above is later than the tag): average 2 µs, maximum 66 to
104 µs a minute, steady for 15 h.

**Not covered:** the alarm never rang; no setting was saved after the
broker, so the save retry and the double-rescan fix did not run; the
codex fixes' own checks (the `411`, the broker-host notice, the
`<+15>-15` rule across a boot, the lit-row case, the denied `set/+`)
stay owed above. The run says the release is stable on the paths a
clock takes every day: boot, join, sync, serve, publish, tick.

### Hardware-chained scan: the ring as the scan path — DONE, verified on the panel by the user and on the bench 2026-11-08 (soak running)

The spike above showed the C3 chains our segment in quad mode, stops
on request within two laps, idles its lines through the CONF state,
and that the gap between laps is a fixed 0.6 µs. This makes the ring
the scan path and takes the CPU out of the loop for good.

**Design.** `src/ring.rs` keeps `Ring` as the spike had it, with
`CONF_BITLEN` 0 and the chip-select, feature and run-stop-restart loop
gone. `scan_task` builds the bus, runs one ordinary dark transfer so
esp-hal programs every register, builds the ring over that buffer,
hands both buffers to the publisher (`ring::init_slots`) and starts
the ring; then it waits on a `STOP_REQ` signal. `stop` sets the flag,
signals, and polls `HALTED` as before; the task clears
`usr_conf_nxt` in the conf USER word (`request_stop`), waits up to 20
ms for `dma_seg_trans_done`, cancels the transfer if `usr` is still
set (never seen; counted as `cancelled` in the heartbeat), reclaims
the ring with `transfer.wait()`, sets `HALTED` and waits for `resume`,
which restarts the ring through `half_duplex_write_buffer`; `prepare`
re-arms the flag and rewrites the conf word from the live USER
register. `halt` latches as before. A stale `STOP_REQ` from a stop
issued while already halted is told by the `STOP` flag. The
`publish` entry point forwards to `ring::publish`: take the slots,
wait until two laps and a bit (`HOLD`) after the previous swap so the
lap then in flight has let go of the buffer, build the image into the
spare, repoint the eleven data descriptors with aligned volatile
stores and a fence (`point_at`; the DMA reads each descriptor as it
reaches it, so the image changes at a cycle boundary within one lap),
swap the slots' roles and put them back. The slots are a
`Mutex<RefCell<Option<Slots>>>` taken for the duration of a publish;
the display task is the one publisher, and the first publish waits a
millisecond at a time for the scan task to have built the ring. The
`FREE`/`READY` channels, `init_spare`, `GapStats`, the histogram and
the priority-3 remap of the done interrupt are gone; `ScanHardware`
carries both buffers. The heartbeat's `scan` field is images
published, stops and cancellations, running or halted, and the
descriptor index the DMA is at. `ring.rs` is the one module touching
registers (SPI2's SLAVE, USER, CMD and the raw interrupt flags; the
DMA channel's current descriptor), through esp-hal's `regs()`. The
callers (`display`, `ota`, `heal`) are unchanged. Build and clippy
clean on both targets, 95 host tests, main stack 66.9 KB (67.1
before).

**Verification.** Bench board first:

1. The boot line `scan: 140 us/cycle (7142 Hz), 5 us/unit, 22 cycles
   and 30847 bytes per lap in 12 descriptors; hardware chained`.
2. Heartbeats: `scan N images published, 0 stops (0 cancelled),
   running, at descriptor k`, N up by about 60 a minute with the
   time tick and k different each time.
3. `scripts/reload.sh <ip>`: one stop, the clock back with the time.
4. An upload killed mid-body: `stops 2`, running again, the heartbeat
   moving.

Then on a clock: a steady image at boot, the colour and brightness
buttons landing without a blink, the time ticking, the temperature
face at 30 s, `/reload` and `POST /ap` blanking cleanly with nothing
left lit, the killed upload bringing the panel back through `resume`,
and a completed upload rebooting onto the new slot. A swap mid-lap (a
cycle of the old image next to a cycle of the new, 140 µs) is by
design invisible; anything visible at a button press is a swap
problem.

**Bench, 2026-11-08, test clock at 10.1.1.30 (the user ran the panel
first: the display works).** The first bench run found a regression
from the codex fixes of 2026-11-07, not the ring: `scripts/reload.sh`
got `411 Length Required`, because the new rule refused every `POST`
without `Content-Length`, and the body-less command posts (`/reload`,
`/ap`, `/pin`, `/scan`) are sent as a bare `curl -X POST`, which
carries no length. Fixed: `http::reads_body` names the routes that
read a body (`/`, `/api`, `/play`, `/token`, `/ota`), and only those
get the `411`; the others take a missing length as none. The same run
then lost the station (`MissingAcks` at 352 s, the near access point
refusing with `Class3FrameFromNonAssociatedStation`, the far one at
−73 dBm expiring ten authentications in a row), so the connect budget
fired the self-heal: `heal: no connection after 10 attempts over 101
s ... blanking the panel and rebooting`, the ring stopped through
`halt`, the time carried, the ring back at boot, the station back
after five more attempts. Wi-Fi, not the scan; the upload test did not
run and is redone with the fixed firmware, over the air.

**Bench, second and third runs, 2026-11-08.** The token in `.env` is
not the one this clock stores (the record wins over the build), so the
first attempts were `bad token`; a PIN from `POST /pin` (sent with an
empty body, which carries a length the old firmware's `411` rule
takes) and `POST /token` set it from the serial log, the bench path
as designed. Then: a truncated upload (200 000 of a declared 650 000
bytes) gave `ota: writing image ... to ota_1`, `upload failed after
196608 of 650000 bytes: body read failed; the running image stays`,
and the heartbeat `2 stops (0 cancelled), running, at descriptor 9`:
the stop and resume path through the ring works, twice. The full
upload failed at 90 KB when the station dropped (`net: link down`,
then `AuthenticationExpired` from the far access point at −77 dBm),
the second drop in a quarter hour, both under HTTP traffic; the ring
resumed and ran on each time, descriptor index moving and about 56
images a minute throughout, refused upload bodies drained and all.
Whether the drops have anything to do with the new firmware is open:
this clock soaked eleven hours on v0.3 the same day without one, but
with no uploads and a page fetch every 30 s. To be watched with a soak
of the ring firmware under `scripts/watch-clock.sh`.

**Fourth run, 2026-11-08: everything passes.** The full upload of the
fixed firmware: `HTTP 200` in 60 s, `ota: rebooting into the new
image`, booted from `ota_1` with the time carried, `image ...
confirmed good` at the first probe, the ring running at boot. Then
`scripts/reload.sh` against the fixed firmware: `HTTP 200`, back in 3
s, `reset reason Some(CoreSw)`, time carried, the ring started again;
so the latched halt path works, and the `411` fix is verified. No
disconnect in that run. The user had already run the firmware on a
clock: the display works. The heartbeat's descriptor index came out
11 at the 60 s heartbeat after both boots, which is what a
deterministic lap period and a fixed boot timeline give; within a run
it read 0, 4, 8, 9 and 11. A soak of the ring firmware runs on the
bench clock from 12:50 with the serial log and
`scripts/watch-clock.sh`, to count station drops against the eleven
clean hours on v0.3: two drops under HTTP traffic in the first
quarter hour of testing are the open question, and the soak answers
whether they were the firmware or the afternoon's air.

Verified by the user on the panel the same day: the display works.
Not re-checked on a panel after the `411` fix, which touches nothing
in the scan; the buttons, the temperature face and the alarm are the
checks left for the next time a clock is flashed.

The drops are the board, not the firmware (the user, 2026-11-08): the
bench clock is a SuperMini, whose antenna mismatch AGENTS.md records,
and it would not connect at all upstairs the day before on the old
firmware. The soak stays running for what it is worth, with nothing
riding on it.

**Soak, 2026-11-08.** The pre-commit build ran 50 minutes from 12:50
on the bench clock with no drop, no probe failure and the heap flat at
54 080 bytes, then the committed `v0.3-3-gdf7f547` was uploaded over
the air at 13:41 (20 s, booted from `ota_0`) and the soak runs on
under `scripts/watch-clock.sh` and the serial log, to compare with
the eleven clean hours of v0.3. Result after 19 h 14 min (09:00 on
2026-11-09): 1154 heartbeats, no disconnect, self-heal or panic, the
scan running with 0 stops and the descriptor index moving every
minute; 2304 pings and `/api` fetches with one failure, the reboot
itself; one gateway probe lost at 07:08 and answered on the next.
The heap sat at 54 032 bytes for the first 7.9 hours, took one 188-byte
step to 53 844 at 21:35 with nothing but `/api` fetches in the log,
and held there for the remaining 11 hours; the six readings below
the line each landed with a response in flight and the next heartbeat
was back on it. At 25 h 36 min (15:18 on 2026-11-09): 1536 heartbeats,
still no disconnect, heal, stop or panic, the heap on 53 844 for 18
hours, 3071 probes with a second one lost at 10:10 and answered on
the next, 3066 watch samples with the one reboot failure. The soak
was left running. Drift, from the 26 hourly NTP offsets: the uptime
clock gains 83 ms an hour on NTP, -23.1 ppm over the 25 hours and
between -21.5 and -24.3 every hour, with roundtrips of 10 to 80 ms;
so the panel is never more than about 85 ms fast, and without the
network it would gain 2 s a day. At 31 h 4 min (20:45 on 2026-11-09):
1864 heartbeats, still nothing but the counters moving, heap on 53 844,
a third probe lost at 18:27 and answered on the next, 3719 watch
samples with the one reboot failure, drift unchanged at -23.1 ppm.
At 43 h 20 min (09:01 on 2026-11-10): 2600 heartbeats, no new event
of any kind, heap on 53 844, no probe lost since 18:27 the day before,
5188 watch samples, drift -23.1 ppm over 44 syncs; the lowest
heartbeat reading of the run, 47 028 at 21:41, landed four seconds
before the hourly NTP sync with its name lookup in flight and was
back on the line a minute later. At 90 h 58 min (08:39 on
2026-11-12, the soak still running): 5458 heartbeats, no disconnect,
heal, stop or panic, one Wi-Fi association and one MQTT session for
the whole run (5461 publishes), the heap on 53 844 for 83 hours and
47 028 still the lowest reading, drift -23.1 ppm over 91 syncs.
10 911 probes with eight lost: the three above, one at 08:16 on
2026-11-11 answered on the next, two in a row at 08:19 the same
morning answered on the third, the longest run of the soak, and one
each at 06:08 and 06:21 on 2026-11-12. The host's 498-byte pings and
`/api` fetches of the clock passed through all five (277 and 188 ms
around the double loss, against the usual 50 to 120), so the clock
was reachable from the LAN while the router did not answer it; two
mornings running between 06:00 and 08:30 points at the router or the
air at that hour, which the logs cannot settle. 10 986 watch samples
with two failures, the reboot and one at 16:25:02 on 2026-11-10, ping
and fetch both, fine again 38 s later: the clock's log has no event
there, its probes on either side passed and no request reached a
listener, so the sample was lost between the host and the clock, at
which end is not known.

**The soak's first disconnects, 2026-11-12.** After 92 h 13 min on
one association the clock was dropped twice, at 09:54:10 and at
10:10:16, both logged `MissingAcks` with the clock's own RSSI at its
usual -39 and -41, and both 0.1 to 0.2 s after it had served a watch
`/api` fetch. Each time it scanned, rejoined the same access point
9 s later, had its address 0.5 s after that and MQTT back 2 s after
the address (a third session by the end). No self-heal, no reset, the
uptime and the time kept, the scan at 0 stops, no probe lost; the heap
went back from 53 844 to 54 032 on the first rejoin, so the 188-byte
step of 2026-11-08 belonged to the association. The user takes this as
the Wi-Fi recovery working as designed. What it exercised is a
dropped association with the access point still in range: not the
connect budget, not the probe-triggered self-heal. The cause is not
settled: the second drop fell inside another session's measurement of
the new SuperMini-adapter clock on the bench (498-byte pings every
second to 10.1.1.11, a board that hears the access point 20 dB
louder), the first 8 minutes before that board appeared on USB. The
first watch ping after each rejoin, about 20 s later, was lost while
the fetch 2 s after it passed, unexplained.

### Thermometer offset: default −2.5 °C — DONE, verified on the bench 2026-11-09

The user's clocks typically need an offset of −2.5 °C (2026-11-09),
so that is the default instead of 0: `config::DEFAULT_TEMP_OFFSET_DC`
(−25 tenths), used by `Settings::DEFAULT`. No record change and no new
version. Who gets it: a clock with no record (a fresh flash, the web
installer with the erase) and one whose record is version 7 or older,
which never had the field and reads it from the defaults. A record of
version 8 or later keeps what it stores, 0 included: the record cannot
tell an offset of 0 the user chose from one never touched, and a
reading that moved 2.5 °C by itself after an update would be worse
than a default that is not applied. Tests: the older-records test
expects the default below version 8, and the JSON test `-2.5` for the
default settings.

**Verify on a clock.** After a USB flash with `nvs` erased (or a
merged image), `/api` says `"temp_offset":-2.5`, the page's field
shows −2.5 and the room line is 2.5 °C under the uncorrected reading;
a clock updated over the air with a stored offset keeps it.

**Bench, 2026-11-09.** A clock flashed over USB with `nvs` erased:
`settings: nothing stored, using defaults`, the settings line with
`temp_offset_dc: -25`, and the first sensor line `ntc 842 mV ... room
48.8 C (119.8 F) with offset -2.5`. So the default is applied. The
48.8 °C means nothing: this bench board has no NTC and no adapter
pull-up, so GPIO0 floats (842 mV) instead of reading high as a clock
with a missing sensor would.

The update path, same board and day: `POST /api temp_offset=0` (the
value a record cannot tell from untouched), then the same firmware
flashed over USB with `nvs` left alone, from a build with no
credentials in its environment. Eight seconds after the reset `/api`
said `"temp_offset":0.0` with the network and token from the record,
so a stored 0 stays 0 under the new default. The board was put back
to −2.5 afterwards. A record of version 7 or older picking up the
default is covered by the host test only.

### Sampled sound on the buzzer: PCM in flash, modulated on the chip — DONE, heard and accepted by the user on white-clock 2026-11-12

The user's idea (2026-11-12): the buzzer line is a 1-bit speaker, so
play sampled music on it. Decided the same day: the clip is stored as
plain PCM and the chip modulates it, rather than a bitstream converted
on the host, because the DMA needs a RAM window either way and PCM is
a quarter of the flash.

**Numbers.** The band is 4 kHz, so the PCM is 8 kHz, 8 bits, unsigned,
mono, raw: 7.8 KB a second, 216 KB for the 27.6 s test clip. In-band
SNR of a 1-bit stream against the music (first 10 s of the test clip,
200 Hz to 3.6 kHz, peak 0.6 of full scale, simulated on the host):

| bit rate | 1st order | 2nd order |
| --- | --- | --- |
| 64 kbit/s | 6 dB | 15 dB |
| 128 kbit/s | 14 dB | 27 dB |
| 256 kbit/s | 22 dB | 35 dB |
| 512 kbit/s | 29 dB | 34 dB (the measurement's floor) |

So a second-order modulator, at 256 kbit/s by default; `CLIP_KBPS` at
build time picks 128, 256 or 512 for the bench.

**Peripheral.** I2S0's transmitter in TDM mode, data line only, 8-bit
stereo at `BIT_RATE / 16` frames a second, so the line carries the
buffer's bytes MSB first at the bit rate (160 MHz / 39.0625 / 16 at
256 kbit/s, exact in the fractional divider; 64 kbit/s is out of the
8-bit divider's reach). GDMA channel 1 loops over a ring of eight
1 KB descriptors (32 ms each at 256 kbit/s). The C3's GDMA reads
internal RAM only, so the clip stays in `.rodata` and the buzzer task
modulates it into the ring a few chunks ahead of the DMA
(`core::clip::Pacer`: the DMA's descriptor register says which slot,
the time since the start says which lap; a stall past the ring is an
underrun, counted and logged, and the audio carries on late).

**Pin.** GPIO5 must never be pushed high. It is made an open-drain
`Output`, released, turned into a signal (which esp-hal freezes, so
no driver touches the pad) with the GPIO matrix's output inverter, and
connected to `I2SO_SD` after the transfer has started on a stream
that begins with zeros: a 1 bit pulls the line low (buzzer on), a 0
releases it. Ending, or a command cutting in, takes the pin back as a
plain released output first and stops the transfer after; that also
clears the inverter, which LEDC's tones must not inherit (the pin's
`func_out_sel_cfg` is written whole by both).

**Modulator** (`core/src/clip.rs`, pure, tested on the host). Second
order, `NTF = (1 - z^-1)^2`, integer, linear interpolation between PCM
samples, integrators clamped. A sample of 255 is 0.7 of the feedback,
the loop's stable range. Mid-scale is a 50 % duty, so the clip is
wrapped in 30 ms ramps from all-zeros and back to keep the step in
the coil's DC out of the ear.

**Build.** `ALARM_CLIP=<file.pcm>` at build time adds the clip as the
last melody (`clip`, label from `ALARM_CLIP_LABEL`, default "Sampled
clip"): on the page, the API, Home Assistant and in the random draw.
Without it there is no entry, no ring (the 8 KB of statics are sized
0) and the releases stay as they are; the test clip is someone else's
music and is not in the repository (`wav/` is ignored).
`scripts/wav2pcm.sh in.wav out.pcm [seconds]` converts with sox
(mono, 300 Hz high-pass, 8 kHz, normalised, dithered to 8 bits).

**Not known until the bench.**

- The PNP has 10 k into its base and nothing to pull the base off, so
  its turn-off may take longer than a bit (3.9 µs at 256 kbit/s). If
  so the dense stream distorts and 128 kbit/s may sound better than
  512; a scope on the collector says. The fallback is PWM-shaped bits
  (one edge pair per carrier period, noise-shaped), which makes the
  edge error a constant.
- The byte and bit order of the 8-bit TDM format is from reading the
  driver, not from a scope; if it is wrong the clip is hiss.
- The buzzer's resonance near 2 kHz will colour everything.
- Whether `out_dscr` names the descriptor being read or the next one;
  the pacer never writes either.

**Verify on a clock.** Build with the clip, pick it on the page and
press Play: the log says `buzzer: preview clip`, then `clip: N chunks,
0 underruns` when it ends; the tune is recognisable, starts and ends
without a thump, and the line is silent after. A press or `Stop`
during the clip silences it at once. A note melody played after the
clip sounds as before (the inverter is gone). An alarm on the clip
loops it with 1.5 s between passes. Try `CLIP_KBPS=128` and `512`.

**Built, 2026-11-12.** `core/src/clip.rs` (`Player`, `Pacer`),
`src/clip.rs` (`Sampler`), the `clip` entry in `melody.rs`,
`scripts/wav2pcm.sh`. Host tests, 101: a full-scale 1 kHz sine comes
back out of the bits with 43, 58 and 73 dB in the band at 128, 256
and 512 kbit/s (a three-pass moving average as the filter; 8-bit
samples carry about 50), the ramps start and end on a released line
with the duty monotonic through them, the integrators stay under half
their clamp on a full-scale square wave, and the pacer never names
the slot being read or the one before it over a thousand chunks.
Sizes against `HEAD`: without a clip the image is 1.4 KB larger and
the main stack 68.4 KB (68.5); with the test clip (221 100 bytes)
`.rodata` grows by 222 KB, `.text` by 6.8 KB and the stack is
60.0 KB. Nothing has been heard yet.

**First hearing, 2026-11-12 (white-clock, over the air).** It played
and the tune was there, but very quiet, and while it played the clock
acted on button presses nobody made. Both point at the per-bit stream:
at 256 kbit/s a mid-scale input is the line toggling every 3.9 µs,
which is no time for the coil to take current and probably none for
the transistor to turn off (10 k into the base, nothing pulling it
off), and it is 128 000 falling edges a second on GPIO5 beside GPIO4,
a button with only the internal pull-up; the debounce wants four
samples 10 ms apart, so the line must have read low a good part of the
time. Whether the coupling is through the board or the supply is not
known.

**Second version, the same day: pulse-width instead of sigma-delta.**
The bits are now one pulse per carrier period, centred in it, as long
as the level at mid-period asks for; `CLIP_CARRIER_KHZ` (8, 16 or 32,
default 16) and `CLIP_KBPS` (default now 512) give
`clip::PERIOD_BITS + 1` pulse lengths, 33 by default, up to 62 µs
long, with 16 000 falling edges a second. The rounding error is fed
back at the order the carrier leaves room for (`ORDER`: second at
32 kHz, first at 16, none at 8, where shaping would put it back in
the band). A pulse grown from the start of the period measured 14 dB
at 16 kHz, its centre moving with the level; centred, the full-scale
1 kHz sine comes back at 34.5 dB by default (19 to 25 at 8 kHz, 28 to
34 at 16, 37 to 42 at 32). Full scale went from 0.7 to 0.95 of the
rail, which a multi-level loop holds. The converter now pushes the
level 12 dB into sox's limiter after the first normalise: the test
clip's RMS went from 0.134 to 0.311 of full scale, 7 dB. `src/clip.rs`
is unchanged. Flashed to white-clock over the air; not yet heard.

**To hear.** Louder than the first version; no phantom presses while
it plays; a note melody after it sounds as before. If it is still
quiet, `CLIP_CARRIER_KHZ=8` doubles the pulses (and may whine at
8 kHz); if the buttons still fire, the cure is on the board (a
pull-up of a few kΩ on the button pins) or a longer debounce while a
clip plays.

**Second hearing, 2026-11-12.** Louder than the first version, still
quiet, and the buttons still fired, so the number of edges was not
the whole story. The button poll is 10 ms, 160 periods of the 16 kHz
carrier on the same crystal: one read per poll lands on the same
point of every period, and a glitch there is read four polls running.
`buttons::down` now takes a pin as down only when it reads low ten
times over 126 µs, a whole period of the slowest carrier. Flashed to
white-clock with `CLIP_CARRIER_KHZ=8` (pulses to 125 µs, 65 lengths,
no shaping); neither the loudness nor the buttons heard back yet. The
aliasing is a hypothesis; if presses still appear the coupling holds
the line low for a whole period and the cure is a pull-up of a few
kΩ on the button pins.

**Third hearing, 2026-11-12.** The 8 kHz carrier is an audible whine
under the music (the user), so 16 kHz stays the default and 8 is not
worth offering as a way to more volume. White-clock is back on 16 kHz,
with the button change.
The converter's limiter drive went from 12 to 20 dB for more volume:
the test clip's RMS is 0.467 of full scale (0.311 at 12 dB, 0.134
before any), flashed to white-clock; not yet heard.

**Fourth hearing and the diode, 2026-11-12.** The user fitted a
1N4148 across the buzzer (the board has no flyback diode) and heard no
difference, so the coil's current was not what limited the volume; by
arithmetic the clip's drive was already within about 6 dB of a
square-wave beep. What is left is the buzzer: a resonator near 2 kHz,
weak under 1 kHz, where the music has most of its energy. The
converter now cuts below 900 Hz at 24 dB an octave before the limiter
(it was 300 Hz at 12), so the line's range goes to the band the buzzer
plays. Flashed to white-clock; not yet heard. Whether the phantom
presses are gone, by the button change or the diode, is not yet
reported either.

**Fifth hearing, 2026-11-12.** The 900 Hz cut changed nothing the
user could hear, and the clip is "much quieter than the beeps".
Phantom presses are still there, fewer. Measured from the host: the
clip played three times by `POST /play` with `/api` polled twice a
second; in the second pass the colour stepped twice within half a
second (orange, yellow, lime), nothing else moved. So it is the top
button, GPIO4, the pin next to GPIO5, taken as down twice, each time
by ten reads over 126 µs at polls 30 ms apart: the line is really low
for stretches, not glitching. The alarm had also gone from 07:40 to
06:40 since the first flash, by whom is not known. Three explanations
for the volume have now failed (edge density, the coil's current, the
band); the next step is a scope on GPIO5, the collector and GPIO4
while the clip plays, or `wav/test_tones.pcm` (six full-scale sines,
500 Hz to 3 kHz) through the same path against the beeps.

**On the scope, and the buttons explained, 2026-11-12.** The user
scoped the buzzer while the clip played: 16 kHz pulses to the full
5 V, about as wide at the buzzer as on GPIO5 (DIP-16 pin 11), so the
transistor neither stretches nor flattens them; with no diode fitted
the coil kicks to −10…−15 V at turn-off, a sharp spike. The drive is
therefore right and the volume is the buzzer's: much quieter than the
beeps, which are a square wave on its resonance. The phantom presses
were not interference: pins 9 and 10 of the DIP-16 (the two buttons)
are shorted on this adapter board. The ten-read button check built on
the aliasing hypothesis is reverted, `src/buttons.rs` is as it was;
both of my explanations for the presses (edge coupling, then the poll
aliasing with the carrier) were wrong. White-clock still runs the
build with the ten-read check until the next flash.

**After the adapter's rework, 2026-11-12.** The short between DIP-16
pins 9 and 10 was under the ESP32-C3; the user lifted the module,
cleaned it and soldered it back. White-clock (10.1.1.58, serial on
ttyACM1) was updated over the air with the plain button read and
watched from the host through `/api` twice a second: 30 s idle, three
whole plays of the clip and one of Twinkle, and no setting moved. The
serial log has `buzzer: preview clip` and `clip: 1733 chunks, 0
underruns` for each of the three plays (27.7 s each), the first time
that line has been read; heap free 54 080 at both heartbeats, MQTT
connected, no heal. So no phantom presses with the short gone. Not
checked from the host: that each button still does its own thing when
pressed. The record was left as found, which is alarm 06:40, armed,
weekdays, random; at the first flash of the day it was 07:40 and
disarmed, so some of that may be the short's doing.

### Beeper clips: a ZX Spectrum tune as it was played — DONE, heard and accepted by the user on white-clock 2026-11-12

The PCM clip works but is too quiet and does not sound good (the
user, 2026-11-12): a resonant buzzer is no speaker. The user's next
idea is music written for a 1-bit speaker: the title tune of the ZX
Spectrum game Trantor, which the beeper plays, from a `.tap` put in
`tap/` (ignored by git, like `wav/`: someone else's music, not for
the repository or a release). Either grab the music or re-implement
the player; grabbing it is the one done.

**The tune.** The tape is a BASIC loader, a screen with a 30-byte stub
behind it and one code block that the stub moves down by 0x600 and
jumps into at 0x5B00. The music starts at once. The player (around
0xA980 to 0xAB60 as it runs: two tone channels as interleaved pulses,
drums out of memory bytes, effects by self-modified operands) was
read in ucsim's disassembly and not ported: its data format would
have been days. Run in SkoolKit's simulator (`pipx install skoolkit`)
the speaker bit flips 865 times a second, in pulses of 47 µs at the
median, 3.6 % of the time on; a pass is 141.5 s (silences of 0.48 s
at 15.3 s and 141.8 s) and 59 541 pulses.

**The clip.** `scripts/zxbeeper.py tape out.bpr --seconds N [--skip N]
[--stretch F]` loads and runs the tape in SkoolKit and writes the
flips as run lengths: `BPR1`, the total in ticks, then LEB128 counts
of 1/128 000 s, off and on alternately (`clip::Beeper`). 150 KB for
the whole pass, each edge within a tick of the Spectrum's. `--stretch`
widens every pulse, never into the next: at 4 the line is on 9.8 % of
the time.

**Playing it.** `clip::Stream` is either kind of clip as the line's
bits, picked by the magic, so `ALARM_CLIP` takes either file and
`src/clip.rs` plays both through the same ring; a beeper clip needs
no modulator, each tick is `BIT_RATE / 128 000` bits. `melody::CLIP_PCM`
became `CLIP_DATA`. Host tests, 102: runs of one, two and three bytes,
empty runs and the zeros after the end come out bit for bit in uneven
pieces. With the stretched clip `.rodata` is 254 KB and the stack
60.0 KB.

**To hear.** `ALARM_CLIP=tap/trantor-x4.bpr ALARM_CLIP_LABEL=Trantor`:
the tune as the Spectrum plays it, louder than the PCM clip was, the
line silent after; `clip: N chunks, 0 underruns` in the log. Then the
same with `tap/trantor-x1.bpr`, the pulses as played, to compare.

### Two-channel tunes: the "Wham! The Music Box" player, re-implemented — DONE, heard and accepted by the user on white-clock 2026-11-12

The Trantor clip played, as recorded and with the pulses widened four
times, and the user did not like either on the buzzer: a masterpiece
of beeper music, and no good here. Next (the user, 2026-11-12): the
tunes of `WHAM.TAP`, Mark Alexander's music editor of 1985, in three
steps: how it plays, where the tunes are, then into the clock.

**How it plays** (read from the disassembly, 0xCE57 to 0xCFE2 of the
block loaded at 50000, and checked in SkoolKit). A tune is two
channels of note bytes, 1000 each, at 60000 and 61000; the byte before
channel 1 is the tempo if above 220 (else the program's 239 stays);
0x40 ends a channel. A step takes the next byte of each, adds 12 and
looks up a period in a table of 53 semitones (255 down to 12; entry
53 is 1, the rest). Tones: a loop of 96 T-states run `(256 - tempo) *
256` times, which writes channel 1's level to the speaker, then 48
T-states on channel 2's, and flips each level every `period` passes,
so the two square waves share the line by time at 36 kHz; code 24 is
3.5 MHz / (96 * 2 * 64) = 285 Hz. Both resting: a silent loop,
`(255 - tempo) * 256` passes of 94. Channel 1 from 0x80 after the
add is a drum for the whole step: 0xFF noise, 0xC0 a falling sweep
through the ROM's beeper (cycle pairs of half period 130 + 1020 k
T-states, the count from a table by tempo), anything else four
quarters of noise or silence by its low nibble; the noise is bits of
the ROM from 0x03E9, sampled where channel 2's byte, rotated a bit a
pass, has a one. 1164 T-states go between steps on the fetch and a
keyboard scan. A Python model of this matched the emulator flip for
flip on tone steps and to tens of T-states a step on the rest.

**Where the tunes are.** The tape's block at 30000 is the program's
memory store, 2000 bytes a tune; names at 65300: Freedom (312 steps,
37 s), Tropicana (280, 30 s), Young Guns (544, 50 s), Whisper (704,
84 s), Bad Boys (256, 31 s). Whisper uses the quarter drum 0xF8, Bad
Boys the sweep. They are arrangements of the pop group's songs, so
like the clips they stay out of the repository: `scripts/wham2clip.py
WHAM.TAP [n out.whm] [--octave N] [--tempo N]` writes one as a
`WHM1` file of 8 bytes and two per step (632 bytes for Freedom).

**In the clock.** `clip::Wham` is a new player after the original's
behaviour, a `RunSource` (level, bits) written out by `clip::Runs`,
which the beeper clip now shares; `Stream` picks it by the magic, so
`ALARM_CLIP` takes it and `src/clip.rs` is untouched. Its timing is
kept in T-states and turned into bits with the fraction carried.
Not the original's: a channel that rests beside a playing one is held
low (the original flips it every pass, 18 kHz), the noise is a shift
register and not the ROM, and `octave` in the header shifts the
periods right, for a buzzer with no bass. Host tests, 105: pitch and
duty of a tone, the octave, rests, the three drums, a tune cut short.
`CLIP_DUMP=file cargo host-test dump_the_clip -- --ignored` writes a
built-in clip's bitstream; against the emulator's output Freedom and
Bad Boys line up over 10 and 20 s with under 0.4 ms of drift, the
sweep drum included. The image is 2.5 KB larger for the player.

**Owed.** One clip a build: `ALARM_CLIP` names one file. Several
tunes as several melodies needs a list (`core/build.rs` generating
the table); to be done when the user has heard one and wants them.

**To hear.** `ALARM_CLIP=tap/freedom-o1.whm ALARM_CLIP_LABEL=Freedom`:
the tune an octave up, two voices, as loud as the note melodies or
near; `tap/freedom-o0.whm` and `-o2` for the other octaves.

**First hearing and all five tunes, 2026-11-12.** Freedom played, an
octave up, and the user wants all five in, with care for the tempo:
Freedom is too fast.

*Tempo.* Four of the five slots begin with a tempo the program takes
(above 220): Tropicana 241, Young Guns 243, Whisper 239, Bad Boys 239.
Freedom's first byte is 62, which the program ignores, so there it
plays at whatever the tempo cell holds, 239 after loading (read back
in the emulator at the menu), and that is what was flashed: 120 ms a
step, the same as on a Spectrum, and too fast to the ear. The tape has
no better number (the second copy of it the user has is byte for byte
the same). Its bars are 16 steps; taken as eighth notes at the
record's 134 beats a minute a step is 224 ms, which is tempo 224, and
that is what `tap/freedom.whm` is now built with (`--tempo 224`). It
is an estimate from the song, not something found in the program.

*Several clips.* `ALARM_CLIP` is now a list of files separated by
commas. `core/build.rs` writes `clips.rs` into `OUT_DIR` (`CLIP_COUNT`,
`CLIP_NAMES`, `CLIP_DATA`) and `melody.rs` includes it and appends one
melody a clip to the nine tunes in a `const` block, which also refuses
a clip under a second, over 1 MB, or with a name already taken. A
clip's API name is its file's name, letters only (`young_guns`), its
label the same in words (`Young Guns`) unless `ALARM_CLIP_LABEL` gives
one at the same place in its own list. `Melody::sampled` became
`clip: Option<usize>`, and `cfg(alarm_clip)` is gone. With the five
tunes the settings page's worst case is 5767 of 6016 bytes and the
discovery payload 4230 of 4608; the five are 4.2 KB of flash.

Flashed to white-clock: Freedom, Tropicana, Young Guns, Whisper and
Bad Boys after Jingle Bells, all an octave up. Not yet heard.

**Sigma-delta again, on a changed circuit, 2026-11-12.** The user put
the flyback diode back across the buzzer and lowered R5, the base
resistor, from 10 k to 3.2 k (three times the base current), and
asked for Trantor and the sigma-delta Futurama back to hear them on
it. `CLIP_MODULATOR=sd` at build time brings back the first version's
modulator (`Player::fill_bits`: second order, a decision a bit, full
scale 0.7; 43, 58 and 73 dB on the test sine at 128, 256 and
512 kbit/s, as on the day); `pwm`, the pulse a period, stays the
default. Flashed to white-clock with `CLIP_KBPS=256`, as the first
hearing was, and seven clips: the five tunes, `tap/trantor.bpr` (the
pulses as played) and `wav/futurama.pcm`, converted as on the first
day (300 Hz high-pass, normalised, no limiter;
`wav/futurama_theme.pcm` stays the 900 Hz, 20 dB version). At
256 kbit/s a tick of the beeper clip is two bits and a slot of the
two-channel player 3.5. Not yet heard.

**Skips under Wi-Fi activity; the clip gets its own task, 2026-11-12.**
The user heard skips while the Wi-Fi was busy. The ring is 8 KB,
eight chunks of 1 KB: 256 ms at 256 kbit/s and 128 ms at 512, of
which six chunks, about 190 and 95 ms, are ahead of the DMA. A skip
is then the buzzer task not run for that long; it sat on the thread
executor with the network stack, under the Wi-Fi driver's thread. No
underrun count was read (the clock is not on USB), so this is the
likely cause and not a measured one.

Of the three cures offered (a larger ring, the count on `/api`, the
refill off the thread executor) the user chose the third first. The
buzzer task cannot move as it is: LEDC's driver holds a reference to
its register block and is not `Send`, which the interrupt executor's
spawner demands. So the clip has a task of its own, `clip::clip_task`,
on the scan's interrupt executor (priority 2), owning I2S0, the DMA
channel and the ring; `clip::play(index, cancel)` is what the buzzer
task calls, three `Signal`s between them (start, stop, done). The pin
is shared by turns: `main.rs` makes a second handle with
`clone_unchecked` (the one `unsafe` there) and the clip task uses it
only from a `play` to its end, while the buzzer task waits in that
call. The slot pointer in `Sampler::play` is kept as an address, since
a raw pointer across an await is not `Send` either. The seven chunks
written before the start now have 500 µs between them, each being a
millisecond or two of work above the threads.

What it costs: a chunk's worth of modulating, a millisecond or two,
now runs at interrupt priority 2 every 16 to 32 ms, above the Wi-Fi
driver's interrupts as well as its thread. The driver's own critical
sections are of that order, so it should not notice; if the probe or
the association suffers while a clip plays, this is where to look.
Flashed to white-clock with the eight clips, sigma-delta, 256 kbit/s.
Not heard, and no log read.

**Crackling tunes, and skips that stayed, 2026-11-12.** With the diode
back and R5 at 3.2 k the user finds Futurama (sigma-delta, 256 kbit/s)
and Trantor fine. The five two-channel tunes crackled in that build:
at 256 kbit/s a channel's 48 T-state slot is three and a half bits, so
the slots came out three and four bits by turns. The player now has
its own bit rate, `clip::WHAM_BIT_RATE`, 512 kbit/s whatever
`CLIP_KBPS` says, with every slot exactly seven bits (0.3 % short of
the original's, five cents sharp); `Stream::bit_rate` tells
`src/clip.rs`, which sets I2S and paces by it per clip (`chunk_us` and
`chunks_in` take the rate).

The skips stayed after the clip moved to the interrupt executor, on
Trantor and Futurama alike, so starving on the thread executor was not
the whole of it, if any. Two changes, neither a diagnosis: the ring is
16 chunks (`SLOTS`; 16 KB, the main stack 51.5 KB with clips), which
is 450 ms ahead of the DMA at 256 kbit/s and 220 at 512; and the pacer
no longer believes a DMA position more than two chunks from where the
clock puts it (`Pacer::STRAY`; the two clocks are one crystal's), since
acting on one bad read of the descriptor register would write over
chunks not yet played or skip ones due, which is a skip. Such reads
are counted and the clip's last log line has them: `clip: N chunks,
U underruns, S stray positions`. Whether either count is ever above
nought is what a serial log of a skipping clip will say; none has been
read. Flashed to white-clock.

**The skips, the crackle and the overlap were one bug: the DMA's
descriptor register reads two ahead — found and fixed 2026-11-12.**
With the clock on USB again the log of a skipping clip said `0
underruns` and 49 stray positions for Futurama, 971 for 25 s of
Trantor: the ring never ran dry, and the register and the clock did
not agree. A trace of both at the start of three clips settled it:
`out_dscr` of the GDMA channel reads slot 2 the moment the transfer
starts and the clock's chunk plus two from then on, the boundaries
within a millisecond of each other. It names the descriptor two on
from the one being read, not that one.

The pacer had taken it for the chunk being read since the first
version. So it thought the DMA two chunks further than it was and
wrote the next pieces of the stream up to and over the chunk being
played: each chunk was replaced, as the DMA entered it, by the one a
lap later. What came out was the stream a lap ahead with a tear at
every chunk, and the first lap played partly as written and partly as
its replacement. That is the user's "overlapping with itself at the
beginning", and before it the skips under Wi-Fi activity, the
crackling tunes at least in part, and probably some of what made the
PCM clip sound poor from its first hearing; none of it was starvation,
and the underrun count was honestly nought throughout. Yesterday's
guard (not believing a slot more than two from the clock) put the
pacing on the clock whenever the two parted by three, which is why it
"sometimes fixed itself".

Now the clock paces alone (`Pacer::played` returns the estimate; the
line's bit clock and the timer are one crystal's), and the register is
only checked against it: a slot not `Pacer::DSCR_AHEAD` (2) on from
the clock's chunk, give or take one, is a stray, counted and logged.
Host tests, 106: the playback loop of `src/clip.rs` against a model of
the DMA, the register anywhere and the clock up to a tick late, feeds
it the stream in order; a short stall is not heard; a long one leaves
the stream late and whole a lap after. On white-clock, serial log:
Futurama whole with the page fetched six times meanwhile, 20 s of
Trantor, Bad Boys whole: `0 underruns, 0 stray positions` each.
Not yet heard by the user.

Left as they are, though neither was the cure: the clip task on the
interrupt executor and the 16-chunk ring (8 KB more of static RAM).
And to look at: `ring::position`, the scan's heartbeat figure, reads
the same register of channel 0 and calls it the descriptor being
read; by this it is two on. Nothing depends on it but the log line.

**The set the user settled on, 2026-11-12,** after hearing the tunes
with the pacer fixed: Young Guns and the quiet Futurama go, and four
tempos change by ear. The build, sigma-delta at 256 kbit/s for the
PCM clip:

```
scripts/wham2clip.py tap/WHAM.TAP 1 tap/freedom.whm   --octave 1 --tempo 228   # was 224 (my estimate): a bit faster
scripts/wham2clip.py tap/WHAM.TAP 2 tap/tropicana.whm --octave 1 --tempo 238   # the tape's 241: slower
scripts/wham2clip.py tap/WHAM.TAP 4 tap/whisper.whm   --octave 1 --tempo 237   # the tape's 239: a bit slower
scripts/wham2clip.py tap/WHAM.TAP 5 tap/bad_boys.whm  --octave 1 --tempo 238   # the tape's 239: a little slower
CLIP_MODULATOR=sd CLIP_KBPS=256 \
ALARM_CLIP=tap/freedom.whm,tap/tropicana.whm,tap/whisper.whm,tap/bad_boys.whm,tap/trantor.bpr,wav/futurama.pcm \
  cargo build --release
```

A step is `(256 - tempo)` times 7.0 ms: 197, 126, 133 and 126 ms.
`wav/futurama.pcm` is now the loud conversion (900 Hz cut, 20 dB into
the limiter; what `scripts/wav2pcm.sh` writes) and the first day's is
`wav/futurama_quiet.pcm`. Flashed to white-clock; the tempos not yet
heard.

**Accepted, 2026-11-12.** With the set and tempos above on white-clock
(the flyback diode fitted, R5 at 3.2 k) the user's verdict is "all
good now".

**The two-channel player removed, Trantor kept in the repository,
2026-11-12.** The user on the Wham tunes, once everything played
right: the player works, the arrangements are not to his taste. So
`clip::Wham`, its tests, `Stream::bit_rate` and `scripts/wham2clip.py`
are gone (the section above stays as the record of how that program
plays, and commit b6ff2c6 has the code); `Runs` and `RunSource` stay,
with the beeper clip their one user. 103 host tests.

Asked whether Futurama and Trantor could go into the repository, the
answer given was no for both: a recording of a television theme, and
a note-for-note capture of a game's title music, neither licensed.
The user's decision: Futurama stays out (`wav/` is ignored), Trantor
goes in with its author named. It is `clips/trantor.bpr`, the pulses
as played, with `clips/README.md` saying that the music is David
Whittaker's, that copyright stays with him and the game's publishers,
and that the file is not under the project's licence. No build takes
it in unless `ALARM_CLIP` names it, so the releases do not carry it.
The set on the two clocks is now built with
`ALARM_CLIP=clips/trantor.bpr,wav/futurama.pcm` and, for Futurama,
`CLIP_MODULATOR=sd CLIP_KBPS=256`; white-clock and martin-clock still
run the earlier set with the four tunes until they are flashed again.

**Chronos, 2026-11-12.** The user's next tape: the title music of
Chronos, Tim Follin's, to be credited as Trantor's is. The tape is a
cracked copy whose BASIC runs `USR 24983` (an unpacker), then `PAUSE
0`, then `USR 64000`, so run by itself it sits in the ROM waiting for
a key and plays nothing. `scripts/zxbeeper.py` got `--settle` and
`--start` for that: run the loaded tape for a while, then jump to the
address the loader would. From 64000 the tune starts at once and
plays once, 184.3 s: 203 022 pulses, 85 µs at the median and on
10.8 % of the time (Trantor's are 47 µs and 3.6 %), each edge within
a tick of the emulator's. The clip is 472 KB, `clips/chronos.bpr`,
with its paragraph in `clips/README.md`. Trantor converted again with
the changed script is byte for byte the file in the repository. Not
yet flashed or heard, and not committed.

**The buzzer mod in the README, 2026-11-12.** The user's two changes
to the buzzer circuit are now documented as an optional mod: a 1N4148
across the buzzer as a flyback diode, and R5 at 5.1 k instead of the
kit's 10 k (the hearings above were at 3.2 k; 5.1 k is the value the
user gives for the README). The user's account of what they do: the
diode removes the negative peak at turn-off and improves the sound;
the lower R5 raises the base current, so the collector's signal, and
the buzzer's, is squarer (with 10 k the falling edge is a slope) and
louder. This supersedes the fourth hearing's "no difference" for the
diode alone. In `README.md` (the parts list, the R5 row, a paragraph
under "Building the clock board") and in `AGENTS.md` under the buzzer.
The diode's orientation in the README is derived from the circuit
(emitter at +5 V, buzzer from the collector to ground, the kick
negative), not read off the user's board.


**Chronos did not sound right; a finer tick, 2026-11-12.** Flashed
with Trantor and Futurama, Chronos was wrong to the user's ear where
Trantor is fine. The captured pulses say why, most likely: Chronos's
commonest widths are 242, 256, 270, 298, 326, 340, 354 T-states, a
ladder of 14 (4 µs), which is how Follin's player shapes loudness and
tone, and the clip's tick was 1/128 000 s, 27 T-states. Each edge
rounded to that grid by itself, so a width that should have stepped
smoothly came out a tick more or less from one pulse to the next, a
tenth of a typical pulse. Trantor's widths are few and far apart (59,
118, ...), which is why it did not show.

The format is now `BPR2`: the same, at 1/512 000 s (6.8 T-states), a
bit a tick, and a beeper clip runs the line at its own
`clip::BEEPER_BIT_RATE`, 512 kbit/s whatever `CLIP_KBPS` says
(`Stream::bit_rate` is back for it). A `BPR1` file would be taken for
PCM, so `melody.rs` refuses one at build time. Both clips converted
again: Trantor 155 140 bytes (149 966), Chronos 572 420 (472 159).
With Futurama the image is 1 806 096 bytes, 92 % of the slot. Flashed
to white-clock. That the tick was the fault is an inference from the
numbers; the user's ear decides.

**Trantor ended on its own beginning, 2026-11-12** (the user heard
it). The clip was cut at 141.5 s, at the second of the two 0.48 s
pauses, on the reading that a pass ran from pause to pause. It does
not: the game starts the tune over at 126.5 s, and the pause at
141.8 s is the one at 15.3 s come round again (the opening's flips
per 10 ms match the capture best at 126.5 to 126.6 s). The clip is
now cut there, `--seconds 126.5`: 141 KB.

**Chronos as PCM, 2026-11-12.** The finer tick did not win the user
over ("I don't like it"), and instead of the beeper clip he recorded
the title tune from the Xpeccy emulator, `wav/chronos_title_xpeccy.wav`
(184.6 s, mono, 44.1 kHz), to be given Futurama's treatment with its
first 4 s cut. `scripts/wav2pcm.sh` takes a fourth argument for that,
seconds to skip, and fades the last second and a half of a clip it
cuts short; Futurama through the changed script differs from the file
in use by the dither alone (2 of 255 at most).

The whole tune from 4 s on is 180.6 s, 1.44 MB of PCM, and does not
fit: the image without clips is 857 KB of a 1920 KB slot. What is
flashed is the first 90 s of it, `wav/chronos.pcm`, 720 KB, beside
Trantor and Futurama: 1 944 048 bytes, 98.9 % of the slot, 22 KB to
spare. The recording has no pause anywhere to cut at, so the 90 s is
the room there was, not a musical choice. `clips/chronos.bpr` is no
longer built in; it and the whole 180 s as PCM (`wav/chronos_full.pcm`)
are kept on disk. Not yet heard.

**Chronos dropped, 2026-11-12.** As PCM from the user's own recording
it "sounds bad" too, so it is out in both forms: `clips/chronos.bpr`
and the two PCM files are deleted (the recording stays in `wav/`), and
`clips/README.md` is back to Trantor alone. Whatever makes that tune
is more than this buzzer gives back. What the attempt leaves behind
is kept: the beeper clip's finer tick (`BPR2`), `--settle` and
`--start` in `scripts/zxbeeper.py`, and the skip and the fade in
`scripts/wav2pcm.sh`. The clocks' set is Trantor and Futurama:
`ALARM_CLIP=clips/trantor.bpr,wav/futurama.pcm` with
`CLIP_MODULATOR=sd CLIP_KBPS=256`.

### Tracker modules (XM) — first trial as a rendered clip 2026-11-12; a player on the chip only planned

The user's next source of tunes: tracker modules, XM rather than MOD,
with one to start from in `mods/`: "halftime" by Orion, a keygen
chiptune (FastTracker 2, 4 channels, 7 patterns, 7 samples of 8 bits
and 4.5 KB in all, 12 642 bytes, 43.5 s).

**Trial, no new firmware code.** Rendered on the host with
`openmpt123 --render --samplerate 44100 --channels 1`, then
`scripts/wav2pcm.sh` as for Futurama: `wav/halftime.pcm`, 349 KB.
Flashed to white-clock beside Trantor and Futurama. Of the render's
energy 76 % is under 300 Hz and 91 % under 900 Hz, which the
converter cuts, so what the buzzer gets is the top of the tune. Not
yet heard.

**If it is liked: play the module itself.** The file is 12.6 KB
against 349 KB of PCM, 28 times less, and the ratio grows with the
tune's length. A player on the chip would mix the channels at 8 kHz
into the same modulator and ring the PCM clip uses, as a third kind
behind `clip::Stream`. Two ways to it, to be chosen: the `xmrsplayer`
crate (no_std, plays XM, MOD, S3M and IT; to be checked for what it
allocates and costs in flash), or a player of our own for the subset
these chiptunes use (notes, the volume column, a dozen effects,
volume envelopes), which would stay within the project's no-`alloc`
rule and be tested in `core/` against openmpt's render.

**The module played on the chip — built 2026-11-12, flashed, not yet
heard.** The rendered trial was liked ("I like it :)"), so the player.

*Not the crate.* `xmrsplayer` 0.15 needs `alloc` (it unpacks a module
into heap structures; its own `lib.rs` and `xmrs`'s say `extern crate
alloc`), and the two are 22 000 lines with serde underneath. The clock
has no heap to give it but the Wi-Fi driver's.

*`core/src/xm.rs`.* A player for what chiptunes use of the format,
written for the clock: the module read in place, eight channels at
most, no allocation. The test module's inventory decided the scope:
effects 0 3 4 6 A B C D and the volume column's set-volume, envelopes
with sustain and loop, forward loops, the linear frequency table. It
plays those and their neighbours (1 2 5 9, E1 E2 E9 EA EB EC ED, F, G,
K, the column's slides, vibrato and tone portamento), FastTracker's
arpeggio tick order included; not tremolo, pattern loop and delay,
multi-retrigger, auto-vibrato, the Amiga table, or more than one
sample an instrument. Channels are mixed at 32 kHz with linear
interpolation and averaged to 8 kHz; then, on the chip, what
`scripts/wav2pcm.sh` does to a PCM clip on the host: two second-order
high-passes at 900 Hz and a limiter with 20 dB of drive (instant
attack, 16 ms release). The samples go to the clip's modulator as PCM
does: `clip::Player` takes its body from PCM or from a `Tune`.

*`scripts/xm2clip.py`.* The file the clock holds is not the XM but
`XMC1`: tables of where each pattern and sample is, the patterns as
packed, samples as plain bytes (XM's are differences), ping-pong loops
unrolled, the tune's length worked out; it lists what the module uses
that the player lacks. "halftime" is 10 843 bytes (the module 12 642,
its PCM 348 960) with nothing unplayed.

*Checked.* 110 host tests, seven of them the player's on modules built
in the test: pitch, relative note and finetune, volumes, each pitch
effect's periods tick by tick, orders, jumps, breaks, speed and tempo,
note delay, the envelope with key-off and fade, the shaping.
`CLIP_PCM_DUMP=file cargo host-test dump_the_tune -- --ignored` writes
what a built-in module gives the modulator; against the liked render
(openmpt, then the host's conversion) it has the same length and
timing (frame energies best at no lag), a median spectral cosine of
0.89 frame by frame, 0.94 between the average spectra, and the same
RMS, 0.42 of full scale. It is not sample for sample openmpt: no
volume ramping, linear and not sinc interpolation, a limiter of our
own. Image 6.5 KB larger; main stack 50.7 KB with clips.

Flashed to white-clock as `halftime` from `mods/halftime.xmc`, beside
Trantor and Futurama. Not known: what a chunk's mixing costs at
interrupt priority on the chip (no serial log this time).

**The module a bit louder, 2026-11-12** (the user's ask after hearing
it). The limiter's release went from 16 ms to 4 and it aims peaks a
quarter over full scale, the excess clipped (`xm::DRIVE`): the test
module's RMS is 0.59 of full scale, from 0.42, 3 dB, with 16.5 % of
the samples at the rails. Flashed to white-clock; not yet heard.

**Two loudnesses side by side, and a second module, 2026-11-12.** To
compare by ear in one firmware, the limiter's drive and release are
now per clip: bytes 10 and 11 of the `XMC1` header, which were spare
(0 keeps the player's `DRIVE` and `RELEASE_SHIFT`), set by
`scripts/xm2clip.py --drive N --release N`. Halftime is built in
twice: `halftime_a` at 127 and 7 (RMS 0.42, the level of the render
first liked) and `halftime_b` at 160 and 5 (0.59).

The second module in `mods/` is "keygentune2003 orn", also Orion's:
4 channels, 57.6 s, 15 235 bytes converted. It uses an instrument's
auto-vibrato, which the player did not have and now does: a sine that
grows to its depth over the sweep, added to the period every tick
(sweep, depth and rate in three of the instrument entry's spare
bytes; other waveforms are played as a sine and the converter says
so). Against openmpt's render through the host's conversion the new
tune has the same length and timing, a median spectral cosine of 0.90
and 0.975 between the average spectra; it is built at the player's
default, the louder setting.

Flashed to white-clock: Trantor, Futurama, Halftime A, Halftime B,
Keygen Tune. 110 host tests. Not yet heard.

**Halftime settled; the second tune's bass, 2026-11-12.** The user
keeps Halftime B, the louder: it is the player's default, so the clip
is `mods/halftime.xmc` again, built with no options.

The second tune came out wrong to the ear: the intro incorrect, the
channels' balance off, the bass not there at all. The player is not
at fault: its mix before the shaping (`CLIP_RAW=1` with
`CLIP_PCM_DUMP`, a new `xm::raw_mix` for tests) is openmpt's within
1 dB in every band for the first sixteen seconds, spectral cosine
1.00, bar a droop of 1 to 2.5 dB above 2 kHz from the linear
interpolation. The shaping is: this tune's bass instrument (1) plays
44 to 148 Hz and carries the intro, its lead (2) 349 to 879 Hz, and
36 % of the tune's energy is under 150 Hz, all of it below the 900 Hz
cut that suited Halftime.

So the cut is the clip's to say now. The header grew to 32 bytes
(`XMC2`; an `XMC1` file is refused at build time): how many passes of
the high-pass, 0 to 2, and its section's five coefficients, which
`scripts/xm2clip.py --highpass HZ --passes N` works out (the chip has
no tangent; 900 Hz twice remains the default, and its section is to
the digit what scipy gives). And `--raise INSTRUMENT:OCTAVES` adds
octaves to an instrument's relative note in the clip, with no code
on the chip: the one way a 100 Hz bass line is heard on this buzzer.

Three of the tune side by side on white-clock for the user's ear:
`keygen_a` as before (900 Hz, 24 dB an octave), `keygen_b` at 300 Hz
and 12 dB, `keygen_c` the same with the bass two octaves up (176 to
592 Hz). Not yet heard.

**300 Hz, steep, 2026-11-12** (the user's ask after hearing A to C).
Added beside the others on white-clock: `keygen_d` at 300 Hz and
24 dB an octave, `keygen_e` the same with the bass two octaves up,
and `halftime_low`, Halftime at 300 Hz and 24 dB to hear again
against the 900 Hz one. Not yet heard.

**The settings chosen by ear, 2026-11-12.** Of three Halftimes and
five Keygens the user keeps the 300 Hz, 12 dB an octave ones with
nothing raised ("Halftime Gentle" and "Keygen B"), at the louder
limiter setting. Those are now `scripts/xm2clip.py`'s defaults, and
the clips are `mods/halftime.xmc` and `mods/keygen.xmc`, built with no
options. White-clock: Trantor, Futurama, Halftime, Keygen.

Against the modules as openmpt renders them, what the clock plays has
the energy under 300 Hz down from 71 % to 13 % (Halftime) and 59 % to
8 % (Keygen), 300 to 900 Hz carrying over half of what is left,
nothing above 4 kHz, and a crest factor of 4 dB where the originals
have 13 to 14: the limiter takes nearly all the dynamics.

**16 kHz for a module, to compare, 2026-11-12.** The user asked
whether a higher sampling rate made sense. For PCM clips no (twice the
flash for a buzzer that tops out early); for modules perhaps, since
they cost nothing more in flash and the 8 kHz mix folds what a square
wave has above 4 kHz back into the band. Built as an option to hear:
byte 17 of the clip's header asks the player for 16 kHz
(`scripts/xm2clip.py --rate 16000`, which also works the high-pass out
for that rate), `xm::Tune` then averages two mixed samples and not
four, and its limiter's release is a shift longer to stay the same
time. The modulator needs no change: it writes `OSR` bits a sample
whatever a sample is, so `Stream::bit_rate` doubles the line's rate
for such a clip, 512 kbit/s in the 256 kbit/s build, and the
sigma-delta's noise stays where it was.

On the host the 16 kHz Halftime has 6 % of its energy between 4 and
8 kHz and, below 4 kHz, is a little nearer openmpt's render with the
same high-pass (spectral cosine 0.947 against 0.936), which is the
aliasing going. White-clock has `halftime` and `halftime_hi`, `keygen`
and `keygen_hi` side by side. Not yet heard.

**16 kHz kept; the bass cut again, 2026-11-12.** The user likes the
16 kHz versions, so that is `scripts/xm2clip.py`'s default now
(`--rate 8000` for the other), and wonders whether 300 Hz cuts too
much at that rate. Four of each tune on white-clock, all 16 kHz and
12 dB an octave: A at 300 Hz (what was chosen at 8 kHz), B at 200,
C at 100, D with no high-pass. More bass in is less of everything
else out, the limiter working on the sum: in Keygen the level of what
lies above 300 Hz is -4.7, -4.8, -5.4 and -8.0 dB for A to D, with
2, 5, 20 and 33 % of the energy under 150 Hz. Not yet heard.

**Settled, 2026-11-12.** A is better: 300 Hz at 12 dB an octave stays,
at 16 kHz. Those are the converter's defaults, so the two clips are
built with no options, and white-clock has Trantor, Futurama,
Halftime and Keygen:
`CLIP_MODULATOR=sd CLIP_KBPS=256
ALARM_CLIP=clips/trantor.bpr,wav/futurama.pcm,mods/halftime.xmc,mods/keygen.xmc`.

**In the repository and in every build, 2026-11-12** (the user's
decision). Orion's two modules go in with their credit, as Trantor
did: the `.xm` files in `mods/` as found, their clips in `clips/`
(`halftime.xmc`, `keygen.xmc`), a section of `clips/README.md` for
them. And Trantor and the two modules are packaged by default:
`core/build.rs` takes `DEFAULT_CLIPS` when `ALARM_CLIP` is not set
(set to nothing, it is a build with none), so the releases carry them
from here on. That the credit is not a licence was said when Trantor
went in, and holds for these and for the releases.

For the default build to be what was listened to, its settings are
the ones white-clock ran: `CLIP_MODULATOR` defaults to `sd` and
`CLIP_KBPS` to 256 (they were `pwm` and 512, from the day the
sigma-delta clip crackled, which was the pacer's doing). `pwm` and
the other rates remain. A module at 16 kHz therefore runs the line at
512 kbit/s, the beeper clip at its own 512.

The default image is 1 036 784 bytes, 53 % of the slot (865 KB with
no clips), the main stack 50.6 KB (67.2 KB with none). Settings page
at its worst 5679 of 6016 bytes, discovery payload 4204 of 4608.
110 host tests in the default build, with no clips, and with `pwm`
at 512 and 256 kbit/s. Futurama stays on the user's clocks only
(`wav/`, ignored). The stock buzzer circuit (no diode, R5 at 10 k)
has not been listened to with these defaults.

**The alarm loops a clip with no gap — built 2026-11-12, flashed, not
yet heard.** Trantor and both modules are written to go round, and
the alarm played each pass as its own clip, 1.5 s apart, through the
ramps and a fresh start of the line. Now a ringing alarm asks for the
clip `looping` (`clip::play(index, looping, cancel)`,
`Stream::new(data, looping)`) and the stream never ends: a beeper
clip starts its runs over at once; a module follows its jump back as
a tracker does (both of Orion's end on `B01`, round to the second
order, past the intro) or, with none, starts over from the order the
header names (the module's restart position, a spare byte of `XMC2`
that `scripts/xm2clip.py` now fills; both clips came out byte for
byte as they were); PCM goes from its last sample to its first with
no ramp between. A press, `Stop` or the alarm's five minutes end it as
before, the line released first. A preview still plays once, and the
note melodies keep their gaps. 111 host tests: three passes of a
beeper clip bit for bit, PCM's levels going round, a module following
its jump and its restart order.

**Which clips loop, 2026-11-12.** Futurama is not written to go round
and should not; Trantor is. `core/build.rs` now writes `CLIP_LOOPS`
beside the clips: true for a beeper clip or a module (the files with
a header, made from music that loops), false for PCM, and
`ALARM_CLIP=file:loop` or `file:once` says otherwise for one file.
The buzzer task asks for the gapless loop only for those; the others
ring as the note melodies do, a pass, `repeat_gap_ms` (1.5 s), a pass.
Flashed to white-clock with the three default clips and Futurama.

**The modules' volume, heard again at 16 kHz, 2026-11-12.** Side by
side on white-clock, both tunes with the limiter as it was first
(peaks at full scale, 16 ms release: RMS 0.42, 2 to 3 % of samples at
the rails) and raised (a quarter over, 4 ms: 0.60, 15 %): the user
takes the first now that the rate is higher. `xm::DRIVE` is 127 and
`RELEASE_SHIFT` 7 again; the clips in `clips/` name neither and so
follow, unchanged as files. `--drive 160 --release 5` is the louder
one for a clip that wants it.

**A PCM clip at 16 kHz, to compare, 2026-11-12.** After the modules,
the user asked for Futurama at the higher rate beside the 8 kHz one.
PCM had no header to say so; now a clip may start with `PCM1` and its
rate (`clip::PCM_MAGIC`; `PCM_HZ` or twice it), and bare PCM is at
`PCM_HZ` as before. The modulator is untouched: the same bits a
sample, the line at twice the rate (512 kbit/s here), as for a module
at 16 kHz. `scripts/wav2pcm.sh` takes the rate as a fifth argument
and writes the header. `wav/futurama_hi.pcm` is 442 KB against 221 KB;
the recording is 22 kHz, so the band from 4 to 8 kHz is really there.
White-clock has both, `futurama` and `futurama_hi`, with the three
default clips. 112 host tests. Not yet heard.

**16 kHz kept for PCM too, 2026-11-12.** "Sounds way better" (the
user, of Futurama Hi against the 8 kHz one). `scripts/wav2pcm.sh`
converts at 16 kHz unless told 8000; `wav/futurama.pcm` is the 16 kHz
clip now and the old one `wav/futurama_8k.pcm`. White-clock: Trantor,
Halftime, Keygen, Futurama.

### Sixteen-bit samples from a module to the modulator — DONE, heard and accepted by the user on white-clock 2026-11-13

**The question.** The user asked whether the I2S transmitter's PDM
mode (the C3 has the PCM-to-PDM converter, esp-hal 1.2.2 a driver for
it, `i2s::pdm`) could replace the software sigma-delta, and pointed
out that it would take 16-bit samples where the clips are 8. The
hardware's lowest line rate is 64 bits a sample (1.024 Mbit/s at
16 kHz, twice today's), and the buzzer's transistor is the doubtful
part at that rate; the sample width does not need the hardware at
all. So the steps, cheapest first, each only if the one before is
heard: (1) wider samples through the software modulator, this entry;
(2) `CLIP_KBPS=512`, the line at the rate PDM would need; (3) the
hardware converter.

**Where the eight bits are.** `clip::Player` works at `FULL` = 65536
and scales an 8-bit sample by `LEVEL`. A PCM clip is 8 bits in flash
and 16 would double it. A module is not: `xm::Tune` mixes in `i32`
and `shape`, after the limiter, rounds to 127 steps. That rounding is
the only place a module loses width, and undoing it costs no flash.
What it can buy: the 8-bit noise is about 50 dB under full scale and
the second-order loop at 32 bits a sample measures 58 dB on the test
sine, so 8 dB on paper, most where the limiter is not working (quiet
passages, fades).

**Design.**
- `Tune::next_sample` gives `i16`: signed, in 256ths of the 8-bit
  step, ±127 × 256 at full scale. A clip that does not ask for width
  gives whole steps (the same division as today, times 256), so its
  bitstream is bit for bit what it was; one that asks gets the
  limiter's quotient at the finer scale (`i64` for the product).
- The clip asks with bit 1 of header byte 17 (bit 0 is the 16 kHz
  rate; the player has always masked it, so an older firmware plays a
  wide clip narrow). `scripts/xm2clip.py --bits 16`; the default
  stays 8 until heard. `XMC2` stays.
- `Player::level` takes a tune's sample as `s * LEVEL / 256`; PCM is
  unchanged.
- Tests: a narrow tune's samples are multiples of 256 and equal the
  wide one's rounded toward zero; the wide one uses the steps
  between; the existing shaping tests read the sample over 256.

**The A/B.** On white-clock, one firmware, the default settings
(sigma-delta, 256 kbit/s, so 512 for these): Trantor, Futurama, then
`Halftime A` (8 bits, the clip in `clips/`), `Halftime B` (16),
`Keygen A`, `Keygen B`. The variant clips go under `target/`.

**To hear.** Whether B differs from A at all: hiss or grit under
quiet notes and note tails is where it would show. If not, the sample
width is not what limits the sound and step 3 loses its second
argument.

**Built, 2026-11-13.** As designed: `xm::STEP` (256) is the 8-bit
step in a tune's samples, `Tune::next_sample` gives `i16`,
`scripts/xm2clip.py --bits 16` sets bit 1 of byte 17, which is the
one byte the B clips differ by from the ones in `clips/`. 112 host
tests; the shaping test now also plays a clip both ways and finds the
narrow samples to be the wide ones rounded toward zero, with more
than half of the wide ones between steps. Flashed to white-clock over
the air with the six melodies above, the build's defaults otherwise.
Not yet heard.

**Heard, 2026-11-13.** The 16-bit clips sound "smoother" (the user).
Not made the default yet: the A and B pairs stay for step 2.

**Step 2: the line at the rate PDM would need, 2026-11-13.** The same
six melodies built with `CLIP_KBPS=512`: 64 bits a sample, so the
16 kHz clips (the four tunes and Futurama) run the line at
1.024 Mbit/s, bits of 0.98 µs, which is the hardware converter's
lowest rate; Trantor stays at its own 512. Nothing in the code
changes. On paper the second-order loop gains 15 dB of in-band noise
for the doubling (73 dB on the 8 kHz test sine at 64 bits a sample),
so B has room to show its width; against that, twice the edges
through the transistor. A chunk is now 8 ms and the ring 128 ms, of
which fourteen chunks, 112 ms, are ahead of the DMA, and the
modulating is twice the work a second, so underruns are the risk:
skips while the Wi-Fi is busy would be that, not the modulator.

**To hear.** Against the 256 build from memory, since the rate is the
build's: whether the tunes are cleaner, the same, or quieter and
duller (the transistor not following); and whether B against A is a
larger difference than it was. Skips or crackle: say so, the serial
log's `clip:` line has the underrun count.

**Heard, 2026-11-13: "B is now MUCH better".** At 1.024 Mbit/s the
16-bit clips stand well clear of the 8-bit ones, as the arithmetic
said they would once the modulator's own noise was out of the way,
and the transistor (white-clock's, with the diode and R5 at 5.1 k)
follows bits of a microsecond. No skips were reported; the serial
log's underrun count has not been read.

**Sixteen bits is the converter's default, 2026-11-13.**
`scripts/xm2clip.py` writes wide clips unless told `--bits 8`, and
`clips/halftime.xmc` and `clips/keygen.xmc` are converted again: one
byte each, the files the B melodies were. The line's rate is not
changed yet: `CLIP_KBPS` still defaults to 256, 512 being heard only
on white-clock's changed circuit and with its cost in processor time
and ring depth unmeasured.

**512 is the default, 2026-11-13.** The user, of the two builds: the
8-bit clips are not much different at 512 than at 256, the 16-bit
ones "way, way better", less noise and less harsh; no skips. So the
8-bit step was the floor at either rate and the modulator's noise was
what the wide clips still had at 256. `CLIP_KBPS` defaults to 512
(1.024 Mbit/s for a 16 kHz clip); 256 and 128 remain. Heard on
white-clock's changed circuit only; the processor's share and the
underrun count are still unread.

### Hardware PDM for a module: a spike to hear — DONE, heard on white-clock 2026-11-13 (128 bits a sample kept, 64 is noise)

**Why now.** Steps 1 and 2 above both came out for it on white-clock:
16-bit samples are heard, and the transistor follows 1.024 Mbit/s.
What I2S0's PCM-to-PDM converter would add to the software loop at
that rate: no modulating on the processor (16 000 samples a second
copied instead of a million decisions), a ring of PCM that is four
times as deep in time (16 KB is 512 ms at 16 kHz, 16 bits), the
converter's own interpolation instead of the linear one, and rates
the software cannot afford, 2.048 Mbit/s first. What it would cost if
kept: a second path through `src/clip.rs` (the beeper clips are bits
and stay on TDM), and a modulator no host test can see.

**What is known and what is not.** esp-hal 1.2.2 has the driver
(`I2s::new_pdm`, `i2s::pdm`), and the C3's metadata says PDM transmit
and the converter are there. The line's rate is the sample rate × 64
× fp/fs, the ratio an integer: 2 is the IDF's default (128 bits a
sample), 1 would be the 64 the software runs at, and whether the
converter works at 1 is not known. Nor is the order of its
sigma-delta, how near the rails it stays stable, or its gain: the
loudness may differ from the software's, and only the ear and a
scope will say.

**Design.**
- A clip asks for it, as it asks for its rate and width: bits 2 and 3
  of header byte 17, 0 for the software loop, 1 for the converter at
  64 bits a sample, 2 at 128 (`scripts/xm2clip.py --pdm 64|128`).
  Modules only; a spike's switch, to be replaced by the build's
  choice if it is kept.
- `core`: for such a clip `Player::fill` writes the level sequence
  itself, ramps and all, as `i16` little-endian, half the loop's
  level (so full scale is the same 0.7 of the rail, to compare
  loudness like with like, and inside whatever the converter holds),
  with −32768, the released line, after the end. `Stream::bit_rate`
  is then the ring's rate, 16 bits a sample, which is what the pacer
  counts by; `Stream::pdm` gives the ratio and `Stream::idle` fills a
  slot with the line's rest, zeros or −32768.
- `src/clip.rs`: `I2s::new_pdm` with the DAC line mode (one channel,
  no clock needed to tell left from right), mono, the high-pass off
  (it would take the ramps and the rest level away, and the tune has
  had its own), fp/fs 480/480 or 960/480, the dividers' minimum. The
  ring, the pacer, the pin's hand-over and the inverter are as they
  are. A TDM configuration clears `tx_pdm_en`, so a beeper clip after
  a PDM one should be itself; Trantor after Halftime B checks that.
- Host test: a PDM clip's bytes are the ramp from −32768, the tune's
  samples at the scale above and −32768 after; its ring rate.

**The A/B/C.** White-clock, the defaults (512, so the software tunes
at 1.024 Mbit/s): Trantor, Futurama, `Halftime A` (software, as
heard), `Halftime B` (hardware, 64, the same 1.024 Mbit/s),
`Halftime C` (hardware, 128, 2.048 Mbit/s), and Keygen the same.

**To hear.** B silent or garbled: the ratio of 1 does not work, and C
is the hardware's voice. A thump at the start or the end of B or C:
the rest level is not a released line (polarity, or the converter
idling on pulses). Then loudness and noise of B and C against A, and
Trantor after them. The serial log's `clip:` line would show a wrong
guess about the ring's rate as stray positions.

**Built, 2026-11-13.** As designed: `xm::pdm_ratio`,
`Player::for_converter` and `fill_samples`, `Stream::converter`,
`sample_rate` and `rest`, `clip::SAMPLE_REST`; in `src/clip.rs` the
one `match` on the stream's kind between `I2s::new` and
`I2s::new_pdm`, with a `clip: pdm, … Hz, … bits a sample` line in
the log. 113 host tests, the new one a module's bytes for the
converter against its samples. Flashed to white-clock over the air
with the eight melodies above. Nothing of the hardware path has run
before this: not heard, no log read, no scope on the line.

**First hearing: white noise from both, 2026-11-13.** B and C alike.
Read against the IDF's `i2s_hal_pdm_set_tx_slot`, esp-hal 1.2.2's
`pdm::regs_v2::configure_tx` differs in one bit that matters: it
clears `tx_pcm_bypass`, where the IDF never touches it in PDM mode
and the TDM path sets it. Cleared, the A-law/µ-law compander
(`tx_pcm_conf`, 0 at reset: A-law decompress; the register resets to 0xb200, bypass set) sits between the DMA and the
converter. `src/clip.rs` now sets the bit and latches it with
`tx_update` after `I2s::new_pdm`, through `I2S0::regs()`. A
hypothesis until heard: the bench board has no buzzer.

**The converter's clock, the same day.** Also from the reading: in
PDM mode the driver, like the IDF, overwrites the master clock's
fractional divider with a whole one ("to reduce the noise"), so the
rate is 160 MHz over integers. With the bit-clock divider at its
minimum of 8, 16 kHz at 64 bits came out at 16 447 Hz, half a
semitone sharp and 2.8 % ahead of the pacer's clock, and at 128 bits
8.5 % sharp. `clip::converter_clock` picks the divider (8 to 20) that
lands nearest and gives the ring's true rate for the pacer: 12 and 13
for the two, both 16 026 Hz, 0.16 % or 3 cents sharp.

**On the bench board (test-clock, no buzzer), 2026-11-13.** Over the
air, the serial log read from the capture an earlier session left
running. Halftime B and C for 14 s each, then A and Trantor: each
`clip:` line has 0 underruns and 0 stray positions, so the ring is
read at the rate assumed (one channel, two bytes a sample) and the
whole dividers are what was computed; the registers read back
`tx_conf 0310b220` (mono, bypass set, PDM on), `pcm2pdm 032aa003` and
`…005` (the ratio 1 and 2), `clkm …0d` and `…06`. What the line
carries is not known from this. The same image is on white-clock.

**Heard, 2026-11-13.** With the compander bypassed: B (64 bits a
sample) is still white noise, so the converter does not work at a
ratio of one; C (128 bits, 2.048 Mbit/s) plays, and "sounds better
than A. Sound is cleaner, less noise" (the user). The decision: keep
PDM, put the PCM clips through it as well, and keep the bit path for
the beeper clips, which are bits.

### PDM is the modulator for PCM clips and modules — DONE, heard and accepted by the user 2026-11-13

**Design.**
- The build's choice, not the clip's: `CLIP_MODULATOR` takes `pdm`,
  the default, beside `sd` and `pwm`. The software modulators stay as
  build options because the converter has been heard only on
  white-clock's circuit (diode, R5 at 5.1 k) and a stock board's
  transistor at 2 Mbit/s is unknown; they can go once one has been
  heard. `CLIP_KBPS` and `CLIP_CARRIER_KHZ` then matter to those two
  builds only.
- `config::CLIP_PDM`; `clip::CONVERTER`, `Some(128)` in such a build:
  the ratio is fixed, 64 being noise. `Player::fill` writes samples
  when it is set, for PCM and tune alike (a PCM clip's 8-bit steps at
  the same scale as before, 0.7 of the rail at full scale; the level
  is left where it was heard). The per-clip switch goes: header bits
  2 and 3 of a module are spare again, `xm2clip.py --pdm` is removed.
- An 8 kHz PCM clip runs the line at 1.024 Mbit/s, still 128 bits a
  sample; that rate through the converter has not been heard (B's
  failure was the ratio).
- `Stream::bit_rate` is the ring's rate for those clips, the beeper's
  512 kbit/s as before; `src/clip.rs` is as the spike left it, with
  the register dump cut down to one line.
- The ring stays 16 KB: half a second of samples now, a quarter of a
  second of a beeper clip's bits.
- Tests: the software modulators' tests call them directly and stay
  whatever the build; the stream tests follow `CONVERTER`.

**Built, 2026-11-13.** As designed: `config::CLIP_PDM` and
`clip::CONVERTER`; `Stream::new` gives every PCM clip and module to
the converter in such a build; `xm::pdm_ratio` and `xm2clip.py --pdm`
are gone, and a module's header bits 2 and 3 are spare (the player
has never read them otherwise). 113 host tests, run for `pdm`, `sd`
and `pwm`: the converter's test now takes PCM and a module through
`fill_samples` and checks the build's choice on the stream. The
software modulators stay linked into the default image, the stream's
kind being a field and not a constant; taking them out is for the
day they go altogether.

On the bench board (test-clock, no buzzer) with Trantor, Halftime,
Keygen and Futurama: Futurama, the PCM clip, to its end in 866 chunks
(886 KB of samples, 27.7 s) with 0 underruns and 0 stray positions;
Halftime, Trantor and Keygen cut short after 6 to 12 s, the same
zeros, a beeper clip after a PDM one and a PDM one after it. The same
image is on white-clock. PCM through the converter has not been
heard.

**To hear.** Futurama: against memory, cleaner or not, and as loud;
its 8-bit steps are now the floor, as the A clips' were. Trantor
unchanged. Halftime and Keygen as C was.

**Owed.** A stock buzzer circuit at 2 Mbit/s; a scope on the line
(the rest level before and after a clip, the pulse train's rate); the
level, still the software loop's 0.7 of the rail, which the converter
may not need; esp-hal's `tx_pcm_bypass` reported upstream.

**The converter's level, an A/B — planned 2026-11-13.** Full scale
under the converter is the software sigma-delta's 0.7 of the rail
(`LEVEL` 361, 127 steps), which was that loop's stability limit and
is nothing the chip's modulator is known to need. To hear, in one
firmware, each module four times: A at 0.70 (as now), B at 0.85
(+1.7 dB), C at 0.95 (+2.7 dB, the pulse-width modulator's figure),
D at 1.00 (+3.1 dB, the peaks at the rails themselves). The modules
are limited hard, 2 to 3 % of their samples at full scale, so a
modulator that overloads will be heard on them: grit or noise on the
loud notes. Only the clip's samples are scaled; the ramps and the
rest level stay. The switch is a trial's again: bits 2 and 3 of a
module's header byte 17 pick the step (`xm::trial`, `xm2clip.py
--trial N`), read only under the converter; `Player::with_step`. The
winner becomes the converter's `LEVEL` for PCM and modules alike and
the switch goes.

**Built, 2026-11-13.** As planned; the A clips are the ones in
`clips/` byte for byte. 113 host tests. On the bench board Halftime D
and Keygen B run and are cut with 0 underruns and 0 stray positions.
On white-clock over the air: Trantor, Futurama (0.70, PCM has no
switch), Halftime A to D, Keygen A to D. Not yet heard.

**Heard, 2026-11-13: B.** "B sounds best, still clear, C overloads in
a few places. D is louder, but at the cost of clipping in quite a few
places" (the user; martin-clock, the stock buzzer circuit, had the
same build by then). So the chip's modulator overloads between 0.85
and 0.95 of the rail. `clip::CONVERTER_LEVEL` is 438, 0.85, for PCM
clips and modules; the trial's switch, `xm::trial` and `--trial`, is
gone. 113 host tests.
White-clock and the bench board have the build with the four usual
clips (Trantor, Halftime, Keygen, Futurama); on the bench Futurama
and Keygen run and are cut with 0 underruns and 0 stray positions.
Martin-clock still has the trial's build with the ten melodies.
Futurama at 0.85 has not been heard.

**Futurama heard, 2026-11-13: "good".** The PCM clip through the
converter at 0.85, which closes what this entry owed the ear.
Martin-clock, the stock buzzer circuit, has the final build as well
(Trantor, Halftime, Keygen, Futurama; committed as `e658f2f`). Still
owed: a scope on the line, and esp-hal's `tx_pcm_bypass` reported
upstream.

**A correction, 2026-11-13.** White-clock's R5 was 5.1 k for all of
this day's hearings (the 16-bit samples, the bit rates, the converter
and its level), not the 3.2 k of 2026-11-12 that these entries first
said; the user put it right, and the entries and `config.rs` now say
5.1 k. That is the README's optional mod as written.

### Commodore 64 tunes: a SID on the chip — built 2026-11-13; heard on martin-clock, 4 steps a sample chosen by the user

The user's next source: `.sid` files, with one to start from in
`sid/`: "Donkey Kong" by Sascha Zeidler (Linus), 2016 Oxyron. A PSID
v2, 3608 bytes: 6502 code loaded at $8900, init there and play at
$8903, one song, called once a frame (PAL), written for the 8580.

**What a `.sid` is.** Not notes but the program that played them: the
C64's own player routine and its data, to be called fifty times a
second, each call writing some of the SID chip's 25 registers. So two
things are needed, a 6502 to run the routine and a SID to make sound
of the registers.

**The 6502 runs on the host, the SID on the chip.** As the beeper
clips are made (`scripts/zxbeeper.py` runs the Spectrum's code in an
emulator and keeps what reached the speaker), `scripts/sid2clip.py`
runs the tune's code in a 6502 of its own (Python, no dependency: the
documented instructions and the undocumented ones players use) and
keeps what reached the SID: the register writes of each frame, in
order, less the ones that change nothing. The clock gets that record
and a SID to play it through. Not a 6502 on the chip: it would want
the tune's 64 KB address space in RAM (or a paged copy of what the
code writes), a routine of unknown length on the interrupt executor,
and it buys nothing a record does not have but endlessness, which the
converter gives back: it hashes the machine's memory after every call
and, when a state comes round again, marks where the tune loops. A
record is 1 or 2 bytes a write; a few hundred bytes a second.

**The clip, `SID1`.** A 40-byte header: the magic, the chip's model
(6581 or 8580, for the filter's curve), the limiter's drive and
release and the high-pass as a module has them, the C64's clock in Hz
(985 248 PAL, 1 022 727 NTSC), the length in ms, the clock cycles a
frame (19 656 for a PAL screen; a tune on a CIA timer has the
timer's), the frame count, and where in the file the loop starts over.
Then the frames: pairs of register and value, then a byte with the top
bit set that says how many frames pass before the next writes (1 to
128, so rests cost nothing). The writes of a frame are applied
together at its start; a routine does them within a few hundred
microseconds. A tune with no loop found is cut at `--seconds` and
ends with a frame that zeroes the registers, so going round to the
start finds the chip as the first pass did.

**The SID, `core/src/sid.rs`.** Written for the clock from the chip's
published behaviour, not cycle by cycle: the oscillators and envelopes
are stepped at 64 kHz (15.4 clock cycles a step) and four steps are
averaged to a 16 kHz sample, which then gets the bass cut and the
limiter the modules get. Per voice: the 24-bit phase accumulator;
triangle (with ring modulation), sawtooth, pulse with its width, noise
from the 23-bit shift register clocked off accumulator bit 19, and
combined waveforms as the AND of their parts (the 8580 is near that;
a 6581's are quieter); hard sync; the test bit; the envelope with the
chip's sixteen rate periods, the exponential decay in its six steps,
and the rate counter's wrap at 32 768 that makes a note come late when
a slower rate is swapped for a faster one (players' "hard restart"
exists because of it, so it is kept). The filter is the two-integrator
loop with the register's resonance and low, band and high-pass taps,
the voices routed into or past it and voice 3's mute; the cutoff is
linear to 12.5 kHz for an 8580 and a typical 6581's curve from a
17-point table, held under 10 kHz, where the loop at 64 kHz is stable
and the output, 8 kHz wide, has nothing anyway. The master volume.
Not played: samples through the volume register (the record keeps a
frame's writes, not their timing), the paddles, and reads of voice 3
(`OSC3`, `ENV3`: a routine that modulates with them gets zero on the
host; the converter says when one reads them).

*Why 64 kHz.* Point-sampling a sawtooth or a pulse aliases: at 64 kHz
a 2 kHz note's fold-back is some 30 dB down, under what the buzzer
does to it. The cost is about a module's (three voices and a filter
at 64 kHz against four to eight channels at 32); `OVERSAMPLE` is one
constant should the ear or the underrun count ask for another figure.

**Shared with the modules.** The high-pass and limiter move out of
`xm.rs` into `core/src/shape.rs` (`Shaper`), unchanged, and both
players end in it. `clip::Player` gets a third body, the build takes
`SID` for a clip made to loop, `melody.rs` needs nothing.

**The converter.** `scripts/sid2clip.py tune.sid out.sidc [--song N]
[--seconds S] [--model 6581|8580] [--highpass 300 --passes 1]
[--drive N --release N]`. PSID only: an RSID needs the whole machine
(its interrupts, the KERNAL), and a PSID with no play address the
interrupt vectors; both are refused with the reason. It prints the
title, author, model, frame rate, the loop it found and the bytes a
second, and what the tune does that the clock will not play.

**Tests (`core/`).** A register's frequency comes out at its pitch;
the pulse's duty is its width; the triangle is one, and ring
modulation inverts it by the other voice; sync restarts the cycle;
noise is noise at the rate bit 19 gives; the envelope's attack, decay
to the sustain level and release take the chip's times, and the late
note of the rate-counter wrap; the filter passes and stops what its
mode says and stays bounded at every cutoff and resonance; the frames
land on their times, a rest costs a byte, the loop goes round where
the header says and a tune played once ends on its last frame; the
shaping is the modules'. The built-in clip, when a build has one,
plays to its length within range.

**To hear.** Donkey Kong as a fourth clip on white-clock beside the
three. The file stays in `sid/` and the clip under `target/` until the
user says whether it is to be a default clip: it is someone else's
music, as the others are, and would need its entry in
`clips/README.md`.

**Built, 2026-11-13.** As planned, with two changes. The envelopes
step once an output sample, not once a chip step, and the steps a
sample are the clip's to say (header byte 38, `--steps 2`; 4 without).

*The converter.* Donkey Kong: 4162 frames, 83.0 s, going round from
37.1 s (the state after frame 1860 comes again after the last); 62 KB,
748 bytes a second, most of it the pulse widths, which the tune sweeps
every frame. More than the estimate; a record of differences would
pack it, if the size comes to matter.

*Against libsidplayfp* (3.1.2 on the host, reSIDfp, through a
throwaway C++ program in the scratchpad; not in the repository). The
registers it shows after each stretch of 19 656 cycles are the
record's frames, in order, over the whole tune (its `play` runs a
little short of what it is asked, so a state repeats every dozen
samples; compared with a two-pointer walk). Its 16 kHz render and the
clock's mix before the shaping: loudness envelopes in 10 ms steps
correlate at 0.71 with a lag of 10 ms and no stretch, and the energy
in seven bands from 50 Hz to 8 kHz agrees within 1 dB up to 2.4 kHz
and is 1.5 dB under the reference above (the four-step average's
droop).

*On the bench* (test-clock, no buzzer, `/play`, the serial log's
`clip:` line, which now ends with the time spent filling the ring):

| clip | filling | share |
| --- | --- | --- |
| Halftime, a module of four channels | 5.1 of 43.5 s | 12 % |
| Donkey Kong, 4 steps a sample, envelopes every step | 23.1 of 83.0 s | 28 % |
| the same, envelopes every sample | 21.6 s | 26 % |
| Donkey Kong, 2 steps a sample | 14.2 s | 17 % |

No underruns or stray positions in any. The chip runs this code at
some five cycles an instruction, modules included, so the estimate of
"about a module's" cost was out by two. At 26 % a chunk is 8 ms on
the interrupt executor every 32; HTTP answered through it.

*To hear.* Martin-clock and test-clock have `v0.4-17-gd9830a5-dirty`
with Trantor, Halftime, Keygen, Futurama, and Donkey Kong twice: A at
4 steps a sample, B at 2 (half the time, more aliasing on high
notes). Stack 50.5 KB, image 1609 KB with Futurama (the slot is 1920).
Owed: the user's ear on A against B and on the tune at all; whether it
becomes a default clip; a 6581 tune and one that uses ring modulation
or sync against the reference; white-clock.

**Heard, 2026-11-13: "A is better".** Four steps of the chip a sample
stays the default, at 26 % of the processor while a tune plays;
`--steps 2` remains the clip's way to halve that. Still owed: whether
Donkey Kong becomes a default clip, a 6581 tune and one with ring
modulation or sync against the reference, white-clock.

**Donkey Kong stays local** (the user, 2026-11-13): the tune and its
clip are in `sid/`, ignored by git as `wav/` and `tap/` are, and the
user's `.env` names `sid/donkey_kong.sidc` after Futurama.

**The click at the start, 2026-11-13.** The user heard one as Donkey
Kong begins. It is the tune's: its routine gates all three voices in
frame 0 and closes them in frame 1, 160 ms before the first note, and
libsidplayfp's render has the same burst; the limiter, starting from
full gain, makes it as loud as the music. `scripts/sid2clip.py --mute
FRAMES` keeps the gate bit out of the control registers' writes for
that many frames and gives the registers what the routine meant at the
frame after; refused past the loop's start. The local clip is now
`scripts/sid2clip.py sid/Donkey_Kong.sid sid/donkey_kong.sidc --mute 2`:
silence for 150 ms, then the tune, same length. Heard on the clock,
2026-11-13: "Sounds good now" (the user). Martin-clock and white-clock
have it, with Trantor, Halftime, Keygen and Futurama.

### ZX Spectrum tunes: an AY-3-8910 and a PT3 player on the chip — built 2026-11-13, on white-clock to be heard; the register comparison with a reference player owed

The user's next source: `.pt3` files, two to start from in `pt3/`:
"Hibernation" by MmcM (5870 bytes, 20 positions) and
"synchronization" by nq (11 147 bytes, 98 positions), both Vortex
Tracker II modules on the ASM tone table. "Similar to what we did for
SID."

**What a `.pt3` is.** Unlike a `.sid` it is notes, not a program: Pro
Tracker 3's compiled module, three channels of patterns, up to 32
samples (a line a frame: level, tone offset, noise or envelope offset,
the mixer's bits) and 16 ornaments (a note offset a frame), a position
list with the position to go round to. The Spectrum's player routine
reads it fifty times a second and writes the AY-3-8910's fourteen
registers. So again two things: the player and the chip.

**Both on the clock.** A SID tune's player is 6502 code, which is why
it runs on the host and the clip is a record of writes, 750 bytes a
second. A PT3's player is one fixed routine, a few hundred lines with
no memory of its own to speak of, and the module is 6 to 11 KB for
three minutes where a record of the registers would be 100 KB or
more (three tone periods and three levels change nearly every frame).
So the clip is the module as it is behind a header, as a tracker
module's is, and the clock interprets it: `core/src/pt3.rs`. The chip
is `core/src/ay.rs`, by itself so that another AY format (PT2, STC,
ASC, a `.psg` record) is a second player and not a second chip.

**The clip, `PT31`.** A 40-byte header as `SID1` has: the magic; flags
(bit 0 a YM2149's 32 levels and not an AY's 16, bit 1 start the tune
afresh at its end and not from the module's loop position); the
limiter's drive and release and the high-pass's passes; the chip's
clock in Hz (1 750 000 on a Pentagon, 1 773 400 on a Spectrum 128);
the length in ms played once; the clock cycles a frame (35 840 on a
Pentagon, 48.83 frames a second; 35 454 on a 128, 50.02); the frames;
the frame the tune goes round to; the high-pass's section. Then the
module, untouched, its offsets its own.

**The player, `pt3::Tune`.** Pro Tracker 3.x and Vortex Tracker II
modules, one chip: the header's version digit chooses the tone table
(four, in an older and a newer cut) and the volume table (3.3/3.4 or
3.5); the pattern codes (note, rest, sample, ornament, volume, noise
base, envelope with its period, rows to skip), the effects (tone
slide, portamento, sample and ornament offset, the on/off vibrato,
envelope slide, tempo), and the per-frame sample and ornament walk
with the tone and level accumulators. Written for the clock from the
format's behaviour; joric/libpt3's C port of the Ay_Emul player (no
licence stated, so read and not copied) is the reference to compare
registers with, frame by frame, on the host. Every read of the module
is bounds-checked and answers 0, and a row's interpreter stops at the
module's end, so a broken module is wrong notes and not a hang. Not
played: TurboSound (two chips; the converter refuses it).

**The chip, `ay::Chip`.** Exact, and cheap for being so: the chip's
generators only ever change on a tick of clock/8 (218 750 a second),
and between changes the output is a constant, so a 16 kHz sample is
the output's mean over its 13 or 14 ticks, found by going from change
to change rather than tick by tick: a few steps a sample where a SID
needs four of everything. Three tone counters (period 12 bits, toggle
each period), noise from the 17-bit shift register stepped every 2·NP
ticks, the envelope's 32 positions a ramp with the sixteen shapes of
continue, attack, alternate and hold and its restart on a write of
R13, the mixer's six bits, the levels 1.5 dB a step (an AY uses every
other one). A generator nothing listens to (its channel silent or its
mixer bit off) is not a reason to stop: it is moved on in one go when
the span ends. No oversampling constant: there is nothing to alias.
Expected cost a fraction of a module's.

*Why the mean and not SID's steps.* A square wave point-sampled
aliases; the SID got 64 kHz steps for that and 26 % of the processor.
An AY's output is piecewise constant on a coarse grid, so its mean
over a sample is exact with integer arithmetic, and is what a chip
through a box filter gives.

**Shared.** `shape::Shaper` after the chip, as the other two players.
`clip::Player` gets a fourth body, `core/build.rs` and the melody
table take `PT3` for a clip made to loop.

**Going round.** Both tunes name their last position as the one to go
round to (an ending that repeats). For an alarm that is the wrong
thing to ring with for five minutes, so the converter's `--restart`
sets the header's flag and the tune starts over from its top, chip
and player as new.

**The converter.** `scripts/pt32clip.py tune.pt3 out.pt3c [--machine
pentagon|spectrum] [--clock HZ] [--chip ay|ym] [--restart]
[--highpass 300 --passes 1] [--drive N --release N]`. It checks the
module, walks the patterns for the length (rows times the tempo, with
the tempo's changes) and where the loop begins, and writes header and
module. The machine defaults by the tone table: the ASM and "real"
tables are tuned for a Pentagon's 1.75 MHz, the Pro Tracker and Sound
Tracker ones for 1.7734 MHz.

**Tests (`core/`).** The chip: a period is its pitch; the mixer's
bits gate tone and noise; noise is noise at its rate; each of the
sixteen envelope shapes level by level and R13's restart; the level
table against its formula; and the change-to-change `run` against a
tick-by-tick model of the chip over random registers and spans, sum
for sum. The player, on modules built in the test: a note's period
from its table, rows and tempo on their frames, the sample's levels
and loop through the volume table, an ornament's notes, slide and
portamento to its target, the envelope and noise registers, the
position list, the loop, `--restart` and the end; a truncated module.
A built-in clip plays to its header's length, which checks the
converter's walk against the player's.

**To hear.** Both tunes as clips on white-clock beside the others,
from `pt3/`, which stays out of git with `sid/` until the user says
whether either becomes a default clip.

**Built, 2026-11-13.** As planned. `core/src/ay.rs`, `core/src/pt3.rs`,
`scripts/pt32clip.py`; 138 host tests (15 new).

*What the tests found.* The chip's change-to-change `run` against the
tick-by-tick model caught two things in the first hour: a counter must
not be clamped to a shortened period at the register write (a period
is two registers, and is short for the moment between the two writes),
only when the next tick comes; and a run of no ticks must touch
nothing. The player's tests are on modules built in the test.

*The converter.* Hibernation: 20 positions, 4724 frames, 96.7 s,
5910 bytes. Synchronization: 98 positions, 25 088 frames, 513.8 s,
11 187 bytes. Both go round to their last position only (0.5 s and
5.2 s of ending), so both are converted with `--restart`; Hibernation
with `--chip ym`, as its title line says "ABC YM". The converter's
walk and the player agree on the frame count of both
(`a_built_in_tune_plays_to_its_length` with `ALARM_CLIP` naming them).

```
scripts/pt32clip.py pt3/MmcM-Hibernation.pt3 pt3/hibernation.pt3c --restart --chip ym
scripts/pt32clip.py pt3/nq-synchronization.pt3 pt3/synchronization.pt3c --restart
```

*Against a reference: owed.* joric/libpt3's `pt3_reader_zxssk.c` was
read and is in agreement with the player line by line, with one
difference on purpose: it adds a sample line's envelope offset as an
unsigned byte (`int AddToEnv += uint8_t`), where the Spectrum's
routine and Ay_Emul sign-extend it, as `pt3.rs` does. Building it on
the host to compare registers frame by frame (`CLIP_REG_DUMP=file
CLIP_INDEX=n cargo host-test dump_the_registers -- --ignored` writes
the clock's side, fourteen bytes a frame) was declined by the
session's permission check, since it is downloaded code; the user's
to run or to waive. Until then the player's notes are checked by its
own tests and by ear only.

*A stack overflow, found on the clocks.* The first image played
Trantor and panicked on every module, SID tune and PT3 (a load fault
in esp-rtos's task list, then `lock is not reentrant`): nothing in
the new player, but `clip::Stream::new` inlined into the clip task's
`poll`, where the fourth kind of player took the frame from about
5 KB to 6016 bytes, each kind built in a slot of its own and moved.
The clip task runs on the interrupt executor, so that frame lands on
whatever task's stack the interrupt found, the Wi-Fi task's included.
Now `Stream::new` is never inlined and builds the player in the
stream, each kind through its own `Player::take_*`: `poll` is 240
bytes, `Stream::new` 880, and the deepest of the three, a module's,
1648 under it, 2.8 KB at the worst and only while a clip starts. The
main stack is unchanged (50.5 KB with clips).

*On the bench* (test-clock, `/play`, 25 s of each, the serial log's
`clip:` line):

| clip | filling | share |
| --- | --- | --- |
| Halftime, a module of four channels | 2978 of 25 055 ms | 12 % |
| Donkey Kong, the SID at 4 steps a sample | 7169 of 25 102 ms | 29 % |
| Hibernation, the AY | 3249 of 25 041 ms | 13 % |

No underruns or stray positions. The AY is half the SID's cost and a
module's, not the fraction of one the plan guessed: the shaper and the
ring's 16-bit samples are a floor all three share.

*To hear.* White-clock and test-clock have `v0.4-19-g619d234-dirty`
(the second image of that name, with the stack fix) with Trantor,
Halftime, Keygen, Futurama, Donkey Kong, Hibernation and
Synchronization; 1572 KB of the slot's 1920. Owed: the user's ear on
both tunes (pitch and tempo against a Pentagon's, AY against YM for
Hibernation's envelope bass, the 300 Hz cut), whether either becomes
a default clip, the register comparison.

### A Stop button beside Play — built 2026-11-13, to be tried on white-clock

The user, with an eight-minute tune in the list: a preview could only
be ended by starting another. `POST /stop` silences the buzzer
(`buzzer::stop`) and, if the alarm is ringing, ends the ring as a
press would (`display::silence`, what MQTT's `silence` does), so the
panel and the state do not go on ringing in silence. No token, as
`/play` has none and the settings form can disarm the alarm anyway; a
command post with nothing to read, so a bare `curl -X POST` works and
a form's body, sent by a browser without script, is ignored. The page
gets `Stop` after `Play`, posted by script as Play is, so the form is
not reloaded: one handler for both buttons, which posts to the
button's own `formaction` (Stop gets the melody too, and ignores it),
because the page at its worst was 5865 bytes of 6016 with the nine
clips of the user's build and a second handler would not fit.

**Stop, on the bench, 2026-11-13.** Test-clock: a preview of
Synchronization, `curl -X POST /stop` eight seconds in, and the serial
log has `http: L1 Post /stop` and `clip: cut short` 2 ms after it.
The page's button and a ringing alarm are the user's to try.

**AY against YM, to hear.** White-clock has Hibernation twice, the one
module with the header's chip flag the only byte that differs:
"Hibernation A (AY)", sixteen levels in the envelope, and "Hibernation
B (YM)", thirty-two; then Synchronization (AY). The clips are under
`target/clips/`; the winner replaces `pt3/hibernation.pt3c`.

**Heard, 2026-11-13: "Let's keep AY"** (the user, of Hibernation A
against B). `pt3/hibernation.pt3c` is the AY one, `--restart` alone;
`--chip ym` stays the converter's option.

**A third tune, 2026-11-13.** `pt3/nq-ppk.pt3`: "'resurection' (c) ppk
/rmx" by Nik-O, a Pro Tracker 3.5 module on the Sound Tracker table,
so a Spectrum 128 by the converter's default (1 773 400 Hz, 50.02
frames a second): 41 positions, 8650 frames, 172.9 s, 6853 bytes. Its
loop is a real one, round from position 14 at 56.2 s, so no
`--restart`; it uses the ornament offset and the envelope slide, which
the first two did not. `scripts/pt32clip.py pt3/nq-ppk.pt3
pt3/ppk.pt3c`, labelled PPK. The player's frame count agrees with the
converter's. White-clock has eight clips: Trantor, Halftime, Keygen,
Futurama, Donkey Kong, Hibernation, Synchronization, PPK.

### Quieter noise for the AY's drums — built 2026-11-13, an A/B/C on white-clock to be heard

The user, of the PT3 tunes: "Drums sound too aggressive. It is white
noise, high pitch, naturally more pronounced on the buzzer. Maybe mix
it in at a lower volume? Again, A/B testing."

An AY drum is a channel with the noise let through its mixer bit. So:
a channel whose noise bit is open is mixed at a fraction of its level,
`ay::Chip::set_noise_level` in sixteenths, the clip's to say in header
byte 38 (0 for all of it), `scripts/pt32clip.py --noise N`. The level
is scaled, not the noise filtered: the chip's output stays piecewise
constant, `run` stays exact, and the tick-by-tick model has the same
rule. A channel with tone and noise both open is scaled too (it is the
same hit, with a pitch under it); the chip's envelope on such a
channel likewise.

To hear, on white-clock: Synchronization three times (the user's
choice of tune), "Sync A" the noise as the chip has it, "Sync B" at
8/16 (−6 dB), "Sync C" at 4/16 (−12 dB); the clips under
`target/clips/`. The chip's model in the tests has the same rule and
`run` still agrees with it sum for sum; 138 host tests pass.

**Heard, 2026-11-13: "B is better"** (the noise at 8/16), and the
user's question: how does the chip really mix, so that the model is
right before the player is bent to the buzzer.

**The chip against two emulators' sources** (Ayumi by Peter Sovietov
and MAME's `ay8910.cpp`, read on 2026-11-13; nothing copied but the
level tables, which are measurements).

| what | the emulators | `ay.rs` |
| --- | --- | --- |
| a channel's gate | (tone or tone off) and (noise or noise off), before the DAC; both off is a steady level | the same |
| tone | counts up, turns at count >= period, period 0 as 1 | the same |
| noise | 17 bits, bit 0 xor bit 3 in at the top, bit 0 out, a step every 2·NP ticks of clock/8 | the same |
| envelope | 16 levels on an AY at 2·EP ticks each, 32 on a YM at EP; the ten shapes | the same; MAME has period 0 as half of 1, here it is 1, as Ayumi has it |
| levels | measured, uneven: an AY's top steps are 1.9 to 2.4 dB, its low ones 3 to 4.4 | **was the data sheet's even 3 dB** |
| the three outputs | added up (Ayumi); MAME also models the pins tied together into one load, as a Spectrum 128 has them, where loud channels press each other down | added up |
| DC | all above nought; a YM has an offset | above nought, the high-pass takes it |

So the logic was right and the levels were not. With 3 dB a level a
note at level 12 came out at 0.35 of full scale where the chip gives
0.49, level 10 at 0.18 for 0.29: the tunes' notes, which sit at 10 to
13, were 3 to 4.4 dB too quiet beside anything at 15, which is where
drums start. Some of "the drums are too aggressive" was that. `ay.rs`
now has the measured tables, `LEVELS_AY` and `LEVELS_YM` (Ayumi's,
scaled to 32767). Not taken: the tied-pins model. It needs a machine's
load resistor, the tunes here are Pentagon ones (each output buffered,
ABC stereo), and its effect, loud sums compressed, is what the limiter
does anyway.

**To hear again, on the measured levels.** The judgment of 8/16 was
made on the wrong table, so the trial is repeated: white-clock has
"Sync A" the noise as the chip has it, "Sync B" at 11/16 (−3 dB),
"Sync C" at 8/16 (−6 dB, the earlier B). What is left for `--noise`
to do after this is the buzzer's share alone.

**Logarithmic, 2026-11-13** (the user's recollection, checked). It is:
the data sheet's sixteen levels are 3 dB apart, and openMSX's
`AY8910.cc` builds its table so (1.5 dB a step over a YM's 32, an AY
on every other), which is what `ay.rs` had first. Ayumi and MAME use
chips measured instead, and those are logarithmic too but not evenly:
from full scale down, in dB,

| level | 14 | 13 | 12 | 11 | 10 | 9 | 8 | 7 | 4 | 1 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| measured AY | −1.9 | −3.9 | −6.2 | −8.6 | −10.7 | −13.8 | −18.0 | −19.4 | −30.3 | −40.0 |
| even 3 dB | −3.0 | −6.0 | −9.0 | −12.0 | −15.0 | −18.1 | −21.1 | −24.1 | −33.1 | −42.1 |
| linear, for contrast | −0.6 | −1.2 | −1.9 | −2.7 | −3.5 | −4.4 | −5.5 | −6.6 | −11.5 | −23.5 |

Neither table in `ay.rs`, then or now, was linear. (A PT3's own volume
table multiplies a sample's level by the channel's volume as plain
numbers, 0 to 15, and the product is then a level register's value:
that is the format, and the logarithm comes after it.) The emulators
disagree on which curve, so the choice between the measured table (in
now) and the even one is one more thing an A/B could settle.

**Heard, 2026-11-13: "B is still better"** on the measured levels: the
noise at 11/16 (−3 dB). The next question is the user's: low-pass the
noise instead, as an A/B.

### A low-pass for the AY's noise — built 2026-11-13, an A/B/C on white-clock to be heard

The chip stays exact: `ay::Chip::run_parts` answers the output in two
sums, the channels with the noise let through and the rest, and `run`
is their total. `pt3::Tune` makes a sample's mean of each and, if the
clip's header byte 39 is not nought, puts the noisy one through a
one-pole low-pass at the sample rate (y += (x − y)·k/256) before
adding them. `scripts/pt32clip.py --noise-lowpass HZ` works out k. The
whole of a drum channel is filtered, a tone under its noise too, and
its mean level passes, so nothing thumps. 6 dB an octave: white noise
through 3 kHz keeps about −2 dB of its power and loses its hiss,
through 1.5 kHz about −5 dB.

To hear, Synchronization three times: "Sync A" the noise at 11/16 and
no filter (the winner so far), "Sync B" the noise at all of its level
through 3 kHz, "Sync C" the same through 1.5 kHz.

**The noise only, 2026-11-13** (the user, before judging: "The tone
under the noise should not be filtered"). The first cut filtered a
drum channel's whole output. Now `run_parts` answers `ay::Parts`:
`plain`, `noisy`, and for the channels that listen to the noise `open`,
what their tones and levels give with the noise's bit always set, and
`noise`, the ticks its bit was set for. With a low-pass asked for, the
player filters that fraction, a number from 0 to 1 a sample, and the
listening channels are `open` times it: the tone's edges are the
chip's, the noise that gates them is smoothed. (Within one sample the
tone and the noise are taken as independent, which they are but for
14 ticks' chance.) Without a low-pass nothing changes and `noisy` is
used as it was, exact. White-clock has the same three variants built
this way.

**Heard, 2026-11-13: "Let's keep A".** The noise at 11/16 with no
low-pass, over both corners of the filter. `scripts/pt32clip.py`
now has `--noise 11` as its default; `--noise-lowpass` stays in the
converter and the player as an option nobody's clip uses. The three
tunes are converted again on the measured levels with the default:

```
scripts/pt32clip.py pt3/MmcM-Hibernation.pt3 pt3/hibernation.pt3c --restart
scripts/pt32clip.py pt3/nq-synchronization.pt3 pt3/synchronization.pt3c --restart
scripts/pt32clip.py pt3/nq-ppk.pt3 pt3/ppk.pt3c
```

White-clock is back on the eight clips: Trantor, Halftime, Keygen,
Futurama, Donkey Kong, Hibernation, Synchronization, PPK.

### Bass cut for a speaker: an A/B on headphones — built 2026-11-13, on test-clock to be heard

The user is waiting for 32 Ω speakers to try in the buzzer's place
and meanwhile listens to test-clock's GPIO5 on headphones: the
clock's own stage on a breadboard (`bench/buzzer-sim.png`: a BC557
for the S8550, 10 k into its base, 100 Ω for the load, the headphones
across it through 4.7 k). There the clips are thin: their bass cut
was chosen for the buzzer, which is all but silent under 1 kHz (PCM:
900 Hz twice in `scripts/wav2pcm.sh`; modules: 300 Hz once, the
default of `scripts/xm2clip.py`). The user's words were "low-pass
filter too low"; taken to mean this cut.

To hear, as six melodies in one build (`target/ab/`, nothing in
`clips/` changes), only the high-pass differing within a tune:

| melody | high-pass |
| --- | --- |
| Halftime A | 300 Hz once, as on the clocks |
| Halftime B | 100 Hz once |
| Halftime C | none |
| Futurama A | 900 Hz twice, as on the clocks |
| Futurama B | 300 Hz once |
| Futurama C | 60 Hz once |

Futurama is its first 12 s, three times, so the image stays in its
slot; the limiter (20 dB of drive) is the clocks' in all six. No code
changes: the module's cut is its header's, and the PCM variants are
`wav2pcm.sh`'s sox line with another `highpass`.

**Heard, 2026-11-13** (the breadboard's base resistor is 5.1 k, the
user says, not the 10 k drawn): "Futurama C is good, all the
Halftimes are distorted (overdriven)". So the cut was the PCM clip's
trouble, and the PDM stream through the stage is not; the module's is
the limiter. Halftime's mix before it peaks at 88428 with an RMS of
18238 (`CLIP_RAW` writes a quarter of the mix, which the first
figures here missed) against the limiter's knee of 3000, 86 % of the
samples over it, and with the release of 16 ms the gain moves 3.3 dB
on average inside 5 ms: it follows the waveform, which on the buzzer
was loudness and on headphones is overdrive. A release of 1 s moves
it 0.1 dB, for 5.3 dB less level (RMS 0.50 and 0.27 of full scale as
the player gives them).

The second round, the high-pass at 60 Hz once in all, as Futurama C:

| melody | limiter's release (`xm2clip.py --release`) |
| --- | --- |
| Halftime A | 7, 16 ms, as on the clocks |
| Halftime B | 10, 128 ms |
| Halftime C | 13, 1 s |
| Futurama C | the first round's, for the level |

**Heard meanwhile: Futurama C "has a little bit of hissing, like
volume knob is at its max".** That is the 20 dB of drive into sox's
limiter, which lifts the quiet parts and the recording's and the
8-bit samples' noise with them. Two more in the same build, the cut
at 60 Hz as C's: Futurama D with 10 dB of drive (RMS 0.29 of full
scale against C's 0.48), Futurama E with none, only normalised (0.15).

### Where a module's overdrive comes from: the limiter's release — measured on the host 2026-11-13, the A/B on test-clock's headphones to be heard

"Let's try fixing Halftime (and MOD player) in general" (the user,
after the first round above). Measured on the host first, so the
player and the limiter are told apart: Halftime's mix before the
shaping against openmpt123's render of the module (32 kHz, mono,
down to 16 kHz) has the same energy in every band from 0 to 8 kHz
within 0.6 dB (58 % of it under 150 Hz: the tune is mostly bass), so
the notes, volumes and interpolation are right. The limiter is the
distortion: what a gain steady over 50 ms cannot explain of its
output is 25 % of the signal at the clocks' release (16 ms) and 6 %
at 1 s, with the knee at 3000 where the mix's RMS is 18238: it is
always limiting, by 19 dB on average, and with the bass let through
it rides the bass.

On the buzzer that was the point (loudness, and under 900 Hz nothing
sounded). "Let's try disabling the limiter?" (the user). Off is a
plain gain, and a gain needs a level to aim at, so the limiter's knee
becomes the clip's: header byte 29 (a spare), the byte times 1024
where one voice at full level is 32768, 0 for the 3000 of before, so
every clip made so far plays as it did. `Shaper::new` takes it,
`scripts/xm2clip.py --knee N` writes it; the SID and PT3 players pass
0 until their headers have the byte. A knee above the tune's peak
(100547 for Halftime's mix after the 60 Hz cut) is the limiter off.

What the host says of the three beforehand: the clocks' limiter puts
3.35 % of the samples at full scale, RMS 0.50; the 1 s release 0.09 %,
RMS 0.27; the limiter off none, peak 126 of 127, RMS 0.16, which is
10 dB under the clocks' level and 4.7 dB under the slow release's. A
knee anywhere under the tune's level changes nothing once the release
is 1 s (3000 to 22272: the same in 97.4 % of the samples), since the
envelope then sits near the peak.

To hear, the cut at 60 Hz once in all:

| melody | limiter |
| --- | --- |
| Halftime A | the clocks': knee 3000, release 16 ms |
| Halftime B | knee 3000, release 1 s (`--release 13`) |
| Halftime C | off: `--knee 100` (102400), a plain gain |
| Futurama C, D, E | 20 dB, 10 dB and no drive into sox's limiter |

**Heard, 2026-11-13: "All Halftimes are overdriven, like it clips"**,
the one with the limiter off included, and "Futurama C is good (even
with hissing)", with the question whether the hiss is the modulator
at only 2 MHz. So the limiter was not it, or not all of it: Halftime
C is a plain gain whose host output peaks at 126 of 127 and touches
the rail nowhere. What is left is told apart by a third build:

| melody | what it is | if it is clean |
| --- | --- | --- |
| Halftime C | the module, limiter off, as before | |
| Halftime PCM | the first 20 s of the host's output of that same clip (the dump, 8 bits), as a PCM clip | the player's samples are good and the chip makes others of the module (time, the 16-bit path) |
| Futurama C | as before | |
| Silence | 6 s of PCM at mid-scale | no hiss here: the hiss is in Futurama's samples (8 bits, 20 dB of drive), not the modulator or the stage |

And off the clock: the same host output and openmpt123's render as
WAV files for the user's headphones on the PC. Overdriven there too,
and it is the tune or the player's arithmetic; clean there and
overdriven from the clock's PCM clip, and it is the converter or the
transistor with this material (Halftime is 58 % bass under 150 Hz,
Futurama is not).

**Heard, 2026-11-13: "Silence isn't quiet, it's hissing, so comes
from modulator. Let's bump it to 4MHz?"** Silence is PCM at
mid-scale, so nothing of it is in the samples: the hiss is the
converter's bits or what the transistor makes of them. The two pull
opposite ways at a higher rate: more bits a sample is less of the
modulator's own noise in the audio band, and shorter pulses (244 ns)
are more of the stage's uneven edges, which fold the bitstream's
noise back down. Only the ear says which wins.

`CLIP_PDM_BITS` at build time, 128 (the default), 256 or 384, is what
`clip::CONVERTER` carries: the converter's ratio fp/fs of two, four
or six, with fp at the IDF's 960 and fs 480, 240 or 160 (esp-hal's own
DAC default is fs = rate/100, a ratio of six at 16 kHz, so these are
the driver's ground). `converter_clock` gives the same ring rate at
each (divider 13, the master's 6, 3 and 2). test-clock has the third
build's four melodies at 256, 4.096 Mbit/s; nothing else differs.
Not known before it is heard: whether the converter is sound at four,
and whether the BC557 passes pulses that short at all.

**Heard at 256 bits, 2026-11-13: "No hissing with Futurama, and it
sounds right … Silence is quiet."** So the hiss was the converter's
own noise at 128 bits a sample, and the BC557 with 5.1 k at its base
passes 4.096 Mbit/s. (Heard on the breadboard stage and headphones
only; the clocks' S8550 and buzzer have not had this rate.)

**And: "Halftime PCM and Halftime C is very quiet with occasional
clips. I don't think that the mixer is implemented correctly."** The
two alike, so the chip plays the samples the host computes. Against
openmpt123's render again, this time the peaks: the crest is 13.7 dB
in ours and 13.9 dB in the reference, the RMS ratio is 0.83 to 0.88
in every one of the 43 seconds, and the twelve largest peaks are at
the same places with the reference's as large or larger. The clips
are the module's own: instrument 1 has 11.5 % of its samples at the
8-bit rail, in runs of up to 20, and instrument 2 has 5.8 %, runs of
15 (the drums, recorded clipped), and the waveform at the loudest
peak has the same flat shelf in both renders. Quiet is what a plain
gain makes of a tune whose peaks are 14 dB over its RMS: −16 dB of
full scale, against Futurama C's −6.

So, between off and the clocks' 20 dB, with the release at 128 ms
(`--release 10`) and the build at 256 bits:

| melody | limiter | RMS of full scale |
| --- | --- | --- |
| Halftime A | off (`--knee 100`) | 0.16, −16.0 dB |
| Halftime B | `--knee 50`: the drums' peaks only, 6 dB | 0.30, −10.6 dB |
| Halftime C | `--knee 25`: 12 dB | 0.36, −8.9 dB |
| Futurama C, Silence | as before | |

**Heard, 2026-11-13: "WAV plays correctly on the host. All 'halftime'
are distorted with strong bass, very low trebles. Do we have the same
in PCM?"** The host's samples are right to the ear as well, then, and
what is wrong happens after them, on the clock, to this tune and not
to Futurama. Halftime PCM in the build before last was the player's
own output as PCM and was heard as the module was. What differs from
Futurama is the bass: 58 % of Halftime's energy is under 150 Hz, so
the bit density sits near its ends for milliseconds at a time, where
a stage with uneven edges gives way, and what rides on the bass (the
treble) goes with it. A guess until heard; the fifth build, at 256
bits, is to tell:

| melody | what it is |
| --- | --- |
| Halftime A | the module, limiter off, as before |
| Halftime WAV | openmpt123's render, the WAV heard on the host, its first 20 s as PCM, normalised and nothing else |
| Tone | 1 kHz at a quarter of full scale, 5 s |
| Tone Bass | the same tone on 80 Hz at 0.6 of full scale |
| Futurama C, Silence | as before |

Halftime WAV wrong as well: nothing of the player is in it, the
output is. The tone pure alone and rough or fading on the bass: the
stage (or the converter) does not carry a small signal on a large
slow one.

**The stage, not the player: heard 2026-11-13.** "2.2K base to
emitter made it almost crisp clear" (the user, on the build above).
The breadboard's BC557 had 5.1 k into its base and nothing to take
the base's charge out when GPIO5 lets go, as the clock's S8550 has
nothing: about 0.84 mA of base current for a 50 mA load is three to
four times what it needs, so it saturated and turned off late, each
pulse stretched by a time that is a good part of a bit, and the error
grows with how often the stream toggles. A resistor from the base to
the emitter is the way out for the charge and leaves 0.5 mA of drive.
`bench/buzzer-sim.png` has it now.

What this takes back: everything heard of Halftime before it, the
limiter rounds included, was heard through that stage, and so was the
hiss at 128 bits a sample, which may have been the stage folding the
stream's noise down and not the converter's own. Both are asked
again in the sixth build, at the default 128 bits, with the resistor
in:

| melody | what it is |
| --- | --- |
| Halftime A | limiter off (`--knee 100`) |
| Halftime B | `--knee 50 --release 10` |
| Halftime C | `--knee 25 --release 10` |
| Halftime D | the clocks' limiter, the cut at 60 Hz |
| Futurama C, Tone Bass, Silence | as before |

And what it leaves open for the clocks: a stock board has no such
resistor. Whether its S8550 saturates depends on the load (a buzzer's
coil, or the 32 Ω speaker at about 156 mA, which 0.84 mA of base
current may not saturate at all); a scope on the collector with the
speaker in will say, and if it does saturate, 2.2 k from base to
emitter is a third part for the buzzer mod.

**256 bits a sample is the default, 2026-11-13** ("I meant make 4MHz
default", the user). `config::CLIP_PDM_BITS` is 256 with no variable
set, 4.096 Mbit/s for a 16 kHz clip; `CLIP_PDM_BITS=128` is the rate
the clocks' buzzers have heard. Owed: a clock's own S8550 and buzzer
at 256, which only the breadboard stage has had, and the sixth build
above (Silence at 128 with the base resistor in, the four limiter
settings), which was built and not flashed.

**8 MHz and 31 250 samples a second: built 2026-11-13, not heard**
("Let's try bumping bitrate to 8MHz and sampling rate accordingly for
pt3, sid and MODs", the user). At 256 bits a sample the converter's
whole dividers give 8 MHz exactly, 160 MHz over 2 and 10, for 31 250
samples a second; 32 000 is not among them (the nearest is 34 722,
8.5 % sharp), so the rate is `clip::FAST_HZ`, 31 250, and the players
work at it, not at a nominal 32 kHz the line would miss.

The rate is the clip's, in its header, so a clip converted before
plays as before and both can be in one firmware for the ear:

| clip | where | player |
| --- | --- | --- |
| module | byte 17 bit 2 | mixes at 62 500 and averages two, as 16 kHz does at 32 000: twice the work |
| SID tune | byte 39 = 1 | the chip stepped `steps` times a sample; the converter writes 2 unless told, 62 500 a second, the work of 16 kHz at four |
| PT3 module | flags bit 2 | the chip's mean over each sample's ticks, as before |

Each converter takes `--rate 31250` and works out the high-pass (and
`pt32clip.py` the noise's low-pass) for it; the limiter's release is
two shifts on from 8 kHz's for the same time (`Shaper::new`'s
`octaves`). The ring's chunk is 512 samples, 16.4 ms, as a 16 kHz
clip's is 32: half the time ahead of the DMA, 230 ms. A build at
`CLIP_PDM_BITS=384` cannot make 12 MHz and refuses such a clip with a
line in the log; at 128 the line is 4 MHz.

To look for on the clock: the `clip:` line's share of the processor
(a four-channel module took 12 % at 16 kHz) and its underruns, pitch
against the 16 kHz clip of the same tune, and whether the stage
passes pulses of 125 ns at all: the breadboard's needed its base
resistor at 4 MHz.

PCM clips take the rate too (the user: "re-create all local clips in
pt3 wav and sid"): `PCM1` with 31 250 in its header, `scripts/wav2pcm.sh
… 31250`, 31 KB of flash a second. Re-created in place at 31 250, with
the options each was made with (the three tunes' old files were the
converters' output byte for byte, checked first): `sid/donkey_kong.sidc`
(`--mute 2`), `pt3/hibernation.pt3c` and `pt3/synchronization.pt3c`
(`--restart`), `pt3/ppk.pt3c`, `wav/futurama.pcm` (the whole of
`futurama_theme.wav`, 864 KB). Not re-created: the other files in
`wav/`, experiments whose commands are not on record, and `clips/`,
which stay at 16 kHz; `mods/halftime-31k.xmc` and `mods/keygen-31k.xmc`
are the two modules at the new rate. The build with all seven is
1818 KB of the slot's 1920.

**Heard and kept, 2026-11-13** ("Sounds good, both bench with
headphones and white clock. Keeping it as the default", the user).
The three converters make a clip at 31 250 unless told `--rate 16000`
(`sid2clip.py` then steps the chip twice a sample unless told
`--steps 4`), and `clips/halftime.xmc` and `clips/keygen.xmc` are
re-made with them. `scripts/wav2pcm.sh` stays at 16 000 unless told:
PCM at 31 250 is 31 KB of flash a second. Not read off the clock: the
`clip:` line's share of the processor for a module at this rate.

### PT3 tunes flat, for headphones — built 2026-11-13, to be heard on test-clock

"Drop our extra filters we made from PT3 player, I want to try it flat
on headphones" (the user). What the player did to the chip's output
was all for the buzzer: the noise at 11 sixteenths, the optional
low-pass over it, the 300 Hz high-pass and the limiter with its 20 dB
of drive. None of it is taken out of the player, since the clocks'
buzzers still want it; a clip says it wants none of it: flags bit 3,
`pt3::FLAT`, `scripts/pt32clip.py --flat`, which also writes the noise
at 16 sixteenths, no low-pass and no pass of the high-pass.

Two things have to stay for a flat clip. The chip's output is all
above nought, so its mean is taken out, by a one-pole tracker at 5 Hz
in `pt3::Tune::next_sample` and not by the shaper's high-pass, whose
Q14 section cannot be put under 100 Hz or so at 31 250 samples a
second (16384 w² is under 1 at 20 Hz). And a gain needs a level: the
limiter's knee is three channels at their top level (`FLAT_KNEE`, 96
in the shaper's 1024s), so nothing ever limits and the header's drive
is a plain gain, 127 for that, to 255 for twice it. The header had no
spare byte for a knee of the clip's own, as `XMC2` has.

On the host, the three tunes as the clocks have them against flat:

| tune | limited: peak, RMS of full scale | flat: lowest, highest of 127, RMS |
| --- | --- | --- |
| Hibernation | 127, 0.41 | −57, 113, 0.17 |
| Synchronization | 127, 0.37 | −52, 126, 0.18 |
| PPK | 127, 0.41 | −48, 119, 0.12 |

So flat is 7 to 10 dB quieter and needs no more gain than the chip's
own: the peaks are within 1 dB of full scale. The flat files are
`target/clips/*_flat.pt3c`, beside the clocks' own in one build:

```
ALARM_CLIP=clips/trantor.bpr,clips/halftime.xmc,clips/keygen.xmc,pt3/hibernation.pt3c,target/clips/hibernation_flat.pt3c,pt3/synchronization.pt3c,target/clips/synchronization_flat.pt3c,pt3/ppk.pt3c,target/clips/ppk_flat.pt3c
```

The host test (`the_samples_are_the_notes_on_their_frames`) has a flat
square wave: a third of full scale from top to bottom, the same either
side of nought once the mean is found, at its pitch. Sent to
test-clock over the air (ota_1, `v0.4-24-g2305f61-dirty`); the clock
did not come back on the LAN after it, see the next entry. Not heard.

**A/B for the headphones, 2026-11-13: on test-clock over USB, to be
heard.** "Sounds like too much bass with little treble" (the user, of
the flat clips). Nothing shapes a flat clip; the bass is the chip's,
which the buzzer's high-pass had hidden: 53 % of flat
Synchronization's energy is under 300 Hz against 9 % as the clocks
have it. So `--flat` no longer forbids the high-pass (`--flat --passes
1 --highpass HZ`; `--noise` likewise), and five of Synchronization
are side by side, `target/clips/sync_*.pt3c`, the first minute's
energy by band from the host:

| melody | what | RMS | under 150 Hz | 150 to 300 | 300 to 1 k | 1 to 3 k | over 3 k |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Sync A Flat | no high-pass | 0.18 | 35 % | 18 % | 16 % | 13 % | 18 % |
| Sync B Hz | 100 Hz, 12 dB an octave | 0.15 | 14 % | 22 % | 21 % | 18 % | 25 % |
| Sync C Hz | 200 Hz | 0.13 | 3 % | 15 % | 26 % | 23 % | 32 % |
| Sync D Hz | 300 Hz | 0.13 | 1 % | 7 % | 27 % | 27 % | 37 % |
| Sync E Clock | the clocks': 300 Hz, noise 11/16, the limiter | 0.37 | 1 % | 8 % | 35 % | 27 % | 29 % |

(The labels lose their digits.) B to D touch full scale in under
0.002 % of the samples. The same five as 16-bit WAV files for the
host, `target/wav/sync_*.wav`: `CLIP_WIDE=1` with `CLIP_PCM_DUMP`
writes the player's samples as `i16` and not in 8-bit steps.

**Heard, 2026-11-13: flat with a 100 Hz cut and 3 dB of gain.** "Limiter
hurts PT3s. Sync A flat sounds correct in headphones, but it's too
quiet (e.g. vs Halftime)" (the user). Halftime's RMS is 0.40 of full
scale and flat Synchronization's 0.19 with its peaks already at full
scale (the waveform is lopsided, +1.0 and −0.41), so more level is a
plain gain that clips the tops: the header's drive, 180 for 1.4 times
and 254 for twice. Two rounds on the bench's headphones, without the
cut (A, F, G) and with 100 Hz at 12 dB an octave (H, I, J):

| melody | cut | gain | RMS | samples at the rails |
| --- | --- | --- | --- | --- |
| Sync A Flat | none | 1 | 0.19 | none |
| Sync F Louder | none | 1.4 | 0.26 | 0.4 % |
| Sync G Loudest | none | 2 | 0.36 | 2 % |
| Sync H Cut | 100 Hz | 1 | 0.15 | none |
| Sync I Cut Louder | 100 Hz | 1.4 | 0.21 | 0.1 % |
| Sync J Cut Loudest | 100 Hz | 2 | 0.30 | 1 % |

"I prefer sync_i_cut_louder": `scripts/pt32clip.py … --flat --passes 1
--highpass 100 --drive 180`. That is a choice for headphones on the
breadboard stage; no clock's buzzer has heard a flat clip, and the
converter's defaults are still the buzzer's.

**The default, 2026-11-13.** "sync_i_cut_louder is also better on the
actual clock. So let's make it default. Regenerate PT3 clips" (the
user, after the same build on white-clock's buzzer). `scripts/pt32clip.py`
with no options now makes that clip: `FLAT`, the noise at 16
sixteenths, the high-pass at 100 Hz once, drive 180. `--flat` is gone
and `--limiter` is the way back: `--limiter --highpass 300 --noise 11`
gives the old Synchronization byte for byte, and no options the
chosen clip byte for byte (both checked). `pt3/hibernation.pt3c`
(`--restart`), `pt3/synchronization.pt3c` (`--restart`) and
`pt3/ppk.pt3c` are re-made; on the host:

| tune | RMS of full scale | samples at the rails |
| --- | --- | --- |
| Hibernation | 0.21 | 0.02 % |
| Synchronization | 0.21 | 0.11 % |
| PPK | 0.14 | 0.01 % |

Only Synchronization was heard with these settings. The player's own
defaults for a clip without the flag are unchanged, so a `.pt3c` made
before plays as it did.

### The station picks its access point: the strongest of each scan — built 2026-11-13, not on a clock yet

"Bench board keeps trying to connect to far AP. … let's do pinning
right after scan - force it to connect to the AP with the strongest
signal. After disconnect and next scan a new choice must be made" (the
user). This is item 3 of the 2026-10 list above, without its
fall-through. `net::wifi_task` scanned before every connect only to
log the candidates and left the choice to the driver, which under
`ScanMethod::AllChannels` should take the strongest and does not
always: it avoids the access point that last dropped it.

Now `strongest_candidate` answers the loudest access point of that
scan and the connect is made with its BSSID and channel in the
`StationConfig` (`station_config(creds, Some(ap))`), logged as
`wifi: connecting to <ssid> at <bssid> channel N, rssi R`. The loop
sets the unpinned config first (the scan runs under it), so a pin
never outlives its connect: after a disconnect or a failed attempt the
next turn scans and chooses again. A scan that fails or hears nothing
leaves the driver to look for itself, as before (`the driver
choosing`). There is no second choice within a turn: if the loudest
access point refuses the clock every time, the run ends in the
connect budget's reset (ten attempts or three minutes) as any other.

To look for in the serial log: the `candidate` lines, then the
`connecting to … at` line naming the loudest of them, then `connected
to` with the same BSSID; pull the near access point's power and the
next turn must name the other.

### The page split in four: clock, settings, Home Assistant, system — built 2026-11-14, to be tried on white-clock

The user: "The main page has way too much." One page held every
setting, the broker and the status. Agreed with the user 2026-11-14:

| page | what it holds | posts to |
| --- | --- | --- |
| `/` | the alarm (armed, time, days, skip, tune with Play and Stop), the display (colour, brightness), the status lines | `POST /`; the display by script to `/api` |
| `/settings` | timezone with the browser-zone button, 12 hour clock, temperature on the panel, thermometer offset, hostname; the local-time line, for checking a rule | `POST /settings` |
| `/ha` | the MQTT broker, port, user, password; the MQTT line with the last error | `POST /ha` |
| `/system` | firmware upload, API token, reboot, access point: one PIN field, four actions | `/ota`, `/token`, `/reload`, `/ap` |

Every page starts with the same line of links. On the access point
`/` stays the setup form, with a link to `/system`; `/settings` and
`/ha` are not served there (the captive redirect takes them), and
`/system` leaves out its access-point button.

**A form post is a partial update.** `form::apply` already changed
only the keys it was sent, which is what `/api` relies on; the one
exception was `checkboxes_absent_off`, which turned `armed`, `skip`
and `twelve_hour` off for a page form that did not send them, since a
browser sends nothing for an unticked box. With 12 hour on another
page, saving the alarm would have cleared it. So the flag is gone:
each checkbox is preceded by a hidden input of the same name with
`value=0`, the later pair wins as it always did, and a form carries
only its own fields. The path a form is posted to decides nothing but
where the redirect returns (`/ha?error=broker`); all three take
`Allow::PAGE` and the broker-password rule is as it was.

**The display is applied as chosen.** Colour and brightness are their
own form, and a script posts the changed select to `/api` at once, as
the buttons on the clock do; the alarm's Save does not carry them, so
a colour stepped on the clock while the page was open is not put back
by it. Without script the form has a Save of its own (`<noscript>`).

**`/system`, one PIN.** `/update` and `/token` each issued a PIN when
opened, and a PIN is one action; a hub linking to both would have put
a PIN on the panel twice. `/system` issues one when opened and has the
file and Upload, the new token and Set token, Reboot, and Access
point; any answer spends the PIN and disables the buttons. `GET
/update` and `GET /token` are redirects to it, so the README of an
older release, the web installer's text and bookmarks still land.

**Reboot and access point take the PIN.** `POST /reload` and `POST
/ap` took the token alone, which a page does not have; they take
`auth::token_or_pin` now, as `/ota` and `/token` do. The PIN is
someone who can see the panel, which is what holding both buttons for
ten seconds is. They are not left open: the access point takes the
clock off the LAN, and `/api` there takes the network keys.

**The token page is linked.** It was reachable by its address only,
on purpose, and the tests asserted it. The user, 2026-11-14: "for the
open source project it doesn't make much sense to have it unlinked".
It is a section of `/system`, behind the PIN as before.

Checks on the host: each page at its worst against `PAGE_MAX`; each
form holds its own keys and no other page's; a checkbox off and on
through the hidden pair, and an absent one left alone. On the clock,
for the user: the four pages in a browser, a save on each with 12
hour on staying on, the colour following the select, the PIN flow of
each of the four actions.

**Save under both, 2026-11-14.** The user, of the first build: "Save
button is in the wrong place in the main page" (it was inside the
alarm's box, the display a second form under it). The clock page is
one form again, Save under both fieldsets as on the other pages; the
display is still posted to `/api` as chosen, and the script makes the
choice the select's default so the mark does not call it unsaved.
Save carries the colour and brightness too, as it did before the
split, so one stepped on the clock while the page was open is put
back by it. The system page's PIN line is bold, at the user's word,
and its texts were cut: the token's rules are the README's.

**Sizes, 2026-11-14,** with the three clips of `clips/`, each page at
its worst: clock 4353, settings 2433, Home Assistant 2253, system
3756 (4038 before its texts were cut the same day), of 6016. The one page was 5564, and 5865 with nine clips. The
main stack is 50.4 KB (`.stack` 0xc4c8), as it was within 0.2 KB. 139
host tests.

**On the bench, 2026-11-14.** Test-clock, by OTA with the token, the
build with four clips. With curl: the alarm form with `armed=0&armed=on`
arms and with `armed=0` alone disarms, `twelve_hour` set before it
staying on through both; the settings form's `twelve_hour=0` turns it
off and leaves the alarm's boxes; a bad timezone comes back as
`/settings?error=value` with the notice; a broker moved without its
password as `/ha?error=broker` with the host unchanged; `/reload` and
`/ap` without credentials and with a wrong PIN are `403`; `/update`
is a `302` to `/system`. In headless Chromium over the DevTools
protocol, real clicks: the colour select posts `color=green` to `/api`
and the panel's setting follows, without the unsaved mark; an edited
alarm time shows the mark; Play posts `/play` and Stop `/stop` without
leaving the page; Save with Skip ticked posts
`armed=0&…&skip=0&skip=on&…` and unticked `…&skip=0&…`, and `/api`
agrees each time; the browser-zone button on `/settings` fills the
rule. On `/system`, with the PIN read from the serial log: Reboot
without a PIN asks for the digits; with it the clock reboots and the
page says `Back up`; Set token answers `token set`; Upload writes the
image to the other slot, the progress bar at 1, and the page waits
for the clock; every button is disabled after an answer. In a window
500 px high the status line, out of view from the Reboot button, is
brought into view by a message (`scrollIntoView` in `say`).

Not run: the Access point button (it takes the bench clock off the
LAN), the pages on the access point (the system page without its
button, the setup form's link), the no-script Save of the display.
For the user: those, and the four pages on white-clock and a phone.

### Cross-site requests refused: `Host`, `Origin`, no framing — built and run on test-clock 2026-11-14

The user, 2026-11-14: "we don't have a protection against CSRF, so any
site on internet can send a request to the clock. Also it looks like
we are not checking Origin and Host headers." True of every route
without a credential: a form post is a request a browser sends across
origins with no preflight, so a page anywhere could make a browser on
the LAN post to `/`, `/settings`, `/ha`, `/api`, `/play`, `/stop`,
`/pin` and `/scan`. The four routes behind `Authorization` (`/ota`,
`/token`, `/reload`, `/ap`) were closed to that already, since the
header forces a preflight and the clock answers no `OPTIONS`; but not
to DNS rebinding, where the attacker's name comes to resolve to the
clock, the browser calls it the same origin, and the PIN can be
guessed at one try in ten seconds with `/pin` issuing new ones.

| what | rule |
| --- | --- |
| `Host`, every request | absent (not a browser), the clock's own IPv4 address, or on the station the hostname bare or with `.lan` or `.local` (the user's list, "for now"); a port and a trailing dot are dropped, case ignored. On the access point the address only, since the captive DNS answers every name |
| another `Host` | `GET`: `302` to `http://<ip>/`, so a LAN with another domain still gets there by address and a rebound page is sent off its origin; on the access point this is the captive portal. `POST`: `403` |
| `Origin`, every `POST` | absent is allowed (`curl`, the scripts); present it must be `http://` and a name the `Host` rule accepts; anything else, `null` included, is `403` |
| framing | `X-Frame-Options: DENY` on every response `status_line` writes, or a click in a frame would be a same-origin post |
| `GET /system` | issues no PIN any more: an `<img>` on any page could put digits on the panel and take a PIN in use away. The page's script posts `/pin` as it loads, a same-origin post the rule above passes, and says so if the clock is locked out |

The rule is `http::admit` in `core`, pure, with the clock's address
and hostname passed in; `parse_head` keeps `Host` and `Origin` and
refuses a head with either twice. `Sec-Fetch-Site` is not used: a
browser sends it only to HTTPS and localhost. A refused request does
not count as served, so it does not confirm a new image. A refused
upload drains its body first, as a wrong token does.

The headers `status_line` writes were 103 bytes for a page, not the 88
the comment said (it predates `Cache-Control`); with the new one they
are 126, so `PAGE_MAX` leaves 160 of the buffer and not 128.

Checks on the host: `admit` over each accepted form and the refusals
(`clock.evil.example`, `clock.lan.evil.example`, a name on the access
point, `null`, `https`, another address). On the clock, for the user:
`scripts/verify-csrf.sh <ip>`; the four pages by address and by
`white-clock.lan`, a save on each, Play, the colour select, an upload
from `/system` with the PIN the page asked for; `scripts/ota.sh` and
`scripts/reload.sh` by name.

**On test-clock, 2026-11-14** (`v0.4-29-g88b0b7a-dirty`, by OTA with
`scripts/ota.sh test-clock.lan`, so a script by name passes):
`scripts/verify-csrf.sh 10.1.1.30`, all 26 checks. By hand with
`curl`: `test-clock.lan` and `TEST-CLOCK.local:80` as `Host` are
answered, `test-clock.lan.evil.example` and `10.1.1.31` are `302`, a
request with no `Host` is answered, a page by name may post to the
address. `GET /system` put no PIN in the serial log, with the clock's
`Host` or another; `POST /pin` did. `scripts/reload.sh` by name.

In headless Chromium, a page served from `http://localhost:8765` as
the other site: a form posted to `/api`, `/` and `/settings`, and a
`fetch` with `no-cors` to `/play` and `/pin`, each `403` with `refused
by its Origin … origin Some("http://localhost:8765")` in the log and
the settings as they were; the clock's page in an `<iframe>`, `200`
and then `ERR_BLOCKED_BY_RESPONSE`, the frame empty; `<img
src=…/system>`, no PIN. The clock's own pages in the same browser: the
colour select to `/api`, Save on `/settings` (12 hour on and off
again), Play and Stop, and `/system` loading with its `POST /pin`
answered `200` and a PIN in the log. With that PIN, `POST /reload`
under a foreign `Origin` was `403` and left the PIN unspent (the rule
comes before the credentials); under the clock's own it rebooted. The
settings were left as found.

The first run of the browser test sent six requests to the clock at
once and three were refused connections, not `403`: three listeners,
as ever, and no part of this change.

The upload from the system page, same day, in headless Chromium with
the monitor attached: the page loaded, its `POST /pin` put a PIN in
the serial log, the script typed it, chose `target/esp32clock-ota.bin`
and clicked Upload; `updated: … written to ota_1` after 56 s, `Back
up: firmware … in ota_1` at 63 s, and the log's `confirmed good`.

Not run: anything on the
access point (the host would leave its LAN): a phone's sign-in sheet
now gets a `302` for `/` too when its probe names another host. For
the user.

### Clips in a partition of their own: the clip pack — DONE, run on test-clock and verified by the user on white-clock 2026-11-14 (upload from the browser, the clips heard); the access point and the release workflow not yet run

The user, 2026-11-14: "move clips into a separate partition on the
flash. Carve out 0.5M out of each app partition and create a new 1M
partition with clips. The legacy way to store the clips (built-in into
the app) should still be there. HTTP should get a separate page to
upload new clip pack. The setting must be backwards-compatible with
older partition table, i.e. app checks whether 'clips' partition
exists, whether its contents is valid and loads it only in this case."
A default pack (Trantor and the two modules), a local one not to be
committed (those, the SID tune, Futurama, the three PT3 modules), the
default one built by the CI and attached to a release as its own file.

**The table.** `ota_0` and `ota_1` shrink from 0x1E0000 to 0x160000
(1408 KB; the release image is about 1040 KB) and `clips` takes the
megabyte they give up:

| name | type | offset | size |
| --- | --- | --- | --- |
| `ota_0` | app | 0x20000 | 0x160000 |
| `ota_1` | app | 0x180000 | 0x160000 |
| `clips` | data, `undefined` (0x06) | 0x2E0000 | 0x100000 |

The subtype is one esp-bootloader-esp-idf 0.6 knows: its
`partition_type()` unwraps the conversion, so a custom subtype (0x40
and up) would panic every image that walks the table, the released
ones included. The partition is found by its label, `clips`. A clock
gets the new table only over USB or from the web installer; an OTA
leaves the old one, with `ota_1` at 0x200000, and the firmware must
run on either.

**The pack** (`core/src/pack.rs`, made by `scripts/mkpack.py`):

| at | what |
| --- | --- |
| 0 | `CLPK` |
| 4 | version, 1 |
| 5 | number of clips, 1 to 16 (`pack::MAX_CLIPS`) |
| 6 | two zero bytes |
| 8 | length of the whole pack, the hash included, little endian |
| 12 | an entry of 64 bytes a clip: offset and length of its data (u32 each), flags (bit 0: made to loop), three zero bytes, the API name (20 bytes, NUL padded, `a-z` and `_`), the label (32 bytes, NUL padded, UTF-8 without control characters and without `< > & "`) |
| after | the clips' data, each as `ALARM_CLIP` takes it |
| end | SHA-256 of everything before it, 32 bytes, as espflash ends an image |

`Pack::parse` checks all of it: the structure, every entry inside the
data, names unique and not a note melody's nor `random`, each clip a
second of sound at least and not of an old format (what the `const`
asserts ask of a built-in clip), and the hash. Sixteen clips with the
longest names and labels must fit the clock page and the discovery
payload; the tests measure both with such a pack.

**Reading it.** The players read a clip in place as `&'static [u8]`,
so the partition is mapped: at boot `src/pack.rs` reads the table,
finds `clips`, and writes its 64 KB pages into free entries of the
flash MMU (the sixteen below the last, 0x3C6F0000 up for an aligned
megabyte; refused if any of them is in use), as esp-storage does for
its encrypted reads: the entry, then `Cache_Invalidate_Addr`. Then the
header through the mapping, `Pack::parse`, and `melody::install`. The
hash is the software `sha2` the upload already uses: about 0.3 s for a
full megabyte by estimate, 50 ms for the default pack, logged at boot.
All of it before the settings are loaded, since a record's melody
index is clamped to the table.

**The melody table at run time.** `melody::MELODIES` was a constant.
Now: the nine note melodies, then the clips of the pack if one is
installed, else the built-in ones (`ALARM_CLIP`, unchanged, the three
of `clips/` by default, so a clock updated over the air keeps its
sounds). A valid pack replaces the built-in clips and is not added to
them: the default pack is the same three. `melody::count()`, `get`,
`all`, `clip_data`, `clip_loops`; `RANDOM` becomes a fixed value out
of any table's reach (it was the table's length) and `is_valid` holds
the index under `melody::LIMIT`, the largest table there can be. The
registry is a `critical_section` cell, and a thread-local in the
tests, which run in parallel. The clip ring (16 KB) is in every build
now, since any build may meet a pack; `HAS_CLIP` goes.

A stored melody is an index, as before: a pack with other clips, or
their order changed, changes what the index means, as a build with
other clips always did.

**Upload.** `POST /clips`, the pack as the raw body, `Bearer` or `PIN`
as `/ota`; `GET /clips` is the fifth page, "Clips": what is playing
now (pack or built in, the clips' labels, the room used) or why not
(no partition in this clock's table: flash over USB or the web
installer), the PIN field, the file and Upload. The script is the
system page's, its shared part split off. The upload: the credentials,
the partition, the size, the first sector's header (magic, version,
the length against `Content-Length`); then the pack is taken out of
the table (`melody::install(None)`), a clip playing from it stopped
and waited for, the scan stopped, and the body written a sector at a
time with read-back and hashed as `ota::install` does, with its
buffers. Then the mapped range's cache is invalidated and the pack
parsed in place: one the clock would refuse at boot is refused now,
its first sector erased, with the reason. On success the clock reboots
(`heal::reboot`), so the settings, the pages and Home Assistant's
options all see the new table from a clean start. A failed upload
leaves the built-in clips until the next one.

`/api` gets a read-only key `clips`: `pack` or `builtin`.

**Scripts and CI.** `scripts/mkpack.py out file[:loop|:once][=Label]…`
names and labels as `core/build.rs` does. `scripts/clips.sh <ip>
[pack]` uploads with the token (the default pack built if none is
named). `packs/` is ignored by git: `packs/clips-default.bin` and the
local `packs/clips-local.bin`. A host test parses the file
`CLIP_PACK_FILE` names, so the script's output is checked by the
firmware's own parser; the CI builds the default pack, runs that test
and publishes the file beside the OTA image; a release attaches
`esp32clock-clips-<tag>.bin`. The web installer does not write it:
the default build has the same three clips built in.

Checks on the host: the pack round trip and every refusal, the table
with and without a pack, the pages and the discovery payload with a
full pack, both packs through `CLIP_PACK_FILE`. On a clock, for the
user: the new table over USB, the boot line, `/clips`, an upload, the
melodies on `/`, a preview of each; an OTA of this build onto a clock
with the old table, which must say there is no partition and play the
built-in three.

**Changed while building, 2026-11-14.** The user: "we now should
account for alarm melody if the clips are missing, since there is no
built-in clips anymore, only beeper melodies. In this case, it should
be set to 'beeps'". So `ALARM_CLIP` is empty by default
(`DEFAULT_CLIPS`), the clips of a default build are its pack alone,
and `Settings::from_record` reads a melody past the table's end as
index 0, the beeps, where it used to land on the last melody. The
value is in memory only until something is saved. With no clips built
in, the web installer writes the default pack too (a fifth part at
0x2E0000, which replaces a pack the clock had), and the CI runs the
host tests a second time with the three clips built in, since the
players' tests over real clips have none otherwise. The session's
`.env` still exported `ALARM_CLIP` with Futurama: that build is
1476 KB and does not fit the 1408 KB slot; the builds here were made
with the variable unset.

Sizes: the image 877 KB (1045 KB with the three built in), the main
stack 49.9 KB, the default pack 171 862 bytes, the local one (eight
clips) 700 557. Pages at their worst with a full pack of sixteen: the
clock page 5443 of 5984, the clips page 3631, the discovery payload
4542 of 4608.

**On test-clock, 2026-11-14** (`v0.4-31-gd90b57d` plus this work):

- By OTA onto the old table (the clock had Futurama built in and
  `melody` at it): booted from `ota_1` at 0x200000, `clips: no
  `clips` partition in the table: 0 clips built in`, `melody` read as
  `beeps`, `/clips` says the table has no partition and has no upload,
  `POST /clips` is `409`.
- Over USB with the new table: `clips: partition at 0x2e0000, 1048576
  bytes, holds no pack` (the old `ota_1`'s bytes are there).
- `scripts/clips.sh test-clock.lan`: 10 s for the default pack, then
  `clips: pack of 3 clips, 171862 of 1048576 bytes at 0x2e0000, mapped
  at 0x3c6f0000, checked in 119 ms`, `"clips":"pack"`. Each of the
  three previewed with `/play`: no underruns, no stray positions.
- The local pack over it while Trantor was playing: the clip was cut,
  31 s for 700 KB, `checked in 482 ms` at boot; the SID tune, Futurama
  and the three PT3 modules previewed, clean lines for each.
- Refused with the pack in use left alone: 20 KB of noise (`not a
  clip pack`), the pack cut at 500 000 bytes (`length mismatch`), a
  wrong token (`403`). A pack with one bit flipped is written, fails
  its hash, the clock goes on with no clips, and the next boot says
  `the pack at 0x2e0000 is refused: Hash`.
- With the PIN (`scripts/pin.sh`, `Authorization: PIN`): the local
  pack again, `200`. The scripts of `/clips` and `/system` pass `node
  --check`. The clock was left with the local pack and `melody` back
  at `futurama`, which a software reset kept.

The boot's hash runs at about 1.4 MB/s, not the 3 MB/s guessed: half
a second for the local pack, before the panel shows the time. The
chip's SHA accelerator would shorten it; not done.

Not run: the clips page in a browser (the PIN field, the progress
bar, the wait for the clock to come back); the sound, on a clock with
a buzzer; the web installer's fifth part and the release workflow,
which run only on a tag; the access point.

**The partition grown to the flash's end, 2026-11-14.** The user,
reviewing the table: the 128 KB after `clips` (0x3E0000 to 0x400000)
belonged to no partition. `clips` is 0x120000 now, 1152 KB, eighteen
pages mapped from 0x3C6D0000; the firmware takes the size from the
table, so only `partitions.csv` and `scripts/mkpack.py`'s limit
changed. The 56 KB between `otadata` and `ota_0` stays unused: an app
slot starts on a 64 KB boundary. Done before any release, so no clock
but test-clock ever had the 1 MB table.

On test-clock, the same day: the table over USB, `clips: pack of 8
clips, 700557 of 1179648 bytes at 0x2e0000, mapped at 0x3c6d0000`, the
pack and the settings as they were. A pack of 1 142 830 bytes (the
local one with Futurama twice) was written in 53 s, checked, and its
last clip, which lies past the old megabyte, previewed. The local
pack was put back after.

**On white-clock, the user, 2026-11-14** (`v0.4-34-gd24c81b`, the new
table over USB): the local pack uploaded from the Clips page in a
browser with the PIN from the panel, and the clips listened to on its
buzzer. Read back from the session with `GET`s only: `"clips":"pack"`,
"Pack in flash: 8 clips, 685 KB of 1152", the eight labels, and
`melody` still `keygen`, at the place it had when the three clips were
built in. That closes the page in a browser and the sound; the access
point, the web installer's fifth part and the release workflow remain.

### The thermometer held while Q1 is hot — measured on white-clock 2026-11-14, the hold built and run there 2026-11-15; a short preview and the serial log's lines not yet seen

The user, 2026-11-14: with the buzzer mod (flyback diode, R5 at
5.1 k) Q1 gets hot while a clip plays through the PDM converter, and
it sits next to the NTC, "so it affects the temperature it measures".
Cold when idle, on the beeps and on Trantor (a beeper clip, 512
kbit/s). The buzzer's coil is 32 Ω. A 2.2 kΩ resistor from base to
emitter did not change it.

Why, by reasoning and not by scope: the converter's bits are 125 ns
(8 Mbit/s) to 250 ns, and the stage's edges are no shorter (under a
milliamp of base current against the collector-base capacitance, and
nothing but recombination or the 2.2 k to turn it off), so the
transistor sits half on with the coil's mean current through it:
about 78 mA at 2.5 V, the worst point, V²/4R = 0.2 W for 32 Ω.
Inside an S8550's rating; the pin's drive strength has no part in it,
R5 sets the current. The README's buzzer mod says so.

**Measured, white-clock over HTTP** (`v0.4-34-gd24c81b`, offset −2.5,
`temp_c` of `/api` every 5 s; the alarm set a minute ahead with
`days=all`, Halftime looped for the ring's five minutes, the settings
put back after):

| | reading |
| --- | --- |
| before, two minutes | 24.3 falling to 24.1 °C |
| 30 s into the ring | 24.3 |
| 1 min | 26.0 |
| 2 min | 28.8 |
| 3 min | 30.7 |
| 5 min, the ring's end | about 33 |
| the peak, 30 s after the end | 33.3, so +9.2 °C |
| 2.5 min after the end | 28.8 (half the rise gone) |
| 6 min | 26.3 (a quarter left) |
| 10 min | 25.2 |
| 14 min | 24.5 |
| 16 min | 24.3 |
| 20 min | 24.0 |

The rise had not levelled off at five minutes; the ring's timeout is
what bounds it. The fall is not one exponential: the halvings take
about 155, 190 and 215 s. Before the test the clock was already
cooling from modules the user had played (their length not known):
25.9 to 24.3 in nine minutes.

**Plan, not built.** Hold the temperature while a converter clip
plays and for a time after it; beeps and beeper clips do not start a
hold.

- `src/clip.rs` knows what it plays (`Stream::converter`): it keeps
  the seconds of converter clip played, in an atomic, bumped as the
  chunks are written.
- The rule is pure, in `core/src/sensors.rs` with its tests: from the
  seconds played and the seconds since the last of them, whether the
  reading is held. First figures: a hold of four times what was
  played, at least two minutes and at most twenty (five minutes
  played is twenty held, where the reading was back within 0.2 °C).
  The short end is a guess: only the five-minute case is measured.
  Plays within a hold add up.
- `sensor_task` leaves the NTC's average and `NTC_MV` alone while
  held (the LDR goes on), so the panel, `/api` and the MQTT state
  keep the reading from before the clip, and the average starts again
  from that when the hold ends. The heartbeat's millivolts stay raw,
  for calibration.
- A software reset loses the hold; the reading is then hot until it
  cools. Not carried: a reset in the twenty minutes after an alarm is
  rare.
- To check on white-clock: the same five-minute ring with `temp_c`
  logged, flat throughout; a 30 s preview the same way, for the short
  end; a stock board, whether it heats at all.

**Built, 2026-11-15**, as planned but for two things. The rule is
`sensors::Hold`, told once a reading (a second) whether
`clip::converting()`, a flag the clip task sets while a converter
clip is on the line; it counts for itself, so the clip task keeps no
seconds. And the NTC's average is not frozen: it goes on underneath,
only the published cell is held, so the reading steps to the live
average when the hold ends and the log line shows the live
millivolts with `held, N s after the clip`. `thermometer held for a
clip's heat` and `released` are logged at the edges. Host tests: the
hold's length at 1, 30, 60, 300 and 3000 s played, and two plays
adding up.

**On white-clock, 2026-11-15** (`v0.4-36-g7b0e0c6-dirty` over the
air, built without `ALARM_CLIP`; the same script: Halftime rung for
its five minutes through the alarm, `temp_c` of `/api` every 5 s, the
settings put back). The reading was 24.1 from half a minute before
the ring to 1200 s after its end, 310 samples alike, and moved again
at the next sample, to 24.3: no step at the release, the board cool
by then. The run without the hold peaked at 33.3. Not seen: the
log's `held` and `released` lines (no serial port on this clock), a
short preview's hold, a reset during a hold. Outside the hold the
reading moved by 0.1 to 0.3 between samples, with single ones 0.4
up, where the first run's moved by 0.1; the average's code is as it
was, and the cause was not looked for.

### The pages after a UX review: PIN at the click, a saved notice, the bad field named — built 2026-11-15, run on test-clock in headless Chromium; the setup form the user's, 2026-11-15; a real upload from the page not tried

A read of the five pages and the setup form as served (white-clock,
test-clock; markup and scripts, nothing clicked) gave twenty findings;
the user took the first group, 2026-11-15. The rest are in the review
backlog's terms below, not built.

1. **Enter in the alarm form played the tune.** A form's default
   button is its first submit button in tree order, which on `/` is
   Play. The script now makes Play and Stop `type=button` (they stay
   submit buttons with `formaction` without script), so Enter saves.
2. **A stale tab put the old colour back.** The alarm form posted
   colour and brightness too. It is still one form with Save under
   both fieldsets (a first build made the display a form of its own,
   which put Save between Alarm and Display; the user: "It should go
   after Display"). The script takes the names off the display's
   selects, kept as `k` for the post to `/api`, so Save posts the
   alarm only and the unsaved mark does not count them; without
   script Save posts both, as before. `skip` and `armed` in a stale
   tab are as they were: any form has that.
3. **A failed post looked done.** `P` now answers a promise that
   fails on anything but a 2xx or the redirect Play and Stop answer
   with; a display change that failed puts the select back, and all
   three say `The clock did not answer; nothing was changed.` in a
   line above Save.
4. **The PIN at the click, not at the page's load** (`/system`,
   `/clips`). Opening the page drew a PIN whether or not anything was
   to be done, the digits left the panel after a minute, often before
   a file was chosen, and any answer, a wrong digit included, disabled
   every button until a reload, which dropped the file and the typed
   token. Now: the action's button checks what it can (a file chosen,
   the token's rule, the two `confirm()`s), posts `/pin`, and a small
   form (the PIN field, OK, Cancel, the status line under it) is moved
   under that button. OK sends the request with the PIN; after any
   answer the buttons are enabled again and the next click draws a new
   PIN, so nothing says "reload this page" any more. A wrong PIN's
   ten-second lockout is what `/pin` then answers, shown as it is. The
   routes and `auth` are untouched.
5. **`Saved.`** A good save redirects to the page with `?saved=1` and
   the page says so above the form (`form::Note`); the script takes
   the query off the address, so a reload does not say it again.
6. **The bad field named.** `form::apply_naming` answers the key that
   failed with the error; the redirect carries it (`?error=value&key=tz`)
   and the page says `bad value for <b>Timezone</b>` from a table of
   the pages' keys and labels (`form::FIELDS`), never the query's own
   text. What was typed is still lost with the redirect; the browser's
   Back has it.
7. **The setup form's password can be seen**: a `Show the password`
   checkbox, unnamed so it is not posted, shown by the script that
   makes it work.

Not verified without a browser: 1 is from the HTML standard's implicit
submission, not seen. To try on test-clock (headless Chromium) and by
hand: Enter in the alarm time saves; colour changes with the clock
unplugged revert and say so; `/system` shows no PIN until a button is
clicked, a wrong PIN leaves the file chosen, and the second try works
after the lockout; a bad timezone rule names the field; the setup
form's checkbox.

**Built and run, 2026-11-15** (`v0.4-37-g61c15f6-dirty` on test-clock
over the air, headless Chromium with real key and mouse events, the
PIN read from the serial log; the settings as found afterwards).
Seen: Enter in the alarm time posts `/` with the alarm's five keys
and no colour, comes back to `/?saved=1`, the page says `Saved.` and
the address is `/` again; Play and Stop post `/play` and `/stop`; a
colour change posts `/api` and leaves the unsaved mark hidden; with
the browser offline the select goes back and the line shows, and
hides at the next good post; with the colour set to green behind the
tab's back, Save left it green; `tz=nonsense,` comes back as `bad
value for Timezone` with the rule unchanged. On `/system`: no `/pin`
at the load; a short token and Upload without a file are refused in
the page with no PIN drawn; Reboot posts `/pin`, the form appears
under the button with the focus in it and the actions disabled; a
wrong PIN is `403: wrong PIN.` with the buttons enabled and the token
field kept; a click at once is `locked out for 9 s after a wrong
PIN.`; Cancel; then the right PIN reboots and the page says `Back
up` with the buttons enabled. Not tried: a firmware or pack upload
from the page (the request is `go`'s, as Reboot's), the access-point
button, the setup form's notices and its `Show the password` (the
host cannot join the access point), a phone.

Also the user's, 2026-11-15: the upload's label names the release's
file with a link to the releases page, not `scripts/ota.sh`, since
"the end user is very unlikely going to build the firmware"; the
clips page's label the same way, with `scripts/mkpack.py` after it.

Left from the review, not built: the token field is a password field
though the token is to be copied; the MQTT line is stale right after
a save; no dark colour scheme; one title for five pages; `HA` and
`General` as link names; the alarm's days are three presets; no
"next alarm" line; `/ha` does not say whether a password is stored;
the status line has no `role=status`; Rescan was not traced for what
it keeps of the typed fields.

### The rest of the UX review — built 2026-11-15, run on test-clock in headless Chromium; the setup form the user's, 2026-11-15

The user, of the findings left above: "do all". What each becomes:

- **Typed input kept on a refusal** (9). The script of a settings form
  posts it with `fetch` and follows the redirect: a refused form's
  notice is taken out of the answer and put above the form, which
  stays as typed; a saved one goes to the page the redirect named.
  If the request fails the form is submitted the plain way.
- **The token field is text** (6): it is copied into scripts.
- **The MQTT line follows the connection** (10): `/ha`'s script asks
  `/api` every 5 s for two minutes and reloads the page when `mqtt`
  is no longer what the page was served with, unless the form has
  unsaved changes.
- **Dark colour scheme** (13): `color-scheme:light dark`, the error
  colour by `light-dark()`.
- **A title a page** (14): `<hostname>: <page>` with the page's full
  name; the heading stays.
- **The links** (15): the names stay `General` and `HA`, the user's
  of 2026-11-14 for one line on a phone; the links get height to tap
  (`nav a`).
- **Seven day checkboxes** (16) in place of the three presets. They
  are all named `days`, so `apply` now reads a `days` key repeated in
  one body as a union (the first replaces, the rest add); `/api`'s
  `days=mon,wed` and the three words are as before. None ticked
  sends no key and changes nothing: an alarm for no day is the
  armed box.
- **`Next alarm: Mon 07:00`** (17) in the alarm's fieldset, from the
  page's local time; with skip set, the one after and which is
  skipped. Nothing while disarmed or before the time is known.
- **Whether a broker password is stored** (18), in the field's label.
  Going from a password to none on the same host still needs the
  host cleared first: a key for it was not thought worth a place in
  the API.
- **Wording** (19): the colours capitalised, `Hostname, the clock's
  name on the network`. The offset stays `°C`, which it says.
- **`role=status`** (20) on the PIN pages' status line, `role=alert`
  on the clock page's.
- **Rescan keeps what was typed** (12): traced, it did not (the
  answer is a redirect to the form as stored). The script posts
  `/scan` itself and takes only the list out of the answer, keeping
  the pick if it is still listed.

The clock page had 144 bytes left at its worst; this needs some 700.
`RESPONSE_BUF` goes from 6 to 7 KB, 3 KB of the main stack for the
three listeners.

Added by the user while it was built: the status lines of the clock
page set closer (`.st p`, 0.2 em between them), and the `Scriptable:`
line gone from it and from the setup form, "it should be in the
documentation, not in the interface".

**Built and run, 2026-11-15** (`v0.4-38-g368c53a-dirty` on
test-clock; the settings as found afterwards). `RESPONSE_BUF` at
7 KB leaves the main stack 46.8 KB (49.9 before); the clock page is
5485 bytes and 6701 at its worst, of 7008. Seen in the browser: the
titles (`test-clock: Settings`); the links 34 px high on one line;
seven day boxes, two unticked and saved as `mon,wed,thu,fri,sat`,
then put back; `Next alarm: Mon 01:00.` on a Sunday afternoon with
Sunday and Tuesday unticked, `Wed 01:00 (Mon skipped)` with skip, and
no line while disarmed; a bad timezone refused with the notice above
the form, the field still `nonsense,` and another field's edit still
there, the address unchanged; a second refusal replacing the first
notice; then saved. `/ha` served while connecting to a wrong port
loaded itself again 4 s after the port was put right and said
`connected`. The token field is text. Under a dark scheme the text
is white and the notice `#f66`; under a light one black and `#b00`.
No script error on any page. Not tried: the setup form (Rescan by
script, the title `Setup`), a form's post with the clock out of
reach (the fallback to the browser's submit), a phone's width.

The setup form, the user on the access point, 2026-11-15: "setup
page works as expected". What was looked at was not said, so the
notices, `Show the password` and Rescan are taken as seen and no
more than that.

Later the same day, the user's: the firmware section of the system
page says `Uploading asks for a PIN, which the clock then shows on
its panel.` before the dark-panel line, as the clips page does, and
`RESPONSE_BUF` is 8 KB ("bump max page size to 8192"): `PAGE_MAX`
8032, the main stack 43.7 KB. The clock page's worst stays 6701.

And the presets back, above the boxes (the user: "let's have presets
in addition to the check boxes as well, right above the check
boxes"): a select of Every day, Weekdays, Weekend and Custom with no
name, so only the boxes are posted; the script shows it, ticks the
boxes when one is picked and shows `Custom` when the boxes match
none. On test-clock in the browser: Weekdays and Weekend tick their
boxes and raise the unsaved mark, Friday ticked on top of Weekend
reads `Custom` and unticked `Weekend` again, the save posts
`days=sat&days=sun` and `/api` says `weekend`. The clock page is
5989 bytes, 7205 at its worst.

One thing seen and not explained: after this build's upload (8 KB
buffer, the presets) test-clock did not answer for three and a half
minutes and came back by a self-heal (`heals` 1, 13:01:50), which is
what three minutes of failed connects end in. No serial log was
running. The three uploads before it that day came back within 20 s,
and two `scripts/reload.sh` resets of the same image afterwards came
back in 38 s and 10 s. Watch the next uploads with the monitor on.

Links one colour (the user: the navigation links changed colour once
visited): `a{color:LinkText}` in the shared head, the browser's link
colour for either scheme. Not seen in a browser: a page cannot read
a visited link's colour back, so it is for the eye.

Play and Stop each half the row with a 1 em gap (the user found them
inconsistent beside Reboot and Access point, which sit at the row's
two ends; they stay a pair, being one control, and now fill the row
as that one does).

The heading a fifth smaller and centred,
`h1{font-size:1.6em;text-align:center}`. The user found it "huge and
repeated on every page" and asked for half the size; seen at 1 em on
white-clock that "doesn't look good", and this is the second try.

Centred as well, the user's: the line of links, Save (the unsaved
mark now under it, so the button stays put when the mark shows) and
the status lines under a form, on the settings and Home Assistant
pages and the setup form too (`.c`, `.st`). The PIN pages' status
line, which answers a button, stays where the button is.

`Saved.` centred too (the user), and with it the refusals in the
same place and the clock page's "did not answer" line above Save.

The clips page warns, above the upload's small print, that a new
pack replaces all the clips and does not add to them (the user).


### A clip pack for the speaker — DONE: the second pack (limiter's release 128 ms) heard and kept by the user 2026-11-15

"Build a clip pack that targets speaker, for both standard pack and
local" (the user). The clips so far are shaped for the buzzer: the
bass cut at 300 Hz (900 Hz twice for PCM) and 20 dB of drive into a
limiter, which `speaker-mod/README.md` already says will be thin and
compressed on a speaker. What a speaker's clips should be was settled
on the breadboard's headphones on 2026-11-13, for the PT3 tunes: the
high-pass at 100 Hz once, no limiter, a plain gain 3 dB over the
peak, some 0.1 % of the samples at the rails ("sync_i_cut_louder").
The pack is every other clip brought to that:

| clip | for the speaker | RMS of full scale, at the rails | as the clocks have it |
| --- | --- | --- | --- |
| Trantor | as it is: the Spectrum's speaker bit | | |
| Halftime | `xm2clip.py --highpass 100 --passes 1 --knee 94 --drive 180` | 0.19, 0.02 % | 0.40, 1.6 % |
| Keygen | the same with `--knee 96` | 0.20, 0.02 % | 0.42, 1.8 % |
| Donkey Kong | `sid2clip.py --mute 2 --highpass 100 --passes 1 --flat` | 0.25, 0.09 % | 0.37, 1.6 % |
| Futurama | `CLIP_HIGHPASS=100 CLIP_DRIVE=10 wav2pcm.sh wav/futurama_theme.wav` (16 kHz, the whole of it) | 0.30, none | 0.48 |
| Hibernation, PPK, Synchronization | as they are: the converter's defaults are that choice | 0.21, 0.14, 0.21 | |

A module's knee is its own: the mix's peak after the 100 Hz
high-pass is 95 575 for Halftime and 97 880 for Keygen (the player's
output with `--knee 255`, a plain gain, scaled back), so 94 and 96
(in 1024s) put the peak at full scale, and the drive of 180 takes it
1.4 times over, as the PT3 clips' is. Without the drive they are
0.14, quieter than everything else in the pack. Futurama's 10 dB is
"Futurama D" of 2026-11-13, made for the hiss the user heard at 20 dB
and never judged; its bass is back (18 % of the energy under 150 Hz,
none before).

Two things had to be added for it. `scripts/wav2pcm.sh` reads
`CLIP_HIGHPASS` (a corner, applied once) and `CLIP_DRIVE` (dB); with
neither it does what it did. And the SID's header has no spare byte
for a knee as `XMC2` has, so it gets what `PT31` got: flags bit 1,
`sid::FLAT`, `scripts/sid2clip.py --flat`, for which the knee is three
voices at their top (`FLAT_KNEE`, 96) and the header's drive a plain
gain. Donkey Kong's filter passes that level in 0.09 % of the
samples, which are clipped. **A firmware older than this ignores the
bit and plays the clip limited**, with the bass in: the local pack's
Donkey Kong wants this firmware on the clock. The host test
`a_flat_clip_is_not_limited`: one voice's pulse wave at full level is
a third of full scale flat and the whole of it limited. 158 tests.

`scripts/speaker-clips.sh` makes the standard pack from `clips/` and
`mods/`: the clips in `packs/speaker/`, the pack
`packs/clips-default-speaker.bin`, 171 862 bytes, under the default
pack's names and in its order, so a melody setting means the same
tune. The local one, by hand, in `packs/clips-local.bin`'s order:

```
scripts/mkpack.py packs/clips-local-speaker.bin clips/trantor.bpr packs/speaker/halftime.xmc packs/speaker/keygen.xmc packs/speaker/donkey_kong.sidc packs/speaker/futurama.pcm pt3/hibernation.pt3c pt3/ppk.pt3c=PPK pt3/synchronization.pt3c
```

700 557 bytes. Both pass `the_pack_file_parses`. Neither is in the CI
or a release.

To hear, on a clock with the speaker in and this firmware:
`scripts/clips.sh <ip> packs/clips-local-speaker.bin`. What nobody
knows yet: whether a plain gain is loud enough for an alarm on a
32 Ω speaker (the clips are 3 to 6 dB under the buzzer's by RMS),
whether 100 Hz is too low a cut for a speaker that small (the
README's range is 100 to 300), and Futurama at 10 dB against 20.

**Heard, 2026-11-15: "They are much quiter now"** (the user, of the
pack above). A plain gain is 6 dB under the buzzer's clips and that
is too little. The level comes back from the limiter, with the knee
and drive the clocks have and only the release changed, so that the
gain follows the tune and not the wave: on the host, the cut at
100 Hz in all, RMS of full scale and the samples at the rails,

| tune | no limiter (the pack above) | release 128 ms (`--release 10`) | 32 ms (`--release 8`) | the clocks' clip (16 ms, 300 Hz) |
| --- | --- | --- | --- | --- |
| Halftime | 0.19 | 0.33, 0.33 % | 0.40, 1.1 % | 0.40, 1.6 % |
| Keygen | 0.20 | 0.32, 0.30 % | 0.42, 1.1 % | 0.42, 1.8 % |
| Donkey Kong | 0.25 | 0.33, 0.37 % | 0.41, 1.2 % | 0.37, 1.6 % |
| Hibernation | 0.21 | 0.34, 0.21 % | 0.43, 0.76 % | |
| PPK | 0.14 | 0.37, 0.13 % | 0.47, 0.74 % | |
| Synchronization | 0.21 | 0.31, 0.17 % | 0.40, 0.69 % | 0.37 |

The second pack is the 128 ms column: 4 dB over the first and 2 dB
under the clocks' clips. Modules `--highpass 100 --passes 1 --release
10` (`scripts/speaker-clips.sh`), Donkey Kong the same with `--mute
2` and no `--flat`, so it needs no newer firmware; the PT3 tunes
`--limiter --noise 16 --highpass 100 --passes 1 --release 10`
(`--restart` for Hibernation and Synchronization), in
`packs/speaker/` beside the rest; Futurama `CLIP_DRIVE=15`, 0.39.
"Limiter hurts PT3s" was said of the 16 ms release; whether 128 ms
does is to be heard, and 32 ms is the next step if this is still
quiet. `sid2clip.py --flat` stays, used by nothing in the pack. The
local pack:

```
scripts/mkpack.py packs/clips-local-speaker.bin clips/trantor.bpr packs/speaker/halftime.xmc packs/speaker/keygen.xmc packs/speaker/donkey_kong.sidc packs/speaker/futurama.pcm packs/speaker/hibernation.pt3c packs/speaker/ppk.pt3c=PPK packs/speaker/synchronization.pt3c
```

**Kept, 2026-11-15**: "Yeah, it is good now. Let's keep this
version" (the user, of the second pack). The speaker's clips are the
buzzer's with the high-pass at 100 Hz once and `--release 10`; the
PT3 tunes limited that way included.

### Synchronization in the default pack — 2026-11-16, built and tested on the host; the pack not on a clock yet

"synchronization can be converted and moved to clips and default
pack. I've got permission from n1k-o (Oleg Nikitin) to use it for this
project" (the user). The module is signed `nq`, 27.06.2015.

- `mods/nq-synchronization.pt3` is the module, copied from the ignored
  `pt3/`; `clips/synchronization.pt3c` is `scripts/pt32clip.py … --restart`
  with the converter's defaults, byte for byte the `pt3/` clip the
  clocks have heard since 2026-11-13 (`cmp`).
- It is the fourth and last clip of the default pack
  (`scripts/clips.sh`, the CI, the release), so the three before it
  keep their places and a stored melody its tune.
- The speaker's pack (`scripts/speaker-clips.sh`) has it as the kept
  pack of 2026-11-15 did: `--limiter --noise 16 --highpass 100
  --passes 1 --release 10 --restart`.
- `clips/README.md` credits n1k-o and says the permission is to this
  project. Hibernation and PPK stay in `pt3/`: nobody was asked.

### Wi-Fi transmit power as a setting — DONE, verified by the user on the bench board 2026-11-15

The user's, 2026-11-15: the bench board's ceramic antenna is poor and
it needs less than the 16 dBm the clocks run at (the working tree had
`WIFI_MAX_TX_POWER_QDBM` edited to 44 for it). Asked whether a build
variable or a stored setting, the user chose the setting: one image
for every clock.

- `Settings::tx_power_qdbm`, quarter dBm as the driver takes it, one
  of `settings::TX_POWER_CHOICES`: the driver's levels up to the
  ceiling, 2, 5, 7, 8.5, 11, 13, 14, 15 and 16 dBm (8, 20, 28, 34, 44,
  52, 56, 60, 68; the table in esp-wifi-sys's
  `esp_wifi_set_max_tx_power` doc). `config::WIFI_MAX_TX_POWER_QDBM`
  stays 68 and is now the cap: the table is cut at it, so nothing
  above it can be chosen, stored or read back, and `net.rs` clamps
  once more. What 18 and 20 dBm did is in the constant's comment.
- Record version 12, one byte appended after the timezone rule
  (`RECORD_LEN` 386). A byte that is not a listed level is a bad
  record, as the temperature byte is. An older record gets the
  default.
- The default is 68, or `WIFI_TX_POWER_QDBM` from the build, which a
  `const` assert holds to the listed levels: for a board that cannot
  join at 16 dBm before anyone has reached its page. Like the
  thermometer offset's default it is for a clock with no record or
  one older than version 12; a stored value stays.
- The key is `tx_power`, its value the dBm as the table names it
  (`8.5`, `16`), a number in the JSON. A select on `/settings` under
  Network. Not guarded by the token (it cannot go above the ceiling;
  too low costs the LAN, as a wrong hostname's users know, and the
  access point's hold brings the clock back) and not among
  `mqtt::SET_KEYS`: the bus is no way to take the clock off the
  network. No Home Assistant entity; the state JSON carries the key.
- The user, while it was being built: the page must say that leaving
  the default is dangerous, since the clock may not connect. First
  the access point was to put the default back; then the user changed
  that: the access point transmits at a fixed 11 dBm
  (`config::AP_TX_POWER_QDBM`, 44: "typically enough for a phone"),
  leaves the station's setting alone, and its setup form has the
  select, so a clock set too low is brought back by the hold and the
  form. The settings page's note says so. A save on the access point
  with a network stored resets into the station, as any does there.
  The settings page is 3812 bytes of 8032, the setup page without a
  list 3601 at its worst (the list's budget still fits: 4977 hostile).
- `net::wifi_task` applies it before every connect and looks again
  every `RSSI_PERIOD` (10 s), so a change needs no reset; the log
  line says the setting, as before. `ap_task` sets its own once.

To verify on the bench board: the boot line `wifi: max tx power 16
dBm`; pick 11 dBm on `/settings`, within 10 s the line again with 11;
`/api` has `"tx_power":11`; the access point's figure for the clock
drops; after `scripts/reload.sh` the first connect says 11. Hold both
buttons 10 s: `wifi: max tx power 11 dBm, the access point's`, the
setup form shows the select at 11; pick 16, Save and join, and the
station says 16.

Verified by the user on the bench board, 2026-11-15: the station's
page and the access point's both have the setting, it works as
expected, and with the power lowered the board, whose ceramic antenna
is poor, connects reliably.

### A SID tune on the other machine: `sid2clip.py --system` — DONE, heard by the user on test-clock's speaker 2026-11-15

The user, of Donkey Kong: "it might be playing a little bit slower
than I am used to", and which C64 the clock models. Neither: the
converter takes the machine from the file's flags (PAL unless it says
NTSC) and writes the chip's clock and the cycles a frame into the
clip's header, and `sid::Tune` takes pitch and tempo from those. The
file says PAL, called once a screen, and the clip had 985 248 Hz and
19 656 cycles a frame, 50.12 calls a second, 83.0 s: as a PAL machine
plays it. The clip's 31 250 samples a second are 8 Mbit/s exactly, so
the line adds nothing.

`scripts/sid2clip.py --system pal|ntsc` plays the tune as that machine
would whatever the file says (the clock, the screen's cycles and the
KERNAL's timer value all go by it); without it the file's flag, as
before, and the default made the old clip again byte for byte. A PAL
tune as NTSC is called 59.83 times a second, 19 % faster, on a chip
clock 3.8 % faster, two thirds of a semitone up: Donkey Kong is 69.6 s
and goes round from 31.1 s, the same 4162 frames and 62 216 bytes.
Nothing in the firmware changed.

**Heard, 2026-11-15**, both in one pack on test-clock's speaker, the
speaker pack with the NTSC one as a ninth clip: "NTSC is the one I
remember" (the user). The local clips are now NTSC under the old name
and label, so a melody setting means the same tune:

```
scripts/sid2clip.py sid/Donkey_Kong.sid sid/donkey_kong.sidc --mute 2 --system ntsc
scripts/sid2clip.py sid/Donkey_Kong.sid packs/speaker/donkey_kong.sidc --mute 2 --highpass 100 --passes 1 --release 10 --system ntsc
```

and `packs/clips-local.bin` and `packs/clips-local-speaker.bin` made
again from them, the same eight clips in the same order, 700 557
bytes each; test-clock has the speaker one. White-clock and
martin-clock have the PAL clip until their pack is uploaded again.

### Beeper clips packed: `BPR3`, repeats as references into the clip; SID tunes the same, `SID2` — DONE, on a clock with the rebuilt packs and accepted by the user 2026-11-16 ("Works nicely")

The user, 2026-11-16: Trantor is 145 KB of the default pack's 183,
store it smaller without losing anything, in a format other beeper
tunes can use.

**What was measured** (126.5 s, 112 604 runs):

- The runs are already tight one by one (1.3 bytes each as LEB128).
  gzip makes 58 KB of the file, zstd 57, xz 45; those need a window
  in RAM and a decoder of some size.
- The same flips in T-states and not rounded to 512 kHz ticks: 194 KB
  plain, gzip 49, xz 41, the LZ below 70 to 73. The rounding is not
  what keeps the runs from repeating. The tune is pulse trains whose
  periods are no multiples of each other, so the gap from one
  channel's pulse to the other's is new each time, and a bar played
  again is not the same runs. A delta from the last pulse of the same
  width (the channels told apart by their widths) did not help either
  (xz 40 KB): there are 235 widths.
- The player itself is some 750 bytes of Z80 and runs without the
  ROM, so the tune as a snapshot for a Z80 on the chip would be a few
  KB and exact. Proposed and not taken: the user chose the packed
  runs, "it is more generic" (any recording of a 1-bit line, whatever
  made it).

**The format.** `BPR3`, the total in ticks as before, then tokens:

| first byte | token |
| --- | --- |
| `0vvvvvvv` | a run of 0 to 127 ticks |
| `10vvvvvv`, a byte | a run of 128 to 16 511 (the 14 bits, plus 128) |
| `110nnnnn`, LEB128 | a reference: the next n + 2 runs are those decoded from the token that starts so many bytes before this one |
| `0xE0`, LEB128 | a longer run |
| `0xF0`, LEB128, LEB128 | a reference of more runs: the count, then the distance |

A reference is to the packed bytes, not to the output: the clip is
read in place from the flash, so going back costs a position and a
count to return by, and nothing is kept of what was played. The runs
referred to may be references themselves, to `BEEPER_DEPTH` (16)
deep, which is 128 bytes of the stream's state. A reference that
uses up its parent's count takes the parent's place on the stack, so
the depth is the nesting and not the length of a chain; and so a
reference may reach itself, which is the same few runs many times
over (a note held on one channel) at no depth. Levels are not stored:
off and on alternate by the count of runs put out, as in `BPR2`.

What a bad file may do: a distance of nothing or past the clip's
start, the depth passed, the bytes ending inside a token, references
that give no run going round. Each ends the clip there, looping or
not; the tokens read for one run are counted, so the last cannot hold
the clip task. A clip of nothing but empty runs would spin
`Runs::fill`, in `BPR2` as before this; a pack is uploaded with the
token or the PIN, and that is left as it was.

**The packer** (`scripts/zxbeeper.py`, `pack`): at each run the 256
latest places where a token starts with the same two runs, the one
that saves most bytes taken, and a run left as it is when the
reference from the next run saves more. A reference is to a token's
start, so a place inside what an earlier reference stands for is not
on offer; its own source is.

**Size.** Trantor 145 516 → 67 559 bytes, the default pack 183 KB →
105. By the steps, on Trantor: references that end before
themselves, three runs at least, a depth of 8: 75.4 KB (4 deep 84,
16 deep 73.7, 64 no better); reaching themselves 69.4; two runs at
least 68.5; the second look 67.6. Some 39 000 of the 112 602 runs
repeat nowhere and stay as they are, 58 KB of the first figure. A
Huffman code over those would take some 12 KB off and needs its
table of 3700 values in the file, most of what it saves; not done.

**Done.**

- `core/src/clip.rs`: `Beeper` reads both formats (`BPR2` stays
  playable: the user's packs and `tap/`), by an offset into the clip
  and a stack of `(return, left)`; `is_beeper` is either magic.
- `scripts/zxbeeper.py`: writes `BPR3`, checks its own output by
  unpacking it as the clock does, and given a `.bpr` in place of a
  tape packs that (or with `--plain` unpacks it) without SkoolKit;
  `--plain` writes `BPR2`.
- `clips/trantor.bpr` packed from the file as it was. Unpacked again
  by the script it is that file byte for byte, and the firmware's
  decoder gave the same nine million bytes of line for both (a pass
  and the start of the next, looping; a test of the day, not kept).
- Tests: each token by hand with references in references, one that
  ends inside another and one that reaches itself, the loop, the
  line against the same runs unpacked; a packer in the test over
  random runs with repeats, round trip; the bad files above; with
  `ALARM_CLIP`, a built-in beeper clip's runs against its header's
  total. 162 host tests.
- No released firmware reads a pack yet, so the default pack with a
  `BPR3` clip meets no firmware that would take it for PCM; a clock
  built from the tree before this would play it as noise, and wants
  the firmware first.

To hear on a clock: Trantor from the new pack, a preview and round
the loop; the serial log's `clip:` line for underruns and the time
spent filling the ring, which should be no more than it was.

**The same for a SID tune: `SID2`** (the user, 2026-11-16: are PT3s
and SID tunes packable this way, and if so add it). Built the same
day and tested on the host; the local packs made again with it.

- A PT3 module is not: the player goes about it by its pointers
  (patterns, samples, ornaments), so it would have to be unpacked
  into RAM, 6 to 11 KB of it to save 4 to 7 of flash (gzip makes
  3.8 KB of Synchronization's 11.2). The same for an XM module. Left
  as they are.
- A SID record is a stream read from front to back, and a tune's
  frames repeat: Donkey Kong, 62 216 bytes (4158 frames with writes,
  1590 of them different; gzip 10.1 KB, xz 6.6). With a frame as the
  unit of a reference 25.1 KB; with a write or a wait as the unit
  11.7 (three at least; two 12.6, four 12.5).
- `SID2`: the header as `SID1`'s, then the record with one more kind
  of first byte. `0x00`–`0x3F` a register and its value, `0x80`–`0xFF`
  the frames to wait, as before; `0x40`–`0x5F`, LEB128 a reference to
  n + 3 items (a write or a wait each) decoded from the token so many
  bytes back; `0x60`, LEB128, LEB128 one of more; the rest ends the
  tune. The loop offset is a token's place outside any reference,
  which the packer sees to by letting no reference run over it.
- The stack and its rules are `core/src/refs.rs` (`Refs`), which
  `clip::Beeper` now uses too; the packer is `scripts/packrefs.py`,
  which `zxbeeper.py` and `sid2clip.py` both import. `sid2clip.py`
  writes `SID2`, `--plain` `SID1`, and a `.sidc` in the `.sid`'s
  place is packed again as it is. `SID1` still plays.
- Tests: a packed tune by hand against the same tune plain, sample
  for sample and round the loop, a reference that ends inside a
  frame; the bad records. 164 host tests.
- Donkey Kong: 62 216 → 11 742 bytes. Unpacked again by the script it
  is the file it was, the loop offset with it; the player's samples
  for one pass of each are the same 2 174 010 bytes
  (`CLIP_PCM_DUMP`). `sid/` is not in the repository and no pack of the
  project's has a SID tune, so nothing shipped changes.
- Trantor packs to the same 67 559 bytes through `packrefs.py`.

### PCM clips at half the flash: IMA ADPCM, `ADP1` — the A/B heard on white-clock 2026-11-16 ("not bad", "way better than PCM 8k"); the format built 2026-11-16 and heard by the user on a speaker the same day ("audible, but acceptable"); the serial log's clip line read on test-clock; the buzzer through the clock's own decoder not yet

The user, 2026-11-16: would the references of `BPR3` and `SID2` (an
LZ) do for PCM too?

**What was measured** (the clips in `wav/`):

- No. lz4 -9, references with no entropy coder as `refs` is, takes
  0.6 % off Futurama at 16 kHz and 1 % off a rendered Halftime; gzip
  4 %, xz 6 %. Under 1 % of the 8-byte windows of a clip have been in
  it before: a bar played again is not the same samples. The samples
  are 7.7 bits each by their histogram and their differences 7.8 to
  8.0, the 900 Hz high-pass and 20 dB of limiter having left
  something near noise, so an entropy coder has nothing either.
- The lossy way, IMA ADPCM at 4 bits a sample, is 2:1 exactly. Its
  error on Futurama (16 kHz, the buzzer's clip) is 17.6 dB under the
  signal, 20.5 on the clip made flat for a speaker: it predicts a
  sample from the last, and there is little to predict.
- The A/B, with no decoder on the clock: the clip encoded and decoded
  on the host and stored as PCM, in one pack with the clip as it was
  (A), the round trip (B) and the 8 kHz clip (C), which is the same
  flash as B today. The user on white-clock: B "is not bad. I hear the
  difference, but yet it is way better than PCM 8k".
- sox's own encoder (`-t ima`) does worse on the same clip, 13.7 dB,
  so the encoder is the project's. One that keeps the four best paths
  through the coder's states (a beam, as ffmpeg's trellis) gains
  0.9 dB over the plain one, 1.3 with sixteen, at no cost to the
  decoder.

**Design:**

- A sixth kind of clip, `ADP1`: the magic, the rate as a `u32`
  (8000, 16000 or 31250, as `PCM1`), then the codes, two a byte, the
  high nibble first, which is sox's raw `ima`, so a clip's body plays
  on the host (`tail -c +9 clip.adp | play -t ima -r 16000 -c 1 -`).
  The coder starts from a prediction of 0 and step index 0; a clip
  has as many samples as it has nibbles.
- `core/src/adpcm.rs`: `Decoder`, the standard IMA step with its two
  tables, the codes read in place, three words of state. Going round
  for a ringing alarm starts the state over with the clip.
- `clip::Player` gets it as a body beside the tunes; its samples are
  16 bits, in `xm::STEP`ths of PCM's step, so they go to the
  converter as a tune's do and not through 8 bits.
- `scripts/adpcm.py` is the encoder (16-bit raw or a PCM clip in,
  `--search` paths kept, 4 by default) and, with `--decode`, the way
  back to 16-bit raw. `scripts/wav2pcm.sh` writes ADPCM when the
  output's name ends in `.adp`, from sox's 16-bit samples.
- Not looped unless asked, as PCM; `mkpack.py` and `ALARM_CLIP` need
  nothing.
- Tests: the decoder against samples sox decoded from the same codes,
  a sine through an encoder in the test and back, the end, the loop,
  a bad rate, the clip through `Player` to the converter's samples.

**Built, 2026-11-16:**

- As designed; 168 host tests. `Player` builds the decoder in place
  (`take_adpcm`), a few words, so the clip task's frame is as it was.
- Futurama (27.6 s at 16 kHz) is 221 109 bytes for 442 209, the
  error 18.4 dB under the signal with four paths and 17.6 with the
  plain encoder, which is what the user heard; the encoder takes 36 s
  for it. sox decodes the clip's body to the samples
  `adpcm.py --decode` gives.
- A firmware before this takes an `ADP1` clip for bare PCM at 8 kHz
  and plays noise, so a pack with one needs the firmware first.
- `packs/ab/adpcm.bin` is now the default clips, Futurama as PCM and
  Futurama as ADPCM, encoded from the PCM clip so the two differ by
  the coder alone. Owed: heard on a clock, with the clip's line in
  the serial log (the fill time against PCM's).
- `packs/ab/adpcm-speaker.bin`: the local speaker pack in its order,
  its Futurama (`packs/speaker/futurama.pcm`, a 100 Hz high-pass and
  10 dB of drive) as A and the same as ADPCM, 19.0 dB, as the ninth
  clip, B. The user, 2026-11-16, with the decoder on the clock: "The
  difference is audible, but acceptable." So ADPCM is a choice for a
  clip that has to be smaller, not a replacement for PCM.
- The user then: Futurama as ADPCM in the local packs.
  `wav/futurama.adp` and `packs/speaker/futurama.adp`, each from its
  PCM clip, take the PCM's place in `packs/clips-local.bin` and
  `packs/clips-local-speaker.bin` under the same name and at the same
  place: 351 026 bytes each for 572 126. Such a pack needs this
  firmware on the clock first.
- On test-clock (the speaker), 2026-11-16, from the serial log: the
  A/B pack's two clips, 866 chunks each with no underrun and no stray
  position; filling the ring took 353 ms of 27.7 s for PCM and 524 ms
  for ADPCM, so the decoder is 0.6 % of the processor. Then `770a519`
  over the air and `packs/clips-local-speaker.bin`: checked at boot in
  242 ms, its Futurama the same 524 ms and no underrun. The buzzer
  through the decoder is still not heard.

### The PT3 clips' limiter: the buzzer's as the speaker's, the speaker's to be made gentler — 2026-11-16, the buzzer's clip converted and tested on the host, not heard on a buzzer; a knee for the PT3 limiter built, and `--knee 37 --drive 150` chosen by the user on test-clock's speaker

The user, hearing Synchronization and PPK on test-clock's speaker with
the limiter (`--limiter --noise 16 --highpass 100 --passes 1 --release
10`, the speaker pack's) and flat (the buzzer's): "what we have now
for speaker should be there for beeper (beeper should not be flat for
sure). For speaker we want to keep limiter, but make it less
agressive". Of PPK: "limiter is quite noticeable in it". This takes
back the choice of 2026-11-13 for the buzzer, which was made against
the limiter at its 16 ms release.

- The buzzer: `clips/synchronization.pt3c` is now the speaker pack's
  clip byte for byte (`cmp`), the recipe in `clips/README.md`.
  `scripts/pt32clip.py`'s default is still the plain gain. Not heard
  on a buzzer: the limited clips the buzzers had were cut at 300 Hz
  with the noise at 11 sixteenths, and this one is 100 Hz and 16.
- A flat clip cannot be brought to the limited one's level for a fair
  comparison: at the header's top gain, 255, Synchronization is 0.297
  of full scale RMS against 0.307 with 1.0 % of the samples clipped,
  and PPK 0.199 against 0.366.
- The speaker: `PT31` has no knee, so what there is to turn is the
  release and the level a peak is aimed at. On the host (RMS of full
  scale, samples at the rails, the spread of the level in 50 ms
  windows between its 10th and 90th percentiles):

| | Synchronization | PPK |
| --- | --- | --- |
| flat, gain 180 | 0.214, 0.11 % | 0.141, 0.01 % |
| A `--release 10`, 128 ms (now) | 0.307, 0.17 %, 5.5 dB | 0.366, 0.13 %, 6.9 dB |
| B `--release 11`, 256 ms | 0.261, 0.09 %, 6.2 dB | 0.284, 0.05 %, 8.8 dB |
| C `--release 12`, 512 ms | 0.231, 0.05 %, 6.9 dB | 0.239, 0.03 %, 9.9 dB |
| D `--release 12 --drive 160` | 0.288, 0.63 %, 6.8 dB | 0.299, 0.46 %, 9.9 dB |
| `--release 14`, 2 s (15 is the same) | 0.191, 0.01 %, 9.0 dB | 0.162, 0.00 %, 13.7 dB |

  A to D of both tunes were in `packs/ab/sync-limiter.bin` on
  test-clock; not heard, since the user asked for the knee instead.
- "Why not to add knee?" (the user). `XMC2` has a byte for it; `PT31`
  and the SID's header have none, only `FLAT`, which is a fixed knee
  of 96. `PT31`'s flags had four bits spare, and they are now the
  knee, `pt3::KNEES`: 0 the shaper's own (3000, 20 dB of drive), each
  step 2 dB more, 4 to 93 in `Shaper::new`'s 1024s. A firmware
  without it reads past the bits and limits hard. `scripts/pt32clip.py
  --knee N` takes the level as `xm2clip.py` does and writes the
  nearest step. Host test: one channel's note at each step is at the
  plain gain the knee gives. 168 tests, clippy clean, the firmware on
  test-clock by OTA.
- What the knee does, at `--release 10` (RMS, at the rails, spread):

| knee | Synchronization | PPK |
| --- | --- | --- |
| the shaper's own (A) | 0.307, 0.17 %, 5.5 dB | 0.366, 0.13 %, 6.9 dB |
| 9 | 0.301, 0.16 %, 5.4 dB | 0.344, 0.11 %, 7.6 dB |
| 15 | 0.294, 0.15 %, 5.5 dB | 0.319, 0.10 %, 10.2 dB |
| 23 (B) | 0.287, 0.15 %, 5.4 dB | 0.283, 0.08 %, 12.3 dB |
| 37 (C) | 0.271, 0.12 %, 6.3 dB | 0.227, 0.04 %, 14.4 dB |
| 37, `--drive 150` (D) | 0.317, 0.78 % | 0.267, 0.35 % |
| 58 | 0.232, 0.07 %, 9.1 dB | 0.163, 0.01 %, 16.0 dB |

  Synchronization sits over the knee most of the time, so it changes
  little under 37; PPK's quiet parts come down with every step. A to
  D of both tunes are in `packs/ab/sync-limiter.bin` on test-clock
  (`sync_a` … `ppk_d`) after the speaker pack's first three clips.
- **Chosen, 2026-11-16: "D seems to be better"** (the user, on
  test-clock's speaker): `--knee 37 --drive 150` with the rest as it
  was. `scripts/speaker-clips.sh` makes Synchronization that way;
  the buzzer's clip stays at the hard limiter. The speaker's XM
  modules are as they were.
  The local speaker pack (`packs/clips-local-speaker.bin`, with
  `packs/speaker/futurama.adp` since the ADPCM work) has Hibernation,
  PPK and Synchronization made the same way.

### Backlog, board revision: a line output on the adapter for an amplifier — an idea of 2026-11-15, nothing built or measured; to be tried on the bench board when the amplifiers arrive

The user's, of the speaker mod: with a proper amplifier in place of
Q1 as the output stage, the adapter can carry the filter, and the
amplifier's input is soldered to a pad on the adapter. The amplifier
then has the converter's bits from the pin and no saturating
transistor between. Everything below is reasoned, not measured.

**The design, the user's of 2026-11-15: a pin of its own and a
setting that chooses the output.** "That would be cleaner: you
cannot mess up your board by using wrong firmware version." The
line output is a second GPIO, push-pull, and GPIO5 stays what it is,
open-drain to Q1. One firmware and one release image serve both
kinds of clock, and a wrong setting is silence and never a buzzer
held on: `line` on a stock clock drives a pin nothing is soldered
to, `buzzer` on a clock with an amplifier plays into Q1 as it always
did.

- The pin: GPIO20, the user's yes of 2026-11-15. The firmware uses 0, 1 and 3 to
  9. GPIO10, the first thought, has the module's own LED on it on
  both the SuperMini and the Zero (the user, 2026-11-15), which the
  converter's bits would light. GPIO2 is a strapping pin that must
  read high at reset, and the filter's capacitor and divider would
  hold it low. GPIO21 is UART0's TX and carries the ROM's boot text
  into the amplifier at every reset. GPIO18 and 19 are the USB.
  GPIO20 is UART0's RX, which nothing uses (the console is the USB
  one): an input with a pull-up while the ROM runs, so the filter
  sees one step to 3.3 V at a reset and no data. Where it comes
  out on each adapter's layout is for the board's drawing.
- The parts, from the pin to the pad: two RC stages, 1 kΩ / 10 nF and
  then 10 kΩ / 1 nF (the second at ten times the impedance, so it
  does not load the first; −3 dB near 10 kHz, 8.2 nF / 820 pF if that
  is dull); a resistor to ground as a divider, or a trimmer's
  footprint, since a full-scale clip is about 1 V rms off a 3.3 V pin
  and a module of the PAM8302's kind (24 dB) clips near 0.1 V rms
  (about 1.2 kΩ under the 11 kΩ for a tenth; the amplifier's input
  impedance is in parallel with it, so the level is to be checked on
  the module); a series capacitor, about 1 µF (many modules have
  their own).
- The setting: `output`, `buzzer` (the default, and what every older
  record reads as) or `line`; a new record version, on `/settings`
  and in `/api`, not in `mqtt::SET_KEYS`. Read at boot, and a save
  that changes it resets the clock as a change of network does: the
  buzzer task's LEDC and the clip task's pin handle are made once in
  `main.rs`, and swapping them under a playing tune is not worth the
  code.
- On `line` the clip's I2S signal goes to that pin through the GPIO
  matrix without the inverter, and the beeps' LEDC to the same pin,
  both push-pull; GPIO5 is left released for the whole boot. The pin
  rests driven low, so the rest is the ground and the rail's noise
  is not in it, and the clip's ramps start from and return to the
  bottom rail as they do from the released line now. Whether 30 ms
  of ramp is a thump through the capacitor is to be heard.
- The thermometer's hold (`sensors::Hold`) is for Q1's heat and does
  not apply on `line`.
- Still true: 3V3 is the converter's reference while a clip plays,
  so the Wi-Fi's bursts on that rail are in the audio; if they are
  heard, a single-gate buffer from an RC-filtered supply is the fix,
  a footprint at most. The amplifier's shutdown pin on one more GPIO
  is a thought for the idle current, not a plan.

**A first try that needs no firmware: a pull-up on GPIO5.** For the
bench, before the setting exists, and not for the board. A 470 Ω
pull-up from the GPIO5 net to 3V3 (never to 5 V), then 10 kΩ /
680 pF twice (−3 dB near 9 kHz), about 2.2 kΩ to ground for a tenth,
the capacitor. The line stays open-drain, the beeps work as they
are, the released line is the top rail, which the ramps already
start from, and the audio is inverted, which nobody hears. The
filter charges through the pull-up and R1 and discharges through R1
alone: a second harmonic of about (Rp/R1)·a/2, some 1 % at full
scale; 7 mA while the pin is low. A high line turns Q1 on through
R5, so the buzzer must be off the board, as the speaker mod has it;
Q1 may stay.

To try on the bench board, in this order: the pull-up and the filter
on a breadboard with the firmware as it is, the level at the
amplifier's input, the hiss with a silent clip and with Wi-Fi busy;
then the setting and the new pin, and the two side by side.

### The project renamed: onebitdawn — 2026-11-17, built and tested on the host; the pages and Home Assistant not yet seen on a clock

The user's: `esp32clock` "sounds too generic". The name is the 1-bit
buzzer line and the morning, and reads as "one bit down". This file
keeps the old name wherever it is history.

- The crates are `onebitdawn` and `onebitdawn-core`; the alias
  `cargo host-test` is unchanged.
- `mqtt::PREFIX` is `onebitdawn`, so the topics move; the device id
  and the entities' unique ids do not, and discovery tells Home
  Assistant the new topics. The manufacturer and the origin's name in
  the discovery payload are `onebitdawn`. The old retained `status`
  and `state` are left on the broker, the firmware does not clear
  them. To check on the bench: the entities keep their history and
  none is doubled.
- `config::AP_PASSWORD` defaults to `onebitdawn`, and the CI's and
  the release's `API_TOKEN` is `onebitdawn` (a seed: a clock with a
  stored token keeps it).
- The release's files are `onebitdawn-*.bin`; the links are
  `github.com/anarsoul/onebitdawn` and
  `anarsoul.github.io/onebitdawn/`, which work once the user has
  renamed the repository (GitHub redirects the old repository's
  address, not the old Pages address).
- The pages: heading `onebitdawn: <hostname>`, title `<hostname>:
  <page> - onebitdawn` (the hostname first, for a narrow tab), the
  setup form `onebitdawn setup`. 18 bytes a page more.

### A clip of a version no player reads is refused — 2026-11-17, built and tested on the host; no upload of such a pack tried on a clock

The user's question: is there a version in a clip's header, and can a
clip of an unsupported one be rejected. There is: the fourth byte of
every kind's magic is the format's version (`PCM1`, `ADP1`, `BPR2`
and `BPR3`, `XMC2`, `SID1` and `SID2`, `PT31`), and the pack has a
version byte of its own. Only `BPR1` and `XMC1` were refused, by
name; any other digit, a newer converter's clip on an older firmware,
matched no player, was taken for PCM with no header and played as
noise. No format changed, so every clip and pack there is stays good.

- `clip::is_unsupported`: the clip starts with one of the kinds'
  three letters (`FAMILIES`) and no player takes it, which covers an
  older version, a newer one and a header cut short. PCM with no
  header is still PCM; one that starts with those letters by chance
  would be refused, which no real sound does.
- `Pack::parse` refuses a pack with such a clip (`PackError::Clip`,
  "a version this firmware does not play"); on an upload that is
  after the write, as the checks of a clip's length and the hash are,
  so the clock is left without a pack, with the note melodies, until
  the next upload (only the pack's header is checked before the
  erase; the clips' first bytes arrive later in the body, so checking
  them first would mean reading the pack into RAM, which there is not);
  the `ALARM_CLIP` assert
  fails the build; `scripts/mkpack.py` refuses to make the pack
  (`PLAYED`, to be kept with the players' magics).
- Not done: a `PCM1` clip at a rate the player does not know is still
  played at 8 kHz, and unknown flag bits in a tune's header are
  ignored.

### Forward compatibility before the first public release — 2026-11-17, built, tested on the host and on test-clock; not committed, not tagged

The user's question before the release after v0.4, which may be the
first one anybody else flashes: will later releases be compatible
with it. The other way round is the one that cannot be fixed later:
what *this* firmware does with a later one's data, after a rollback
by hand or by the third unconfirmed boot. An audit of what crosses
releases (the partition table, the settings record and its map, the
carry record, the clip pack, the OTA checks, `/api`, MQTT, the web
installer) found three faults and one decision.

- **A value the image does not know cost the whole record.**
  `Settings::from_record` answered `None` for any field out of range:
  a later image with one more `temp_show`, one more transmit power
  level or a rule this parser refuses would leave a rolled-back clock
  on the defaults, with no network, as an access point. Now each
  field falls back by itself (the default; a network or broker that
  does not read is none; an alarm at no time of day is the default
  time, disarmed; the offset is clamped; day bits past the week are
  dropped; brightness past the list is the brightest) and `None` is
  only a record shorter than its version says. The test that listed
  the refusals lists the fallbacks, and a record of 0xFF bytes is
  still a valid one.
- **A longer record erased the settings.** `storage::load` erased the
  map on any error, and an item longer than the 512-byte scratch is
  one (`BufferTooSmall`). The scratch is 1024 bytes now and
  `storage::ITEM_MAX` (1000) is the promise: no later firmware writes
  a longer item, under any key. The first version of the fix only
  left such an item alone, and the bench showed why that is not
  enough: `sequential-storage` reads every item it passes, so with a
  1086-byte item in the page a record saved beside it was `saved`
  and then not read at the next boot, the same warning again. So an
  image that meets one runs on the defaults (with the build's seeds)
  and leaves it for the newer firmware, until it has something to
  save, and then erases the map first.
- **A carry layout change dropped the boot count.** A mark of
  another layout discarded the record, `ota_boots` with it, so an
  update that changed the layout could never be rolled back. From
  layout 4 the record is append-only and `carry::readable` takes a
  later layout by the words it knows. v0.4 is layout 4 too, so the
  update to this release was never affected.
- **The prebuilt token.** The CI's and the release's images had
  `API_TOKEN=onebitdawn` (and `esp32clock` before), printed in the
  manual: every installed clock would take a firmware from anything
  on its LAN, and `seed` stores the build's token at the first save,
  so no later release could take it back. The user: "let's ship
  without a token". The images have none; the PIN on the panel does
  everything from the pages, and the scripts work once a token is set
  on `/system`. The manual, the installer page, the release's text
  and the changelog say so.
- The pack's header comment says what a later pack must do for a clip
  of a new kind (a reserved bit or the version: this firmware would
  play it as PCM), and `AGENTS.md` has a "Compatibility" section with
  the rules. The changelog lost a stale line (the image is 886 KB,
  not 1040) and a second `### Changed`.
- Not changed: the partition table (520 KB of slot to grow into),
  the pack format, the OTA checks (magics and the appended hash, no
  version or name compared).

On the host: 170 tests with the CI's settings (no token), with the
clips built in and with the session's; clippy on both crates; the
release workflow's image steps run by hand (bootloader and table cut
from the merged image, the app at 0x20000 identical to the OTA image,
the pack through the parser). The main stack is 42.2 KB.

On test-clock (10.1.1.30, serial log read), with two throwaway builds
standing in for a later firmware (record version 13 with `temp_show`
3 and a transmit power of 72 in it, carry layout 5 with a tenth word;
one with 40 bytes appended to the record, one with 700):

| step | seen |
| --- | --- |
| OTA from `55eac74` to this build | `settings: loaded`, all settings as before, `boot 1 of 2`, `confirmed good` |
| three settings changed, 6 s, `scripts/reload.sh` | back in 3 s with them, time carried |
| the 426-byte version 13 record, then this build again | `loaded`; network, hostname, broker, token, alarm, melody kept; `temp_show` back at `c` and `tx_power` at the build's default, as meant; `carried over … ota_boots: 1` from the layout 5 record, `boot 1 of 2` |
| the 1086-byte record, then this build | `a stored item needs 1088 bytes of scratch … using defaults`, on the network by the build's seeds as `clock`; (first version: a save, a reload, the same warning: the fault above) |
| the settings posted back, 8 s, reload | `erasing the map for the item this firmware cannot read`, `saved`, then `loaded from flash` with everything |
| token cleared with `POST /token` | `/reload` and a network key on `/api` with the old token: `403`; `scripts/pin.sh`, `POST /token` with the PIN: `token set`; reload with it: `200` |
| the five pages | `200` each; `/api` as at the start but for the version |

Not tested: a clock that never had a token on a build with none
(test-clock's was cleared, which is the same path but for the
`token_cleared` flag); the access point's setup on such an image; the
release workflow itself, which runs on a tag. Still owed from before:
the default pack on a stock buzzer circuit.

### AGENTS.md cut to the rules, the references moved to `agents/` — 2026-11-17

`AGENTS.md` had grown to 90 KB, loaded into every agent session:
rules mixed with the description of the pages and services, a
per-file table whose rows had become module documentation, the
converters' flags with their history, and dated measurements. It is
now 24 KB of rules, and the rest moved into five
files read when the work touches them: `agents/behaviour.md`
(flashing and seeds, settings, pages and `/api`, clip packs, the
system page and the PIN, provisioning, self-heal, Home Assistant,
cross-site requests, firmware updates, releases), `agents/modules.md`
(the per-file table, what the host tests cover), `agents/clips.md`
(the converters, the pack tools, the sound path's build variables),
`agents/hardware.md` and `agents/sizes.md` (the facts and limits with
their figures). The move was first made with the text unchanged and
checked by script (every non-blank line of the old file in the new
one or in a reference); the references were then put under
sub-headings, bullets and tables, with every backticked name kept.
What that second pass dropped is only the account of how a thing was
before (the PIN drawn as the system page loaded, the token's own
page, the header size before `X-Frame-Options`), which the entries
above have; measurements and choices by ear kept their dates. New in `AGENTS.md`: "Reference files" (which to
read before touching what, and the rule that dates and measurements
go there or here, not into the guide), "Behaviour rules" (the rules
that were spread through the description: no `GET` with a side
effect, secrets never read back, the network keys' refusal on the key
alone, one PIN one action, the `set/` allow list, resets through
`heal`), a one-line-per-file layout table. "Timing rules in the scan
path", "Compatibility" and "Conventions" are as they were.
`ARCHITECTURE.md`, `MANUAL.md` and two scripts' headers point at the
new places.

### To do before the first public release — a list, 2026-11-17

The user's, and what the readiness check of the same day left open.

- [x] Screenshots of the web interface (the user's): six in
  `screenshots/`, taken from white-clock with headless Chromium at
  420 px and twice the scale, the clock page light and dark and the
  others dark, the network's name and the broker blurred in the page
  before the capture (the user's choice; the version line reads
  `v0.4-67-gfb0e18f`). Three in the README under the feature list,
  all six in the manual beside what they show; the manual said four
  pages and has `/clips` in its list now.
- [ ] A picture of the board with the buzzer mod, beside the README's
  description of it (the user's; a photograph, so the user's to take).
- [ ] `CHANGELOG.md`: `[Unreleased]` made the version's section, the
  compare links under it.
- [ ] An install of an image with no token from the web installer or
  the CI's artifact: the access point's setup, then a token set with
  the PIN.
- [ ] The default pack heard on a stock buzzer circuit (no diode, R5
  at 10 kΩ), which is what a builder has; the README's wording on the
  mod by what is heard.
- [x] The installer's list no longer offers v0.1 to v0.4 (the old
  partition table, the `esp32clock` token): `docs/index.html` leaves
  them out by tag (`OLD`); their files stay on `gh-pages` and their
  GitHub releases as they are. The page is deployed by the release
  workflow, so this is live with the next tag. Checked under Node
  against GitHub's list as it is today, which leaves nothing, the
  page's fallback to `latest/`.
- [ ] The annotated tag; then the release's three files and one
  install from the live page, since the workflow has not run with the
  pack and the new names.

# Module reference

What each file holds, and what the host tests cover. `AGENTS.md` has
the one-line version; update both when a module changes its role.

The pure modules live in `core/src/` and the firmware re-exports them
from `src/lib.rs`, so firmware code says `crate::settings`; inside
`core` the same `crate::` paths point at its own modules. Each `core`
module ends with its `#[cfg(test)] mod tests`.
`core/src/lib.rs` is `#![cfg_attr(not(test), no_std)]` and the module
list, nothing else.

Nine modules are split, the pure half in `core/src/` under the same
name and the hardware half in `src/`:

| module | pure half | hardware half |
| --- | --- | --- |
| `buttons` | event types | GPIO task |
| `clock` | calendar arithmetic | the offset and the carry |
| `carry` | record codec | RTC static and reset gate |
| `http` | head parser and `RESPONSE_BUF` | sockets |
| `ota` | image constants, `Slot`, `hash_chunk`, `version_from` | flash and partitions |
| `pack` | the clip pack's layout and checks | the partition, its mapping and the upload |
| `clip` | the pulse-width `Player` and the ring's `Pacer` | I2S0, its DMA ring and the pin |
| `mqtt` | topics, routes and the discovery payload | the session task |
| `sensors` | the NTC and LDR tables and the thermometer's hold | the ADC task |

## Display

**`core/src/aip33628.rs`**: frame format, bit-reversed current table,
quad-nibble waveform encoder; pure `core`.

**`core/src/panel.rs`**: COM/SEG map, font, per-position colour as
duty `Levels` (no RGB; the palette is written in levels),
`Panel::render` → `ScanImage`.

**`src/scan.rs`**: the cycle and transfer (one lap) builder, and the
scan task.

- The task builds the SPI2 quad + DMA bus itself, since an async
  esp-hal driver is `!Send`. It runs one plain dark transfer so
  esp-hal programs the registers, starts the ring, and from then on
  only acts on three requests.
- `stop`: the ring ends on its blanking frame within two laps, before
  a flash write. `resume`: after a failed upload. `halt`: before a
  deliberate reset, the stop latched, so `resume` is void after it and
  an upload failing under the reset's pause cannot restart the scan.
- `publish` forwards to the ring; `Report` is for the heartbeat.

**`src/ring.rs`**: the scan as one SPI2 DMA-controlled configurable
segmented transfer (TRM 27.5.8.5) on a one-segment descriptor ring.
The one module with register access to SPI2, through esp-hal's
`regs()`.

- `Ring` is a `DmaTxBuffer` of a conf descriptor (bitmap with magic
  0xA and the USER word) and eleven data descriptors of two cycles
  each, the last pointing at the first. Its `prepare` switches the
  mode on in the window esp-hal leaves between programming the
  registers and setting `usr`.
- `request_stop` clears `usr_conf_nxt` in the conf USER word.
- `publish` builds into the spare buffer and repoints the data
  descriptors (`point_at`), holding the buffer it left for two laps
  before it is written again.
- `position` is for the heartbeat.

**`src/display.rs`**: the UI loop: tick, button or settings change →
`ui` → draw → publish.

- Rings the alarm and signals the ringing edges (`ringing_changed`)
  for the MQTT state; `silence` ends a ring as a press would.
- Shows the IP, or `AP` in place of the sweep on the access point.
- Acts on the hold before the UI sees it.
- Seeds and carries the skip flag and the time the alarm last rang.

## UI and settings

**`core/src/ui.rs`**: the mode state machine, pure logic, one timer
for the current mode; it edits the settings it is handed.

- Alarm due check and missed-alarm grace, both gated by the carried
  local second the alarm last rang. Local rather than Unix, so the
  hour a fall-back transition repeats rings once; an alarm minute a
  spring-forward transition skips rings when the clock lands past it.
- Consumes `skip_next`.
- `view` takes the room reading and answers the `Temp` face mid-minute
  in clock mode.

**`core/src/settings.rs`**: `Settings`, the palette and brightness
tables, `Hostname`, `Text`/`Secret` strings, `Credentials`, the token
and the MQTT `Broker` with their rules, the build-time `seed`, the
shared `Watch` with its change signal, the versioned append-only flash
record.

**`src/storage.rs`**: the one flash handle (`with_flash`), the nvs
partition lookup, the `sequential-storage` map over it, and the
debounced persist task. The task compares records, not structs, and
resets into station mode after a save that changed the credentials, or
after any save on the access point with a network stored.

**`core/src/buttons.rs`**: `Button`, `Kind`, `Event`.

**`src/buttons.rs`**: debounced short and long press events, the
two-button long press, and the 10 s two-button `Hold`.

**`core/src/config.rs`**: build-time env vars and compile-time
defaults.

## Web

**`core/src/auth.rs`**: the API token (a setting; constant-time
compare) and the panel PIN with its TTL and lockout: `token`,
`token_or_pin`, `issue_pin`, `pin_to_show`, `Refusal` with its HTTP
status. Pure rules, with the clock and the random number passed in.

**`core/src/form.rs`**: pure logic.

- Parsing: key=value pairs, all-or-nothing `apply` on a copy with the
  `Allow` flags for the guarded keys, `apply_naming`, `parse_token`.
- Output: the JSON; `JsonStr`/`HtmlStr` escaping for the SSID.
- The five pages (`Page`): `write_page`, `write_settings_page`,
  `write_ha_page`, `write_clips_page`, `write_system_page`, and the
  setup form, over the shared `write_top` (head, heading, the line of
  links, the error notice).
- Parts: `Check` (a checkbox with its hidden `0`), `SAVE` and
  `UNSAVED_JS` (a script compares the form with the defaults it was
  served with), the read-only `Status` lines, `Note` and `FIELDS`
  (what a page says of the post that led to it, with the bad field's
  label).
- The PIN: `PIN_FORM` (the PIN's field, hidden until asked for) and
  `PIN_JS` (`ask`, which posts `/pin` at a button's click and moves
  the form under it, `say`, `go`, `back`, `up` for an upload), before
  the system page's `SYSTEM_JS` (four actions, a PIN each) or the
  clips page's `up('/clips')`.

**`core/src/http.rs`**: `Method`, `Head` (`content_length` is `None`
when the header is absent, and `transfer_encoding` says one was sent),
`parse_head`, `admit`; `RESPONSE_BUF` and `PAGE_MAX`, which the page
tests assert against.

**`src/http.rs`**: the HTTP/1.0 server, `LISTENERS` sockets each in
its own task, with per-listener state and counters for the heartbeat.

- Every request goes through `http::admit` first. One it refuses is
  answered by `turn_away` (a `302` to the address for a `GET` under
  another `Host`, else `403`; not counted as served). `status_line`
  sends `X-Frame-Options: DENY`.
- Before a route is looked at (`NoRequest`): a `POST` without
  `Content-Length` to a route that reads a body (`reads_body`: `/`,
  `/settings`, `/ha`, `/api`, `/play`, `/token`, `/ota`) is answered
  `411`, and any `Transfer-Encoding` `400`, since a bodiless post is
  not an empty form. The command posts (`/reload`, `/ap`, `/pin`,
  `/scan`, `/stop`) take a bare `curl -X POST`. A head with no end
  within `REQUEST_BUF` is a `431`, a form's body over `BODY_MAX` or
  the buffer a `413`, a head that does not parse a `400`; the first
  two after what the client is still sending has been read and thrown
  away (`discard_until_quiet`, `discard`), within `HEAD_TIMEOUT`, past
  which the connection is closed with no answer.
- Forms: `GET`/`POST` on `/`, `/settings` and `/ha` on the station
  (`/` is the setup form on the access point) and on `/api`; `POST
  /play`, `POST /stop`. A saved form redirects to its own page with
  `?saved=1`, a refused one with `?error=…` and the bad field's `key`.
- Clips: `GET /clips`, and `POST /clips`, a pack streamed to
  `pack::install`, read as `/ota`'s body is; `411` without a length.
- System: `GET /system` (issues no PIN; its script posts `/pin`), `GET
  /update` and `GET /token` (redirects to it), `POST /ota` with the
  body streamed to `ota::install`, `POST /reload`, `POST /ap` and
  `POST /token` with the token or the PIN, `POST /pin`, `POST /scan`
  on the access point.
- Static: `GET /favicon.ico` and `GET /tz.js` (`src/favicon.ico` and
  `src/tz.js` embedded), the two cacheable responses; the page loads
  the script as `/tz.js?v=<version>` so an update is not served the
  cached one.
- The pages are headed `onebitdawn: <hostname>` and titled
  `<hostname>: <page> - onebitdawn`.

**`src/tz.js`**: the page's `Use this browser's zone` button, served
as `/tz.js`; `scripts/tz-check.mjs` runs it under Node.

- It gives the browser's IANA zone as a POSIX rule from next year's
  two offset changes, so a zone dropping daylight time this year, as
  Vancouver did in 2026, comes out fixed.
- Each transition's rule is checked against the six years after (a
  midnight change is Saturday 24:00 in Chile and Sunday 00:00
  elsewhere). Names come from the first locale that has letters.
- It throws, and the page shows the message, for a zone with other
  than none or two changes a year, or none of whose candidate rules
  predicts the six years after (Casablanca's Ramadan hour).

## Home Assistant

**`core/src/mqtt.rs`**: pure: `DeviceId` and its topics, `Link` (the
state for the page), `Route` and `Command`, `SET_KEYS` and `apply_set`
over `form::apply_one`, `write_discovery` (the device payload with
every entity) and `DISCOVERY_MAX`.

**`src/mqtt.rs`**: the task: resolve, connect with the will,
subscribe, discovery and state publishes, keepalive pings, the select
loop over `poll_header`, the back-off, `goodbye` for the reset paths,
`link()` and `Report` for the page and the heartbeat.

## Sound

**`core/src/melody.rs`**: pure.

- The alarm tunes as pitch and length data with tempo; `RANDOM`
  (`usize::MAX`) and the pure `pick` that turns it into a tune.
- Built-in clips: `CLIP_COUNT`, `CLIP_NAMES` and `CLIP_DATA`, included
  from `clips.rs`, which `core/build.rs` writes from `ALARM_CLIP`;
  none by default.
- The table at run time: the nine note melodies and then the clips of
  the pack `install` was given, else the built-in ones. `count`, `get`
  (a `Melody` by value, the beeps past the end), `all`, `clip_data`,
  `clip_loops`, `LIMIT` (what a settings index is held under).
- The pack in force sits in a `critical_section` cell, a thread-local
  in the tests.

**`core/build.rs`**: `rerun-if-env-changed` for the build-time
variables `config.rs` reads; writes `clips.rs` (count, names, labels,
`include_bytes!` of the files in `ALARM_CLIP`, and which of them loop)
into `OUT_DIR`.

**`core/src/pack.rs`**: the clip pack.

- Layout: `CLPK`, a version, up to `MAX_CLIPS` (16) entries of offset,
  length, loop flag, API name (20 bytes) and label (32), the clips'
  data, a SHA-256 at the end.
- `declared_len` from a header; `Pack::parse` with every check
  (`PackError`); `clip(k)`.
- A clip whose magic is one of the kinds' with a version digit no
  player reads, older or newer, is refused (`clip::is_unsupported`),
  as the `ALARM_CLIP` assert and `scripts/mkpack.py` do.
- The test `the_pack_file_parses` reads the file `CLIP_PACK_FILE`
  names.

**`src/pack.rs`**: the `clips` partition.

- `init`, before the settings load: the partition is found by its
  label in the table (absent on a clock flashed before 2026-11-14,
  which is not an error), its pages written into the flash MMU's
  entries below the last (`map`; refused if any is in use), the pack
  parsed through the mapping and installed.
- `install`, the upload of `POST /clips`: header checked, pack out of
  the table, a clip playing from it stopped (`clip::playing_in`), scan
  stopped, `ota::write_hashed`, the cache invalidated, the pack parsed
  in place (its first sector erased if refused); then the caller
  reboots.
- `room` is for the page.

**`src/buzzer.rs`**: LEDC PWM on an open-drain pin; plays a melody on
command, looped or once, and stops within a note.

- `preview` and `stop_preview`, the pages' and the bus's, do nothing
  while the alarm rings; `ring` and `stop` are the display task's.
- A clip goes to `clip::play`, which hands it to the clip task on the
  same pin.
- For a ringing alarm a clip is asked to loop with no gap if
  `melody::CLIP_LOOPS` says it is made to (`Stream::new(data,
  looping)`: a beeper clip starts over, a module follows its own jump
  back, PCM wraps); the note melodies repeat after `repeat_gap_ms`.

**`core/src/clip.rs`**: sampled sound for the 1-bit buzzer line, pure.

- `Player`, default build (`CONVERTER`, from `config::CLIP_PDM`):
  writes a PCM clip's or a module's levels as 16-bit samples for the
  chip's PCM-to-PDM converter. `fill_samples` does the ramps and the
  clip, with full scale at 0.85 of the rail (`CONVERTER_LEVEL`, the
  loudest that did not overload by ear) and `SAMPLE_REST` for the
  released line. `converter_clock` gives the bit-clock divider and the
  true ring rate under the converter's whole dividers.
- `Player`, `CLIP_MODULATOR=sd` or `pwm` build: a software modulator
  that writes a clip's bitstream a buffer at a time, wrapped in 30 ms
  ramps from and to the released line. Sigma-delta works a bit at a
  time. Pulse-width is one pulse per carrier period, centred, its
  length the level at mid-period, with the rounding error fed back as
  far as the carrier leaves room (`ORDER`). Integer; linear
  interpolation between the 8 kHz samples; a sample of 255 is 0.95 of
  the rail.
- `Beeper`: a clip that is the line's level as run lengths, counts of
  1/512 000 s, off and on alternately (`scripts/zxbeeper.py`). `BPR2`
  has LEB128 counts. `BPR3`, the packed one, has tokens that are a run
  or a reference to the runs decoded from a token so many bytes back,
  read in place with a stack of returns (`refs::Refs`), nothing kept
  of what was played. A broken clip ends where it breaks, written out
  as it is. The line runs at its own `BEEPER_BIT_RATE`, 512 kbit/s
  (`Stream::bit_rate`).
- `Runs` writes a `RunSource` (the beeper clip) out as bytes.
- `Stream`: either kind, by its first bytes (PCM as ADPCM, `adpcm.rs`,
  is a `Player`'s body as a tune is). `Stream::new` is never inlined
  and builds a tune's player in place through `Player::take_*`:
  inlined, with a slot a kind, it was 6 KB of the clip task's frame,
  on whatever stack the interrupt found. For the firmware half it has
  `converter`, `sample_rate`, `rest` and `bit_rate` (the rate the ring
  is read at: the line's bits, or 16 a sample).
- `Pacer` says which chunks of the ring to write from the time since
  the start alone (one crystal clocks the line and the timer). It
  counts underruns, and counts as strays the reads of the DMA's
  descriptor register that are not two slots on from that.
- Constants: `BIT_RATE` and `CARRIER_HZ` from `config::CLIP_KBPS` and
  `CLIP_CARRIER_KHZ`, `CHUNK` 1024, `SLOTS` 16.

**`src/clip.rs`**: `clip_task` and `Sampler`.

- The task is on the scan's interrupt executor so that Wi-Fi work on
  the thread executor cannot starve the ring. The buzzer task, whose
  LEDC driver is not `Send`, stays on the thread executor and asks
  with `play`, lending the pin for the clip's length; `main.rs` makes
  the second pin handle, its one `unsafe`.
- `Sampler` is I2S0's transmitter on GDMA channel 1, looping over a
  sixteen-descriptor ring. For a PCM clip or a module it is in PDM
  mode with the PCM-to-PDM converter: mono 16-bit samples in the ring,
  256 bits each on the line by default. `tx_pcm_bypass` is set again
  after esp-hal 1.2.2 clears it, or the A-law compander makes white
  noise of the samples. For a beeper clip and the software modulators
  it is in TDM mode: two 16-bit channels, no clock pins, so the data
  line is the ring's bits at the bit rate, each pair of the stream's
  bytes swapped in the ring since a word goes out high byte first.
  Sixteen bits in both modes on purpose (`agents/hardware.md`, "I2S0's
  sample width").
- `play` writes the clip's bits (`clip::Stream`) from flash into the
  ring ahead of the DMA. It connects GPIO5 as a frozen open-drain
  signal through the GPIO matrix's inverter once the stream runs, and
  hands it back released before stopping. It logs the clip's chunks,
  underruns and the time spent filling the ring (the player's share of
  the processor).
- Registers, through `regs()`: the DMA's descriptor register, for the
  pacer's check only, and I2S0's `tx_conf`, for the bypass bit.

**`core/src/adpcm.rs`**: a PCM clip at four bits a sample, pure.
`Decoder` reads IMA ADPCM in place behind the `ADP1` header (the rate,
as `PCM1`'s), the codes two a byte with the high nibble first, as
sox's raw `ima`; the state starts over when a looping clip goes round.
It gives 16-bit samples in `xm::STEP`ths of PCM's step to
`clip::Player`, as a tune does. The encoder is `scripts/adpcm.py`,
which keeps the four best paths through the coder's states. LZ does
nothing for PCM (HISTORY.md, 2026-11-16).

**`core/src/xm.rs`**: a tracker module as a clip, pure.

- `Tune` is a player for the part of FastTracker 2's XM that chiptunes
  use (see its header for the effects). It reads in place the `XMC2`
  file `scripts/xm2clip.py` makes of a module; no heap, eight
  channels.
- It mixes at 32 kHz and averages to 16 kHz (8 if the clip asks; at
  62.5 kHz to 31.25 for a clip at `clip::FAST_HZ`).
- Then a high-pass and a hard limiter, as `scripts/wav2pcm.sh` gives
  PCM (`shape.rs`), their settings read from the clip's header; the
  converter works out the filter's coefficients.
- Its samples go through `shape::Shaper` to `clip::Player` as `i16` in
  256ths of PCM's 8-bit step (`xm::STEP`), whole steps only for a clip
  converted with `--bits 8`.

**`core/src/sid.rs`**: a Commodore 64 tune as a clip, pure.

- `Tune` plays the `SID1` record `scripts/sid2clip.py` makes: the
  register writes of each frame, then a byte for the frames to the
  next, with a loop offset in the header. `SID2` is the same with
  references in it to writes and waits it has had before (`refs`).
- `Chip` is a SID written for the clock. Three voices: accumulator,
  triangle with ring modulation, sawtooth, pulse, noise, combined
  waveforms as an AND, sync, test bit, and the envelope with the
  chip's rate periods, exponential decay and rate-counter wrap. The
  filter has a 6581's or an 8580's cutoff curve, routing and volume.
- It is stepped 64 000 times a second (32 000 if the clip's header
  says 2 steps a sample) and averaged to 16 kHz, or to 31 250 for a
  clip that says so. Then `shape::Shaper`, with the limiter's knee at
  three voices' top for a clip whose flags say `FLAT` (a plain gain,
  for a speaker).
- No 6502 on the chip, no samples through the volume register.

**`core/src/ay.rs`**: the AY-3-8910 (and YM2149), pure.

- `Chip` has `write` and `run(ticks)`. `run` answers the output summed
  over that many ticks of clock/8 by going from one change of a
  generator to the next, so a sample is the chip's exact mean and
  nothing aliases.
- Three tones, the 17-bit noise, the envelope's sixteen shapes over 32
  positions, the mixer, the three channels added up.
- The levels are as measured on the chips (`LEVELS_AY`, `LEVELS_YM`,
  Ayumi's tables), not the data sheet's even 3 dB, which made notes 3
  to 4 dB too quiet beside the drums.
- `set_noise_level`, the clock's own, mixes a channel that has the
  noise in it at so many sixteenths, for a buzzer that makes more of
  white noise than of notes (a clip's header byte 38,
  `scripts/pt32clip.py --noise N`, 11 by default, chosen by ear).
- `run_parts` answers the output as `Parts`, the noisy channels apart
  with their tones and the noise's share each by itself, for the
  player's optional low-pass over the noise alone (header byte 39,
  `--noise-lowpass HZ`, off by default: the lower level was preferred
  to it).
- A generator nothing listens to is moved on in one go. A counter past
  a shortened period turns at the next tick, not at the write.

**`core/src/pt3.rs`**: a ZX Spectrum tune as a clip, pure.

- `Tune` plays a Pro Tracker 3 / Vortex Tracker II module in place
  behind the `PT31` header `scripts/pt32clip.py` gives it: clock,
  cycles a frame, length, flags for a YM2149 and for starting afresh
  at the end.
- It handles the pattern codes, the effects (tone slide, portamento,
  sample and ornament offset, on/off vibrato, envelope slide, tempo),
  samples and ornaments a line a frame, and the tone and volume tables
  by the module's version. Every read is bounds-checked. No
  TurboSound.
- For a clip whose header says `FLAT` (the converter's default): its
  mean is taken out (a 5 Hz tracker), then `shape::Shaper` is the
  clip's high-pass and a plain gain, the header's drive in 127ths of
  three channels at their top as full scale.
- Without the flag the shaper limits as it does a module, from the
  knee the flags' top four bits name (`KNEES`; none is the shaper's
  own).

**`core/src/refs.rs`**: references in a packed clip, pure. `Refs` is
the stack of where to go on and how many items are left that a reader
of `BPR3` or `SID2` keeps while it reads a stretch again from earlier
in the clip's own bytes (`leave`, `enter`, `gave`), `DEPTH` deep; a
reference that takes all its parent wants takes its place. Also
`leb128`. `scripts/packrefs.py` is the packer both converters import.

**`core/src/shape.rs`**: `Shaper`, the high-pass and limiter the
players end in, set from a clip's header; `STEP`.

## Sensors

**`core/src/sensors.rs`**: pure.

- The NTC table (10 kΩ, B 3950, under the adapter's 10 kΩ pull-up from
  3V3) and `temperature_dc` by interpolation; `fahrenheit_tenths`;
  `Tenths`.
- `ldr_ohms` and `light_pct`: the logarithm of the LDR's resistance, 1
  kΩ is 100 and 50 kΩ is 0.
- `face`: the panel's four characters for a temperature.
- `Hold`, the rule that holds the thermometer for the heat of the
  buzzer's transistor. Told once a second whether a clip is playing
  through the PDM converter, it holds for `HOLD_PER_SECOND` (4)
  seconds a second played, `HOLD_MIN_SECS` (120) at least and
  `HOLD_MAX_SECS` (1200) at most, plays within a hold adding up.

**`src/sensors.rs`**: `sensor_task`.

- The async `Adc<ADC1>` with `AdcCalCurve` (the scheme that reads
  straight on the C3) on GPIO0 (NTC) and GPIO1 (LDR), 8 samples a
  second each into a 10 s exponential average, published as millivolts
  in two atomics.
- While `clip::converting()` and for the hold after it
  (`sensors::Hold`) the NTC's cell keeps the reading from before the
  clip, so the panel, `/api` and MQTT do. The average goes on
  underneath for the log, which says `held`. A reset loses the hold.
- `room()` is for `http::status`, `Report` for the heartbeat.

## Time and resets

**`core/src/tz.rs`**: POSIX TZ rules. `parse` (`const`, so a
build-time rule is checked by the compiler), `Tz::period_at(unix)`
(the offset, the daylight flag and the name in force), `fixed(minutes)`
for the old setting, the calendar arithmetic; `TZ_MAX` 48.

**`core/src/clock.rs`**: `LocalTime` (with the `offset_secs` it was
computed with and `local_secs`), `weekday_of_days`, `local_time(unix,
offset_secs)`.

**`src/clock.rs`**: Unix time = uptime + NTP offset; `period_at` and
`local_time` under the settings' rule. `carry_time` and
`restore_carried` move the time across a software reset as plain Unix
time, written every second by the display tick. `RESET_PAUSE` is for
every deliberate reset.

**`core/src/carry.rs`**: `Carried`, `MARK` with its layout version,
`readable` (this layout or a later one, which only appends words),
`encode`/`decode`.

**`src/carry.rs`**: the RTC-RAM record, used only after a software
reset (`CoreSw`/`Cpu0Sw`) or the RTC watchdog's; anything else zeroes
it at boot. It holds
the self-heal count and time, the skip flag, the local second the
alarm last rang, the Unix time last written, the boots of an
unconfirmed OTA image, the access-point flag for the next boot and the
watchdog's resets with the cause of the last. The
mark carries a layout version. `freeze` is for a mode switch.

**`core/src/watchdog.rs`**: the timeouts (`HARD_TIMEOUT_SECS`,
`DEADLINE_MS`, `FEED_PERIOD_MS`), the parties that check in (`Party`),
`overdue` and `verdict`, and `Trips`, the carry record's word: resets
since power-up, the `Cause` of the last, and `clips_suspect`, set by a
reset over the clip task and cleared by `clips_trusted`.

**`src/watchdog.rs`**: `start` sets the RTC watchdog (stage 0 a core
reset, stage 1 a system reset) right after `esp_hal::init`;
`watchdog_task`, on the thread executor, feeds it every second while
`verdict` finds nobody overdue, and otherwise notes the party, halts
the scan (bounded by `HALT_WAIT`) and resets. `check_in` and `expect`
are for the parties: the display task on every pass of its loop, the
clip task from a clip's start to its end. `note_boot` counts the
timer's own reset from the reset reason. `clips_suspect` is what the
buzzer task asks before it picks a melody, and `trust_clips` what the
firmware and pack uploads call before their reboot. `Report` is the heartbeat's:
the count, the last cause, the longest gap between two feeds.
`bench`, only with the `bench-hang` feature, hangs the firmware on
purpose: `POST /hang/thread`, `irq` or `cs` (a spin in a thread-mode
task, on the interrupt executor, in a critical section; the timer's to
end), `/hang/display` or `clip` (that task parked at its check-in; the
feeder's), all with the token or a PIN, and a build with
`BENCH_HANG=boot` spins in `main` after `ota::init` on every boot,
which sent to `/ota` is the update that hangs and is rolled back.
`cargo build --release --features bench-hang`.

**`src/heal.rs`**: the deliberate resets.

- `reboot` says the MQTT goodbye, blanks the panel through
  `scan::halt`, carries the time and resets; for `/reload` and the OTA
  reboot.
- `reset`, the self-heal, waits out a ringing alarm and counts itself
  first.
- `switch_mode`, the reset between station and access point, starts
  the carry record over and freezes it.

## Firmware image

**`core/src/ota.rs`**: image header and descriptor offsets, `Slot`
names, `hash_chunk` for the appended SHA-256, `version_from`.

**`src/ota.rs`**: the firmware upload: slot lookup and the boot-time
rollback, the sector writer with read-back and the appended hash
(`write_hashed`, shared with `pack::install`, as are the buffers),
activate, reboot; `VERSION` from `build.rs`.

## Network

**`src/net.rs`**:

- The station, on the stored credentials: a scan before every connect,
  the connect pinned to the BSSID and channel that scan heard loudest
  (a new choice each time; the driver chooses only when the scan heard
  nothing), and a transmit-power ceiling.
- A minute RSSI line. Connect failures are logged with their meaning
  (`wrong password?`, `no such network in range`), with a budget that
  ends in `heal::reset`.
- DHCP is switched on 500 ms after each connect and off at disconnect.
- The link and address monitor, the gateway ping probe with the
  self-heal trigger, SNTP, a DHCP renew on a hostname change.
- The boot's `Mode`, the access-point task (AP+STA, scans on request
  into the network list) and its static stack config.

**`src/dhcpd.rs`**: the access point's DHCP server: `parse`, `build`
and the lease table pure, the socket task around them; every reply
broadcast.

**`src/dnsd.rs`**: the access point's captive DNS: `answer` pure
(every A question is the clock, anything else an empty answer), the
socket task around it.

## Start-up

**`src/bin/main.rs`**: pin setup, the display wires with their
pull-downs, executors, task spawning, the panic reset hook, log
timestamps, the watchdog's start, the reset reason and carried time
at boot and the OTA rollback check (before the clip pack and the
settings, so a hang there is counted), the minute heartbeat log.

## What the host tests cover

`cargo host-test` runs the tests of `core/` on the host: 181 on
2026-11-17. The players' tests over real clips run only with
`ALARM_CLIP` naming some, which the CI does in a second run.
`.cargo/config.toml` aims every cargo command at the chip, which is
why the alias passes `--target host-tuple`; the `[unstable] build-std`
line there is ignored by stable cargo. `cargo clippy -p onebitdawn-core
--target host-tuple --tests` lints the tests.

Covered:

- Display: the AiP33628 encoder against a behavioural model of the
  chip (shift on CLK rise, latch on DIN rise while CLK high), the
  panel map, the palette and brightness tables.
- Settings: the hostname, password and token rules, the record round
  trip and every older version.
- Web: the form parser and the page sizes against `PAGE_MAX`, the head
  parser, the PIN and token header rules.
- Image and state: the appended-hash split, the carry codec (an older
  layout's record included), the watchdog's deadline, verdict and
  carry word, which resets put the clips under suspicion and the
  notes-only pick that follows, the clip
  pack's layout with every refusal, the melody table with and without
  a pack.
- Time: the calendar arithmetic, the POSIX TZ parser with its rejects
  and the 2026 transitions of both hemispheres, the UI's alarm, grace,
  skip and daylight-saving logic.
- Clips, generally: the melodies, the ADPCM decoder against sox's
  samples, the clip's modulator (a sine back out of the bits, one
  centred pulse a period, the ramps, full-scale stability), the beeper
  clip's runs bit for bit, the packed clip's tokens, a packer's round
  trip over random runs and the bad clips that must end.
- The module player: pitch, volumes, each effect's periods tick by
  tick, the song's order, the envelope, the shaping.
- The SID: pitch, each waveform's shape, ring modulation, sync, the
  envelope's times and its late note, the filter's modes and its
  stability at every setting, the frames' timing and the loop; a
  packed record by hand against the same tune plain, sample for sample
  and round the loop, and the bad records. A tune was compared with
  libsidplayfp's render once, by hand (HISTORY.md).
- The AY: `run` against a tick-by-tick model of the chip over random
  registers, pitch, the mixer, noise, each envelope shape, the levels.
- The PT3 player, on modules built in the test: notes by table and
  version, rows and tempo, sample lines and their sums, ornaments,
  slides and portamento, the envelope, the positions, the loop and the
  fresh start, cut and random modules.
- The ring's pacer, run against a model of the DMA. `CLIP_KBPS` and
  `CLIP_CARRIER_KHZ` change what is tested; `ALARM_CLIP=… cargo
  host-test` covers the table with the clip in it.
- Sensors: the NTC and LDR tables against their equations, the
  thermometer's hold.
- MQTT: the topics, routes and discovery payload, parsed with
  `serde_json` (a host-only dev-dependency) and checked key by key
  against the `/api` JSON and the form parser.
- `node scripts/tz-check.mjs`: the browser half of the timezone field,
  `src/tz.js`, against zones with known rules under Node, whose `Intl`
  is a browser's, and against Casablanca, which must throw.

Dumps for comparing a player with a reference:

- `CLIP_PCM_DUMP=file cargo host-test dump_the_tune -- --ignored`
  writes a built-in module's, SID tune's or PT3 module's samples;
  `CLIP_INDEX` says which clip.
- `CLIP_REG_DUMP=file CLIP_INDEX=n cargo host-test dump_the_registers
  -- --ignored` writes a built-in module's registers, fourteen a
  frame, for a reference player.

Not under test: `scan.rs`'s `build_cycle` (the M1 lit-time check needs
it on the host), `ring.rs` (descriptors and registers; its layout is
`const`-asserted), `src/clip.rs` (the I2S transfer and the pin),
`dhcpd.rs` and `dnsd.rs` (their pure halves use embassy-net's address
types), `storage.rs`'s map over the flash, `mqtt.rs`'s session loop.
`HISTORY.md` records the checks of each milestone; new ones are tests.

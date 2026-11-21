# Hardware reference

The hardware facts in full, with what was measured and when.
`AGENTS.md` has each as a rule; this is the evidence, and the place
for a new measurement that changes one.

## Buzzer (GPIO5)

The pin drives a PNP with its emitter at +5 V. Any driven level turns
it on, including a 3.3 V high; only a released open-drain line is
silent. Never make it push-pull.

The parts:

- Q1 is an S8550 in TO-92: 625 mW at 25 °C, about 200 °C/W, 500 mA,
  −25 V (typical datasheet figures; makers differ a little).
- The buzzer's coil is 16 Ω (the user's measurement, 2026-11-17; the
  README said 32 Ω until then).
- The most Q1 can dissipate with that load is at half the supply
  across it: 2.5 V, 156 mA, 0.39 W, whatever its gain and R5. That is
  a pin stuck low or a transistor held half on; inside the rating,
  with the junction near 120 °C in a warm case. Calculated.
- R5 sets the base current, not the pad: about 0.43 mA at 10 kΩ and
  0.84 mA at 5.1 kΩ. Saturating into 16 Ω takes about 300 mA of
  collector current, so Q1 does not saturate with either. Calculated.

The pin under a clip:

- The clip drives it from I2S0 with the GPIO matrix's inverter on, so
  a 1 bit is the buzzer on. A peripheral given the bare pin would make
  it push-pull, so it goes in as an open-drain `Output` turned into a
  signal, which esp-hal freezes.
- The inverter is part of the pin's connection: `clip.rs` takes the
  pin back as a plain `Output` when the clip ends, which clears it. A
  tone's LEDC must never be connected over an inverted pin: its
  released level would be the buzzer on.

The bit rate:

- A PCM clip or a module is the chip's PDM converter's bits. On the
  line output: 8 Mbit/s for a clip at 31 250 samples a second, the
  converters' default, and 4.096 Mbit/s for a 16 kHz one, 256 bits a
  sample. On the buzzer's pin half that, 128 bits a sample
  (`clip::BUZZER_CONVERTER_BITS`), whatever the build's.
- Why: a speaker in the buzzer's place, on the S8550 (the speaker
  mod), hisses under any sound, pure sines of 250 Hz and 1 kHz too,
  and a clip of zeros much less; at 128 bits a sample there is less of
  it than at 256 (the user's ear on test-clock, 2026-11-20), and the
  same clips on the line output's amplifier have none. Reasoned, not
  seen on a scope: the transistor turns off more slowly than it turns
  on, the line's level then goes with the bits' pattern, and the
  converter's noise comes down from over the audio band; zeros make a
  regular pattern with nothing to bring down. 64 bits a sample the
  driver refuses at this rate. A stock buzzer has not been heard at
  128 bits and 31 250 Hz.
- At 2.048 Mbit/s (16 kHz, `CLIP_PDM_BITS=128`) on white-clock
  (flyback diode, R5 at 5.1 k) it was cleaner than the software
  sigma-delta at 1 Mbit/s (2026-11-13). A stock board's transistor has
  not been heard at either; `CLIP_MODULATOR=sd` is the way back.
- The default of 256 bits a sample is the user's choice of 2026-11-13,
  and no clock's own transistor and buzzer have had it yet. It was
  heard only on a breadboard copy of the stage with headphones for the
  buzzer (`bench/`, 2026-11-13: a BC557, 5.1 k, a 100 Ω load). There a
  silent clip hissed at 128 bits and not at 256, and clips with heavy
  bass were distorted at either until a 2.2 kΩ resistor from base to
  emitter let the saturated transistor turn off in time. On a clock's
  own board that resistor changes nothing on the scope (2026-11-17,
  under the mod below): into 16 Ω the S8550 does not saturate.
- The converter at 64 bits a sample is white noise.
- The clip's bits come in long pulses: a per-bit sigma-delta stream at
  256 kbit/s was quieter than one pulse per 16 kHz period
  (2026-11-12).

On the scope:

- The pulses at the buzzer are as wide as GPIO5's, to the full 5 V.
  With no diode on the board the coil kicks to −10…−15 V at each
  turn-off.
- The clip is still much quieter than the beeps, which sit on the
  buzzer's resonance.
- The phantom presses seen while a clip played were the user's
  adapter, pins 9 and 10 of the DIP-16 shorted, not interference.

The user's mod, in the README as optional; the firmware assumes
neither part:

- a 1N4148 across the buzzer as a flyback diode (cathode to the
  collector side), which takes the negative peak away;
- R5 at 5.1 kΩ instead of 10 kΩ, whose doubled base current squares
  the collector's falling edge (a slope with 10 kΩ) and makes the
  buzzer louder.

The mod was four parts until 2026-11-17, with RBE (2.2 kΩ from Q1's
base to its emitter) and a BAT42 from GPIO5 to 3.3 V. The user's scope
that day, on a clock's board:

- The signal with all four is the same as with the diode and R5 at
  5.1 kΩ alone, so RBE and the BAT42 are out of the README.
- A BAT42 with no RBE keeps Q1 on all the time: never fit it alone.
  With RBE as well Q1 is off while the pin is released.
- `speaker-mod/` keeps its own RBE and BAT42, with other values.

The buzzer in the speaker's stage (330 Ω, 100 Ω, D1, the BAT42), the
user's bench, 2026-11-17:

- It is very quiet there, in the 8 Ω speaker's place.
- With RS left in, the collector's signal is clean and the buzzer's
  own has sharp peaks on every rising and falling edge. A speaker
  shows none.
- So the buzzer stays on the diode and R5 at 5.1 kΩ, with Q1 in its
  linear region and warm; the switching stage is the speaker's.
- Why, reasoned and not measured: with 5.1 kΩ Q1 is a class A stage,
  a current source of its gain times the mean base current (±85 to
  ±120 mA of swing for a gain of 200 to 290), which pushes that
  current through the coil whatever its impedance; a switch sets the
  voltage, and the coil on its iron core then takes little current at
  2 to 4 kHz. The buzzer's inductance is not known. The audio current
  could be read across RS at 1 ms a division with the bandwidth
  limited, in both stages.

## Line output (GPIO20)

For a clock with an amplifier in place of the buzzer's stage; the
`output` setting chooses it. Nothing here is measured yet: the design
and its figures are `bench/pdm-amp.md` until it is built and heard.

- Push-pull, at rest driven low. The tones' LEDC is silent there at a
  duty of 0 %, and a clip's stream goes out with no inverter, its
  zeros the bottom rail.
- GPIO20 is UART0's RX, which nothing uses: an input with a pull-up
  while the ROM runs. The firmware drives it only once the setting is
  `line` or `split`: at boot if stored so, else at the first sound
  that goes there after the change.
- `split` is for a clock with the amplifier and its buzzer both: the
  note melodies on GPIO5 as on `buzzer`, the clips here as on `line`.
- Why this pin and not 2, 10, 18, 19 or 21 is in the work's section
  (`PLAN.md`, "A line output on the adapter").

## Amplifier shutdown (GPIO10)

The shutdown pin of the amplifier behind the line output, a PAM8302A's
SD: push-pull, high is on. Built 2026-11-18, nothing soldered or
heard; the wiring is `bench/pdm-amp.md`, "The shutdown pin".

- Why: at rest the amplifier makes the Wi-Fi's traffic heard as faint
  clicks (the user, 2026-11-18, a PAM8403 on a breadboard).
- High from `buzzer::AMP_WAKE` before a melody that goes to the line
  output until it has ended and the pin is at its rest; through the
  gaps between notes and between an alarm's repeats. Low otherwise.
- Driven only for a sound on the line output, and low at boot, before
  GPIO20, when the stored `output` is `line` or `split`. On `buzzer`
  the pin stays as the ROM left it, an input.
- Through a reset it floats: the adapter's pull-down holds SD low,
  and the module's own pull-up to its 5 V supply is taken off (10 kΩ
  on Adafruit's board), so nothing on the pin is above 3.3 V.
- From the datasheet: SD is high from 1.2 V and low to 0.4 V; under
  1 µA shut down; no time from SD to sound is given, so `AMP_WAKE` is
  a guess.
- On the ESP32-C3 Zero GPIO10 is the module's addressable LED, which
  the firmware never used.

## The GDMA's `out_dscr`

It is not the descriptor being read. On the C3 it names the one two
further on, from the moment a transfer starts (traced against the
clock, 2026-11-12).

- The clip's pacer took it for the current one and rewrote every chunk
  as it was played, which was heard as skips, crackle and the clip
  overlapping itself, with no underrun counted.
- Pace a DMA ring by time; use the register as a check at most.
- `ring::position`, the scan heartbeat's figure, is off by the same
  two.

## I2S0's sample width

The transmitter is only ever set up for 16-bit samples, the beeper
clip's stream of bytes included (two 16-bit TDM channels, the bytes
swapped in pairs).

- A transmitter that has run with 8-bit samples (`tx_bits_mod` 7)
  sometimes goes on taking one byte a sample when it is next set up
  for 16: the registers read as they should, every one, and the DMA
  is read at exactly half the rate. A converter clip then plays an
  octave down while the pacer fills the ring by the clock, so it
  stutters as well.
- It was about one start in sixteen after a beeper clip (59 of some
  1000 on test-clock, 2026-11-17), and it stays through the
  peripheral's reset and every later start, until the transmitter has
  run at 8 bits again, which clears it fifteen times in sixteen.
- Not cured by: another master-clock divider (5 as 2), `tx_update`
  after the start as the IDF has it, a `tx_reset` and `tx_fifo_reset`
  with the clock still on at the stop, letting the transmitter run dry
  to `tx_idle` before the stop, or writing another width and the right
  one back, each latched.
- Forcing `tx_bits_mod` to 7 in a converter clip gives the same half
  rate every time; none of the TDM path's other values does.
- With 16 bits for the beeper clip too: 0 slow starts in 249 rounds,
  and in 440 clips of the change as committed.
- The check is the clip's log line: a slow clip counts stray
  positions by the dozen, a good one none.
- Not in Espressif's errata for the C3 (three entries on 2026-11-17:
  CPU-863, ADC-183, ADC-270, none about I2S), and no report of it was
  found in the esp-idf or esp-hal issues. ADC-270 is the same kind of
  thing in another peripheral: a counter the reset does not clear.

## I2S0's clock divider

`clip::settle_clock` writes the master clock's divider a second time
before every transfer, in the IDF's sequence for this chip
(`i2s_ll_tx_set_raw_clk_div`, "the double division issue"): a small
division first (2, with y alone set), then the target's `yn1`, z, y,
x and the whole part, in that order. esp-hal 1.2.2 does it on the
chips with a PCR only; its `main` does it for the C3 too, so
`settle_clock` is a stopgap, to be dropped at the update that brings
that.

- Without it the divider's fraction is not what the registers say on
  every start but the first after a reset. The beeper clip's line,
  160 MHz by 39 1/16, ran 0.165 % off: the DMA left the pacer's clock
  by a chunk in 9.7 s, counted as stray positions from about 10 s in
  (1890 in 30 s, 5589 in 60 s on another-bench, 2026-11-18), and by
  some thirty chunks over a five-minute ring. Which way was not
  measured.
- With it: none in 30 and 60 s, whatever was played before.
- The converter's divider is whole and its clips counted none either
  way; they get the same write.
- The check is the clip's log line after 20 s or more of a beeper
  clip; a 10 s preview shows nothing.

## Display wires (GPIO6–9)

They are SPI2 SIO0–3.

- The SPI builder resets the pull of a bare pin it takes but leaves a
  `Flex` alone (it arrives as a frozen signal). So `main.rs` sets the
  idle pull-downs on `Flex` pins and the scan task builds the bus from
  those.
- No PAC access for the pins. `ring.rs`, `clip.rs` and `pack.rs` are
  the modules that touch registers (SPI2's segmented-mode bits, each
  DMA channel's current descriptor and the flash MMU's table), through
  esp-hal's `regs()`.

## Buttons

GPIO4 is the upper button, GPIO3 the lower. No board pull-ups; the
internal ones are used.

## Sensors (GPIO0, GPIO1)

The adapter's 10 kΩ pull-ups from 3V3, with the clock board's NTC (R4)
and LDR (R3) to ground. The board's own R1 and R2 pull up to 5 V and
must stay off.

- A pin reads high when the part is cold, dark or missing. So only the
  NTC can report "no sensor" (the pull-up alone is colder than −20
  °C); a missing LDR looks dark.
- ADC1 is the converter that works with Wi-Fi on. 11 dB attenuation is
  only calibrated to about 2.5 V. `AdcCalCurve` is the scheme to use:
  the line fit is percent off.

The NTC reads warm:

- It reads a few degrees above the room on the bench and creeps up
  with uptime (2026-11-01): the board warms it. The user's thermometer
  is the reference, not the reading, and the `temp_offset` setting
  carries the difference (−0.7 on blue-clock).
- Q1, the buzzer's transistor, sits beside the NTC and runs at up to
  0.2 W while a clip plays through the PDM converter, whose pulses are
  shorter than the stage switches; it is cold on beeps and beeper
  clips. Five minutes of a module read 9.2 °C high on white-clock and
  took sixteen to come back (2026-11-14), which is what the hold in
  `sensors` is for.

## Antennas

What the clock hears of the access points, by board:

| board | near access point | notes |
| --- | --- | --- |
| adapter with the C3 Zero's chip antenna | 10 to 25 dB weaker than the old bench board at the same spot | the antenna sits inside the adapter's ground fill |
| the same with a U.FL receptacle at the feed and a patch antenna | −29 dBm at the bench (2026-11-01) | 25 to 30 dB put back; installed in the case, both access points read it at −70 |
| SuperMini adapter, SuperMini Plus, external antenna on its IPEX jack | −21 to −23 (2026-11-12) | the old white-clock board in the same case and spot: −38 to −54; the access points' side not yet read |
| v3 Zero adapter, chip antenna over a notch at the board's edge | −52 to −60 | level with the old board at the near access point, and unlike it always lists the far one, at −86 to −92 |

## Current

`IS` comes from brightness alone, capped at `MAX_CURRENT_STEP`, a
build-time variable, default step 5 (the 50 % step, about 15 mA per
sink).

- `settings::BRIGHTNESS_STEPS` is cut at the cap, so the button, the
  page and the API offer 10, 25 and 50 % by default, and a stored
  higher step lands on the brightest one offered.
- With the transmit ceiling at 16 dBm the board browns out on a quick
  step to 75 % or 100 % at the old cap of 11; the supply, not the
  firmware, is the limit (2026-10-22).
- Never derive the current from how much is lit, and never park the
  panel all-white at high current; the drivers get hot.

## Wi-Fi power

`tx_power` is a setting since 2026-11-15, verified on the bench board
that day: a select on `/settings`, `settings::TX_POWER_CHOICES` (the
driver's levels from 2 to 16 dBm), record version 12. It is for a
board whose antenna wants less, as the bench board's ceramic one.

- `net::apply_tx_power` gives it to the driver before every connect
  and every 10 s, so a change needs no reset. Not an MQTT `set/` key.
- The default is `WIFI_TX_POWER_QDBM` from the build, else the
  ceiling. `WIFI_MAX_TX_POWER_QDBM` is 68, the ceiling, which the
  driver rounds to its 16 dBm level (the level table is in the
  constant's comment; the log prints the setting).
- It is the station's only. The access point transmits at
  `config::AP_TX_POWER_QDBM`, 44 (11 dBm, enough for a phone beside
  the clock), whatever the setting, and never changes it. The setup
  form has the select too, which is the way back for a clock set too
  low to reach its network; the settings page says so (the user's,
  2026-11-15, after a first version that reset the setting on the
  access point).

The uplink:

- The access point hears the clock about 20 dB below what the clock
  hears of it, so the uplink is the side that fails, and the clock's
  own RSSI line looks fine while pages and lease renewals are lost.
- 14 and 16 dBm each gained a full step at the access point. 18 and 20
  dBm read louder at the near AP but quieter at the far one, with
  slower probes and failed associations, which is the SuperMini
  antenna mismatch.
- Measure any change with the AP's signal figure for the clock, from
  inside the case, not with the clock's RSSI.

## First frame after connect

The driver reports the station connected before it can pass a frame,
so a DHCP discover sent at that instant is lost and smoltcp retries
only after 10 s.

- The stack starts with no IPv4 config, and `net::wifi_task` enables
  DHCP `LINK_SETTLE` after the connect.
- embassy-net does not wake its runner on a config change. `net::poke`
  (a throwaway UDP bind) does, and must follow any `set_config_v4`.

## Flash

Writes run with interrupts off, so the scan stalls: a record write is
invisible, a sector erase is a brief dark blink.

- That is why the persist task debounces and skips writes that would
  not change the record, and why a firmware upload stops the scan
  first rather than flicker through 200 erases.
- A save that fails is tried again after 2 s, doubling to 60 s, with
  whatever the settings are by then.

One flash handle: `storage::with_flash` is the only way to the flash,
one blocking operation at a time, never across an await. A second
`FlashStorage` does not work: its constructor sizes the flash with a
chip-ID command over SPI1, and after the first handle has used the
flash that read comes back 0, so every operation on the second handle
fails its bounds check (measured 2026-10-22).

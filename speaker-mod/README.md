# Speaker in place of the buzzer

Two ways to put a small speaker where the buzzer LS1 is:

1. **Q1 stays the output stage**, with new resistors and two diodes.
   Built on the bench stage and measured (2026-11-16).
2. **An amplifier module** (PAM8302 or similar) in Q1's place. Not
   built; the modules are on their way, and the measurements go here
   when they arrive.

How it got here, the variants that were dropped and every simulated
table are in [`LOG.md`](LOG.md).

## 1. Q1 as a switch: 330 Ω, 100 Ω, a flyback diode and a BAT42

```
 +3.3 V ───┐         +5 V ──┬──────────┐
           │K               │          │
      D2   ▲              [RBE]        │E
           │A               │          │
 GPIO5 ────┴────[R5]────────┴─────────B  Q1  S8550
 open-drain                            │C
                            ┌──────────┤  LS1 +
                            │K       [RS]  in the speaker's lead
                       D1   ▲          │
                            │A       [SPK]
                            │          │
 GND ───────────────────────┴──────────┘  LS1 −
```

| part | value | where |
| --- | --- | --- |
| SPK | 32 Ω or 8 Ω, see RS below | LS1's pads; the buzzer comes out |
| D1 | 1N4148 | across LS1's pads, band to LS1 `+` (Q1's collector) |
| R5 | 330 Ω | R5's place (10 kΩ in the kit) |
| RBE | 100 Ω | from Q1's base to its emitter, across the two legs |
| D2 | BAT42 (or BAT54) | from GPIO5 to 3.3 V, anode to the pin |
| RS | by the speaker, below | in series with one speaker lead, off the board |

- **D1 is required with a speaker.** Without it every turn-off sends
  the coil's current into Q1's collector breakdown, millions of times
  a second during a clip. With it the current goes round through the
  diode and the coil smooths the bitstream: the stage is a small
  class D amplifier with the speaker as its filter.
- **R5 and RBE make Q1 a switch at a clip's bit rate.** R5 gives the
  base enough current to turn on within a bit, and RBE takes the
  base's charge out when the open-drain pin lets go, which nothing
  else does. RBE at 0.3 of R5 makes the two edges about as long.
  GPIO5 sinks about 10 mA while low.
- **D2 keeps the released pin inside its range.** With 430 Ω to 5 V a
  released GPIO5 would rest at 5 V, where the data sheet's input
  range ends at 3.6 V. The diode holds it there, for 3 mA into the
  3.3 V rail whenever nothing plays. Q1 stays off with it.
- **RS is sized for the line stuck on**, a fault the firmware does
  not produce, so that the speaker is inside its rating even then.
  D1 goes across the pads, so it spans RS and the speaker together.

| | 32 Ω 0.5 W | 32 Ω 0.25 W | 8 Ω 0.25 W |
| --- | --- | --- | --- |
| RS | none | 27 Ω, 0.25 W | 22 Ω, 0.5 W (or two 47 Ω 0.25 W in parallel) |
| current while on | 156 mA | 85 mA | 167 mA |
| speaker, stuck on | 0.78 W | 0.23 W | 0.22 W |
| speaker, loudest clip | 0.27 W | 0.08 W | 0.08 W |
| RS, any clip | | 0.10 W | 0.31 W |
| level against 32 Ω direct | 0 dB | −5.3 dB | −5.5 dB |

Calculated, with the supply at 5.0 V and nothing lost in Q1. The 32 Ω
0.5 W speaker is the one to want: no RS, nothing that gets hot, and
the loudest. The 8 Ω one must never go in bare (625 mA), and its RS
runs hot on any clip, which is in order.

No firmware change. The pin stays at esp-hal's default drive
strength: the strongest one was tried and made the collector's pulses
worse. After fitting, with nothing playing, there must be 0 V across
LS1's pads.

### Measured

The breadboard stage (`bench/`) on test-clock's GPIO5, 2026-11-16: an
S8550 with hFE 290 on the meter (the spares read 270), D1, an 8 Ω
speaker behind its series resistor, a meter and a Hantek 2C72. The
scope's figures are read off photographs of its screen at 200 ns a
division, so they are good to 20 ns or so.

At rest, nothing playing:

| stage | released GPIO5 | across the load |
| --- | --- | --- |
| 5.1 kΩ, 2.2 kΩ, D1, no D2 | 5 V | 0 V |
| 5.1 kΩ, 2.2 kΩ, D1, BAT42 | 3.49 V | 0 V |
| 330 Ω, 100 Ω, D1, BAT42 | 3.58 V | 0 V |

Playing a clip of the default build, 330 Ω, 100 Ω, D1 and the BAT42:
alternating bits of about 122 ns, a 4 MHz square wave, which is the
line at 8 Mbit/s.

| | default drive | strongest drive |
| --- | --- | --- |
| GPIO5, low | 0.7 to 1 V | about 0.35 V |
| GPIO5, released | to 3.6 V, over about 100 ns | the same |
| collector | −0.7 V (D1) to the rail, every bit | the same |
| collector's edges | about 50 ns each way | |
| collector half way up after the pin falls | 40 to 50 ns | 40 to 60 ns |
| collector half way down after the pin lets go | about 65 ns | 90 to 150 ns |
| a single on-pulse, for the pin's 122 ns | about 150 ns | |
| a single off-pulse, for the pin's 122 ns | about 100 ns | about 50 ns |
| base current while low, by the pin's volts | 2.8 mA | 4.3 mA |

- **Q1 switches at 8 Mbit/s**: a square wave to both rails on every
  bit, a lone one included, and no bit is lost. The on-pulses are a
  fifth of a bit long, some 25 ns.
- **Q1 stays cool** while a clip plays.
- **The BAT42 holds the released pin at 3.58 V**, with 3.3 mA
  through it, and Q1 is off: 0 V across the load.
- **The pad itself conducts nothing above 3.3 V** at these currents:
  5 V on the released pin without D2.
- **A stronger pin is worse.** With `DriveStrength::_40mA` the low
  came down to 0.35 V and the collector let go later, Q1 being driven
  deeper into saturation. The firmware went back to the default.
- Not measured: the distortion and the noise, the line at 4 Mbit/s,
  a transistor with a low gain.

## 2. An amplifier module (PAM8302 or similar)

Not built, nothing measured. A class D module takes Q1's place as
the output stage: it gets the converter's bits from the pin through
an RC filter, with no saturating transistor between, and drives the
speaker itself. The design is in `PLAN.md` ("a line output on the
adapter for an amplifier"); in short:

- **First, on the bench, with the firmware as it is**: a 470 Ω
  pull-up from GPIO5 to 3.3 V (never to 5 V), two RC stages of 10 kΩ
  and 680 pF (−3 dB near 9 kHz), about 2.2 kΩ to ground as a divider,
  since a module with 24 dB of gain clips near 0.1 V rms, and a
  series capacitor into the amplifier's input. The buzzer must be off
  the board, since a pulled-up line turns Q1 on.
- **Then a pin of its own**: GPIO20, push-pull, with the filter on
  the adapter (1 kΩ / 10 nF, then 10 kΩ / 1 nF, a divider, 1 µF) and
  an `output` setting, `buzzer` or `line`, so that one firmware
  serves both kinds of clock and a wrong setting is silence.

Measurements: to come when the module arrives. To be read: the level
at the amplifier's input, the hiss on a silent clip and with Wi-Fi
busy, and the two wirings side by side.

## The model of the S8550 stage, and why it is pessimistic

`bench/stage-sim.py` runs the stage in ngspice: onsemi's SPICE model
of the SS8550 with the gain from 120 to 300 and a storage time from
20 to 150 ns, D1, the loads with guessed inductances, GPIO5 as the
data sheet's strongest drive (18 Ω). It plays bit patterns and
compares the collector's pulses with the pin's, and with `--sine` a
1 kHz sine at half of full scale through a sigma-delta modulator of
its own.

The patterns: the share of the time Q1 is neither on nor off, and
its heat.

| R5, RBE | 4 Mbit/s | 8 Mbit/s |
| --- | --- | --- |
| 5.1 kΩ, 2.2 kΩ | 77 %, 0.43 W | 100 %, 0.45 W |
| 330 Ω, 100 Ω | 9 %, 61 mW | 20 %, 86 mW |

The sine at 4 Mbit/s into a 32 Ω speaker, over the nine transistors
(three gains, three storage times):

| stage | THD, worst to best | THD, the middle one | SNR | level |
| --- | --- | --- | --- | --- |
| 5.1 kΩ, 2.2 kΩ | −30.9 to −35.6 dB | −32.3 dB | 37 to 41 dB | −6.4 to +0.8 dB |
| 330 Ω, 100 Ω | −28.7 to −46.5 dB | −34.8 dB | 46 to 54 dB | −0.3 to +1.7 dB |
| 330 Ω, 100 Ω, BAT54 from the pin to 3.3 V | −29.0 to −42.6 dB | −33.9 dB | 45 to 55 dB | −0.5 to +2.7 dB |

What the model says: 330 Ω and 100 Ω against 5.1 kΩ and 2.2 kΩ is 9
to 18 dB less noise, a level that is the supply's and not the
transistor's gain, and a cool Q1, with the distortion much as it was,
2 to 4 %; the diode at the pin costs nothing over the spread of
transistors. It also says that 4 Mbit/s is the rate this transistor
switches at: at 8 Mbit/s it has Q1 between the rails a fifth of the
time, an edge a quarter of a bit, and for the one transistor it was
run with there, a THD of −22 dB. Its worst corner is a high gain with
a long storage time, where the pulses come out long.

**The bench says the model is pessimistic.** At 8 Mbit/s, the rate
it liked least, with a transistor of the gain it feared (290), the
collector is a square wave to both rails on every bit, and Q1 stays
cool. The S8550 on the bench stores less than the model's worse
columns assume, and the real pin, at 0.7 to 1 V while low, drives
the base less hard than the model's 18 Ω. So the model's figures are
a lower bound for ranking the variants and not what to expect of a
clock; its distortion and noise figures have no measurement beside
them yet.

## Also

- The speaker draws 85 to 167 mA in pulses from the 5 V rail. Do the
  main README's C3 replacement (10 µF) and use a 1 A supply.
- The clips in `clips/` are shaped for the buzzer and sound thin on
  a speaker. `scripts/speaker-clips.sh` makes the default pack for
  one, and `scripts/clips.sh <ip> packs/clips-default-speaker.bin`
  sends it.
- The raised lid has a socket for a round speaker
  (`openscad -o lid-speaker.stl -D speaker=true lid/lid.scad`), and
  `speaker_shim.3mf` is the TPU ring that holds a 28.4 mm speaker in
  it. The details are in `LOG.md`.

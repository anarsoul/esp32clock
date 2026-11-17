# Speaker in place of the buzzer: the log

How the speaker mod got to where `README.md` has it, kept as it was
written (to 2026-11-16) and not brought up to date: the first paper
design, the build with 5.1 kΩ and 2.2 kΩ that was heard on a clock,
Q1 as a linear stage, every simulated variant, the bench stage's
measurements, the capacitor-coupled variant, the lid. Where a section
here says "above", "below" or names another section, it means this
file. What is built now, and its figures, is in `README.md`.

## Where it started

Three small speakers in LS1's place, with as little done to the clock
board as each needs. A paper design: the figures below are calculated,
none of the three has been fitted to a clock or heard. The "check after
fitting" section says what to measure before trusting it.

The priorities, in this order: few changes to the board, nothing
overloaded (speaker, Q1, GPIO5, the 5 V supply), then the sound.

## The short version

The one that has been on a clock and heard (2026-11-15, the 8 Ω
0.25 W speaker), and the easiest:

- the speaker in LS1's pads, with RS in its lead unless it is the
  32 Ω 0.5 W one;
- D1, a 1N4148, across the pads;
- R5 at 5.1 kΩ;
- RBE, 2.2 kΩ, across Q1's base and emitter legs.

Q1 is a switch and the speaker's coil is the filter. The 32 Ω 0.5 W
speaker is the one to want: no RS, nothing that gets hot, and 6 dB
more than the 8 Ω 0.25 W one, which works too, behind its 22 Ω (two
47 Ω in parallel on the clock tried). That RS runs hot on any clip,
0.3 W, which is in order.

A capacitor in RBE's place, which makes Q1 a linear stage, was on the
same clock first: it works, RS runs cooler, R5 has to be found for
each transistor, and it was quieter and heavier in the bass. It is
"The alternative" below. The rest of this page is the reasoning, the
figures and the other variants.

## The stage as the kit has it

```
 +5 V ─────────────┐
                   │E
 GPIO5 ───[R5]────B  Q1  S8550 (PNP)
 open-drain        │C
 10 k (kit)        │
 5.1 k (buzzer     ├───── LS1 +
 mod)              │
                 [LS1]  buzzer
                   │
 GND ──────────────┴───── LS1 −
```

GPIO5 low turns Q1 on and puts 5 V across LS1; GPIO5 released is
silence. A tone is a square wave at the note's pitch, on for a half or
a third of the time. A clip is a 1-bit stream (PDM at 4 or 8 Mbit/s, or
the beeper's 512 kbit/s) whose share of ones is the level: a clip's
silence is half the time on, so a speaker carries a steady current for
as long as a clip plays, not only on the loud parts.

What a speaker changes: it is a few ohms of copper where the buzzer is
a coil the firmware only ever drives at its resonance. With the full
5 V across it, a 32 Ω speaker draws 156 mA and an 8 Ω one 625 mA, which
is more than Q1 (500 mA) and the supply will give.

## The mod

```
 +5 V ─────────────┬──────────┐
                   │          │
                 [RBE]        │E
                   │          │
 GPIO5 ───[R5]─────┴─────────B  Q1  S8550
                              │C
                              │
                   ┌──────────┤  LS1 +
                   │          │
                   │K       [RS]   in the speaker's lead
              D1   ▲          │
                   │A       [SPK]
                   │          │
 GND ──────────────┴──────────┘  LS1 −
```

| part | 32 Ω 0.5 W | 32 Ω 0.25 W | 8 Ω 0.25 W | where |
| --- | --- | --- | --- | --- |
| SPK | in LS1's pads | in LS1's pads | in LS1's pads | the buzzer comes out |
| D1 | 1N4148 | 1N4148 | 1N4148 | across LS1's pads, band to LS1 `+` (Q1's collector) |
| RS | none (a wire) | 27 Ω, 0.25 W | 22 Ω, 0.5 W (or two 47 Ω 0.25 W in parallel) | in series with one speaker lead, off the board, in heat-shrink |
| R5 | 5.1 kΩ | 5.1 kΩ | 5.1 kΩ | R5's place; the kit's 10 kΩ also works, quieter |
| RBE | 2.2 kΩ | 2.2 kΩ | 2.2 kΩ | from Q1's base to its emitter, see "Step 2" |
| CBE, CS | | | | in RBE's place and across LS1's pads, for "The alternative" only |

**Step 1, required: SPK, D1 and RS.** Nothing on the board changes but
what is soldered to LS1's two pads; a clock that has the README's
buzzer mod already has D1 and the 5.1 kΩ.

- **D1 is not optional with a speaker.** Without it every turn-off
  sends the voice coil's current into Q1's collector breakdown, a few
  million times a second during a clip. With it the current goes round
  through the diode, and the coil's inductance smooths the bitstream
  into the audio current: the stage is a small class D amplifier with
  the speaker as its filter. A 1N4148 carries the 170 mA at most that
  flows here; a 1N5819 does as well.
- **RS is sized for the line stuck on**, not for music: with Q1 on for
  good (a fault; the firmware releases the line whenever nothing
  plays, and a reset releases it too) the speaker must still be inside
  its rating. Music is then well inside it.
- RS goes in the speaker's lead and D1 across the pads, so the diode
  spans the resistor and the speaker together.

**Step 2, for the sound: R5 = 5.1 kΩ and RBE = 2.2 kΩ.** RBE goes
from Q1's base to its emitter (+5 V), on the solder side across the
two legs.

With 5.1 kΩ alone Q1 gets 0.84 mA of base current and nothing takes
the base's charge out when GPIO5 lets go, which on the breadboard
copy of the stage (`bench/`, 2026-11-13, a BC557 with 50 mA)
stretched every pulse and distorted the clips with heavy bass until
2.2 kΩ from base to emitter went in. RBE is the way out for the
charge, and takes 0.3 mA of the drive, which leaves the base 0.55 mA.

On the clock tried (2026-11-15: the 8 Ω speaker behind 23.5 Ω, a
transistor whose gain came out near 300) that is a base current that
just carries the 160 mA of the load: Q1 reaches the rail and is
hardly saturated, so it also lets go quickly. Heard there after the
alternative below, on the same clips: louder and less heavy in the
bass, and the better of the two. The level is the supply's and the
load's and not the transistor's, the reason nothing is trimmed, and
the speaker is driven with a voltage through RS, which damps its
resonance where a current source does not. RS is hot with it, 0.3 W
on any clip, as "The figures" has it.

- A transistor with less gain than about 290 does not reach the rail
  with these values at 160 mA and is quieter; 3.3 kΩ at R5 with the
  same RBE gives the base 1 mA, enough for a gain of 160. GPIO5 sinks
  1.3 mA then.
- Keep RBE under half of R5. (Written for a pin whose pad holds it
  near 3.9 V when released, where RBE would otherwise leave Q1 partly
  on. The bench stage's released GPIO5 reads 5 V, 2026-11-16, so
  there is no such hold; the simulation still wants RBE at 0.3 of R5,
  for the turn-off.)
- Not on a scope yet: whether the pulses at the collector are as long
  as GPIO5's at 8 Mbit/s. A simulation says they are not, and that
  with these values Q1 is not a switch at a clip's bit rates at all:
  "Simulated" below, with the values it would take.

### The alternative: Q1 as a linear stage, a capacitor in RBE's place

CBE, 47 nF, from Q1's base to its emitter where RBE would be, and R5
to suit the transistor: a ceramic capacitor that never sees more than
0.7 V, so any voltage rating will do. This was on the clock before
step 2's pair and lost to it by ear; it is kept for what was learned
and for a clock whose RS must stay cool.

It goes the other way and keeps Q1 out of saturation altogether.
R5 turns each `1` of the bitstream into a fixed charge into the base
node (the base sits 0.65 V under the rail whatever happens, so R5 is
a current source of 4.3 V / R5 while GPIO5 is low), CBE holds that
charge between the bits, and the base draws it out steadily. No charge
is lost, so the base current is the bitstream's mean, exactly, and the
collector current is that times the gain:

    I(collector) = gain × (4.3 V / R5) × share of ones

Q1 integrates the PDM itself and drives the speaker as a current
source, class A, with a steady draw from the 5 V rail where the switch
draws pulses.

- **R5 sets the level and has to match the gain.** The stage is right
  when a full stream of ones just reaches the rail, so that a clip's
  silence (half ones) sits at half the supply:
  R5 ≈ 0.9 × gain × (RS + speaker). An S8550 is sold with a gain
  anywhere from 85 to 300, so the value is found on the clock, not
  here. Start with the table's value in "The figures, Q1 linear", play
  any clip and read the DC voltage across LS1's pads with a
  multimeter: a clip's mean is its silence, so the meter shows the
  bias. Aim at 2.0 to 2.4 V. Otherwise the new R5 is the old one times
  the reading over 2.2 V; a reading of 4 V or more means Q1 is
  saturated even on silence, so double R5 and read again. A reading
  over about 2.6 V is low for what it is, since the loud half is
  already clipping and pulls the mean down: take a step more than the
  sum says and read again. (The one clock tried, 2026-11-15, the 8 Ω
  speaker behind 23.5 Ω: 3.73 V with 5.1 kΩ, 3.14 V with 7.2 kΩ,
  2.71 V with 10 kΩ, 2.40 V with 12 kΩ and 1.71 V with 20 kΩ, the
  resistors hot at the first two and not from 10 kΩ on. The last
  three are not in proportion either: within 0.02 V they are
  20 V·kΩ / R5 + 0.71 V, a gain near 300 and 23 mA of collector
  current that R5 does not set. A likely cause, not measured: the
  charge the pin's capacitance takes through the base at every edge
  of the stream, 6 pF at 8 Mbit/s, which the model leaves out; if so
  a 4 Mbit/s clip sits lower and a beeper clip has none. 12 kΩ was
  kept: a swing from 1.0 to 3.8 V at full scale by that sum. Each
  doubling of R5 is 6 dB off the sound whatever the bias does, so
  stop at the first value that does not clip.) Too little
  gain for R5 only makes the clock quieter (a gain of 120 where 200
  was assumed: 1.5 V, 4 dB down, clean); too much clips the loud half
  (300 for 200: 3.2 V and a quarter of the signal is distortion).
- **Aim low rather than high.** A transistor's gain rises as it warms,
  and Q1 warms itself by some 40 °C over a long clip, so a bias set
  cold at 2.4 V creeps up into the rail. 2.0 V cold costs 1.6 dB.
- **CBE is a compromise, and 47 nF is the middle of it.** The filter it
  makes with the base is faster the more current flows, so the quiet
  half of a wave is smoothed more than the loud half, and that is
  distortion that grows with CBE and with pitch. Too small a CBE
  leaves more of the bitstream's ripple in the collector current. The
  voice coil does not let its current change that fast, so the
  collector is thrown to the rail (Q1 saturated for a moment) or to
  −0.6 V (D1 conducting) by the ripple, and the more ripple, the more
  of the sound goes with it. Modelled with a gain
  of 200, a 32 Ω speaker of 0.3 mH and a tone at the clips' full
  scale:

  | CBE | 1 kHz | 5 kHz | |
  | --- | --- | --- | --- |
  | 10 nF | 5.4 % | 5.6 % | ripple: 1.1 dB lost as well |
  | 22 nF | 3.2 % | 4.0 % | |
  | 47 nF | 2.1 % | 6.2 % | |
  | 100 nF | 3.4 % | 10 % | |
  | 220 nF | 6.5 % | 14 % | 1.2 dB down at 5 kHz |

  At half that level 47 nF is under 1 % at 1 kHz and 2 % at 5 kHz.
  The figures are for the 8 Mbit/s of a clip at 31 250 samples a
  second; at 4.096 Mbit/s the ripple is twice as large and 47 nF reads
  2.9 % and 5.9 %, so the older 16 kHz clips want 47 nF or a little
  more, not less.
- **CBE goes with R5.** The filter's speed is the base current over
  CBE, so a stage with half the current wants half the capacitor:
  47 nF is for an R5 near 5.6 kΩ, 22 nF for 10 kΩ and over, 68 nF for
  3.6 kΩ. (47 nF at the 39 mA of the 100 Ω row further down reads
  3.2 % at 1 kHz where the 75 mA stage reads 2.1 %.)
- **The 8 Ω speaker behind its 22 Ω is the same stage**: 30 Ω where
  the bare speaker is 32. Modelled with a coil of 0.05, 0.1 and
  0.3 mH it reads 2.0 to 2.2 % at 1 kHz and 6 % at 5 kHz with 47 nF
  alone, 0.7 % and 3.4 % with 22 nF and CS, 0.3 % and 1.6 % with 10 nF
  and CS, at −5.3 dB against the 32 Ω speaker direct, as the switch
  gave it. The coil's size hardly matters, since RS is most of the
  load.
- **A fourth part worth having: CS, 220 nF across LS1's pads**, beside
  D1. Even with 47 nF the ripple is a few milliamps a bit, and into a
  bare coil that is still a collector jumping between the rail and
  D1 at the bit rate, with Q1 saturating briefly each time: the part
  of this the model is least to be trusted on, and D1 stays required.
  The capacitor gives the ripple a way round the coil, the collector
  then carries the sound and some millivolts of ripple, and Q1 is
  linear in fact. CBE can then come down, since it no longer has to
  spare the coil: 22 nF reads 0.8 % at 1 kHz and 4.2 % at 5 kHz, 10 nF
  0.4 % and 2 %, in the same model. With the speaker's inductance it
  makes a low-pass near 20 kHz (for 0.3 mH; nobody has measured
  these speakers).
- **No resistor from base to emitter with CBE.** It would take the
  first 0.65 V / RBE of every bit's current past the base, a dead zone
  at the quiet end: the two are one or the other.
- Tones and beeper clips are as loud as before: with GPIO5 low for a
  whole half-wave Q1 goes to the edge of saturation as the switch did,
  its edges rounded over some 20 µs.
- Released, the base still hangs on GPIO5 through R5 as it always
  has; CBE adds nothing to that. After a clip the last of the
  collector current dies away as 1/t: 1 mA after a quarter of a
  millisecond, 1 µA after a quarter of a second.

The percentages are a model's, not a measurement: one equation for
the base and a current-limited coil (no Early effect, no gain that
falls with current, a second-order modulator standing in for the
chip's), so they rank the capacitors and no more. What the clock said
of it (2026-11-15, 47 nF, no CS, R5 from 5.1 to 20 kΩ): it plays, at
10 and 12 kΩ with RS no longer hot, and step 2's switch then sounded
better. What the model leaves out is the likely reason: the
transistor's gain, which moves with its current, is in the signal
here and not in a switch, and a current source lets the speaker's
resonance through, which is the heavier bass.

## Simulated: R5 and RBE for Q1 as a switch at 4 and 8 Mbit/s

A model, not a measurement (2026-11-16, `bench/stage-sim.py`,
ngspice): nothing here has been built or heard, and the build above,
5.1 kΩ and 2.2 kΩ, is still the one that has. Q1 is onsemi's SPICE
model of the SS8550 (their `ss8550.lib`, Fairchild's of 2004, copied
into the script, as the FETs' models further down are), with the gain
taken from 120 to 300 and, since the model has none, a storage time
from 20 to 150 ns (the model's `TR`, its reverse transit time, which
is what sets the storage: the delay a scope shows also depends on how
hard the base is driven and pulled back, and a general-purpose PNP's
`TR` can be longer than 150 ns); D1; each load with a guessed
inductance; GPIO5 as
the ESP32-C3's data sheet has it (v2.5: 28 mA at 0.495 V at the
strongest drive, so 18 Ω to ground) with a diode to 3.3 V. It plays
four bit patterns (a lone 1 in eight bits, 1010, 1100, a lone 0 in
eight) and compares the collector's pulses with the pin's.

**Corrected the same day: the pin has no such diode.** A released
GPIO5 on the bench stage (5.1 kΩ, 2.2 kΩ, D1) reads 5 V; a diode to
3.3 V in the pad would have held it near 3.9 V. So in this section
and in its tables the released pin is wrong: nothing flows into the
pad at rest (the "GPIO5, released" column is none in every row), the
pin rests at 5 V, and Q1 turns off faster than these figures have
it. What stands: the choice of 330 Ω and 100 Ω, 4 Mbit/s, and what
is said of the load, the capacitor and the collector's BAT54. The
figures with the pin as measured are in "The pin as measured", after
this section, and they are the better ones.

**And a second correction, after a review: the sine's figures are
one transistor's.** Every sine table from here to "Over the
transistor's spread" is for a gain of 178 and 50 ns of storage, and
the best THD figures in them (−43 dB for 330 Ω and 100 Ω, −40 dB with
the FET, −44 dB push-pull) are a null at that transistor, where
turn-on and turn-off happen to balance. Over the gains and storage
times the patterns were run with, the same stages are −29 to −46 dB,
and the order the nominal tables put the variants in does not hold.
Read those tables for the level and the noise, and "Over the
transistor's spread" for the distortion and for which variant to
build.

What it says:

- **5.1 kΩ and 2.2 kΩ is not a switch at these rates.** The 0.5 mA
  of base current has the model's 100 pF of base and 33 pF of
  collector capacitance to move, against a bit of 244 ns at 4 Mbit/s
  and 125 ns at 8. Q1 is between the rails for three quarters of the
  time at 4 Mbit/s and all of it at 8, at up to 0.45 W, and at the
  model's own gain of 178 it does not carry a 32 Ω load to the rail.
  What was heard on the clock was a stage that slews, not one that
  switches.
- **The load hardly matters.** D1 keeps the coil's current going
  through the off bits, so Q1 meets 80 to 160 mA at every turn-on
  whatever is in LS1's pads, and the 32 Ω buzzer, the 32 Ω speaker and
  the 8 Ω one behind its 22 Ω come out within a tenth of a bit of
  each other. One pair of values serves all three.
- **RBE at 0.3 of R5.** RBE is what takes the base's charge out
  when the pin lets go, and it takes its 0.7 V / RBE from the drive
  while the pin is low: at this ratio the two edges take about as
  long. More RBE than that and the pulses come out long (330 Ω with
  330 Ω: a bit too long each), less and Q1 is short of base current.
- **Below 330 Ω there is little left to gain.** Something of the
  transistor's own sets the edges from there (not looked into: the
  model's base resistance is 122 Ω only at microamps and falls to
  27 Ω at these base currents, so it is not simply that): 220 Ω and
  68 Ω takes half as much again from the pin
  for two points less of the time between the rails.
- **4 Mbit/s is the rate this transistor switches at.** With 330 Ω
  and 100 Ω Q1 is between the rails a tenth of the time at 4 Mbit/s
  and a fifth at 8, where an edge is a quarter of a bit and no values
  make it a clean switch.
- **A saturated Q1 lets go late, by its storage time**, which the
  model does not give: with 50 ns the pulses are 0.05 to 0.5 of a bit
  long at 4 Mbit/s, with 150 ns a lone 0 between ones is lost whole.
- **A capacitor across R5 does not help.** An open-drain pin cannot
  push the base back up, so the capacitor only adds to the charge.
- **A BAT54 from Q1's collector to its base** (anode to the
  collector, cathode to the base) keeps Q1 out of saturation, so the
  storage time and the gain no longer matter: every pulse comes out
  short by the same slow turn-on, 0.15 to 0.3 of a bit at 4 Mbit/s
  and 0.25 to 0.55 at 8. It costs 0.2 V of the swing as well, and
  with a sine through the stage (below) it is the worse of the two at
  4 Mbit/s for a transistor with 50 ns of storage: one to keep for a
  transistor that turns out to store much longer, not to fit first.

The pulse error is in bits over the three loads, the four patterns
and the spread of transistors; "between the rails" is the share of the
time Q1 is neither on nor off.

| rate | R5 | RBE | clamp | pulse error | between the rails | Q1 | GPIO5, low | GPIO5, released |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 4 Mbit/s | 5.1 kΩ | 2.2 kΩ | none | −7 to +0.4 | 78 % | 0.44 W | 0.8 mA | 0.15 mA |
| 4 Mbit/s | 330 Ω | 100 Ω | none | −0.2 to +1.0 | 10 % | 65 mW | 13 mA | 2.6 mA |
| 4 Mbit/s | 330 Ω | 100 Ω | BAT54 | −0.30 to +0.08 | 12 % | 72 mW | 13 mA | 2.6 mA |
| 8 Mbit/s | 5.1 kΩ | 2.2 kΩ | none | −7 to +1.0 | 100 % | 0.45 W | 0.8 mA | 0.15 mA |
| 8 Mbit/s | 330 Ω | 100 Ω | none | −0.3 to +1.0 | 20 % | 86 mW | 13 mA | 2.6 mA |
| 8 Mbit/s | 330 Ω | 100 Ω | BAT54 | −0.57 to +0.10 | 24 % | 94 mW | 13 mA | 2.6 mA |
| 8 Mbit/s | 220 Ω | 68 Ω | BAT54 | −0.45 to +0.11 | 19 % | 82 mW | 19 mA | 3.8 mA |

A sine through the stage (`bench/stage-sim.py --sine`): near 1 kHz
at half of full scale, through a second-order sigma-delta loop of the
script's own (the chip's converter is not documented; second order is
the likely one), into the 32 Ω speaker, with Q1 at the model's gain of
178 and 50 ns of storage. The collector's mean over each bit, then a
spectrum over the clip's band, half its sample rate. "Bits alone" is
the bitstream before the stage, its sine over everything else; the
level is the stage's sine against the bits' at 5 V; THD is the
harmonics against the sine, SNR the sine over what is not a harmonic,
SINAD the sine over both, and the bits are what that SINAD is worth.

| clip | line | bits alone | R5, RBE | clamp | level | THD | SNR | SINAD | bits |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 16 kHz × 128 | 2.048 Mbit/s | 84.9 dB | 5.1 k, 2.2 k | none | −5.2 dB | −24.1 dB | 37.3 dB | 23.9 dB | 3.7 |
| | | | 5.1 k, 2.2 k | BAT54 | −5.3 dB | −22.0 dB | 38.3 dB | 21.9 dB | 3.3 |
| | | | 330, 100 | none | +0.1 dB | −30.7 dB | 49.2 dB | 30.6 dB | 4.8 |
| | | | 330, 100 | BAT54 | +0.1 dB | −31.2 dB | 52.8 dB | 31.2 dB | 4.9 |
| 31.25 kHz × 128 | 4.0 Mbit/s | 81.1 dB | 5.1 k, 2.2 k | none | −4.2 dB | −35.1 dB | 36.3 dB | 32.6 dB | 5.1 |
| | | | 5.1 k, 2.2 k | BAT54 | −4.0 dB | −35.7 dB | 37.0 dB | 33.3 dB | 5.2 |
| | | | 330, 100 | none | +0.1 dB | −36.8 dB | 49.9 dB | 36.6 dB | 5.8 |
| | | | 330, 100 | BAT54 | −0.3 dB | −26.3 dB | 44.9 dB | 26.2 dB | 4.1 |
| 16 kHz × 256 | 4.096 Mbit/s | 95.6 dB | 5.1 k, 2.2 k | none | −4.2 dB | −37.7 dB | 38.0 dB | 34.8 dB | 5.5 |
| | | | 5.1 k, 2.2 k | BAT54 | −4.0 dB | −38.1 dB | 38.6 dB | 35.3 dB | 5.6 |
| | | | 330, 100 | none | +0.1 dB | −37.6 dB | 51.2 dB | 37.4 dB | 5.9 |
| | | | 330, 100 | BAT54 | −0.3 dB | −26.2 dB | 48.5 dB | 26.1 dB | 4.0 |
| 31.25 kHz × 256 | 8.0 Mbit/s | 97.0 dB | 5.1 k, 2.2 k | none | −3.0 dB | −25.0 dB | 38.8 dB | 24.9 dB | 3.8 |
| | | | 5.1 k, 2.2 k | BAT54 | −2.8 dB | −24.8 dB | 38.1 dB | 24.6 dB | 3.8 |
| | | | 330, 100 | none | +1.1 dB | −22.7 dB | 39.6 dB | 22.6 dB | 3.5 |
| | | | 330, 100 | BAT54 | −0.9 dB | −21.7 dB | 40.8 dB | 21.6 dB | 3.3 |

- **The stage is the limit, not the bits.** The bitstream is good for
  81 to 97 dB; after Q1 it is 22 to 37 dB, four to six bits, and the
  harmonics are most of what is lost.
- **4 Mbit/s with 330 Ω and 100 Ω, no clamp, is the best of them**:
  THD near −37 dB (1.4 %) and the noise 50 dB down, with the clip at
  31 250 samples a second and 128 bits or at 16 kHz and 256.
- **8 Mbit/s is the worst with 330 Ω and 100 Ω**, THD −22 dB (8 %),
  and no better than 2 Mbit/s with 5.1 kΩ and 2.2 kΩ (−25 dB for
  −24): the bits a sample it adds are lost in the transistor.
- **330 Ω and 100 Ω is level and noise more than distortion**: 4 to
  5 dB louder than 5.1 kΩ and 2.2 kΩ, which at this gain does not
  reach the rail, and 12 to 14 dB less noise at 2 and 4 Mbit/s; the
  harmonics are as they were at 4 Mbit/s and 7 dB lower at 2.
- **The BAT54 costs 11 dB of THD at 4 Mbit/s** with these values and
  this transistor, and changes little elsewhere.
- **2 Mbit/s is not better than 4** (THD −31 dB against −37), which
  has not been looked into.
- One transistor, one load, one level and one pitch; the same row
  with the solver's tolerance ten times tighter came out the same, so
  the figures are the model's and not the solver's. The other
  transistors are in "Over the transistor's spread", and they change
  the THD by more than anything in this table does.

So, by the model: 330 Ω and 100 Ω, no clamp, and the line at
4 Mbit/s: `CLIP_PDM_BITS=128` for the clips at 31 250 samples a
second, or the default 256 with clips at 16 kHz.

What the ESP32-C3's data sheet (v2.5) says of the pin, and does not:

- **No limit on one pin's current.** The only maximum is 1000 mA for
  all the pins together (table 5-1). The DC table gives 28 mA as the
  typical sink current at 0.495 V with `PAD_DRIVER = 3`; esp-hal's
  default is the step below (`DriveStrength::_20mA`), so 13 mA is
  inside what the pin does but leaves it some tenths of a volt above
  ground, and the strongest drive is the setting for these values.
- **A pin's input is specified to VDD + 0.3 V**, 3.6 V. A released
  GPIO5 pulled towards 5 V is outside that with any R5, the kit's
  10 kΩ included; what differs is the current.
- **Nothing on current into a pin above the rail**: no injection
  figure, no word on the pad's diode. The one number is the latch-up
  test's trigger, ±200 mA (JESD78), which is a qualification and not
  a condition to run at.

Before any of it is soldered:

- **The model is of onsemi's SS8550**, the clock's part is whoever's
  S8550, and the inductances (3 mH, 300 µH, 50 µH) are guesses, which
  the result hardly depends on.
- A scope on Q1's collector against GPIO5 on a clip's silence
  (alternate bits) settles it.

## Simulated: the pin as measured, no diode in its pad

The same stage with GPIO5 as the bench stage has it
(`bench/stage-sim.py --no-pad-diode`, 2026-11-16): a released pin
goes to 5 V through R5 and RBE and takes nothing. At rest every pair
with an RBE has the pin at 5.00 V, the base at the emitter and Q1
off. That nothing goes into the chip is what the model was told, not
what it found: it has nothing in the pad to conduct, on the strength
of one meter reading taken through 7.3 kΩ. Through 430 Ω whatever is
in the pad has seventeen times the current to draw on, which is what
the FET further down is for.

The patterns at 4 Mbit/s, over the three loads; the error is for the
transistor as the model has it, the share of time between the rails
the worst over the gains and storage times:

| R5 | RBE | pulse error | between the rails | Q1 | GPIO5, low |
| --- | --- | --- | --- | --- | --- |
| 5.1 kΩ | none | −1.0 to +2.6 | 95 % | 0.36 W | 0.8 mA |
| 5.1 kΩ | 2.2 kΩ | −7 to −0.1 | 77 % | 0.43 W | 0.8 mA |
| 1 kΩ | 330 Ω | −0.3 to +0.4 | 22 % | 90 mW | 4.2 mA |
| 1 kΩ | 1 kΩ | +0.7 to +1.6 | 25 % | 0.16 W | 4.2 mA |
| 470 Ω | 150 Ω | −0.03 to +0.39 | 12 % | 69 mW | 9 mA |
| 470 Ω | 470 Ω | +0.9 to +1.5 | 14 % | 0.13 W | 9 mA |
| 330 Ω | 100 Ω | −0.02 to +0.35 | 9 % | 61 mW | 13 mA |
| 330 Ω | 330 Ω | +0.9 to +1.4 | 10 % | 0.11 W | 13 mA |

The sine at 4 Mbit/s into the 32 Ω speaker:

| R5, RBE | clip | level | THD | SNR | SINAD | bits |
| --- | --- | --- | --- | --- | --- | --- |
| 5.1 k, none | 31.25 kHz × 128 | −2.8 dB | −24.0 dB | 37.2 dB | 23.8 dB | 3.7 |
| | 16 kHz × 256 | −2.7 dB | −24.3 dB | 39.6 dB | 24.2 dB | 3.7 |
| 5.1 k, 2.2 k | 31.25 kHz × 128 | −3.4 dB | −34.2 dB | 37.0 dB | 32.4 dB | 5.1 |
| | 16 kHz × 256 | −3.4 dB | −35.2 dB | 38.7 dB | 33.6 dB | 5.3 |
| 1 k, 330 | 31.25 kHz × 128 | −0.5 dB | −27.3 dB | 40.3 dB | 27.1 dB | 4.2 |
| | 16 kHz × 256 | −0.5 dB | −27.2 dB | 43.9 dB | 27.1 dB | 4.2 |
| 470, 150 | 31.25 kHz × 128 | +0.1 dB | −42.1 dB | 46.5 dB | 40.7 dB | 6.5 |
| | 16 kHz × 256 | +0.1 dB | −42.2 dB | 49.5 dB | 41.4 dB | 6.6 |
| 330, 100 | 31.25 kHz × 128 | +0.2 dB | −42.9 dB | 47.3 dB | 41.6 dB | 6.6 |
| | 16 kHz × 256 | +0.2 dB | −42.9 dB | 50.3 dB | 42.2 dB | 6.7 |

- **330 Ω and 100 Ω is THD −43 dB**, 0.7 %, six and a half bits, for
  this transistor: 6 dB better than with the pin the first runs had,
  since a pin that goes all the way to 5 V stops pulling at the base.
  Over the other gains and storage times it is −29 to −46 dB, −35 dB
  in the middle.
- **470 Ω and 150 Ω is the same within a decibel** for this
  transistor, for 9 mA from the pin and not 13; over the others it is
  within 3 dB of 330 Ω and 100 Ω but for the longest storage with
  the highest gain, where it is 4 dB worse.
- **Against 5.1 kΩ and 2.2 kΩ, the build that has been heard**:
  3.5 dB louder, 8 dB less THD, 10 dB less noise, for this
  transistor. Over the others the noise's 6 to 15 dB stand, the level
  is a matter of the gain (5.1 kΩ and 2.2 kΩ is 6.4 dB down at a
  gain of 120 and at the rail at 300), and the THD is no gain to
  count on: 2 dB in the middle, and at the longest storage with a
  gain of 178 or 300 5.1 kΩ and 2.2 kΩ is the better.
- **1 kΩ and 330 Ω is not a step on the way**: louder than 5.1 kΩ
  and 2.2 kΩ and with more distortion, −27 dB. The gain is below
  500 Ω.
- **The released pin is at 5 V**, where the data sheet's input range
  ends at 3.6 V. That is so with any R5, the kit's 10 kΩ included,
  and is how every one of these clocks has always rested. A FET
  between the pin and R5 takes it to 1.2 V for 3 dB of THD, and a
  BAT54 from the pin to 3.3 V to 3.65 V for 12: see "A FET between
  GPIO5 and R5" and "A BAT54 from GPIO5 to 3.3 V".

So, by the model with the pin as measured: 330 Ω and 100 Ω, or 470 Ω
and 150 Ω, D1, the line at 4 Mbit/s. (This said 1 % parts: the
ratio is sharp for one transistor, but the transistor moves the best
RBE further than a 5 % resistor does.)

## Simulated: the stage without RBE, with and without D1

The stage as the kit has it (R5 at 10 kΩ, no D1, no RBE) and as the
README's buzzer mod has it (5.1 kΩ and D1), in the same model with
the pin as measured (`bench/stage-sim.py --no-pad-diode --load
"buzzer 32R, lossy" 10000:0 5100:0`, with `--no-flyback` for no D1;
2026-11-16). The load is the buzzer, since no speaker goes in without
D1. Without D1 the coil's current has to go somewhere, and the model
gives it two places: 30 V of breakdown across Q1, and 180 Ω of loss
across the buzzer's coil, chosen so that 70 mA kicks the collector to
the −10 to −15 V the scope showed on a board with no diode
(2026-11-12). The same 180 Ω is there in the runs with D1, so the two
are like for like, and these figures are not to be set beside the
speaker's above. What the buzzer's losses really are is not known.

At rest Q1 is off with either R5: the released pin floats up to
within a junction of 5 V and the base has nothing to draw through.
(The first runs of this, with a diode in the pin's pad, had Q1
passing 9 and 16 mA at rest; 0 V across LS1 on a clock with 5.1 kΩ
and no RBE, and 5 V on the bench stage's released pin, said
otherwise.)

The sine at 4 Mbit/s (16 kHz × 256), as above but into the buzzer:

| R5 | D1 | level | THD | SNR | SINAD | bits |
| --- | --- | --- | --- | --- | --- | --- |
| 10 kΩ | fitted | −7.0 dB | −17.7 dB | 34.8 dB | 17.6 dB | 2.6 |
| 10 kΩ | none | −6.6 dB | −17.0 dB | 34.0 dB | 16.9 dB | 2.5 |
| 5.1 kΩ | fitted | −2.0 dB | −22.7 dB | 36.5 dB | 22.5 dB | 3.4 |
| 5.1 kΩ | none | −1.3 dB | −20.8 dB | 36.3 dB | 20.7 dB | 3.1 |

- **5.1 kΩ for 10 kΩ is 5 dB of level and 4 to 5 dB of THD**, which
  is what was heard on the clocks: louder.
- **D1 changes the sound little** in the model: one or two decibels
  of THD, under a decibel of level. What it changes is where the
  coil's energy goes: the collector no longer swings below ground,
  and Q1 no longer meets its breakdown if the buzzer's own losses do
  not take the kick.
- **Neither is a switch.** Q1 is between the rails for most of the
  time with or without D1, and in the pattern runs with D1 it runs at
  up to 0.4 W, carrying the coil's current with volts across it, and
  at 0.1 W without, where the current falls back between pulses.
- **Against 330 Ω and 100 Ω with D1** (the speaker's figures, so
  roughly): the kit's stage is 7 dB quieter with 25 dB more THD,
  13 % for 0.7 %, and 15 dB more noise.

## Simulated: a BAT54 from GPIO5 to 3.3 V

A Schottky diode from the pin to 3.3 V, anode to the pin
(`bench/stage-sim.py --pin-clamp`, 2026-11-16; a model, as all of
this). It was tried for the current a released pin was thought to
pass into its pad; with the pin as measured there is none, and what
is left for it to do is hold a released GPIO5 inside the data sheet's
input range, which ends at VDD + 0.3 V. At rest, nothing playing,
with 330 Ω and 100 Ω:

| | no diode | BAT54 at the pin |
| --- | --- | --- |
| released GPIO5 | 5.00 V | 3.65 V |
| into the chip | none | none |
| through the BAT54, into the 3.3 V rail | | 3.1 mA |
| Q1's base under its emitter | 0 V | 0.31 V |

Playing, at 4 Mbit/s into the 32 Ω speaker, the sine as above:

| R5, RBE | pin diode | between the rails | THD, 31.25 kHz × 128 | THD, 16 kHz × 256 | SNR | level |
| --- | --- | --- | --- | --- | --- | --- |
| 330, 100 | none | 9 % | −42.9 dB | −42.9 dB | 50.3 dB | +0.2 dB |
| 330, 100 | BAT54 | 11 % | −31.0 dB | −31.4 dB | 50.4 dB | 0.0 dB |
| 330, 82 | BAT54 | 13 % | −28.4 dB | −28.1 dB | 45.9 dB | −0.4 dB |
| 330, 120 | BAT54 | 10 % | | −22.2 dB | 44.3 dB | +0.3 dB |
| 330, 150 | BAT54 | 11 % | | −16.9 dB | 38.1 dB | +0.7 dB |
| 470, 150 | BAT54 | 13 % | | −30.2 dB | 49.4 dB | −0.1 dB |
| 270, 100 | BAT54 | 9 % | | −21.3 dB | 43.4 dB | +0.4 dB |

- **It takes the pin from 5 V to 3.65 V**, at the edge of the range
  and no longer 1.4 V past it, for 3.1 mA into the 3.3 V rail
  whenever nothing plays.
- **It costs 12 dB of THD**: 2.8 % for 0.7 %, with the level and the
  noise as they were. Held at 3.65 V the pin goes on pulling at the
  base through R5 after it lets go, which is the slow turn-off the
  first runs had throughout.
- **Q1 is still off at rest**, with 0.31 V from base to emitter.
- **With it the ratio is sharp.** 100 Ω is the value for 330 Ω;
  120 Ω, 0.36 of R5 for 0.30, is 9 dB worse again, so 1 % resistors.
  470 Ω and 150 Ω is as good within a decibel, 2.2 mA through the
  diode at rest, the pin at 3.64 V.

So, for this transistor, it is a choice and not a gain: the pin at
the edge of its range, or 12 dB less distortion with the pin at 5 V,
where the kit's own circuit has always left it. Over the other gains
and storage times the 12 dB is not there: −29 to −43 dB with the
diode, −29 to −46 dB without, the same in the middle. What it does
cost stands: 3 mA into the 3.3 V rail at rest, and a pin at the edge
of its range and not inside it, where the FET of the next section
puts it.

A BAT42 does for the BAT54: the same diode in a through-hole DO-35,
which is easier to fit between GPIO5 and 3.3 V on this board. Not
modelled apart: the script's diode is a plain Schottky of 0.35 V at
3 mA and 10 pF, and the two data sheets differ by less than that
model is good for (0.4 V at most at 10 mA for either, 7 pF for
10).

## Simulated: a FET between GPIO5 and R5

With R5 and RBE at a few hundred ohms a released GPIO5 is tied to 5 V
through 430 Ω, and whatever in the pad might conduct above the pin's
range has milliamps to draw on (the user's point, 2026-11-16). A
small N-channel FET in the wire from the pin to R5, a cascode, keeps
the 5 V off the pin and leaves the stage as it was
(`bench/stage-sim.py --cascode 2n7000`, or `bss138`; a model, as all
of this):

```
 +5 V ────────────────────────┬──────────┐
                              │          │
                            [RBE]        │E
                              │          │
            D ┌────[R5]───────┴─────────B  Q1  S8550
              │                          │C
 +3.3 V ──G ──┤  QF  2N7000              │
              │      (or BSS138)         ├──────────┐
            S │                          │          │
              │                          │K       [SPK]  with RS in its
 GPIO5 ───────┘                     D1   ▲          │    lead if it needs one
 open-drain                              │A         │
                                         │          │
 GND ────────────────────────────────────┴──────────┘
```

QF's source to GPIO5, its gate to 3.3 V, its drain to the end of R5
that went to the pin. With the pin low the FET is on and R5 is pulled
low as before. With the pin released R5 and RBE take the drain to
5 V, so Q1's base goes all the way to its emitter, and the source
follows only until the FET shuts, a threshold under its gate. The
firmware does not change, and through a reset the pin floats, the FET
is off and RBE holds Q1 off.

The FET's models are onsemi's (`bss138.lib`, and `2n7002.lib` for the
2N7000, the same transistor in a TO-92). At rest, with 330 Ω and
100 Ω, and the patterns at 4 Mbit/s for the transistor as the model
has it:

| at the pin | released GPIO5 | Q1 at rest | between the rails | pulse error |
| --- | --- | --- | --- | --- |
| nothing | 5.00 V | off | 9 % | −0.02 to +0.35 |
| BSS138 | 2.00 V | off | 10 % | +0.08 to +0.47 |
| 2N7000 | 1.21 V | off | 9 % | +0.01 to +0.40 |

The sine at 4 Mbit/s (16 kHz × 256) into the 32 Ω speaker, with the
2N7000, over R5 and RBE:

| R5 | RBE | level | THD | SNR | SINAD | bits |
| --- | --- | --- | --- | --- | --- | --- |
| 330 Ω | 82 Ω | −0.4 dB | −26.2 dB | 45.3 dB | 26.1 dB | 4.0 |
| 330 Ω | 91 Ω | −0.1 dB | −34.5 dB | 49.5 dB | 34.3 dB | 5.4 |
| 330 Ω | 100 Ω | +0.1 dB | −40.3 dB | 52.4 dB | 40.0 dB | 6.4 |
| 330 Ω | 110 Ω | +0.3 dB | −30.4 dB | 51.1 dB | 30.4 dB | 4.8 |
| 330 Ω | 120 Ω | +0.4 dB | −26.3 dB | 48.4 dB | 26.3 dB | 4.1 |
| 390 Ω | 120 Ω | +0.1 dB | −37.6 dB | 52.3 dB | 37.5 dB | 5.9 |
| 390 Ω | 130 Ω | +0.2 dB | −30.4 dB | 51.0 dB | 30.4 dB | 4.8 |
| 470 Ω | 120 Ω | −0.6 dB | −27.0 dB | 45.5 dB | 26.9 dB | 4.2 |
| 470 Ω | 130 Ω | −0.3 dB | −33.5 dB | 48.8 dB | 33.4 dB | 5.3 |
| 470 Ω | 150 Ω | 0.0 dB | −33.8 dB | 51.7 dB | 33.7 dB | 5.3 |
| 470 Ω | 165 Ω | +0.1 dB | −27.6 dB | 49.3 dB | 27.6 dB | 4.3 |
| 470 Ω | 180 Ω | +0.3 dB | −24.5 dB | 46.4 dB | 24.5 dB | 3.8 |

The three ways to leave the pin, with 330 Ω, 100 Ω and D1:

| at the pin | released GPIO5 | THD | SNR |
| --- | --- | --- | --- |
| nothing | 5.00 V | −42.9 dB | 50.3 dB |
| 2N7000 | 1.21 V | −40.3 dB | 52.4 dB |
| BAT54 to 3.3 V | 3.65 V | −31.4 dB | 50.4 dB |

- **The FET costs 3 dB of THD and nothing else** for this
  transistor, and the pin rests at 1.2 V, well inside its range, with
  nothing flowing. Over the other gains and storage times it costs
  nothing: it is the most even of the stages, −36 to −41 dB at eight
  of the nine and −30 dB at the ninth, where the bare pin is −29 to
  −46 dB.
- **330 Ω and 100 Ω is the pair with it too**: THD −40 dB, 1.0 %,
  the noise 52 dB down, six and a half bits. The best ratio is 0.30
  of R5 at each R5 tried, and the lower R5 is the better: −40 dB at
  330 Ω, −38 dB at 390 Ω, about −34 dB at 470 Ω.
- **The ratio is sharp** for one transistor: a tenth more or less of
  RBE is 6 to 10 dB of THD. It is a balance of Q1's turn-on against
  its turn-off, which are the transistor's own, so on a clock the
  best RBE may be a step from 100 Ω, and a closer tolerance buys
  nothing: the transistor moves the balance further than a 5 % part
  does. The level and the noise hardly
  move over the table, and its worst rows are what the buzzer mod is
  today.
- **A 2N7000's threshold may be too high for a 3.3 V gate.** The data
  sheet has 0.8 to 3.0 V, typically 2.1. In the model it works to
  2.6 V with either pair, begins to fail at 2.8 V with 330 Ω (not
  yet with 470 Ω and 150 Ω, which asks 9 mA of it and not 13) and at
  3.0 V does not turn Q1 on at all. The check on a clock: with GPIO5
  low, QF's drain within 0.3 V of ground. A BSS138, 1.3 V in its
  model, has the margin, and a little more capacitance on R5's end.
- **Not a power FET.** The complements of an FDN306P (FDN327N,
  FDN337N) have hundreds of picofarads where these have tens, on a
  node that R5 and RBE have to pull up to 5 V every bit. Not
  modelled.

So, by the model: 330 Ω and 100 Ω, D1, and a 2N7000 or a BSS138
between GPIO5 and R5, the line at 4 Mbit/s. Against the kit's stage
(into the buzzer, so roughly) that is 7 dB louder, 1 to 1.5 % of
distortion for 14 % (3 % with the one transistor it likes least),
and the pin at 1.2 V and not 5.

The firmware's part: the pin in the model is the chip's strongest
drive, 18 Ω, and the firmware sets none, so GPIO5 runs at esp-hal's
default, the step below. Sinking 13 mA wants `DriveStrength::_40mA`
on GPIO5; without it the pin sat at 0.7 to 1 V while low on the
bench stage, which the model has not been run with. Tried on the
bench stage, 2026-11-16, and taken out again: the pin's low came
down to 0.35 V and the collector's pulses got worse, Q1 being
driven deeper into saturation ("Measured: the bench stage"). The
firmware stays at the default drive, and the model's 18 Ω pin is
stronger than the one that works.

## Simulated: Q1 at 3.3 V, GPIO5 push-pull

The same model with the stage rearranged (`bench/stage-sim.py
--push-pull`, 2026-11-16), and as little built: Q1's emitter to 3.3 V
and not 5 V, GPIO5 a push-pull pin, R5 with a capacitor across it,
and RBE only a 10 kΩ pull-up that holds Q1 off while the pin floats
through a reset. The pin now pulls the base back up itself and never
sees more than its own rail, where the open-drain stage's released
pin rests at 5 V.

What it costs before any figure:

- **Q1's emitter has to leave the 5 V rail**: a leg lifted or a track
  cut on the clock board and a wire to 3.3 V, and a firmware that
  drives the pin both ways, which on an unmodified clock would hold
  the buzzer on for good (3.3 V is still "on" to an emitter at 5 V).
- **The load's current comes from the board's 3.3 V regulator**, 100
  to 165 mA of it in pulses beside the Wi-Fi's, and the NTC's and
  LDR's dividers hang on the same rail.
- **3.6 dB on a 32 Ω load**, buzzer or speaker: 103 mA where 5 V
  gives 156. The 8 Ω 0.25 W speaker loses nothing: its RS goes from
  22 Ω to 12 Ω for the same 165 mA, and runs cooler.

The patterns, as for the open-drain stage (three loads, the gain from
120 to 300, the storage time from 20 to 150 ns; R5 with the capacitor
across it):

| R5 | capacitor | pulse error, 4 Mbit/s | between the rails | pulse error, 8 Mbit/s | between the rails | GPIO5 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 kΩ | none | −0.2 to +2.0 | 19 % | −0.1 to +2.0 | 33 % | 2.5 mA |
| 330 Ω | none | +0.2 to +2.1 | 8 % | +0.5 to +2.3 | 14 % | 7.7 mA |
| 2.2 kΩ | 100 pF | −0.07 to +1.0 | 29 % | −0.12 to +1.0 | 33 % | 1.2 mA |
| 1 kΩ | 100 pF | −0.04 to +2.0 | 14 % | −0.06 to +2.0 | 24 % | 2.5 mA |
| 1 kΩ | 220 pF | −0.05 to +1.6 | 12 % | −0.12 to +2.0 | 18 % | 2.5 mA |

With the transistor as the model has it (gain 178, 50 ns) the last
row is another matter: errors of 0.1 of a bit but for a lone 1, which
is up to 0.6 long, and Q1 between the rails 1 to 2 % of the time.

The sine, as above, into the 32 Ω speaker, R5 at 1 kΩ:

| clip | line | capacitor | level | THD | SNR | SINAD | bits |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 16 kHz × 128 | 2.048 Mbit/s | 100 pF | +0.3 dB | −22.9 dB | 41.4 dB | 22.8 dB | 3.5 |
| | | 220 pF | +0.9 dB | −43.7 dB | 58.9 dB | 43.5 dB | 6.9 |
| 31.25 kHz × 128 | 4.0 Mbit/s | 100 pF | +1.0 dB | −21.8 dB | 42.1 dB | 21.8 dB | 3.3 |
| | | 220 pF | +0.8 dB | −45.1 dB | 54.2 dB | 44.6 dB | 7.1 |
| 16 kHz × 256 | 4.096 Mbit/s | 100 pF | +1.0 dB | −21.9 dB | 44.5 dB | 21.8 dB | 3.3 |
| | | 220 pF | +0.8 dB | −44.3 dB | 53.7 dB | 43.8 dB | 7.0 |
| 31.25 kHz × 256 | 8.0 Mbit/s | 100 pF | +2.7 dB | −21.1 dB | 47.1 dB | 21.1 dB | 3.2 |
| | | 220 pF | +0.7 dB | −38.1 dB | 47.2 dB | 37.6 dB | 6.0 |

- **A push-pull pin alone is no gain.** With a bare resistor it turns
  Q1 on hard and has only the 0.7 V across R5 to take the charge out
  again: pulses up to two bits long.
- **1 kΩ with 220 pF is the best figure of any stage here**, for
  this transistor: THD −44 to −45 dB (0.6 %) at 2 and 4 Mbit/s, seven
  bits: one or two decibels better than the open-drain stage's −43 dB
  with the pin as measured, and −38 dB at 8 Mbit/s. It is the gain
  it does not mind (−42 to −44 dB from 120 to 300) and the storage
  it does: −32 dB at 20 ns, and −12 to −25 dB at 150 ns, the worst
  of any stage.
- **It is a capacitor matched to a transistor.** Its charge is what
  this Q1 stores, at this gain and 50 ns; 100 pF on the same Q1 is
  −22 dB, and the patterns with other gains and storage times are as
  bad as with no capacitor. On a clock it would be found on a scope
  or by ear, for that clock's transistor.
- **The BAT54 does not earn its place here either.** With it the
  figures no longer depend on the transistor or on the capacitor
  (Q1 between the rails 3 to 4 % at both rates), and they are THD −29
  to −33 dB at 2 and 4 Mbit/s and −23 to −26 dB at 8: no better than
  the open-drain stage without it. The script leaves it out unless
  given `--clamp`.

So, by the model, this stage is not worth its changes to the board
for the sound: 330 Ω and 100 Ω on the open-drain stage does as well
within two decibels with nothing moved and no capacitor to match.
What it has that the other has not is a pin that stays inside its
own rail, which the FET gives the open-drain stage too.

## Simulated: over the transistor's spread

The sine again, for every stage above, with Q1's gain at 120, 178
and 300 and its storage time at 20, 50 and 150 ns, the nine
transistors the patterns were run with (2026-11-16, after a review
of the sections above, whose sine tables are the middle one alone;
`bench/stage-sim.py --sine --stream 16000:256 --gain 300 --storage
150 330:100`, with `--cascode 2n7000`, `--pin-clamp` or `--push-pull
1000:10000:220` for the other stages; a run is eight minutes). The
line at 4.096 Mbit/s, a sine near 1 kHz at half of full scale, the
32 Ω speaker, the pin as measured. A model, as all of this.

THD in dB, the gain down and the storage time across:

| stage | gain | 20 ns | 50 ns | 150 ns |
| --- | --- | --- | --- | --- |
| 330 Ω, 100 Ω | 120 | −32.1 | −38.7 | −34.8 |
| | 178 | −34.0 | −42.7 | −33.4 |
| | 300 | −35.9 | −46.5 | −28.7 |
| 330 Ω, 100 Ω, 2N7000 | 120 | −36.2 | −40.5 | −41.3 |
| | 178 | −38.7 | −40.3 | −39.2 |
| | 300 | −41.0 | −39.3 | −30.5 |
| 330 Ω, 100 Ω, BAT54 from the pin to 3.3 V | 120 | −36.9 | −33.5 | −42.6 |
| | 178 | −36.0 | −31.4 | −35.2 |
| | 300 | −33.9 | −30.0 | −29.0 |
| 470 Ω, 150 Ω | 120 | −31.6 | −37.2 | −33.1 |
| | 178 | −34.1 | −42.2 | −30.7 |
| | 300 | −37.0 | −47.1 | −24.4 |
| 5.1 kΩ, 2.2 kΩ | 120 | −31.3 | −31.3 | −31.3 |
| | 178 | −34.8 | −35.2 | −35.6 |
| | 300 | −30.9 | −32.3 | −32.7 |
| push-pull, 1 kΩ with 220 pF | 120 | −32.3 | −42.5 | −24.5 |
| | 178 | −32.4 | −44.3 | −20.9 |
| | 300 | −32.6 | −42.3 | −12.3 |

The same as ranges over the nine, with the level and the noise:

| stage | THD, worst to best | THD, the middle one | SNR | level |
| --- | --- | --- | --- | --- |
| 330 Ω, 100 Ω | −28.7 to −46.5 dB | −34.8 dB | 46 to 54 dB | −0.3 to +1.7 dB |
| 330 Ω, 100 Ω, 2N7000 | −30.5 to −41.3 dB | −39.3 dB | 49 to 57 dB | −0.3 to +1.7 dB |
| 330 Ω, 100 Ω, BAT54 at the pin | −29.0 to −42.6 dB | −33.9 dB | 45 to 55 dB | −0.5 to +2.7 dB |
| 470 Ω, 150 Ω | −24.4 to −47.1 dB | −34.1 dB | 46 to 55 dB | −0.5 to +2.2 dB |
| 5.1 kΩ, 2.2 kΩ | −30.9 to −35.6 dB | −32.3 dB | 37 to 41 dB | −6.4 to +0.8 dB |
| push-pull, 1 kΩ with 220 pF | −12.3 to −44.3 dB | −32.4 dB | 42 to 56 dB | +0.7 to +3.1 dB |

And 330 Ω with 100 Ω at the nominal transistor at other levels: a
sine at a tenth of full scale is THD −32.6 dB and SNR 31.7 dB, at
nine tenths −32.2 dB and 55.9 dB, where half scale is −42.7 dB and
50.3 dB. So the null is one level's as well as one transistor's, and
the noise under a quiet passage is some 5 dB higher than under a
loud one.

- **The −43 dB was a null.** At 50 ns the bare stage's turn-on and
  turn-off balance and the THD is −39 to −47 dB; at 20 ns and at
  150 ns it is −29 to −36 dB. What to expect of 330 Ω and 100 Ω on a
  clock is 2 to 4 % and not 0.7 %.
- **The FET is the stage to build, and not for the pin alone.** It
  is the most even: 1 to 1.5 % at eight of the nine transistors,
  and its worst, 3 %, is the corner that is every low-value stage's
  worst. The nominal table had it 3 dB behind the bare pin; in the
  middle of the nine it is 4.5 dB ahead, with 2 dB less noise. Why
  it should be flatter has not been looked into.
- **The BAT54 at the pin costs no THD over the spread**, where the
  nominal table had 12 dB: the same range and the same middle as the
  bare pin. Its 3 mA at rest and the pin at 3.65 V are what is left
  against it.
- **A long storage time with a high gain is the corner to fear**:
  −24 to −31 dB for every stage at a few hundred ohms, with the
  level 1.7 to 2.7 dB up, which is pulses coming out long. A
  transistor that stores longer than 150 ns, which the model cannot
  rule out, would be worse; the scope on a clip's silence shows it
  as on-pulses wider than the pin's.
- **470 Ω and 150 Ω follows 330 Ω and 100 Ω** within 3 dB but for
  that corner, where it is 4 dB worse: the stronger pair is the one
  to take.
- **5.1 kΩ and 2.2 kΩ does not care about the storage**, since it
  hardly saturates, and its THD is −31 to −36 dB whatever the
  transistor: as good as the bare low-value stage in the middle.
  What it lacks is level at a low gain (−6.4 dB at 120, −3.4 dB at
  178, at the rail at 300, which is the clock that was heard) and 6
  to 15 dB of noise, and it runs Q1 at 0.4 W.
- **The low values are for the noise, the level and the heat**: 46
  to 57 dB of SNR for 37 to 41, a level that is the supply's and not
  the transistor's, and Q1 at 60 to 90 mW. The distortion is a gain
  only with the FET.
- **The noise goes with the storage time**: 52 to 57 dB at 20 ns,
  45 to 52 dB at 150 ns, for every stage at a few hundred ohms.
- **Push-pull is out.** Its capacitor is matched to the storage
  time, not the gain: −42 to −44 dB at 50 ns, −32 dB at 20 ns, −12
  to −25 dB at 150 ns.
- Still one load, one pitch and, but for the two rows above, one
  level; the FET at one threshold; R5 and RBE at one pair a stage,
  so no stage has had its RBE trimmed for each transistor, which on
  a clock it could be.

So, by the model, over the transistors: 330 Ω and 100 Ω, 5 % parts,
D1, a 2N7000 or a BSS138 between GPIO5 and R5, the line at
4 Mbit/s. Against 5.1 kΩ and 2.2 kΩ that is 4 to 10 dB less
distortion with eight of the nine transistors and 2 dB more with the
ninth, 9 to 18 dB less noise, the full level at any gain and a cool
Q1; without the FET it is the noise, the level and the heat, and the
distortion much as it was.

The one that is planned (the user's, 2026-11-16) is the diode and
not the FET: 330 Ω and 100 Ω, D1, and a BAT54 or a BAT42 from GPIO5
to 3.3 V, one part and two joints where the FET is three and a cut
in the wire to R5. By the table that is the bare stage's distortion,
noise and level, with the released pin at 3.65 V and not 5 V, for
3 mA into the 3.3 V rail whenever nothing plays.

## Measured: the bench stage, 2026-11-16

The first of this that is not a model: the breadboard stage
(`bench/`) on test-clock's GPIO5, with an S8550 and D1, an 8 Ω
speaker behind its series resistor for the load, a meter and a
Hantek 2C72. The scope's figures are read off photographs of its
screen, at 200 ns a division and some 8 ns a sample with both
channels on, so they are good to 20 ns or so and no better.

The transistors: the stage's S8550 reads hFE 290 on the meter's
socket, and the spares in the box 270. There is no low-gain part to
try, and these are the gain of the clock that was heard (near 300).

The pin at rest, nothing playing, with a meter:

| stage | released GPIO5 | across the load |
| --- | --- | --- |
| a clock, 5.1 kΩ, no RBE, D1 | about 4.5 V | 0 V |
| bench, 5.1 kΩ, 2.2 kΩ, D1 | 5 V | 0 V |
| bench, 5.1 kΩ, 2.2 kΩ, D1, BAT42 from GPIO5 to 3.3 V | 3.49 V | 0 V |
| bench, 330 Ω, 100 Ω, D1, BAT42 | 3.58 V | 0 V |

- **The pad conducts nothing above 3.3 V** at these currents: 5 V
  with RBE, and a junction under it without, where only the meter's
  10 MΩ draws base current. A diode to 3.3 V in the pad would have
  held both near 3.9 V. (A reading of 0 V on the first clock's pin
  earlier that day was a wrong one.) This is the pin
  `bench/stage-sim.py` has by default; it was read through 5 kΩ and
  more, and says nothing of what the pad does with the 4 mA that
  330 Ω and 100 Ω can give it.
- **The BAT42 holds the pin inside the data sheet's range**, which
  ends at 3.6 V: 3.49 V with 0.2 mA through it, 3.58 V with 3.3 mA,
  where the model's diode had 3.65 V.
- **Q1 stays off with the diode in**: 0 V across the load with
  either pair, the base 0.45 V under the emitter with 5.1 kΩ and
  2.2 kΩ and 0.33 V with 330 Ω and 100 Ω, by the currents.

Playing, 330 Ω, 100 Ω, D1 and the BAT42, a clip of the default
build: bits of about 122 ns, alternating ones at 4 MHz, which is
8 Mbit/s, the rate the model liked least.

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

- **Q1 switches at 8 Mbit/s with these values**: to both rails on
  every bit, a lone one included, and none is lost. The model had it
  between the rails a fifth of the time there; the edges are about
  what it had, and at this rate they are much of a bit.
- **The on-pulses are a fifth of a bit long** at 8 Mbit/s, some
  25 ns, which would be a tenth at 4 Mbit/s. That is a transistor
  with a short storage time, the better columns of "Over the
  transistor's spread", and not its worst corner, though at this
  gain the long one would have been.
- **The pin is weaker than the model's.** At esp-hal's default
  drive it sits at 0.7 to 1 V with 10 mA in it, where the model's
  18 Ω would be at 0.2 V.
- **A stronger pin is worse.** With `DriveStrength::_40mA` in
  `src/clip.rs` the low came down to 0.35 V and the collector let
  go later: the base has half as much current again and Q1 goes
  deeper into saturation. The two captures are not like for like,
  the second having caught runs of two bits on, which store more
  as well; but the collector looked fine at the default, and the
  firmware went back to it the same evening (the user's choice).
  So the weak pin is part of why these values work, and a
  transistor with this gain is overdriven by them: less base
  current, RBE at 91 or 82 Ω, is the direction if anything is to
  be trimmed.
- Not measured: the sound, Q1's heat, anything at 4 Mbit/s, another
  transistor.

## The figures, Q1 switching (steps 1 and 2)

Supply taken as 5.0 V with nothing lost in Q1, so the real figures are
a little lower. "Stuck on" is the fault case, "tone" a square wave half
the time on, "clip" the bitstream at its loudest sustained level
(silence in a clip is 3/4 of that figure for the first row: 0.20 W).

| | 32 Ω 0.5 W, no RS | 32 Ω 0.25 W, 27 Ω | 8 Ω 0.25 W, 22 Ω |
| --- | --- | --- | --- |
| current while on | 156 mA | 85 mA | 167 mA |
| speaker, stuck on | **0.78 W** | 0.23 W | 0.22 W |
| speaker, tone | 0.39 W | 0.12 W | 0.11 W |
| speaker, clip | 0.27 W | 0.08 W | 0.08 W |
| RS, stuck on | | 0.19 W | 0.61 W |
| RS, tone or clip (a clip is on half the time, whatever it plays) | | 0.10 W | 0.31 W |
| level against 32 Ω direct | 0 dB | −5.3 dB | −5.5 dB |
| Q1, worst case (not saturated) | 0.20 W | 0.11 W | 0.21 W |

- **32 Ω 0.5 W** is the one that needs nothing but the diode, and the
  loudest. The one figure over its rating is the stuck line, 0.78 W,
  which the firmware does not produce. To cover that too, put 10 Ω
  0.25 W in its lead: 0.45 W stuck on, at a cost of 2.4 dB.
- **32 Ω 0.25 W** without RS would be over its rating on every tone
  (0.39 W) and at it on a loud clip, so RS is required. 27 Ω holds it
  at 0.25 W up to a 5.25 V supply; 22 Ω is 0.8 dB louder and 10 % over
  the rating with the line stuck.
  RS itself is at 0.19 W at most, so an ordinary 0.25 W part.
- **8 Ω 0.25 W** must never go in bare: 625 mA through Q1 and from the
  board's supply. With 22 Ω it loads the stage as the 32 Ω speaker
  does and plays at the level of the 32 Ω 0.25 W one; most of the
  power is heat in RS: 0.31 W on a tone, so 0.5 W or two 47 Ω 0.25 W
  in parallel, not one 0.25 W part. With the line stuck on it is
  0.61 W, about 20 % over either, which runs hot and still protects
  the speaker; a 1 W part (a 2512 is 6 × 3 mm) covers that too.
  Of the three it is the poorest fit.
- Q1 is inside a TO-92's 625 mW in every case, saturated or not.

## The figures, Q1 linear (the alternative)

A gain of 200 assumed for R5, 0.2 V left across Q1 at the top, the
bias at half of what is left (2.4 V across LS1's pads). "Clip" is the
bias plus a tone at the clips' full scale; a stuck line and a tone
drive Q1 fully on, so those rows of the table above hold here too.

| | 32 Ω 0.5 W, no RS | 32 Ω 0.25 W, 27 Ω | 8 Ω 0.25 W, 22 Ω |
| --- | --- | --- | --- |
| R5 to start from | 5.6 kΩ (5.1 kΩ will do) | 10 kΩ (the kit's) | 5.6 kΩ (5.1 kΩ reads 2.5 V, a little high) |
| CBE | 47 nF | 22 nF | 47 nF |
| current, a clip's silence | 75 mA | 41 mA | 80 mA |
| speaker, a clip's silence | 0.18 W | 0.05 W | 0.05 W |
| speaker, clip | 0.25 W | 0.07 W | 0.07 W |
| RS, clip | | 0.06 W | 0.19 W |
| Q1, a clip's silence (its worst) | 0.20 W | 0.11 W | 0.21 W |
| base current while GPIO5 is low | 0.77 mA | 0.43 mA | 0.84 mA |

- The level is the switch's, within half a decibel: the same swing of
  current, got by another road. What changes is the distortion and
  where the heat is.
- **Q1 now runs warm on every clip**, 0.2 W for as long as it plays
  and most on silence, where the switch was at that only if it failed
  to saturate. Inside a TO-92's 625 mW, but it sits beside the
  thermometer: the firmware's hold on the reading while a clip plays
  is for exactly this.
- The 32 Ω 0.5 W speaker carries 0.18 W of steady current through any
  clip and its cone sits off centre for that long, as with the switch
  (0.20 W). The variant below takes that away.
- RS is still sized by the stuck line, so its values and ratings stay.

## The variant with a capacitor: no DC in the speaker

```
 +5 V ─────────────┬──────────┐
                   │          │
                 [RBE]        │E
                   │          │
 GPIO5 ───[R5]─────┴─────────B  Q1  S8550
                              │C
                              │
              ┌───────┬───────┤  LS1 +
              │       │       │+
              │K      │     [C1]  100 µF, 10 V or more
         D1   ▲     [RP]      │
              │A      │     [SPK]
              │       │       │
 GND ─────────┴───────┴───────┘  LS1 −
```

A capacitor in series with the speaker and a pull-down RP from Q1's
collector to ground, in place of RS. Q1 charges C1 through the speaker
and RP discharges it through the speaker, so the speaker carries the
audio and no steady current. C1 is an electrolytic with its `+` to the
collector; 47 µF is enough for a 32 Ω speaker. D1, R5 and RBE are as
above. The tables are for Q1 switching; the linear stage is after
them.

What it gives that the series resistor does not:

- **A stuck line cannot hurt the speaker.** Once C1 has charged, no
  current flows, whatever the speaker's rating. The 32 Ω 0.25 W
  speaker needs no RS.
- **The cone rests at its centre.** Without C1 it is pushed out by the
  mean current for the whole length of a clip, which uses up travel
  on one side and adds distortion on a small speaker.
- **The speaker heats from the sound alone**, not from the 0.2 W of
  steady current a clip's silence is in a bare 32 Ω speaker.

What it costs: the stage only pushes, so RP is the one way C1
discharges and sets the other half of the audio current. To give a
useful level it must be about the speaker's own resistance, and then
it lies across the 5 V whenever Q1 is on. For a 32 Ω speaker,
calculated with the coil's inductance left out:

| RP | level against 32 Ω direct | RP, clip or tone | RP, stuck on | current while on |
| --- | --- | --- | --- | --- |
| 33 Ω, 1 W | −3.5 dB (45 % of the power) | 0.42 W | 0.76 W | 200 mA |
| 100 Ω, 0.5 W | −8 dB (16 %) | 0.17 W | 0.25 W | 80 mA |
| 1 kΩ | about −25 dB | | | 10 mA |

The two 32 Ω speakers are the same here, since the speaker's own
power is under 0.1 W with either RP: the rating no longer decides
anything. The 8 Ω 0.25 W speaker, with no RS and C1 at 220 µF:

| RP | level against 32 Ω direct | speaker | RP, clip or tone | RP, stuck on | current while on |
| --- | --- | --- | --- | --- | --- |
| 33 Ω, 1 W | −3.7 dB (43 %) | 0.08 W | 0.55 W | 0.76 W | 255 mA |
| 47 Ω, 1 W | −5.9 dB (26 %) | 0.05 W | 0.41 W | 0.53 W | 185 mA |

That is the 8 Ω speaker at the level its 22 Ω RS gives it (−5.5 dB)
with 47 Ω, or 1.8 dB louder with 33 Ω at half as much current again,
and RP need not be near the speaker's 8 Ω: it only has to pass the
discharge.

- Three parts on LS1's pads in place of one, the resistor larger and
  hotter than any RS above, and with 33 Ω more supply current than
  the bare speaker draws.
- For the 32 Ω 0.25 W speaker, 33 Ω here is 1.8 dB louder than its
  27 Ω RS, for a 1 W resistor; 100 Ω is 2.7 dB quieter.
- A tone starts with a thump as C1 charges and ends with one as it
  discharges; only the clips ramp up from and down to the released
  line.
- With 33 Ω the current while on is past what 5.1 kΩ at R5 saturates
  a switching Q1 for, so this variant belongs with step 2.

**With Q1 linear (the alternative)** RP is the collector's load for the steady
current and the speaker takes only the changes, as in any class A
stage with a coupled load. The bias that gives both halves of the wave
the same room is no longer half the supply:

| speaker, RP | bias across LS1's pads | current, silence | R5 to start from (gain 200) | level against 32 Ω direct | RP, silence | Q1, silence |
| --- | --- | --- | --- | --- | --- | --- |
| 32 Ω, 33 Ω 1 W | 3.2 V | 97 mA | 4.3 kΩ | −4.0 dB | 0.31 W | 0.17 W |
| 32 Ω, 100 Ω 0.5 W | 3.9 V | 39 mA | 11 kΩ | −8.5 dB | 0.15 W | 0.04 W |
| 8 Ω, 33 Ω 1 W | 4.0 V | 122 mA | 3.6 kΩ | −4.0 dB | 0.49 W | 0.12 W |
| 8 Ω, 47 Ω 1 W | 4.2 V | 89 mA | 4.7 kΩ | −6.2 dB | 0.37 W | 0.07 W |

A bias that close to the rail leaves the ripple no room. RP gives it
a path the bare coil does not (a few milliamps in 33 Ω is a few
tenths of a volt, so nothing reaches D1), but with the 8 Ω speaker at
4.0 or 4.2 V the model has Q1 against the rail for a tenth of the
time, on the ripple's peaks. Either fit CS, 220 nF across the pads
(with 22 nF at CBE: no saturation, 0.5 % at 1 kHz and 2.6 % at 5 kHz
with 33 Ω, 0.7 % and 3.5 % with 47 Ω), or raise R5 by 15 % (4.0 kΩ
and 5.6 kΩ: a bias of 3.5 and 3.6 V, no saturation, 1.2 dB quieter,
1.3 % and 5.3 % with 47 nF alone). The 32 Ω rows have the room as
they are. The 8 Ω speaker itself carries 0.03 W at the loudest.

The levels are the switching tables' within half a decibel; RP runs a little cooler
on a clip, and Q1 takes a share of the heat. The trim is the same
reading with the new target: the new R5 is the old one times the
reading over the bias in the table (aim a tenth under it, for the
warm transistor). The thump of C1 charging is gone from the clips,
whose ramp from the released line is slower than C1; a tone still
has it.

The series resistor stays the recommendation for the fewest parts.
This variant is the one to take when the speaker's safety against a
stuck line or its centred cone matters more than a 1 W resistor. A
second transistor pulling the collector down in RP's place would give
the same with no heat and no loss of level, and is no longer a small
change.

## The rest of the board

- The speaker draws 85 to 167 mA in pulses from the 5 V rail (with
  the linear stage, 40 to 80 mA steadily for a clip's length), on top of the
  panel and Wi-Fi. Do the README's C3 replacement (10 µF) and use
  a 1 A supply; the board is known to brown out on less.
- No firmware change is needed. The clips in `clips/` have their bass
  cut and limiter set for the buzzer and will sound thin and
  compressed on a speaker. `scripts/speaker-clips.sh` makes the
  default pack for one, `packs/clips-default-speaker.bin`: the same
  four clips under the same names, the three modules with the
  high-pass at 100 Hz and the limiter letting go over 128 ms, so
  that it follows the tune's level and not its waveform, and
  `scripts/clips.sh <ip> packs/clips-default-speaker.bin` sends it.
  For other clips the options are in `AGENTS.md` (`--highpass`,
  `--release`, `--knee`, `--flat`, `CLIP_HIGHPASS`). Keep some
  high-pass, 100 to 300 Hz: a speaker this size does nothing with the
  bass but heat up. With no limiter at all the clips were too quiet.

## The speaker in the lid

The raised lid (`lid/lid.scad`) has a socket for a round speaker,
switched on with `speaker=true`:

```
openscad -o lid-speaker.stl -D speaker=true lid/lid.scad
```

`lid/lid-speaker-printed.stl` is the one printed and fitted on
2026-11-15. The socket is a ring on the lid's inner face, in the
middle, where the original lid has its own: 30 mm inside and 5 mm
deep, with a 10 mm cut for the wires facing the button end, where the
buzzer's pads are. Inside it is a ledge 1 mm high and 1.5 mm wide (27 mm
inside) that the speaker's rim sits on, so the cone stays clear of the
plate and the rim seals all round. The lid's middle group of vent
slots lies inside the ring and is the speaker's grille. The figures
are parameters at the top of the file (`speaker_d`, `speaker_depth`,
`speaker_seat_d`, `speaker_seat_h`, `speaker_wire_w`,
`speaker_wire_a`).

The speaker goes in cone towards the vents. One sold as 29 mm
measured 28.4 mm, and is held by a ring printed in TPU,
`speaker_shim.3mf`: 28 mm inside, 30.2 mm outside, 5 mm tall, pushed
over the speaker and then into the socket. That one needed no glue. If
the fit is not tight, scale the ring in the slicer, or hold it with a
drop or two of hot glue.

## Check after fitting

1. Nothing playing: 0 V across LS1's pads. Anything else means Q1 is
   not fully off; stop and look at RBE, R5 and GPIO5.
2. A clip playing, scope on Q1's collector: pulses from 0 V to within
   0.3 V of the rail (saturated) or to a lower flat top (not
   saturated: a smaller R5), and no excursion below −1 V (D1 works).
   With the linear stage and the 220 nF across the pads: the sound's
   own waveform around the bias with some millivolts of ripple on it,
   never flat against the rail on the loud passages (R5 too small).
   Without the 220 nF the ripple reaches both the rail and −0.6 V at
   the bit rate, and only the mean under it is the sound. A
   multimeter on DC across the pads reads the bias there, 2.0 to
   2.4 V without C1.
3. After the alarm has rung for its five minutes: the speaker and Q1
   warm at most, RS hot and no more than that (0.3 W behind the 8 Ω
   speaker), the 5 V rail without dips, no self-heal or reset in the
   log.

## To do

On the bench stage with 330 Ω, 100 Ω and the BAT42 (see "Measured"):

- By ear against 5.1 kΩ and 2.2 kΩ: the hiss on a clip's silence
  and on a quiet clip, the one difference the model says might be
  heard.
- Q1's heat after some minutes of a clip.
- The two-channel capture at 4 Mbit/s (`CLIP_PDM_BITS=128`), and on
  a clip's silence at either rate, where the pattern is the same
  every time.
- RBE a step or two down, 91 or 82 Ω, if the on-pulses' length
  turns out to matter.

# Speaker in place of the buzzer

Three small speakers in LS1's place, with as little done to the clock
board as each needs. A paper design: the figures below are calculated,
none of the three has been fitted to a clock or heard. The "check after
fitting" section says what to measure before trusting it.

The priorities, in this order: few changes to the board, nothing
overloaded (speaker, Q1, GPIO5, the 5 V supply), then the sound.

## The short version

The one that has been on a clock (2026-11-15), and the easiest with a
sound that is good enough:

- the speaker in LS1's pads, with RS in its lead unless it is the
  32 Ω 0.5 W one;
- D1, a 1N4148, across the pads;
- CBE, 47 nF or 33 nF, across Q1's base and emitter legs;
- R5 left as the kit has it, 10 kΩ.

The 32 Ω 0.5 W speaker is the one to want: no RS, nothing that gets
hot, and 6 dB more than the 8 Ω 0.25 W one, which works too, behind
its 22 Ω (two 47 Ω in parallel on the clock tried).

10 kΩ is right for a load near 30 Ω (the bare 32 Ω speaker, or 8 Ω
and its RS) and a transistor like the one tried, whose gain was near
300; one with less gain is only quieter. The 32 Ω 0.25 W speaker
behind 27 Ω is twice the load and wants about 20 kΩ. Read the DC
voltage across the pads while a clip plays: 2.0 to 2.8 V is in order,
3 V or more (or an RS too hot to touch) wants a larger R5. The rest
of this page is the reasoning, the figures and the other variants.

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
                 [CBE]        │E
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
| R5 | 5.1 kΩ | 5.1 kΩ | 5.1 kΩ | R5's place; the kit's 10 kΩ also works, quieter; with CBE it is trimmed, see "Step 2" |
| CBE | optional, 47 nF | optional, 47 nF | optional, 47 nF | see "Step 2" |
| CS | with CBE, 220 nF | with CBE, 220 nF | with CBE, 220 nF | across LS1's pads, see "Step 2" |

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

**Step 2, optional, for the sound: Q1 as a linear stage, CBE = 47 nF
and R5 to suit the transistor.** CBE is a ceramic capacitor from Q1's
base to its emitter (+5 V), on the solder side across the two legs; it
never sees more than 0.7 V, so any voltage rating will do.

With step 1 alone Q1 is a switch. 5.1 kΩ gives it 0.84 mA of base
current, which saturates it at 85 mA only if its gain is over 100 and
at 160 mA only over 190, so what it does depends on the transistor in
hand; and where it does saturate, nothing takes the base's charge out
when GPIO5 lets go, which on the breadboard copy of the stage
(`bench/`, 2026-11-13, a BC557 with 50 mA) stretched every pulse and
distorted the clips with heavy bass until a resistor from base to
emitter went in.

Step 2 goes the other way and keeps Q1 out of saturation altogether.
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
  at the quiet end. The earlier form of this step was such a pair
  (R5 2.2 kΩ, RBE 1 kΩ: Q1 switching hard with a way out for the
  charge); it is the alternative if the linear stage disappoints.
- Tones and beeper clips are as loud as before: with GPIO5 low for a
  whole half-wave Q1 goes to the edge of saturation as the switch did,
  its edges rounded over some 20 µs.
- Released, the base still hangs on GPIO5 through R5 as it always
  has; CBE adds nothing to that. After a clip the last of the
  collector current dies away as 1/t: 1 mA after a quarter of a
  millisecond, 1 µA after a quarter of a second.

None of this has been on a clock or a breadboard. The model is one
equation for the base and a current-limited coil (no Early effect, no
gain that falls with current, a second-order modulator standing in
for the chip's), so the percentages rank the capacitors and do not
predict a measurement.

## The figures, Q1 switching (step 1 alone)

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

## The figures, Q1 linear (step 2)

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
                 [CBE]        │E
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
collector; 47 µF is enough for a 32 Ω speaker. D1, R5 and CBE are as
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

**With Q1 linear (step 2)** RP is the collector's load for the steady
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
  step 2, 40 to 80 mA steadily for a clip's length), on top of the
  panel and Wi-Fi. Do the README's C3 replacement (10 µF) and use
  a 1 A supply; the board is known to brown out on less.
- No firmware change is needed. The clips in `clips/` have their bass
  cut and limiter set for the buzzer and will sound thin and
  compressed on a speaker. `scripts/speaker-clips.sh` makes the
  default pack for one, `packs/clips-default-speaker.bin`: the same
  three clips under the same names, the two modules with the
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
   not fully off; stop and look at R5 and GPIO5.
2. A clip playing, scope on Q1's collector. Step 1 alone: pulses from
   0 V to within 0.3 V of the rail (saturated) or to a lower flat top
   (not saturated), and no excursion below −1 V (D1 works). With
   step 2 and the 220 nF across the pads: the sound's own waveform
   around the bias with some millivolts of ripple on it, never flat
   against the rail on the loud passages (R5 too small). Without the
   220 nF the ripple reaches both the rail and −0.6 V at the bit
   rate, and only the mean under it is the sound. A multimeter on DC across the pads reads the bias, 2.0
   to 2.4 V without C1.
3. After the alarm has rung for its five minutes: the speaker, RS and
   Q1 warm at most, the 5 V rail without dips, no self-heal or reset
   in the log.

# Speaker in place of the buzzer

Three small speakers in LS1's place, with as little done to the clock
board as each needs. A paper design: the figures below are calculated,
none of the three has been fitted to a clock or heard. The "check after
fitting" section says what to measure before trusting it.

The priorities, in this order: few changes to the board, nothing
overloaded (speaker, Q1, GPIO5, the 5 V supply), then the sound.

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
| RBE | optional | optional | optional | see "Step 2" |

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

**Step 2, optional, for the sound: R5 = 2.2 kΩ and RBE = 1 kΩ.** RBE
goes from Q1's base to its emitter (+5 V), on the solder side across
the two legs. With 5.1 kΩ alone Q1 gets 0.84 mA of base current, which
saturates it at 85 mA only if its gain is over 100 and at 160 mA only
over 190, so the speaker's level depends on the transistor in hand;
and where it does saturate, nothing takes the base's charge out when
GPIO5 lets go, which on the breadboard copy of the stage (`bench/`,
2026-11-13, a BC557 with 50 mA) stretched every pulse and distorted
the clips with heavy bass until a base-emitter resistor went in.
2.2 kΩ and 1 kΩ give the base about 1.1 mA (GPIO5 sinks 1.9 mA) with a
way out for the charge. Keep RBE under half of R5: GPIO5 is a 3.3 V
pin on a 5 V stage, and RBE must not pass enough through the released
pin to turn Q1 on. These two values have not been on a clock; the
bench had 5.1 kΩ and 2.2 kΩ, which is the cautious pair to start from
and enough for the 32 Ω 0.25 W speaker's 85 mA.

## The figures

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
| RS, tone | | 0.10 W | 0.31 W |
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
above.

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

- Three parts on LS1's pads in place of one, the resistor larger and
  hotter than any RS above, and with 33 Ω more supply current than
  the bare speaker draws.
- For the 32 Ω 0.25 W speaker, 33 Ω here is 1.8 dB louder than its
  27 Ω RS, for a 1 W resistor; 100 Ω is 2.7 dB quieter.
- A tone starts with a thump as C1 charges and ends with one as it
  discharges; only the clips ramp up from and down to the released
  line.
- It does not help the 8 Ω speaker: RP would be some 10 Ω, 0.5 A
  through Q1 whenever it is on, so it needs RS as well.

The series resistor stays the recommendation for the fewest parts.
This variant is the one to take when the speaker's safety against a
stuck line or its centred cone matters more than a 1 W resistor. A
second transistor pulling the collector down in RP's place would give
the same with no heat and no loss of level, and is no longer a small
change.

## The rest of the board

- The speaker draws 85 to 167 mA in pulses from the 5 V rail, on top of
  the panel and Wi-Fi. Do the README's C3 replacement (10 µF) and use
  a 1 A supply; the board is known to brown out on less.
- No firmware change is needed. The clips in `clips/` have their bass
  cut and limiter set for the buzzer and will sound thin and
  compressed on a speaker; converting them again with a lower
  high-pass and the limiter off is in `AGENTS.md` (`--highpass`,
  `--knee`). Keep some high-pass, 100 to 300 Hz: a speaker this size
  does nothing with the bass but heat up.

## Check after fitting

1. Nothing playing: 0 V across LS1's pads. Anything else means Q1 is
   not fully off; stop and look at RBE and R5.
2. A clip playing, scope on Q1's collector: pulses from 0 V to within
   0.3 V of the rail (saturated) or to a lower flat top (not
   saturated; step 2 raises it), and no excursion below −1 V (D1
   works).
3. After the alarm has rung for its five minutes: the speaker, RS and
   Q1 warm at most, the 5 V rail without dips, no self-heal or reset
   in the log.

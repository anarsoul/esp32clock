# A PDM output on GPIO20 into a class D amplifier module

Design on paper, 2026-11-18. Nothing here is built or heard yet; when
it is, this moves to where the speaker work is recorded.

GPIO20 is push-pull at 3.3 V and carries the PDM stream at 4 or 8 MHz.
A two-stage RC filter takes it to the input of a PAM8302A (the
CJMCU-832 board) or a PAM8403 module with a volume pot. The drawings
are `pdm-pam8302.svg` and `pdm-pam8403.svg` (and their `.png`).

## From the datasheets

Diodes Inc., PAM8302A (May 2021) and PAM8403 (DS36439 rev. 2-3,
September 2020).

| | PAM8302A | PAM8403 |
| --- | --- | --- |
| gain | 23.5 dB (×15), `20·log(RF/RI)` | 24 dB (×16), `20·log(2·RF/RI)` |
| RF, RI | 150 kΩ, 10 kΩ on each input | 142 kΩ, 18 kΩ |
| input | differential | single-ended |
| oscillator | 200 / 250 / 300 kHz | 260 kHz |
| PSRR at 1 kHz | 50 dB | 58 dB |
| supply | 2.0 to 5.5 V | 2.5 to 5.5 V |
| input capacitor | 0.1 to 0.22 µF recommended | `fC = 1/(2π·RI·CI)` |

- An external resistor in series with a PAM8302A input lowers the
  gain: `20·log(150k/(10k + RIN))`.
- The PAM8403's datasheet warns that too large an input "will damage
  the device" once the output clips.
- The PAM8403's datasheet says nothing of an unused channel.
- At 5 V the PAM8302A gives 1.3 W into 8 Ω at 1 % THD, which is
  ±4.56 V on the speaker: the clipping threshold used below.

Not in the datasheet, my estimate: with A− at AC ground the PAM8302A's
A+ loads its source with about 19 kΩ, `RI / (1 − RF / (2·(RI + RF)))`,
not 10 kΩ.

## The CJMCU-832

Taken as a copy of Adafruit's PAM8302 board, which the sellers'
descriptions fit; no schematic of the CJMCU-832 itself was found.
Adafruit's schematic (rev. B) has a 10 kΩ trimmer across A+ and A−,
its wiper through 1 µF and a series resistor to IN_P, and A− through
1 µF and a series resistor to IN_N. The series resistors' value could
not be read off the drawing (it looks like 100 Ω).

Check on the board, unpowered:

1. A+ to A− reads about 10 kΩ whatever the trimmer's position. If it
   reads open, there is no trimmer across the input: R3 is then 2.7k.
2. The two resistors between the capacitors and the chip. Marked
   `103` (10 kΩ) they halve the gain to ×7.5: R3 is then about 10k
   and C2 2.2n.

## Filter for the CJMCU-832

```
GPIO20 ──R1 2k2──┬──R2 10k──┬──────┬── A+
                 │          │      │
              C1 4n7     C2 4n7  R3 3k3
                 │          │      │
GND ─────────────┴──────────┴──────┴── A−
```

The load on node b is R3, the trimmer's 10 kΩ and the amplifier's
19 kΩ in parallel, 2.2 kΩ.

| | trimmer at maximum |
| --- | --- |
| division | 0.152 (−16.4 dB) |
| at the input, full-scale PDM | 0.50 V peak to peak |
| on the speaker | about ±3.8 V, 0.9 W into 8 Ω |
| −3 dB | 11.6 kHz |
| at 250 kHz | −45 dB |
| at 4 MHz | −93 dB |

- No coupling capacitors of our own: the board has them.
- The trimmer only turns it down. Without R3 the trimmer alone would
  divide, but C2 would then see 5.5 kΩ and the corner would fall to
  about 5.5 kHz.
- A− goes to the filter's ground, the foot of C2 and R3, as a wire
  beside A+, not to the amplifier's ground: the differential input
  then takes the ground's noise off.
- A board with no trimmer: R3 2.7k gives 0.162, 0.53 V peak to peak,
  ±4.0 V, 11.3 kHz. With 3.3k it would sit on the clipping threshold.
  Such a board needs 100 nF in each input if it has none.

## Filter for the PAM8403 module

```
GPIO20 ──R1 1k──┬──R2 4k7──┬── L in (the pot)
                │          │
             C1 6n8     C2 1n5
                │          │
GND ────────────┴──────────┴── GND
```

The pot's value on the module is not known; both likely ones are
given.

| | 50 kΩ pot | 10 kΩ pot |
| --- | --- | --- |
| division | 0.90 | 0.64 |
| −3 dB | 13.8 kHz | 16.9 kHz |
| at 250 kHz | −41 dB | −38 dB |
| at 4 MHz | −89 dB | −86 dB |

The amplifier clips above about 0.63 V peak to peak at its input
(5 V · 2 / 16), so with nearly 3.3 V at the pot the usable travel is
the first fifth, and the datasheet's warning applies above it. The
divider that was not asked for, should the pot prove too touchy: R3
3.3k across C2, with C1 10n and C2 3.3n.

| with R3 | 50 kΩ pot | 10 kΩ pot |
| --- | --- | --- |
| division | 0.35 | 0.30 |
| −3 dB | 13.0 kHz | 13.8 kHz |
| at 250 kHz | −43 dB | −41 dB |

- The unused channel's input goes to ground at the module's pin,
  behind its coupling capacitor, never at the chip's pin, which sits
  at VDD/2.
- The outputs are bridged: neither speaker wire to ground.

## How the figures were reached

With RL the load on node b, the network is

```
Vs/Vb = (1 + s·R1·C1)·(1 + R2/RL + s·R2·C2) + R1/RL + s·R1·C2
```

so the division at DC is `1 / (1 + (R1 + R2)/RL)`. The tables are
this evaluated numerically (the corner and the two attenuations read
off the magnitude), not the two stages' corners taken separately:
the stages load each other.

Two stages because a sigma-delta's noise rises with frequency and one
pole only levels it; what is left around the amplifier's 250 kHz and
its harmonics is folded back into the audio band by the class D
modulator.

The corner goes as the inverse of the capacitors. If the clips need
nothing above 8 kHz, capacitors 1.5 times larger take it there.

## What the filter cannot fix

- A GPIO as a one-bit DAC has no supply rejection: whatever is on the
  3.3 V rail, the Wi-Fi's transmit bursts for one, is in the audio at
  full level. The amplifier's own PSRR does not help.
- 4 or 8 MHz is the same to the filter. 8 MHz has less noise in band
  and twice the edges, and a push-pull pin's unequal edges are
  distortion; 4 MHz first.
- R1 sits at the pin, so the edges do not travel along a wire beside
  the antenna.

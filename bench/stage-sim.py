#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""The buzzer's stage under a clip's bitstream, in ngspice: R5 and RBE.

Q1 (S8550, emitter at +5 V) driven by GPIO5 as an open-drain pin
through R5, with RBE from base to emitter and D1 across the load. For each pair of values
it runs four bit patterns at each bit rate into each load and prints
how far the collector's pulses are from the pin's, in bits, and how
long Q1 spends between the rails.

    bench/stage-sim.py                       # the table in speaker-mod/README.md
    bench/stage-sim.py 5100:2200 330:100     # these pairs of R5:RBE (0 for no RBE)
    bench/stage-sim.py --rate 8e6 270:82     # at 8 Mbit/s and not the default 4.096
    bench/stage-sim.py --clamp 330:100       # also with a BAT54 from collector to base
    bench/stage-sim.py --pin-clamp 330:100   # a BAT54 from GPIO5 to 3.3 V
    bench/stage-sim.py --pad-diode 330:100   # the pin as first assumed, a diode to 3.3 V in its pad
    bench/stage-sim.py --cascode bss138 330:100   # a BSS138 between the pin and R5, its gate at 3.3 V
    bench/stage-sim.py --cascode 2n7000 --cascode-vto 3.0 470:150   # a 2N7000 there, at its highest threshold
    bench/stage-sim.py --no-flyback --load "buzzer 32R, lossy" 10000:0   # the kit's stage: no D1

The BAT54 is off unless asked for: it made the result independent of
the transistor and no better (2026-11-16), see speaker-mod/README.md.

With --sine it plays a sine through a sigma-delta modulator of its
own and through the stage, and prints what the stage adds in the
audio band, beside the bitstream's own figures:

    bench/stage-sim.py --sine                          # 330:100 and 5100:2200, at 2 and 4 Mbit/s
    bench/stage-sim.py --sine --stream 31250:128 330:100
    bench/stage-sim.py --sine --order 1 --level 0.3 --load "buzzer 32R" 330:100

With --push-pull the stage is another one: Q1's emitter at 3.3 V and
GPIO5 a push-pull pin, which pulls the base back up through R5 itself
and never sees more than its own rail. RBE is then only what holds Q1
off through a reset, a third figure is a capacitor across R5 in pF,
and the 8 ohm speaker's RS is 12 ohms, for the current it has behind
22 at 5 V:

    bench/stage-sim.py --push-pull                     # the pairs tried
    bench/stage-sim.py --push-pull --sine 1000:10000:100

The transistor is onsemi's SS8550 model (ss8550.lib, Fairchild, 2004;
the clock's S8550 is taken to be like it). It has no TR, so no storage
time: that is swept here, with the gain (--sine takes one of each).
The pin is the ESP32-C3 data sheet's (v2.5, table 5-4): 28 mA at
0.495 V with PAD_DRIVER = 3, so an 18 ohm switch, though esp-hal's
default is the step below (and 40 mA at 2.64 V driving high, 16.5 ohms),
with 8 pF for the pin's 2 pF and the track.
Without D1 the coil's current has to go somewhere, and the model has
two places for it: a 30 V diode across Q1 for its breakdown, and, in
the load "buzzer 32R, lossy", 180 ohms across the coil, which is what
makes 70 mA kick the collector to the -10 to -15 V seen on the scope
with no diode on the board (2026-11-12). That load is the one to use
with --no-flyback; what the buzzer's losses really are is not known.
The pin has no diode to 3.3 V in its pad: a released GPIO5 reads 5 V
on the bench stage and a junction under it on a clock with no RBE
(2026-11-16). --pad-diode puts one in, as the first runs had it, which
holds a released pin near 3.9 V and passes current through R5 and RBE
for good.
What is assumed and not measured: the storage time, the loads'
inductances, and for --sine the modulator: the chip's converter is
not documented, so the bitstream is a textbook loop's and not the
chip's.
Needs ngspice and numpy.
"""
import argparse
import itertools
import os
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor

import numpy as np

# Ohms across a load's coil, for its losses: see --no-flyback above.
LOSSY = "buzzer 32R, lossy"
LOSS_OHMS = 180
# name: (the speaker's ohms, RS in its lead, henries)
LOADS = {
    "buzzer 32R": (32.0, 0.0, 3e-3),
    "speaker 32R": (32.0, 0.0, 300e-6),
    "speaker 8R + 22R": (8.0, 22.0, 50e-6),
    LOSSY: (32.0, 0.0, 3e-3),
}
# With D1 across the load, as every build but the kit's has it.
FLYBACK = True
# The same at 3.3 V: the 8 ohm speaker with the RS that gives it 5 V's current.
LOADS_3V3 = {
    "buzzer 32R": (32.0, 0.0, 3e-3),
    "speaker 32R": (32.0, 0.0, 300e-6),
    "speaker 8R + 12R": (8.0, 12.0, 50e-6),
}
# The stage: Q1's emitter's volts, and whether the pin drives high too.
SUPPLY = 5.0
PUSH_PULL = False
# A BAT54 from the pin to 3.3 V, to take the released pin's current off the pad.
PIN_CLAMP = False
# A diode to 3.3 V in the pin's pad, as first assumed. The bench stage's released
# GPIO5 reads 5 V (2026-11-16), so there is none, and it is off unless asked for.
PAD_DIODE = False
# A BSS138 between the pin (its source) and R5 (its drain), gate at 3.3 V: the
# pin pulls R5 low through it and never rises past 3.3 V less a threshold, while
# R5's end goes to 5 V. onsemi's bss138.lib (BSS138/FAI) at 25 degrees.
CASCODE = None
# The threshold in a cascode's model, for a part at the end of its spread.
CASCODE_VTO = None
CASCODES = {}
CASCODES["bss138"] = """.SUBCKT CASC 20 10 30
Rg 10 1 1
M1 2 1 3 3 DMOS L=1u W=1u
.MODEL DMOS NMOS(VTO=1.3 KP=0.65 THETA=0.086 VMAX=2.2E5 LEVEL=3)
Cgs 1 3 40p
Rd 20 4 0.2
Dds 3 4 DDS
.MODEL DDS D(BV=50 M=0.36 CJO=23p VJ=0.8)
Dbody 3 20 DBODY
.MODEL DBODY D(IS=1.4E-13 N=1 RS=40m TT=100n)
Ra 4 2 0.3
Rs 3 5 10m
Ls 5 30 .5n
M2 1 8 6 6 INTER
E2 8 6 4 1 2
.MODEL INTER NMOS(VTO=0 KP=10 LEVEL=1)
Cgdmax 7 4 68p
Rcgd 7 4 10meg
Dgd 6 4 DGD
Rdgd 4 6 10meg
.MODEL DGD D(M=0.3 CJO=68p VJ=0.4)
M3 7 9 1 1 INTER
E3 9 1 4 1 -2
.ENDS
"""
# onsemi's 2n7002.lib (F2N7002, Fairchild, 2007) at 25 degrees; the 2N7000 is
# the same transistor in a TO-92. The data sheet's threshold is 0.8 to 3.0 V.
CASCODES["2n7000"] = """.SUBCKT CASC 20 10 30
Rg 10 1 1.58
M1 2 1 3 3 DMOS L=1u W=1u
.MODEL DMOS NMOS(VTO=2.10 KP=0.2245 THETA=0.04 VMAX=5.0E5 ETA=0.001 LEVEL=3)
Cgs 1 3 16p
Rd 20 4 0.13
Dds 3 4 DDS
.MODEL DDS D(BV=60 M=0.38 CJO=25p VJ=0.48)
Dbody 3 20 DBODY
.MODEL DBODY D(IS=3.0E-14 N=1.0 RS=2.015 EG=1.16 TT=52n)
Ra 4 2 0.13
Rs 3 5 0.01
Ls 5 30 1.0n
M2 1 8 6 6 INTER
E2 8 6 4 1 2
.MODEL INTER NMOS(VTO=0 KP=10 LEVEL=1)
Cgdmax 7 4 46p
Rcgd 7 4 10meg
Dgd 6 4 DGD
Rdgd 4 6 10meg
.MODEL DGD D(M=0.56 CJO=46p VJ=0.384)
M3 7 9 1 1 INTER
E3 9 1 4 1 -2
.ENDS
"""


def cascode():
    """The cascode's subcircuit, with the threshold asked for."""
    text = CASCODES[CASCODE]
    if CASCODE_VTO is not None:
        head, _, rest = text.partition("NMOS(VTO=")
        text = f"{head}NMOS(VTO={CASCODE_VTO}{rest[rest.index(' '):]}"
    return text
# A 1 is the pin low, Q1 on. One pulse a pattern.
PATTERNS = {"12%": "10000000", "50%": "10", "50%x2": "1100", "88%": "01111111"}
GAINS = (120, 178, 300)  # the model's own is 178
STORAGE = (20e-9, 50e-9, 150e-9)  # the model's TR
DEFAULT_PAIRS = ("5100:2200", "1000:330", "470:150", "330:100", "220:68")
PUSH_PULL_PAIRS = (
    "1000:10000", "470:10000", "330:10000",
    "2200:10000:100", "1000:10000:100", "470:10000:100", "1000:10000:220",
)
# --sine: samples a second and the converter's bits a sample.
# No 8 Mbit/s (31250:256) unless asked for: it was the worst rate of every
# stage tried (2026-11-16), as --rate 8e6 was for the patterns.
DEFAULT_STREAMS = ("16000:128", "31250:128", "16000:256")
SINE_PAIRS = ("5100:2200", "330:100")
PUSH_PULL_SINE_PAIRS = ("1000:10000", "1000:10000:100")
SINE_SECONDS = 8e-3  # eight periods of the sine, near 1 kHz
SINE_PERIODS = 8
SINE_LEAD = 1e-3  # played before the measured part, for the coil to settle
EDGE = 2e-9
# The collector's volt-seconds are taken by an integrator on this capacitor.
INTEGRATOR = 1e-3


def circuit(load, bits, bit, r5, rbe, cap, clamp, bf, tr, i0, control, options=""):
    """The stage playing `bits` (0s and 1s, a 1 the pin low), `bit` seconds each."""
    r_spk, r_ser, henries = loads()[load]
    points, last = ["0 0"], 0
    for n, b in enumerate(bits):
        if b != last:
            points.append(f"{n * bit:.9e} {last} {n * bit + EDGE:.9e} {b}")
        last = b
    pwl = "\n+ ".join(" ".join(points[k : k + 8]) for k in range(0, len(points), 8))
    return f"""the buzzer's stage
* onsemi's ss8550.lib (Fairchild, 2004), with BF and TR ours: it has no TR.
.model QS8550 PNP(IS=1.17631E-13 BF={bf} NF=1 BR=5 NR=1 ISE=1.882E-14 NE=1.5 ISC=2E-16 NC=1.5
+ VAF=51.25 VAR=36.6808 IKF=3.3034 IKR=0.01 RB=122 RBM=27.073 IRB=8.32235E-5 RE=0 RC=1 NS=1.998
+ CJE=9.95126E-11 VJE=0.777949 MJE=0.380089 FC=0.499252 CJC=3.32783E-11 VJC=0.651017 MJC=0.389073
+ TF=7.96e-10 EG=1.1695 XTB=1.6730 XTI=3 XCJC=0.724 TR={tr})
.model D4148 D(IS=2.5n RS=0.6 N=1.75 CJO=1.2p TT=5n BV=100)
.model DBAT54 D(IS=1e-8 N=1.05 RS=3 CJO=10p)
* Q1's collector-emitter breakdown, which the transistor's model lacks.
.model DBREAK D(IS=1e-14 BV=30 IBV=1m RS=5)
.model DPAD D(IS=1e-14 RS=20 CJO=1p)
.model SW SW(RON=18 ROFF=100MEG VT=0.5 VH=0.1)
* The pin driving high: on while the gate is under 0.4.
.model SWP SW(RON=16.5 ROFF=100MEG VT=-0.5 VH=0.1)
VCC vcc 0 {SUPPLY}
V33 v33 0 3.3
VG g 0 PWL({pwl})
S1 pin 0 g 0 SW
{"S2 v33 pin 0 g SWP" if PUSH_PULL else ""}
CPIN pin 0 8p
VPAD pin pd 0
{"DPIN pd v33 DPAD" if PAD_DIODE else ""}
{"VEXT pin px 0" if PIN_CLAMP else ""}
{"DEXT px v33 DBAT54" if PIN_CLAMP else ""}
{cascode() if CASCODE else ""}
{"XC r5 v33 pin CASC" if CASCODE else "VC r5 pin 0"}
R5 r5 b {r5}
{f"C5 r5 b {cap}p" if cap else ""}
{f"RBE vcc b {rbe}" if rbe else ""}
{"DS c b DBAT54" if clamp else ""}
VE vcc e 0
Q1 c b e QS8550
{"D1 0 c D4148" if FLYBACK else ""}
DBR c vcc DBREAK
{f"RLOSS c 0 {LOSS_OHMS}" if load == LOSSY else ""}
RL c n1 {r_spk + r_ser}
L1 n1 0 {henries} ic={i0}
BINT 0 int I=v(c)
CINT int 0 {INTEGRATOR} ic=0
RINT int 0 1e12
.options abstol=1e-9 {options}
.control
{control}
.endc
.end
"""


def loads():
    return LOADS_3V3 if PUSH_PULL else LOADS


def settled(load, duty):
    """The coil's current where it would settle, so a run starts there."""
    r_spk, r_ser, _ = loads()[load]
    if not FLYBACK:
        return 0.0
    return max(0.0, (duty * (SUPPLY - 0.15) - (1 - duty) * 0.7) / (r_spk + r_ser))


def released(r5, rbe, cap):
    """The stage at rest, nothing playing: the pin's volts, the amps into
    the pad's diode and into the BAT54 beside it, Q1's base under its
    emitter and the amps Q1 passes."""
    load = next(iter(loads()))
    control = "op\nprint v(pin) i(VPAD) v(vcc,b) i(VE) v(r5)" + (" i(VEXT)" if PIN_CLAMP else "")
    text = circuit(load, [0], 1e-6, r5, rbe, cap, False, 178, 50e-9, 0, control)
    with tempfile.NamedTemporaryFile("w", suffix=".cir") as f:
        f.write(text)
        f.flush()
        out = subprocess.run(["ngspice", "-b", f.name], capture_output=True, text=True, check=True).stdout
    value = lambda name: next(float(l.split("=")[1]) for l in out.splitlines() if l.startswith(name + " ="))
    ext = value("i(vext)") if PIN_CLAMP else 0.0
    return value("v(pin)"), value("i(vpad)"), ext, value("v(vcc,b)"), value("i(ve)")


def simulate(tmp, n, text):
    cir, out = f"{tmp}/{n}.cir", f"{tmp}/{n}.dat"
    with open(cir, "w") as f:
        f.write(text.replace("OUT", out))
    subprocess.run(["ngspice", "-b", cir], capture_output=True, check=True)
    data = np.loadtxt(out, skiprows=1).T
    os.remove(out)
    os.remove(cir)
    return data


def run_pattern(job):
    tmp, n, load, rate, pattern, r5, rbe, cap, clamp, bf, tr = job
    bits = PATTERNS[pattern]
    bit = 1.0 / rate
    period = bit * len(bits)
    cycles = max(8, round(30e-6 / period))
    duty = bits.count("1") / len(bits)
    stop = period * cycles
    control = f"""set wr_singlescale
set wr_vecnames
tran 0.5n {stop} {stop - 4 * period} 1n uic
wrdata OUT v(c) i(VE)"""
    seq = [int(b) for b in bits * cycles]
    text = circuit(load, seq, bit, r5, rbe, cap, clamp, bf, tr, settled(load, duty), control, "reltol=1e-3")
    data = simulate(tmp, n, text)
    t, vc, ie = data[0], data[1], data[2]
    span = t[-1] - t[0]
    mean = lambda y: np.trapezoid(y, t) / span
    vec = SUPPLY - vc
    return {
        "key": (rate, r5, rbe, cap, clamp),
        "pattern": pattern,
        "nominal": bf == 178 and tr == 50e-9,
        # The collector's on time less the pin's, a pulse, in bits.
        "error": (mean((vc > SUPPLY / 2).astype(float)) - duty) * period / bit,
        "between": mean(((vec > 0.5) & (vec < SUPPLY - 0.5)).astype(float)),
        "watts": mean(vec * ie),
        "vsat": vec.min(),
    }


def tried(args):
    """The loads a pattern run goes over: the one asked for, or all but the lossy buzzer."""
    return [args.load] if args.load else [name for name in loads() if name != LOSSY]


def patterns(args, pairs, clamps):
    rates = args.rate or [4.096e6]
    with tempfile.TemporaryDirectory() as tmp:
        jobs = [
            (tmp, n, load, rate, pattern, r5, rbe, cap, clamp, bf, tr)
            for n, (rate, (r5, rbe, cap), clamp, load, pattern, bf, tr) in enumerate(
                itertools.product(rates, pairs, clamps, tried(args), PATTERNS, GAINS, STORAGE)
            )
        ]
        with ThreadPoolExecutor(os.cpu_count()) as pool:
            results = list(pool.map(run_pattern, jobs))

    print(f"Loads: {', '.join(tried(args))}.")
    print("Pulse error in bits, lowest..highest over the loads; 'worst' is also over")
    print(f"gains {GAINS} and storage times {[int(s * 1e9) for s in STORAGE]} ns.")
    print("between: the share of the time Q1 is between the rails. pin: its current, low.")
    print("At rest, the pin released (or high, push-pull): its volts, the current into its")
    print("pad's diode to 3.3 V and into the BAT54 beside it, the base under the emitter, and")
    print("what Q1 passes into the load.\n")
    print(stage_line())
    head = "Mbit/s    R5   RBE   pF clamp case    | " + " ".join(f"{k:>12}" for k in PATTERNS)
    print(head + " | between  Q1 mW  Vsat | pin mA | pin V  pad mA  BAT54 mA  Veb  Q1 mA")
    for rate, (r5, rbe, cap), clamp in itertools.product(rates, pairs, clamps):
        for case in ("nominal", "worst"):
            rows = [
                r for r in results
                if r["key"] == (rate, r5, rbe, cap, clamp) and (case == "worst" or r["nominal"])
            ]
            cells = []
            for pattern in PATTERNS:
                e = [r["error"] for r in rows if r["pattern"] == pattern]
                cells.append(f"{min(e):+.2f}..{max(e):+.2f}")
            pin_v, pad, ext, veb, rest = released(r5, rbe, cap)
            print(
                f"{rate / 1e6:6.3f} {r5:5d} {rbe:5d} {cap:4d} {'BAT54' if clamp else '  -  '} {case:7} | "
                + " ".join(cells)
                + f" | {100 * max(r['between'] for r in rows):5.0f} %  {1e3 * max(r['watts'] for r in rows):5.0f}  "
                f"{max(r['vsat'] for r in rows):.2f} | {(SUPPLY - 0.75) / r5 * 1e3:6.1f} | {pin_v:5.2f}  {pad * 1e3:6.3f}  {ext * 1e3:8.3f}  {veb:.2f}  {rest * 1e3:5.2f}"
            )


def modulate(x, order):
    """A sigma-delta loop's bits for the levels `x`, each within -1..1."""
    bits = np.empty(len(x), dtype=np.int8)
    s1 = s2 = 0.0
    for n, v in enumerate(x):
        if order == 1:
            y = 1.0 if s1 >= 0 else -1.0
            s1 += v - y
        else:
            y = 1.0 if s2 >= 0 else -1.0
            s1 += v - y
            s2 += s1 - 2 * y
        bits[n] = y > 0
    return bits


def spectrum(y, top):
    """Of `y`, a whole number of the sine's periods: the sine's amplitude,
    and the harmonics' and the rest's power in the bins up to `top`."""
    f = np.fft.rfft(y - y.mean()) / len(y) * 2
    power = np.abs(f[: top + 1]) ** 2
    signal = power[SINE_PERIODS]
    harmonics = power[2 * SINE_PERIODS :: SINE_PERIODS].sum()
    noise = power[1:].sum() - power[SINE_PERIODS :: SINE_PERIODS].sum()
    return np.sqrt(signal), harmonics, noise


def db(ratio):
    return 10 * np.log10(max(ratio, 1e-30))


def run_sine(job):
    tmp, n, load, hz, ratio, r5, rbe, cap, clamp, bf, tr, order, level, reltol = job
    rate = hz * ratio
    bit = 1.0 / rate
    count = 1 << round(np.log2(SINE_SECONDS * rate))
    lead = round(SINE_LEAD * rate)
    phase = 2 * np.pi * SINE_PERIODS * np.arange(-lead, count) / count
    bits = modulate(level * np.sin(phase), order)
    start, stop = lead * bit, (lead + count) * bit
    # `interp` keeps only the points a bit apart, of the integrator alone.
    control = f"""set wr_singlescale
set wr_vecnames
set numdgt=15
save v(int)
tran {bit:.12e} {stop:.12e} {start:.12e} 1n uic
wrdata OUT v(int)"""
    text = circuit(load, bits, bit, r5, rbe, cap, clamp, bf, tr, settled(load, 0.5), control, f"interp reltol={reltol}")
    data = simulate(tmp, n, text)
    t, vint = data[0], data[1]
    # The collector's mean over each bit, on the bits' own grid.
    grid = start + bit * np.arange(count + 1)
    volts = np.diff(np.interp(grid, t, vint)) * INTEGRATOR / bit
    # The band is the clip's: half its sample rate.
    top = count // (2 * ratio)
    ideal, ideal_h, ideal_n = spectrum(SUPPLY * bits[lead:], top)
    amp, harm, noise = spectrum(volts, top)
    return {
        "key": (hz, ratio, r5, rbe, cap, clamp),
        "rate": rate,
        "hz": SINE_PERIODS * rate / count,
        "ideal_sndr": db(ideal**2 / (ideal_h + ideal_n)),
        "gain": db((amp / ideal) ** 2),
        "thd": db(harm / amp**2),
        "snr": db(amp**2 / noise),
        "sndr": db(amp**2 / (harm + noise)),
    }


def sine(args, pairs, clamps):
    streams = [tuple(int(v) for v in s.split(":")) for s in (args.stream or DEFAULT_STREAMS)]
    bf, tr = args.gain, args.storage * 1e-9
    with tempfile.TemporaryDirectory() as tmp:
        jobs = [
            (tmp, n, args.load, hz, ratio, r5, rbe, cap, clamp, bf, tr, args.order, args.level, args.reltol)
            for n, ((hz, ratio), (r5, rbe, cap), clamp) in enumerate(itertools.product(streams, pairs, clamps))
        ]
        with ThreadPoolExecutor(os.cpu_count()) as pool:
            results = list(pool.map(run_sine, jobs))

    print(f"A sine of {args.level:g} of full scale near 1 kHz through a sigma-delta loop of order {args.order}")
    print(f"(the script's, not the chip's) into the {args.load}; Q1's gain {bf}, storage time {args.storage:g} ns.")
    print("The band is half the sample rate. bitstream: the bits themselves, signal over the rest.")
    print("level: the stage's sine against the bits' at the rail. THD: the harmonics against the sine.")
    print("SNR: the sine over what is not a harmonic. SINAD: over both; bits = (SINAD - 1.76) / 6.02.\n")
    print(stage_line())
    print("  kHz  bits  Mbit/s    R5   RBE   pF clamp | bitstream | level dB  THD dB  SNR dB  SINAD dB  bits")
    for r in results:
        hz, ratio, r5, rbe, cap, clamp = r["key"]
        print(
            f"{hz / 1e3:5.2f}  {ratio:4d}  {r['rate'] / 1e6:6.3f} {r5:5d} {rbe:5d} {cap:4d} {'BAT54' if clamp else '  -  '} |"
            f" {r['ideal_sndr']:6.1f} dB | {r['gain']:+8.1f}  {r['thd']:6.1f}  {r['snr']:6.1f}  {r['sndr']:8.1f}"
            f"  {(r['sndr'] - 1.76) / 6.02:4.1f}"
        )


def stage_line():
    pin = "push-pull" if PUSH_PULL else "open-drain"
    clamp = ", a BAT54 from it to 3.3 V" if PIN_CLAMP else ""
    clamp += ", a diode to 3.3 V in its pad" if PAD_DIODE else ""
    vto = f" (threshold {CASCODE_VTO:g} V)" if CASCODE_VTO is not None else ""
    clamp += f", a {CASCODE.upper()}{vto} between it and R5" if CASCODE else ""
    d1 = "" if FLYBACK else ", no D1"
    return f"Q1's emitter at {SUPPLY:g} V, GPIO5 {pin}{clamp}{d1}. pF: a capacitor across R5.\n"


def main():
    global SUPPLY, PUSH_PULL, PIN_CLAMP, FLYBACK, PAD_DIODE, CASCODE, CASCODE_VTO
    p = argparse.ArgumentParser(description="The buzzer's stage under a clip's bitstream, in ngspice: R5 and RBE.")
    p.add_argument("pairs", nargs="*", help="R5:RBE[:pF] in ohms, 0 for no RBE; pF is a capacitor across R5")
    p.add_argument("--rate", type=float, action="append", help="bits a second (default 4.096e6)")
    p.add_argument("--clamp", action="store_true", help="also with a BAT54 from collector to base")
    p.add_argument("--pin-clamp", action="store_true", help="a BAT54 from GPIO5 to 3.3 V")
    p.add_argument("--pad-diode", action="store_true", help="GPIO5 with a diode to 3.3 V in its pad, as first assumed")
    p.add_argument("--no-pad-diode", action="store_true", help="the default since the pin was measured; kept for old command lines")
    p.add_argument("--cascode", choices=sorted(CASCODES), help="an N-channel FET between GPIO5 and R5, gate at 3.3 V")
    p.add_argument("--cascode-vto", type=float, help="that FET's threshold, V, in place of its model's")
    p.add_argument("--push-pull", action="store_true", help="Q1's emitter at 3.3 V and GPIO5 push-pull")
    p.add_argument("--sine", action="store_true", help="noise and distortion of a sine, in dB")
    p.add_argument("--stream", action="append", help="--sine: SAMPLES:BITS, e.g. 31250:256 for 8 Mbit/s (default: the three at 2 and 4 Mbit/s)")
    p.add_argument("--order", type=int, choices=(1, 2), default=2, help="--sine: the modulator's order (2)")
    p.add_argument("--level", type=float, default=0.5, help="--sine: the sine's share of full scale (0.5)")
    p.add_argument("--no-flyback", action="store_true", help="without D1 across the load, as the kit has it")
    p.add_argument("--load", choices=sorted(set(LOADS) | set(LOADS_3V3)), help="the one load (--sine: speaker 32R)")
    p.add_argument("--gain", type=int, default=178, help="--sine: Q1's gain (178, the model's)")
    p.add_argument("--reltol", type=float, default=1e-3, help="--sine: the solver's tolerance (1e-3)")
    p.add_argument("--storage", type=float, default=50, help="--sine: Q1's storage time, ns (50)")
    args = p.parse_args()
    clamps = [False, True] if args.clamp else [False]
    if args.push_pull:
        SUPPLY, PUSH_PULL = 3.3, True
    PIN_CLAMP = args.pin_clamp
    FLYBACK = not args.no_flyback
    PAD_DIODE = args.pad_diode
    CASCODE, CASCODE_VTO = args.cascode, args.cascode_vto
    if args.sine and not args.load:
        args.load = "speaker 32R"
    if args.load and args.load not in loads():
        p.error(f"--load is one of {', '.join(loads())} for this stage")
    if PUSH_PULL:
        names = args.pairs or (PUSH_PULL_SINE_PAIRS if args.sine else PUSH_PULL_PAIRS)
    else:
        names = args.pairs or (SINE_PAIRS if args.sine else DEFAULT_PAIRS)
    pairs = [tuple(int(v) for v in (pair + ":0").split(":")[:3]) for pair in names]
    (sine if args.sine else patterns)(args, pairs, clamps)


if __name__ == "__main__":
    main()

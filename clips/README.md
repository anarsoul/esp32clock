# Clips

The clips of the default clip pack, the extra alarm melodies a clock
has in its `clips` partition (see the README's "Clip packs"):

```
scripts/clips.sh                                       # packs/clips-default.bin, these four
scripts/mkpack.py packs/one.bin clips/trantor.bpr      # a pack of one of them
ALARM_CLIP=clips/trantor.bpr cargo build --release     # or built into the firmware, the older way
```

They are other people's music and are **not** covered by the project's
MIT OR Apache-2.0 licence; each is credited below, and copyright stays
with its author.

## `trantor.bpr`

The title music of *Trantor: The Last Stormtrooper* (ZX Spectrum, Probe
Software / Go!, 1987), composed by **David Whittaker**. The music is
his; copyright remains with him and the game's publishers, and nothing
here grants any right to it.

The file is what the game's own player sends to the Spectrum's
speaker, recorded in an emulator as run lengths of the speaker bit by
`scripts/zxbeeper.py --seconds 126.5` (one pass of the tune, to the
point where the game starts it over; pulses as played), which writes
a stretch of runs that has been played before as a reference to it:
the file is half the size and the runs are the same. It is not a new arrangement or performance.

## `halftime.xmc` and `keygen.xmc`

Two chiptunes by **Dualtrax** (of Chemical Reaction and Analogik),
written as FastTracker 2 modules for the key generators of the group
ORiON: "halftime" (2002) and "keygentune2003 orn". The modules are
signed in their instrument names, `dt/cro/analogik` and
`dualtrax/cro/`; see [Dualtrax on Demozoo](https://demozoo.org/sceners/5763/).
The music is Dualtrax's; copyright remains with its author, and
nothing here grants any right to it.

The modules themselves are in `mods/` as they were found, and these
two files are what `scripts/xm2clip.py` makes of them for the clock's
own player (`core/src/xm.rs`), with its defaults: the same patterns
and samples rearranged, nothing rendered.

```
scripts/xm2clip.py "mods/Orion - Nero Burning ROM v5.5.7.8kg.xm" clips/halftime.xmc
scripts/xm2clip.py mods/Orion-Nero6.3.0.2.xm clips/keygen.xmc
```

## `synchronization.pt3c`

"synchronization" (2015) by **n1k-o** (Oleg Nikitin), a ZX Spectrum
tune for the AY-3-8910, written in Vortex Tracker II; the module is
signed `nq`. It is here with his permission, given to this project
(2026-11-16). The music is his; copyright remains with
its author, and nothing here grants any right to it beyond that.

The module is in `mods/` as it was found, and this file is the module
as it is behind the header `scripts/pt32clip.py` gives it for the
clock's own player and AY (`core/src/pt3.rs`, `core/src/ay.rs`):
nothing rendered. The header asks the player for its limiter, letting
go over 128 ms, and a high-pass at 100 Hz.

```
scripts/pt32clip.py mods/nq-synchronization.pt3 clips/synchronization.pt3c --restart \
    --limiter --noise 16 --highpass 100 --passes 1 --release 10
```

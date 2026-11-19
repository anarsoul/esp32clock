# Clips

The clips of the default clip pack, the extra alarm melodies a clock
has in its `clips` partition (see "Clip packs" in `MANUAL.md`):

```
scripts/clips.sh                                       # packs/clips-default.bin, these seven
cargo pack -o packs/one.bin clips/trantor.bpr          # a pack of one of them
ALARM_CLIP=clips/trantor.bpr cargo build --release     # or built into the firmware, the older way
```

The first four are other people's music and are **not** covered by
the project's MIT OR Apache-2.0 licence; each is credited below, and
copyright stays with its author. The last three were made here.

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
two files are what the pack maker (`tools/pack`) makes of them for the clock's
own player (`core/src/xm.rs`), with its defaults: the same patterns
and samples rearranged, nothing rendered.

```
cargo pack --clip clips/halftime.xmc "mods/Orion - Nero Burning ROM v5.5.7.8kg.xm:name=halftime"
cargo pack --clip clips/keygen.xmc mods/Orion-Nero6.3.0.2.xm
```

## `synchronization.pt3c`

"synchronization" (2015) by **n1k-o** (Oleg Nikitin), a ZX Spectrum
tune for the AY-3-8910, written in Vortex Tracker II; the module is
signed `nq`. It is here with his permission, given to this project
(2026-11-16). The music is his; copyright remains with
its author, and nothing here grants any right to it beyond that.

The module is in `mods/` as it was found, and this file is the module
as it is behind the header the pack maker gives it for the
clock's own player and AY (`core/src/pt3.rs`, `core/src/ay.rs`):
nothing rendered. The header asks the player for its limiter, letting
go over 128 ms, and a high-pass at 100 Hz.

```
cargo pack --clip clips/synchronization.pt3c mods/nq-synchronization.pt3:restart,name=synchronization
```

## `morning_mood.xmc`, `morning_mood_chip.xmc` and `mountain_king.xmc`

Two movements of **Edvard Grieg**'s first *Peer Gynt* suite, Op. 46
(1888; he died in 1907 and the music is in the public domain), as
tracker modules made for this project: *Morning Mood* (No. 1) twice,
with an orchestra's recorded instruments and with plain waves as a
chiptune has them, and *In the Hall of the Mountain King* (No. 4) with
the orchestra's. A fourth, the Mountain King with plain waves, is in
`mods/` and not in the pack.

- **The notes.** The Mountain King's are the Mutopia Project's piano
  score, [piece 1888](https://www.mutopiaproject.org/cgibin/piece-info.cgi?id=1888),
  typeset by Coyau and placed in the public domain; its MIDI file and
  LilyPond source are in `mods/src/` as Mutopia gives them. Morning's
  were read here off the composer's own arrangement for piano four
  hands, Edition Peters 2432, in the Sibley Music Library's
  [scan](https://hdl.handle.net/1802/16617). It is shortened: bars 1
  to 31 and 56 to 76, where a bar's rest ends it.
- **The instruments.** The orchestra's are recordings from Versilian
  Studios' [VSCO 2 Community Edition](https://github.com/sgossner/VSCO-2-CE),
  which is CC0, cut down to a note an instrument. The plain waves are
  made by the script.
- **The arrangements and the modules** are this project's, under its
  licence, MIT OR Apache-2.0.

The modules are written by `scripts/mkxm.py` from the tunes in
`mods/src/`, and the clips are the pack maker's defaults of them:

```
scripts/mkxm.py mods/src/morning.py mods/grieg-morning-bright.xm bright
scripts/mkxm.py mods/src/morning.py mods/grieg-morning-chip.xm chip
scripts/mkxm.py mods/src/mountain_king.py mods/grieg-mountain-king-bright.xm bright
scripts/mkxm.py mods/src/mountain_king.py mods/grieg-mountain-king-chip.xm chip
cargo pack --clip clips/morning_mood.xmc mods/grieg-morning-bright.xm
cargo pack --clip clips/morning_mood_chip.xmc mods/grieg-morning-chip.xm
cargo pack --clip clips/mountain_king.xmc mods/grieg-mountain-king-bright.xm
```

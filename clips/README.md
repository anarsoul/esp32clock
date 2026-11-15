# Clips

The clips of the default clip pack, the extra alarm melodies a clock
has in its `clips` partition (see the README's "Clip packs"):

```
scripts/clips.sh                                       # packs/clips-default.bin, these three
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
point where the game starts it over; pulses as played). It is not a new arrangement or performance.

## `halftime.xmc` and `keygen.xmc`

Two chiptunes by **Orion**, written as FastTracker 2 modules for
software key generators: "halftime" and "keygentune2003 orn". The
music is his; copyright remains with him, and nothing here grants any
right to it.

The modules themselves are in `mods/` as they were found, and these
two files are what `scripts/xm2clip.py` makes of them for the clock's
own player (`core/src/xm.rs`), with its defaults: the same patterns
and samples rearranged, nothing rendered.

```
scripts/xm2clip.py "mods/Orion - Nero Burning ROM v5.5.7.8kg.xm" clips/halftime.xmc
scripts/xm2clip.py mods/Orion-Nero6.3.0.2.xm clips/keygen.xmc
```

# Clips

Files here can be built into the firmware as extra alarm melodies with
`ALARM_CLIP` (see `AGENTS.md`):

```
ALARM_CLIP=clips/trantor.bpr cargo build --release
```

They are **not** covered by the project's MIT OR Apache-2.0 licence.

## `trantor.bpr`

The title music of *Trantor: The Last Stormtrooper* (ZX Spectrum, Probe
Software / Go!, 1987), composed by **David Whittaker**. The music is
his; copyright remains with him and the game's publishers, and nothing
here grants any right to it.

The file is what the game's own player sends to the Spectrum's
speaker, recorded in an emulator as run lengths of the speaker bit by
`scripts/zxbeeper.py` (141.5 s, one pass of the tune, pulses as
played). It is not a new arrangement or performance.

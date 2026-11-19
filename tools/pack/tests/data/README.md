# Fixtures

A module of each kind the pack maker converts and a Commodore 64 tune,
written for these tests by a script (a few notes over made-up samples,
a counter in a SID register): nobody's music, under the project's
licence.

`tiny-mod.xmc`, `tiny-s3m.xmc` and `tiny.sidc` are what the Python
converters the crate was ported from (`scripts/mod2xm.py`,
`s3m2xm.py`, `xm2clip.py` and `sid2clip.py`, as they were on
2026-11-19) made of `tiny.mod`, `tiny.s3m` and `tiny.sid` with their
defaults; `tests/default_pack.rs` holds the crate to them, byte for
byte.

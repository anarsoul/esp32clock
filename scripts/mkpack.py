#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Vasily Khoruzhick
"""Make a clip pack for the clock's `clips` partition.

    scripts/mkpack.py out.bin clip[:loop|:once][=Label] ...

A clip is a file `ALARM_CLIP` would take: raw PCM (scripts/wav2pcm.sh),
as it is or as ADPCM (scripts/adpcm.py), a beeper clip (scripts/zxbeeper.py), a tracker module
(scripts/xm2clip.py), a Commodore 64 tune (scripts/sid2clip.py) or a
ZX Spectrum one (scripts/pt32clip.py). As there, a clip's API name is
its file's name without the extension, in lower case with everything
but letters turned into `_`, and its label the same in words unless
`=Label` gives another; a tune (all but PCM and ADPCM) is taken to
be made to loop, so a ringing alarm plays it round and round with no
gap, unless `:once` says otherwise, and PCM not unless `:loop` does.

The pack goes to a clock from its Clips page, with
`scripts/clips.sh <ip> out.bin`, or over USB:

    espflash write-bin 0x2e0000 out.bin && espflash reset

The layout is in core/src/pack.rs, whose parser is what the clock
reads the pack with; `CLIP_PACK_FILE=out.bin cargo host-test
the_pack_file_parses -- --nocapture` runs it on the host.
"""
import hashlib
import re
import struct
import sys
from pathlib import Path

MAGIC = b"CLPK"
VERSION = 1
MAX_CLIPS = 16
NAME_MAX = 20
LABEL_MAX = 32
HEADER_LEN = 12
ENTRY_LEN = 64
HASH_LEN = 32
# The partition's size in partitions.csv.
ROOM = 0x120000
# The note melodies' names (core/src/melody.rs) and the random draw's.
RESERVED = {"beeps", "twinkle", "ode", "frere", "morning", "westminster", "reveille", "elise", "jingle", "random"}
LOOPING = (b"BPR", b"XMC", b"SID", b"PT3")
# The clips' magics this firmware plays (clip::is_unsupported): the
# fourth byte is the format's version.
FAMILIES = (b"PCM", b"ADP", b"BPR", b"XMC", b"SID", b"PT3")
PLAYED = (b"PCM1", b"ADP1", b"BPR2", b"BPR3", b"XMC2", b"SID1", b"SID2", b"PT31")


def fail(message):
    sys.exit(f"mkpack: {message}")


def clip(arg):
    """(name, label, loops, data) for one argument."""
    arg, _, label = arg.partition("=")
    asked = None
    if arg.endswith(":loop"):
        arg, asked = arg[:-5], True
    elif arg.endswith(":once"):
        arg, asked = arg[:-5], False
    path = Path(arg)
    try:
        data = path.read_bytes()
    except OSError as e:
        fail(f"{arg}: {e.strerror}")
    words = [w.lower() for w in re.split(r"[^A-Za-z]+", path.stem) if w]
    if not words:
        fail(f"{arg}: the file's name needs letters, they are the clip's name")
    name = "_".join(words)
    label = label or " ".join(w.capitalize() for w in words)
    if len(name) > NAME_MAX:
        fail(f"{arg}: the name {name} is longer than {NAME_MAX} characters; rename the file")
    if name in RESERVED:
        fail(f"{arg}: the name {name} is a melody of the firmware's; rename the file")
    if len(label.encode()) > LABEL_MAX:
        fail(f"{arg}: the label {label!r} is longer than {LABEL_MAX} bytes")
    if not label.strip() or any(c in '<>&"' or ord(c) < 32 or ord(c) == 127 for c in label):
        fail(f"{arg}: the label {label!r} is empty or has one of < > & \" or a control character")
    if data.startswith(FAMILIES) and not data.startswith(PLAYED):
        fail(f"{arg}: a clip of the version {data[:4].decode('latin-1')}, which the firmware does not play; convert it again")
    loops = data.startswith(LOOPING) if asked is None else asked
    return name, label, loops, data


def main():
    if len(sys.argv) < 3 or sys.argv[1] in ("-h", "--help"):
        sys.exit(__doc__.strip())
    out = Path(sys.argv[1])
    clips = [clip(arg) for arg in sys.argv[2:]]
    if len(clips) > MAX_CLIPS:
        fail(f"{len(clips)} clips; a pack holds {MAX_CLIPS}")
    names = [c[0] for c in clips]
    for name in names:
        if names.count(name) > 1:
            fail(f"two clips named {name}; rename a file")
    at = HEADER_LEN + len(clips) * ENTRY_LEN
    total = at + sum(len(c[3]) for c in clips) + HASH_LEN
    pack = MAGIC + struct.pack("<BBHI", VERSION, len(clips), 0, total)
    for name, label, loops, data in clips:
        pack += struct.pack(f"<IIB3x{NAME_MAX}s{LABEL_MAX}s", at, len(data), int(loops), name.encode(), label.encode())
        at += len(data)
    for clip_ in clips:
        pack += clip_[3]
    pack += hashlib.sha256(pack).digest()
    assert len(pack) == total
    if total > ROOM:
        fail(f"{total} bytes; the clips partition holds {ROOM}")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(pack)
    for name, label, loops, data in clips:
        print(f"  {name:<{NAME_MAX}} {label:<{LABEL_MAX}} {len(data):>7} bytes{', loops' if loops else ''}")
    print(f"{out}: {len(clips)} clips, {total} bytes of {ROOM} ({total * 100 // ROOM} %)")


if __name__ == "__main__":
    main()

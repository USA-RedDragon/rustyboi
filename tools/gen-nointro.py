#!/usr/bin/env python3
# Regenerate the bundled No-Intro CRC32->name index used for game identification
# (rustyboi-session/data/no_intro.bin). Fetches the Game Boy + Game Boy Color
# No-Intro DATs from libretro's mirror and packs them into a compact blob.
#
# Blob format (little-endian):
#   "RBNI" magic (4 bytes), version u8 = 1, count u32,
#   then `count` entries sorted by crc: crc32 u32, name_len u16, name utf-8.
#
# Usage: python3 tools/gen-nointro.py
import re, struct, urllib.request
BASE = "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/"
DATS = ["Nintendo - Game Boy.dat", "Nintendo - Game Boy Color.dat"]

def parse(text):
    out, name = [], None
    for line in text.splitlines():
        m = re.match(r'\tname "(.+)"\s*$', line)
        if m: name = m.group(1); continue
        m = re.search(r'\bcrc ([0-9A-Fa-f]{8})', line)
        if m and name: out.append((int(m.group(1), 16), name)); name = None
    return out

entries = {}
for d in DATS:
    text = urllib.request.urlopen(BASE + d.replace(" ", "%20")).read().decode("utf-8", "replace")
    for crc, name in parse(text):
        entries[crc] = name

items = sorted(entries.items())
buf = bytearray(b"RBNI"); buf.append(1); buf += struct.pack("<I", len(items))
for crc, name in items:
    nb = name.encode("utf-8"); buf += struct.pack("<IH", crc, len(nb)) + nb
open("rustyboi-session/data/no_intro.bin", "wb").write(buf)
print(f"wrote rustyboi-session/data/no_intro.bin: {len(items)} entries, {len(buf)} bytes")

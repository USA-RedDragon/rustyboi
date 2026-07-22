#!/usr/bin/env python3
"""Derive the refs/ppu/window_scx_ignore.dmg.png oracle from first principles.

The oracle is computed from the documented hardware rule, not screenshotted from
an emulator (README provenance rule). Rule (Pan Docs, "Window"): a full-width
window (WX=7, WY=0) is locked to screen coordinates and ignores the SCX&7
fine-scroll discard, so the ROM's per-tile "dark column 0" mark stays
tile-aligned regardless of SCX. The ROM (window_scx_ignore.dmg.png.asm) fills
the screen with a tile whose leftmost pixel is color 3 and the rest color 0,
under the identity palette BGP=$E4 (color 3 -> black, color 0 -> white). So:

  pixel(x, y) = black (#000000) iff x % 8 == 0, else white (#FFFFFF)

The pre-fix bug shifted the dark columns to x % 8 == 5 (the SCX=3 discard leaking
into the window), which this all-x%8==0 oracle rejects. Nothing here runs an
emulator, so the ref cannot inherit a rustyboi bug.

Run from anywhere: `python3 test-roms/gen_window_scx_ref.py`.
"""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))
from gen_manifests import write_png_rgb  # noqa: E402

W, H = 160, 144

px = [0x000000 if (x % 8 == 0) else 0xFFFFFF for _ in range(H) for x in range(W)]

ref = ROOT / "test-roms" / "refs" / "ppu" / "window_scx_ignore.dmg.png"
write_png_rgb(ref, W, H, px)
print(f"derived oracle into {ref}")

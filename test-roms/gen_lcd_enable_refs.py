#!/usr/bin/env python3
"""Derive the refs/ppu/lcd_enable_frame_* png oracles from first principles.

The oracles are computed from the documented hardware rule (the CGB panel
repeats the previously displayed image for the skipped post-enable frame; the
DMG panel shows blank white) plus the ROMs' OWN authored pattern data — the
tile shapes, map/attribute formulas and palette tables below are byte-for-byte
the constants in include/lcd_enable_pattern.inc. Nothing here runs an
emulator, so the refs cannot inherit a rustyboi bug (README provenance rule).

Render rule (plain BG, no window/sprites, SCX=SCY=0, BG8000 addressing):
  tx = x//8, ty = y//8, tile = (tx+ty)&3, palette = (tx^ty)&3
  color index = tile shape at (x&7, y&7)
  CGB 8-bit channel = (v5*255)//31 (the runner's Linear conversion; the
  comparison masks to the top 5 bits, which equal v5 for every v5)

  DMG shade = (BGP >> 2*c) & 3; the runner maps shades 0..3 to the grays
  0xFFFFFF / 0xAAAAAA / 0x555555 / 0x000000

  lcd_enable_frame_repeat.cgb.png = pattern in PalA (the held image — the
      ROM swaps to PalB during the LCD-off, so the skipped in-flight frame
      renders in colors the panel never displayed)
  lcd_enable_frame_after.cgb.png  = pattern in PalB (PalA reversed per
      palette; the first frame DISPLAYED after the skip)
  lcd_enable_frame_blank.dmg.png  = all white (Pan Docs "LCDC.7" blank rule)
  lcd_enable_repeat_decay.cgb.png = all white (the panel-drive countdown —
      SameBoy frame_repeat_countdown, measured on CGB-E: 144*456*2 + 3640
      8 MHz cycles re-armed at each VBlank line start — expires during the
      ROM's ~4.7-line off, so the skipped frame blanks instead of repeating)
  lcd_enable_frame_after.dmg.png  = pattern through BGP=$1B (the $E4
      identity palette reversed per color; the first frame DISPLAYED after
      the skip — pins the blank's END on DMG)

Run from anywhere: `python3 test-roms/gen_lcd_enable_refs.py`.
"""

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))
from gen_manifests import write_png_rgb  # noqa: E402

W, H = 160, 144


def tile_rows(low, high):
    """2bpp decode: color index per (py, px) from per-row low/high bytes."""
    return [[(((h >> (7 - px)) & 1) << 1) | ((l >> (7 - px)) & 1) for px in range(8)]
            for l, h in zip(low, high)]


# Must match TileData in include/lcd_enable_pattern.inc.
TILES = [
    tile_rows([0xFF] * 8, [0xFF] * 8),                                # solid c3
    tile_rows([0xCC] * 8, [0x33] * 8),                                # vstripes 1,1,2,2
    tile_rows([0xFF, 0xFF, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00],
              [0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00]),      # hbands 1,2,3,0
    tile_rows([0x80 >> r for r in range(8)],
              [0x80 >> r for r in range(8)]),                         # diagonal c3
]

# BGR555 (r,g,b) 5-bit values. Must match PalA in include/lcd_enable_pattern.inc
# (little-endian words r | g<<5 | b<<10).
PAL_A = [
    [(31, 0, 0), (0, 31, 0), (0, 0, 31), (31, 31, 0)],
    [(31, 0, 31), (0, 31, 31), (15, 15, 15), (31, 15, 0)],
    [(12, 0, 24), (31, 31, 31), (0, 0, 0), (15, 31, 15)],
    [(0, 15, 31), (31, 12, 12), (15, 15, 0), (0, 31, 15)],
]
# PalB = PalA with the color order reversed inside each palette.
PAL_B = [list(reversed(p)) for p in PAL_A]


def render_cgb(pal):
    px = []
    for y in range(H):
        for x in range(W):
            tx, ty = x >> 3, y >> 3
            c = TILES[(tx + ty) & 3][y & 7][x & 7]
            r, g, b = pal[(tx ^ ty) & 3][c]
            px.append((r * 255 // 31) << 16 | (g * 255 // 31) << 8 | (b * 255 // 31))
    return px


# Runner normalize_frame gray for DMG shades 0..3.
DMG_GRAYS = [0xFFFFFF, 0xAAAAAA, 0x555555, 0x000000]


def render_dmg(bgp):
    px = []
    for y in range(H):
        for x in range(W):
            tx, ty = x >> 3, y >> 3
            c = TILES[(tx + ty) & 3][y & 7][x & 7]
            px.append(DMG_GRAYS[(bgp >> (2 * c)) & 3])
    return px


refs = ROOT / "test-roms" / "refs" / "ppu"
write_png_rgb(refs / "lcd_enable_frame_repeat.cgb.png", W, H, render_cgb(PAL_A))
write_png_rgb(refs / "lcd_enable_frame_after.cgb.png", W, H, render_cgb(PAL_B))
write_png_rgb(refs / "lcd_enable_frame_blank.dmg.png", W, H, [0xFFFFFF] * (W * H))
write_png_rgb(refs / "lcd_enable_frame_after.dmg.png", W, H, render_dmg(0x1B))
write_png_rgb(refs / "lcd_enable_repeat_decay.cgb.png", W, H, [0xFFFFFF] * (W * H))
print(f"derived 5 oracles into {refs}")

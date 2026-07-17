# Behavior coverage audit

The living list that gates which behaviors get a first-party ROM. A behavior is a
ROM candidate only when it is **uncovered by a public suite** AND
**silicon-verified** (documented in Pan Docs, or bench-confirmed). CPU-observable
behaviors become `mooneye` ROMs; render-only behaviors become `png` ROMs with a
rule-derived oracle. Anything only Gambatte-derived / emulator-reference-derived
is parked here until the hardware bench confirms it — listed, not authored.

Status: `ROM` (authored + validated) · `rust` (guarded by an in-code test, ROM
pending) · `parked` (not silicon-verified yet).

## Done

| Behavior | Grading | Status | Notes |
|---|---|---|---|
| Window ignores the SCX fine-scroll discard (full-width WX==7) | png (dmg) | **ROM** | `ppu/window_scx_ignore` — fails pre-fix (markers shift x%8 5), passes post-fix. Rust test dropped. |
| CGB panel repeats the previous image for the skipped first frame after LCDC.7 enable | png (cgb) | **ROM** | `ppu/lcd_enable_frame_repeat` — EA-style in-VBlank LCD off/on with palettes swapped to PalB during the off, graded mid-skipped-frame vs the derived PalA signature-pattern oracle (include/lcd_enable_pattern.inc). Discriminates blank-white, zeroed-buffer black, and back-buffer/in-flight PalB presents; fails pre-fix (all white). SameBoy-measured on CGB-E (frame_repeat_countdown); corroborated by the EA CGB middleware (Madden/NHL 2000, MiB) never flashing on hardware. Bench-confirm candidate. |
| CGB panel resumes display right after the one skipped frame (repeat ends) | png (cgb) | **ROM** | `ppu/lcd_enable_frame_after` — same flip, graded two frames later vs the PalB pattern oracle; a panel stuck blanking (white) or stuck repeating the PalA image both fail. Guards over-regression of the repeat rule. |
| DMG panel shows blank (white) for the skipped first frame after LCDC.7 enable | png (dmg) | **ROM** | `ppu/lcd_enable_frame_blank` — same script as the CGB repeat ROM (signature pattern, BGP=$E4), all-white oracle (Pan Docs "LCDC.7" blank rule); pins the CGB/DMG asymmetry — a wrong CGB-style repeat on DMG would show the pattern and fail. |

## Candidate list (silicon-verified, ROM pending)

| Behavior | Grading | Status | Notes |
|---|---|---|---|
| HBlank DMA transfers exactly one 0x10-byte block per HBlank | mooneye (cgb) | **rust** | Fix in `memory/mmio.rs`, guarded by `hblank_dma_tests`. The *specific* arm-line double-fire is a sub-dot phase artifact that a mode-2-synced ROM arm does not hit (the same-HBlank block count stays ≤1 even pre-fix); reproducing it needs the exact CPU/PPU phase the Crystal cutscene produced. Revisit — likely easiest to confirm/author against the hardware bench. |

## Method (to run down the rest)

1. Enumerate every in-code Rust test asserting hardware behavior (`grep -rn '#\[test\]'
   rustyboi-core/src/**` — printer, sgb, ir, serial, dmg07, ppu/fetcher,
   ppu/controller, memory/mmio, cartridge, cheats, input, …), plus behaviors from
   the project memory notes and shipped fixes.
2. Classify each: covered by a public ROM? · CPU-observable (mooneye) vs render
   (png)? · provenance (documented/bench vs Gambatte/emulator-ref)?
3. Author ROMs for `uncovered ∧ silicon-verified`, one at a time; validate
   fail-pre/pass-post; drop the paired Rust test; re-ratchet the suite floor.
4. Move Gambatte/emulator-ref behaviors from `parked` to a candidate only after
   the bench confirms them.

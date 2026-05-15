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

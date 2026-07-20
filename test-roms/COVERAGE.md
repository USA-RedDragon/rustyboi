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
| DMG panel resumes display right after the one skipped frame (blank ends) | png (dmg) | **ROM** | `ppu/lcd_enable_frame_after` (dmg) — same flip with BGP inverted to $1B during the off, graded two frames later vs the $1B pattern oracle; a panel stuck blanking (white) or wrongly repeating the $E4 image both fail. DMG mirror of the CGB after ROM. |
| SGB MLT_REQ multiplexes the joypad ID on the P15 LOW->HIGH edge (1/2/4 players, with wrap) | mooneye (sgb) | **ROM** | `sgb/mlt_req_player_cycle` — Pan Docs "Multiplayer Command": ID table $xF..$xC, "next joypad is automatically selected when P15 goes from LOW (0) to HIGH (1)", and one-player mode never advances (gbdev ICD2 $6003: advance gated on "number of controllers greater than one"). Each subtest is entered from a *known* player count, since Pan Docs warns MLT_REQ transfers themselves clock the counter. Fails pre-fix with the P15 edge disabled, with the wrap mask removed (only the 5th reading diverges), and with the ID table reversed. |
| ...identically on SGB2 | mooneye (sgb2) | **ROM** | `sgb/mlt_req_player_cycle` (sgb2) — same script; SGB2 differs in clock source, not in the ICD2 command interface. Fails when SGB support is scoped to `Hardware::SGB` alone, which the sgb build cannot catch. |
| An SGB-flagged cart in a plain Game Boy must NOT multiplex | mooneye (sgbcart) | **ROM** | `sgb/mlt_req_no_multiplex` — Pan Docs "Joypad Input" ($30 written => low nibble reads $F) and "Unlocking SGB Functions" ("a normal Game Boy would typically always return $0F as the ID"). Must be built `sgbcart`, not `dmg`: the unflagged build passes even with SGB force-enabled on DMG because the unlock gate masks it. |
| SGB command packets are ignored unless the header unlocks SGB functions | mooneye (sgblocked) | **ROM** | `sgb/mlt_req_header_locked` — Pan Docs "Unlocking SGB Functions": $0146=$03 AND $014B=$33 or the cart "cannot access any of the special SGB functions". Same hardware/packets/reads as `mlt_req_player_cycle.sgb`, differing only in those two header bytes. Fails when the gate is forced open. |
| A both-lines-LOW pulse mid-transfer resets the ICD2 receiver | mooneye (sgb) | **ROM** | `sgb/packet_reset_aborts` — Pan Docs "Command Packet Transfers": both bits 4/5 to 0 "will reset and start the ICD2 packet receiving circuit". Injects the pulse after bit 64 (command must not dispatch), then re-sends cleanly (must dispatch), so it cannot be satisfied by ignoring MLT_REQ. Fails when the partial-packet reset is removed. Weaker sourcing than the rest: an application of the documented "reset" wording, corroborated by sgb-ext-test's real-SGB reference. |

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

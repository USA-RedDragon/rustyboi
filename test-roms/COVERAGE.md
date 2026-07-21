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

## Hardware-bench measurement ROMs (`bench` grading — no verdict, raw capture)

These are the inverse of the table above: cells where the behavior is NOT
silicon-verified, so no ROM may assert it. They are built to `build-bench/`,
carry no manifest row, and record raw bytes to cart SRAM for an operator to read
back. Each is retired the moment the bench answers it — either into a real
graded ROM, or into a documented fix.

| Question | ROM | Our model's (unverified) position |
|---|---|---|
| Does KEY0 / DMG-compat mode on CGB silicon change the APU, or does the sound die ignore it? | `apu/compat_wave_ram.dmgoncgb.bench` | CGB rules apply in compat mode (the CH3 wave-RAM quirk is gated on silicon, not cart mode — the `cgb` argument to `audio.sync_cc` in `memory/mmio.rs`). Basis: SameBoy `Core/apu.c` uses `GB_is_cgb` 12× and `GB_is_cgb_in_cgb_mode` 0×, plus "the sound die has no mode bit". Instrumenting all four gated sites over the 4,950-row corpus found ZERO discriminating accesses, so no existing suite can confirm or refute it. |
| On the post-STOP LY-increment "glitch dot", does AGB fold `ly & (ly+1)` like CPU-CGB-B/C, or read the stale `ly` like CPU-CGB-D/E? | `ppu/stop_ly_glitch.cgb.bench` | AGB folds, i.e. it sits on the **CGB-B/C** side. That is INHERITED, not measured: it falls out of the bare `is_cgb_de()` at both arms of the fold in `ppu/controller.rs`, and the LY-glitch fold is outside the four families `Mmio::set_cgb_de` documents as deliberate. **rustyboi's PREDICTION (a model output, NOT truth)**: on `Hardware::CGB` the ROM records the folded partial latch at 7 glitch-dot samples of block `$03` (rounds 5/15/25/33/37/41/45 — `0x34`, `0x10`, `0x8C`, `0x8C`, `0x30`, `0x88`, `0x34`, each one less than the odd LY it replaces); on `Hardware::CGBE` those same samples instead read the stale odd LY (`0x35`, `0x13`, `0x8D`, `0x8D`, `0x37`, `0x89`, `0x35`); on `Hardware::AGB` the fold ALSO applies, but at AGB's own 7 glitch-dot samples (rounds 5/15/25/33/37/41/45 at different pads — `0x38`, `0x00`, `0x88`, `0x88`, `0x34`, `0x80`, `0x38`), because AGB's unrelated FF41/LY-153 differences shift its whole read alignment, so the AGB and CGB records are NOT byte-comparable to each other — compare each against its own flipped counterpart, not across consoles. **Discrimination is verified**: temporarily flipping both arms to `is_agb() || is_cgb_de()` moves exactly those 7 AGB bytes (`0x88`->`0x8B` at LY 139, `0x80`->`0x87` at LY 135, `0x00`->`0x0F` at LY 15, `0x38`->`0x39`/`0x3B`, `0x34`->`0x35`) and moves ZERO bytes on CGB — so this ROM can actually answer the question, which no capture in the existing corpus can (all 150 graded AGB rows are unchanged by the same flip). |
| On DMG, what does a second NR44 trigger do while the first one's 6-cycle delayed start is still in flight? | `apu/ch4_retrigger_deferral.dmg.bench` | The second trigger RESTARTS the deferral, so a retrigger burst produces exactly one eventual start, 6 APU cycles after the last write (`Noise::trigger` in `audio/noise.rs`). The alternative is an immediate start at the second write plus a ghost start at the original crossing. Basis: first principles ("one delayed-start pipeline, re-entering it re-runs it"). SameBoy models the 6-cycle delay but explicitly declines this cell — `Core/apu.c` ~:1213, "TODO: When restarting a channel right after starting it… Only certain behaviors of this edge case are emulated" — and no published ROM discriminates the two. **Prediction under our model** (measured by running the bench ROM under rustyboi, DMG): cells `$01`/`$02`/`$04` all read NR52 = `$F8`; the discriminator `$03` reads `$F0`, i.e. bit 3 still clear at W2+4. |
| On DMG, is channel 4's envelope frame-escape race anchored at the NR44 write or at the delayed start's +6 crossing, and how wide is the escape window? | `apu/ch4_env_race.dmg.bench` | Anchored at the CROSSING (`env_trigger_cc = self.deferred_reapply_cc.unwrap_or(self.cc)` in `audio/noise.rs`), with an escape window of exactly 2 APU cycles (`event_cc.wrapping_sub(env_trigger_cc) <= 2` in `audio/envelope.rs`). Neither is measured: the anchor is a first-principles inference and the 2-cycle constant has no published source. Not CPU-observable at all on DMG — the ROM sweeps the trigger across a 64 Hz frame and the result is an audible 15.625 ms step in silence onset that an operator captures from line out. **Prediction under our model** (measured from rustyboi's emitted PCM, DMG): the escape edge sits at D = 14335 M-cycles for the non-deferring series and D = 14332 for the deferring one — exactly 3 sweep steps (6 APU cycles) earlier — and each edge is one step wide. Note that rustyboi puts the first envelope event after the anchor at 7×2048 M-cycles, not the 8×2048 that `include/apu.inc`'s step numbering implies; the ROM sweeps both candidates rather than betting the bench run on our phase model. |

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

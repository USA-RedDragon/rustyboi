# DS sub-dot phase engine — the coupled timing-core build

Goal: drive the gambatte hwtests suite from main@131 toward the floor (~4 + ~12 harness).
The remaining reducible bulk (~50-70: m2int, speedchange-lcdoff, lyc, m1, much of hdma-ds,
~21 APU, scx-steady) is ONE coupled root: the DS sub-dot phase (renderer `line_cycle`, timer
`div_anchor`, APU `cycleCounter_`, `p_now`) is realized on a per-dot grid with TUNED compensating
offsets instead of being sub-dot exact. Every peripheral inherits the rounded phase; tests bracket
it so constants are 1-for-1 swaps. Fix = make the DS sub-dot phase exact across PPU+timer+APU
SIMULTANEOUSLY and delete the compensations together.

## Proven facts (cctracer, this campaign — do NOT re-derive)
- CPU dispatch is FAITHFUL (full event-cc gate = net +0; bias = swap). m0Time predictor BYTE-EXACT.
- APU rebase to Gambatte cycleCounter_/lastUpdate_ is CORRECT + byte-exact (single→double cc=4097,
  base nr52 cc=8193 — one counter serves both regimes). Mechanics proven: faithful generateSamples
  (`cycles=(abs-lu)>>(1+ds); lu+=cycles<<(1+ds)`), floored `lastUpdate_`, parity (`reset`:
  `lu=((lu+3)&-4)-!ds`; boot `lu=abs-1`), atomic STOP switch (divReset(old_ds)@stopcc →
  generateSamples(stopcc+8*!old_ds) → single↔double fold). Net +10 ALONE — gated on timer DIV-phase.
- Timer `_ds` regressions root: stop-cc DIV-phase bucket (`cycleCounter_ & 0x800`, bit-11/12 at STOP).
  rustyboi ~0x800 low (4096 vs Gambatte 8192 bucket) — from `div_anchor`/STOP_DERIV_OFF/STOP_APU
  offsets tuned for the renderer line_cycle.
- PPU m2 6cc-late post-double-STOP: renderer `line_cycle` 6 dots behind Gambatte; m2 sched
  (`lcdstat_change` `lc.time=abs_cc+456-line_cycle`) inherits it. Only lever = line_cycle = bridge.
- `_1`/`_2`, `_1a/_1b/_2a/_2b` are BRACKET pairs straddling the rounded phase → any constant swaps.

## Offsets to DELETE (the compensations) — inventory
- timer.rs: `STOP_DERIV_OFF=-4`, `STOP_TIMA_SS/DS_EXTRA`, `STOP_APU_SS/DS_EXTRA`,
  `STOP_EI_PROMOTE_ADJ_*`, `div_anchor_apu` (the APU-vs-DIV split anchor).
- opcodes.rs stop(): the `bridge` dot-counts (6/8/3/5), `arm_sc_mode3_pullback`/`take_*`,
  `set_dsss_lytime_adjust`, the hdma suppress-edge fudges.
- ppu/controller.rs: `m2irq_off_ds`/`m0irq_off_ds`/`write_cc_off_ds`, the firing `+ds` fudges,
  `cgb_ss_m0_anticip`, the lytime `+1` corrections.
- cpu/bus.rs: `step_subdot` + the DS parity-gate (resolve_one_dot:81) — the per-dot rounding.

## Target invariant
Every peripheral resolves at the TRUE master_cc (sub-dot, including odd DS half-dots). The renderer
keeps its 1-pixel-per-2cc cadence, but `line_cycle`/mode-boundary tracking, `div_anchor`,
`cycleCounter_`, and `p_now` all carry full master_cc precision. With that, the STOP switch
re-anchors all three at the EXACT Gambatte switch cc (`instr_start + (ds?0:4)`) with NO bridge
dot-count, and the offsets above are deleted.

## Staging (flag-gated RB_SUBDOT; flag-OFF == 131 always, bounded valley)
- Stage 0 (THIS): scaffolding — `RB_SUBDOT` ENV flag (off=identity; env OK on-branch for testing,
  INLINE/remove before main like RB_EXACTCC), this doc. Zero behavior change.
- NOTE: Stages 1-4 are validated by cctracer BYTE-EXACTNESS of intermediate quantities (line_cycle,
  m2 event cc, DIV-phase, APU cc) — NOT suite count, which only moves at Stage 5. (Earlier coupled
  one-shots valleyed precisely because they were suite-judged mid-coupling.)
- Stage 1 (FOUNDATION, hardest): make `line_cycle`/mode-boundary tracking sub-dot exact at DS —
  remove the parity-gate ROUNDING for line_cycle/STAT (keep renderer pixel cadence). Validate
  line_cycle == Gambatte lineCycle at odd+even master_cc via cctracer FF44/FF41 on a DS canary.
  STATUS: DONE (commit on ds-subdot-engine). Root found: CPU FF41/FF44 reads ALWAYS land on even
  master_cc at DS (Gambatte resolves all register reads at even cc), so the "odd-dot stale" is the
  renderer GRID PARITY — `abs_cc`/`line_cycle` advance on the even-render-dot grid which sits 1
  master_cc below Gambatte's even LINE phase at DS, so the bare `lyTime` runs 1cc low and
  `lineCycles=456-((lyTime-cc)>>1)` reads +1 high. FIX: `ly_counter_obs()` (flag-gated, DS-only +1,
  honors `lytime_no_plus1`) used by the CPU READ observers (get_stat_mode_at_cc, the midframe
  first-line branch, get_lyc_flag_at_cc); the internal STAT-event SCHEDULE keeps the un-corrected
  `ly_counter` (its fire-cc re-anchor is Stage 2-4), so read CCs do NOT shift. m0Time/cgbp anchors
  untouched (their `plus1` already encoded the DS +1; not double-counted). PROOF (cctracer, CGB DS):
  m2int_scx{1..5}_m3stat_ds_{1,2} ALL byte-exact lineCycles+mode+m0Time+lyTime — e.g. scx1 ds_1
  lineCycles 251->250, lyTime 140567->140568(==198944-boot), mode3; ds_2 253->252, mode0. SS
  byte-identical flag-on/off (DS-only correction). REGRESSION GUARD: flag-OFF suite = 131 (114 CGB +
  17 DMG), exact identity. Flag-ON suite = 132 (+1): ONE first-line count-boundary straddle
  `enable_display/frame0_m3stat_count_ds_2` (a 1-for-1 bracket whose first-line `lu_`/
  `display_enable_inactive_until` anchor is re-anchored in Stage 2); 0 fixed (expected — offsets
  still compensate the old phase until Stage 5).
- Stage 2: STOP switch sub-dot re-anchor — line_cycle + p_now advance EXACTLY across the switch
  (delete bridge dot-counts + pullback + lytime_adjust). Validate m2 event cc == Gambatte
  (speedchange2_lcdoff canaries; the 6cc lag → 0).
  STATUS: DONE for the DS->SS-during-OAMSearch case (commit on ds-subdot-engine). Gambatte
  `Memory::stop` runs `lcd_.speedChange(cc_=cc+8*!old_ds)` = `update(cc_)` (old speed) then
  `ppu_.speedChange()` (`p_.now -= old_ds`, lineCycle PRESERVED) then reschedule at new speed.
  For DS->SS `cc_==cc` so `update` advances ZERO dots and `p_.now=cc-1`. PROOF via cctracer
  stop-hook (CCT_STOPDBG, since reverted): switch2 cc=197736 cc_=197736 lineCycle=16 ly=0 →
  Gambatte `p_.now=cc-1`. rustyboi at that switch: pre-bridge line_cycle=16 (== Gambatte) and
  `p_now+abs_cc == master_cc-1` (== Gambatte) — the Stage-1 per-dot stepper ALREADY lands the
  exact phase in OAMSearch. So the faithful bridge there is 0; the old `+3` over-advanced
  line_cycle, leaving lyTime/m0Time 2cc low (the failing m2). FIX (RB_SUBDOT, opcodes.rs stop):
  `faithful_dsss = subdot && !to_double && ppu.is_in_oam_search()` → bridge=0, skip
  set_dsss_lytime_adjust, consume the pullback marker. RESULT (cctracer-anchored, boot offset
  58371 from the m2 event): scx1_1 lyTime 271129->271132 == Gambatte 329503 (BYTE-EXACT, the
  3-dot/6cc switch lag DISSOLVED); m2 event byte-exact relative to lyTime (both fire lyTime_133-4).
  scx1_1 mode3 PASS, scx1_2 mode0 PASS (BEFORE: scx1_1 mode0 FAIL — m0Time 2cc low). Residual ±1
  (read access_cc -1, m0Time-vs-lyTime +1) is the SEPARATE per-access-cc / m0-phase root (NOT
  Stage 2); it flips the boundary-exact scx4_2 bracket (Gambatte margin 0=mode0; rustyboi +2) —
  a 2-for-2 swap with scx1_1, NET-ZERO. SCOPE LIMIT: switches DURING/after mode-3 (PixelTransfer/
  HBlank, e.g. ly44_m3) keep the per-dot stepper's mode-3-length phase deficit and the tuned
  bridge — their faithful re-anchor couples to the mode-3-length work (deferred). SS->DS bridge
  unchanged. GUARDS: flag-OFF full suite = 131 (byte-identical to main_131, verified). Flag-ON
  full = 132 (== Stage 1; Stage-2 net-zero: scx1_1 fixed / scx4_2 broke; the +1 is the carried
  Stage-1 `enable_display/frame0_m3stat_count_ds_2` leftover — NOT resolved by Stage 2, its
  first-line enable anchor is re-anchored later).
- Stage 3: timer div_anchor sub-dot at STOP (delete STOP_DERIV_OFF/TIMA/APU extras, unify
  div_anchor_apu). Validate DIV-phase bucket (`&0x800`) == Gambatte at STOP (speedchange_tima).
  STATUS: DONE, coupled with LEVER A below (one build). Under RB_SUBDOT `stop_div_reset`
  collapses to `anchor_cc = tima_cc = apu_cc = abs_cc` (no STOP_DERIV_OFF/TIMA/APU extras,
  no EI-promote adj, div_anchor_apu unified). Verified byte-exact: Gambatte's divReset cc
  == STOP cc_in in EVERY tima canary (cctracer DIVRESET hook: speedchange2_tima00_2a 67680/
  199264, tima01_1 66624/197716, tima02_1a 66464/197560), and rustyboi abs_cc maps with the
  CONSTANT 58368 offset. ALL speedchange tima/div tests pass flag-on (0 tima/div in the
  flag-on broke set vs the +56 they were with the old offsets pre-LEVER-A).

## LEVER A — DS-entry per-access cc skew (THE FOUNDATION) — DONE (with Stage 3)
ROOT (cctracer, CCT_STOP/CCT_DIVRESET hooks, since reverted): Gambatte `case 0x10` is
`cc() = mem_.stop(cc() - 4)` AFTER `PC_READ_OPERAND` (+4) — the unhalt window `0x20000 + 4`
is measured from the PRE-operand cc (= just after the opcode fetch). rustyboi's opcode fetch
ALREADY ticks master_cc +4 (one M-cycle) before `stop()` runs, and `tick_remaining` charges
the returned 8 against that already-ticked M-cycle, so the returned 8 only NETS +4 of advance
— folding the opcode-fetch tick INTO the STOP's 8 and losing 4cc across the whole window.
MEASURED (speedchange2_lcdoff…scx1_1): abs_cc↔Gambatte offset 58368 at STOP1, drifting to
58372 by the SS→DS resume (rustyboi 4 short). FIX (opcodes::stop, RB_SUBDOT): no-fetch stall =
`0x20000` (not `0x20000 + 4 - 8`); the returned 8 contributes (8-4)=4 net, so the window from
the post-opcode cc is exactly `0x20000 + 4` = Gambatte's `(cc()-4) + 0x20000 + 4`.
PROOF (AFTER): offset CONSTANT 58368 at STOP1 (66612/8244) AND STOP2 (197736/139368), and the
whole inter-STOP instruction stream byte-exact (0x0164..0x016D all offset 58368). divReset
byte-exact at anchor=abs_cc (above). Stage-1 m2int_m3stat canaries 0/0 flag-on/off; Stage-2
speedchange2…scx1_1 PASS flag-on. REGRESSION GUARD: flag-OFF full suite = 131 (114 CGB + 17
DMG), exact identity. FLAG-ON full suite = 214 (vs the pre-LEVER-A flag-on baseline 132):
net +82, ALL CGB. The valley is ENTIRELY the downstream DS firing-offset coupling that LEVER A
exposes (NOT a LEVER-A bug — abs_cc is now byte-exact everywhere, verified on the regressing
`offset1_lyc99int_m2irq_count_1` too: STOP offsets 58368/58368, abs_cc perfect, only the
rendered LY-count is off). Breakdown of the +82: 41 `ly44_m3` (mid-mode-3 SS→DS bridge,
explicitly deferred — couples to mode-3-length), 17 APU (Stage 4), and ~41 double-speed
`lcdoffset*/offset*_lyc*` count tests whose controller.rs DS firing offsets (`m2irq_off_ds`,
`m0irq_off_ds`, `write_cc_off_ds`, the `+ds` fudges, `cgb_ss_m0_anticip`, the lytime `+1`) and
the SS→DS/mid-mode-3 `bridge` dot-counts in opcodes::stop ENCODE the now-eliminated 4-short
abs_cc and must be rebased (this IS Stage 5: delete/rebase the firing offsets + bridge). 16
fixed flag-on (the tima/div + several APU `_b` + the scx1_1 m2 canaries). GATE (per roadmap)
MET: cctracer byte-exactness + flag-OFF=131 + Stages 1-2 canaries byte-exact; the flag-on
valley is the documented Stage-5 trigger, not a regression of main (main stays 131).
BLOCKER for net-positive flag-on: Stage 5 (rebase controller DS firing offsets -4, make the
SS→DS / mid-mode-3 bridge faithful) — large coupled build, deliberately separate.
- Stage 4: APU rebase (mechanics proven above) — now lands (DIV-phase exact). Validate whole sound
  suite (length/nr52/duty/env/sweep/wave) byte-exact.
  STATUS: DONE (commit on ds-subdot-engine). Under RB_SUBDOT the APU clock is the FAITHFUL
  single-counter model (audio/controller.rs): `advance_to` = Gambatte generateSamples
  (`cycles=(abs_cc-last_update)>>(1+ds); last_update += cycles<<(1+ds)`, so `last_update` is the
  FLOORED boundary, parity preserved); `len_cc` collapses to mirror `cc` (no LEN_FOLD_BIAS/
  LEN_CC_OFF/dual clock); boot anchor `last_update = abs_cc-1` deferred past the abs_cc==0 pre-boot
  sync; `psg_reset` faithful (no drift bias, `last_update=((lastUpdate_+3)&-4)-!ds` parity
  re-anchor); divReset/`set_read_len_cc`/`set_write_len_cc` use the single counter with the
  BEFORE-shift delta. The atomic STOP switch is `psg_speed_change_at(old_ds, stop_cc)` (mmio.rs
  `perform_speed_switch` under subdot): sync at OLD ds so the divReset fold runs at the old speed
  (KEY1 is already toggled, so it must pass the captured `old_ds`), then flush
  `generateSamples(stop_cc + 8*!old_ds)`, then the single↔double divCycles/2 fold.
  PROOF (cctracer PSG hook on setNr14/setNr24/divReset/speedChange, since REVERTED + cctracer
  rebuilt pristine): the DUTY single→double canary (speedchange_ch1_duty0_pos6_to_pos7_timing_2)
  is now BYTE-EXACT through the whole switch — divReset 4024->4096, spdchg pre cc=4096, flushed
  cc=4100/last_update=8055 (==Gambatte 4100/66423−58368), fold post cc=4097, post-switch trigger
  cc=37282 (==Gambatte, pos7). BEFORE Stage 4 RB_SUBDOT gave 37283 (+1, pos6) because the old
  reconstruction did not absorb Lever A's exact STOP cc. The base nr52 canary
  (speedchange2_ch2_nr52_2b) trigger cc=4164 (==Gambatte), len_cc 4165->4164 (single counter).
  RESULTS: flag-ON SOUND subset (sound+speedchange+div dirs) net −30 vs the Lever-A flag-on base
  (82->52): +30 FIXED = all ch1_duty0_pos6_to_pos7 (single→double + _ds + _nop variants),
  ch2_nr52_*a/*b length-boundary, ch2_late_*_nr52, ch2_reset_length_counter; ZERO sound
  regressions. The remaining 52 flag-on sound failures are the ly44_m3 mid-mode-3 SS→DS bridge +
  scx PPU firing-offsets (Stage 5, deferred — pre-existing at the Lever-A base, NOT Stage-4).
  REGRESSION GUARD: flag-OFF full suite = 131 (114 CGB + 17 DMG), exact identity. The whole
  rebase is behind RB_SUBDOT; flag-off keeps the legacy `>>1`-anchored dual-clock reconstruction
  byte-identical.
- Stage 5a (firing-offset cluster recovery): rebase the DS firing/bridge offsets so the
  lcdoffset1/offset[123]/lyc*/m2int_m3stat cluster that main_131 PASSES but the Lever-A flag-on
  valley BROKE is recovered under RB_SUBDOT.
  STATUS: DONE (commit on ds-subdot-engine). ROOT FOUND (cctracer + runner DBGLY/DBGW/DBGSTOP,
  since reverted): the 40 cluster cases all execute a DOUBLE STOP at boot (SS->DS during VBlank,
  then DS->SS during PixelTransfer = mode 3 of an early line), then DEFER the graded LY/STAT/cgbp
  read to a much later frame (LY 152/153, the bracket boundary). The 2nd stop is a mode-3 DS->SS
  switch, so the tuned `bridge` (opcodes::stop else-branch, base 3/5) was calibrated for the OLD
  4-short abs_cc; once Lever A made abs_cc byte-exact the bridge OVER-ADVANCES the renderer line
  phase by exactly 2 dots, and the deferred read's LY-increment-vs-access boundary lands on the
  wrong side (lcdoffset1/offset1 FAIL, lcdoffset3/offset3 PASS — a 1-for-1 straddle; e.g.
  offset1_lyc98int_ly_count_1 read LY=0 via the line-153-reads-0 rule where Gambatte anticipated
  152->153=0x99, because rustyboi's 152->153 increment fired ~5cc early relative to the read).
  FIX (opcodes::stop, RB_SUBDOT only): the mode-3 DS->SS bridge rebases `-2`
  (`base.saturating_sub(2)`), gated OUT when HDMA is enabled (the hdma_late_m3speedchange_*_ds
  cases couple to the HDMA block-fire/timer phase across the switch — keep the tuned bridge there,
  Stage 5b). PROOF (byte-exact, all bracket sides served by the SINGLE -2 — the roadmap invariant):
  offset1/offset2/offset3_lyc98int_ly_count_1 AND _2 ALL PASS (displayed count = 0x99/0x9A exact);
  the lcdoffset1 enable brackets (late_enable / late_ff41/ff45_enable / cgbpal_read/write_m3start /
  preread/prewrite / m1irq_late_enable / ly143_late_m0enable) PASS with their lcdoffset3 partners.
  RESULTS: flag-ON full suite 184 -> 148 (net -36): 37 RECOVERED of the 40-case cluster, 1
  net-zero bracket swap (offset1_lyc99int_m2irq_count_1 broke->fixed / count_2 fixed->broke; the
  m2irq read straddles a sub-dot the whole-dot bridge can't split — couples to per-access m2irq
  event-cc, Lever-A residual). 3 RESIST: frame0_m3stat_count_ds_2 (the carried Stage-1 first-line
  enable-anchor leftover) + speedchange2[_nop]_lcdoff_nop_m2int_m3stat_scx4_2 (LCD-OFF around the
  stop -> the re-enable first_line anchor, not the mode-3 bridge — separate root). Remaining flag-on
  broke = 41 ly44_m3 (Stage 5b) + 1 oamdma (harness) + the 3 above. REGRESSION GUARD: flag-OFF
  full suite = 131 (114 CGB + 17 DMG), EXACT identity (the -2 and the HDMA gate are both behind
  `subdot`). NOTE: the faithful-getLyReg port (drop the +1 lyTime correction, return the
  cc-resolved counter instead of deferring to the renderer register) was attempted first and
  NET-REGRESSED (-2: fixed offset3 brackets but broke offset1_ds_2/offset2_1/offset2_3) because the
  read-resolution was never the lever — the renderer LINE PHASE (the mode-3 bridge) is. Reverted.
- Stage 5b (mid-mode-3 SS->DS / DS->SS bridge — the ly44_m3 mode3-then-mode3 switch):
  STATUS: DONE (commit on ds-subdot-engine). ROOT FOUND (cctracer + runner RB_DBG_M3STAT
  m3stat-read hook, since reverted): the ~41 `*_ly44_m3_*_m3stat_scx{1..4}` cases switch speed
  WHILE the PPU is in mode-3 (PixelTransfer) at LY44, then defer the m3stat FF41 read to a much
  later line (LY 37/58). With Lever A holding abs_cc + access_cc byte-exact, the m3stat read's
  access_cc is now EXACTLY Gambatte's, but the renderer LINE PHASE (line_cycle / m0Time_master)
  was 2 dots over-advanced across the switch — so `getStat`'s `cc+2 < m0Time` boundary flipped
  mode3->mode0 (the m0Time inherits the line phase, landing 4cc low at DS / 2cc low at SS).
  The over-advance came from the TWO tuned mid-mode-3 bridge dot-counts (calibrated for the old
  4-short abs_cc): the SS->DS PixelTransfer/rendering-line bridge (6) and the DS->SS pullback
  "restore" (+2 -> base 5). FIX (opcodes::stop, RB_SUBDOT only): (1) SS->DS mid-mode-3 bridge
  rebased 6 -> 4 (the renderer no longer overshoots the post-switch line phase by 2 dots); (2)
  DS->SS bridge: the pullback `+2` restore is dropped under subdot (the preceding SS->DS bridge
  is now itself faithful, so there is nothing to restore) — base stays 3 and the Stage-5a -2
  rebase lands it at 1, serving BOTH the Stage-5a VBlank-then-mode3 cluster (no pullback) AND the
  ly44_m3 mode3-then-mode3 double switch (pullback). PROOF (cctracer byte-exact, boot offset
  58368): single-switch `speedchange_ly44_m3_m3stat_scx1_1` read access_cc=174972 m0Time=174976
  lineCycles=250 == Gambatte (233340/233344/250) -> mode3 PASS (BEFORE: m0t=174972 lineCycle=252
  -> mode0 FAIL). Double-switch `speedchange2_ly44_m3_m3stat_scx3_1` access_cc=305572 m0Time=305575
  lineCycles=251 == Gambatte (363940/363943/251) -> mode3 PASS. `..._nop_m3stat_scx1_1` access_cc=
  305572 m0Time=305575 lineCycles=249 == Gambatte (305572/305575/249). The SINGLE faithful re-anchor
  serves ALL scx{1..4} + _nop/_nopx2 (the roadmap invariant — abs_cc exact -> one formula, no
  per-scx fudge): all 66 `*ly44_m3*m3stat_scx*` CGB cases PASS flag-on (0 FAIL). RESULTS: flag-ON
  full suite 148 -> 106 (net -42). vs main_131: net -25 (fixed 28 = the Stage-4 APU + Stage-5a
  + 6 hdma_late_m3speedchange wins flag-on holds over main; broke 3 = the documented residuals
  ONLY: frame0_m3stat_count_ds_2 [Stage-1 first-line enable-anchor leftover] + the two
  speedchange2[_nop]_lcdoff_nop_m2int_m3stat_scx4_2 [LCD-OFF-around-stop separate root, NOT the
  mode-3 bridge]). ZERO ly44_m3 in the broke set; the 29 wins + 37 Stage-5a recoveries intact.
  REGRESSION GUARD: flag-OFF full suite = 131 (114 CGB + 17 DMG), EXACT identity vs main_131 (the
  4->bridge rebase and the dropped pullback +2 are both behind `subdot`). The lcdoff_m2int and
  HDMA-gated DS->SS paths are untouched (verified: lcdoff takes bridge=8 SS->DS-VBlank then the
  faithful_dsss=0 OAMSearch path; never the changed mid-mode-3 branch).
- Stage 5c (finalization — flip the sub-dot engine permanent + remove the flag):
  STATUS: DONE (commits on ds-subdot-engine). `subdot_enabled()` flipped to
  unconditional true, then EVERY `if subdot_enabled() { NEW } else { OLD }` site
  inlined to NEW and the OLD compensation branch deleted, incrementally with the
  full suite (NO env var) held at 106 after EACH file:
    * timer.rs: `stop_div_reset` collapses to the bare `abs_cc` anchor; DELETED
      the dead constants `STOP_DERIV_OFF`, `STOP_TIMA_{SS,DS}_EXTRA`,
      `STOP_APU_{SS,DS}_EXTRA`, `STOP_EI_PROMOTE_ADJ_{SS,DS}` and the old
      direction-split derivation.
    * audio/controller.rs + mmio.rs: the faithful single-counter APU clock is the
      ONLY path; removed the `subdot` field, the legacy `>>1` dual-clock
      reconstruction, the legacy `psg_speed_change`, and the `LEN_FOLD_BIAS` /
      `LEN_CC_OFF` constants. `perform_speed_switch` uses `psg_speed_change_at`.
    * opcodes.rs stop(): inlined the faithful DS->SS OAMSearch re-anchor, the
      SS->DS mid-mode-3 bridge=4, the DS->SS mode-3 bridge=1 (HDMA-gated), and the
      LEVER-A unhalt stall 0x20000; deleted the old 6/3/5 + `0x20000+4-8`.
    * ppu/controller.rs: `ly_counter_obs` DS +1 sub-dot correction unconditional.
    * cpu/bus.rs: `subdot_enabled()` DELETED entirely. No `RB_SUBDOT` references
      remain anywhere; the core is env-free. `cargo build` is dead-code-warning-
      clean (release + debug lib).
  KEPT (load-bearing, NOT compensation): `step_subdot` and the DS parity-gate in
  `resolve_one_dot` — the legitimate double-speed 1-pixel-per-2-master_cc render
  cadence reached by the now-default sub-dot path. The roadmap's "delete
  step_subdot/parity-gate" was aspirational; removing them would break DS render.
  ALSO KEPT (still live on unconditional paths, NOT flag-gated — deleting would
  change behavior and break 106): the controller DS firing offsets
  `m2irq_off_ds`/`m0irq_off_ds`/`write_cc_off_ds`/`cgb_ss_m0_anticip`, the
  pullback (`arm/take_sc_mode3_pullback`) + `set_dsss_lytime_adjust` machinery,
  and `div_anchor_apu`. These are the landed (flag-on) firing model — the
  roadmap's "Offsets to DELETE" inventory described the full-Stage-5 endgame, not
  the 106-holding landed state.
  RESULT: full suite with NO env var = 106, net -25 vs main_131
  (fixed 28, broke 3 = the documented residuals ONLY: frame0_m3stat_count_ds_2 +
  speedchange2[_nop]_lcdoff_nop_m2int_m3stat_scx4_2 x2). Failure set BYTE-IDENTICAL
  to the prior RB_SUBDOT=1 run (diffn vs s5b_on: fixed 0 / broke 0). SMOKE TEST:
  Pokemon Crystal + Harry Potter CoS both ran 600 frames in DEBUG (overflow checks
  ON) with NO overflow panic. The sub-dot engine is now the permanent, env-free
  default — the merge candidate.
- Stage 5 (remaining, follow-up): the 3 residual failures + the further coupled
  cluster (per the original aspiration). Out of scope for 5c.

## Discipline
- cctracer byte-exact per stage BEFORE suite. Flag-off must stay 131 at every stage.
- loadgate before every build/suite. Keep underflow guards (suite is RELEASE).
- Each stage: a bounded understood valley behind the flag is OK; the LANDED (flag-on) state must
  net-improve by Stage 5 or the flag stays off. main NEVER regresses below 131.

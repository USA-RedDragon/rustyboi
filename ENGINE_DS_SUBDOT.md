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
- Stage 4: APU rebase (mechanics proven above) — now lands (DIV-phase exact). Validate whole sound
  suite (length/nr52/duty/env/sweep/wave) byte-exact.
- Stage 5: delete `step_subdot` + parity-gate + all firing/DS offsets; flip RB_SUBDOT default on.
  Full-suite re-validate; expect the ~50-70 coupled cluster to fall together.

## Discipline
- cctracer byte-exact per stage BEFORE suite. Flag-off must stay 131 at every stage.
- loadgate before every build/suite. Keep underflow guards (suite is RELEASE).
- Each stage: a bounded understood valley behind the flag is OK; the LANDED (flag-on) state must
  net-improve by Stage 5 or the flag stays off. main NEVER regresses below 131.

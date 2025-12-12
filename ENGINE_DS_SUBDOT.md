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
- Stage 0 (THIS): scaffolding — `RB_SUBDOT` flag (off=identity), this doc. Zero behavior change.
- Stage 1 (FOUNDATION, hardest): make `line_cycle`/mode-boundary tracking sub-dot exact at DS —
  remove the parity-gate ROUNDING for line_cycle/STAT (keep renderer pixel cadence). Validate
  line_cycle == Gambatte lineCycle at odd+even master_cc via cctracer FF44/FF41 on a DS canary.
- Stage 2: STOP switch sub-dot re-anchor — line_cycle + p_now advance EXACTLY across the switch
  (delete bridge dot-counts + pullback + lytime_adjust). Validate m2 event cc == Gambatte
  (speedchange2_lcdoff canaries; the 6cc lag → 0).
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

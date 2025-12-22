# Sub-master-cc CPU-store-vs-PPU-latch ordering — Stage-0 scoping + verdict

Goal: drive the gambatte hwtests suite from main@66a563d (**73** = 57 CGB + 16 DMG)
toward the floor by closing the deepest remaining timing lever — the within-`master_cc`
ordering of a CPU memory STORE (SCX/SCY/LCDC/WY) against the PPU BG fetcher's TileNumber
LATCH and the mode-3 line-end / lcdcChange checkpoint.

This doc mirrors `ENGINE_PERACCESS.md` / `ENGINE_DS_SUBDOT.md`: flag-gated `RB_SUBCC`
(env, off == identity == 73 at EVERY stage; inline/remove before any merge to main),
intermediate stages validated by cctracer/runner-trace BYTE-EXACTNESS of the
store-vs-latch order, NOT suite count (which only moves when the render path is rebased).
main NEVER regresses. NEVER push. Build only in `/home/reddragon/rb-subcc` (`f-subcc`).

---

## THE DECISIVE VERDICT (answered with cc evidence)

**ORDER-SOLVABLE ON THE INTEGER-cc GRID. It does NOT require modeling time below
`master_cc`.** The lever is a RENDER-GEOMETRY fix (the fetcher's FIFO-ahead column
derivation vs Gambatte's `xpos`/`update(cc)` plot-time apply), driven entirely by
integer cc. No half-cc / sub-dot tiebreak field is needed.

This OVERTURNS two stale claims:
- The memory `scx-during-m3-plus2cgb` / `deep-floor-at-73` "FINAL" claim that `_4`/`_6`
  are *byte-identical at every integer-cc observable* and therefore need a
  sub-master-cc clock. **Falsified in the current tree** (measured below): the DS-sub-dot
  + per-access engines that landed since those notes made `abs_cc` byte-exact, and the
  variants now DIVERGE at integer cc (write_cc 612 vs 620; an extra f1 fetch in `_6`).
- The `ENGINE_PERACCESS.md` Stage-5 claim that scx "needs a per-COLUMN-scx closed-form
  renderer." That analysis was about the **disabled** `render_full_line`
  (`linerender_enabled()==false`, controller.rs:113 — REFUTED to true; the live path is
  the per-dot FIFO). The live per-dot fetcher already re-reads `scx_delayed` per tile;
  the bug is the FIFO-ahead column geometry, which IS integer-cc addressable.

### Evidence A — Gambatte's mechanism is integer-cc, never sub-cc
libgambatte `src/video.cpp`:
```
void LCD::scxChange(unsigned newScx, unsigned long cc) {
    update(cc + 2 * ppu_.cgb());   // render the fetcher up to an INTEGER cc, old scx
    ppu_.setScx(newScx);           // THEN commit
    mode3CyclesChange();
}
void LCD::scyChange(...) { update(cc + 2*cgb); setScy(...); }                 // same shape
void LCD::lcdcChange(data, cc): CGB non-enable: update(cc+1); setLcdc(td,cc+1);
                                                update(cc+2); setLcdc(data,cc+2);  // staged +1/+2
void LCD::wyChange:  update(cc + 1 + cgb); apply p.wy                          // wy1 latch
```
Every "order" op is `update(cc + FIXED_INTEGER_OFFSET)` then apply. `update(cc)` advances
the PPU renderer to an **integer** cc (`p.now`, `p.cycles`, `p.xpos` are integer-cc
quantities). The ONLY thing that differs between the `_3/_4/_5/_6` variants is the integer
`cc` of the write — there is no continuous sub-cc anywhere in the apply path.

cctracer `[SCXCHG]` hook (temporary, REVERTED + rebuilt pristine) on `scx_0761c0`, the
2nd write per line (0x07->0x61), CGB, boot offset removed via lyTime anchoring:
```
variant  write_cc  upd_cc(=wc+2)  xpos_post   (xpos = fetcher position at apply)
 _3       80716     80718          3
 _4       80720     80722          7
 _5       80724     80726          4 (next tile)
 _6       80728     80730          8
```
The variants are a clean **4-cc integer stride**; `update(wc+2)` lands `xpos` deterministically.
The straddle tile gets old-vs-new scx by whether its `xpos` is reached before/after `wc+2`.

### Evidence B — the discriminator is integer-cc-visible in rustyboi NOW
runner `[RB_SCXW]`/`[RB_FETCH]` hooks (temporary, REVERTED) on `scx_0761c0`, LY=1, CGB,
2nd write (new_scx=97) and the f1 fine-scroll prologue:
```
_4: SCXW write_cc=612 abs_cc=612 x=0 ticks=88
    FETCH abs_cc=608 x=0 fifo=0 pending_discard=0 scx_delayed=7   <- f1 tile 1
    SCXW  612 ...                                                  <- store applies
    FETCH abs_cc=616 x=0 fifo=8 pending_discard=0 scx_delayed=97  <- next tile, new scx
_6: SCXW write_cc=620 abs_cc=620 x=0 ticks=96   (+8 cc vs _4)
    FETCH abs_cc=608 x=0 fifo=0 pending_discard=0 scx_delayed=7   <- f1 tile 1
    FETCH abs_cc=616 x=0 fifo=8 pending_discard=7 scx_delayed=7   <- EXTRA f1 tile, OLD scx
    SCXW  620 ...                                                  <- store applies LATER
    FETCH abs_cc=624 x=1 fifo=8 pending_discard=0 scx_delayed=97
```
`_4` and `_6` are **NOT byte-identical**: write_cc 612 vs 620, ticks 88 vs 96, and `_6`
has an extra old-scx f1 fetch (abs_cc=616, pending_discard=7) that `_4` lacks. The
discriminator is purely the INTEGER write_cc relative to the fetcher's integer latch dots
(608, 616, 624...). Both bracket cleanly; no sub-cc tiebreak is required to separate them.

### Evidence C — the rendered failure localizes to the FIFO-ahead column geometry
`_4` fails 1144px (= 8px x 143 lines) at x=135..142 (the 0xc0=192 straddle tile); `_3`
fails 8px at x=143..150 y=0 only (first-line f1 prologue); `_5`/`_6` PASS. Trace: `_4`
write_cc=748 (scx=192) at display x=130; the fetcher latches at abs_cc=753 x=134 reading
`scx_delayed=192` and derives the column from `display_x(134)+fifo(9)`. Gambatte instead
decides that displayed column's tile under the scx live when `xpos` reached it
(`update(748+2)` then setScx). The rustyboi fetcher reads scx at the FETCH dot but
projects the column to `display_x+fifo` — so the FIFO depth (the fetch-ahead lead) is the
exact integer-cc offset that misplaces the boundary. This is a column-projection geometry
bug, integer-cc throughout.

### Late-enable / late-wy: same verdict
- `late_enable_afterVblank` (lcdcChange enable toggle): `update(cc)` (old lcdc=off ->
  renders nothing) then `setLcdc(data,cc)` and reschedule ALL events at the exact integer
  `cc`. Order = render-before-apply, integer cc. The CGB non-enable LCDC path stages at
  `cc+1`/`cc+2` (integer). rustyboi's `handle_lcdc_write` runs pre-`tick_m` (the store
  commits, then the M-cycle's dots render) — the apply ORDER is the lever, integer-cc.
- `late_wy` (`late_wy_FFto2`, `late_scx_late_wy_FFto4`): Gambatte `wyChange` =
  `update(cc+1+cgb)` then apply `p.wy`; the weMaster/win-Y checkpoint reads it at that
  integer cc. rustyboi already models this (`on_wy_write` -> `wy1_apply_cc = write_cc +
  WY1_DELAY + cgb`, `wy2_apply_cc = write_cc + WY2_DELAY{_CGB,_DMG} - ds`,
  controller.rs:2703). The residual is a DMG-vs-CGB delay-constant sweep + the late_scx
  interaction, NOT a sub-cc clock — same integer-cc apply-cc-vs-checkpoint-cc family.

**Conclusion:** the discriminator is a fixed sub-M-cycle relationship on the INTEGER grid
(store at `write_cc`, latch/checkpoint at the fetcher-substep / `write_cc + 2*cgb` /
`+1+cgb` cc — all derivable). The fix is to make the per-dot fetcher's column projection
and the store-apply cc match Gambatte's `update(cc+2*cgb)`-then-`setScx` geometry. No
half-master_cc field is needed.

---

## WHERE THE GEOMETRY MISMATCH ENTERS (the sites to fix)

1. **`ppu/fetcher.rs::step` State::TileNumber (~L207-227).** The BG tile-map column is
   `bg_tile_x = (scx + (display_x + fifo.size() - pending_discard) + cgb_adj)/8 % 32`,
   reading `self.scx_delayed` LIVE at the latch dot. Gambatte uses `xpos` (the fetcher's
   own progress, not display+FIFO) and the scx that was live at that `xpos` after
   `update(cc+2*cgb)`. The `display_x + fifo.size()` projection is the FIFO-ahead lead
   that misplaces the straddle boundary by exactly the FIFO depth in cc.
2. **`ppu/controller.rs::on_scx_write` (~L2763).** `scx_apply_cc = write_cc + delay`
   with `delay=0` (column path) — the comment admits "no positive lever; needs the
   read-cc convergent root, out of scope." The f1 path already uses the faithful
   `scx_f1_apply_cc = write_cc + 2*cgb` (scx_f1_pending_at_cc). The column path should
   adopt the SAME `+2*cgb` apply AND have the fetcher project the column at the apply cc
   against the displayed-x geometry, not the fetch-ahead-x.
3. **`cpu/bus.rs::write` else branch (~L764-778).** Store commits (`mmio.write`) then
   `on_scx_write`/`on_scy_write`/`handle_lcdc_write` BEFORE `tick_m`. The store-apply is
   already cc-anchored (write_cc); the issue is purely the fetcher's column projection (1)
   reading the apply against the wrong x. No change to the store ORDER is needed once the
   fetcher geometry is correct.
4. **`ppu/controller.rs::dispatch_stat_events` (~L2149).** `scx_delayed = scx_pending`
   when `scx_apply_cc <= abs_cc` — per-dot, integer-cc. This is correct as-is; it is the
   FETCHER's use of `scx_delayed` (column projection) that is wrong, not the apply timing.

The compensations that PAPER OVER this today and would be rebased/deleted at the final
stage: `on_scx_write delay=0` (vs the faithful `2*cgb`), the `rewrite_first_fifo_tile` /
`scx_f1_*` first-tile latch (subsumed once the general column projection is correct), the
`SCY_DELAY`/`WY2_DELAY_*` swept constants (rebased to the `update(cc+2*cgb)`/`+1+cgb`
faithful offsets).

---

## Target invariant

Each mid-mode-3 SCX/SCY/LCDC/WY store applies at `write_cc + Gambatte's integer offset`
(`2*cgb` for scx/scy, `1+cgb`/`2*cgb` for wy1/wy2, `cc`/`cc+1`/`cc+2` for lcdc), and the
per-dot BG fetcher derives each tile's tile-map column at the cc/x that tile will be
PLOTTED (Gambatte `xpos` geometry: the column the displayed pixel occupies), so a tile
fetched-ahead in the FIFO uses the scx live when its display column is reached — exactly
reproducing `update(cc+2*cgb); setScx`. With that, the straddle boundary lands at the
correct display column for every variant and `_3/_4`/`_ds_2..5`/`spx*`/`scx_attrib`
collapse to one faithful geometry; the f1 first-tile special-case and the swept SCY/WY
delay constants are deleted.

---

## Staging (flag-gated `RB_SUBCC`; flag-OFF == 73 at EVERY stage)

> Validation rule (from the prior two engines): Stages 1-3 are judged by cctracer/runner
> trace BYTE-EXACTNESS of the store-apply cc and the fetcher column projection (the
> `[SCXCHG]` xpos/scx pair vs the rustyboi `[RB_FETCH]` column), NOT suite count — the
> count moves only at the final stage when the geometry is rebased and the f1/SCY/WY
> compensations deleted. loadgate before every build; `-j4`; suite is RELEASE (keep
> underflow guards); revert + rebuild cctracer pristine each session.

### Stage 0 (THIS) — scoping + verdict + scaffolding. DONE.
This doc + the verdict above. `RB_SUBCC` env flag (`cpu/bus.rs::subcc_enabled()`,
OnceLock-cached, default OFF == identity == 73). Zero behavior change. Same pattern as the
historical `RB_PERACCESS`/`RB_SUBDOT`; inline/remove before any merge to main.

### Stage 1 (FOUNDATION) — faithful column projection in the per-dot fetcher (SMALLEST FIRST).
Make `fetcher.rs::step` State::TileNumber derive the BG tile-map column from the tile's
PLOT position (the display x it will occupy) using Gambatte's `(scx + xpos + 1 - cgb)/8`
geometry, where the scx is the value live at the apply cc, instead of the live
`display_x + fifo.size()` projection. Concretely: carry a per-queued-tile FROZEN projected
display-x (extend the FIFO/BgPixel to record the xpos the tile was fetched FOR) so a tile's
column is fixed at fetch time but evaluated against the scx that is live when its display
column is reached. Adopt `on_scx_write delay = 2*cgb` (match the f1 path) under `RB_SUBCC`.
- **Validate (runner [RB_FETCH] + cctracer [SCXCHG]):** on `scx_0761c0` `_4`/`_6`, the
  straddle tile's column == Gambatte's `xpos`-derived column at the same plot x; the
  aligned passing set (`scx_0060c0`, `scx_0363c0` etc.) stays byte-exact. Flag-OFF == 73.
- **Cases unlocked:** `scx_during_m3_3/_4` (SS), `scx_during_m3_spx0/1/2` (DMG+CGB),
  `scx_attrib_during_m3_*` — the steady-state mid-mode-3 straddle (~10-14).
- **Risk/coupling:** MEDIUM-HIGH. Touches the fetcher hot path + the FIFO element.
  Memory `scx-during-m3-plus2cgb` warns the naive `restart_current_tile` re-derive nets
  +81 (drains-FIFO drift) — the FROZEN-xpos approach is exactly its prescribed fix. The
  aligned cases (column unchanged by the write) MUST stay byte-exact: the frozen xpos must
  reproduce the live projection when no straddle occurs. Independently landable.

### Stage 2 — DS f1 / fine-scroll straddle (`scx_during_m3_ds_2..5`, `spx2_ds`, ~6).
Extend Stage 1's frozen-column geometry across the f1 fine-scroll discard at double speed:
the f1 loop already honors `scx_f1_apply_cc = write_cc + (2<<ds)` (controller.rs:2797); fold
the column projection's apply cc to the same `2*cgb<<ds` so the DS straddle (1 PPU dot =
`1<<ds` master_cc) lands the boundary one f1 iteration correctly (memory: scx_0367c0/0761c0
_ds regressed when the dot delay didn't scale with ds).
- **Validate:** `scx_during_m3_ds_2..5` straddle column == Gambatte at DS; SS Stage-1
  cases stay byte-exact. Flag-OFF == 73.
- **Risk:** MEDIUM. DS dot/cc scaling is the known trap; reuse the proven `<<ds` f1 scale.
  Depends on Stage 1.

### Stage 3 — late_wy delay-constant rebase + late_scx interaction (~3).
Rebase `WY2_DELAY_CGB/DMG`, `WY1_DELAY`, `SCY_DELAY` from swept constants to Gambatte's
faithful `update(cc + 1 + cgb)` (wy1) / `update(cc + 2*cgb)` (wy2 via mode3CyclesChange) /
`update(cc + 2*cgb)` (scy) offsets, and verify the `late_scx_late_wy_FFto4` interaction
(both stores in the same M3) orders correctly under the Stage-1 geometry.
- **Validate (cctracer):** wy1/wy2 apply cc == Gambatte `wyChange` on `late_wy_FFto2`,
  `late_scx_late_wy_FFto4`; the passing wy cases stay byte-exact. Flag-OFF == 73.
- **Risk:** LOW-MEDIUM. Mostly constant rebase; the late_scx interaction couples to
  Stage 1's frozen column. Independently landable after Stage 1.

### Stage 4 — late_enable / m0int_m0stat_scx4 (~4) — lcdcChange apply order.
Confirm `handle_lcdc_write` reproduces `lcdcChange`'s `update(cc)`-then-`setLcdc`-then-
reschedule-at-cc (enable) and the CGB `cc+1`/`cc+2` staged non-enable path, so
`late_enable_afterVblank_2/4`, `late_enable_afterVblank_ds_lcdoffset1_2`, and the DMG
`m0int/m0irq_m0stat_scx4_2` (the scx4 fetch-vs-store order at HBlank) land. These are the
lcdc-store-vs-render-checkpoint half of the lever (FACET-1-adjacent); may partly overlap
the PERACCESS FACET-1 carry already landed.
- **Validate:** the enable reschedule event cc == Gambatte; flag-OFF == 73.
- **Risk:** MEDIUM. lcdcChange is the most-tuned write path (DS engine + per-access carry).
  Couples to the landed STAT-phase carry — verify no double-apply. Do LAST.

### Stage 5 — FINALIZE (flip default-on + delete compensations).
Flip `subcc_enabled()` to unconditional true; inline each `if subcc { NEW } else { OLD }`
to NEW; delete the dead compensations (the `on_scx_write delay=0` branch, the
`rewrite_first_fifo_tile`/`scx_f1_*` first-tile special-case IF the general projection
subsumes it, the swept `SCY_DELAY`/`WY2_DELAY_*` constants), incrementally, holding the
full suite after EACH file. Remove the env var (env not allowed in main). FINAL gate: full
suite < 73 with no env, HARD self-verify, smoke-test 2 real ROMs in DEBUG (overflow
checks).

---

## Stage-1 recommendation (smallest contained validatable FIRST step)

**Stage 1 (faithful frozen-column projection in the per-dot fetcher, SS).** It is the
smallest self-contained step that closes the largest sub-cluster (the ~10-14 steady-state
scx straddle cases), validatable by the runner `[RB_FETCH]`-vs-cctracer `[SCXCHG]` column
comparison without a sub-cc hook (the discriminator is integer-cc visible, proven above),
and it is the geometry the other scx stages build on. It carries the known
`restart_current_tile` net-+81 trap (memory `scx-during-m3-plus2cgb`) — the frozen-xpos
approach is the prescribed avoidance; the acceptance gate is the aligned `scx_0060c0` set
staying byte-exact.

## Independence verdict

- **Stage 1** is the foundation (SS scx straddle); independently landable.
- **Stage 2** (DS f1) depends on Stage 1.
- **Stage 3** (late_wy) and **Stage 4** (late_enable/lcdc) each depend on Stage 1 only
  loosely (the late_scx interaction) and are otherwise independent.
- NOT all-or-nothing: stageable behind `RB_SUBCC` exactly like the prior two engines,
  each stage cctracer/runner-trace-validatable, flag-OFF == 73.

---

## Feasibility + recovery estimate

**Stageable behind `RB_SUBCC` (flag-OFF == 73), NOT all-or-nothing, NOT sub-cc-time.**
The two prior engines proved the pattern; this one is SHALLOWER than feared — it is a
render-geometry rebase on the already-byte-exact integer cc, not a new clock.

- **Realistic recovery: ~18-24 of the 73.** Direct targets: ~10-14 scx steady (Stage 1),
  ~6 scx_ds (Stage 2), ~3 late_wy (Stage 3), ~4 late_enable/m0stat_scx4 (Stage 4). A few
  of the hdma/dma `_scx1` cases (`hdma_late_m3speedchange_ly`, `hdma_*_ldaaimm_hdma_scx1`)
  may also carry an scx fetch-vs-store component but are primarily the HDMA event-cc
  family (PERACCESS FACET 3) — count them as upside, not baseline.
- **Floor after this build:** the 16 oamdma/`_dumper` HARNESS cases (memory
  `oamdma-dumpers-are-harness-floor`, NOT emulator bugs) + the residual hdma/EI-service
  event-cc cluster + a few (ch2 env anchor, fexx, bgoff, window-edge). Suite plausibly
  toward ~49-55, with the harness floor (~16) the dominant remainder.
- **Hardest stage: Stage 1** (the fetcher column-projection rebase — the
  frozen-xpos-vs-live-FIFO geometry that nets +81 if done as a naive re-derive). The
  acceptance gate (aligned `scx_0060c0` byte-exact while the straddle flips) is the
  precise tripwire; it is integer-cc validatable, which is the key de-risking vs the old
  "needs sub-cc" framing. Stage 4 (lcdc) is second-hardest (most-tuned write path).
- **Caveat:** this rebases the per-dot fetcher, the live render path for ALL CGB+DMG
  rendering. A subtle column regression would be broad. Flag-OFF == 73 + the aligned-set
  byte-exact gate + a DEBUG smoke of a real ROM are mandatory each stage.

## Discipline
- cctracer/runner trace byte-exact per stage BEFORE suite. Flag-OFF must stay 73 every stage.
- loadgate before every build/suite; `-j4`; keep underflow guards (suite is RELEASE).
- Revert any cctracer/rustyboi instrumentation and rebuild cctracer PRISTINE before
  finishing each session. Best executed as deliberate focused work, NOT parallel one-shot
  agents. main NEVER regresses.

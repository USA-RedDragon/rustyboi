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

### Stage 1 ATTEMPT LOG (session 2026-06-28) — frozen-column built, calibration UNSOLVED, reverted to 59e6eec.

Built the full frozen-plot scaffolding and three candidate mechanisms behind RB_SUBCC.
All reverted (broke 0 was NOT achievable for any candidate; flag-off stayed 73). Concrete
empirical map of the `scx_0761c0` CGB cluster, measured this session (boot offset removed
via the runner's own 15-frame steady state, LY=1):

**Ground truth (flag-OFF, == HEAD) for scx_0761c0 CGB:** fails 6 = `_3` (8px @ x=143 y=0),
`_4` (1144px @ x=135..142 y=1..143), `_ds_2.._5`. `_1 _2 _5 _6` PASS. The per-line 2nd
SCX write (0x61->0xc0, new_scx=192 over old=97) lands at a clean **4-cc integer stride**
across variants, with the draw cursor `self.x` at the write:
```
variant  write_abs_cc  self.x  fifo  OLD-result
 _1        760          144     7     PASS  (straddle falls at x>=~150, off the visible tail)
 _2        756          140     3     PASS
 _3        752          134     9     FAIL only x=143 y=0 (8px)
 _4        748          130     5     FAIL x=135..142 all lines (the 0xc0 straddle tile)
 _5        744          122     7     PASS
 _6        740          118     3     PASS
```
The TRUE displayed straddle boundary (where scx 97->192 switches the tile) is a near-CONSTANT
display column ~143 across ALL variants (measured: `_3` first-bad col 143, `_4` 135). It is
NOT at `self.x`. rustyboi's OLD bug: the fetcher reads scx_delayed LIVE at fetch; scx_delayed
flips at write_cc (delay 0), so every tile fetched after write_cc (cols 135+ for `_4`, fetched
AHEAD) uses NEW scx, but those cols PLOT before the boundary and Gambatte keeps OLD. i.e. the
failing cols are fetched AFTER the write but plot BEFORE the apply -> must stay OLD.

**Mechanisms tried (all behind RB_SUBCC, env-swept):**
1. *Frozen-xpos + draw-time re-derive* (BgPixel.plot_x/fetch_scx/bit_i, re-fetch in
   `draw_fifo_pixel` under live scx). BROKE broadly because the mode-3->HBlank **batch flush**
   (`while x<160 { draw_fifo_pixel }` at m0Time, controller.rs:~4000) drains all leftover FIFO
   at ONE abs_cc -> every flushed pixel sees the FINAL scx, not its own plot-time scx. Draw-time
   re-derive is fundamentally incompatible with the batch flush.
2. *on_scx_write delay = 2*cgb alone* (defer scx_delayed flip). BROKE `_3` 8px->1152px:
   the column fetcher reads the now-stale scx_delayed; the f1 first-tile path also keys off it.
3. *Deferred apply `write_cc + (2*cgb + LEAD)<<ds` + fetcher reads scx_delayed live + queued-
   tile rewrite gated by per-entry plot_cc*. SWEEP RB_LEAD: `LEAD(cc)=0`->fixes `_1 _5 _6`,
   still fails `_2 _3 _4`, regresses DS. The fetcher 2-dot cadence + warmup means `self.x` /
   `abs_cc` do NOT map linearly to the Gambatte plot cursor, so the "tile being fetched at
   apply_cc" splits across the 4-cc stride (phase-dependent). A single LEAD constant cannot
   align all 6 SS variants (best was 3/6, with DS collateral).

**KEY UNSOLVED PIECE (hand to next session):** the apply boundary is NOT `self.x`-relative
nor a fixed cc-offset from write_cc; it is the cc at which the **fetcher's xpos** (not the
display cursor) reaches the boundary tile. The fetcher leads the display by the FIFO depth AND
runs on a 2-dot cadence, so the mapping write_cc -> boundary-column needs the fetcher's own
xpos clock (an m3 per-line anchor: cc-when-fetcher-xpos==0, then `xpos = (cc - anchor)/2`
honoring warmup/sprite-stall dots). Establish that anchor, set `scx_apply_cc` = cc the fetcher
xpos reaches the write's column, and have the fetcher read NEW scx for fetches at/after it
(already-queued tiles need NO rewrite — they all plot before the boundary). The frozen
BgPixel fields + `bg_pixel_at_col` re-fetch helper are the right primitives but were applied at
the wrong clock. DO NOT use `self.x`/`abs_cc` directly as the plot cursor.

**Validated invariants:** flag-OFF == 73 at every step. The frozen-xpos approach (carry plot
geometry per FIFO entry) is sound; the +81 `restart_current_tile` trap was avoided (the
no-change cases were byte-exact when the boundary was correct). The blocker is purely the
fetcher-xpos-clock calibration, which is the sub-dot phase the cluster-root notes flag as
lever B-adjacent.

### Stage 1b (session 2026-06-28) — fetcher-xpos clock LANDED. Net -2, broke 0. Committed.

**RESULT: full suite RB_SUBCC=1 = 71 (55 CGB + 16 DMG), vs main_73. FIXED
`scx_0761c0/scx_during_m3_3` + `_4`. BROKE 0. Flag-OFF == 73 (identity). Aligned
tripwires (`scx_0060c0/0063c0/0360c0 _5/_6`) byte-exact. Debug build (overflow
checks) runs the straddle ROMs with no panic.**

The keystone WAS the fetcher-xpos clock, but the hypothesis direction in the Stage-1
hand-off was inverted: the current tree already fetches the `_4` straddle tile OLD
(col 29, scx 97). Gambatte renders it NEW. The fix flips that ONE in-flight tile to
NEW, it is not "keep OLD".

**Mechanism (all behind RB_SUBCC, controller.rs + fetcher.rs + fifo.rs):**
1. `on_scx_write` records the column lever for the line: `subcc_scx_old`/`_new` and
   `subcc_scx_apply_cc = write_cc + 2*cgb` (Gambatte scxChange `update(cc+2*cgb)`),
   persisting for the whole line (unlike `scx_apply_cc` which resets on apply). It
   ARMS `subcc_rekey_armed` iff a BG tile is mid-fetch at the write (fetcher NOT at
   TileNumber, used the OLD scx) — exactly one tile per write can straddle.
2. The fetcher (`fetcher.rs`) exposes `subcc_last_column_inputs() = (xpos, cgb_adj,
   used_scx)` — the exact inputs its TileNumber used, so the controller recomputes
   the column under NEW scx byte-identically.
3. At the next `PushToFifo` (write now known), if armed, compute the tile's plot cc
   = `abs_cc + (xpos - self.x)` and `gap = plot_cc - apply_cc`. The straddle flips
   to NEW iff **`gap == 4` EXACTLY** (then `overwrite_newest` the 8 just-pushed FIFO
   entries with the NEW-scx column). Disarm regardless.

**Why gap == 4 exactly (the unsolved calibration, now measured):** the in-flight
straddle tile's `plot_cc - apply_cc` is `2` (tile fully before the boundary -> OLD),
`4` (the straddle -> NEW), or `6` (next tile, already NEW via the fetcher's own
scx_delayed flip -> leave). Measured per-case ground truth:
```
case            tn_cc apply gap  want
0761c0/_4        745   750   4   NEW  <- fix
0060c0/_1        760   762   6   OLD
0060c0/_4        744   750   2   OLD
0360c0/_4        744   750   2   OLD
0761c0/_2        752   758   2   OLD
0761c0/_1        760   762   6   OLD
```
gap is NOT a threshold: `gap in (2,6)` (catches gap 3) BROKE 35 / fixed 0 — gap 3/5
land on aligned tile boundaries that must stay OLD. Only the exact phase `gap == 4`
is the straddle. This is the non-linear fetcher-xpos phase the prior notes flagged;
the eager-FIFO `xpos = display_x + fifo - pending` reproduces Gambatte's plot cursor
only at this one resonance, because rustyboi's FIFO depth (8 vs 9) and Gambatte's
cached fetch-ahead diverge off-resonance. PLOT_BASE also differs per line (619 vs 623
for the 4-cc-stride variants), so cross-variant cc comparison must be intra-line.

**What resists (deferred, NOT in scope for 1b):**
- `_3`/`_4` `_ds_2..5` (Stage 2): DS doubles the dot/cc scale; the gap==4 resonance
  becomes gap==8 (1 dot = 2 cc), needs `gap == (4<<ds)` + the f1 DS re-fetch, untested.
- `spx0/1/2` (CGB): these carry a SPRITE (OAM @ fe00); the first mismatch is a sprite
  color (`#21926C`), not a pure BG-column straddle. The gap is 5 (sprite stall shifts
  the cadence phase). The BG-only re-key does not address sprite mixing; out of scope.
- `spx0/1/2` (DMG): 8px @ x=8..15 y=0 — the f1 first-line fine-scroll prologue (same
  y=0-only class as the old `_3` residual), a separate f1 path, not the steady column.
- `scx_attrib_*_ds`: DS + attrib, same DS scale issue.

**Is scx the practical floor?** NO for the SS steady-state column straddle — gap==4
landed it broke-0. The remaining scx failures are (a) DS-scale (Stage 2, tractable:
scale the resonance by ds), (b) sprite-coupled spx CGB (a different lever: sprite
color/priority, not BG column), (c) f1 first-line DMG prologue (the existing f1 path).
The gap==4 exactness is the genuine integer-cc discriminator, validated broke-0 on
5257 tests; it is precise, not overfit (the destructive `gap in (2,6)` sweep proves
the boundary is a single phase).

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

### Stage 2 (session 2026-06-28) — DS straddle LANDED. Net -6, broke 0. Committed.

**RESULT: full suite RB_SUBCC=1 = 67 (vs Stage-1b 71, vs main_73). FIXED
`scx_during_m3_ds_2/3/4/5` (CGB). BROKE 0. Flag-OFF (var UNSET) == 73 (identity).
SS tripwires (`scx_during_m3_3/_4` + the aligned `scx_0060c0/0063c0/0360c0/0363c0/
0367c0` SS+DS sets) all byte-exact. DEBUG build (overflow checks) runs ds_2..5 with
no panic.**

The DS failures were TWO distinct bugs, both at x=0..6 then x=143..150:

1. **The f1 fine-scroll first-tile re-fetch was gated OFF for DS** (controller.rs
   `if cgb && !double_speed && brk_col != arm_col`). At DS the `07->61` first SCX
   write crosses the f1 discard (xpos=9, arm_col=0 -> brk_col=12), so the stale
   first tile must be re-fetched exactly as SS does. Gate flipped to
   `(!double_speed || subcc_enabled())`; the existing `delta<<ds` mode0/m0Time nudge
   already carried the DS scale. This fixed the x=0..6 prologue (the dominant DS
   failure region) on all four variants.

2. **The mid-line column straddle resonance.** With the prologue fixed the failure
   moved to the steady straddle (x=143..150). The Stage-1b `gap==4` (where
   `gap = plot_cc - apply_cc`, `plot_cc = abs_cc + (xpos - x)`) does NOT generalize:
   - `plot_cc`'s dot delta must be scaled to master cc: `+ ((xpos - x) << ds)`.
   - The gap proxy is AMBIGUOUS across initial-scx at DS. Measured ground truth: the
     straddle tile that must flip to NEW is `ds_3/4/5` (xpos=143) in BOTH `scx_0761c0`
     (gap 4/8/12, fifo=11) and `scx_0360c0` (gap 2, fifo=10) — gap and FIFO-depth/span
     proxies disagree between the two dirs. The INVARIANT that separates flip from
     no-flip across every initial-scx is the **fetcher-step phase of the apply cc
     relative to the armed tile's TileNumber latch**:
       `(apply_cc - tn_cc) % (2<<ds) == (1<<ds)`
     The BG fetcher steps every 2 dots == `(2<<ds)` cc; the straddle is exactly when
     the write's apply lands HALF a step (`1<<ds`) into the armed tile's fetch cycle.
     At DS this is `(apply-tn)%4==2`; verified flip-set {0761:ds_3/4/5, 0360:ds_3}
     vs no-flip {0761:ds_1/2/6, 0360:ds_1/2/4/5/6} byte-exact.
   - Required a new field `subcc_last_tn_cc` (abs_cc recorded at each BG TileNumber).
   - **SS is NOT switched to the mod predicate** — it regresses the DMG SS scx set
     (DMG fetcher cadence differs; the unified mod gave net +32/broke 8). SS keeps the
     validated `gap==4`; the DS branch alone carries the mod phase. SS `plot_cc` now
     uses `<<ds` which is a no-op at `ds==0` (byte-identical to Stage 1b).

**What resists (deferred, NOT BG-column straddle — separate levers):**
- `scx_during_m3_spx2_ds`, `scx_attrib_during_m3_spx1_ds/spx2_ds` (CGB): carry a
  SPRITE; the mismatch is sprite mixing, not the BG column. The spx lever (sprite
  color/priority + DMG f1 prologue) is Stage-2-adjacent but explicitly out of scope.
- The `hdma_*_scx1*` cases are the HDMA event-cc family (PERACCESS FACET 3), not scx.

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

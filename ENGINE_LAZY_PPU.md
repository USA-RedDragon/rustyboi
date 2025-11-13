# Engine rewrite (A): lazy / per-access closed-form PPU timing

Branch: `engine-lazy-ppu` (off `core-loop` @ 5f11996, baseline 562).
Goal: make every CPU-visible PPU timing fact resolve at the **exact access cc** from **one
closed-form anchor**, matching Gambatte. This dissolves the entire mode-boundary "straddle"
failure class (DS *and* SS, reads *and* writes) — the documented convergent root that has
defeated 4 constant-tuning attempts (see memory `cl-m2int-ds-gap10-retriangulation`).

## Why the current model can't be correct

rustyboi is a **hybrid**: it eagerly steps the PPU per dot (`controller.step` from `tick_t`,
poking the FF41 mode bits via `set_lcd_status_mode` at dot boundaries) and then *refines* only
the mode3→0 read via a closed-form `m0_time_master`. To reconcile the eager dot grid with the
closed-form value, ~a dozen swept fudge offsets exist (`CC_OFF=5`, `STAT_READ_CC_OFF=6`,
`K=6/7`, `KD=-1`, `cgb/dmg_mode0_offset=4`, `M0IRQ_OFFSET=-3`, `ACCESS_CC_RELOC=4`,
`dma_scx_m0_nudge`, `RB_CGBP_DS_END`, …). Each is right on average and wrong by 1-4 cc at a
boundary — and they are **decoupled** (the FF41 read, the VRAM/OAM block, the cgbpal window,
the rendering transition, and the m0 IRQ each use *different* offsets), so any single change is
a 1-for-1 swap. Gambatte derives all of them from ONE `m0Time`.

## Target architecture

Split the PPU into two concerns:

1. **Timing state — pure closed-form from one anchor, resolved at the access cc.** No eager
   mode-register pokes, no swept offsets. Everything below is a function of `cc` (the master
   clock `abs_cc`/`master_cc`) relative to the LCD-enable anchor `p_now`, exactly as Gambatte's
   `lcd_.update`-free predictors do:
   - `lineStart(cc)` / `lineCycles(cc)` — Gambatte `LyCounter`. (rustyboi already has
     `ly_counter` + `internal_ly` closed-form.)
   - `m0Time = lineStart_cc + (predictCyclesUntilXpos(167) << ds)` — the SINGLE boundary.
     `compute_m3_length_win` already computes `predictCyclesUntilXpos(167)` in dots; the anchor
     and `<<ds` must be Gambatte-exact (no K/KD).
   - **getStat(cc):** mode = 2 while `lineCycles < 80`, then 3 until `cc + 2 >= m0Time`, then 0;
     mode 1 in VBlank. (Gambatte `getStat`.) The FF41 read computes this; the stored register
     holds only the enable bits + LYC flag.
   - **LY(cc):** from `LyCounter` (with the line-153 early-zero rule already present).
   - **Accessibility:** VRAM blocked in mode 3; OAM in mode 2/3; cgbpal `lineCycles+ds>=80 &&
     cc < m0Time+2` — all from the SAME `m0Time`, Gambatte's exact thresholds.
   - **IRQs:** already scheduled-event based (`dispatch_stat_events`) — keep, but re-anchor any
     event time that currently carries a swept offset onto the exact `m0Time`/`lineCycles`.

2. **Pixel production — batched, image-only, does NOT affect CPU timing.** The framebuffer line
   is rendered from the same closed-form line geometry (scx discard, window start, sprite list,
   m3 length). It can stay eager or be computed per-line; pixels are only read at frame end, so
   their *timing* is irrelevant — only their final values must be correct.

## The true access cc (first thing to nail empirically)

Gambatte resolves a read at `cc` then does `cc += 4`. rustyboi snapshots `abs_cc` before
`tick_m`; CL1 uses `abs_cc + 1` ("honest start-of-access cc"). The exact relationship
(including the DS half-dot phase — `abs_cc` advances 1/T-cycle = half a PPU dot at DS) must be
pinned with the cctracer oracle before LP1, by dumping Gambatte's `cc` at a known read and its
`m0Time`/`lineStart` for that line. Extend `cctracer` to expose Gambatte's internal `m0Time`
(`NextM0Time::predictedNextM0Time_`) and `lineCycles`.

## LP0 RESULTS (calibrated via the extended cctracer oracle)

Ground truth (cctracer now dumps Gambatte `m0Time`, `lineCycles`, `lyTime` at every FF41 read):
- **The boundary rule is `cc + 2 < m0Time` → mode3** (constant `+2`), confirmed identical across
  `m2int_m3stat_ds_2` (mode0), `ds_1` (mode3), and the inline `dma/gdma_cycles_short_ds_1`. NOT
  rustyboi's `access_cc + 6`.
- **The read cc `cc` == rustyboi's RAW `abs_cc`** (the master cc before the access M-cycle ticks),
  NOT the CL1 `abs_cc + 1`. (m2int_ds_2: Gambatte cc=198532 ↔ rustyboi abs_cc=149604, offset 48928.)
- **`m0Time = lyTime − ((456 − m0_lineCycle) << ds)`**, where `lyTime` is the next-LY cc (rustyboi
  has this exactly from `ly_counter`) and `m0_lineCycle` is the mode-0-start lineCycle from the
  pixel predictor. For scx0/cgb/no-sprite/no-window: `m0_lineCycle = 251` (m0Time = lyTime − 410 at ds=1).
- rustyboi's current `m0_time_master` is ~3cc high at DS purely from the `K(6)/KD(-1)` arm-anchor
  fudge; anchoring on `lyTime` with the exact `m0_lineCycle` and comparing `abs_cc + 2 < m0Time`
  removes ALL of `CC_OFF`/`+1`/`+6`/`K`/`KD` at once for the read path.

LP1 task: get `m0_lineCycle` exact across scx / sprite / window configs (build the calibration
table from cctracer; it should equal `(80-ish base) + compute_m3_length_win(...)` — verify the
base/anchor) and switch getStat to the lyTime-anchored `abs_cc + 2 < m0Time` form.

### LP1 calibration table (cctracer, CGB DS, handler read pc 0x107x)
m0_lineCycle vs scx (no sprite, no window): scx0=251, scx1=252, scx2=253, scx3=254, scx4=255,
scx5=256, scx6=257, scx7=258 → **linear `m0_lineCycle = 251 + scx`**. Window (wx03/wx07) = 257
= 251 + 6 (= `WIN_M3_PENALTY`). Since CGB `compute_m3_length_win = scx + 167 (+6 win + sprites)`,
this means **`m0_lineCycle = compute_m3_length_win + 84`** (84 = 80 mode-2 + 4). The predictor
DELTAS (scx +1, window +6, sprites) are ALREADY correct in `compute_m3_length_win`; only the
base anchor (84) and the formula were wrong. So:
  **`m0Time = lyTime − ((456 − 84 − m3_len) << ds)` = `lyTime − ((372 − m3_len) << ds)` (CGB)**,
  where `lyTime` = the next-LY cc (rustyboi `ly_counter().time()`), and getStat: **mode3 iff
  `abs_cc + 2 < m0Time`** (raw abs_cc, no +1/+6). DMG/single-speed base: calibrate with cctracer
  on `.gb` roms (the `(1-cgb)` term in m3_len implies the DMG base is 83, i.e. m0_lineCycle =
  m3_len + 83 — VERIFY). All OTHER `m0_time_master` consumers (cpu_access_blocked VRAM/OAM/cgbp,
  m0irq arm, cgbp_block) must switch to this same exact `m0Time` with Gambatte's own per-consumer
  constants (VRAM/OAM unblock `cc + 2 >= m0Time`; cgbp accessible `cc >= m0Time + 2`), removing
  `K`/`KD`/`ACCESS_CC_RELOC`/`*_mode0_offset`/`dma_scx_m0_nudge`/`RB_CGBP_DS_END`.

## LP1 RESULTS — calibration byte-exact; LP1 must merge with LP2 (atomic whole-grid move)

LP1-in-isolation = net 0 (reverted): every variant fixed EXACTLY the 562 baseline failures and broke
a **disjoint** ~755 (verified `overlap=0`). Diagnostic proof that the exact `cc+2 < m0Time` boundary is
*correct*, but the rest of the PPU sits ~8cc off it, so moving only the read desyncs the grid.

**Byte-exact calibration (verified vs cctracer, ready to use):**
- `m0Time = (p_now + ly_counter().time + 1) − ((456 − (m3_len + BASE)) << ds)`
  - `BASE = 84` (CGB, SS *and* DS); `BASE = 83` (DMG — the `(1−cgb)` term already lives in `m3_len`).
  - Two rustyboi anchor corrections vs raw Gambatte: (1) `ly_counter().time` is PPU-relative → add
    `p_now`; (2) rustyboi's `LyCounter.time` runs exactly **1 master-cc below** Gambatte's `lyTime` → `+1`.
  - With both, byte-identical to cctracer's `m0Time` (boot offsets cancel).
- getStat: **mode3 iff `abs_cc + 2 < m0Time`** (raw `master_cc()`, NOT `ppu_access_cc()`/`+6`).

**Architecture finding — there are TWO mode-0 notions that must be unified:**
1. *Emergent*: the pixel pipeline reaching `x == 160` (controller.rs ~2092) sets `State::HBlank` and
   pokes the eager FF41 mode register (`set_lcd_status_mode(mmio,0)`). ~755 currently-passing tests ride
   this grid.
2. *Closed-form*: `m0_time_master`/`scheduled_mode0_dot` — feeds the FF41 read refinement, m0 IRQ arm,
   VRAM/OAM/cgbp access blocking. The 562 failing straddle tests ride this.
The swept offsets exist only to reconcile (1) and (2). **They are ~8cc apart at DS.**

**THE ATOMIC CHANGE (do LP1+LP2 — and effectively LP3 — together):** make **mode 3 end at the exact
`m0Time`, period.** Drive the `PixelTransfer→HBlank` transition off `abs_cc >= m0Time` (exact), set the
eager mode register there, anchor the m0 IRQ + VRAM/OAM/cgbp blocking on the same `m0Time`, and let the
pixel pipeline become image-only — flush remaining FIFO pixels to 160 at the transition rather than
letting `x==160` drive timing. Then ALL the swept offsets (`CC_OFF`/`STAT_READ_CC_OFF`/`K`/`KD`/
`*_mode0_offset`/`ACCESS_CC_RELOC`/`dma_scx_m0_nudge`/`RB_CGBP_DS_END`/`reported_mode0_early_nudge`)
collapse to Gambatte's constants (read `+2`, vram/oam unblock `cc+2>=m0Time`, cgbp `cc>=m0Time+2`).
Expect this single atomic move to flip the 562 to pass while keeping the ~755 (they were passing on an
internally-consistent-but-offset grid; the exact grid is also internally consistent AND matches Gambatte).

**Flagged anomaly to investigate first:** in the isolated-field experiment, merely *assigning* a value to
a new `m0_time_exact` field read ONLY by `get_stat` flipped `sprites/space/10spritesPrLine_*_m3stat_ds_1`
(a rom that never reads FF41) from pass→fail — reproduced with a constant 12345 and via `RB_NO_EXACT`.
Suggests a PPU state coupling/aliasing not yet mapped (possibly `compute_m3_length` side-effects, or the
assignment site perturbing control flow). Trace this before trusting any partial approach.

## Phasing (each on this branch; red allowed if attributed; merge only net-positive)

- **LP0 — anchor calibration (read-only + cctracer):** extend cctracer to dump Gambatte
  `m0Time`, `lineStart`, and `cc` at each FF41 read. Produce the exact formula:
  `m0Time = lineStart_cc + (m3_len << ds)` and the access-cc relationship `cc = abs_cc + ?`.
- **LP1 — exact closed-form read resolution.** Replace `get_stat_mode3to0_at_cc` with a full
  `getStat(cc)` (all modes) and route VRAM/OAM/cgbpal accessibility + the FF41/LY reads through
  the single exact `m0Time` and `lineCycles`, using the LP0 access cc with NO swept offsets.
  Keep the eager renderer + its mode pokes as a *fallback only* for now. Expect the straddle
  clusters (m2int, oam_access, vram_m3, dma, speedchange `_ds`/`_1`/`_2`) to converge. Validate.
- **LP2 — remove eager mode register + swept offsets.** Once reads are exact, delete
  `set_lcd_status_mode` pokes for the mode bits (compute on read), and remove the now-zero
  `CC_OFF`/`STAT_READ_CC_OFF`/`K`/`KD`/`mode0_offset`/`ACCESS_CC_RELOC`/nudges. Re-anchor the
  scheduled IRQ times onto the exact `m0Time`/`lineCycles`.
- **LP3 — decouple pixel rendering + delete dead code.** Clean separation of the batched
  framebuffer renderer from timing; remove the reconciliation scaffolding.

## Validation discipline

cctracer is the per-line oracle (exact boundary cc). After each phase run the FULL suite
(`--json`), diff with `/tmp/diff_runs.py`, keep only net-positive, attribute every red to an
unfinished phase. Single-speed clusters and the non-PPU clusters must converge back. Mergeable
to `main` only when net-positive with zero unexplained regressions.

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

## LP-ATOMIC RESULTS (branch `lp-atomic`, all 5 wiring steps done together)

Implemented the full atomic move: `m0_time_master` = exact lyTime-anchored m0Time;
`scheduled_mode0_dot` DERIVED from it (`arm_ticks + (m0t-arm_cc)>>ds`); getStat
(`master_cc+2 < m0Time`), the `PixelTransfer->HBlank` transition (`master_cc >= m0Time`, flush
FIFO to 160, x==160 only a no-m0Time fallback), VRAM/OAM/cgbp blocking, and the m0 IRQ arm all
driven off the ONE boundary. Removed `K`/`KD`/`STAT_READ_CC_OFF`/`ACCESS_CC_RELOC`/
`RB_CGBP_DS_END`/`dma_scx_m0_nudge` from these paths. bus passes raw `master_cc()`.

**Net = 828 vs 562 (−266; fixed 69 / broke 335).** NOT mergeable. The formula is **byte-exact vs
cctracer wherever the LCD-enable anchor `p_now` is correct** (verified: non-sprite scx0..7 ds_1
mode3 + ds_2 mode0 are byte-identical m0Time AND lyTime). The intended straddle targets converge
(m2int_m3stat 17→13; cgbpal/dma/m2int_m0irq partially). But the move EXPOSES two pre-existing,
**anchor** bugs that the swept fudges were silently absorbing — both are `p_now`/abs_cc
miscalibrations, NOT m0Time-formula errors, and both are unreachable from the PPU closed form:

1. **Sprite-heavy roms: `p_now` is 4cc (2 dots DS) too high → abs_cc/lyTime/m0Time 4cc LOW.**
   `sprites/space` (112 were passing, all broke) + `sprites` (net −40). cctracer proof on
   `10spritesPrLine_nr10space10_m3stat_ds_1` (out3=mode3): Gambatte cc=216984 m0Time=216990
   (mode3); rustyboi master_cc=149820 m0t=149822 (mode0, WRONG by reading 1 step late). CPU is
   perfectly synced — DISPATCH 0x48 and the FF41 read share offset 67164 — but offset-correcting
   Gambatte's lyTime (217184→150020) vs rustyboi's (150016) shows the **PPU trails the CPU 4cc on
   these roms only**. rustyboi `line_cycle` IS locked to abs_cc (constant +45 phase), so the drift
   is in `p_now` set at this rom's enable/LY-write/speedchange sequence, not the line counter.
   m3_len matches Gambatte exactly (275 = m0_lineCycle 359 − BASE 84) — sprite cycles are right.
2. **Post-speedchange lines: m0Time 1cc too HIGH** (the lyTime `+1` over-corrects after a
   `speed_change` p_now rebase). `speedchange` net −34, `dma/hdma_late_m3speedchange` etc.
   cctracer proof on `speedchange2_frame1_m2int_m3stat_scx2_2` (out0=mode0): Gambatte cc=417748
   m0Time=417750 (mode0); rustyboi 354976 m0t=354979, off +1 high → reads mode3. The identical
   non-speedchange `m2int_scx2_m3stat_ds_2` is byte-exact (rustyboi m0t 149610 == Gambatte 198538
   offset-corrected). So the `+1` is right at steady state, wrong right after a speed change.

**Anomaly resolved:** the flagged "assigning a get_stat-only field flips a sprite render test" is
the SAME root — those sprite render tests were riding the eager grid whose m0-boundary the
swept K=6 fudge put 5cc above the exact value; the exact m0Time is 4cc below where the drifted
`p_now` needs it, so any consumer reading the exact value desyncs the sprite line. It is a control
coupling only through `p_now`, not a hidden side effect of `compute_m3_length` (which is pure).

**Next levers (both are CPU/anchor-engine, outside this closed form):**
- Fix the `p_now` enable/speedchange rebase so abs_cc tracks Gambatte's `p_.now()` on the
  sprite + post-speedchange roms (the 4cc and the +1). Bisect the enable / LY-write / `speed_change`
  / `stop_bridge_advance` p_now math against cctracer's lineStart on one sprite rom. Once `p_now`
  is exact the formula is byte-exact everywhere (proven on the non-drifted clusters).
- The branch keeps the exact grid; it is the correct target. It is net-negative ONLY because of
  these two anchor drifts. Do NOT re-introduce K/KD to mask them — fix `p_now`.

## PNOW-FIX RESULTS (branch `pnow-fix` off `lp-atomic`, HEAD d773dab)

**828 -> 816 (-12, net +12).** Bug #2 FIXED; bug #1 root narrowed but not fixed.

**Bug #2 FIXED** (`ppu: drop lyTime +1 correction after DS->SS speed switch`):
- Root: the DS->SS stop bridge (`opcodes::stop`, `bridge=3` dots, each subtracts `1<<ds=2` from
  `p_now`) lands the LyCounter exactly 1 master-cc HIGH vs Gambatte (the DS half-dot the whole-dot
  bridge can't express). So the `+1` LyCounter correction in `m0_time_exact` over-corrects by 1
  right after a DS->SS switch.
- Fix: added `lytime_no_plus1` flag — set by `set_dsss_lytime_adjust()` on the DS->SS switch
  (`!to_double`), cleared at the next LCD enable. `m0_time_exact` drops the `+1` while set.
- Verified byte-exact: `speedchange2_frame1_m2int_m3stat_scx2_2` now m0t=354978==Gambatte
  (offset-corrected), passes. Confirmed via dispatch(354744<->417516 off 62772) + inline-INSTR sync.
- Did NOT touch the bridge dot counts (3/8) — those also drive line_cycle/IRQ scheduling.

**Bug #1 (sprite 4cc) — DEEPER than "p_now seed/rebase"; it is a per-rom PPU-vs-CPU phase, NOT
reachable from any rustyboi PPU state.** Proven via dual-rom byte comparison + inline-INSTR offset:
- Sprite rom (`10spritesPrLine_nr10space10_m3stat_ds_1`, LYC-driven, OBJ on) and the PASSING twin
  (`m2int_scx2_m3stat_ds_2`, m2-driven, OBJ off) have **byte-identical rustyboi PPU state** at the
  read: same `p_now=7832`, same `abs_cc`, same enable seq (enable@mc7841 + 8-dot SS->DS bridge,
  NO LY-write, NO re-enable), same lineStart_master=149104, same ly_time_full=150016.
- CPU is PROVABLY synced on BOTH (offset confirmed by the 0x48 dispatch AND two inline INSTRs at
  pc 0x1074/0x1075 — all give sprite offset 67164; read cc 149820<->216984 exact).
- Yet Gambatte's lineStart (offset-corrected) is 149108 on the sprite rom (rustyboi 4 LOW) but
  149104 on the twin (rustyboi EXACT). i.e. Gambatte's LY-counter sits 4cc later vs the (synced)
  CPU on the sprite rom only. Identical for 1-sprite and 10-sprite roms (NOT sprite-count-scaled;
  m3_len=275 matches Gambatte). lineCycle at read: rustyboi 358 vs cctracer 356 (2 dots high).
- The LyCounter is sprite-INDEPENDENT in rustyboi, and the enable phase is identical to the twin,
  so this 4cc CANNOT come from any closed-form input. It is a genuine 4cc divergence in how
  *Gambatte* advances its PPU `now` relative to the CPU between the (identical) boot enable and the
  read — correlated with OBJ-enabled / LYC-driven roms but not caused by sprite cycles.
- **A sprite-count or OBJ-gated lineStart/m0Time nudge would be exactly the per-cluster fudge the
  doc forbids (it'd be coincidental, not the real phase).** Resolving it needs Gambatte's enable
  `p_.now()` (or a per-line `now`) exposed by cctracer — NOT modifiable per instructions. Likely a
  CPU/PPU cc-accounting difference accumulated over the sprite rom's longer pre-read path, or a
  one-time enable-with-OBJ warmup offset Gambatte applies that rustyboi does not. Bisect with a
  cctracer that dumps `now`/lineStart at LCD-enable and at each LY increment on ONE sprite rom.

## BUG#1 RE-DIAGNOSIS (corrected — it is NOT p_now / PPU phase)

Hands-on offset-free tracing (line_cycle = within-line dot 0..455, directly comparable to Gambatte
`lineCycles`, NO boot-offset needed) overturns the prior "sprite p_now 4cc" framing:

- At the decisive read, ALL three roms read at LY=1. Offset-free line_cycle vs Gambatte lineCycles:
  - `m2int_m3stat_ds_2` (twin, PASS): rustyboi 250 == Gambatte 250 ✓
  - `m2int_scx2_m3stat_ds_2` (PASS): rustyboi 252 == Gambatte 252 ✓
  - `10spritesPrLine_nr10space10_m3stat_ds_1` (FAIL): rustyboi 358 vs Gambatte 356 — **+2 dots**
- The m2/LYC DISPATCH is at rustyboi line_cycle ~10 vs Gambatte 8 (+2 dots); the handler timing
  (dispatch→read) MATCHES. So the PPU phase / p_now / m0Time formula are all FINE — the **STAT
  interrupt dispatches 2 dots late**, carrying the whole handler (and its read) 2 dots late.
- **Discriminator:** the failing rom writes `STAT=0x40` (bit6 = **LYC=LY** IRQ enable) and `LYC=1`,
  so the LYC=LY interrupt fires at LY=1. The passing twin/scx2 use `STAT=0x20` (mode-2 IRQ, via
  `ldff(c)`), no LYC. So bug#1 is the **LYC=LY STAT interrupt dispatching ~2 dots late at DS**, NOT
  sprites and NOT a PPU anchor. (The agent's "OBJ-correlated 4cc p_now" was a cross-LY artifact.)
- `lyc_schedule` (`lyc*456 − 2`, `+6` for lyc0) MATCHES Gambatte `lyc_irq.cpp::schedule` exactly. So
  the 2 dots are in the FLAG/DISPATCH path, not the schedule — prime suspect: `dispatch_stat_events`
  fires `sched_lycirq <= abs_cc` on the even render dot, rounding an odd-cc LYC time up to the next
  render dot at DS (the `step_subdot` sub-dot handling covers m2/m0 but may miss LYC). Verify the LYC
  fire cc vs Gambatte (add a LYC hook to cctracer like the m2 one) and route LYC through the sub-dot
  dispatch. This is a small, localized `stat_irq`/`dispatch_stat_events` fix, not an anchor rewrite.

### Net status of the rewrite (honest)
`lp-atomic`/`pnow-fix` = 816 (vs 562 core-loop). m0Time formula byte-exact; bug#2 fixed (+12);
bug#1 (LYC dispatch) ~157 tests pending. The lp-atomic disjoint break was 335 broke / 69 fixed, so
beyond bug#1+#2 there remain ~150+ other breakages (each a similar pre-existing timing layer the
fudges masked). Reaching net-positive vs 562 is a multi-fix convergence, not one change. main/core-loop
remain clean at 562; nothing net-negative merges.

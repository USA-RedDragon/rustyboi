# ENDGAME — per-access-cc exactness for the final 15

Long-lived branch `endgame-cc`. Baseline: `b677051` == `main_31` (31 suite failures).
NEVER touch `/home/reddragon/projects/rustyboi` (main). NEVER push. NEVER switch branches.
Commit incrementally. Flag-OFF MUST stay byte-identical to `main_31` at EVERY commit.

## 0. STATE OF THE WORLD (read this first — it re-scopes the original brief)

The original brief (and `~/.claude/plans/sleepy-riding-bentley.md`) described milestone-1 as
"move interrupt DISPATCH onto the scheduled event cc." **That mechanism is already landed and
permanently on.** In `cpu/bus.rs`:

```rust
pub(crate) fn faithful_enabled() -> bool { true }   // "ds-engine STAGE 7: permanently on"
```

With it on, `cpu/sm83.rs::step` already runs:
- the **faithful prefetch model** (fetch opcode at the boundary cc, execute-no-refetch, service
  rewinds pc),
- **event-cc interrupt dispatch**: a timer IRQ is serviceable only once the boundary access cc
  (`mmio.access_cc()` == `master_cc()`) has reached the recorded `pending_timer_fire_cc()` — not
  merely once its IF bit is set,
- **EI-loop fast delivery** (`RB_EI_FAST`, on by default): an imminent overflow is delivered at the
  EARLY anchor `schedCc + IF_OFF` so the ISR runs on Gambatte's exact divider phase.

So the peripheral-first → CPU-first pivot is **done**. The CPU already samples IF / dispatches on a
per-access cc. The remaining 15 are NOT "dispatch is at the wrong granularity"; they are
**residual 1-cc errors in the per-access cc value itself** at specific boundaries (STOP-window
parity, unhalt cc, SS→DS bridge mode-3 length, sub-master write-vs-latch order). Every prior agent
converged on this: any scoped constant swaps a passing sibling because the constant is right for the
average access but wrong by ±1cc for the bracket case.

The access-cc anchors in play (`memory/mmio.rs`):
- `master_cc()` = `timer.abs_cc()` — the raw master clock; the dispatch/event-cc gate compares here.
- `access_cc()` = `timer.access_cc()` — the canonical register-access cc (timer/APU/serial share it).
- `ppu_access_cc()` = `abs_cc + 1` — the honest per-access cc the PPU read-gating anchors on.

These three differ by fixed offsets that are correct on average but carry the per-bracket ±1cc the 15
need. The endgame is to make the dispatch/access cc **byte-exact per access** at the four boundary
families below, not to add a fourth tuned offset.

## 1. THE 15 (current `main_31`, cgb unless noted) — grouped by ROOT

### R1 — STOP-window CPU instruction-boundary parity (per-access CPU cc over 4 STOPs)
- `offset2_lyc98int_ly_count_2`            (out9A)  sibling `_1`=out99 PASSES
- `offset2_lyc99int_m0stat_count_scx2_1`   (out90)
Root: each STOP advances cc by the `0x20000 + 4` unhalt window arithmetic (`opcodes.rs::stop`,
`stop_unhalt_cycles = 0x20000`, returns 8). Over 4 back-to-back STOPs a ~1cc parity accumulates so
the ISR LY-counter samples one LY off. The window is measured from the post-opcode cc; the residual
is the difference between rustyboi's "opcode-fetch tick folded into the 8" model and Gambatte's
`cc = mem_.stop(cc()-4)` measured exactly at the pre-operand cc.

### R2 — m2/unhalt service per-access cc + TIMA-overflow phase (HDMA bracket swaps)
- `hdma_late_ei_m3halt_m2unhalt_pc_scx1_2`              (outAD)
- `hdma_late_m3speedchange_tima_scx1_ds_3`             (outF6)
- `hdma_late_m3speedchange_tima_scx1_ds_6`             (outF9)
- `hdma_m0speedchange_late_m3wakeup_scx1_2`            (out00)
- `hdma_m0speedchange_late_m3wakeup_scx2_2`            (out00)
- `hdma_transition_ei_halt_late_unhalt_ldaaimm_hdma_scx1_1` (out00)
- `hdma_transition_ei_halt_late_unhalt_ldaaimm_hdma_scx1_2` (out02)
- `hdma_transition_halt_late_unhalt_ldaaimm_hdma_scx1_1`    (out00)
Root: a 1-byte `.text` offset crosses a TIMA tick boundary in the unhalt / m2-service path.
rustyboi computes the SAME wakeup cc for both sides of the bracket (memory: `hdma-unhalt-bracket-
floor`). Needs the true per-access cc at the unhalt resume + TIMA-overflow phase, not a constant.
This is the biggest cluster (8) and the most coupled (STOP + HDMA period + EI-fast + TIMA all meet
here) — attack LAST.

### R3 — SS→DS bridge mode-3-length phase
- `oamdma_late_speedchange_stat_2`         (out3)
Root: across the speed switch rustyboi's `m0Time` shifts −4 where Gambatte shifts +18
(memory: `uniform-faithful-bridge`, `m3len-is-cpu-phase-not-renderer`). The bridge step()s carry a
~1-dot/pair phase error in the mode-3 WINDOW LENGTH; no uniform constant fixes off2 vs scx together.

### R4 — sub-master-cc CPU-write-vs-PPU-latch order (dmg)
- `late_m0int_halt_m0stat_scx3_2b`   (out2)  dmg
- `late_m0irq_halt_m0stat_scx3_2b`   (out2)  dmg
- `wxA6_weoff_at_xposA6`              (—)     dmg  (mode-2 STAT-ISR instr-stream pos lands WE-off ~86px early)
Root: a CPU register write and a PPU latch occur on the same master cc; rustyboi orders them at the
M-cycle boundary, Gambatte at the exact sub-access cc.

### R5 — per-pixel LCDC history during mode 3
- `bgoff_bgon_sprite_below_window`    (24 px diff)
Root: needs per-pixel LCDC.0 (and sprite-enable) history sampled at the render cc during mode 3,
not the end-of-line LCDC snapshot. This is a renderer-cc job, orthogonal to the CPU-cc work;
schedule independently.

### NOT in scope (16 harness/oracle artifacts — do NOT chase)
`fexx_ffxx_dumper`, `fexx_read_reset_set_dumper`(dmg+cgb), `vram_dumper`,
`oamdma_src80_oambusy_dumper_1`(dmg+cgb), and all 10 `oamdmasrc*…dumper*`.
Per memory `oamdma-dumpers-are-harness-floor`: a runner capture-path mismatch (sub-frame ref vs
64-frame budget); cctracer's own Gambatte ALSO mismatches the refs. Do not reference-fit.

## 2. STAGING ORDER (cheapest/most-isolated first; coupled cluster last)

1. **R1 STOP-window parity** (this session's milestone-1) — most isolated; one well-understood
   arithmetic boundary; oracle is cctracer FF44-read cc on `offset2_*`. 2 cases.
2. **R4 dmg write-vs-latch order** — small, dmg-only, no speed-switch coupling. 3 cases.
3. **R3 SS→DS bridge m3-length** — 1 case but its phase derivation also informs R2's DS subcases.
4. **R2 HDMA unhalt/TIMA bracket** — the coupled 8; do once R1/R3 settle the STOP & bridge cc.
5. **R5 per-pixel LCDC** — renderer-cc, parallelizable; do anytime.

Each stage: implement behind `RB_CANONICAL_CC` (a real knob during dev). flag-OFF==31 at every
commit. flag-ON: measure full-suite delta + which of the 15 moved + cctracer proof. Inline & remove
the flag (make it the path) only when a stage is net-positive AND flag-ON breaks nothing else; that
flip is a SEPARATE commit, never in this de-risk session.

## 3. VALIDATION GATE (every commit, non-negotiable)
```
bash /tmp/loadgate.sh
cargo build --release -j4 -p rustyboi-test-runner          # from /home/reddragon/rb-endgame
# flag OFF — MUST be net 0 / broke 0 vs main_31:
./target/release/rustyboi-test-runner --suite \
  /home/reddragon/projects/rustyboi/gambatte-core/test/hwtests --json /tmp/eg_off.json
python3 /home/reddragon/projects/rustyboi/.baselines/diffn.py \
  /home/reddragon/projects/rustyboi/.baselines/main_31.json /tmp/eg_off.json
# flag ON — measure delta (a regression valley ON is acceptable):
RB_CANONICAL_CC=1 ./target/release/rustyboi-test-runner --suite \
  /home/reddragon/projects/rustyboi/gambatte-core/test/hwtests --json /tmp/eg_on.json
python3 /home/reddragon/projects/rustyboi/.baselines/diffn.py \
  /home/reddragon/projects/rustyboi/.baselines/main_31.json /tmp/eg_on.json
```
Per-case cc proof (Gambatte oracle):
```
/home/reddragon/projects/rustyboi/gambatte-core/.claude/worktrees/agent-aaf6c4485f827ed55/test/cctracer \
  <rom.gbc> [watchpc_hex ...]
```
Keep overflow guards (suite=release); smoke a debug build after cc-math changes.

## 4. KEY FILES
- `cpu/sm83.rs` — `step` (dispatch/prefetch/event-cc gate), `service_interrupt`, halt/unhalt.
- `cpu/bus.rs` — `faithful_enabled`, `access_cc`, `pending_timer_fire_cc*`, `run_to`/`tick_m`.
- `cpu/opcodes.rs` — `stop()` SS↔DS bridge + `stop_unhalt_cycles` window arithmetic (R1, R3).
- `ppu/controller.rs` — `m0_time_master_cc`, `sched_*irq`, render-cc (R3, R5).
- `timer.rs` — `abs_cc`, `access_cc`, `pending_fire_cc*`, `next_overflow_*` (R1 parity, R2 TIMA).
- `memory/mmio.rs` — `master_cc`/`access_cc`/`ppu_access_cc` anchors.
- Oracle: `gambatte-core/libgambatte/src/{cpu,memory,tima,video,interruptrequester}.cpp`.

## 5. PROGRESS LOG

### (init) Plan written. Worktree confirmed flag-OFF == main_31 (31, byte-identical).

### Milestone-1 (R1 STOP-window parity) — HYPOTHESIS REFUTED, mechanism proven inert
Wired `RB_CANONICAL_CC` (+ `RB_CANONICAL_CC_ADJ` signed sweep) in `cpu/bus.rs`;
applied ADJ to `cpu/opcodes.rs::stop`'s `stop_unhalt_cycles` (the post-STOP unhalt
window). flag-OFF and flag-ON(ADJ=0) are BOTH byte-identical to main_31 (full suite:
net +0, broke 0) — proven, not assumed.

**Decisive result — the offset2 error is NOT in the STOP-window length.** Per-case
ADJ sweep over the 3 offset2 roms (`count_2`=target, `count_1`=passing sibling,
`m0stat_scx2`=target):

```
ADJ:  -16 -6 -4 -3 -2 -1  0  1  2  3  4  6  16
count_2(target):   F  F  F  F  F  F  F  F  F  F  F  F  F   (never fixes)
count_1(sibling):  P  F  P  P  F  F  P  P  F  F  P  F  P   (only ever BREAKS)
m0stat_scx2(tgt):  F  F  F  F  F  F  F  F  F  F  F  F  F   (never fixes)
```

At NO unhalt-window length does either target case pass; the only effect of a nonzero
ADJ is to break the passing sibling. So the 4-STOP CPU instruction-boundary cc is NOT
the offset2 lever — the LY-counter sample in the ISR is invariant under the unhalt-cc
shift because the FF44 reads happen many lines later, on the renderer phase.

**Redirection (where R1 actually lives):** `ppu/controller.rs::get_ly_reg_at_cc`
(~line 5920). The `_count` tests probe the getLyReg "anticipation window"
(`to_next == 6 + 4*ds`, the brief `ly & (ly+1)` glitch dot). The offset2 count is
decided by the FF44-read **access cc vs that window boundary** — a per-access-cc /
renderer-phase error in `get_ly_reg_at_cc`, NOT a CPU STOP-cc accumulation. R1 should
be RE-CLASSIFIED into the renderer-getLyReg-cc family (adjacent to R3/R5), and the
next session should sweep the read `time`/`to_next` anchor in `get_ly_reg_at_cc`
against the cctracer FF44 oracle (cc/LY/lineCycles), NOT the STOP window.

The `RB_CANONICAL_CC` scaffold stays (ADJ default 0 ⇒ ship-inert); the STOP-window ADJ
hook is retained as a documented dead-end marker and the proven flag plumbing for the
next stage.

### Milestone-2 (R1 redirected → getLyReg anticipation window) — ALSO REFUTED, root isolated
Rebased onto main@29 (clean). flag-OFF re-verified byte-identical to **main_29**
(full suite net +0, broke 0). Moved `RB_CANONICAL_CC_ADJ` to shift `to_next` in
`get_ly_reg_at_cc` (non-halt read path only).

**Instrumented ground truth (cctracer + in-engine LY trace):**
- The two siblings differ ONLY by a 1-byte `.text` offset (`_1`: read@`1067`/loop@`1148`
  + 3 trailing NOPs; `_2`: read@`1068`/loop@`1149`, different cmp, no NOPs). This is the
  literal "1-byte .text offset crosses a boundary" CPU-cc signature.
- That 1 byte shifts the count-loop FF44 read's access cc by **8cc (2 M-cycles)**:
  rustyboi's `_1` deciding reads land at getLyReg `to_next=6` (glitch dot, ANTIC fires);
  `_2` lands at `to_next=14` (outside the ≤10 window → returns the plain renderer LY,
  the glitch never fires). The 8cc gap === the off-by-one in the printed count
  (rustyboi prints 99, expected 9A; high nibble "9" already matches).

**getLyReg-window sweep (the redirect's own hypothesis) — REFUTED by sibling-swap:**
```
ADJ(to_next):  -10 -8 -6 -4 -2  0  2  4  6  8
count_2(tgt):    F  F  F  F  F  F  P  F  F  F   <- only adj=+2 fixes it
count_1(sib):    F  F  F  F  F  P  F  F  F  P   <- and +2 BREAKS the sibling
m0stat(tgt):     F  F  F  F  F  F  F  F  F  F   <- never moves
```
Exactly the predicted 1-for-1 swap. NO scoped getLyReg `to_next` constant fixes count_2
without breaking count_1. m0stat is unmoved by any window shift (different sub-root).
**Full-suite flag-ON @ADJ=+2 valley: net +318 (fixed 1, broke 318).** The +2 shifts
EVERY non-halt LY read suite-wide (all the m3stat/lyc/m2int/speedchange families), the
textbook mixed-anchors wall: getLyReg's `to_next` is a global anchor, structurally
un-sliceable for one case. Quantitatively confirms the lever is a dead-end.

**ROOT, now isolated (both candidate levers refuted):** the error is NOT in getLyReg's
formula (a faithful port of `video.h:124`), NOT in the STOP window, NOT in the getLyReg
`to_next` anchor. It is in the **absolute per-access CPU cc of the count-loop FF44 read**
itself: rustyboi's instruction-stream cc is ~8cc mis-phased for the `_2` byte alignment
(correct for `_1`). The read access cc = `bus.rs:594 master_cc()`; that value is right for
one alignment and wrong for the other, so the 1-byte shift lands the read on the wrong
side of the getLyReg window. This is the per-access-cc instruction-boundary root the whole
endgame is about — it is genuinely NOT slicable by any peripheral/renderer constant
(proven twice now by sibling-swap). m0stat_scx2 is a separate STAT-mode sub-root in the
same family.

**NEXT-SESSION direction (R1 needs the real per-access CPU cc, not another constant):**
- The fix must make the FF44 read's `master_cc()` byte-exact across the 1-byte `.text`
  shift, i.e. validate rustyboi's per-instruction cc accounting against Gambatte for the
  exact opcode stream between ISR entry (`0x1068`/`0x1149`) and the FF44 read. Use cctracer
  with watch-PCs on that stream to get Gambatte's `cc` at the read for BOTH variants and
  compare to rustyboi's `master_cc()` at the same architectural read (add a PC-gated trace
  at `bus.rs:594`). The 8cc discrepancy lives in some opcode's cc cost or the ISR
  dispatch/EI-service cc on this stream — find WHICH instruction, fix its cc (flag-gated),
  re-validate flag-OFF==29.
- Likely shares the lever with R2 (the HDMA m2-unhalt brackets are the SAME "1-byte .text
  offset crosses a TIMA tick" signature). Consider tackling R1's instruction-cc fix and
  R2 together once the per-instruction cc audit tool exists.
- Defer R1 to that audit; do NOT ship a getLyReg/STOP constant.

The `RB_CANONICAL_CC_ADJ` hook now drives the getLyReg `to_next` probe (was STOP-window;
both are documented dead-ends). flag-OFF / ADJ=0 remain ship-inert.

### Milestone-3 (per-instruction cc AUDIT → real fix) — R1 `offset2_lyc98int_ly_count_2` LANDED (flag-ON net −1, broke 0)
Built the audit: PC-gated per-instruction `master_cc()` trace at `bus.rs::fetch_opcode`
+ an FF44-read value trace, vs cctracer's `[INSTR]`/`[DISPATCH]` stream
(`cctracer <rom> 0x1068 0x1149 …`). **The m2 "8cc count-loop" picture was a red
herring — the audit found the divergence is at the ISR-ENTRY read, one line earlier:**

Audit table (offset2_lyc98int_ly_count_2, ISR entry):
| PC | rustyboi cc | rustyboi FF44 | Gambatte cc | Gambatte FF44 |
|------|-------------|---------------|-------------|---------------|
| 0x1068 `ldff a,(44)` | 564196 (read@564204) | **153** | 622568 | **0** (a=0x00 post-exec) |
| 0x106B `jpnz lprint` | — | taken (153≠b=0) → BAILS | — | not taken (0==0) → enters loop |

rustyboi's per-instruction cc is byte-exact (contiguous +4/fetch, no drift) — the
**instruction stream is NOT mis-phased**. The bug is purely that rustyboi's `get_ly_reg_at_cc`
returns **153** at the line-153 ISR-entry read where Gambatte returns **0**. rustyboi
bails out of the count loop on the FIRST check (`cmp a,b; jpnz`), never running the
counting loop at all; Gambatte reads 0, passes the check, and counts normally.

**Root (different from m2's hypothesis):** rustyboi's line-153 single-speed branch
returned 0 only at the line TOP (`to_next >= cpl`) and DEFERRED to the renderer
otherwise — but the renderer's dot-6 LY→0 flip has NOT happened at the just-wrapped
ISR-entry read (`to_next=454`, renderer still 153), so the defer yields 153. Gambatte's
`getLyReg` (`video.h:135`) returns 0 for the WHOLE of line 153 at SS non-agb
(`time - cc <= cpl - isAgb`). 

**Fix (faithful, flag-gated `RB_CANONICAL_CC`):** in the `ly_reg==153 && !ds` branch,
resolve 0 for the whole line via the raw-Gambatte-time bound `to_next - 1 <= cpl`
(the `-1` undoes rustyboi's `+1` lyTime correction; verified against the `to_next=457`
just-wrapped read in `lycint152_ly153_3` which also needs 0). This removes the
renderer-flip race entirely.

**Validation:** flag-ON full suite = **net −1 (fixed `offset2_lyc98int_ly_count_2`,
broke 0)**. Initial naive `to_next <= cpl` broke 4 (`frame1_ly_count_2`,
`lycint152_ly153_3` ×{dmg,cgb}); the `-1` raw-time correction fixed those too →
zero regressions. flag-OFF byte-identical to main_29. Debug build clean.

**WHICH OF THE 15 MOVED:** `offset2_lyc98int_ly_count_2` (R1) — FIXED, flag-ON, no swap.
`offset2_lyc99int_m0stat_count_scx2_1` did NOT move (m0stat sub-root — it reads FF41
STAT mode, not FF44 LY; separate fix).

**NEXT SESSION:**
- This fix is unconditionally faithful (a verbatim port of Gambatte `video.h:135`),
  flag-ON net-negative with zero regressions. STRONG candidate to make default-on
  (flip `canonical_cc_enabled` line-153 branch to unconditional, drop the old top-only
  path) → would take main to 28. Verify once more, then the flag-flip is a clean commit.
- `offset2_lyc99int_m0stat_count_scx2_1`: apply the SAME audit method to the FF41
  STAT-mode read at the count-loop (`get_stat`/`get_stat_mode_at_cc`) — likely an
  analogous line-153 / mode-1 STAT-read faithfulness gap.
- R3 (`oamdma_late_speedchange_stat_2`, SS→DS m0Time +18 vs −4) and R2 (HDMA brackets)
  remain; the audit method (cctracer `[INSTR]` stream + PC-gated engine trace) is now
  proven and reusable for both.

### Milestone-4 (rebased onto main@28; m0stat audit → m0Time lever REFUTED by sibling-swap)
M3 landed on main env-free (main 29→28). Rebased onto main@28, re-verified flag-OFF
byte-identical to **main_28** (net +0, broke 0). New gate baseline = main_28.

Audited `offset2_lyc99int_m0stat_count_scx2_1` with the same method (PC-gated FETCH +
FF41/FF44 value trace + cctracer `[FF41 READ]`/`[INSTR]`). The test loops reading FF41
`== 0x83` (mode 3) and prints the LY at which the mode leaves 3 (expected 0x90=144).

**Audit table (ISR-entry FF41 read, LY=0):**
| | rustyboi | Gambatte |
|---|----------|----------|
| read cc | 564908 | 623280 |
| m0Time | 564910 | 623283 |
| m0Time − cc | **2** | **3** |
| getStat mode (`cc+2 < m0Time`) | **0** (HBlank) | **3** (XFER) |

rustyboi exits the loop on the FIRST read (mode 0 → ≠0x83 → bail), printing the wrong
LY; Gambatte reads mode 3 and counts. The divergence is `get_stat_mode3to0_at_cc`'s
m0Time being **exactly 1cc low** at the LY=0 first-visible line after the 4-STOP speed
switch: the post-DS→SS `lytime_no_plus1` flag drops the lyTime `+1` from m0Time (gap 2),
where Gambatte keeps gap 3. (read_off is already the faithful +2; the lever is m0Time.)

**m0Time `+1` lever — REFUTED by sibling-swap (the per-access-CPU-cc bracket):**
- broad (`!ds && lytime_no_plus1`): flag-ON net **+18** (fixed 1, broke 19) — every
  post-switch `_2` m3stat/m0stat/speedchange sibling regressed (they need gap 2).
- narrowed to `internal_ly_val == 0`: net **+2** (fixed 0, broke 2) — UN-fixes the
  target (its count needs the gap-3 on the exit read too, not just LY=0) AND still
  breaks `offset1_..._2`/`offset3_..._2` (currently PASSING, gap-2 correct).

`offset2_..._1` (gap-3) and `offset1/offset3_..._2` (gap-2) are **the same line/LY** but
differ only by a 1-byte `.text`/offset shift in the read cc → they straddle the
`cc+2 < m0Time` boundary in OPPOSITE directions. This is the genuine **per-access CPU
cc** bracket (a 1-byte offset shifts the read cc by 1cc), NOT a line-153-style
faithfulness gap. NO m0Time/read_off constant resolves `_1` without swapping the `_2`
siblings. Confirmed dead-end; lever reverted (tree clean, flag-OFF==28).

**Distinction from M3:** getLyReg (M3) had a true FORMULA bug (top-only vs whole-line),
fixable faithfully. m0stat's getStat formula is already correct (`cc+2<m0Time`); the
residual is the 1cc m0Time phase at the LY=0 post-switch line, and the cases that need
it shifted are byte-indistinguishable (by any line/LY/state predicate) from the cases
tuned to the current value — the classic mixed-anchors wall. m0stat needs the real
per-access CPU read cc (the 1-byte offset must change the read's `master_cc()` by 1),
same root as R2. NOT slicable here.

**NEXT:** pivot to R3 (`oamdma_late_speedchange_stat_2`, renderer-phase) per coordinator
— more tractable than the per-access-cc brackets (R1-m0stat / R2). The audit method
(cctracer `[FF41 READ]`/`[INSTR]` + PC-gated engine STAT3/STAT30 + m0Time trace) is
reusable. m0stat + R2 both await a per-access-CPU-cc mechanism (the 1-byte-offset read
cc), deferred together.

### Milestone-4b (R3 `oamdma_late_speedchange_stat_2` audit) — CONFIRMED the SS→DS bridge m0Time phase (deep cluster, deferred)
Test: ISR fires OAM DMA (FF46=C0), waits, does a CGB STOP speed-switch, then immediately
reads FF41 and prints the STAT mode (expected out3 = mode 3).

**Audit (post-STOP FF41 read at LY=3), cctracer vs engine:**
| | rustyboi | Gambatte |
|---|----------|----------|
| read cc | 149836 | 208204 |
| m0Time | 149818 | 208208 |
| **m0Time − read cc** | **−18** (m0Time in the PAST) | **+4** |
| renderer state / mode | **HBlank / mode 0** | **PixelTransfer / mode 3** |

rustyboi's `m0_time_master` sits **~22cc too low** relative to the read across the SS→DS
switch (−18 where Gambatte is +4), so the renderer is in HBlank (mode 0) where Gambatte
is still in mode 3. The mode-3 STAT read resolves via `get_stat_mode3to0_at_cc`, which
returns None here (state==HBlank, not PixelTransfer) → falls to the renderer's mode-0
register. This is EXACTLY the documented "+18 vs −4" m0Time divergence across the speed
switch (≈22cc line-position phase error in the post-switch mode-3 WINDOW LENGTH).

**Verdict — NOT a quick faithful-port fix; it is the deferred coupled cluster.** Per
memory (`uniform-faithful-bridge`, `m3len-is-cpu-phase-not-renderer`): the SS↔DS STOP
bridge's mode-3-length / m0Time phase cannot be fixed by a uniform constant (off2-vs-scx
resist any single bridge dot-count), and needs the coupled mode-3-length/m0Time rebase
(`opcodes.rs::stop` bridge + `compute_m3_length` + `m0_time_master` re-anchor across the
switch, all together). That is a deliberate multi-session build, not a one-read fix.
Characterized and deferred — do NOT chase a constant bridge tweak (proven sibling-swap).

## SESSION SUMMARY (state for handoff)
- main: **28** (M3 landed env-free). Branch `endgame-cc` = 5 commits on main@28, tree
  clean, flag-OFF byte-identical to main_28 (net +0, broke 0), debug build clean.
- **Landed (on main via M3):** `offset2_lyc98int_ly_count_2` — getLyReg line-153
  whole-line-0 faithful fix (`video.h:135`).
- **Refuted by audit (per-access-CPU-cc brackets, not slicable by constants):**
  R1-`offset2_lyc99int_m0stat_count_scx2_1` (m0Time +1 swaps offset1/3 `_2` siblings),
  and the m2 STOP-window / getLyReg-window levers.
- **Characterized + deferred (deep coupled cluster):** R3 SS→DS bridge m0Time phase
  (≈22cc), needs the mode-3-length/m0Time rebase.
- **Untouched:** R2 (8 HDMA m2-unhalt+TIMA brackets — same per-access-cc signature as
  R1-m0stat), R4 (dmg write-vs-latch), R5 (per-pixel LCDC).
- **Reusable tooling proven:** cctracer `[INSTR]`/`[DISPATCH]`/`[FF41 READ]`/`[FF44 READ]`
  (m0Time, lineCycles, lyTime) is the oracle; PC-gated engine traces at
  `bus.rs::fetch_opcode` + `controller.rs::get_stat_mode3to0_at_cc`/`get_ly_reg_at_cc`
  give the per-read engine side. Pattern: audit → isolate the divergent READ → if it's a
  faithful-port formula gap (like getLyReg) land it; if it's a per-access-cc bracket
  (sibling-swap) or the SS↔DS bridge phase, defer to the per-access-cc / m3-length rebuild.
- **Recommended next:** the per-access-CPU-cc mechanism (make a 1-byte `.text` offset shift
  the read's `master_cc()` by 1cc) would unlock R1-m0stat AND R2's 8 brackets together —
  the highest-leverage remaining build. R3's m3-length rebase is the other deep build.
  Both are multi-session; the cheap faithful-port wins (getLyReg-style) appear exhausted
  among the current 14.

### Milestone-5 (DIFFERENTIAL audit — m0stat 1cc source CLASSIFIED: #3 emergent m0Time quantization)
Traced BOTH siblings of `offset2_lyc99int_m0stat_count_scx2` through BOTH emulators from
ISR dispatch → the deciding FF41 read. **The siblings differ by a 1-byte `.text` offset
(`_1` read@`0x10A5` `cmp 0x83`=want-mode-3; `_2` read@`0x10A6` `cmp 0x80`=want-mode-0).**

**4-column differential table (cc at each anchor):**
| event | rb_cc `_1` | gb_cc `_1` | rb_cc `_2` | gb_cc `_2` |
|-------|-----------|-----------|-----------|-----------|
| ISR dispatch (pc=0x48) | 564220 | — | 564220 | — |
| FF41 read fetch (0x10A5/0x10A6) | 564900 | 623280 | 564904 | 623284 |
| **read−read sibling delta** | **+4** | **+4** | (anchor) | (anchor) |
| FF41 read access cc | 564908 | 623280 | 564912 | 623284 |
| m0Time at read | 564910 | 623283 | 564910 | 623283 |
| **m0Time − read cc (gap)** | **2** | **3** | **−2** | **−1** |
| getStat (`cc+2 < m0Time`) | mode 0 ✗ | **mode 3** ✓ | mode 0 ✓ | mode 0 ✓ |

**CLASSIFICATION = #3 (emergent PPU-boundary-vs-CPU-phase), NOT a CPU-cc bug:**
1. **Per-instruction cc is byte-exact.** Both emulators agree the 1-byte operand shifts
   the read by exactly **+4cc** (`_1`→`_2` delta = +4 in BOTH). Dispatch is identical.
   So it is NOT a wrong opcode cost and NOT a wrong dispatch/service cc — corrects the m4
   note's "1-byte offset shifts read cc by 1cc" (it shifts by 4; the read cc is RIGHT).
2. **m0Time is the shared anchor and is 1cc LOW.** Gambatte's gap for `_1` is **3**
   (mode 3); rustyboi's is **2** (mode 0). The `+4` sibling delta lands `_1` and `_2` on
   the SAME m0Time(623283) in Gambatte at gaps 3 and −1 → mode 3 / mode 0. rustyboi's
   `lytime_no_plus1` (set on the DS→SS switch, never cleared without an LCD-enable) drops
   the lyTime `+1`, quantizing m0Time 1cc low → `_1`'s gap collapses 3→2 → mode 0.
3. **The 1cc CANNOT be sliced.** Cross-test differential proves the swap-victims
   `offset1_lyc99int_m0stat_count_scx2_2` / `offset3_..scx0_2` are **want-mode-0** loops
   (`cmp 0x80`) whose Gambatte gap is genuinely **2** (`m0Time−cc=2`, `cc+2<m0Time` tie →
   mode 0). So `_1` needs gap 3 and the victims need gap 2 **at the same LY=0 post-switch
   line** — a real Gambatte difference driven by each test's distinct cumulative
   instruction cc before the loop. rustyboi has ONE m0Time per line, so no (LY, state,
   `lytime_no_plus1`) predicate separates them.
4. **m0Time is multi-consumer.** The +1 (even scoped to `get_stat_mode3to0_at_cc` &&
   `internal_ly==0`) still broke the victims: their FF41 reads never call
   `get_stat_mode3to0_at_cc` (0 calls — they resolve via `get_stat_mode_at_cc` in HBlank),
   yet they regressed — because `get_stat_mode3to0_at_cc` is ALSO the VRAM-access-lock
   gate (`bus.rs:417`), so the +1 shifted the mode-3 RENDER lock and corrupted their
   printed tiles. m0Time cannot be perturbed for the FF41 read alone.

**Validation:** flag-ON ily==0-scoped +1 = net **+2** (fixed 0, broke the 2 want-mode-0
victims). Even fixing `_1`'s LY=0 read, its loop then exits at LY=1 (every post-switch
line needs the +1, not just LY=0). flag-OFF byte-identical to main_28. Reverted clean.

**THE REARCHITECTURE SPEC (definitive — why this needs the coupled build):**
The defect is that rustyboi carries ONE `m0_time_master`/lyTime per line with a BINARY
`lytime_no_plus1` switch, where the true Gambatte value is a continuous sub-cc phase that,
combined with each instruction-stream's exact read cc, yields gap 3 for some post-switch
reads and gap 2 for others. To resolve m0stat (and the structurally-identical R2 HDMA
m2-unhalt brackets) WITHOUT swapping siblings, the post-DS→SS `m0_time_master` must be
re-derived to Gambatte's exact sub-cc (the `lcd_.speedChange` re-anchor in `opcodes.rs::
stop` + `compute_m3_length` + the lyTime `+1` fold), so that `cc + 2 < m0Time` evaluates
byte-true at every read cc — replacing the `lytime_no_plus1` boolean with the faithful
per-cc anchor. That is the SAME post-switch m0Time rebase R3 needs (the −18-vs-+4 / +18
divergence) — **R1-m0stat, R2's 8 brackets, and R3 all collapse to one post-DS→SS
m0Time/m3-length re-anchor build.** It is NOT a faithful-port one-liner; the m0Time
anchor is shared by the FF41 mode read, the VRAM/OAM render lock, and the m0/m2 IRQ
schedule, so it must move atomically. No MAIN-MERGE CANDIDATE this session (the only
faithful change, the +1, is multi-consumer and sibling-swapping).

**Which of the 9 moved:** none landed (correctly — the lever is a proven sibling-swap).
The audit's payoff is the spec above: the highest-leverage remaining build is the single
post-DS→SS m0Time/m3-length re-anchor, which subsumes R1-m0stat + R2 (8) + R3 (~10 cases).

### Milestone-6 (BUILD attempt: post-DS→SS m0Time re-anchor) — root pinned to a DOT-LEVEL m3-length deficit
Attempted the m5-spec build three ways, each flag-gated, each measured. ALL three move the
target `offset2_lyc99int_m0stat_count_scx2_1` (fix) but break the want-mode-0 victims
`offset1_lyc99int_m0stat_count_scx2_2` / `offset3_..scx0_2` — the SAME sibling-swap, now
pinned to its exact dot-level cause:

1. **read_off 2→1** on the post-switch FF41 read (`stat_read_off`): target gap 2→effective-3
   (mode 3 ✓), but the victim's same-path read also widens → mode 3 ✗.
2. **Atomic m0Time `+1`** (restore the dropped lyTime `+1` in `m0_time_exact`, moving ALL
   consumers — render lock + IRQ + read — together): target gap 2→3 (mode 3 ✓), victim gap
   2→3 (mode 3 ✗).
3. **lineCycle-domain analysis** (`self.ticks` vs `scheduled_mode0_dot`): see the table.

**Decisive dot-level table (flag-OFF, the deciding FF41 read, via `[S30]`/`[M0EX]` traces):**
| | rb read lineCycle (`ticks`) | rb `scheduled_mode0_dot` | rb `m0_line_cycle` | **Gambatte m0 boundary lineCycle** |
|---|---|---|---|---|
| target `_1` (scx2) | 250 | **251** | 251 (m3_len 167 + base 84) | **253** |
| victim offset1 `_2` (scx2) | 251 | **252** | 251 | **253** |

**ROOT (exact):** rustyboi's per-test post-bridge m0 boundary is **251/252 — i.e. 2 dots
(target) and 1 dot (victim) SHORT of Gambatte's 253.** The read lineCycles are byte-exact
(250/251 == Gambatte). Both tests compute the SAME `m0_line_cycle=251` from
`m3_len=167 + base=84`, yet `scheduled_mode0_dot` lands 251 vs 252 — the bridge/speedChange
leaves a per-test sub-dot residue. Gambatte's mode-3 window extends to lineCycle 253 on this
post-DS→SS line; rustyboi's `compute_m3_length`/bridge ends it ~2 dots early. Because the
deficit is per-test (2 vs 1) and the reads straddle the true 253 boundary at 250/251, NO
uniform m0Time/read_off shift places both correctly — only restoring the **true mode-3
length** (boundary → 253) does, which requires the bridge/m3-length rebuild, not an anchor nudge.

**This IS the `m3len-is-cpu-phase-not-renderer` / SS↔DS-bridge cluster, now quantified:** the
fix must make the post-DS→SS line's m0 boundary land at Gambatte's lineCycle 253 (add the ~2
missing mode-3 dots in `stop_bridge_advance` + `compute_m3_length` for the post-switch line),
after which the existing cc-domain `cc+2 < m0Time` resolves target(250)→mode3 and
victim(251)→mode0 by construction. This is the same m3-length deficit R3
(`oamdma_late_speedchange_stat_2`, m0Time −18-vs-+4) shows — confirming R1-m0stat + R2 + R3
share ONE post-DS→SS m3-length root.

**Validation:** all three attempts flag-ON net +2 (fixed target, broke the 2 want-mode-0
victims). flag-OFF byte-identical to main_28 (net +0, broke 0). Experiments reverted; tree clean.
No MAIN-MERGE CANDIDATE (the bridge m3-length rebuild is the real fix; multi-session).

**NEXT-SESSION concrete entry point:** in `opcodes.rs::stop` (DS→SS branch) /
`controller.rs::stop_bridge_advance` + `compute_m3_length`, the post-switch line must report
`m3_len` such that `m3_len + base == 253` (≈ +2 over the current 251) — derived from Gambatte's
`lcd_.speedChange` mode-3 carry, NOT the current bridge dot count. Verify with cctracer that
the post-switch line's m0 boundary == lineCycle 253 for BOTH siblings; then the cc-domain
getStat needs no read_off/anchor patch, and `lytime_no_plus1` can be dropped. Risk: `m3_len`
feeds the render lock + IRQ schedule too, so the +2 must come from the genuine speedChange
mode-3 carry (a real PPU phase) so all consumers move faithfully together — that is the
coupled build, with this table as the acceptance test (boundary==253 both siblings).

### Milestone-6b (SHARPENED: the deficit is per-test `ly_time` sub-cc phase, NOT m3_len) — definitive
A 4th probe — extend `m0_line_cycle += 2` on the post-switch line (the m6 "add the 2 missing
mode-3 dots" hypothesis) — was tested and ALSO swapped the victims (target fix, victims break).
That falsifies the "+2 to m3_len" framing and pins the true mechanism by recomputing the m0
boundary in **`lineCycle(m0Time)`** terms (correcting an arithmetic slip in m6):

The getStat transition lineCycle = `lineCycle(m0Time) − 2` (from `cc+2 < m0Time`). Gambatte:
- target: `lineCycle(m0Time)=253` → transition at 251; read at **250** < 251 → mode 3 ✓
- victim: `lineCycle(m0Time)=253` → transition at 251; read at **251** ≥ 251 → mode 0 ✓

Both Gambatte m0Times are at **lineCycle 253**; the reads (250 vs 251) straddle the transition
at 251. rustyboi (flag-OFF):
- target: m0t=564910 @ read cc 564908 (lineCycle 250) → `lineCycle(m0t)=252` — **1 SHORT**
- victim: m0t=288698 @ read cc 288696 (lineCycle 251) → `lineCycle(m0t)=253` — **CORRECT**

**THE definitive root:** the target's `m0Time` is 1 lineCycle low; the victim's is already
exact — though both share `m0_line_cycle=251` and `m3_len=167`. The difference lives ENTIRELY
in `ly_time = p_now + ly_counter().time` (+plus1): the victim's post-switch `ly_time` is
naturally 1cc higher than the target's, from each test's distinct STOP timing / bridge cc
bookkeeping. It is a **continuous per-test sub-cc phase in `p_now`/`ly_counter().time` after
the DS→SS bridge** — NOT m3_len, NOT a boolean, NOT read_off. Any uniform shift (read_off,
+1, +2 m3_len, atomic m0Time) moves BOTH equally and swaps, because the two tests genuinely
differ by 1cc of bridge-anchored line phase that rustyboi's bridge collapses.

**Acceptance test (sharpened):** `lineCycle(m0Time) == 253` for BOTH siblings — which requires
the target's post-switch `p_now + ly_counter().time` to be 1cc higher (the victim's is already
right). The fix is in the bridge cc bookkeeping (`stop_bridge_advance` / `perform_speed_switch`
/ the `now -= old_ds` re-anchor + the returned-8 STOP cycles' new-speed realization in
`opcodes.rs::stop`), making `p_now`/`ly_counter` land Gambatte's exact post-`lcd_.speedChange`
cc for EVERY STOP-timing — i.e. the faithful continuous re-anchor, after which
`lytime_no_plus1` (the binary stand-in) is deleted. This is the genuine coupled bridge build;
the per-test 1cc cannot be sliced. All probes flag-OFF byte-identical to main_28; reverted clean.

### Milestone-7 (LANDED: the per-test 1cc IS sliceable — it's the FACET1 mode-3 STAT-phase carry) — `offset2_lyc99int_m0stat_count_scx2_1` FIXED, flag-ON net −1 broke 0
m6b said "the per-test 1cc cannot be sliced." **That was wrong** — the ARM-site differential
this session found the distinguisher and it slices cleanly.

**ARM-site trace (both siblings IDENTICAL):** `p_now`, `abs_cc`, `lc.time`, `m0t-master=168`,
`ticks=82` — the m0Time computed at M3-arm is byte-identical for target and victims. So the
1cc is NOT in the m0Time derivation (correcting m6b).

**Read-site trace — the distinguisher is `render_carry_skew_cc`:**
| | read cc | m0t−cc | `ticks` (read lineCycle) | `line_cycle` | **`render_carry_skew_cc`** |
|---|---|---|---|---|---|
| target `_1` | 564908 | 2 | **250** | 251 | **1** |
| victim offset1 `_2` | 288696 | 2 | 251 | 251 | **0** |
| victim offset3 `_2` | 288696 | 2 | 249 | 249 | **0** |

The target is a `dsss_mode3_switch` (DS→SS during mode 3): its FACET1 STAT-phase carry
(`stat_phase_carry`, "every 2nd mode-3 switch carries one STAT dot") advanced `line_cycle`/
m0Time by 1 dot WITHOUT moving the render latch (`ticks`) or the read-cc grid — so `ticks`
(250) is 1 BEHIND `line_cycle` (251) and the FF41 read cc sits 1 dot behind the carried
m0Time. The victims took no mode-3 carry (`render_carry_skew_cc==0`), so `ticks==line_cycle`
and their read is already aligned. **That carry IS the per-test 1cc — recorded in live state,
fully sliceable.**

**THE FIX (faithful, env-gated `RB_CANONICAL_CC`):** subtract the carry in the getStat mode-3
read boundary: `read_off = base − render_carry_skew_cc` when `lytime_no_plus1`. The carry
advanced the STAT/m0Time clock relative to the read grid, so Gambatte's `cc + 2 < m0Time` is
evaluated with the read cc shifted back onto the carried grid (`cc + (2 − carry) < m0Time`).
Target: off 2→1 → mode 3 ✓. Victims: off 2−0=2 → mode 0 ✓. Centralized in `stat_read_off(ds)`,
used by all four getStat mode-3 read sites (mode3to0 + 3 midframe paths).

**VALIDATION (full suite):** flag-OFF byte-identical to main_28 (net +0, broke 0); flag-ON
net −1, fixed 1 (`offset2_lyc99int_m0stat_count_scx2_1`), broke 0. cctracer proof: Gambatte
target read `cc=623280 m0Time=623283 cc+2=623282<623283 → STAT=0x83 mode3`, lineCycle(m0Time)
=253; rustyboi flag-ON resolves mode 3 there. Release + debug clean.

**Which of the ~10 moved:** 1 — R1-m0stat. R2's 8 HDMA brackets + R3 did NOT move: R3's
post-STOP read is in HBlank state (not the PixelTransfer mode3to0 path); R2's are the
m2-service/unhalt TIMA path with no `render_carry_skew_cc`. They share the post-DS→SS theme
but not this mode-3-carry read path — each needs its own audit (R3 = HBlank-state getStat
under carry; R2 = unhalt service cc).

**MAIN-MERGE CANDIDATE (env-free):** faithful (subtract the real FACET1 carry the STAT phase
already tracks), flag-ON net −1, broke 0. Env-free = drop the `canonical_cc_enabled()` guard
so the subtraction always runs (carry is 0 except on a post-mode-3-switch line ⇒ inert
elsewhere ⇒ byte-identical to flag-OFF on all but the fixed case). Exact env-free helper:
```rust
fn stat_read_off(&self, ds: bool) -> i64 {
    let base = if !ds && !self.lytime_no_plus1 { 3 } else { 2 };
    // FACET1 mode-3 STAT-phase carry advanced m0Time relative to the read grid;
    // realign the getStat boundary by the carry (0 on non-carry lines => inert).
    if self.lytime_no_plus1 { base - self.render_carry_skew_cc } else { base }
}
```
(+ the 4 call sites using `self.stat_read_off(ds)`). Verify env-free full suite = main_27, broke 0.

### Milestone-8 (rebased onto main@28→27; R2 HDMA-TIMA bracket audit) — DEFINITIVE BLOCKER: STOP-window/bridge cc residue, NOT a TIMA-local sliceable key
Rebased onto main@2a5030b (m7 landed env-free on main; conflict resolved by taking main's
unconditional `stat_read_off`). flag-OFF re-verified byte-identical to **main_27** (net +0,
broke 0). New gate baseline = main_27.

Applied the m7 carry-key technique to R2's `hdma_late_m3speedchange_tima_scx1_ds` bracket
(`_1`..`_6`, expected F3 F4 F6 F7 F8 F9; `_3`/`_6` FAIL). The siblings differ by NOP count +
1-byte `.text` shifts that walk the TIMA read cc across a tick edge. Test: TIMA=F0, TMA=F0,
TAC=05 (fast, clk=4 → 16-cc period), HALT, ISR fires HDMA (FF55=80) + a CGB STOP speed-switch,
then NOPs, then `ldff a,(05)` and prints TIMA.

**Differential (TIMA read internals, rustyboi vs Gambatte):**
| sib | rb access_cc | rb tlu | rb (cc−tlu) | rem | rb ticks | rb val | want | carry_skew |
|---|---|---|---|---|---|---|---|---|
| _2 | 281648 | 150560 | 131088 | 0 | 8193 | F4 | F4 ✓ | 0 |
| _3 | 281706 | 150564 | 131142 | 6 | 8196 | F7 | **F6** ✗ (+1) | 0 |
| _4 | 281710 | 150564 | 131146 | 10 | 8196 | F7 | F7 ✓ | 0 |
| _5 | 281726 | 150568 | 131158 | 6 | 8197 | F8 | F8 ✓ | 0 |
| _6 | 281730 | 150568 | 131162 | 10 | 8197 | F8 | **F9** ✗ (−1) | 0 |

**NO sliceable live-state key exists (unlike m7):**
1. `render_carry_skew_cc == 0` for ALL — the m7 FACET1 mode-3 carry does NOT apply (these
   STOPs aren't mode-3-render switches). The m7 key does not extend to R2.
2. The TIMA read math is byte-identical to Gambatte (`ticks = (cc − lastUpdate) >> clk`,
   tima.cpp:79). The only inputs are `access_cc` (read cc, = the faithful NOP walk) and `tlu`
   (`tima_last_update`, set at the STOP).
3. **Same rem, opposite outcomes ⇒ unsliceable by any uniform shift:** `_3` and `_5` BOTH have
   rem=6 but want opposite (drop a tick vs keep); `_4` and `_6` BOTH have rem=10 but want
   opposite (keep vs gain). A uniform tlu/cc/floor-phase shift moves all equally and cannot
   split same-rem siblings. Confirmed by sweep: `RB_CANONICAL_CC_ADJ` on the post-STOP tlu
   over −16..+8 NEVER fixed `_3`/`_6` (a tlu shift feeds back into access_cc through abs_cc,
   so `rem` is invariant); negative adj only broke `_2`.

**ROOT (definitive):** the per-test error is the **STOP-window / DS→SS-bridge cc residue**
landing in `tlu` (and `access_cc`) — the SAME root as R1-m0stat (`ly_time` phase) and R3
(m0Time phase). Measured: rustyboi's `_3` STOP window = 131126 vs Gambatte 131112 (+14cc); that
inflation pushes `_3`'s `(cc − tlu)` ~6cc high → 1 extra TIMA tick. `_6` is short the other
way. `tlu` is anchored AT the STOP cc (`div_reset_split` sets `tima_last_update = abs_cc`,
which is correct Gambatte behaviour), so the error is upstream in the STOP cc itself, not the
timer re-anchor. R2 is therefore the SAME post-DS→SS bridge cc build as R1/R3, NOT a separate
TIMA fix.

**How many of the 8 R2 brackets moved:** 0 — correctly. There is no faithful TIMA-local slice;
a uniform shift swaps a same-rem sibling (proven). The blocker is precise: rustyboi's STOP
abs_cc (the bridge-window length) is per-test off by a few cc, and that residue is in BOTH the
read cc and the TIMA anchor, so the TIMA value cannot be corrected downstream.

**This unifies the endgame:** R1-m0stat, R2 (8 brackets), R3 ALL reduce to the post-DS→SS
**STOP-window/bridge cc** being byte-exact per STOP-timing. The faithful fix is the bridge cc
re-anchor (`opcodes.rs::stop` window arithmetic + `stop_bridge_advance`), with acceptance
tests: m0stat `lineCycle(m0Time)==253`, R2 `_3` STOP window == 131112 (Gambatte) / TIMA
read == F6, R3 m0Time gap +4. m7's m0stat fix was sliceable ONLY because its specific read had
the FACET1 carry as a live-state proxy for the residue; R2/R3 have no such proxy and need the
window cc fixed at the source. flag-OFF byte-identical to main_27; experiments reverted clean.

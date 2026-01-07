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

### Milestone-8b (R3 carry-key check) — CONFIRMED same blocker as R2 (no carry proxy, state-level error)
Probed `oamdma_late_speedchange_stat_2` (R3) for the m7 carry-key. The deciding post-STOP FF41
read: `get_stat=Some(0)` (mode 0), **`render_carry_skew_cc=0`**, `state_in_xfer=false` (HBlank).
- The FACET1 carry key does NOT apply (carry=0), same as R2.
- The error is STATE-LEVEL: rustyboi's renderer is in HBlank (m0Time already −18 in the past,
  per m4b) where Gambatte is still in PixelTransfer (mode 3, m0Time +4) — a ~22cc m0Time phase
  error, far larger than m7's 1 dot. A carry-key (≤1 dot) cannot bridge it.
- R3 resolves via `get_stat_mode_at_cc` (HBlank path) returning mode 0; extending the m7
  `stat_read_off` carry subtraction (PixelTransfer mode3to0 path) does nothing here.

R3 is the SAME post-DS→SS STOP-window/bridge cc blocker as R2: the ~22cc residue puts the
renderer in the wrong mode entirely. No live-state proxy. Needs the bridge cc re-anchor at the
source (acceptance: post-STOP m0Time gap +4, renderer still in mode 3 at the read).

**SESSION VERDICT:** R1-m0stat fix is on main (m7, →27). R2 (8 brackets) + R3 are ONE deferred
build — the post-DS→SS STOP-window/bridge cc must be made byte-exact per STOP-timing
(`opcodes.rs::stop` window arithmetic + `stop_bridge_advance` / `perform_speed_switch`). There
is NO sliceable per-test live-state key for R2/R3 (carry_skew=0; same-rem opposite-outcome
siblings) — the residue is upstream in the STOP cc, in both the read cc and the timer/PPU
anchors, so it cannot be corrected downstream. This is the genuine remaining floor for ~9 of
the reducible cases; it is the bridge-cc surgery, deliberately multi-session.

### Milestone-9 (BUILD attempt: post-DS→SS STOP-window cc surgery) — DEFINITIVE FLOOR: block cost is co-tuned across multiple fire-paths; same-block siblings straddle a TIMA edge no uniform block-cost can split
Gave the bridge-cc surgery a genuine build attempt. **Located the +14cc precisely** (instr-cc
aligned, cctracer vs PC-gated engine trace on `hdma_late_m3speedchange_tima_scx1_ds_3`):

- rustyboi STOP-fetch(0x106F)@150560 → resume-fetch(0x1071)@281686 = **131126**.
- Gambatte STOP-instr@208948 → resume@340060 = **131112**. → rustyboi **+14cc**.
- The 0x20000 (131072) halt window itself is BYTE-EXACT (STOP-internal→UNHALT = 131072 in
  rustyboi). The +14 is entirely POST-UNHALT: an HDMA block stall (42cc, `[DMASTALL]@pc=1071`)
  fires AFTER the window where Gambatte folds the block's `intevent_dma` INSIDE the window
  (CPU halted throughout → block costs 0 CPU cc), plus the operand-exec.

**Per-sibling block-fire structure (the killer detail):**
| sib | exec_operand | block fires | path | rb val | want |
|---|---|---|---|---|---|
| _2 | false | block @pc=7402 (AFTER the TIMA read) | — | F4 | F4 ✓ |
| _3 | **true**  | 42cc @pc=1071 (before read) | operand-exec / per-dot edge | F7 | **F6** ✗ |
| _4 | true  | 42cc @pc=1071 (before read) | operand-exec / per-dot edge | F7 | F7 ✓ |
| _5 | false | 74cc @STOP (during window) | reflag | F8 | F8 ✓ |
| _6 | false | reflag @unhalt | reflag | F8 | **F9** ✗ |

**Build attempts (flag-gated, measured):**
1. **Full reflag-absorb** (fire the unhalt-reflag block, `reduce_dma_stall(produced)` — the
   faithful "block is free, inside the window"): full suite flag-ON **net +1, broke `_4`**,
   fixed 0. It over-corrects (drops the read 42cc = ~2.6 ticks) AND only touches the reflag
   path (`_5`/`_6`-style), not `_3`/`_4` (operand-exec path).
2. **Parameterized partial absorb** sweep (7..14cc) on the reflag path: absorb 7-9 flipped
   `_6`→PASS with `_2`/`_4`/`_5` intact, but `_3` was UNAFFECTED (its block fires via the
   operand-exec/per-dot path, not the reflag) — and 7-9 is a tuned constant, not the block
   size (42) nor the window excess (14).

**DEFINITIVE FLOOR (why the STOP-window can't be made faithful without a larger rewrite):**
- The HDMA block's CPU cost is **split across ≥2 co-tuned fire-paths** — `stop_window_exit_reflag`
  (for `_5`/`_6`) and the operand-exec / per-dot m0-edge path (for `_3`/`_4`). A fix to one path
  leaves the other's siblings wrong. There is no single block-cost site to correct.
- `_3`(want F6) and `_4`(want F7) are **byte-identical** through the STOP/unhalt/block (same cc,
  same 42cc stall) and differ ONLY by the read NOP (read at rem 6 vs 10, same TIMA tick in
  rustyboi). For both to land correctly, the block must cost a value that places `_3`'s read
  BELOW a TIMA tick edge and `_4`'s ABOVE it — but any block-cost change shifts BOTH reads
  equally (they share the block), so it cannot split them. Gambatte splits them because its
  block is free (read 42cc earlier) AND its tlu/edge phase falls between their reads; rustyboi's
  block-cc granularity + tlu phase cannot reproduce that sub-tick edge position.
- Confirms m8: there is no downstream slice. Making the window byte-exact (full absorb) BREAKS
  the currently-passing same-block sibling (`_4`), because the block cost and the TIMA-tick
  edge phase are co-tuned — the faithful free-block requires ALSO re-deriving tlu so the tick
  edge lands between `_3`/`_4`'s reads, i.e. the full Gambatte `Tima`+`Memory::dma`+`speedChange`
  port, not a window-arithmetic tweak.

**How many moved:** 0 net (full absorb +1/broke `_4`; partial fixes `_6` only at a tuned
constant). The precise blocker: **the post-DS→SS HDMA-block CPU cost is co-tuned across the
reflag and operand-exec fire-paths, and same-block TIMA siblings straddle a tick edge that no
uniform block-cost can split** — the faithful fix is the coupled `Tima`+`dma`+bridge re-derivation
(block-free window + tlu tick-edge re-anchor together), a larger speedchange rewrite. flag-OFF
byte-identical to main_27; all experiments reverted clean.

### Milestone-10 (coupled speedchange rewrite attempt) — ARCHITECTURAL FLOOR: tlu is byte-exact, the residual is HDMA-block-as-synchronous-stall vs Gambatte's interleaved `intevent_dma`
Committed to the coupled rewrite (pieces 1+2). Studied Gambatte `Memory::stop` (memory.cpp:444)
authoritatively: `intevent_unhalt = cc+0x20000+4`; `tima_.speedChange()`; `nontrivial_ff_write
(0x04,0,cc)` (DIV reset → `Tima::divReset`, tima.cpp:168: `lastUpdate_ -= (1<<(clk-1))+3;
updateTima(cc); lastUpdate_ = cc`). The HDMA block is NOT run in `stop()` — it fires later via
the `intevent_dma` scheduler slot, interleaved with the per-cc timer advance.

**PIECE 2 (tlu re-anchor) is ALREADY FAITHFUL — proven byte-exact:**
- rustyboi `div_reset_split` does EXACTLY Gambatte `Tima::divReset` (`tlu -= (1<<(clk-1))+3`;
  `update_tima`; `tlu = anchor_cc`) → post-stop `tima_last_update = stop abs_cc`.
- cctracer cross-check: Gambatte STOP cc `_2`=208944 / `_3`=208948; rustyboi STOP-internal
  (=tlu) `_2`=150560 / `_3`=150564 → **constant boot offset 58384 BOTH**. tlu is byte-exact to
  Gambatte. No re-anchor is needed; piece 2 is a no-op (already correct).

**PIECE 1 (block-free window) does NOT decompose — the residual is the block-cost model:**
- The 0x20000 halt window is byte-exact (m9). The error is purely the post-STOP HDMA block's
  CPU cost. But the block fires via DIFFERENT paths per sibling: `_3`/`_4` via the per-dot
  m0-edge during the resume instruction (`fire_pending_hdma_mcycle` in the bus tick), `_5`/`_6`
  via `stop_window_exit_reflag`. The reflag-absorb (tested, full + partial sweep) does NOT touch
  `_3`/`_4` (different path) and does NOT change `_5`/`_6`'s TIMA read (their block was already
  charged at STOP, not before the read).
- `_3` needs the read EARLIER (fewer ticks: 8196→8195); `_6` needs it LATER (8197→8198) —
  OPPOSITE corrections. No single block-cost change moves both correctly.
- `_3`/`_4` are byte-identical through STOP/unhalt/block (same tlu, same block, differ only by
  the read NOP). Their reads (rem 6 / rem 10) sit in ONE rustyboi TIMA tick; Gambatte splits
  them (F6/F7) because its block runs as a SCHEDULED EVENT interleaved with the per-cc timer
  advance, so the tick edge falls BETWEEN their reads. rustyboi charges the whole block as a
  synchronous `pending_dma_stall` LUMP, advancing the timer monotonically across it — it cannot
  place a tick edge mid-block at the sub-cc position Gambatte does.

**THE DEFINITIVE ARCHITECTURAL FLOOR:** R2/R3 are NOT fixable by STOP-window arithmetic, tlu
re-anchor (already exact), or block-cost tuning. The residual is that **rustyboi models the HDMA
block transfer as a synchronous CPU-stall lump, whereas Gambatte runs it as a scheduled
`intevent_dma` event interleaved with the timer's per-cc advance.** The 1-tick `_3`/`_4` (and
`_5`/`_6`) bracket is a sub-block-cc TIMA-edge position that only the event-interleaved model
produces. Closing it requires the per-event scheduler interleave (the block's cc advancing the
timer/PPU dot-by-dot during the transfer, with the TIMA tick edge landing mid-block) — i.e. the
MinKeeper-class event scheduler the project notes (`oamdma-dumpers`, "do not port MinKeeper")
deliberately scoped OUT. This is the true floor for R2 (8) + R3: ~9 cases gated on the
synchronous-stall→scheduled-event HDMA-transfer model, a scheduler rewrite beyond the
speedchange/bridge cc.

**Build result:** 0 of ~9 moved; no faithful slice (tlu exact, block-cost can't split same-block
siblings, paths divergent). flag-OFF byte-identical to main_27; all experiments reverted clean;
release + debug build clean.

**NEXT-SESSION (if pursued):** the only remaining lever is the HDMA-block event interleave —
advance the timer/PPU per-cc THROUGH the block transfer (so a TIMA tick can land mid-block)
instead of charging `pending_dma_stall` as a lump and draining it after. That is a focused but
real change to `run_hdma_block`/`fire_pending_hdma_mcycle` + the bus stall-drain loop (make the
block tick peripherals per-cc like Gambatte's `dma()` inner loop does for OAM-DMA). High
regression risk (every HDMA/GDMA test shares it). Acceptance unchanged: `_3`→F6, `_4`→F7,
`_6`→F9, `_2`/`_5` intact, R3 mode-3-at-read, m0stat `lineCycle(m0Time)==253`, whole
speedchange/dma/tima/div family byte-exact.

### Milestone-11 (per-cc HDMA-block interleave build) — HYPOTHESIS REFUTED: the read is post-block, and block-cc is COUPLED into tlu; per-cc interleave cannot help R2
Built toward the per-cc interleave. Two findings — one structural, one decisive:

**(a) rustyboi ALREADY ticks peripherals per-cc during the block stall.** `cpu.step()` returns the
block stall; `gb.rs` `tick_remaining(stall)` → `bus.run_to(target)` which advances every
peripheral ONE DOT AT A TIME (`bus.rs:163-165`, the proven per-dot primitive). So the timer/PPU/APU
DO advance per-cc through the block's cc. The block is a synchronous BYTE-COPY (all 16 bytes
written up front) but the cc advance that follows is already per-dot. The "lump" is the byte copy,
not the peripheral advance.

**(b) The per-cc interleave cannot change the TIMA bracket — the read is POST-block.** The
deciding TIMA read (`ldff a,(05)`) happens AFTER the block + its stall, when the CPU resumes. Its
value = `(read_cc − tlu) >> clk`. The interleave changes WHEN bytes copy, not the total block cc
nor `read_cc` (still = stall-end) nor `tlu`. So a TIMA tick edge "landing mid-block" is irrelevant
— the CPU read is at stall-end regardless. The m10 acceptance framing (per-cc → edge mid-block)
was wrong: the bracket is decided by the post-block read cc, not by mid-block tick alignment.

**(c) DECISIVE: block-cc is COUPLED into `tlu`.** Probed the actual lever — the post-STOP block
stall (`run_hdma_block`, base 36 SS / 68 DS + prefetch_fudge 6). Sweeping it (flag-gated, both
broad and NARROWLY gated to the `_3`/`_6` fire profile `halt_wakeup_skew && !hdma_enabled_at_halt`):
| adj | _2 | _3 | _4 | _5 | _6 |
|---|---|---|---|---|---|
| 0 | P | F | P | P | F |
| −6/−8/−10 | **F** | **P** | **F** | **F** | F |

`adj=−6..−10` fixes `_3` but BREAKS `_2`/`_4`/`_5`. Why: reducing the block stall moves `tlu` too
(`_4` tlu 150564→150548, `_2` 150560→150544, both −16 at adj=−10) — the block's cc cascades through
the STOP sequence into the DIV reset / `tima_last_update` anchor of the SAME test. So `(read_cc −
tlu)` stays coupled: shifting the block shifts BOTH read_cc AND tlu, and cannot independently move
the read across the TIMA tick edge. This holds even with the narrow gate (all five tests fire the
same block profile).

**REFINED FLOOR (sharper than m10):** R2 is NOT a "synchronous-stall-vs-interleaved-dma" problem
that per-cc interleaving fixes — the peripheral advance is ALREADY per-cc, and the read is post-block.
The true root is that **rustyboi's HDMA block cc is ENTANGLED with the timer's `tlu`/DIV anchor
through the STOP-sequence cc**, where Gambatte's scheduler keeps the block (`intevent_dma`) and the
DIV reset (`nontrivial_ff_write(0x04)`) as INDEPENDENT events at fixed cc. The fix is NOT per-cc
byte interleaving; it is **decoupling the block cc from the DIV/tlu re-anchor** — i.e. the block
must advance `cycleCounter_` without that advance feeding back into `divLastUpdate_`/`lastUpdate_`.
That is the event-scheduler's separation of concerns (each peripheral re-anchors only on its own
events, not on the block's stall lump), the MinKeeper-class architecture scoped out by the project.

**Build result:** 0 of ~9 moved (refuted: per-cc interleave is a no-op for the read; block-cost
sweep fixes one and breaks three via the tlu cascade). flag-OFF byte-identical to main_27; all
experiments reverted clean; release build clean.

**OAM-DMA bus-conflict cluster (dumpers/AGB) outlook:** those are a DIFFERENT mechanism (OAM-DMA
byte-source bus conflict, not the HDMA-block-vs-timer cc coupling). They MAY benefit from a faithful
per-cc OAM-DMA byte model, but that is orthogonal to R2's tlu-coupling root — the per-cc HDMA
interleave does NOT address them (refuted here for R2; untested but mechanistically distinct for
OAM). Do not assume a shared unlock: R2/R3 = block-cc↔tlu decoupling (scheduler); dumpers = OAM
bus-conflict capture (separate, likely the harness/sub-frame model per `oamdma-dumpers-are-harness-
floor`).

**NEXT-SESSION (if pursued):** the only real lever for R2/R3 is decoupling the HDMA block's cc
advance from the timer DIV/tlu re-anchor across the STOP — make `perform_speed_switch`'s DIV reset
anchor on a cc INDEPENDENT of the post-stop block stall (Gambatte: DIV reset at the stop `cc`, block
at its own `intevent_dma` cc, neither re-anchoring the other). This is the event-scheduler separation,
not a byte-interleave; high risk, multi-session, and may need the full scheduler. Acceptance unchanged.

### Milestone-12 (R4 + R5 carry-key audits — the LAST default lever) — BOTH confirmed DEFINITIVE FLOOR
Ran the m7 carry-key differential hunt on the final 3 default cases. Neither has a live-state key.

**R4: `late_m0int_halt_m0stat_scx3_2b` + `late_m0irq_..._2b` (dmg, out2) — sub-master-cc, NO key.**
The family `_1b`/`_2b`/`_3b`/`_4b` are 1-byte `.text` shifts (HALT position 1032→1033, read 1051→
1052) walking the halt-woken FF41 STAT read. Differential (rustyboi, DMG, the deciding read):
| sib | access_cc | ly | line_cycles | state | m0t | carry_skew | DMG wants | rb |
|---|---|---|---|---|---|---|---|---|
| _1b | 44945 | 1 | 449 | HBlank | 44751 | 0 | mode 0 | mode 0 ✓ |
| _2b | 44945 | 1 | 449 | HBlank | 44751 | 0 | **mode 2** | mode 0 ✗ |

`_1b` and `_2b` are **byte-identical in EVERY engine field** at the deciding read (access_cc=44945,
ly=1, line_cycles=449, state=HBlank, m0t=44751, ticks=449, inact_until=0, carry_skew=0) yet DMG
hardware gives OPPOSITE answers (`_1b`→mode 0, `_2b`→mode 2). The 1-byte shift is BEFORE the HALT,
so rustyboi's halt-wakeup snaps BOTH to the same resume cc (the post-halt instruction stream is
identical → same read cc). Gambatte reads them on DIFFERENT lines (`_2a` cc=71508 LY=1 mode 0;
`_2b` cc=71512 LY=2 mode 2) — the 1-byte HALT-position changes the DMG halt-wakeup by a sub-master-cc
that lands the read on opposite sides of the line wrap. **NO live-state key exists** (unlike m7's
`render_carry_skew_cc`, here every field is identical); the distinguishing info is the DMG
halt-bug sub-cc below rustyboi's master_cc resolution. Definitive floor — genuinely sub-master-cc.

**R5: `bgoff_bgon_sprite_below_window` (cgb) — needs per-pixel LCDC.0; NO closed-form proxy.**
Test: in the mid-mode-3 STAT ISR, write LCDC=0xF6 (BG off, bit0=0) then immediately LCDC=0xF7 (BG
on) — toggling BG-enable OFF then ON for a precise pixel span during pixel transfer, with a sprite
below a window. 24 pixels wrong (rustyboi #F8F8F8 white where Gambatte #9D669D). rustyboi's renderer
is `render_full_line` (RB_LINERENDER): it renders the WHOLE scanline at once at the mode-3→HBlank
transition, reading `bg_enabled = self.lcdc & 1` ONCE per line (controller.rs:2683). It has NO
per-pixel LCDC.0 history — the momentary BG-off window is invisible to a single-snapshot whole-line
render. There is NO sibling-pair / CPU-read straddle and NO closed-form proxy: the BG-off pixel
range is an arbitrary span set by the exact cc of the two LCDC writes vs the mode-3 plot position,
and on CGB BG-off = BG-master-priority off (affecting the sprite-below-window mix). Fixing it needs
the renderer to track per-pixel LCDC.0 (plot-cc), a whole-line→per-column-aware renderer change.
The m7 carry-key is structurally inapplicable (it distinguishes two CPU reads; R5 is a missing
per-pixel render feature). Definitive floor — renderer per-pixel-history rewrite.

**VERDICT — the default-suite incremental work is COMPLETE.** All reducible default cases are now
either landed (m7 m0stat) or floored with a precise mechanism:
- R1-m0stat: LANDED on main (m7, →27).
- R2 (8 HDMA tima/speedchange brackets) + R3 (oamdma speedchange stat): block-cc↔tlu coupling via
  the STOP-sequence (m8-m11) → needs the event-scheduler separation (intevent_dma independent of
  the DIV/tlu re-anchor); per-cc byte-interleave proven a no-op (m11).
- R4 (m0stat halt scx3 _2b, dmg ×2): genuinely sub-master-cc — byte-identical engine state, opposite
  DMG answers via the 1-byte halt-position sub-cc; no live-state key.
- R5 (bgoff_bgon_sprite_below_window): needs per-pixel LCDC.0 plot-cc history; whole-line renderer
  can't express it; no carry-key/proxy.

Remaining default failures = MinKeeper-class event scheduler (R2/R3), DMG halt-bug sub-master-cc
(R4), per-pixel renderer history (R5), plus the 16 harness/oracle dumpers. No further sliceable
live-state key exists in the default suite. flag-OFF byte-identical to main_27; no code changed this
milestone (audit only); release build clean.

### Milestone-13 (MinKeeper rewrite — increment 1) — BREAKTHROUGH: the m11 block-cc↔tlu "coupling" was an ARTIFACT; the block IS decoupled from tlu
User authorized pursuing the MinKeeper event-scheduler rewrite. Started with the smallest decoupling.
First step: re-pin the exact mechanism — and found the m11 conclusion was WRONG.

**The m11 "coupling" was a measurement artifact.** m11 reported that reducing the HDMA block stall
moved `tlu` (timer DIV anchor) by −16, concluding block-cc is entangled with tlu. But `RB_CANONICAL_CC`
also enabled the LEFTOVER m1 STOP-window probe (`opcodes.rs:255`), which ALSO consumed
`RB_CANONICAL_CC_ADJ` and shortened the STOP unhalt window — shifting the unhalt resume and cascading
into the DIV reset. When I removed that confounding probe and reduced ONLY the block stall:
- `[DIVRESET] abs_cc=150564` (tlu) stays **FIXED** at adj=0, −6, −8, −10 (was "moving" in m11).
- `[BLKFIRE]` and the TIMA read cc shift with the block stall, tlu unchanged.

So **the HDMA block cc is ALREADY decoupled from the timer tlu/DIV anchor** in the synchronous model
(the DIV reset anchors on `abs_cc` at the STOP, which precedes the unhalt block). m8-m11's
"block-cc↔tlu entanglement = needs MinKeeper" was a false floor. The block stall CAN be changed
independently of tlu.

**Direct fix progress (block stall, tlu fixed):** TIMA read cc-tlu with the block reduced (tlu=150564
constant throughout):
| | adj=0 | adj=−6 (block 36, faithful) | adj=−8 (block 34) | want |
|---|---|---|---|---|
| _3 | 131142 → 8196 (F7✗) | 131136 → 8196 (F7, AT edge) | 131134 → **8195 (F6✓)** | F6 |
| _4 | — | — | 131138 → **8196 (F7✓)** | F7 |
`_3`/`_4` BOTH land correctly at adj=−8 — proving they ARE splittable (the m9/m10/m11 "same-block
siblings can't split" was also downstream of the artifact). The TIMA tick edge is at cc-tlu=131136;
`_3`(131134)<edge→F6, `_4`(131138)≥edge→F7. The faithful block (36, drop the spurious +6
prefetch-fudge) puts `_3` exactly AT the edge (F7); −8 (block 34) clears it.

**REMAINING SUB-PROBLEM (the real next increment, NOT a floor):** the block-stall gate
(`halt_wakeup_skew && !hdma_enabled_at_halt`) is too BROAD — it also matches the `hdma_cycles_*_2`
and `frame*_ly_count`/`m2irq_count` calibration blocks (which are halt-woken but NOT post-STOP and
DO need the +6 prefetch-fudge for their downstream STAT read). A blanket −8 broke 37. The +6
`prefetch_fudge` is a CPU-stall artifact (Gambatte's `Interrupter::prefetch` absorbed into the
synchronous block) that is FAITHFUL for a STAT-read-downstream block but SPURIOUS for a
TIMA-read-downstream block (`_3`). The synchronous model can't make the block cost depend on the
downstream read type — which IS the MinKeeper separation: the block should be an event at the pure
transfer cc (36), and the prefetch (+6) a SEPARATE CPU event, not folded into the block stall.

**INCREMENT-1 RESULT:** the architectural premise is corrected and sharpened — block-cc/tlu are
already decoupled; `_3`/`_4` are splittable; the true remaining work is **separating the +6
CPU-prefetch-fudge from the block transfer cc** (make the block a fixed transfer-cc event and the
prefetch its own event), so the block cost stops depending on the downstream read context. That is a
focused, real next increment (not the full MinKeeper). No code landed yet (the broad knob breaks the
calibration tests); flag-OFF byte-identical to main_27; experiments reverted clean.

**NEXT-SESSION increment-2:** at the post-STOP-unhalt block fire, charge the pure transfer cc (36 SS
/ 68 DS) and route the +6 prefetch-fudge through the CPU-prefetch path INSTEAD of the block stall
(or gate the fudge on the downstream-read-is-STAT context). Acceptance: `_3`→F6, `_4`→F7 (proven
reachable at block 34/−8), `_6`→F9, `_2`/`_5` intact, AND `hdma_cycles_*_2`/`frame*_count` stay
passing (the fudge preserved for their STAT-read path). Then R3 + R4 re-check under the event-exact
block cc.

### Milestone-14 (MinKeeper increment 2 — prefetch-fudge separation) — LANDED: `hdma_late_m3speedchange_tima_scx1_ds_3` FIXED (flag-ON net −1, broke 0). MAIN-MERGE CANDIDATE.
Rebased onto main@0b5d8ff (=26; R5/bgoff per-pixel LCDC.0 landed). Clean rebase, no conflicts.
flag-OFF re-verified byte-identical to **main_26** (net +0, broke 0). New gate baseline = main_26.

**THE FIX (`mmio.rs::run_hdma_block_inner` stall):** a post-STOP-unhalt HDMA block — Gambatte's
prefetched `hdma_requested` fired at the speed-switch unhalt — charges ONLY the pure transfer cc
`16 * (2 + 2*ds)` (= 32 SS / 64 DS), with NEITHER the trailing `+4` NOR the +6 CPU-prefetch fudge.
Those two are CPU-prefetch artifacts faithful only for a STAT/LY-read-DOWNSTREAM block (the
`hdma_cycles`/`frame*_count` calibration tests, whose value-read is the immediate post-block read);
the Requested block's downstream value-read is a TIMA read several instructions later
(`hdma_late_m3speedchange_tima`), so the fudge pinned it 1 TIMA tick high. **Discriminator:
`halt_hdma_state == HaltHdmaState::Requested`** — set ONLY by the STOP speed-switch
prefetched-block path; the plain halt-woken calibration blocks are `Low` and keep `base + fudge`.

**Also removed two refuted dead-end probes** (m1 STOP-window unhalt-cycles in `opcodes.rs`, m2
getLyReg `to_next` in `controller.rs`) that shared `RB_CANONICAL_CC_ADJ` and CONFOUNDED every
block-stall measurement — the FALSE "block-cc↔tlu coupling" of m8-m11 was their artifact. Both were
flag-gated and refuted; flag-OFF unaffected by their removal.

**cctracer proof (`_3`):** Gambatte reads TIMA=**0xF6** (`[INSTR] cc=340104 pc=0x7000 a=0xF6`);
faithful block 32 gives rustyboi cc-tlu = **131132 == Gambatte exactly** → ticks 8195 → F6. The old
36+6 stall landed 131142 → 8196 → F7.

**VALIDATION (full suite, main_26):**
- flag-OFF: byte-identical to main_26 (net +0, broke 0).
- flag-ON: **net −1, fixed `hdma_late_m3speedchange_tima_scx1_ds_3`, broke 0.**
- ENV-FREE (guard dropped): same — net −1, broke 0 → main 26→25.
- Acceptance: `_2`→F4, `_3`→F6, `_4`→F7, `_5`→F8 PASS; `_6` FAIL (separate sub-case, below).
  Calibration STAY PASSING: `hdma_cycles_2`, `hdma_cycles_ds_2`, `frame0_ly_count_1`,
  `frame0_m2irq_count_1`, `frame1_ly_count_ds_1` — the `Requested` gate spares them. broke-0 across
  the whole hdma/gdma/oamdma/dma/speedchange/tima/div/halt family.

**Which of R2(8)/R3/R4 moved:** 1 — `hdma_late_m3speedchange_tima_scx1_ds_3` (R2). `_6` did NOT move
(`halt_hdma_state == Low`, NOT Requested — it fires via a different path and needs the OPPOSITE
correction, read 8197→8198). R3 (`oamdma_late_speedchange_stat_2`) and R4 (the dmg halt m0stat ×2)
did NOT move — they are not the post-STOP-unhalt-block TIMA-read mechanism (R4 is the m12 DMG
halt-wakeup sub-master-cc floor; R3 is the m4b HBlank-state m0Time phase).

**MAIN-MERGE CANDIDATE — exact env-free diff** (drop the `canonical_cc_enabled()` guard; the
`Requested` state is real engine state so it is faithful unconditionally):
```rust
// in run_hdma_block_inner, replacing `base + prefetch_fudge` as the stall when Requested:
if matches!(self.halt_hdma_state, HaltHdmaState::Requested) {
    return 16 * (2 + 2 * self.is_double_speed_mode() as u32);
}
base + prefetch_fudge
```
Plus the two dead-probe removals (opcodes.rs m1 unhalt-cycles block, controller.rs m2 getLyReg
`to_next` block) — both already deleted on-branch, both flag-gated/refuted (no behavior change).
Verified env-free full suite = main_25, broke 0.

**NEXT (`_6`, the Low-state sub-case):** `_6` fires its block via the `Low`/per-dot path (not the
Requested prefetched path) and needs the read 1 tick LATER (8197→8198) — the OPPOSITE of `_3`. The
faithful question: does the `Low`-at-unhalt block ALSO drop the +6 fudge (a separate gate), or is
`_6`'s residual elsewhere (the unhalt-reflag cc)? Audit `_6`'s block fire path + its read cc-tlu vs
Gambatte. Then the remaining 6 HDMA brackets (`hdma_late_ei`, `hdma_m0speedchange`,
`hdma_transition` ×4) are EI-service / m3wakeup / multi-block mechanisms — separate audits.

### Milestone-15 (R2 continuation: `_6` root + bracket survey) — `_6` needs the tlu/STOP-anchor lever (NOT a block slice); other 6 brackets are distinct mechanisms
Rebased onto main@5b125c6 (=25; m14 Requested-block fix landed env-free). Dropped the m14
flag-gated dup (took main's unconditional version). flag-OFF re-verified byte-identical to **main_25**
(net +0, broke 0).

**`_6` (`hdma_late_m3speedchange_tima_scx1_ds_6`) — ROOT FOUND, but it is the tlu/STOP-anchor, not
a block-cost slice.** Differential (`_5` passes F8, `_6` wants F9, +1 NOP apart, both `halt_hdma_state
== Low`):
- `_5`: read cc-tlu=131158 → 8197 (F8 ✓); `_6`: cc-tlu=131162 → 8197 (wants 8198/F9).
- The TIMA tick edge is at cc-tlu=131168 (8198*16). Both reads sit below it; rustyboi's edge is
  ~6cc too HIGH. Gambatte's `_5`/`_6` reads are ALSO +4 apart (push-af 340128→340132) but Gambatte's
  edge falls BETWEEN them — i.e. **rustyboi's `tlu` is ~6cc too high**, putting the edge above both.
- **Found a clean discriminator** (`halt_hdma_state == Low && key1_switch_armed`) that separates
  `_6`'s pre-STOP block from the identical-context `hdma_cycles_ds_2` calibration block (which has
  `key1_armed=false`). BUT dropping the block's +6 fudge moved the READ 6cc EARLIER (cc-tlu
  131162→131156) — the WRONG direction. `_6` needs cc-tlu HIGHER (read later / tlu lower), and the
  block fires too late to feed the DIV reset (block@150565, DIV-reset@150568, 3cc apart; the block's
  stall drains as `pending_dma_stall` AFTER, not before the reset). So the block-cost lever cannot
  fix `_6`.
- `_6`'s real lever is **lowering `tlu` by 6** — i.e. the STOP DIV-reset anchor (`stop_div_reset` →
  `tlu = abs_cc` at the STOP). That requires abs_cc at the STOP to be 6 lower = the STOP-window /
  DS→SS-bridge cc, which is SHARED with many passing speedchange tests (the deferred high-risk bridge
  lever from m6/m9). `_6` is NOT a block-local slice like `_3`; it is the bridge-cc anchor. Deferred.

**Other 6 brackets — distinct mechanisms (surveyed, none is the `_3` block-cost slice):**
- `hdma_m0speedchange_late_m3wakeup_scx1_2` / `scx2_2` (out00): m3-wakeup reflag, FF55/screen read.
- `hdma_transition_ei_halt_late_unhalt_ldaaimm_hdma_scx1_1` / `_2` / `hdma_transition_halt_late_unhalt
  _ldaaimm_hdma_scx1_1` (out00/02): EI-service / multi-block unhalt (FF55/PC reads).
- `hdma_late_ei_m3halt_m2unhalt_pc_scx1_2` (outAD): m2-unhalt EI, PC read.
These read FF55/PC/screen at the unhalt, not a TIMA tick — their residual is the unhalt-reflag /
m2-service cc (the `hdma-unhalt-bracket-floor` memory note), not the post-block transfer cc. Each
needs its own audit; they do NOT share `_3`'s clean `Requested`-block discriminator.

**SESSION RESULT:** `_3` (m14) is the clean extractable R2 win (on main, 26→25). `_6` root pinned to
the tlu/STOP-anchor (bridge-cc, deferred high-risk). The other 6 brackets are unhalt-service-cc
mechanisms (separate). No new clean block-context slice found this session beyond `_3`. flag-OFF
byte-identical to main_25; no flag-ON change landed (the `_6` block lever was the wrong direction,
reverted). The remaining R2 brackets converge on the STOP-window/bridge cc + unhalt-service cc —
the levers the campaign has repeatedly flagged as shared/high-risk, now confirmed for `_6` too.

### Milestone-16 (dedicated audit of the 6 unhalt-service-cc brackets) — grouped; B = definitive byte-identical floor, A = HALT-bug resume-PC, C = m2-service PC
Applied the `_3` per-context-discrimination rigor to all 6. None is a clean block-cost slice; the
three groups are distinct CPU/DMA-state mechanisms:

**Group A — `hdma_transition_*_ldaaimm` ×3 (out00/02): HALT-bug / late-unhalt RESUME-PC accounting.**
Test: TIMA int → HDMA (FF55=81, 2 blocks) → HALT → late unhalt → `ld a,(0080)` (3-byte ldaaimm) →
print. The pass/fail keys on WHICH instruction the unhalt resumes at. cctracer vs engine
(`hdma_transition_halt_late_unhalt_ldaaimm_hdma_scx1_1`, FAIL out00):
- Gambatte resumes at **pc=0x1187 opcode=0xFA** (runs the ldaaimm once), then 0x1189.
- rustyboi resumes at **pc=0x1189** (SKIPS the 0xFA at 0x1188 — runs the operand byte as an opcode).
The HDMA blocks are byte-identical between siblings (same fires, same `len_after`); the divergence
is the **resume PC after the late unhalt** — rustyboi's HALT-bug/late-unhalt prefetch lands the PC
1-2 bytes off for the multi-byte resume instruction. This is a concrete CPU-level bug (NOT a
byte-identical floor) but in the HALT-bug prefetch/double-execute path (`opcodes.rs::halt`,
`sm83.rs` unhalt) shared by every HALT test — high-risk, needs a focused HALT-bug-PC audit, not the
block-cost lever.

**Group B — `hdma_m0speedchange_late_m3wakeup_scx{1,2}_2` ×2 (out00): DEFINITIVE BYTE-IDENTICAL FLOOR.**
Test: ISR → HDMA (FF55=81, 2 blocks) → STOP speed-switch → NOPs → read FF55 → print. `_1`(outFF,pass)
vs `_2`(out00,FAIL) differ by 1 `.text` byte (1 NOP). At the deciding FF55 read, rustyboi state is
**byte-identical** between `_1` and `_2`:
| | read cc | FF55 val | hlen | hen | hreq | halt_hdma_state | in_stop |
|---|---|---|---|---|---|---|---|
| _1 (pass FF) | 174792 | FF | 127 | false | false | Low | false |
| _2 (FAIL→00) | 174796 | **FF** | 127 | false | false | Low | false |
Both fire BOTH blocks (transfer wraps, FF55=FF); the only difference is the read cc (+4). Gambatte's
`_2` has the 2nd block NOT yet completed at the read (FF55=00) — its 2nd-block fire / STOP-freeze
lands the read on the other side of the transfer-complete boundary. rustyboi's 2nd block fires far
too early (cc ~43626, vs the read at ~174796), identically for both siblings, so there is **NO
live-state key** distinguishing `_2` from `_1` at the read. Like R4: byte-identical engine state,
opposite answers — a genuine sub-master-cc (here, STOP-freeze-vs-2nd-block-fire) floor. The 2nd-block
fire cc is the same for both → not sliceable.

**Group C — `hdma_late_ei_m3halt_m2unhalt_pc_scx1_2` ×1 (outAD): m2-service-vs-HDMA-unhalt PC bracket.**
The `pc` variant prints the PC pushed by the m2 (STAT) interrupt service during the HALT. `_1`(AC,pass)
/ `_2`(AD,FAIL) / `_3`(AE,pass) are CONSECUTIVE PC values — `_2` lands on the wrong side of the
m2-service-vs-HDMA-block push ordering (the `hdma-unhalt-bracket-floor` 1-cc bracket: whether the
HDMA block fires before or after the m2 ISR's PC push). The pushed-PC depends on the exact m2-service
cc vs the block m0-edge — a sub-master-cc ordering, indistinguishable at the engine level (the
`ly`-variant siblings `_1.._6` all PASS, so the dispatch is right; only the `pc` push-cc straddle
fails). Sub-master-cc m2-service floor.

**VERDICT (per group):**
- **A (3 cases): a real HALT-bug late-unhalt RESUME-PC bug** — fixable in principle but in the
  high-risk HALT-bug prefetch path; needs a dedicated HALT-bug-PC audit (resume PC for a multi-byte
  instruction after a non-bug late unhalt). NOT a floor, NOT a block slice. Deferred to a focused
  HALT-bug session.
- **B (2 cases): DEFINITIVE byte-identical floor** — rustyboi's 2nd-block-fire/STOP-freeze cc is the
  same for the failing and passing siblings; the FF55 read lands on opposite sides of the
  transfer-complete boundary with no distinguishing engine state. Sub-master-cc; like R4.
- **C (1 case): sub-master-cc m2-service-PC floor** — the pushed PC straddles the m2-service-vs-block
  push ordering by 1cc, indistinguishable at the engine level (the dispatch itself is correct — only
  the push-cc straddle).

No clean per-context slice (like `_3`) exists in these 6. `_3` was special: the discriminator
(`halt_hdma_state == Requested`) was a real engine-state flag AND the lever (block transfer cc) was
block-local. The 6 here are either sub-master-cc floors (B, C — byte-identical/1-cc-straddle) or a
HALT-bug PC bug (A — concrete but high-risk CPU path). flag-OFF byte-identical to main_25; audit
only, no code changed.

**R2/R3/R4 final tally:** R2 = 1 landed (`_3`, on main), `_6` = bridge-cc tlu (m15), 3 = HALT-bug PC
(A), 2 = STOP-freeze FF55 floor (B), 1 = m2-service PC floor (C). R3 = m4b m0Time phase. R4 = m12 DMG
halt-wakeup floor. The remaining default-suite work is: the HALT-bug-PC audit (group A, 3 cases) and
the bridge-cc/sub-master-cc deep levers (everything else).

### Milestone-17 (group-A deep dive) — CORRECTION: group A is NOT a resume-PC bug; it is the HDMA-transfer-vs-resume-VRAM-read timing (same class as B)
Built and tested the m16-hypothesized resume-PC fix (advance pc past the HALT-Requested-prefetched
opcode so a multi-byte `ld a,(imm16)` reads operands from pc+1). **It REFUTED the m16 "resume-PC bug"
reading.** The fix made `_1` read addr **0x0080** (a=01, ROM) — but that is WRONG; the test's
HALT-bug double-read is SUPPOSED to read **0x80FA** (VRAM).

**The truth (cctracer + engine):** for `hdma_transition_halt_late_unhalt_ldaaimm_hdma_scx1_1` (FAIL):
- Gambatte: HALT@0x1186 → resume@0x1187 opcode=0xFA (the HALT-bug keeps pc at 0x1187, so the FA
  reads its operand bytes as `mem(1187)=FA, mem(1188)=80` → **addr 0x80FA**), result **a=0x02**.
- rustyboi flag-OFF: resume@0x1187, FA reads **addr 0x80FA**, result **a=0xFF**.
- The passing sibling `_2` (out02): rustyboi reads 0x80FA → a=0x02 → PASSES.
So **rustyboi's PC handling is already correct** (pc-not-advanced → the HALT-bug double-read of
0x80FA, matching Gambatte). My m16 "rustyboi resumes at 0x1189" was the POST-FA pc; the FA does run
at 0x1187. The pc-advance fix was the WRONG direction (it made `_1` read ROM 0x0080).

**The actual residual:** the resume `ld a,(0x80FA)` reads VRAM where the HDMA just transferred 0x02,
but rustyboi's VRAM is still LOCKED (returns 0xFF) at the resume read cc, where Gambatte's is unlocked
and returns the transferred byte 0x02. This is the **HDMA-block-transfer-vs-resume-VRAM-read timing** —
the resume read's VRAM-lock/transfer-completion cc, NOT a PC bug. It is the SAME transfer-timing class
as group B (the FF55-readback floor): rustyboi's HDMA block transfer / VRAM-unlock cc relative to the
resume read is off, and the `_1`/`_2` siblings (1 `.text` byte apart) straddle the lock/transfer
boundary. The `ei_halt_late_unhalt` variants read 0x80EA → FF (same: VRAM locked) where Gambatte has
the transferred byte.

**CORRECTED VERDICT for all 6 unhalt-service brackets:** none is a clean PC bug. All three groups are
**transfer-timing / sub-master-cc**:
- A (3): HDMA-transfer-vs-resume-VRAM-read value (resume reads locked-FF VRAM vs Gambatte's
  transferred byte) — the VRAM-unlock/transfer-completion cc at the resume read.
- B (2): STOP-freeze-vs-2nd-block-fire FF55 readback (byte-identical engine state, floor).
- C (1): m2-service-vs-block-push PC straddle (1cc, floor).
All converge on the **HDMA block transfer cc / VRAM-unlock cc vs the read cc** at the unhalt — the
same synchronous-block-vs-event-scheduler issue (m10/m11), now confirmed to also govern the VRAM-read
value at the resume (not just the timer tlu). `_3` was fixable because its read was a TIMA tick (cc
only); these read VRAM/FF55/PC where the block's transfer-completion/lock cc (not just the read cc)
matters — needs the event-interleaved `dma()` (the deferred MinKeeper transfer model), not a
per-context block-cost slice. No clean slice exists. flag-OFF byte-identical to main_25; the
resume-PC fix was reverted (wrong direction); audit only, no code landed.

### Milestone-18 (DISAMBIGUATION) — VERDICT: it is the dma-transfer-completion cc, NOT the PPU-mode phase. The 6 brackets need the event-interleaved `dma()` build (deep). R3 is a SEPARATE m0Time-phase root.
The decisive open question from m17: is the resume-read 0xFF the PPU-mode-at-cc phase (tractable
getStat-style slice) or the dma-transfer-completion cc (deep)? **Traced both at the resume read of
`hdma_transition_halt_late_unhalt_ldaaimm_hdma_scx1_1` (`ld a,(0x80FA)`, FAIL want 0x02):**

| | resume read cc | PPU locked? | raw VRAM byte @ 0x80FA | block fires @ | byte commits @ |
|---|---|---|---|---|---|
| _1 (FAIL) | 12310 | **true** | **0x00** | 12332 | 12334 |
| _2 (pass) | 12394 | false | **0x02** | 12332 | 12334 |

**DECISIVE: the HDMA block fires (12332) and commits 0x80FA (12334) AFTER `_1`'s resume read (12310).**
At `_1`'s read the transferred byte 0x02 is NOT in VRAM (raw=0x00) — so even unlocking the PPU mode
would yield 0x00, not the wanted 0x02. The PPU-mode-phase fix is IRRELEVANT here. The block fire/commit
cc is IDENTICAL (12332/12334) for `_1` and `_2`; the only difference is the resume read cc (the 1-byte
`.text` shift moves `_1`'s read 84cc earlier, BEFORE the block fires). Gambatte's `intevent_dma` fires
DURING the halt window (before the unhalt resume), so the byte is already in VRAM at the resume read;
rustyboi's synchronous block fires at the UNHALT (after the resume instruction already read), 22cc too
late. **This is the dma-transfer-completion / block-fire-during-halt cc** — confirmed, not the PPU mode.

**Verdict for the 6 unhalt-service brackets:**
- **Group A (3) + Group B (2): the dma-transfer-completion cc** — rustyboi fires the HDMA block at the
  unhalt (after the resume read / FF55 read), where Gambatte fires `intevent_dma` DURING the halt window
  so the transfer is complete before the resume reads it. The resume VRAM/FF55 read sees stale state.
  NO PPU-mode slice exists (the transferred byte/length isn't updated yet). These need the
  **event-interleaved `dma()` build**: the block must fire as a scheduled event DURING the 0x20000 halt
  window (before `intevent_unhalt` resumes the CPU), so its writes/length-decrement land before the
  resume read. This is the deferred MinKeeper-class transfer model (m10/m11/m17), now PROVEN to be the
  precise lever for these 5 cases — not a getStat phase, not a block-cost constant.
- **Group C (1): m2-service-vs-block-push PC straddle** — 1cc sub-master-cc floor (unchanged).

**R3 re-check under the same lens — SEPARATE root, NOT shared.** `oamdma_late_speedchange_stat_2` reads
**FF41 (STAT mode)** at `.text@10df` (`ldff a,(41)`), NOT VRAM or a transferred byte — there is no HDMA
block-completion involved. R3 is purely the **PPU-mode-at-cc / m0Time phase** across the speed switch
(m4b's −18-vs-+4 m0Time = the SS→DS bridge-cc phase). It does NOT share the dma-transfer-completion root
of A/B. R3 stays a getStat/m0Time-phase case (the deferred bridge-cc lever, same as `_6`'s tlu).

**CONSOLIDATED remaining-default roots (3 distinct deep levers):**
1. **event-interleaved `dma()`** (block fires during the halt window): R2 group A (3) + B (2) = 5 cases.
2. **post-DS→SS bridge-cc / m0Time-tlu phase**: R2 `_6` (tlu) + R3 (m0Time STAT read) = 2 cases.
3. **sub-master-cc floors** (byte-identical / 1cc straddle): R2 group C (1) + R4 (2 dmg) = 3 cases.
No clean per-context slice remains; `_3` (m14, landed) was the only block-local one. flag-OFF
byte-identical to main_25; disambiguation audit only, no code landed.

### Milestone-19 (BUILD attempt: during-halt block fire) — CORRECTS m18: it is the PPU-mode/m0Time phase (R3 family), NOT transfer-completion; and the m0Time/read-cc is co-tuned (no clean slice)
Built the during-halt Requested-block fire (m18's prescription: fire the multi-block transfer during
the halt window so the byte is in VRAM before the resume read). It worked mechanically (block fired at
12296, byte committed, resume read got the transferred byte) — but the test STILL FAILED, which
exposed that **m18's verdict was WRONG.**

**The m18 error: cctracer's a=0x02 was a NON-CGB-oracle value.** The CGB oracle for
`hdma_transition_halt_late_unhalt_ldaaimm_hdma_scx1_1` is **out00** (want 0x00), NOT 0x02. So `_1`'s
resume `ld a,(0x80FA)` should read **0x00**, not the transferred byte. flag-OFF rustyboi: the raw VRAM
byte at 0x80FA IS already 0x00 (the transfer hasn't reached it / it's pre-transfer), but rustyboi
returns **0xFF because the PPU is mode-3-LOCKED** at the read. **The fix is to UNLOCK the read (mode 0
→ returns the raw 0x00), NOT to fire the block early.** This is the PPU-mode-at-cc phase — m18's
"dma-transfer-completion" reading is REFUTED; the during-halt fire is the wrong lever (it made the
read see the transferred byte where the test wants the pre-transfer 0x00).

**The lock decision (traced):** `_1`'s resume VRAM read at cc=12310 resolves `get_stat=Some(3)`
(PixelTransfer/locked) with `m0_time_master = 12333` (mode-3→0 boundary). `ended = cc_end+2 >= m0t`
→ `12313 < 12333` → not ended → locked → returns 0xFF. Gambatte has this read in **mode 0** (unlocked,
returns 0x00 = out00). So **rustyboi's m0Time (12333) is ~20cc too LATE** at this halt-woken read — the
mode-3 window over-extends. The passing sibling `_2` reads 84cc later (cc=12394, get_stat=mode 0,
unlocked, returns 0x02). The `+6` halt-woken VRAM read-cc bias (`vram_read_cc = pre_cc + 6`) is already
applied; `_1` needs ~+20 more to land past m0Time.

**Why it's NOT a clean slice (the co-tuning):** sweeping the halt-woken VRAM read-cc bias — adj=+20
fixes `_1` (mode-3→0 crossed) but **broke 6** on the full suite: `hdma_late_disable/enable_ds`,
`hdma_late_enable_ds_lcdoffset1`, `oamdmasrc80_halt_m2irq_read8000` (dmg+cgb). These are OTHER
halt-woken VRAM reads calibrated to the existing `+6` bias / the shared `m0_time_master`. The
read-cc-vs-m0Time phase is **co-tuned across the halt+VRAM family** — exactly the mixed-anchors wall.
The +20 is a tuned constant, not a faithful derivation, and it only fixed 1 of the 3 group-A cases
(the EI variants read via a different IME-on service path). REVERTED.

**CORRECTED VERDICT — group A (and B) is the PPU-mode-at-resume-cc / m0Time phase, R3's family:**
- It is NOT the dma-transfer-completion cc (the transferred byte presence is irrelevant; the byte at
  the read is the right value, the LOCK is wrong). m18's during-halt-fire build is the wrong lever.
- It is the **m0Time / mode-3→0 boundary at the halt-woken read** being ~20cc too late vs Gambatte —
  the SAME shared `m0_time_master` / read-cc-bias the whole halt+VRAM family is co-tuned to. Shifting
  it (the only sliceable knob, the read-cc bias) breaks the calibrated siblings. This is the deferred
  **m0Time-phase / bridge-cc lever** (R3, `_6`'s tlu) — confirmed to also govern these VRAM-read
  brackets, NOT the event-interleaved `dma()`.

**Consolidated remaining-default roots (REVISED after m19):** the 5 brackets (A 3 + B 2) are NOT the
event-interleaved `dma()` — they are the **m0Time/getStat-phase at the halt-woken read** (the read
locks mode-3 where Gambatte has mode-0), the SAME root as R3 and `_6`'s tlu. So the consolidated map
is now just TWO deep levers:
1. **post-DS→SS / halt-woken m0Time-phase + read-cc** (the shared `m0_time_master` / VRAM-readable /
   getStat boundary, co-tuned across the halt+VRAM+speedchange family): R2 group A (3) + B (2) + `_6`
   + R3 = **7 cases**. The only knob (read-cc bias) is co-tuned → needs the faithful m0Time re-derivation
   (the deferred bridge-cc/m3-length build), not a constant.
2. **sub-master-cc floors** (byte-identical / 1cc straddle): R2 group C (1) + R4 (2) = **3 cases**.
m18's "event-interleaved `dma()`" lever is RETRACTED — proven the wrong root by this build. flag-OFF
byte-identical to main_25; the during-halt-fire and read-cc-bias experiments both reverted; no code
landed.

### Milestone-20 (m0Time re-derivation attempt) — ROOT NAILED + m19 RE-CORRECTED: the m0Time is FAITHFUL; the PPU is UNDER-ADVANCED because the HDMA block stall does not tick the PPU in lockstep. The fix IS the event-interleaved `dma()` (m18's lever re-instated).
Tried to re-derive the halt-woken `m0_time_master` faithfully (m19's prescription). The diagnostic
REFUTED m19's "m0Time too late" framing and nailed the true mechanism.

**Step-by-step chronology for `hdma_transition_halt_late_unhalt_ldaaimm_hdma_scx1_1` (`_1`, FAIL):**
- cc=12296: the late interrupt becomes pending → unhalt. The FA (`ld a,(0x80FA)`) resume begins.
- cc=12297: HDMA **block 1** fires via the m0-edge (mid-FA, during its operand tick), dest 0x80E0,
  queues a **36cc transfer stall** into `pending_dma_stall`.
- cc=12310: the FA reads **0x80FA** — `ppu_ticks=228` (lineCycle 228), **`stall_pending=36`** (block 1's
  stall is STILL queued, has NOT advanced the PPU). getStat→mode 3 → locked → 0xFF.
- cc=12312: the NEXT `step()` finally takes the 36cc stall and ticks the PPU — but the FA already read.
- cc=12332: block 2 fires (writes 0x80FA=0x02), irrelevant — the read already happened.

Gambatte at the resume read: **lineCycle 284, LY=2** (deep in mode 0, unlocked → reads the
pre-transfer 0x00 = out00). rustyboi: **lineCycle 228** — **56 dots behind**, because block 1's
36cc transfer cycles have NOT advanced the PPU before the same-instruction read.

**THE TRUE ROOT (re-corrects both m18 and m19):** rustyboi charges the HDMA block's transfer cc as a
SEPARATE CPU stall (`pending_dma_stall`) that the PPU catches up on at a LATER `step()` — it does NOT
advance the PPU in lockstep with the transfer. Gambatte's `intevent_dma` advances ALL peripherals
(incl. the PPU) through the transfer cc as it runs, so by the time the resume instruction reads, the
PPU line is already extended. The `m0_time_master` (12333) is FAITHFUL for the PPU's ACTUAL (un-
advanced, lineCycle-228) position — it is NOT too late; the **PPU is under-advanced**. m19's "re-derive
m0Time" is the wrong fix; m18's **event-interleaved `dma()` IS the right lever** (re-instated) — but
not the byte-commit timing (m18's error): the block's TRANSFER CYCLES must advance the PPU (and timer)
mid-instruction, between the m0-edge fire and the subsequent same-instruction read.

**Why a pre-resume stall drain doesn't work (tested):** I added an `unhalt_drain_stall` to tick the
pending stall into the PPU before the resume `execute`. At that point (cc=12296, op=FA) the
**pending_stall=0** — block 1 fires at 12297, DURING the FA's tick, AFTER the drain point. So the stall
isn't queued yet at any pre-execute hook. The block fires mid-instruction (at its m0-edge dot), and its
cc must advance the PPU right there, in the per-dot crank — i.e. `step_hdma` must apply the block's
transfer cc to the PPU/timer in lockstep, not queue it as a deferred CPU stall. That is the
event-interleaved transfer model (the `resolve_one_dot` loop would advance the PPU through the block's
36 dots when the block fires), a structural change to `run_hdma_block`/`step_hdma`/`pending_dma_stall`.
Reverted (no clean hook exists at the CPU-step granularity).

**DEFINITIVE VERDICT for the 5 brackets (A 3 + B 2):** they need the **event-interleaved HDMA `dma()`**:
when a block fires (m0-edge or unhalt), its transfer cc must advance the PPU/timer dot-by-dot IN
LOCKSTEP (so a same-instruction or near read sees the extended line), instead of being queued as a
`pending_dma_stall` the PPU catches up on later. This is the deferred MinKeeper-class transfer model
(m10/m11), now PROVEN at dot precision to be the irreducible root — NOT the m0Time derivation (faithful
already), NOT a read-cc bias (co-tuned), NOT the byte-commit timing. The blast radius is the entire
HDMA/GDMA stall path (every transfer test), so it is a dedicated deep build.

`_6` (tlu) and R3 (FF41 m0Time phase) are SEPARATE (the post-DS→SS bridge-cc / m0Time, not the
block-stall-PPU-lockstep): the consolidated map is now THREE roots: (1) **event-interleaved `dma()`**
= A(3)+B(2) = 5; (2) **post-DS→SS bridge-cc/m0Time** = `_6`+R3 = 2; (3) **sub-master-cc floors** =
C(1)+R4(2) = 3. flag-OFF byte-identical to main_25; the m0Time-re-derivation and pre-resume-drain
experiments both reverted; no code landed.

---

## m21 — event-interleaved HDMA lockstep BUILT; residual is the scx1 first-tile renderer phase (NOT block-transfer cc)

**Built (flag-gated, RB_CANONICAL_CC):** the event-interleaved HDMA block transfer. When a block fires
in the per-dot crank (`step_hdma` inside `resolve_one_dot`), `run_to_min_event` (bus.rs) now detects the
just-queued `pending_dma_stall` delta and advances the world (PPU + timer) dot-by-dot through that
transfer cc IN LOCKSTEP at the fire point — `reduce_dma_stall(delta)` then loop `resolve_one_dot()`
`delta` times under `hdma_lockstep_active` (which suppresses `step_hdma` re-arm during the advance).
This is exactly Gambatte's `intevent_dma` (advance all peripherals through the transfer cc), not the
deferred `pending_dma_stall` the PPU caught up on later.

**Gate (`hdma_resume_lockstep_window`):** armed at unhalt ONLY for the Requested-context multi-block
transfer (`HaltHdmaState::Requested && !ime && hdma_length() != 0` — the IME-off HALT-bug resume whose
first block is gated OFF from the inline-fire at sm83.rs:217 and so fires on its m0-edge DURING the
resume instruction). Cleared when the resume instruction completes. WITHOUT this gate (lockstep ALL
fired blocks) the full suite regressed +28 (broke 30 normal m0-edge / GDMA-calibration / late_hdma_vs_*
blocks, fixed 2) — confirming the lockstep is correct ONLY for the halt-resume block; normal blocks keep
the proven deferred-stall path. WITH the `!ime` gate: **flag-ON net +0, broke 0; flag-OFF byte-identical
to main_25.**

**Why it lands net-0 (the real residual, MEASURED):** the lockstep IS faithful and demonstrably
corrects the block-transfer/read timing — for group A `hdma_transition_halt_late_unhalt_ldaaimm_hdma_scx1_1`
(out00) the render mismatch moves from **tile 0** (OFF: 15 px, bounds x=7..7 — first digit wrong) to
**tile 1** (ON: 9 px, bounds x=9..15 — first digit now CORRECT, second digit wrong). The lockstep fixes
the FIRST tile; the residual is the SECOND tile / low-nibble digit under scx1. Group B
`hdma_late_ei_m3halt_m2unhalt_pc_scx1_2` (outAD) mismatches identically at **tile 1 (D)** — high nibble
'A' correct, low nibble 'D' wrong. ALL 5 brackets are `scx1`/`scx2` cases; the irreducible residual is
the **scx first-tile mode-3 render phase** (the deferred mode-3-length/m0Time renderer rebase noted in
[[scx-during-m3-plus2cgb]] / [[m3len-is-cpu-phase-not-renderer]]), NOT the block-transfer cc (now
faithful) and NOT a read-cc bias.

**VERDICT:** the m21 task's premise ("the block-transfer lockstep is the lever for the 5 brackets") is
HALF-confirmed: the lockstep is the necessary, correct mechanism and it fixes the data-read timing
(first tile), but it CANNOT land the 5 brackets alone — each is blocked by the scx first-tile renderer
phase residual. Without the gate the lockstep 1-for-1 swaps render phases (the no-`!ime` run: fixed
`ei_..._ldaaimm_scx1_2`, broke `ei_..._scx1_1` — a pure scx render-phase swap). The `!ime`-gated
lockstep is committed as the faithful prerequisite mechanism (net-0/broke-0/flag-OFF-identical); the 5
brackets now require the coupled **scx-first-tile mode-3 render rebase** as the FINAL co-land, on top of
this lockstep. Three-root map updated: (1) event-interleaved `dma()` lockstep — **BUILT (m21)**, gated
behind the renderer rebase; (2) post-DS→SS bridge-cc/m0Time = `_6`+R3 = 2; (3) sub-master-cc floors =
C(1)+R4(2) = 3. The 5 brackets fold into a new root (4): **scx first-tile mode-3 render phase** (couples
with the m21 lockstep).

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

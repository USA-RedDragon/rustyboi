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
- (init) Plan written. Worktree confirmed flag-OFF == main_31 (31, byte-identical).

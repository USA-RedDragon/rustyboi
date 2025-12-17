# Per-access exact-event-cc engine — the final timing-core build (Stage-0 scoping)

Goal: drive the gambatte hwtests suite from main@86 toward the floor (~4 real bugs
+ ~19 oamdma/`_dumper` harness-floor). The remaining ~50 reducible failures ALL
reduce to ONE root the per-cluster agents proved exhaustively across the campaign:
even though `master_cc`/`abs_cc` is now byte-exact (DS-sub-dot engine, Lever A), the
**per-access / within-M-cycle / instruction-boundary cc still rounds to the dot grid**.
`run_to` cranks one dot at a time (`resolve_one_dot` per dot + the DS parity-gate at
`cpu/bus.rs:resolve_one_dot`), peripherals advance in whole dots inside `tick_m`, and
the STOP bridge re-anchors the renderer in whole dots — so a CPU access and a PPU
fetch that happen WITHIN the same M-cycle cannot be ordered at their true sub-dot cc.

`cpu/bus.rs:~26-29` states this explicitly: "`run_to`'s per-cc primitive still
resolves one dot at a time internally — the true min-event jump and the
step_subdot/parity-gate removal are deferred until the mid-M3 per-column render is
finished."

This is the historical "engine rewrite A" / per-access cc lever (see memory:
`per-access-cc-is-linchpin`, `coordinated-cputiming-verdict`,
`endgame-one-coupled-ds-subdot-build`). It is staged here exactly like the DS-sub-dot
engine (`ENGINE_DS_SUBDOT.md`): flag-gated `RB_PERACCESS`, flag-OFF == 86 at every
stage, intermediate stages validated by cctracer BYTE-EXACTNESS (not suite count,
which only moves when the compensations are deleted at the final stage).

---

## Baseline & inventory (measured this session)

- Worktree `/home/reddragon/rb-pa`, branch `f-peraccess`, HEAD `0f79c1f` = **86**
  (70 CGB + 16 DMG). `.../scratchpad/pa_baseline.json`.
- Failure buckets (this session, `fails.txt`):
  - 19  oamdma / `_dumper`           — HARNESS FLOOR (not a bug; memory: `oamdma-dumpers-are-harness-floor`)
  - 14  scx/scy_during_m3            — FACET 2 (within-M-cycle store-vs-latch)
  - 10  hdma_halt/unhalt             — FACET 3 (m0_time sub-dot phase at HALT)
  -  9  dma (hdma_late_m3speedchange / m0speedchange_late_m3wakeup / hdma_pc_7ffe / late_gdma_pc_7ffe) — FACET 3 family (m0-edge × speed-switch/wakeup)
  -  8  lcd_offset offset2/lcdoffset2 — FACET 1 (CPU instruction-boundary phase)
  -  7  window                       — mixed (some facet-2 fetch-order, some m0-edge)
  -  6  halt (m0int/m0irq_m0stat, m1int_ly, lycirq_m2stat) — FACET 1/3 (instruction-boundary + m0-edge at HALT)
  -  4  m1                           — line-end / instruction-boundary phase (facet 1 family)
  -  2  irq_precedence, 2 lcd_offset offset1, 2 sound, 1 bgen, 1 serial, 1 tima — assorted residual phase
- ~32 of the 67 non-harness failures map directly to the three named facets; the
  remaining ~35 are the SAME per-access root expressed in window/halt/m1/precedence
  clusters (each previously triaged to "per-access / m0-edge / instruction-boundary
  cc", never an independent bug).

---

## The three facets — confirmed localizations + cc deltas

### FACET 1 — CPU instruction-boundary phase (offset2/lcdoffset2 + offset1 + m1 + lycirq, ~12–16)
Canary `late_enable_lcdoffset2_1_cgb04c_out2`. These are 4-LCD-on-STOP (2 switch-pair)
tests; the 1-pair `lcdoffset1_1` passes.
- **cctracer (Gambatte):** LY=129 LYC dispatch `cc=611648`; handler runs `0xAF`(@611664),
  reaches the FF41-enable write region `pc=0x102C`(@611844), and the m2 IRQ flags at
  `cc=612082`. (Inter-line m2 cadence = 456cc.)
- **Measured root (memory `m3len-is-cpu-phase-not-renderer`, decisive):** the LCD/PPU
  phase is already CORRECT (LYC fires at IDENTICAL `ly128 line_cycle454` in 1-pair AND
  2-pair). The error is CPU-side: the handler's FF41 mode-enable write lands at dispatch
  **line_cycle 50 (fail) vs Gambatte 47 (pass)** — 3 dots late. `abs_cc` parity at LYC
  fire differs (**mod4 = 1 vs 0**). Over 4 STOPs the `cc += 8` + the 0x20000 unhalt
  windows shift the CPU instruction-boundary phase relative to the (correct) LCD by 3
  dots. The handler takes 49cc (1-pair) vs 52cc (2-pair); at lc50 the m2-schedule
  boundary (~lc451) sits 3 dots late → m2 fires ~453cc later → the IF read misses it.
- **Why it is per-access:** every PPU bridge/window lever is stop-count-coupled and a
  strict 1-for-1 swap (RB_BR_SD_REND ±1 = +49 each side; RB_UNHALT −2 fixes the 2-pair
  but breaks the 1-pair because the window delta SCALES with stop count). No PPU-side
  constant exists. The fix must make the CPU instruction boundaries land at Gambatte's
  exact cc across multiple STOPs — i.e. the STOP `cc+=8` / unhalt-window must keep the
  CPU boundary locked to the LCD with SUB-DOT precision, not whole-dot bridge advance.

### FACET 2 — within-M-cycle store-vs-latch order (scx/scy_during_m3, ~14)
Canaries `scx_during_m3_4` (must rewrite) / `_6` (must NOT rewrite), scx_0060c0.
- **The two ROMs are BYTE-IDENTICAL at every integer-cc observable** (write_cc, fetch_cc,
  self.x, fifo, abs_cc, draw-x↔cc — confirmed cctracer shows the identical SCX-write
  instruction `0xE2` LD(C),A at the same cc; no scx/fetch divergence at dot resolution)
  yet require OPPOSITE behavior. `_4`: the in-flight tile's TileNumber is fetched at the
  SAME cc as the SCX store but BEFORE it (saw old SCX → must rewrite). `_6`: the next
  tile is fetched AFTER the store (live fetcher correct → must NOT rewrite). No
  `write_cc + OFFSET` boundary works (`_3` needs OFFSET≤5, `_6` needs OFFSET>7 —
  contradiction). Memory `scx-during-m3-plus2cgb` DECISIVE: render-side rewrites at any
  offset break the passing aligned cases (scx_0060c0) for 0 net.
- **The discriminator is purely the WITHIN-DOT ordering of the CPU SCX store vs the PPU
  fetcher's TileNumber latch.** In rustyboi (`cpu/bus.rs::write`, else branch) the SCX
  store commits (`mmio.write`) and `on_scx_write` runs BEFORE `tick_m`; the fetcher's
  TileNumber latch runs DURING `tick_m`'s `resolve_one_dot`. The write is anchored at
  `write_cc(ds) = abs_cc + off + write_subdot` (`ppu/controller.rs:write_cc`,
  `on_scx_write` currently `delay=0`, "no positive lever; needs the read-cc convergent
  root, out of scope") — a whole-/half-dot parity, NOT the true interleave of the store
  against the fetch within the M-cycle.

### FACET 3 — m0_time sub-dot phase at HALT/speed-switch (hdma-during-halt + halt m0stat + dma m3wakeup, ~25)
Canaries `hdma_transition_halt_late_unhalt_scx1_1` (out00) / `_2` (outFF).
- **cctracer (Gambatte), the decisive capture this session** — the two ROMs differ only
  by a 1-instruction-length tweak that shifts the HALT entry across a mode-0 boundary:
  - `_1`: 2nd HDMA block (`period=1`) fires at **cc=71152** (pc=0x7404, AFTER unhalt
    resumes program flow) — the held block waits for the NEXT mode-0 rising edge.
  - `_2`: 2nd HDMA block (`period=1`) fires at **cc=70700** (pc=0x1187, still inside the
    unhalt service) — the m0 edge had already passed at the unhalt cc.
  - First block (tag=0 cc=70664) is IDENTICAL in both. The discriminator is whether the
    m0 rising-edge that re-flags the held HDMA block lands BEFORE or AFTER the unhalt cc;
    a **~4cc shift at the HALT/unhalt cc → a 452cc swing in the block fire cc** (one
    whole mode-0 period). This is a 1-for-1 swap on the dot grid.
- **Root (memory `endgame…`, `coordinated-cputiming-verdict`, `ds-subdot-engine-build-state`
  WAVE-2 hdma triage):** the PPU m0 rising-edge / `m0_time_master` phase at a HALT-entry
  that straddles a mode-0 boundary is decided on the dot grid; `hdma_period_halt(cc,ds)` /
  `hdma_period_unhalt(cc,ds)` / `m0_time_master_cc()` (`ppu/controller.rs:4258-4356`)
  bracket the edge in whole dots, so a HALT/unhalt at the sub-dot straddle picks the
  wrong side. The same m0-edge phase drives `halt/m0int_m0stat`, `m0irq_m0stat`, and
  `dma/hdma_late_m3speedchange` / `m0speedchange_late_m3wakeup` (m0-edge × speed-switch).

---

## Where the dot-grid rounding ENTERS (the rounding sites to fix)

The DS engine already made every register READ resolve at its exact access cc (the
read-path snapshots in `cpu/bus.rs::read`: STAT `get_stat(cc)`, LY `get_ly_reg_at_cc`,
LYC `get_lyc_flag_at_cc`, APU/timer/IF pre-tick snapshots). What is NOT sub-dot exact:

1. **`cpu/bus.rs::run_to`** — `while master_cc < target { resolve_one_dot(); }`. Advances
   one dot at a time; cannot stop AT a sub-dot event cc. (The "true min-event jump"
   deferred in the header comment.)
2. **`cpu/bus.rs::resolve_one_dot`** — the DS **parity-gate** (`cpu_t_phase() % 2`): at DS
   the PPU/audio step only on even half-dots and the CPU's odd half-dot runs `step_subdot`.
   This rounds any event scheduled at an odd-cc to the render dot. (Header: "step_subdot
   /parity-gate removal deferred.")
3. **`cpu/bus.rs::write` (else branch)** — store commits + `on_scx_write` BEFORE `tick_m`;
   the fetcher TileNumber latch runs DURING `tick_m`. The store↔fetch interleave WITHIN
   the M-cycle is collapsed to `write_subdot` parity. **(FACET 2 site.)**
4. **`cpu/opcodes.rs::stop`** — `stop_bridge_advance(bridge)` re-anchors the renderer in
   WHOLE dots; the CPU resumes at `master_cc + 0x20000` (Lever A exact) but the bridge
   cannot represent the sub-dot phase, so over multiple STOPs the CPU instruction
   boundary drifts mod-4 vs the LCD. **(FACET 1 site.)**
5. **`ppu/controller.rs::hdma_period_halt/unhalt`, `m0_time_master_cc`** — the m0
   rising-edge bracket is whole-dot; a HALT/unhalt at the sub-dot straddle picks the
   wrong side. **(FACET 3 site.)**

The compensating offsets that PAPER OVER #2–#5 today (must be deleted at the final
stage): `write_cc_off_ds/ss`, `write_subdot`, the `on_scx_write delay`; the
`hdma_*_for_unhalt_adj`/`+6`/`bias_cc` per-access read biases in `bus.rs::read`; the
`halt_wakeup_skew` VRAM `+6`; the STOP `bridge` dot-counts. (Many were already
collapsed by the DS engine; the rest are the per-access residue.)

---

## Target invariant

The CPU runs a true event-driven loop: each memory access resolves peripherals to the
EXACT access cc (read-at-cc — already done) AND each access advances `master_cc` by its
duration with `run_to` jumping to the MIN of (target_cc, next scheduled peripheral
event cc) — never cranking past an event. Every peripheral event (PPU mode edge / m0
rising edge, STAT/m2/m0/lyc IRQ flag, HDMA m0 re-flag, timer overflow) fires at its
exact scheduled cc, including odd DS half-dots, WITHOUT the parity-gate rounding. The
STOP re-anchor advances the renderer by the exact fractional phase (no whole-dot
bridge). Within an M-cycle the CPU store and the PPU fetch are ordered by their true
sub-dot cc. With that, the compensating offsets are deleted and the bracket pairs
(`_1`/`_2`, `_4`/`_6`) collapse to a single faithful value.

---

## Staging (flag-gated `RB_PERACCESS`; flag-OFF == 86 at EVERY stage)

> Validation rule (learned from the DS engine): Stages 1–4 are judged by cctracer
> byte-exactness of intermediate quantities (event cc, fetch-vs-store order, m0 edge
> cc), NOT by suite count — the count only moves at the FINAL stage when the
> compensations are deleted. Every coupled one-shot that was suite-judged mid-coupling
> VALLEYED. loadgate before every build; `-j4`; suite is RELEASE (keep underflow guards).

### Stage 0 (THIS) — scoping + scaffolding. DONE.
`RB_PERACCESS` env flag (`cpu/bus.rs::peraccess_enabled()`, OnceLock-cached, default
OFF = identity), this doc. Zero behavior change; flag-OFF == 86. (Same pattern as the
historical `RB_SUBDOT`/`RB_FAITHFUL`; inline/remove before merge to main.)

### Stage 1 (FOUNDATION, smallest validatable — RECOMMENDED FIRST) — true min-event `run_to`.
Convert `run_to` from a per-dot crank into a true min-event-jump driver: maintain a
single ordered "next scheduled peripheral event cc" (PPU mode/m0 edge, STAT-IRQ,
timer overflow, serial, DMA, HDMA m0 re-flag) and advance `master_cc` directly to
`min(target_cc, next_event_cc)`, firing the event exactly there, WITHOUT changing any
event's scheduled cc and WITHOUT removing the parity-gate yet (keep the proven render
cadence). This is a pure refactor of the ADVANCE mechanism: behavior must be
byte-identical to the per-dot crank when no event is mid-dot, and at DS it lets an
odd-cc event fire at the true half-dot instead of being rounded to the next render dot.
- **Why first:** it is the mechanism the other two facets BUILD ON (an event must be
  *firable* at a sub-dot cc before the STOP re-anchor or the store-vs-latch order can be
  made sub-dot exact), yet it is self-contained and independently validatable.
- **Validate (cctracer):** on a DS canary, every STAT/m2/m0/lyc event cc and the HDMA
  m0-edge cc fire byte-identically to flag-OFF (no regression), and an odd-cc-scheduled
  DS event now fires at its odd cc (vs rounded). Flag-OFF == 86. No suite movement
  expected (compensations still live).
- **Risk/coupling:** MEDIUM. Touches the single hottest path (`run_to`/`resolve_one_dot`).
  The renderer's per-instruction `dot`/`ticked` bookkeeping (`tick_remaining`,
  PPU per-instruction `dot`) must be preserved across a multi-dot jump. Keep the
  parity-gate; only the *granularity* of advance changes. Independently landable.

### Stage 2 — FACET 3: sub-dot m0 rising-edge at HALT/unhalt.
With Stage-1 events firable at sub-dot cc, make `hdma_period_halt/unhalt` and
`m0_time_master_cc` resolve the m0 rising edge at the exact cc (drop the whole-dot
bracket + the `hdma_*_for_unhalt_adj` widen and the `+6`/`bias_cc` read biases).
- **Validate (cctracer):** `hdma_transition_halt_late_unhalt_scx1_1`/`_2` 2nd-block fire
  cc == Gambatte (71152 / 70700) from the SAME formula; `halt/m0int_m0stat_scx*` and
  `dma/hdma_late_m3speedchange` m0-edge cc == Gambatte. Flag-OFF == 86.
- **Risk/coupling:** MEDIUM. Self-contained to the m0-edge predicate IF Stage 1 lands.
  Couples to FACET 1 only through the speed-switch variants (`hdma_late_m3speedchange`).
  Largest single facet (~25 cases incl. the dma/halt family). Independently landable
  after Stage 1.

### Stage 3 — FACET 1: sub-dot STOP re-anchor (CPU boundary locked to LCD).
Replace the whole-dot `stop_bridge_advance` with the exact fractional renderer
re-anchor across the STOP, so the CPU instruction boundary's `abs_cc` parity stays
locked to the LCD across 1-, 2-, 4-pair STOP chains. (Lever A already makes the
unhalt-window cc exact; this carries the sub-dot phase to the renderer so the handler
FF41 write lands at line_cycle 47 not 50.)
- **Validate (cctracer):** `late_enable_lcdoffset2_1` handler FF41 write at line_cycle
  47 and m2 IRQ at cc=612082 == Gambatte; the SAME re-anchor serves `lcdoffset1_1`
  (1-pair, still passing) AND `lcdoffset2_1` (2-pair). Flag-OFF == 86.
- **Risk/coupling:** HIGH. The STOP bridge is the most-tuned site (DS engine Stages
  2/5a/5b). The 1-pair/2-pair/4-pair brackets must ALL be served by one fractional
  formula — exactly the invariant the DS engine proved is reachable once the phase is
  exact. Must NOT disturb the DS-engine-landed `ly44_m3` / OAMSearch / lcdoff paths.
  Depends on Stage 1 (sub-dot advance); somewhat coupled to Stage 2 via the
  speed-switch+m0 variants.

### Stage 4 — FACET 2: within-M-cycle store-vs-latch order.
Make the CPU SCX/SCY (and any VRAM/OAM/reg) store and the PPU fetcher's TileNumber
latch resolve in their true sub-dot order within the M-cycle: instead of committing the
store before `tick_m` and parity-tagging it, drive the store at its exact access cc
inside the Stage-1 event loop so the fetch that runs at the same dot sees the store
iff the store's cc precedes the latch's cc. Delete `write_cc_off`, `write_subdot`, and
the `on_scx_write` render-rewrite machinery.
- **Validate (cctracer / runner FETCH trace):** `scx_during_m3_4` rewrites (store after
  latch → old SCX visible) and `_6` does NOT (store before next latch), from the SAME
  ordering rule; the aligned passing cases (scx_0060c0 baseline lines) stay byte-exact.
  Flag-OFF == 86.
- **Risk/coupling:** HIGH. Requires the store to participate in the sub-dot event order
  (Stage 1) and touches the fetcher hot path. Memory warns render-side rewrites at any
  offset net 0; the fix is the ORDER, not an offset. Hardest to validate (the
  discriminator is invisible at integer cc — needs a custom fetch-vs-store-cc hook).
  Depends on Stage 1; otherwise independent of Stages 2–3.

### Stage 5 — FINALIZE (flip default-on + delete compensations).
Flip `peraccess_enabled()` to unconditional true; inline each
`if peraccess { NEW } else { OLD }` to NEW; DELETE the dead offsets
(`write_cc_off_*`, `write_subdot`, `on_scx_write delay`, the read-path `+6`/`bias_cc`/
`hdma_*_for_unhalt_adj` biases, the STOP whole-dot bridge dot-counts, and — if Stage 1
fully subsumes them — `step_subdot`/parity-gate), incrementally, holding the full suite
after EACH file. Remove the env var (env not allowed in main). FINAL gate: full suite
< 86 with no env, HARD self-verify, smoke-test 2 real ROMs in DEBUG (overflow checks).

---

## Stage-1 recommendation (smallest contained validatable FIRST step)

**Stage 1 (true min-event `run_to`)** is the smallest self-contained, independently
validatable step. It changes only the ADVANCE mechanism (per-dot crank → min-event
jump), is byte-identical to the per-dot path when no event is mid-dot (flag-OFF == 86,
cctracer-provable), and is the prerequisite the other three facets all build on (an
event must be firable at a sub-dot cc before any of the store/STOP/m0 orderings can be
made exact). It does NOT yet remove the parity-gate or any compensation, so it carries
zero suite-count risk.

## Independence verdict

- **Stage 1** is independently landable (pure refactor, no suite movement).
- **FACET 3 (Stage 2)** is independently landable on top of Stage 1 — the biggest, most
  self-contained win (~25 cases), couples to FACET 1 only via the speed-switch variants.
- **FACET 1 (Stage 3)** and **FACET 2 (Stage 4)** each depend on Stage 1 but are
  independent of each other. FACET 1 is high-risk (STOP bridge is the most-tuned site);
  FACET 2 is high-risk-to-validate (invisible at integer cc).
- The facets are NOT all-or-nothing: the engine is **stageable behind a flag** exactly
  like the DS engine, with each stage cctracer-validatable and flag-OFF == 86.

---

## Feasibility verdict

**Stageable behind `RB_PERACCESS` (flag-OFF == 86 preserved), NOT all-or-nothing.** The
DS engine proved the pattern works (562→106→merged, now 86): once the underlying cc is
made exact, each compensation can be deleted and its bracket pair collapses to one
faithful value. The per-access engine is the same shape one level down (sub-dot/within-
M-cycle instead of M-cycle/DS-dot).

- **Realistic recovery:** ~30–50 of the 67 non-harness failures. The three named facets
  are ~25–30 directly; the window/halt/m1/precedence residue (~30) is the same
  per-access root and should fall with Stages 1–4, but some fraction may expose a second
  smaller phase error (as the DS engine left 3 residuals). Floor after this build is the
  ~19 oamdma/`_dumper` harness cases + a handful of real-bug residuals → suite ~36–56,
  best-case toward the campaign floor.
- **Hardest stage: Stage 4 (FACET 2, within-M-cycle store-vs-latch).** The discriminator
  is invisible at every integer-cc observable, so it cannot be validated without a custom
  cctracer fetch-vs-store-cc hook, and it requires the CPU store to participate in the
  sub-dot event order (the deepest coupling to Stage 1). Stage 3 (FACET 1 STOP re-anchor)
  is the second-hardest (most-tuned site, must serve all 1/2/4-pair brackets at once).
- **Biggest, safest win: Stage 2 (FACET 3, ~25 cases)** — do it first after the Stage-1
  foundation for early, low-risk progress.
- **Caveat:** Stage 1 touches the single hottest path (`run_to`/`resolve_one_dot`); a
  subtle regression there is the main execution risk. It must be proven byte-identical
  (flag-OFF == 86) and cctracer-byte-exact on a DS canary before any facet stage.

## Discipline
- cctracer byte-exact per stage BEFORE suite. Flag-OFF must stay 86 at every stage.
- loadgate before every build/suite; `-j4`; keep underflow guards (suite is RELEASE).
- Revert any cctracer/rustyboi instrumentation and rebuild cctracer pristine before
  finishing each session. Best executed as deliberate focused work, NOT parallel
  one-shot agents (every coupled one-shot this campaign valleyed). main NEVER regresses.

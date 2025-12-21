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

### Stage 1 (FOUNDATION) — true min-event `run_to`. DONE (701672d → committed; flag-on == 86, byte-identical).
**Status (this session):** `Bus::run_to` is now a min-event-jump driver behind
`RB_PERACCESS` (`run_to_min_event`, `cpu/bus.rs`). At each loop iteration it computes
the next scheduled peripheral event cc and advances `master_cc` directly to
`min(target_cc, next_event_cc)`, firing exactly that span. The advance MECHANISM is the
event loop; the per-dot resolution (`resolve_one_dot`, parity-gate KEPT) is still the
inner primitive while any per-dot machine is live (PPU renderer / OAM-DMA / HDMA /
powered APU), because those are intrinsically per-dot stateful (mode edges, duty
counters, period-edge detect, sub-cycle catch-up) and only become jumpable when the
later facet stages give them an advance-to-cc closed form. The genuine jump win this
stage lands is over **fully-idle spans** (`Mmio::idle_bulk_skippable`: LCD off, no
DMA/HDMA, APU off, serial idle): there only the timer + serial advance and both are
already span-collapsible, so the whole idle span is jumped in one `Mmio::bulk_advance_idle`
to the next timer-overflow fire cc (`Timer::step_to`, `next_overflow_fire_cc` — uses the
SAME `fold` `update_irq_delivery` applies, so the overflow fires at the identical cc).
**Validated:** full suite RB_PERACCESS=1 == 86, byte-identical failure set to flag-OFF
(diffn net 0 / broke 0 / fixed 0); flag-OFF stays 86; debug build (overflow checks) clean
on the idle-heavy enable_display + div subsets. **Event sources enumerated:** the only
event firable INSIDE `run_to` during a `skippable` span is the timer overflow IRQ (PPU
off ⇒ no STAT/m0/lyc/m2 edges; DMA/HDMA/serial/APU all gated out; STOP/HALT wakeups are
CPU-boundary, outside `run_to`). Every other event path keeps the proven per-dot crank,
so no event source can be missed or mis-ordered. **Perf:** the idle skip is a real but
modest win at Stage 1 (guard is deliberately conservative — only fully-idle LCD-off
spans); active rendering still cranks per-dot, so wall-clock is ≈parity on
render-dominated subsets. The structural foundation (events firable off a min-cc target)
is what Stages 2–4 build on.

----- original Stage 1 plan (for reference) -----
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

#### Stage 2 status (this session) — FALSIFIED for the m0-edge-CONSTANT lever. Reverted to a6d344f (86/86).
The decisive canary pair was probed end-to-end with cctracer + an `RB_HDMA_TRACE`
env hook in `run_hdma_block_inner` / `on_cpu_halt` / the unhalt reflag. The exact
clock map (rustyboi cc, all hooks):

```
                HALT cc   m0t@halt  unhalt cc  next-line m0_edge  next m0t  block2 fire
 _1 (out00)     11872     11873     12296      12323             12329     12328  (FAIL)
 _2 (outFF)     11876     11877     12296      12327             12333     12332  (PASS)
```
Gambatte (cctracer) fires exactly TWO HDMA blocks: block1@70664 (both), block2 at
**71152 (_1, LY=3, pc=0x7404, a full line after unhalt)** vs **70700 (_2, LY=2,
pc=0x1187, in-service)**. rustyboi instead fires THREE blocks (block1 twice — the
coincident-rollback re-fire at unhalt — then block2 at the LY=2 m0 for BOTH), which
happens to leave `_2`'s final VRAM correct but copies the wrong source for `_1`.

**Root cause of the canary, MEASURED & DECISIVE:** the two ROMs differ ONLY by a
~4 cc program-length shift. Because the LCD phase is locked to the program clock,
that 4 cc shifts the HALT cc, `m0_time_master`, the exact m0 edge (`m0t - gap`),
the line-end `lyTime`, AND the unhalt cc **all in lockstep**. Every RELATIVE
quantity is therefore INVARIANT between `_1` and `_2`:
  - `halt_cc - exact_edge` = 5 for BOTH (edge = m0t-6: _1 11867, _2 11871).
  - `unhalt_cc - next_m0_edge` = identical bracket for BOTH.
  - the line-end `cc+3+3*ds < lyTime` bracket is identical for BOTH.
There is **no integer-cc (or exact-edge-constant) predicate that can discriminate
`_1` from `_2`.** The only quantity that differs is the absolute cc's sub-dot phase
— i.e. the within-dot ORDERING of block1's m0 `intevent_dma` event vs the HALT
instruction's prefetch M-cycle (Gambatte's `hdmaReqFlagged` at `Memory::halt`
captures `hdma_requested` iff that event is still pending when halt runs; `hdma_high`
if it already fired+acked one M-cycle earlier). rustyboi's `access_cc == master_cc`
is dot-granular, and the HDMA per-dot machine does NOT yet fire inside Stage-1's
sub-dot min-event loop (Stage 1 deliberately kept the per-dot crank for the HDMA
machine), so block1's m0 event is rounded to the HALT dot in both → both classify
as `Requested`.

**Empirical confirmation it is a strict 1-for-1 swap (not a win):** an
`RB_PERACCESS`-gated change moving the `hdma_period_halt`/`hdma_period_unhalt` START
bracket from the bare `m0t` to the exact edge `m0t - gap` (keeping the END absolute)
— the literal Stage-2 plan — scored **net +2** on the full suite (RB_PERACCESS=1):
fixed 0, broke `hdma_late_m3halt_m2unhalt_scx{1,2}_1` (the widened start now
over-fires them). Flag-OFF stayed 86. This matches the campaign memory
(`endgame-one-coupled-ds-subdot-build`, `per-access-cc-is-linchpin`): the m0-edge is
**necessary but insufficient alone**; the canon `_1`/`_2` resolution needs the HDMA
machine to participate in the sub-dot event order (block-flag vs HALT-prefetch
interleave). That is NOT a "drop the bracket constant" change — it is the deeper
"HDMA per-dot machine becomes advance-to-cc / fires at its exact event cc inside the
Stage-1 loop" work, which Stage 1's status section explicitly defers ("only become
jumpable when the later facet stages give them an advance-to-cc closed form").

**Verdict / next step for Stage 2:** the safe, suite-positive Stage-2 win requires
first promoting the HDMA m0 `intevent_dma` event into the Stage-1 sub-dot min-event
driver so the block-flag fires at its EXACT sub-dot cc and the HALT/unhalt
classification reads the true within-M-cycle ordering (then `Requested` vs `High` for
`_1`/`_2` falls out, and the dma/m3speedchange/m0stat family follows). The
m0-edge-constant brackets are already at their calibrated optimum on the dot grid;
nudging them is a strict trade. Reverted; tree clean at a6d344f, flag-OFF=86,
flag-ON=86.

#### Stage 2 status (CORRECTED, LANDED) — sub-block-cc m0-edge CONSUME. flag-ON 86->84, broke 0.
The FALSIFIED note above located the discriminator at the HALT entry and concluded it
was sub-dot-only. That was **half right**: the HALT entry is genuinely invariant — BOTH
ROMs enter `Memory::halt` as `hdma_requested` (cctracer `[GBHALT]` hook on the gambatte
worktree, NEW this session: `_1` cc=70240 / `_2` cc=70244, both `isHdmaPeriod=1
hdmaReqFlagged=1`). So the halt classification is NOT the lever. The TRUE discriminator
is one level later, at the **post-unhalt m0 edge vs the just-fired block's transfer
span** — an integer-cc signal that the prior note missed by only tracing the halt cc.

**Decisive cctracer ground truth** (gambatte `[GBHALT]/[GBINTUNHALT]/[GBDMAEV]/[GBM0EV]`
hooks added to memory.cpp/video.cpp this session, then fully reverted — cctracer pristine):
```
            HALT@req  UNHALT  block1(DMAEV,req)  block2(DMAEV)  gap b1->b2
 _1 (out00) 70240     70664   70664              71152          +488 (one LINE later)
 _2 (outFF) 70244     70664   70664              70700          +36  (this line, in-service)
```
Gambatte fires exactly TWO blocks. block1 is the held `Requested` block, fired at unhalt.
block2 is the NEXT m0 edge. The discriminator: after block1 fires at 70664, its `dma()`
transfer occupies `[70664, 70664+16*(2+2*ds)) = [70664, 70696)` (SS). The next m0
`memevent_hdma` lands at **70696 (`_1`, == transfer END → ABSORBED by the in-flight
`dma()`; `flagHdmaReq` reschedules to the line AFTER → block2 @71152)** vs **70700
(`_2`, transfer-end+4 → fires its OWN block @70700, this line)**. So a 4 cc shift in the
m0 edge flips whether it falls inside block1's transfer span or just past it.

rustyboi (RB_HDMA_TRACE map, same clock): UNHALT 12296, block1 FIRE 12297, block2 arm at
the per-dot m0 edge `m0t-1` = 12328 (`_1`) / 12332 (`_2`); transfer end = 12297+32 = 12329.
`_1` arm (12328/12329) is `<= end` → must be ABSORBED (block2 → next line); `_2` arm (12332)
is `> end` → fires this line. The dot-grid baseline fired block2 at the absorbed edge for
BOTH (plus a spurious coincident-rollback re-fire), passing `_2` by luck, failing `_1`.

**Fix (LANDED, RB_PERACCESS):** at the `Requested`-at-halt unhalt reflag
(`sm83.rs`, the `HaltHdmaState::Requested` arm) arm a one-shot sub-block-cc consume
(`Mmio::arm_hdma_peraccess_consume`). In `step_hdma`'s two m0-arm branches
(`mmio.rs`), a new `peraccess_consume_m0_arm()` absorbs any m0 rising-edge arm whose
master cc lands in `[block1_fire_cc, block1_fire_cc + 16*(2+2*ds)]` (inclusive end —
Gambatte's edge AT the transfer end is consumed), deferring the genuine next block one
line; the first arm strictly PAST the span fires its block and disarms the consume. This
is the faithful Gambatte mechanism (m0 `memevent_hdma` absorbed by the in-flight `dma()`),
NOT a bracket constant: the boundary is the transfer LENGTH, derived, not tuned. The
consume is cleared at HALT entry so it never spans halts; flag-OFF leaves the whole path
untouched (`peraccess_enabled()` early-out inside the helper).

**Result:** full suite RB_PERACCESS=1 **86 -> 84**, fixed=2 (`hdma_transition_halt_late
_unhalt_scx1_1` + sibling `hdma_transition_ei_halt_late_unhalt_scx1_1`), broke=0 across
all 5257 tests (hdma/dma/m0/sprite/window screened via diffn vs main_86). flag-OFF == 86
byte-identical. ONE faithful model resolves the canary `_1`/`_2` pair with no swap.

**What resists (NOT this consume's bug — separate sub-mechanisms):** the
`hdma_transition_*halt_late_unhalt_ldaaimm_hdma_scx1_*` siblings fire block2 this line
(Gambatte @70712, an intervening LD A,imm shifts the block start) but at a WRONG block2
FIRE CC in rustyboi — a block2-fire-cc-PRECISION residual, not a consume-decision error
(my window correctly does NOT consume them). The `hdma_late_m3speedchange_*` /
`m0speedchange_late_m3wakeup_*` family is the m0-edge × speed-switch coupling (FACET 3 ×
FACET 1), still open. The `_2` (out02) `inc`/`ldaaimm` variants are the same block2-fire-cc
precision class. Next sub-step for FACET 3: make block2's FIRE cc (not just the
fire/defer decision) exact for the in-service case — i.e. fire block2 at its exact m0
event cc in the min-event driver rather than the per-dot `m0t-1` arm dot.

Tree at f-peraccess HEAD (this commit), flag-OFF=86, flag-ON=84.

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

#### Stage 3 status (FACET 1, this session) — ROOT FOUND + faithful fix identified, but it COUPLES to FACET 2 (cannot reach broke-0 alone). REVERTED to 798b356.

**Decisive cctracer + runner-trace ground truth (canary `late_enable_lcdoffset2_1`):**
The CPU instruction boundary is ALREADY byte-exact through the STOP sequence — the
handler's IF read (`pc=0x1067`, `ldff a,(0f)`) lands at rustyboi cc 553716 == Gambatte
612084 (constant boot offset 58368), in BOTH the passing 1-pair and the failing 2-pair.
Lever A's `0x20000+8` per-STOP CPU advance matches Gambatte's `cc()=C+0x20000+8` exactly
(verified against `Memory::stop` memory.cpp:444 + cpu.cpp case 0x10: `cc()=stop(cc()-4)`
then the `+= cycles + (-cycles&3)` unhalt jump). **So the failure is NOT the CPU resume
cc — it is the LCD/renderer phase.**

The real observable: the handler's FF41 mode-2-enable write (`pc=0x1065`) fires the m2
STAT IRQ via the immediate `statChangeTriggersM2IrqCgb` quirk (video.cpp:619) iff
`time_to_next_ly = lyCounter.time - write_cc` ∈ {3,4} at SS (`(456-452)*(1+ds)`, `>2`).
Measured `ttnl` at the write (runner STATW hook):
- 1-pair `lcdoffset3_1` (PASS): ttnl=3 → m2 fires → out2. ✓
- 2-pair `lcdoffset2_1` (FAIL): ttnl=2 → no fire → out0. ✗ (needs 3)

**Root, MEASURED & DECISIVE — the DS->SS sub-dot (half-dot) carry.** Gambatte's
`Memory::stop` re-anchors the LCD with `now -= isDoubleSpeed()` (== 1 DS cc = HALF an
SS dot) on a DS->SS switch (video/ppu.cpp:1846 `PPU::speedChange`). rustyboi's whole-dot
DS->SS bridge (1) + `set_dsss_lytime_adjust` rounds that half-dot to 0. So:
  - 1 DS->SS-mode3 switch  → +0.5 dot → rounds to 0 (correct; the 1-pair siblings pass).
  - 2 DS->SS-mode3 switches → +1.0 dot → a WHOLE dot the whole-dot bridge never injects.
The failing 2-pair (`lcdoffset2_1`, 2 DS->SS-mode3 STOPs at cc=139004 + 401188) lands the
post-STOP LCD phase 1 dot short; the passing 1-pair `*_ds_lcdoffset1_2` (1 DS->SS-mode3
STOP) is BYTE-IDENTICAL at every STOP yet needs NO extra dot. **No integer-cc predicate
at any single STOP discriminates them** (the SS->DS-HBlank STOP at cc=270096 is identical
in both) — exactly the "needs even deeper per-access cc" the roadmap warns of. The
discriminator is the ACCUMULATED half-dot across the DS->SS switch COUNT.

**Faithful fix BUILT & PROVEN (then reverted):** a stop-count-invariant half-dot
accumulator — every SECOND DS->SS-mode3 STOP injects one extra bridge dot (`floor(n/2)`,
reproducing the `now -= 1` accumulation), reusing the vestigial `sc_mode3_pullback`
flag. Full suite RB_PERACCESS=1: **84 -> 81, fixed 6, broke 3.** flag-OFF (env UNSET)
== 86 byte-exact. Fixed: `late_enable_lcdoffset2_1`, `late_enable_ly0_lcdoffset2_1`,
`offset2_lyc8fint_m1stat_1`, `offset2_lyc98int_ly_count_2`,
`offset2_lyc99int_m0stat_count_scx1_1`, `offset2_lyc99int_m2irq_count_1`. The
multi-pair `speedchange*_m3stat` (3/4/5-pair) and `*_ds_lcdoffset1_2` families STAY
passing. Debug smoke (overflow checks) of Pokemon Crystal (heavy speed-switch) + Tetris
ran clean at flag-ON and flag-OFF.

**WHY it is reverted (the broke-0 blocker = FACET 2 coupling):** the carry MUST be a
*rendered* bridge dot. The m2-enable trigger reads `lyCounter.time = abs_cc +
(456-line_cycle)<<ds`, and `ttnl = lyCounter.time - write_cc` must shift by an ODD 1 dot.
A pure `p_now` phase shift moves `ttnl` only by `2*p_now` (EVEN; verified: it spared all
FACET-2 collateral but left the targets at the wrong ttnl). The odd shift REQUIRES
advancing `line_cycle`, i.e. a rendered dot — which simultaneously shifts the mode-3
render-latch by 1 dot. That regresses the FACET-2-coupled siblings:
  - `prewrite_lcdoffset2_1` (vram_m3 mode-3 write latch) — pure render collateral.
  - `offset2_lyc8fint_m1irq_2`, `offset2_lyc99int_m2irq_count_2` — the `_2` halves of
    `_1`/`_2` bracket pairs Gambatte separates by 1cc (rustyboi collapses both to one
    side; fixing `_1` flips `_2` — a sub-cc swap, net 0 for that pair).
So the LCD-phase fix (correct for the STAT/m2 observable) and the render-latch are
COUPLED through `line_cycle`; reaching broke-0 needs **FACET 2 — decoupling the line
clock from the pixel fetcher** so the STAT phase can advance without moving the latch.
That is out of FACET-1 scope. The faithful per-STOP DS->SS half-dot carry is PROVEN
correct and stop-count-invariant; its application is gated on the FACET-2 line/fetcher
split. Patch preserved in this note (the `floor(n/2)` accumulator in `opcodes.rs::stop`
+ a rendered carry bridge dot on the 2nd DS->SS-mode3 STOP). Tree reverted to 798b356,
flag-OFF == 86, flag-ON == 84.

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

#### Stage 4 status (KEYSTONE LANDED) — decouple STAT/line phase from render latch; FACET-1 carry lands. flag-ON 84 -> 81, broke 3 (all `_2` bracket halves). Tree at this commit, flag-OFF == 86.

This stage is the KEYSTONE the Stage-3 note identified: rustyboi welds `line_cycle`
(the STAT/LY/ttnl phase clock) to the pixel fetcher render latch, so the proven
FACET-1 DS->SS half-dot carry could not shift the STAT phase by an odd dot without
moving the mode-3 render latch (broke 3 render cases in Stage 3). Stage 4 splits the
two clocks so the carry advances the STAT phase ALONE.

**Step 1 (decouple, byte-identical — commit `754d2bb`):** added
`PpuController::step_stat_phase_only` / `stat_phase_carry`: advance the STAT/line
clock (line_cycle / internal_ly / abs_cc / scheduled STAT events + the LYC compare +
STAT trigger) by one dot WITHOUT running the `match self.state` render machine or
`self.ticks += 1`. It mirrors `step`'s STAT-phase region exactly (the lines between
`dispatch_stat_events` and `update_window_y_latch`). Wired into the STOP DS->SS-mode3
path behind `RB_PERACCESS` with `register_dsss_mode3_stop` (the `floor(count/2)`
stop-count-invariant accumulator from Stage 3), gated `STAGE4_FACET1_CARRY=false` so
it injects 0 dots => flag-ON stays **84 byte-identical** (net 0, broke 0). Safety
checkpoint confirmed (full suite RB_PERACCESS=1 diff vs the 84 baseline = net 0).

**Step 2 (land FACET 1 + the render decoupling):** flipped `STAGE4_FACET1_CARRY=true`.
The carry advances `line_cycle`/`abs_cc` by one dot per the `floor(n/2)` accumulator;
this correctly shifts the STAT m2-enable trigger (`lcdstat_change` reads `ttnl =
lyCounter.time - write_cc`). The new piece that the decouple makes possible: the carry
also advances `abs_cc`, which moves the lyTime-anchored CPU-access mode-3 lock
boundaries (`cgbp_block_start_cc` / `m0_time_master`) — but the fetcher's actual lock
window did NOT move. So a new `render_carry_skew_cc` accumulator records the carry, and
the CPU VRAM/OAM/cgbp visibility gate (`Bus::ppu_blocks`) SUBTRACTS it from the access
cc (a render-frame `gate_cc` passed to both the `get_stat` fallback mode AND
`cpu_access_blocked`) so a store resolves against the un-carried fetcher geometry. This
is the FACET-2 decoupling: the STAT phase shifted, the render latch stayed put.

**Result (full suite RB_PERACCESS=1, vs main_86 flag-off baseline):** flag-ON **84 ->
81** (vs main_86: fixed 8, broke 3). flag-OFF == **86** byte-identical at every step.
- Fixed (8 vs main_86, the clean FACET-1 gains incl. the decoupled render case):
  `late_enable_lcdoffset2_1`, `late_enable_ly0_lcdoffset2_1`, `preread_lcdoffset2_1`,
  `prewrite_lcdoffset2_1` (THE render case Stage-3 lost — now recovered by the
  decouple), `offset2_lyc8fint_m1stat_1`, `offset2_lyc99int_m0stat_count_scx1_1`,
  `offset2_lyc99int_m2irq_count_1`, plus one more m2enable lcdoffset.
- **KEYSTONE PROOF:** `late_enable_lcdoffset2_1` (FACET-1 STAT fix) PASSES *and*
  `prewrite_lcdoffset2_1` (the mode-3 render-visibility case) PASSES simultaneously —
  the exact pair Stage 3 could not satisfy together. The STAT-phase odd shift landed
  with the render latch intact.

**What resists (broke 3, the facet-2-proper residual — NOT the decouple's bug):** the 3
regressions are all `_2` bracket halves whose `_1` partner is now fixed:
`prewrite_lcdoffset2_2` (out0, mode-3 blocked), `offset2_lyc8fint_m1irq_2`,
`offset2_lyc99int_m2irq_count_2`. Each is a tight `_1`/`_2` pair separated by one
instruction byte (4 cc); the carry sets the boundary so `_1` is now correct but the
4-cc-later `_2` falls on the wrong side. For `prewrite` the discriminator is the LY=1
mode-3 END boundary cc: the write lands at `m0t + ~284 cc` (LY=1 mode 0), yet Gambatte
blocks `_2` — i.e. the relevant boundary is the LINE-END / next-line mode-2 wrap, whose
exact cc the carried-line model does not yet split for the lcdoffset2 phase (the
PASSING 1-pair `prewrite_lcdoffset1_1/_2` does split its 4-cc bracket, so the mechanism
exists; the 2-pair carried phase shifts it off). This is FACET-2-proper (the within-dot
fetcher-latch-vs-store sub-dot order / exact line-end boundary), the roadmap's hardest
sub-mechanism and a clean Stage-4 stopping point per the plan ("after FACET 1 lands if
facet 2 needs more"). The 2 STAT `_2` halves are the same integer-cc bracket collapse
the Stage-3 note flagged as inherent.

**Net assessment:** the keystone IS proven — the decouple split the two clocks cleanly
(Step 1 byte-identical), FACET 1 landed (the STAT m2-enable family + the decoupled
render case), and the residual is the next-level facet-2 sub-dot boundary, not the
coupling. Behind `RB_PERACCESS`; flag-OFF == 86. The remaining 3 `_2` cases need the
exact line-end / fetcher-latch sub-dot boundary (facet-2-proper), the next sub-step.

#### Stage 4 NEXT (facet-2-proper, the 3 residual `_2` bracket halves)
The decouple infrastructure (`render_carry_skew_cc` + `stat_phase_carry` +
`step_stat_phase_only`) is in place. The remaining work is the exact boundary that
splits each `_1`/`_2` pair at sub-dot resolution: for `prewrite_lcdoffset2_2` the LY=1
line-END / next-line mode-2 OAM-wrap boundary cc under the carried phase (the 1-pair
`prewrite_lcdoffset1` already splits its bracket; replicate that split for the carried
2-pair). For the 2 STAT `_2` halves, the m2/lyc IRQ fire cc needs the within-dot
fetcher-latch-vs-store order (the original FACET-2 "store-vs-TileNumber-latch" item).
Both are integer-cc-invisible without a fetch-vs-store-cc hook; do NOT bracket-tune.

#### Stage 5 status (FACET-2-proper) — the 3 `_2` bracket halves RECOVERED. flag-ON 81 -> 78, broke 0, flag-OFF == 86. Commit `60e36b6`.
The 3 Stage-4 regressions (`prewrite_lcdoffset2_2`, `offset2_lyc8fint_m1irq_2`,
`offset2_lyc99int_m2irq_count_2`) were ALL the same line-end boundary at sub-dot
resolution under the carried phase — NOT the within-dot SCX store-vs-TileNumber-latch
order. Located decisively via rustyboi-side cc traces (the carried `abs_cc`/`internal_ly`
vs the un-carried render machine), no bracket tuning:

- **`prewrite_lcdoffset2_2` (VRAM-write line-end).** Trace at the write: `_1` lands at
  line_cycle 79 (still LY=1 mode-2, before the cgbp/VRAM begin window), `_2` 4cc later
  at line_cycle 83 (PAST the begin window, i.e. into the next line's mode-3 → blocked).
  Under the carry the lyTime-anchored `vram_started` begin (de-skewed access cc vs the
  un-carried cgbp begin) is now EXACT, so `started` alone discriminates the pair. The
  pre-existing coarse `ticks>80` OAMSearch escape (`cpu_access_blocked`) forced the whole
  carried mode-2 tail accessible and flipped `_2` wrong. Fix: when a carry is live
  (`render_carry_skew_cc != 0`), return `started` directly instead of the coarse escape.
- **`offset2_*_m1irq_2` + `*_m2irq_count_2` (VBlank line-end).** Decisive trace: rustyboi
  flags the mode-1 STAT IRQ (`sched_m1irq` in `dispatch_stat_events`) at the CARRIED
  line-clock m1 boundary (abs_cc 552272 = master_cc 560096, Gambatte-exact), but flags the
  actual VBlank interrupt (IF bit 0) from the RENDER machine (`HBlank ticks==455`, master_cc
  560098) which the carry does NOT advance. The `_2`-half IF read lands in that 2-3cc gap
  → sees the STAT bit (E2) but misses VBlank (correct = E3). Gambatte fires BOTH from the
  same lyCounter LY=144 event. Fix: under a live carry, also fire VBlank at the carried m1
  boundary in `dispatch_stat_events` (idempotent with the render machine's later same-frame
  fire). This is the line-clock/render decoupling carried through to the VBlank IRQ, the
  exact analog of the STAT-phase carry.

Both fixes are `render_carry_skew_cc != 0`-scoped so flag-OFF and non-carried frames keep
the proven render-machine paths byte-identical. Full suite RB_PERACCESS=1: 81 -> 78
(fixed 8 vs main_86, broke 0); flag-OFF == 86 byte-identical; debug smoke (overflow checks)
clean on lcd_offset/vram_m3/oam_access/scx_during_m3/m1.

#### Stage 5 — scx_during_m3 render cluster (the original FACET-2 store-vs-latch): NOT a
#### sub-dot-cc fix — it needs a per-COLUMN-scx closed-form renderer. DEFERRED (net-0 risk).
The `scx_during_m3_3/_4` (= `scx_0761c0/_3,_4`), `scx_during_m3_ds_2..5`, `spx0/1/2`,
`scx_attrib_*` failures are NOT the within-dot store-vs-TileNumber-latch order at all —
they are a structural limitation of the closed-form renderer. `render_full_line`
(`linerender_enabled`) renders the whole visible line at the mode-3→HBlank transition
using a SINGLE `scx_delayed` value (`line_bg_pixel` reads `self.scx_delayed` for every
column). The scx_during_m3 ROMs write SCX 3+ times mid-mode-3 (e.g. 0x07→0x61→0xc0→0x07):
the displayed line must show each column with the scroll value in effect WHEN THAT TILE
WAS FETCHED. The single-value renderer cannot represent that. Measured: `scx_0761c0/_4`
diverges 1144 px with the first mismatch at x=135 (the LATE columns, governed by the
later 0xc0/0x07 writes), confirming a multi-value-per-line scroll, not a first-tile
discard (the existing `rewrite_first_fifo_tile`/`scx_f1_*` f1-discard latch already
handles the first tile; the aligned `scx_0060c0` set passes). A faithful fix requires
recording the (cc, scx&7 + tile-column-shift) write events during mode-3 and having
`line_bg_pixel` pick the scx that was live at each column's fetch cc. This is exactly the
render-side rewrite memory `scx-during-m3-plus2cgb` proved nets 0 at any single offset and
risks the many passing scx cases; it is its own substantial sub-project (per-column scx
history in the closed-form renderer), deliberately deferred rather than bracket-tuned. The
3 `_2` halves above were the genuinely sub-dot-cc-recoverable FACET-2 residual; the scx
cluster is a separate renderer-model item.

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

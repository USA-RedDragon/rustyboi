# Plan: cycle-accurate CPU↔peripheral memory timing

## Progress log
- **Stage 1 DONE (verified):** severed the cpu↔ppu borrow (`Mmio::request_interrupt`); PPU no longer takes `&mut cpu`.
- **Stage 2 DONE (verified):** `Bus` (`Deref<Target=Mmio>`) threaded through `cpu.step`/`execute`/`execute_cb`/all 105 handlers as a pass-through — zero handler-body changes. Suite unchanged (2623).
- **Stage 3 DONE (verified):** timer + serial also use `request_interrupt` (no `&mut cpu`); the delayed-write queue moved from `SM83` into `Mmio` (`queue_delayed_write`/`step_delayed_writes`/`clear_delayed_writes`). The per-dot tick body is now **fully CPU-free**, so a `Bus::tick_t` can run it. Suite unchanged (2623).
- **Stage 4 PROTOTYPED, reverted (saved in `/tmp/stage4-reference/`):** made `Bus::read/write` tick one M-cycle inline and `gb.rs` tick only the remaining internal cycles. Result on the full suite: **net −143** (470 fixed / 613 regressed). Big *wins* in CPU-timing categories (tima +47/+15 depending on access order, halt +16, ly0, m1, m2enable). But it **breaks PPU rendering** — `bgtilemap`/`bgtiledata` (100% passing at Stage 3) drop to ~0 in *both* read/write orderings. Conclusion: inline ticking is correct but the PPU's display-start + mode-3 fetcher timing is calibrated to atomic execution and must be **re-tuned first**. Access-order finding: tick-*after* helped sprites/serial, tick-*before* helped tima more — the right answer is per-access (gekkio ordering), not a single global choice.

- **Stage 4 (partial) LANDED — timer+serial inline (verified net −48):** rather than step everything inline (which broke PPU rendering), only the **timer and serial** advance inline via `Bus::read/write` (tick-before; `tick_remaining` finishes internal cycles); **DMA and PPU stay in the `gb.rs` catch-up loop** (their timing is calibrated to per-instruction advancement). Result: tima +47, serial/div small gains, **zero PPU/DMA regressions**. Suite 2602→2554 failures (2978→2554 = −424 from the original baseline). This is the shipped state.

- **Timer overflow-delay + DIV/TAC glitches LANDED (verified net −80):** with the timer now ticking inline (accurate IRQ cycle), re-applying the cycle-accurate timer model (4-cycle TMA-reload delay, unified falling-edge detector with DIV/TAC-write glitch increments, TIMA-write reload cancel) is now net-positive — earlier it regressed dma/irq/m1 *under atomic execution*, but inline timing fixes that. tima 129→209. Suite 2554→2474 (−504 from original baseline). Shipped.

- **Inline OAM-DMA (lookahead 0) LANDED (−98):** stepping the DMA engine inline at the true cycle removes the need for the atomic-era `DMA_READ_LOOKAHEAD` fudge. oamdma 178→276.
- **Inline PPU LANDED (−87): the frontier is cracked.** The PPU now ticks inline in `Bus::tick_t`, and `Bus::write` fires `handle_lcdc_write`/`on_stat_register_write` at the write's true cycle. Key discovery — **writes must split by target**: registers of peripherals we tick inline (timer 0xFF04-07, serial 0xFF01-02, DMA 0xFF46, and WY 0xFF4A) latch at the *end* of the write M-cycle (tick-*before*); PPU registers + memory take effect as issued (tick-*after*). Reads are always tick-before. This kept rendering intact (bgtilemap/bgtiledata/scx unchanged) while gaining window +98, sprites +69, ly0 +30, enable_display +26, m1 +25, lcd_offset +19, halt +18. Suite 2351→2264 (−714 from original baseline).

### Remaining inline-PPU refinement (window/sprites churn)
Window `late_wy`/`late_disable`/`late_sc` and the basic sprite tests still churn — per-register, per-opcode mid-mode-3 write sub-timing (the old `queue_delayed_write` delays encoded this per-opcode; the split-by-address heuristic is an approximation). Each PPU register's exact latch dot can be tuned via the `tick_before` set in `Bus::write`. Diminishing returns per register; the deeper fix is making the PPU's internal mode-3 fetcher timing consistent so no per-register compensation is needed.

### Identified next target: APU frame sequencer DIV-coupling (sound, ~113 fails)
The APU frame sequencer (`audio/controller.rs`) uses an independent 8192-cycle countdown; on hardware it's the falling edge of **DIV bit 12** (bit 13 in double-speed) — same counter as the timer. That's why `div_write_*` / `*_counter_timing` sound tests fail. Fix: clock the sequencer off `timer.internal_counter()` bit 12. Because the timer advances inline (before the APU's catch-up step), either (a) step the APU inline in `Bus::tick_t` too, or (b) have the inline timer count bit-12 falling edges into a pending counter the APU drains in catch-up.

### Next session (extend Stage 4)
The remaining inline-ticking wins need the PPU (and its delayed-write/mode-3 calibration) moved inline too — that's the part that broke `bgtilemap`/`bgtiledata` (a clean +8-dot shift from mid-mode-3 LCDC-write timing; the atomic-era `queue_delayed_write` delays must be recalibrated). Also consider DMA inline once oamdma's `DMA_READ_LOOKAHEAD` is re-tuned (it dropped 178→142 when stepped inline). Prototype + diagnosis notes in `/tmp/stage4-reference/`.

### (old) Next session (Stage 4 proper)
1. Re-apply `/tmp/stage4-reference/` (bus.rs + gb.rs).
2. Fix the PPU frame/display-start regression first (diagnose why a fully-static BG render like `bgtilemap` breaks — likely LCDC-enable dot or `frame_ready` boundary under inline ticking), using `bgtilemap`/`bgtiledata` as the green oracle (they must return to 100%).
3. Then tune per-access tick ordering against the CPU-timing categories (tima/STAT) without re-regressing PPU.

## Why
Gambatte's remaining ~2400 failures (oamdma, window, sprites, scx/scy_during_m3,
sound, tima, dma, the m0/m1/m2/lyc STAT families) all share one root cause: the
CPU executes each instruction **atomically** (all `mmio.read/write` happen up
front inside the opcode handler), then `gb.rs::step_instruction` catches the
peripherals up afterward. Hardware interleaves them — a memory access on M-cycle
3 of an instruction sees peripheral state as of M-cycle 3. The tests read
LY/STAT/IF/OAM at exact intra-instruction M-cycles, so atomic execution is off
by 1–3 cycles every time. The `delayed_mmio_writes` mechanism is a partial
patch for this; the fix is to make it the rule, not the exception.

## Core idea
Make every CPU memory access advance the system clock by exactly one M-cycle
(4 dots), ticking timer/serial/DMA/PPU/audio *during* the access — so reads and
writes observe and mutate live peripheral state at their true cycle.

## Key enabler (do first, low risk)
`Ppu::step`, `step_scheduled_stat_events`, `enter_scheduled_mode2`,
`check_and_trigger_stat_interrupt` take `&mut cpu` **only** to call
`cpu.set_interrupt_flag`, which is just `mmio.write(IF, …)`. Add
`Mmio::request_interrupt(flag)` / `clear_interrupt(flag)` (the `| 0xE0` IF
read-mask already lives in mmio) and drop the `&mut cpu` params from the PPU.
This severs the cpu↔ppu↔mmio borrow cycle and is the precondition for a clean
bus.

## Preferred architecture (refined after Stage 1): PPU-into-Mmio auto-tick
Stage 1 freed the PPU from the CPU borrow, and the PPU is only referenced inside
`rustyboi-core` (22 sites in `gb.rs`, none in egui/debugger/platform). So instead
of threading a `Bus` through all 105 opcode handlers, **move `ppu: Ppu` into
`Mmio`** and make `Mmio::read`/`Mmio::write` advance all peripherals one M-cycle
per access (`tick_m`). Then:
- Every existing opcode handler call to `mmio.read/write` becomes cycle-accurate
  with **zero handler changes** — the access auto-ticks in program order.
- Simple measurement loads (`LDH A,(n)`, `LD A,(nn)`, `LD A,(HL)` — which are the
  bulk of the failing boundary tests) get correct read timing immediately.
- Only opcodes with **internal (non-memory) M-cycles** (taken JP/CALL/PUSH,
  16-bit INC/DEC, etc.) need explicit `mmio.tick_m()` calls placed at the right
  point — a far smaller, well-enumerated set than "all 105 handlers."
This subsumes the `Bus` design below and is the recommended path; the `delayed_
mmio_writes` queue is then deleted (writes auto-tick inline).

**Caveat (borrow checker):** making `Ppu` a field of `Mmio` means `ppu.step()`
can no longer take `&mut Mmio` — the PPU reads VRAM/OAM/registers that live in
the same struct (self-borrow). Either (a) restructure `Mmio` so memory+registers
are a sub-struct the PPU borrows while the `ppu` field is borrowed disjointly,
or (b) keep `Ppu` a sibling and use the `Bus` design below (thread through
handlers). Both are comparable, multi-day efforts — there is no single-session
shortcut. Pick (a) to avoid touching opcode handlers; pick (b) to avoid
restructuring `Mmio`.

Cost/containment: 22 `self.ppu` → `self.mmio.ppu()` accessor updates in `gb.rs`;
move `step_dma`/`step_serial`/`step_timer`/audio + ppu stepping into `tick_m`;
delete the per-cycle catch-up loop in `step_instruction`. All inside one crate.

## (Superseded) The Bus
```
struct Bus<'a> { mmio: &'a mut Mmio, ppu: &'a mut Ppu, double_speed: bool }
impl Bus {
    fn tick_t(&mut self)            // == today's gb.rs per-dot loop body
    fn tick_m(&mut self)            // 4× tick_t (PPU gated to 2 dots in double-speed)
    fn read(&mut self, a) -> u8     // tick_m (hardware order) then mmio.read
    fn write(&mut self, a, v)       // mmio.write then tick_m (order per gekkio)
    fn read16/write16/internal()    // compose the above
}
```
`tick_t` is literally the current `gb.rs` loop body (step_timer, step_serial,
step_dma, gated step_audio + ppu.step + scheduled STAT, step_lcdc_events). The
existing per-cycle loop and `delayed_mmio_writes` get deleted — their job moves
inline.

## Stages (each ends green against the suite; keep `/tmp/integrated2.json` as oracle)
1. **Break the borrow cycle** — `request_interrupt` on mmio; strip `&mut cpu`
   from PPU. Pure refactor, zero behavior change. Verify suite == baseline.
2. **Introduce `Bus` + `tick_t/tick_m`**, constructed in `step_instruction`.
   Keep the *old* atomic path but route the post-instruction catch-up through
   `tick_t` to prove equivalence. Verify == baseline.
3. **Thread `Bus` through `cpu.step` → `execute` → all 105 opcode handlers**,
   replacing `mmio.read/write` with `bus.read/write`. Mechanical; do it in
   groups (loads, ALU, 16-bit, jumps/calls, CB, stack). After each group, the
   instruction still ticks the *same total* cycles, just inline.
4. **Encode intra-instruction ordering** — the accuracy payload. For each
   opcode, place the read/write/internal ticks at the correct M-cycle per
   gekkio's *Game Boy: Complete Technical Reference* (e.g. `PUSH` = internal,
   write-hi, write-lo; taken `JP` = read-lo, read-hi, internal). This is what
   flips the boundary tests.
5. **`service_interrupt`** — model the 5 M-cycle sequence (2 internal, push-hi,
   push-lo, vector fetch) with the IF/IE re-check at the documented cycle.
6. **HALT/EI/STOP** — interrupt check between M-cycles; HALT-bug and EI-delay
   semantics fall out naturally once ticking is per-M-cycle.
7. **Double-speed** — `tick_m` advances PPU 2 dots (not 4) while timer/divider
   run at full CPU rate; confirm `speedchange` improves.
8. **Delete** the old `gb.rs` catch-up loop, `delayed_mmio_writes`,
   `write_mmio_from_cpu`. Re-run full suite.

## Risk & sequencing
- Stages 1–2 are safe (no behavior change) and de-risk the borrow refactor.
- Stage 3 is large but mechanical; stage 4 is where regressions hide — gate
  each opcode group on the suite, and diff per-category so a bad ordering is
  caught immediately.
- Reference: gekkio GBCTR opcode timing tables; cross-check against blargg
  `instr_timing` + the gambatte suite already wired up.
- Do this on a **branch off committed WIP** (commit the current verified
  serial/IF/LY/OAM-DMA gains first) so worktrees/agents can build on it and a
  bad stage is one `git reset` away.

## Expected payoff
This is the single change that unblocks the boundary-test categories. Realistic
target after a clean stage-4: the bulk of tima/STAT/oamdma/scx_during_m3 sub-
cycle failures clear; <100 becomes reachable once PPU mid-M3 fetch timing (the
remaining PPU-internal piece) is also pinned. Effort: multi-day, staged.
```

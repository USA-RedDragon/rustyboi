# Event-Driven Engine Rewrite — Design & Migration Plan

Status: **design complete, ready to execute on the `engine-rewrite` branch.**
Main is shippable at **660** gambatte-hwtest failures; this rewrite targets the
remaining root that the per-dot engine structurally cannot reach: **100% DMG/CGB
cycle accuracy**.

## Why a rewrite (the finding that forced it)

rustyboi advances time by **stepping every subsystem one PPU dot at a time**
(`cpu/bus.rs::tick_t`), and delivers interrupts per-dot. Gambatte instead has
**one master clock `cc` (T-cycles)** and an **event scheduler**: it runs CPU
opcodes until `cc` reaches the nearest scheduled event, dispatches that event,
and every subsystem derives its state lazily as `(cc - anchor) >> shift`.

Four independent attempts (Phase B, D1, D2, D3) proved the per-dot model cannot
be made cycle-exact incrementally:

- **D1 (proven):** CPU register accesses must resolve at the **M-cycle START cc**
  (`abs_cc` before `tick_m`), not the end. Porting Gambatte's scheduled TIMA +
  start-cc FF04–07 writes fixed the exact 10 `tc00_late_tc01_*`/`tc00_start`
  tests two prior agents could not. → the mechanism is correct.
- **D2/D3 (the wall):** the DIV-write phase is shared by TIMA, the per-dot APU
  frame-sequencer bit-12 edge, and the serial shift clock — AND the TIMA IRQ is
  delivered per-dot while reads resolve at start-cc (**mixed anchors**). Every
  partial config is net-negative (−84 to −125); you "cannot reference DIV and
  TIMA writes to different cc anchors." The only correct end-state moves the
  **entire** DIV-coupled cluster onto one start-cc phase with **scheduled IRQ
  delivery** — which is net-negative until it's *all* done, i.e. not slice-able
  under a zero-regression rule.

Conclusion: the correct architecture *is* Gambatte's event-driven core. Build it.

## Target architecture (from the Gambatte map)

- **One clock** `cc: u64` (T-cycles), advanced `cc += 4` per CPU memory access
  / opcode internal cycle. Boot `cc` = `(8 - stalled) & 0xFFFF`; the documented
  boot DIV phase is reproduced by `div_anchor` (rustyboi's `0x1EA0`/`0xABCC`
  already equal Gambatte's `(0x102A0 - (-0x1C00)) & 0xFFFF` — **no boot-phase
  change needed**).
- **MinKeeper event scheduler**: a small min-heap of absolute-cc event times,
  one slot per source: `{ unhalt, end(frame), blit, serial, oam_dma, gdma/hdma,
  tima, video(ppu), interrupts }`. The CPU loop runs opcodes while
  `cc < min_event_time()`; on reaching an event, dispatch it (which may itself
  advance `cc` and reschedule). Counter-wrap fold at bit-31 subtracts an aligned
  delta from `cc` and every anchor.
- **Lazy subsystems**, each an `anchor` (in cc units) + a derive function:
  - DIV: `(cc - div_anchor) >> 8 & 0xFF`; FF04 write → `div_anchor = cc`.
  - TIMA: `(cc - last_update) >> timaClock[tac]`; overflow IRQ scheduled at
    `last_update + ((256 - tima) << clk) + 3`; divReset/setTac/speedChange
    glitches per `mem/tima.cpp`. **Already ported & verified arithmetically exact
    in D1/D2 — reuse it.**
  - Serial: completion event at `cc - (cc - div_anchor) % P + step*cnt`
    (P,step = 0x100/0x200 DMG, 8/0x10 CGB-fast). Drop `WRITE_CC_OFFSET=8`.
  - PSG/sound: `(cc - last_update) >> (1+ds)`; FS step `(cc>>12)&7`; length
    `((cc>>13)+len)<<13`; PSG::reset/divReset/speedChange folds. **Root 2**:
    re-derive all 4 channels from this single cc (abandon the discrete-FS
    counter) — this is what the 5 APU agents bounced off because the per-dot FS
    edge can't match bit-true cc.
  - PPU/LCD: `(cc - p_now) >> ds`; LyCounter.time_; M0 = `p_now + (predict << ds)`.
    The mode-3 predictor (`compute_m3_length`) and the FF41 decouple are **already
    cycle-exact** (C2 confirmed) — the PPU needs the least change, just re-anchor
    `p_now` to the master cc instead of its own `abs_cc`/`ticks`.
- **Interrupt dispatch**: `cc += 12; +4; +4` (= 20), with the IRQ vector sampled
  **late** (after the +16) so SP-overwrite cancellation works (`interrupter.cpp`).
- **Double speed**: `cc` rate unchanged; each subsystem `>> ds` incoming and
  `<< ds` on scheduled times; sound `>> (1+ds)`. No `step_subdot`/`cpu_t_phase`
  half-dot hacks — they disappear.

## Migration order (each step builds + runs the suite; red is OK on this branch)

The PPU is already exact, so migrate it LAST and least. Start with the CPU
clock + scheduler skeleton, then the DIV-coupled cluster as one unit (the thing
that wouldn't slice on main), then APU root-2, then fold the PPU onto the master
cc, then delete the per-dot scaffolding.

1. **Scheduler + master cc skeleton.** Add the MinKeeper + `cc`. Keep the
   existing per-dot stepping running in parallel, driving `cc` so nothing
   changes yet (net-zero checkpoint). This is the harness everything moves onto.
2. **CPU access cc = start-cc.** Make `read`/`write` resolve effects at `cc`
   (start of access) then `cc += 4`. Reproduce current read-at-cc snapshots as
   the default, not special cases.
3. **DIV-coupled cluster, atomically** (timer + serial + APU-FS edge), all
   scheduled against `cc`, IRQs delivered via the scheduler. This is D3's content
   but now with scheduled (not per-dot) IRQ delivery — the missing piece. Reuse
   the verified scheduled-TIMA port. *Expected: large red, then converges as the
   cluster lands together; target = tima 17→~0, serial-abort 5→0, plus
   speedchange/div tics.*
4. **APU root 2.** Re-derive square/wave/noise/sweep + length + envelope from the
   single `cc` (delete discrete FS). *Target: sound DS length-rate + nr52 (~14–19).*
5. **PPU onto master cc.** Replace `Ppu::abs_cc`/`ticks`/`line_cycle` with
   derivations from `cc` + `p_now`. The predictor/decouple stay. Then the 4
   residual CGB 1-dot nudges should fall out as the m0/getStat derivation becomes
   exact (`cc+2 < m0Time`). *Target: the CGB scx1/2/3/5 nudges + scattered DS.*
6. **m2-IRQ + FF41 read-context** land naturally once STAT events are scheduled
   against the master cc and reads resolve at start-cc.
7. **Delete the per-dot scaffolding** (step 1's parallel path, `cpu_t_phase`,
   `step_subdot`, the tuned offset families). Final convergence pass.

## Regression discipline (per the approved rule)

On this branch, red is expected mid-migration. The rule is unchanged in spirit:
**every red test at a checkpoint must be attributable to a not-yet-completed
step in this list.** A checkpoint is mergeable to main only when it is
net-positive vs 660 with zero unexplained regressions. Keep a per-step
`failed` ledger in commit messages.

## Reusable assets already in the tree (main)

- `timer.rs`: `abs_cc` + `div_anchor` (A1 unify — already Gambatte's DIV model).
- The verified scheduled-TIMA port (D1/D2 — re-apply from those agents' diffs).
- `ppu/controller.rs`: exact M3Start predictor (C2) + FF41 decouple
  (`reported_mode0_dot_value`) — the hard PPU work is done.
- `audio/controller.rs`: the absolute APU master clock seed (`8a552a0`).
</content>

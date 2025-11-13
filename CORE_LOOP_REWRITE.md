# Core-Loop Rewrite — per-access CPU cycle timing

Status: **planned, ready to execute on a `core-loop` branch after the surgical vein is mined.**
Main is shippable at **570/5257 (−80.9%)**. This rewrite targets the one convergent
root that ~10 agents (M5,M6,M18,M19,M24,M25,M27,M28-partial) traced the *majority*
of the remaining failures to. It is the analog of the (successful) event-driven
engine rewrite, and the path from ~570 toward 100%.

## The convergent root (one sentence)

rustyboi advances time **per PPU dot** and resolves each CPU memory access at a
**fixed offset from the dot counter** (`access_cc = abs_cc + CC_OFF`, e.g. +5 for
timer); Gambatte advances `cc` **per memory access** (`cc += 4`) and resolves every
access at the **exact intra-instruction cc**. The fixed-offset approximation is
right on average but off by **1–4 cc** for the `_1`/`_2` "straddle pair" tests that
read a register one CPU access apart across a mode/IRQ/length boundary — so one of
the pair always lands on the wrong side. This is globally co-calibrated: a uniform
offset change is a strict 1-for-1 swap, and a global ISR-latency change measured
**+1041**. No scoped fix works; only making the access cc *exact* does.

## What's already in place (reuse — do NOT rebuild)

The engine rewrite + follow-ups built most of the scaffolding:
- **Single master cc** `abs_cc` (timer.rs), Gambatte `cycleCounter_` semantics; DIV
  = `(abs_cc - div_anchor)`. All subsystems (timer, APU single-counter, serial, PPU
  via `p_now`) derive from it.
- **Canonical access cc**: `mmio::access_cc()` = `abs_cc + 5` (the M1 read phase).
- **Read-at-cc snapshots** (cpu/bus.rs `read`): FF41 (`get_stat_mode3to0_at_cc`, M20),
  NR52 (M22), IF/serial/wave, timer regs (M1). These compute peripheral state at
  `access_cc` instead of returning a dot-stepped register.
- **Write-at-cc / delayed-visibility latches**: timer FF04-07 (M1/M8, write-before-tick),
  serial (M8), WY `wy1`/`wy2` (M26, applied at `write_cc + (1+cgb)`), SCY `scy_delayed`
  (M28, +2cc CGB). These make a register write visible to the PPU/peripheral at the
  Gambatte-correct cc, not immediately.
- **m0_time_master** (controller.rs): the Gambatte-exact mode-0 boundary in master cc;
  confirmed dot-exact (M25 — the *boundary* is correct, only the *read cc* is off).

So the remaining work is NOT to invent per-access cc — it's to make it **exact and
universal**, replacing the fixed `CC_OFF` fudges and covering the ISR-dispatch path.

## The precise residual, by facet (from the agents)

1. **ISR-dispatch cc** (M25): the m2-STAT-interrupt-entry → first-FF41-read path is
   ~4 cc off vs Gambatte. The boundary is right; the ISR reads it at the wrong cc.
   Gates m2int_m3stat_ds, parts of halt/irq_precedence/m0enable. `cpu/sm83.rs`
   `service_interrupt` (currently 3 internal cycles + pushes = 20cc). The latency is
   co-calibrated — the fix must make the ISR's first-access cc *exact*, not shift a
   global constant.
2. **Read-cc straddle** (M19/M20/M24): FF41/STAT/IF reads one access apart land on the
   wrong side of the boundary. M20 fixed the steady case (`access_cc + 2 < m0Time`);
   the residual `_2`/`_ds_2` need the access cc to be exact (not `abs_cc+5` fixed).
   Gates enable_display, ly0, lcd_offset, m2int_m3stat, scx_during_m3 reads.
3. **Write-visibility cc** (M27): mid-M3 SCX writes (and the residual scy/window) land
   ~4 dots off. SCY fixed via `scy_delayed=+2` (M28); SCX still needs it. Gates
   scx_during_m3 writes, some window/dma.
4. **Opcode granularity** (M18): two reads a single `nop` apart can produce
   byte-identical peripheral traces because the per-dot resolution + fixed offset
   collapses them. The deepest facet — the access cc must reflect the true
   intra-instruction position.

## Target model

Resolve every CPU memory access (read AND write) and the ISR vector dispatch at the
**exact cc the access occurs** — the `abs_cc` at the *start of that access's M-cycle*,
with no fixed `CC_OFF` fudge — and make instruction-internal cycles advance `abs_cc`
so consecutive accesses differ by their true cc. Equivalent to Gambatte resolving at
`cc` then `cc += 4`. The per-dot peripheral stepping (PPU rendering, the divider) can
stay — only the *access-resolution cc* changes.

## Phased migration (each on the `core-loop` branch; red allowed if attributed)

The risk is that everything reads the access cc, so changing it moves many clusters
at once — these phases must converge together, like the engine rewrite's atomic
DIV-coupled phase. Order = least-coupled first to build confidence.

- **CL1 — exact start-of-access cc, replacing `CC_OFF`.** Make `read`/`write` resolve
  the peripheral at the true start-of-M-cycle `abs_cc` (before the access's 4 dots
  tick), uniformly, and *remove* the `+5`/`CC_OFF` and the per-register delayed-latch
  fudges (`scy_delayed`, `wy1`, etc.) that compensate for the wrong resolution point.
  EXPECTED: large transient churn as the fudges come out; the end state must net ≥
  baseline. This is the net-zero-or-better refactor checkpoint that makes the access
  cc honest. *Validation: the M20/M22/M26/M28 wins must hold with the fudges removed
  (they become free once the cc is exact).*
- **CL2 — ISR-dispatch cc.** Make `service_interrupt` advance `abs_cc` so the ISR's
  first instruction fetch/read lands at Gambatte's exact cc (the `cc += 12; +4; +4`
  with late vector sampling, anchored so the post-dispatch access cc matches). Targets
  the m2int/halt/irq_precedence straddles. Verify non-STAT interrupts hold.
- **CL3 — opcode-granular internal cycles.** Ensure instruction-internal cycles
  (`internal_cycle`, the prefetch, multi-byte ops) advance `abs_cc` so two accesses a
  `nop` apart differ by 4cc at the resolution point — the M18 quantization fix.
- **CL4 — converge + delete scaffolding.** Remove the now-dead `access_cc` constant,
  the delayed-visibility latches subsumed by CL1, and the read-at-cc special-cases
  that become the uniform path. Final convergence pass; sweep the remaining straddle
  clusters (enable_display, ly0, lcd_offset, scx_during_m3, dma m3speedchange, ch2).

## Regression discipline (per the standing rule)

Red is expected mid-migration on the branch. Every red test at a checkpoint must be
attributable to a not-yet-completed CL phase. Mergeable to main only when net-positive
vs the current main with zero *unexplained* regressions. Keep a per-phase `failed`
ledger in commit messages. Single-speed and the non-CPU-timing clusters (which already
pass) must converge back.

## Recon to run first (CL0)

Before CL1, a read-mostly agent should produce the exact map: every site that reads
`access_cc()`/applies a `CC_OFF`/uses a delayed-visibility latch, every read-at-cc
snapshot in bus.rs, and the `service_interrupt` cc accounting — so CL1 knows the full
set of fudges to remove atomically. (Much of this is already in memory
`ppu-mode3-length-lever.md`.)

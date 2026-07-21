# Known Failures — every failing ROM, with proof

42 test cases fail across the 28 suites: 4 in gbmicrotest, 9 in gambatte, 29 in gbc_hw_tests. Every one of the 42 is adjudicated below. Entries make **no assumptions**: every claim is tagged with its provenance, reproducible claims include the command that re-verifies them against this tree, and claims that outrun their evidence are labelled **PROVISIONAL** rather than dressed up.

Counts measured at `87e29aa3` after an explicit `cargo build --release -p rustyboi-test-runner` (this tree has a documented stale-binary trap that has produced false PASS, false FAIL *and* a committed false README count — never quote a number produced without a rebuild) **[R]**:

```sh
$ cargo build --release -p rustyboi-test-runner   # MANDATORY: never skip
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh gbc_hw_tests
Ran 342 total tests.
29 total failures.
Ran 301 CGB tests.
29 CGB failures.
Ran 41 DMG tests.
0 DMG failures.
PASS  gbc_hw_tests         passed=313/342 (floor: passed>=313)
```

Note **342**, not the 343 earlier revisions of this file quote: `tac_set_enabled`'s CGB
column was removed from the manifest after adjudication — its capture is
structurally defective (DMG-shaped 15364 bytes where every other CGB capture of
that ROM family is 31748), so the row count itself dropped by one. **The whole
DMG column is now green.**

The other two suites are unchanged at this revision **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh gbmicrotest gambatte
PASS  gbmicrotest          passed=509/512 (floor: passed>=509)
PASS  gambatte             passed=5248/5257 failed=9 (floor: failed<=9)
```

Two suites are cited repeatedly below as *counter-oracles* — the ones that a
"fix" for a contested gbc_hw_tests row would break. Both are green **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh mooneye age
PASS  mooneye              passed=193/193 (floor: passed>=193)
PASS  age                  passed=56/56 (floor: passed>=56)
```

So the three suites that carry failures stand at 4 + 9 + 29 = **42**. The corpus
total is **6842** (the previous revision's 6843 less the one retired
`tac_set_enabled` CGB row); that total is *derived*, not re-measured here,
because re-running the full 28-suite battery was out of scope for this pass.
Treat 6842 as arithmetic on the three measured suites plus an unchanged
remainder, and re-measure before quoting it as a headline.

> **The README suite table is regenerated separately and is not the source here.** This document's arithmetic comes only from the runs pasted above. Fixing or confirming the README is a separate change and is out of scope for this file.

**A gitignored-asset warning, because it silently falsified results six times in one day** — one of which reached `main` as a regression. `gb-test-roms/`, `bios/` and `test-roms/build/` are gitignored, so a fresh worktree does not have them and the suite will happily "pass" while skipping nearly everything. Before quoting any number, confirm the run reports **`Ran 342 total tests`** for gbc_hw_tests. A row count below that means missing ROMs, not progress. `test-roms/build/` must be a **real copy**; symlinks are fine for the other two.

Provenance tags:

- **[R]** Reproducible here — run the command shown against this checkout.
- **[V]** Verified against third-party references built from source (SameBoy, libgambatte, GateBoy sources) or by instrumented traces/experiments; the method is described where cited.
- **[D]** Documented upstream — Pan Docs, gekkio's gb-ctr, or the test author's own files.

Verdict classes:

- **LOGIC-IMPOSSIBLE** — real-hardware captures pin the *same physical quantity* to different values in different capture sessions. A deterministic emulator must pick one value, so for each such family the failure count is forced by arithmetic, not by a modeling gap.
- **ORACLE-CONFLICT** — two *different* real-hardware oracles, each independently credible, demand incompatible answers for the same behaviour. Distinct from LOGIC-IMPOSSIBLE, which is one oracle disagreeing with *itself* across sessions: here each oracle is internally consistent, so the failure is not forced by arithmetic — it is forced by a choice about *which oracle is authoritative*, and that choice cannot be made from the captures alone. Resolving one requires establishing provenance (which silicon revision, which physical unit, which capture is intact) — not emulator work. **As of 2026-07-21 this is the single largest class in the document**: it covers 25 of the 29 gbc_hw_tests rows. Note that an ORACLE-CONFLICT can dissolve: the former `oam_echo_ram` conflict was resolved as a **revision mismatch** once both sides' `rev=` pins were compared, and both oracles now pass simultaneously (its entry has since been deleted, the family being green). That precedent is live for §1b.
- **BLOCKED-ON-ORACLE** — a hardware-correct answer exists in principle, but no captured/documented value exists anywhere; grading the emulator's own output would assert nothing. Unblockable with real hardware.
- **UN-GRADEABLE** — the ROM writes no stable verdict anywhere.
- **ANALOG** — the behavior has no register-level correlate; reproducing the value without a physical derivation would be a single-observable fit.
- **OPEN-TARGET** — a genuine modeling gap with a real-silicon (or author-endorsed deterministic) oracle. Unlike the classes above these are *fixable*; they are open accuracy work items, not excused floors.

---

## gbmicrotest — 3 failures (509/512)

The suite's protocol is `FF82==0x01` pass with `FF80`=actual, `FF81`=expected (60 frames). 13 additional no-verdict ROMs are graded via `mem <addr>=<val>` with disassembly-justified bytes (see the manifest header for per-test provenance).

`temp.gb` is **EXCLUDED** (no longer graded, so the total is 512 not 513): it is a `nop` dev stub that writes no `FF82` verdict of any kind, so grading it asserts nothing (see below and `gen_manifests.py:line_for`). It is dropped on the same basis as `cpu/corrupted_stop` and `timers/tac_set_everything`, not counted as a permanent fail.

### 1–2. `500-scx-timing.gb` and `minimal.gb` — BLOCKED-ON-ORACLE

**Failure [R]:** `FF82=64 (want 01); FF80=2B FF81=0B` — that signature is *uninitialized HRAM*: the ROM never writes FF80–FF82 at all.

**The two ROMs are the same ROM [R]:**

```sh
$ md5sum gb-test-roms/gbmicrotest/minimal.gb gb-test-roms/gbmicrotest/500-scx-timing.gb
719e6f331d16d03443aa43ed76fb5ced  (both)
```

**What it does [V]:** dual-HALT TIMA measurement of mode-3 length at SCROLL=0; the raw TIMA count is written to VRAM `$8000`. Patched-ROM probes confirm the flow (halt 1 wakes at line-1 mode-2, halt 2 at line-1 HBlank).

**Why it cannot be graded [V]:** the author's only hardware record is *relative* ("overhead 65" sweep rows for DMG and AGS). Both rows decode exactly to the M-cycle-grid quantization `(scx + ((−scx−e) mod 4))/4` with DMG e=0, AGS e=2 — and a patched SCROLL=0..7 sweep reproduces **all 8 DMG deltas**, so the *physics* is validated. But no **absolute** pass byte exists anywhere: the GateBoy/MetroBoy harnesses grade only the `FF80==FF81 && FF82` self-verdict (no independent expected values are stored upstream), and a documents-only derivation stacks ≥6 sub-M-cycle constants. Grading the emulator's own `$4A` would assert nothing.

**Unblock:** capture the absolute byte on real DMG hardware (measurement → cart SRAM → reader). +2 tests.

### `temp.gb` — UN-GRADEABLE (dev stub), now EXCLUDED

**Content [R]:**

```sh
$ xxd -s 0x100 -l 4 gb-test-roms/gbmicrotest/temp.gb   # entry: nop; jp $0150
00000100: 00c3 5001
$ xxd -s 0x150 -l 32 gb-test-roms/gbmicrotest/temp.gb  # $0150: all zero bytes (nop sled)
```

**What happens [V]:** PC slides through zeroed ROM as `nop`s, continues into VRAM-as-code, and collapses into an `RST $38` loop whose pushes walk SP through IO/OAM/WRAM. Deterministic on silicon — but the trajectory executes boot-logo VRAM bytes as opcodes and no capture of the end state exists. **There is no verdict write of any kind.** Confirmed [R]: its graded end state is `FF82=64 FF80=2B FF81=0B`, byte-identical to `minimal`/`500-scx-timing` (which are cmp-verified never to write those bytes) — i.e. pure `skip_bios` HRAM residue. Because the `memauto` oracle asserts `FF82==0x01` and this ROM never makes that claim, grading it asserts nothing, so it is excluded rather than reported as a permanent fail.

### 3. `halt_op_dupe_delay.gb` — ANALOG

**Failure [R]:** `FF80(actual)=01 FF81(expected)=55`.

**The digital chain [V/D]:** the ROM arms the STAT/HBLANK interrupt with IME=0, then HALTs. IF bit 1 is already latched before the HALT, and IF bits are sticky (set on the interrupt line's rising edge; cleared only by CPU write, dispatch, or reset — none occur here). Every register-level model therefore falls through the HALT immediately and reads DIV ≈ **0x01**.

**Proof there is no register-level path to 0x55 [V]** (three independent eliminations):

1. SameBoy built from source produces 0x01 and **fails identically** (the sibling `halt_op_dupe` passes on both).
2. GateBoy's own source (`GateBoyInterrupts.cpp`): IF bit 1 is `LALU_FF0F_D1p.dff22` — a rising-edge sticky DFF; GateBoy also computes 0x01. GateBoy has **no gate-level SM83** (register-level CPU core; its README flags async glitches as unmodeled).
3. Force-clearing IF.1 at the HALT wakes at the *next* HBLANK (1 line, DIV≈0x02) — not 47 lines. Per-line traces show LY=2..53 are byte-identical (same STAT edge, same phase every line; 456 T is a multiple of 4, so zero drift): **nothing digital distinguishes LY=49.**

**What 0x55 means [D]:** DIV ∈ [0x5500,0x55FF] ⇒ the CPU slept ~48 scanlines and woke at LY≈49's HBLANK. That requires (a) the latched IF bit not to count at the HALT (the async STAT-write runt-pulse clearing a latched IF — a real, observed DMG glitch), and (b) ~47 digitally-identical HBLANK edges to be ignored before the 48th is honored. The consistent physical explanation is an analog node left at an intermediate level, drifting to threshold over ~5 ms (an RC time constant of the specific die). The number 47 is stored nowhere in the machine; encoding it would be a fit to a single observable from a single unit.

**Unblock (partial):** run the ROM on additional real DMG units. Exact 0x55 on an independent unit would *reopen* the digital question; a different or unstable value confirms the analog verdict.

---

## gambatte — 9 failures (5248/5257; floor gated `failed<=9`)

All 9 are CGB dumper tests (OAM-DMA/GDMA conflicts + the FEXX region). Background facts:

- **Oracle provenance [V]:** gambatte's own `testrunner.cpp` grades only `_out`-named ROMs and `.png`s; it never reads these `.bin`/`.dump` files. They are **external real-hardware captures** shipped in `gambatte-core/test/hwtests/` (rig artifacts — logs and LCD photos — sit beside several of them), not emulator output.
- **The sibling ROMs are different ROMs [R]:** the `_1/_2/_3` variants are deliberate *phase probes* — each shifts the conflict timing by a few M-cycles. All five md5s are unique, and the `_1`→`_2` delta is a NOP insertion plus an `ldh (n),a`→`ld (c),a` swap:

```sh
$ md5sum gb-test-roms/gambatte/oamdma/oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_{1,2}.gbc \
         gb-test-roms/gambatte/oamdma/oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_ds_{1,2,3}.gbc
# five distinct hashes
```

  The differences between sibling *dumps* in OAM proper are therefore real, deterministic phase effects — and they are modeled correctly (those bytes pass).
- **Region structure [V/D]:** CGB `FEA0–FEFF` reads through `addr & 0xE7`, mirroring canonical 8-byte rows ×4. During an OAM-DMA, a concurrent GDMA "conflict" write lands in `ioamhram[p & 0xE7]` where `p = src&0xFF` (gambatte `memory.cpp:348-373`; same model here) — reaching **odd columns only**, and per the tick arithmetic the ds/ROM-source variants' conflicts never reach the FEA0+ tail at all.
- **The impossibility lemma [V]:** every failing byte below images a physical cell **unreachable by the sibling ROMs' code deltas** (even-column residue cells, or uninitialized WRAM the ROM never fills), yet the captures pin that same quantity to **different values in different power-on sessions**. Every disagreement is a 1–2-bit flip (48↔4A, B4↔B6, BD↔BF, 08↔18, FF↔F7) — marginal-bit behavior in exactly the region gekkio's gb-ctr marks "OAM DMA bus conflicts: TODO" [D]. A deterministic emulator must commit to one value per cell.

### The cross-capture disagreement evidence [R]

The same residue cells across the capture set (extract at any time):

```sh
$ python3 - <<'EOF'
import glob
for f in sorted(glob.glob("gb-test-roms/gambatte/oamdma/*oamdumper*.dump")):
    d=open(f,'rb').read()
    print(f"{f.split('/')[-1]:60s} A0={d[0xA0]:02X} A5={d[0xA5]:02X} A7={d[0xA7]:02X} C6={d[0xC6]:02X}")
EOF
```

| capture | A0 | A5 | A7 | C6 |
|---|---|---|---|---|
| `gdmasrc0000_gdmalen04_1` (passes) | 18 | 48 | BD | B6 |
| `gdmasrc0000_gdmalen04_ds_1` | 18 | 48 | **BF** | B4 |
| `gdmasrc0000_gdmalen13_ds_1` | **08** | **4A** | BD | B4 |
| `gdmasrcC000_gdmalen13_1` | 18 | BD | BF | **B4** |
| `gdmasrcC000_gdmalen13_2` (passes) | 18 | BD | BF | B6 |
| `gdmasrcC000_gdmalen13_ds_1` | **08** | **CA** | BF | B6 |
| `gdmasrcC000_gdmalen13_ds_2` | 18 | **C8** | BD | B6 |
| `gdmasrcC000_gdmalen13_ds_3` | 18 | **4A** | BD | B6 |
| `gdmasrcC0F0_gdmalen13_1` | 18 | 42 | 40 | **B4** |

The same cells take different values across sessions with no correlation to the ROM parameters. An exhaustive subset search over the tail-graded captures' 12 conflicting cells shows **at most 2 are jointly satisfiable** — and the current seeds achieve exactly that maximum (the only alternative choice is a tie at the same count) [V].

### 5. `oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_1` — LOGIC-IMPOSSIBLE

Fails at 0xC6: expected 0xB4, produced 0xB6 **[R]**. The `_2` phase-probe's capture demands 0xB6 at the same residue cell — and `_2` **passes** [R: absent from the failing list]. The cell is not conflict-reachable by the `_1`→`_2` code delta [V], so the B4/B6 disagreement is session flicker, not phase: satisfying `_1` breaks `_2` one-for-one.

### 6–8. `…gdmalen13_oamdumper_ds_1 / _ds_2 / _ds_3` — LOGIC-IMPOSSIBLE

Fail at 0xA0 (exp 0x08, got 0x18) and 0xA5 (exp 0xC8 / 0x4A, got 0x48) **[R]**. Three phase-probe captures demand **three different bytes at 0xA5** (CA / C8 / 4A) and two at 0xA0 (08 / 18 / 18) [R: table above] at cells their conflicts cannot reach [V]. The produced values (A0=18 / A5=48) are the ones demanded by the **currently-passing** `gdmasrc0000_gdmalen04` captures [R: table]; moving toward any `_ds_N` breaks a passing capture one-for-one.

### 9. `oamdmasrcC000_gdmasrc0000_gdmalen04_oamdumper_ds_1` — LOGIC-IMPOSSIBLE

Fails at 0xA7: expected 0xBF, got 0xBD **[R]**. 0xA7 across sessions ∈ {BD, BF} [R: table]; the passing `gdmalen04_1` demands BD. Forced choice.

### 10. `oamdmasrcC000_gdmasrc0000_gdmalen13_oamdumper_ds_1` — LOGIC-IMPOSSIBLE

Fails at 0xA0: expected 0x08, got 0x18 **[R]**. Same pivot as §6–8 (0x08 vs the passing captures' 0x18).

### 11. `oamdmasrcC000_gdmasrcC0F0_gdmalen13_oamdumper_1` — LOGIC-IMPOSSIBLE

Fails at 0xC6: expected 0xB4, got 0xB6 **[R]** — the same B4/B6 session-flicker cell as §5, pinned oppositely to the passing `gdmalen13_2`. (This ROM's *distinctive* bytes — the uninitialized-WRAM images from its GDMA source running past the fill window, C0F0+320−1 = C22F ≥ C200 — all pass; its companion `vramdumper` passes outright.)

### 12. `fexx_read_reset_set_dumper.gbc` (CGB) — LOGIC-IMPOSSIBLE

Fails at SRAM offset 0xA5 (= FEA5, dump 1 of 3 = the power-on pass): expected 0x48, got 0x4A **[R]**. The two CGB fexx captures — pristine reads of the same untouched region — disagree at exactly four canonical cells (each mirrored ×4 through the `&0xE7` fold) **[R]**:

```sh
$ python3 - <<'EOF'
a=open("gb-test-roms/gambatte/fexx_ffxx_dumper_cgb.bin",'rb').read()
b=open("gb-test-roms/gambatte/fexx_read_reset_set_dumper_cgb.bin",'rb').read()
print([(hex(i),hex(a[i]),hex(b[i])) for i in range(0x100) if a[i]!=b[i]])
EOF
# 16 cells = canonical {A5: 4A vs 48, C3: 7F vs 5F, E3: 3A vs 7A, E4: 10 vs 00} × 4 mirrors
```

The produced values match the `ffxx` capture — which **passes**. Max 1 of 2.

Note on the DMG fexx variants (which pass): their power-on OAM window FE00–FE9F is excluded from grading because the two DMG references themselves disagree on **105/160** OAM bytes for the identical power-on while agreeing **0/96** on the FEA0+ region the tests are named for [R]:

```sh
$ python3 - <<'EOF'
a=open("gambatte-core/test/hwtests/fexx_ffxx_dumper_dmg08.bin",'rb').read()
b=open("gambatte-core/test/hwtests/fexx_read_reset_set_dumper_dmg08.bin",'rb').read()
print(sum(1 for i in range(0xA0) if a[i]!=b[i]), "/160 OAM;",
      sum(1 for i in range(0xA0,0x100) if a[i]!=b[i]), "/96 FEA0+")
EOF
```

### 13. `oamdmasrc8000_gdmasrcC000_2xgdmalen09_oamdumper_1` — LOGIC-IMPOSSIBLE

Fails at OAM[0x13]: expected 0xF7, got 0xFF **[R]**.

**The cell [V]:** OAM[0x13]'s last writer in this ROM is the GDMA conflict with source byte C113 — a WRAM cell the ROM never initializes. The `srcC000` twin ROM drives a **byte-identical conflict stream** through the same cell, and its capture demands **0xFF** — while this capture demands **0xF7** (a single bit-3 flip) **[R]**:

```sh
$ python3 - <<'EOF'
D="gb-test-roms/gambatte/oamdma/"
a=open(D+"oamdmasrc8000_gdmasrcC000_2xgdmalen09_oamdumper_1.dump",'rb').read()
b=open(D+"oamdmasrcC000_gdmasrcC000_2xgdmalen09_oamdumper_1.dump",'rb').read()
print(f"src8000 capture: OAM[0x13]=0x{a[0x13]:02X};  srcC000 capture: OAM[0x13]=0x{b[0x13]:02X}")
EOF
```

The `srcC000` twin **passes**. Poking C113=F7 flips which of the two passes — an exact 1-for-1 swap [V]. Same uninitialized cell, two sessions, two values: session flicker, not a computable operand (no deterministic mechanism produces F7 from the actual bus contents [V]).

---

## gbc_hw_tests — 29 failures (313/342)

Adopted from AntonioND/gbc-hw-tests (real-device SRAM captures; see SUITES.md
for grading provenance and the revision caveat). Each ROM is graded per
*column*: CGB (`rev=cgbe`, vs `real_gbc.sav`), AGB (`rev=agb`, vs `real_gba.sav`
where the dir ships one, else `real_gba_sp.sav`), and for DMG-flagged ROMs a DMG
column. A "row" below is one (ROM, column) case, which is what the 29 counts.

**This section was rewritten on 2026-07-21 after a large day of fixes took the
suite 284/343 → 313/342** (59 failures → 29, and one row retired). Every family
the previous revision described as failing that is now green has had its entry
**deleted**, not left standing. Those are:

| retired family | rows | how it closed |
|---|---:|---|
| `dma/hdma_halt` | 2 | manifest `skip=` extended to `0x3-0x6,0x9-0xA` — the previous revision's own recommendation |
| `timers/tac_set_enabled` (CGB) | 1 | column removed from the manifest (defective capture); this is why the suite is 342, not 343 |
| `timers/tac_set_enabled` (AGB) | 1 | fixed |
| `dma/dma_valid_sources_dmg_mode` | 1 | fixed — the DMG column is now green |
| `memory/oam_echo_ram_lcd_on` | 2 | fixed (OAM-lock assertion window) |
| `lcd/mode2` | 2 | fixed (4-cell OAM-lock edge) |
| `lcd/lcd_frame_timings/*` | 21 | 42 → 21 rows across the day's PPU/IF/timer fixes |

The single largest structural change is that **`lcd/mode3`, `dma/hdma_timing_fine`
and the bulk of `lcd/lcd_frame_timings` have all converged onto the same shape**:
a small, uniform, *fully characterized* offset that we can close at will — and
that closing it craters an independent, currently-passing real-silicon suite.
They are ORACLE-CONFLICTs, not modelling gaps, and they are now the majority of
what remains.

### Method — the runner diagnoses this itself now

**The `/tmp/probe` scratch crate that previous revisions of this file built is no
longer needed and should not be rebuilt.** The runner carries the two
diagnostics that replace it, and unlike the probe they grade through exactly the
same path as the suite (no drift between what you analyse and what is scored):

- **`RB_SRAM_VERBOSE=1`** — instead of bailing at the first mismatching cell,
  print *every* one as `0xOFFSET:want=0xWW,got=0xGG`, prefixed by the count.
  This is what every cell-level number in this section is derived from.
- **`RB_SRAM_TRACE=<rom-substring>|1`** — additionally attribute each mismatching
  cell to the instruction that wrote it, as an `SRAM_BLAME` line
  (`rustyboi-test-runner/src/sramtrace.rs`). `1` or `all` traces every
  SRAM-graded case; any other value is a substring matched against the ROM path,
  which is what you want when picking one row out of 342.
  `RB_SRAM_TRACE_LIMIT` raises the 4096-line cap on the chronological log (the
  blame map itself is always complete). Both force `--jobs 1`.

`SRAM_TRACE` works without any core callback: the dump path already steps one
instruction at a time, so it snapshots `save_ram()` around each step and any byte
that changed was written by the instruction that just retired. On top of that it
resolves **provenance** — which IO register the stored byte was read from — by
shadowing the eight CPU register slots with an "origin" IO address, seeded by the
three loads that can reach IO space (`ldh a,(n8)`, `ld a,(c)`, `ld a,(nn)` with
`nn >= FF00`) and propagated across `ld r,r'`. It also resolves upstream `.sym`
symbols. A line looks like this **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_SRAM_VERBOSE=1 RB_SRAM_TRACE=tac_set_disable \
    tools/run-suites.sh gbc_hw_tests 2>&1 | grep SRAM_BLAME
SRAM_BLAME off=0x638E want=0xFD got=0xFC pc=0x0342 cc=4193676 src=A from=FF05(TIMA) sym=Main.loop+0xEE
SRAM_BLAME off=0x6796 want=0xFF got=0xFE pc=0x03D4 cc=4288716 src=A from=FF05(TIMA) sym=Main.loop+0x180
SRAM_BLAME off=0x6B94 want=0x01 got=0x00 pc=0x0085 cc=1787896 src=A from=-        sym=memset+0x1
```

**This tool is the reason several verdicts in this section are firm rather than
guessed, and its absence is why earlier analyses were wrong.** The `from=` field
is the specific fix: a block of `0xE0`/`0xE1`/`0xE3` bytes in these captures was
analysed across several agent-runs as "STAT mode bits, `0xE0` = mode 0" when the
bytes were really **`IF` (FF0F)** reads with `0xE0` meaning *no interrupts
pending*. Nothing about the byte values distinguishes those readings; only the
provenance does. Read `from=` before theorising about any cell.

Upstream ships **source**, not just ROMs: 556 `.asm`, 180 `.sym`, and ~30 `.txt`
author notes documenting expected results. `sync_gbchwtests_roms` copies only
`*.gbc` and `real_*.sav`, so the source is not in-tree; the notes cited below are
**[D]** against the pinned upstream ref **[R]**:

```sh
$ git clone https://github.com/AntonioND/gbc-hw-tests && cd gbc-hw-tests
$ git checkout 631e60000c885154a8526df0b148847f9c34ce42
$ find . -name '*.asm' | wc -l ; find . -name '*.sym' | wc -l ; find . -name '*.txt' | wc -l
556
180
30
```

The 180 `.sym` files are what turn `SRAM_BLAME`'s raw PC into the
`sym=stat_read_test_delay_gbc_0+0x4AF` field; `tools/run-suites.sh` places them.

Several sections quote a per-family roll-up. They all use this one helper,
referred to as **`fams.py`**, run over a captured verbose log **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_SRAM_VERBOSE=1 \
    tools/run-suites.sh gbc_hw_tests > /tmp/verbose.txt 2>&1
$ cat > /tmp/fams.py <<'PY'
import re, sys, collections
rows = []
for ln in open(sys.argv[1]):
    m = re.match(r'FAILED: (\S+) (?:CGB|DMG) (\S+): SRAM dump \S+: (\d+) diffs: (.*)', ln.strip())
    if m:
        rows.append(m.groups())
def fam(rom):
    p = rom.replace('gb-test-roms/gbc-hw-tests/', '')
    if 'lcd_frame_timings' in p:
        return 'lcd/lcd_frame_timings/' + p.split('lcd_frame_timings/')[1].split('/')[0]
    return '/'.join(p.split('/')[:2])
agg = collections.defaultdict(lambda: [0, 0])
xor = collections.Counter()
for rom, ref, nd, cells in rows:
    a = agg[fam(rom)]; a[0] += 1; a[1] += int(nd)
    if 'lcd_frame_timings' in rom:
        for c in cells.split():
            w, g = re.match(r'0x[0-9A-F]+:want=0x([0-9A-F]+),got=0x([0-9A-F]+)', c).groups()
            xor[int(w, 16) ^ int(g, 16)] += 1
print("%-40s %5s %8s" % ("family", "rows", "cells"))
for k in sorted(agg):
    print("%-40s %5d %8d" % (k, agg[k][0], agg[k][1]))
print("%-40s %5d %8d" % ("TOTAL", sum(v[0] for v in agg.values()), sum(v[1] for v in agg.values())))
t = sum(xor.values())
print("\nlcd_frame_timings XOR(want^got): %s" % dict(xor.most_common()))
print("  touches STAT mode bits (&0x03): %d of %d" % (sum(v for k, v in xor.items() if k & 3), t))
print("  touches LYC bit        (&0x04): %d of %d" % (sum(v for k, v in xor.items() if k & 4), t))
PY
$ python3 /tmp/fams.py /tmp/verbose.txt
family                                    rows    cells
dma/dma_timing_lcd_on                        2       90
dma/hdma_timing_fine                         2       64
lcd/lcd_frame_timings/ly_equals_lyc          8      378
lcd/lcd_frame_timings/mode1                  7      159
lcd/lcd_frame_timings/mode2                  6      642
lcd/mode3                                    2       22
serial/sc_change_freq_gbc                    1     1895
timers/tac_set_disabled                      1        3
TOTAL                                       29     3253

lcd_frame_timings XOR(want^got): {3: 746, 1: 208, 2: 189, 4: 31, 6: 4, 5: 1}
  touches STAT mode bits (&0x03): 1148 of 1179
  touches LYC bit        (&0x04): 36 of 1179
```

### Three measurement traps that invalidate naive triage

All three produced *wrong classifications in earlier revisions of this very
document*. Read this before quoting any cell count.

**Trap 1 — a mismatch count at the default budget is not a proximity metric.**
The suite runs a flat 800-frame budget. A ROM that has not finished writing its
result table by frame 800 shows its unwritten tail as mismatches, and those
counts swamp the real ones. `lcd/mode3` was the live example: it read **647**
diffs at 800 frames and **11** at 4000, because 636 of the 647 were bytes the ROM
had simply not written yet. A previous revision read those 647 cells as "a value
error" and built a whole verdict on truncation noise. The manifest now carries
`frames=3000` on both `lcd/mode3` rows **[R]**, so the family reports its true 11
cells per row — but the trap is general: **if a family's diff count is large,
check for unwritten `0xFF` before theorising.**

```sh
$ grep -c 'frames=' rustyboi-test-runner/suites/gbc_hw_tests.manifest
14
```

**Trap 2 — the edge-displacement predicate is circular.** The `ours[i] ==
ref[i±1]` test is conditioned on *mismatching* cells, and mismatching cells sit
at edges by construction — which is exactly where `ours[i] == ref[i-1]` holds for
**any** locally-displaced edge. It therefore scores ~96% for essentially any
hypothesis and discriminates nothing. A previous revision used it to "establish"
a uniform one-M-cycle late latch across `lcd_frame_timings`; the unconditioned
test (compare our table against the reference offset by one, over *all* cells)
refuted it outright, making every row roughly an order of magnitude worse. **Do
not cite this predicate as evidence for anything.** It also under-reports in the
other direction: on a single contiguous 4-cell edge it scores 0%, because only
the outermost cell of a run has a differing neighbour.

**Trap 3 — `0xE0`/`0xE1`/`0xE3` in these captures are `IF` (FF0F), not STAT.**
`0xE0` is *no interrupts pending*, not "mode 0". This misreading survived several
independent analyses because the byte values alone cannot distinguish the two
registers. It is now mechanically checkable — `RB_SRAM_TRACE`'s `from=` field
names the source register — and that is the first thing to run on any unfamiliar
cell block.

### Family breakdown (rows sum to 29)

| # | Family | Rows | Cells | Verdict | Confidence |
|---|---|---:|---:|---|---|
| 1 | `lcd/lcd_frame_timings/*` | 21 | 1179 | ORACLE-CONFLICT (1148 mode-bit cells) + OPEN-TARGET (31 LYC-bit cells) | firm arithmetic; **provenance OPEN** |
| 2 | `serial/sc_change_freq_gbc#agb` | 1 | 1895 | LOGIC-IMPOSSIBLE (AGB dither) | **provisional** |
| 3 | `dma/dma_timing_lcd_on` | 2 | 90 | ORACLE-CONFLICT (vs `age/oam-read-cgbE`) | **provisional** |
| 4 | `dma/hdma_timing_fine` | 2 | 64 | ORACLE-CONFLICT (vs 13 gambatte rows) | firm |
| 5 | `lcd/mode3` | 2 | 22 | ORACLE-CONFLICT (vs mooneye sprite timing) — **closed** | firm |
| 6 | `timers/tac_set_disabled#agb` | 1 | 3 | OPEN-TARGET | firm |

21+1+2+2+2+1 = **29** rows, 1179+1895+90+64+22+3 = **3253** cells — both match
the `fams.py` roll-up above **[R]**.

Firmly adjudicated: families 1 (arithmetic), 4, 5, 6 — **26 of 29 rows**.
Provisional: families 2 and 3 — **3 rows**. Family 1 carries an additional open
*provenance* question that is not about the emulator at all; see §1b.

**Exactly 1 of the 29 rows is a plain modelling gap** — family 6, and within it
only the 2 `TIMA` cells. The other 28 are either forced by a conflict between two
credible real-silicon oracles (25 rows), or unfittable/unproven (3 rows). That is
a materially different situation from the previous revision, where 42 rows sat
under a single mischaracterized OPEN-TARGET.

### 1. `lcd/lcd_frame_timings/*` — 21 rows, 1179 cells — ORACLE-CONFLICT + OPEN-TARGET

8 rows under `ly_equals_lyc/`, 7 under `mode1/`, 6 under `mode2/` **[R:
`fams.py`]**. Down from 42 rows.

**These cells really are STAT reads — verified, not assumed [R].** Given Trap 3,
this is checked rather than presumed; every failing cell in a swept row is an
`FF41(STAT)` read:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_SRAM_TRACE=stat_timings_lyc_0_gbc_mode \
    tools/run-suites.sh gbc_hw_tests 2>&1 | grep SRAM_BLAME | head -3
SRAM_BLAME off=0x2257 want=0x84 got=0x87 pc=0x44B2 cc=3098985 src=A from=FF41(STAT) sym=stat_read_test_delay_gbc_0+0x4AF
SRAM_BLAME off=0x2290 want=0x80 got=0x83 pc=0x4524 cc=3099897 src=A from=FF41(STAT) sym=stat_read_test_delay_gbc_0+0x521
SRAM_BLAME off=0x22C9 want=0x80 got=0x83 pc=0x4596 cc=3100809 src=A from=FF41(STAT) sym=stat_read_test_delay_gbc_0+0x593
```

**The family contains two independent defects, and this is the key structural
finding — a fix for either one alone flips zero rows.** The XOR histogram
separates them cleanly **[R: `fams.py`]**:

| `want ^ got` | cells | meaning |
|---|---:|---|
| `0x03` / `0x01` / `0x02` | 746 / 208 / 189 | STAT **mode bits** — 1148 cells (97.4%) |
| `0x04` | 31 | STAT **LY=LYC coincidence bit**, alone |
| `0x06` / `0x05` | 4 / 1 | both bits at once |

So **1148 of 1179 cells touch the mode bits and 36 touch the LYC bit**. A row
goes green only when *both* are right, which is why mode-bit work has repeatedly
shown a 0-row delta. Representative LYC cells **[R: `RB_SRAM_VERBOSE`]**:

```
ly_equals_lyc/stat_timings_lyc_0_gbc_mode        off=0x2995 want=0x85 got=0x81
ly_equals_lyc/stat_timings_lyc_152_153_gbc_mode  off=0x295B want=0x85 got=0x81
```

#### 1a. The mode-bit half (1148 cells) — ORACLE-CONFLICT at one degree of freedom

`get_stat_mode3to0_at_cc` (`rustyboi-core/src/ppu/controller.rs:10487`) resolves
the mode-3→0 boundary as a pure function of `d = m0t − cc`, with **exactly one
degree of freedom** — the comparison offset. Two same-silicon oracles pin it to
different values on *plain* double-speed geometry (`scx=0`, no sprites), so no
single constant satisfies both **[V: session experiment, not reproducible at this
revision — the core is unmodified]**:

| oracle | needs | implies |
|---|---|---|
| `age` `spsw-mode0-cgbBCE` | `d=3` → mode 3 | offset ≤ 2 |
| gbc-hw `stat_timings` | `d=4` → mode 0 | offset ≥ 4 |

Setting offset 4 fixes **all** the mode-bit errors and takes `gambatte` 9 → 146
failures and `age` 56 → 52. A geometry-gated resolver was tried and also fails
(`age` 55/56), which **falsifies** the natural hypothesis that this is an
`scx`/sprite-penalty effect — it is not. Both counter-oracles are green at this
revision **[R]** (`mooneye 193/193`, `age 56/56`, `gambatte failed=9`), so the
conflict is live, not historical.

**The residual shape [V: session].** Hardware splits its mode-3 runs 50/50
between 22- and 21-probe; we produce 75/25 — always one probe too long. And the
**entire single-speed half of every capture is byte-perfect; every error is in
double-speed.** Consistent with that, the failing offsets in a swept row begin at
`0x2257` in a 62082-byte capture — everything before is exact **[R:
`RB_SRAM_VERBOSE`]**. The single/double-speed attribution itself comes from the
session's table decode and is **attributed, not re-derived here** (upstream
`.asm` is not in-tree).

#### 1b. The provenance question — **OPEN, not resolved**

The conflict above assumes both oracles describe the same silicon. **That
assumption is not established, and if it is wrong the conflict may dissolve
entirely.**

- Our `rev=cgbe` pin on AntonioND's captures is an **inference**, taken from
  SameBoy-built-from-source, whose gate is `model <= CGB_C`. That establishes
  only **"post-C"** — it does not distinguish CGB-D from CGB-E. AntonioND
  documents no revision at all.
- `age`'s corpus names its units `{cgbBCE, cgbE, cgbBC, cgb}` — with **no D
  anywhere**.

If AntonioND's unit is a **CGB-D**, the two oracles are simply describing
different steppings and both can be satisfied by a revision-gated resolver. This
is being investigated separately and **must not be recorded as adjudicated in
either direction**. Note the precedent: an earlier revision's `oam_echo_ram`
ORACLE-CONFLICT dissolved exactly this way, as a revision mismatch, once both
sides' `rev=` pins were compared.

#### 1c. The LYC half (31 cells) — OPEN-TARGET (firm)

A second, independent defect: the LY=LYC coincidence bit is clear where hardware
sets it. Small, uniform, and unentangled with §1a's conflict — this is genuine,
tractable accuracy work, and it is a prerequisite for *any* of these 21 rows
flipping.

#### 1d. Near-green rows

5 of the 7 `mode1/` rows fail on **one or two cells** each **[R:
`RB_SRAM_VERBOSE`]** — `mode1_disablestat_end_dmg_mode` (1),
`mode1_disablestat_gbc_mode` (1), `mode1_disablevbl_gbc_mode` (1),
`vbl_mode1_lcdoff_dmg_mode` (1), `vbl_mode1_lcdoff_gbc_mode` (2). These are the
cheapest rows in the section and are worth triaging on their own rather than as
part of the 21.

### 2. `serial/sc_change_freq_gbc#agb` — 1 row, 1895 cells — LOGIC-IMPOSSIBLE (**provisional**)

The CGB column passes. The AGB column remains.

**The claim is that this row is unfittable by design [V: session, PROVISIONAL —
not re-derived here].** The AGB serial dither is *provably not a function of the
divider*: identical inputs at sweep iterations **1096 and 1104** produce
different values *within the same capture*. If that holds, a single GBA-SP
capture cannot separate clock drift from a real dependence, and no deterministic
model can satisfy the table — which is LOGIC-IMPOSSIBLE (one oracle disagreeing
with itself) rather than a conflict between two oracles.

This is labelled **provisional** because the iteration-level decode was not
reproduced against this tree; verifying it needs the upstream `.asm` to map sweep
iterations onto capture offsets. Until then, treat "1895 cells" as measured **[R:
`fams.py`]** and the verdict as attributed.

This row grades against an in-tree emitted prefix
(`rustyboi-test-runner/suites/refs/gbc-hw-tests/serial/sc_change_freq_gbc.gbasp.sav`)
because the upstream capture is a raw 128K card dump; see the manifest header's
"Grading window" note for why that is a trimmed real capture and never an
emulator output.

### 3. `dma/dma_timing_lcd_on` — 2 rows, 45 cells each — ORACLE-CONFLICT (**provisional**)

**First, this family is independent of §1 — proven, and worth stating because it
was previously lumped in.** Sweeping the double-speed STAT offset leaves this
family at a constant diff count at *every* value **[V: session]**, and its diffs
are not mode bits at all: all 45 are **distinct** XOR masks of DMA data **[R:
`RB_SRAM_VERBOSE`]**, against §1's 6-value histogram and §5's single mask.

```
dma_timing_lcd_on   real_gbc.sav   n=45
    delta(want-got) histogram: {-252: 1, 247: 1, -206: 1, 190: 1, -149: 1, ...}
    distinct XOR masks       : 45
```

**Two independent edges, each ~5 cc late [V: session]:** the open edge sits
uniformly at `cc − m0t = −4`; the close edge uniformly at `lcat=447, lc=452`. A
uniform **+5** takes the CGB row to **0 diffs** — and breaks `age/oam-read-cgbE`,
whose reads sit in the *same phase class, at the same `d`, on the same revision*,
demanding the opposite answer. `age` is green at this revision **[R]**, so that
is a real trade, not a hypothetical.

**Provisional** because the edge characterization is a session result not
re-derived here, and because the paired-displacement signature (marker missing at
one offset, present 5–16 cells later) has a *non-constant* stride, which a single
displaced write index would not produce. More than one effect may be present.

### 4. `dma/hdma_timing_fine` — 2 rows, 32 cells each — ORACLE-CONFLICT (firm)

The cleanest signature in the section, and fully characterized **[R:
`RB_SRAM_VERBOSE`]** — every cell is `want = got + 2`, at every odd offset, one
delta bucket:

```
hdma_timing_fine   real_gbc.sav   n=32
    delta(want-got) histogram: {2: 32}
    distinct XOR masks       : 2
    offsets parity           : all odd
```

**We can close this at will, and the cost is 13 gambatte rows.** Setting
`fudge=0` makes both rows byte-exact (+2 rows) and breaks 13 currently-passing
gambatte rows **[V: session]**. No structural discriminator exists: the at-risk
gambatte rows take the *same code path*, with `kick=false` and LCD on. The real
difference is downstream **read distance**, which means the `+6` is a
**read-phase correction miscast as elapsed time**.

**The obvious fix is structurally impossible, and this is the load-bearing
finding [V: session].** The natural move — carry the correction in
`prefetch_stat_bias` — cannot work, because **8 of the 13 at-risk gambatte rows
never read STAT at all**; they read `FF55`. A STAT-keyed bias has no way to reach
them. Any real fix has to model the read-phase dependence directly.

Net: +2 gbc_hw_tests for −13 gambatte is not a defensible trade, so the rows
stand. Firm.

### 5. `lcd/mode3` — 2 rows, 11 cells each — ORACLE-CONFLICT, **closed** (firm)

The previous revision's truncation artifact is gone (`frames=3000`, Trap 1). What
remains is uniform and unambiguous **[R: `RB_SRAM_VERBOSE`]** — 11 cells, one
delta bucket, one XOR mask (`0x03`, the STAT mode bits): every cell is
`want=0xC4 got=0xC7`, i.e. we report **mode 3 where hardware reports mode 0**.

```
lcd/mode3   real_gbc.sav   n=11
    delta(want-got) histogram: {-3: 11}
    distinct XOR masks       : 1
```

**This is closed as a genuine oracle conflict, and our side is the
positively-validated one.** Our sprite cost `6N+3` is not a fitted constant: it
is independently confirmed by mooneye's `intr_2_mode0_timing_sprites`, which
**sweeps every residue at the same distance-2 geometry** — the exact
discriminating experiment — and passes on both DMG and CGB **[R]**:

```sh
$ grep -n 'intr_2_mode0_timing_sprites' rustyboi-test-runner/suites/mooneye.manifest
77:...intr_2_mode0_timing_sprites.gb|dmg|mooneye|...
78:...intr_2_mode0_timing_sprites.gb|cgb|mooneye|...
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh mooneye age
PASS  mooneye              passed=193/193 (floor: passed>=193)
PASS  age                  passed=56/56 (floor: passed>=56)
```

Both levers that satisfy the gbc-hw capture crater the counter-oracles — `age`
53/56 and mooneye 191/193 **[V: session]**. Trading a broad residue sweep plus 3
`age` rows for 2 rows against an undocumented-revision capture is not defensible.
**These 2 rows are expected to stay red**, and that is the correct outcome unless
the §1b provenance work reclassifies AntonioND's unit.

### 6. `timers/tac_set_disabled#agb` — 1 row, 3 cells — OPEN-TARGET (firm)

The CGB column passes. The AGB column is now graded against `real_gba_sp.sav`
(manifest line 403), which **sidesteps the 1145-cell inter-unit conflict** the
previous revision documented at length: the two physical AGB units disagree at
1145 of 31748 bytes, and grading against the SP capture avoids all of them.

**Correcting the framing this row is usually given:** the 3 surviving cells are
*not* excused by that disagreement. All 3 sit at offsets where the two units
**agree** **[R]**:

```sh
$ python3 - <<'PY'
D="gb-test-roms/gbc-hw-tests/timers/tac_set_disabled/"
gba=open(D+'real_gba.sav','rb').read(); sp=open(D+'real_gba_sp.sav','rb').read()
n=min(len(gba),len(sp)); dis={i for i in range(n) if gba[i]!=sp[i]}
print("the two AGB units disagree at:", len(dis), "of", n)
ours=[0x638E,0x6796,0x6B94]
print("our 3 cells inside the disagreement:", [hex(x) for x in ours if x in dis])
print("our 3 cells where the units AGREE  :", [hex(x) for x in ours if x not in dis])
print("disagreement cells in 0x6000-0x6FFF:", len([i for i in dis if 0x6000<=i<0x7000]))
PY
the two AGB units disagree at: 1145 of 31748
our 3 cells inside the disagreement: []
our 3 cells where the units AGREE  : ['0x638e', '0x6796', '0x6b94']
disagreement cells in 0x6000-0x6FFF: 317
```

So the *surrounding block* is contaminated (317 disagreeing cells in
`0x6000-0x6FFF`, which is why a fit derived from neighbouring cells would be
unreliable) but the three graded cells themselves are agreed by both units.
**These are genuinely ours.**

`SRAM_BLAME` splits them into two different classes — and this only became
visible with the trace **[R: the `RB_SRAM_TRACE` output in Method]**:

- `0x638E`, `0x6796` — `from=FF05(TIMA)`, `sym=Main.loop+…`: real TIMA reads,
  each exactly **one count short**. The same `+1`-glitch-one-step-out signature
  that the retired `tac_set_enabled` AGB column showed. **OPEN-TARGET, and the
  most tractable genuine gap left in the section.**
- `0x6B94` — `from=-`, `sym=memset+0x1`, `cc=1787896` against the others'
  `cc≈4.2M`: this byte's **last writer is the ROM's `memset`**, not a result
  store. We never wrote a result there at all. That is a different defect (the
  ROM's table not reaching that offset in our run) and should not be lumped in
  with the TIMA cells; it is unproven whether more frames would close it.

---

## Floor arithmetic

- **gbmicrotest:** 509/512 is the maximum for any register-level emulator without inventing oracles (`temp` is now excluded as un-gradeable — it writes no verdict, so it was never a real fail). +2 (`500-scx-timing`, `minimal`) become gradeable the day a hardware capture of the absolute byte exists; `halt_op_dupe_delay` requires characterizing analog die physics.
- **gambatte:** 7 (residue tail) + 1 (fexx) + 1 (C113) = **9 is the permanent minimum for any deterministic emulator, including a perfect gate-level one** — every failing byte is pinned oppositely by a *currently-passing* capture of the same physical quantity, and the exhaustive subset search confirms the current choices are globally optimal.
- **gbc_hw_tests:** 313/342 is a ratcheted progress floor. It is no longer "nothing like a proven ceiling" — the day's work moved most of what remains *out* of the fixable class. Sorting the 29 by what actually blocks them:
  - **2 cells, in 1 row, are a plain modelling gap.** `tac_set_disabled#agb`'s two `FF05(TIMA)` cells are each exactly one count short (§6). This is the only unambiguous, uncontested accuracy work left in the suite, and closing it flips 1 row (the row's third cell is a `memset` fill, a different defect).
  - **31 cells across the 21 `lcd_frame_timings` rows are a plain modelling gap** — the LY=LYC coincidence bit (§1c). Closing it flips **zero rows on its own**, because each of those rows also carries mode-bit cells; it is a *prerequisite*, not a win.
  - **25 of the 29 rows are ORACLE-CONFLICT** — `lcd_frame_timings` (21, §1a), `hdma_timing_fine` (2, §4), `lcd/mode3` (2, §5). In each case we can produce the demanded value at will, and in each case doing so breaks a currently-passing real-silicon suite by more than it gains: offset 4 costs gambatte 137 rows and age 4; `fudge=0` costs 13 gambatte rows; the mode3 levers cost age 3 and mooneye 2. **No emulator change resolves these. They need provenance, not code** — specifically §1b's CGB-D-vs-E question, which is the single highest-leverage open item in this document because it gates 21 rows.
  - **The remaining 3 rows are unfittable or unproven** — `sc_change_freq_gbc#agb` (1, LOGIC-IMPOSSIBLE if the AGB-dither finding holds, §2) and `dma_timing_lcd_on` (2, an oracle conflict against `age/oam-read-cgbE` whose edge characterization is not re-derived here, §3).
- 4 + 9 + 29 = 42: every gbmicrotest and gambatte failure is accounted for byte by byte, and every gbc_hw_tests failure carries a family, a verdict class and a confidence label — with **26 of its 29 rows firmly adjudicated** and 3 (`dma_timing_lcd_on` ×2, `sc_change_freq_gbc#agb` ×1) marked provisional.

**The shape of the remaining work has inverted, and that is the headline.** The
previous revision's 59 failures were dominated by one large, mischaracterized
OPEN-TARGET. After the fixes, 25 of 29 rows are cases where **we already know how
to produce the expected bytes and have measured that doing so is a net loss**.
The bottleneck is no longer modelling; it is silicon provenance — whose unit,
which revision, which capture. That is bench work, not emulator work.

**Corrections this revision makes to its predecessor**, recorded because each was
stated as settled and each was wrong:

1. §1's global one-M-cycle-displacement claim was an artifact of conditioning the statistic on mismatching cells; the unconditioned test refutes it (5297 → 52573 under the shift). Generalized as **Trap 2**.
2. The predecessor's §3 claimed `tac_set_disabled#agb`'s residue sat *inside* the two AGB units' disagreement. Re-measured here against the now-graded `real_gba_sp.sav`: **all 3 surviving cells sit where the units agree** (§6). The 1145-cell conflict is real but is now sidestepped by the reference change, and it does not excuse the residue.
3. `lcd/mode3` was twice described as a value error of 647 cells. It is 11 cells, and it is an oracle conflict our side wins on independent evidence (§5). Generalized as **Trap 1**.

All three survived a full write-up because a plausible mechanism was fitted to a
statistic that could not discriminate. The three measurement traps documented at
the top of the gbc_hw_tests section are the generalization, and `RB_SRAM_TRACE`
(§ Method) is the tool that makes the third one mechanically checkable rather
than a matter of judgement.

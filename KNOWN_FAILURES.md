# Known Failures — every failing ROM, with proof

16 test cases fail across the 28 suites: 3 in gbmicrotest, 9 in gambatte, 4 in gbc_hw_tests. Every one of the 16 is adjudicated below. Entries make **no assumptions**: every claim is tagged with its provenance, reproducible claims include the command that re-verifies them against this tree, and claims that outrun their evidence are labelled **PROVISIONAL** rather than dressed up.

Counts measured at `9297ac9b` after an explicit `cargo build --release -p rustyboi-test-runner` (this tree has a documented stale-binary trap that has produced false PASS, false FAIL *and* a committed false README count — never quote a number produced without a rebuild) **[R]**:

```sh
$ cargo build --release -p rustyboi-test-runner   # MANDATORY: never skip
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh gbc_hw_tests
Ran 342 total tests.
4 total failures.
Ran 301 CGB tests.
4 CGB failures.
Ran 41 DMG tests.
0 DMG failures.
PASS  gbc_hw_tests         passed=338/342 (floor: passed>=338)
```

**This is a large change from the 313/342 the previous revision of this file
documented, and the direction is worth stating up front.** In the interval a run
of revision-gated PPU/timer fixes closed **25 of the 29 gbc_hw_tests rows** that
revision recorded — including *every* row it had filed as `ORACLE-CONFLICT` under
`lcd_frame_timings` (21), `dma_timing_lcd_on` (2) and `lcd/mode3` (2). Those were
**not** resolved by establishing silicon provenance, the way that revision
predicted they would have to be; they were fixed in the emulator, gated on the
CGB-D/E double-speed phase, without regressing any counter-oracle. Their entries
are **deleted**, not left describing green tests. This is the same lesson the
`oam_echo_ram` conflict taught earlier — an "irreducible" oracle conflict
dissolving under a closer look — at 25× the scale; read every remaining
`ORACLE-CONFLICT` verdict below with that precedent in mind.

Note **342**, not the 343 earlier revisions of this file quote: `tac_set_enabled`'s CGB
column was removed from the manifest after adjudication — its capture is
structurally defective (DMG-shaped 15364 bytes where every other CGB capture of
that ROM family is 31748), so the row count itself dropped by one. **The whole
DMG column is green.**

The other two failure-carrying suites **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh gbmicrotest gambatte
PASS  gbmicrotest          passed=509/512 (floor: passed>=509)
PASS  gambatte             passed=5248/5257 failed=9 (floor: failed<=9)
```

gambatte is unchanged at 9 failures. gbmicrotest carries 3 failures over 512
rows: `temp.gb` was excluded from the manifest (it writes no verdict of any kind
— see the gbmicrotest section), which is why the total is 512 not 513, and why
the three failure-carrying suites now sum to 3 + 9 + 4 rather than the
temp-inclusive 4 an earlier header line carried.

Two suites are cited repeatedly below as *counter-oracles* — the ones that a
"fix" for a contested gbc_hw_tests row would break. Both are green **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh mooneye age
PASS  mooneye              passed=193/193 (floor: passed>=193)
PASS  age                  passed=56/56 (floor: passed>=56)
```

So the three suites that carry failures stand at 3 + 9 + 4 = **16**. The full
28-suite battery was **not** re-run for this doc-only pass; the corpus case total
is **6841** (the previous revision's 6842 less the now-excluded gbmicrotest
`temp.gb`), of which 16 fail. That figure is *derived*, not re-measured here — it
is arithmetic on the three measured suites plus an unchanged remainder, and it
must be re-measured before quoting as a headline.

> **The README suite table is regenerated separately and is not the source here.** This document's arithmetic comes only from the runs pasted above. Fixing or confirming the README is a separate change and is out of scope for this file.

**A gitignored-asset warning, because it silently falsified results six times in one day** — one of which reached `main` as a regression. `gb-test-roms/`, `bios/` and `test-roms/build/` are gitignored, so a fresh worktree does not have them and the suite will happily "pass" while skipping nearly everything. Before quoting any number, confirm the run reports **`Ran 342 total tests`** for gbc_hw_tests. A row count below that means missing ROMs, not progress. `test-roms/build/` must be a **real copy**; symlinks are fine for the other two.

Provenance tags:

- **[R]** Reproducible here — run the command shown against this checkout.
- **[V]** Verified against third-party references built from source (SameBoy, libgambatte, GateBoy sources) or by instrumented traces/experiments; the method is described where cited.
- **[D]** Documented upstream — Pan Docs, gekkio's gb-ctr, or the test author's own files.

Verdict classes:

- **LOGIC-IMPOSSIBLE** — real-hardware captures pin the *same physical quantity* to different values in different capture sessions. A deterministic emulator must pick one value, so for each such family the failure count is forced by arithmetic, not by a modeling gap.
- **ORACLE-CONFLICT** — two *different* real-hardware oracles, each independently credible, demand incompatible answers for the same behaviour. Distinct from LOGIC-IMPOSSIBLE, which is one oracle disagreeing with *itself* across sessions: here each oracle is internally consistent, so the failure is not forced by arithmetic — it is forced by a choice about *which oracle is authoritative*, and that choice cannot be made from the captures alone. **This class has proven the most reversible in the document, and the reader should treat every instance below as provisional-until-disproven.** The previous revision filed 25 of its 29 gbc_hw_tests rows here and called the arithmetic "firm"; a later run of revision-gated PPU fixes then closed all 25 — the conflicts were not between two same-silicon oracles at all, but artifacts of applying one offset globally instead of gating it on the CGB-D/E double-speed phase. Once gated, both "conflicting" oracles pass at once. That is the second time an ORACLE-CONFLICT here has dissolved: the earlier `oam_echo_ram` conflict resolved as a **revision mismatch** once both sides' `rev=` pins were compared. So while resolving a *genuine* one requires provenance work (which silicon revision, which physical unit, which capture is intact) rather than emulator work, the standing lesson is that most rows filed here were mis-filed. Only the 2 `hdma_timing_fine` rows still carry this class, and even they name the specific missing measurement that would settle them (§1).
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

## gbc_hw_tests — 4 failures (338/342)

Adopted from AntonioND/gbc-hw-tests (real-device SRAM captures; see SUITES.md
for grading provenance and the revision caveat). Each ROM is graded per
*column*: CGB (`rev=cgbe`, vs `real_gbc.sav`), AGB (`rev=agb`, vs `real_gba.sav`
where the dir ships one, else `real_gba_sp.sav`), and for DMG-flagged ROMs a DMG
column. A "row" below is one (ROM, column) case, which is what the 4 counts. The
runner prints these rows as `CGB` because they all run on CGB-mode silicon; the
`#agb` rows differ only in carrying `rev=agb` and grading against an AGB capture.

**This section was reconciled to the 338/342 live state.** The previous revision
documented 29 failures at 313/342 and filed 25 of them as `ORACLE-CONFLICT`; a
run of revision-gated PPU/timer fixes then closed all 25. Every family that is
now green has had its entry **deleted**, not left describing a passing test:

### What closed since 313/342

| closed family | rows | how it closed |
|---|---:|---|
| `lcd/lcd_frame_timings/*` | 21 | fixed — the CGB-D/E double-speed STAT mode-bit and LY=LYC coincidence work (commits `81419a5e`, `102f3733`, `12a88ae3`, `d0230e38`, `140864f4`, `98bcb076`, `af94f21b`, `25c09e6d`), gated on the post-C double-speed phase so mooneye/age/gambatte were untouched |
| `dma/dma_timing_lcd_on` | 2 | fixed — the non-serviced-HALT-woken OAM read resolved on the CPU M-cycle grid (`58398084`) |
| `lcd/mode3` | 2 | fixed — the DS mode-3 STAT-read boundary put 3 dots below m0 on CGB-D/E + AGB (`102f3733`, `140864f4`) |

**The headline correction this makes to its predecessor:** those 25 rows were
filed as unfixable oracle conflicts — "we can produce the demanded value at will,
and doing so craters an independent real-silicon suite" — and the prediction was
that only silicon-provenance work could move them. It was wrong. The fixes were
emulator-side, gated on the CGB-D/E double-speed phase rather than applied as a
global offset, and the counter-oracles stayed green throughout
(`mooneye 193/193`, `age 56/56`, `gambatte failed=9` — all still green **[R]**,
verified above). The conflict was never between two same-silicon oracles; it was
between one blunt global constant and the phase-gated behaviour the hardware
actually shows. Read the two `ORACLE-CONFLICT` rows that remain (§1) with that in
mind.

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
SRAM_BLAME off=0x6B94 want=0x01 got=0x00 pc=0x0085 cc=1787896 src=A from=- sym=memset+0x1
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
    return '/'.join(p.split('/')[:2])
agg = collections.defaultdict(lambda: [0, 0])
for rom, ref, nd, cells in rows:
    a = agg[fam(rom)]; a[0] += 1; a[1] += int(nd)
print("%-40s %5s %8s" % ("family", "rows", "cells"))
for k in sorted(agg):
    print("%-40s %5d %8d" % (k, agg[k][0], agg[k][1]))
print("%-40s %5d %8d" % ("TOTAL", sum(v[0] for v in agg.values()), sum(v[1] for v in agg.values())))
PY
$ python3 /tmp/fams.py /tmp/verbose.txt
family                                    rows    cells
dma/hdma_timing_fine                         2       64
serial/sc_change_freq_gbc                    1     1895
timers/tac_set_disabled                      1        3
TOTAL                                        4     1962
```

(The previous revision's `fams.py` carried an extra `lcd_frame_timings`
`want^got` XOR histogram used to split that family's STAT-mode-bit and LY=LYC
defects; with the family green there is nothing left for it to bin, so it has
been dropped from the helper.)

### Three measurement traps that invalidate naive triage

All three produced *wrong classifications in earlier revisions of this very
document*. Read this before quoting any cell count.

**Trap 1 — a mismatch count at the default budget is not a proximity metric.**
The suite runs a flat 800-frame budget. A ROM that has not finished writing its
result table by frame 800 shows its unwritten tail as mismatches, and those
counts swamp the real ones. `lcd/mode3` was the example that exposed this: it
read **647** diffs at 800 frames and **11** at 4000, because 636 of the 647 were
bytes the ROM had simply not written yet. A previous revision read those 647
cells as "a value error" and built a whole verdict on truncation noise. The
manifest now carries `frames=3000` on both `lcd/mode3` rows **[R]**, which cut it
to its true 11 cells per row; those 11 were subsequently fixed and the family is
**green** — but the trap is general: **if a family's diff count is large, check
for unwritten `0xFF` before theorising.**

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

### Family breakdown (rows sum to 4)

| # | Family | Rows | Cells | Verdict | Confidence |
|---|---|---:|---:|---|---|
| 1 | `dma/hdma_timing_fine` (CGB + AGB) | 2 | 64 | ORACLE-CONFLICT (vs 13 gambatte rows); the clean fix is BLOCKED-ON-ORACLE | firm arithmetic |
| 2 | `serial/sc_change_freq_gbc#agb` | 1 | 1895 | BLOCKED-ON-ORACLE (AGB dither, undecidable from one capture) | **provisional** |
| 3 | `timers/tac_set_disabled#agb` | 1 | 3 | OPEN-TARGET (2 AGB `TIMA` cells) + un-reached fill (1 cell) — **potentially fixable, flagged** | **provisional** |

2+1+1 = **4** rows, 64+1895+3 = **1962** cells — both match the `fams.py` roll-up
above **[R]**.

**None of the 4 is a settled, uncontested modelling gap the way this section once
carried dozens.** The one that comes closest is §3: 2 of its 3 cells are a genuine
`TIMA`-increment gap on the AGB, corroborated by *both* physical AGB units, and
that is the single item flagged below as **potentially fixable rather than
floored** — with the caveat that the surrounding AGB-TAC-write behaviour is a
device-dependent race the manifest header explicitly warns against fitting. §1
(2 rows) is a live oracle conflict whose clean resolution needs a measurement no
capture in-tree provides; §2 (1 row) is undecidable from the single GBA-SP
capture that exists. Every cell count here is the live `fams.py` roll-up; every
verdict is re-derived below, not carried over.

### 1. `dma/hdma_timing_fine` — 2 rows, 32 cells each — ORACLE-CONFLICT (firm); clean fix BLOCKED-ON-ORACLE

Both columns of the one ROM fail identically: the CGB row (vs `real_gbc.sav`) and
the AGB row (`#agb`, vs `real_gba_sp.sav`). Failing in both columns means this is
not AGB-specific — it is one unmodelled behaviour, imaged twice. It is the
cleanest signature in the section: every cell is `want = got + 2`, at every odd
offset, one delta bucket **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_SRAM_VERBOSE=1 \
    tools/run-suites.sh gbc_hw_tests 2>&1 | grep 'hdma_timing_fine.*CGB real_gbc' \
  | python3 -c 'import sys,re; c=re.findall(r"0x([0-9A-F]+):want=0x([0-9A-F]+),got=0x([0-9A-F]+)",sys.stdin.read()); print("n=%d deltas(want-got)=%s all_odd=%s" % (len(c),{int(w,16)-int(g,16) for _,w,g in c},all(int(o,16)%2 for o,_,_ in c)))'
n=32 deltas(want-got)={2} all_odd=True
```

`SRAM_BLAME` attributes all 64 cells (both rows) to `sym=lcd_func+0x8`, `from=-`
— a computed HDMA byte-count result, not an IO register read.

**We can close this at will, and the cost is 13 gambatte rows.** Zeroing the
LCD-on HDMA fudge makes both rows byte-exact (+2 rows) and breaks 13
currently-passing gambatte rows **[V: session]**. No structural discriminator
exists: the at-risk gambatte rows take the *same code path*, with `kick=false`
and LCD on. Two independent agent passes split the 13 consumers into 6 that are
read-phase-sensitive and 7 that behave like elapsed time, with **no block-local
predicate** that separates them.

**The correction is provably a read-phase effect, not elapsed time — this is the
load-bearing finding, and it is now re-derivable in-tree [R].** The LCD-on branch
of the HDMA block-cost path returns `base + 6` (the `else { 6 }` fudge), and
`sm83.rs` charges that value to the CPU as a stall **raw, with no M-cycle
rounding** (`return dma_stall;`):

```sh
$ sed -n '3702,3704p;3717,3718p' rustyboi-core/src/memory/mmio.rs
        } else {
            6
        };
        let base = if self.is_double_speed_mode() { 68 } else { 36 };
        base + prefetch_fudge
$ sed -n '144,146p' rustyboi-core/src/cpu/sm83.rs
        let dma_stall = mmio.take_dma_stall();
        if dma_stall > 0 {
            return dma_stall;
```

So the single-speed LCD-on block costs `36 + 6 = 42` cc = **10.5 M-cycles**. A
CPU stall that is genuine elapsed transfer time must be a **whole** number of
M-cycles (the bases 36 = 9 M-cycles and 68 = 17 M-cycles both are). 10.5 cannot
be elapsed time; the `+6` is a downstream read-phase correction wearing a stall's
clothing. `hdma_timing_fine` measures the block cost *immediately* and so wants
the M-cycle-aligned 36; the 13 gambatte consumers read their value further
downstream, and at least 7 of them need the `+6`.

**The obvious fix is structurally impossible [V: session].** The natural move —
carry the correction in `prefetch_stat_bias` — cannot work, because **8 of the 13
at-risk gambatte rows never read STAT at all**; they read `FF55`. A STAT-keyed
bias has no way to reach them. Any real fix has to model the read-phase distance
directly.

**Verdict: ORACLE-CONFLICT (firm on the arithmetic); the clean resolution is
BLOCKED-ON-ORACLE.** +2 gbc_hw_tests for −13 gambatte is not a defensible trade,
so the rows stand. What would unblock a fix that satisfies *both* oracles is a
single CGB ROM measuring one HDMA block's cost **both immediately and ≥1 frame
later on the same silicon** — the experiment that pins the read-phase distance and
splits the 13 consumers. No such capture exists in-tree, which is why the clean
fix, not just the trade, is blocked.

### 2. `serial/sc_change_freq_gbc#agb` — 1 row, 1895 cells — BLOCKED-ON-ORACLE (**provisional**)

The CGB column passes; only the AGB column (`#agb`, graded against the emitted
GBA-SP prefix) fails, at 1895 cells **[R: `fams.py`]**. `SRAM_BLAME` places every
one of them at `pc=0x0058`, `from=-` — a tight result-store loop, not an IO read.

**The claim is that this row cannot be fitted by *any* deterministic model, and
the honest label for that is BLOCKED-ON-ORACLE, not LOGIC-IMPOSSIBLE [V: session,
PROVISIONAL — not re-derived here].** The AGB serial dither is reported to be
*not* a function of the divider: identical divider inputs at sweep iterations
**1096 and 1104** produce different values *within the same capture*. If that
holds, the single GBA-SP capture that exists cannot separate two explanations —
analog clock drift (unfittable) versus a real dependence on some higher-order bit
the decode is not tracking (fittable, if identified). One capture cannot choose
between them, so the row is **undecidable from the evidence in hand** — which is
BLOCKED-ON-ORACLE. The previous revision filed it as LOGIC-IMPOSSIBLE (one oracle
contradicting *itself*); that is the stronger claim and it is **not** established,
because a hidden-bit dependence is not ruled out. Downgraded here on purpose.

This is **provisional** on top of that: the iteration-level decode (which capture
offset corresponds to iterations 1096 / 1104) was not reproduced against this
tree — it needs the upstream `.asm`, which `sync_gbchwtests_roms` does not copy
in. Treat "1895 cells" as measured **[R]** and the 1096/1104 finding as
attributed.

**Unblock:** a second independent GBA-SP capture. If iterations 1096 and 1104
read the same values they did before, the dither is a fixed function of
*something* and may be fittable; if they read differently, it is drift and the
row is a true floor.

This row grades against an in-tree emitted prefix
(`rustyboi-test-runner/suites/refs/gbc-hw-tests/serial/sc_change_freq_gbc.gbasp.sav`)
because the upstream capture is a raw 128K card dump; see the manifest header's
"Grading window" note for why that is a trimmed real capture and never an emulator
output.

### 3. `timers/tac_set_disabled#agb` — 1 row, 3 cells — OPEN-TARGET (2 cells) + un-reached fill (1 cell) — **potentially fixable, flagged**

The CGB column passes. The AGB column (`#agb`) grades against `real_gba_sp.sav`
(manifest line 414), which sidesteps a 1145-cell inter-unit conflict: the two
physical AGB units (plain GBA vs GBA-SP) disagree at 1145 of 31748 bytes, and the
SP capture is the complete one. **All 3 surviving cells sit where the two AGB
units *agree* [R]** — a behaviour both units show, not a unit-selection artifact:

```sh
$ python3 - <<'PY'
D="gb-test-roms/gbc-hw-tests/timers/tac_set_disabled/"
gba=open(D+'real_gba.sav','rb').read(); sp=open(D+'real_gba_sp.sav','rb').read()
n=min(len(gba),len(sp)); dis={i for i in range(n) if gba[i]!=sp[i]}
print("the two AGB units disagree at:", len(dis), "of", n)
ours=[0x638E,0x6796,0x6B94]
print("our 3 cells inside the disagreement:", [hex(x) for x in ours if x in dis])
print("our 3 cells where both AGB units AGREE:", [(hex(x), hex(gba[x])) for x in ours if x not in dis])
PY
the two AGB units disagree at: 1145 of 31748
our 3 cells inside the disagreement: []
our 3 cells where both AGB units AGREE: [('0x638e', '0xfd'), ('0x6796', '0xff'), ('0x6b94', '0x1')]
```

Both AGB units read N+1 at these three; we, and the CGB unit, read N.
`SRAM_BLAME` splits the three into two different classes **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_SRAM_VERBOSE=1 RB_SRAM_TRACE=tac_set_disable \
    tools/run-suites.sh gbc_hw_tests 2>&1 | grep SRAM_BLAME
SRAM_BLAME off=0x638E want=0xFD got=0xFC pc=0x0342 cc=4193676 src=A from=FF05(TIMA) sym=Main.loop+0xEE
SRAM_BLAME off=0x6796 want=0xFF got=0xFE pc=0x03D4 cc=4288716 src=A from=FF05(TIMA) sym=Main.loop+0x180
SRAM_BLAME off=0x6B94 want=0x01 got=0x00 pc=0x0085 cc=1787896 src=A from=- sym=memset+0x1
```

- `0x638E`, `0x6796` — `from=FF05(TIMA)`, `sym=Main.loop+…`: real TIMA reads, each
  exactly **one count short**. Both AGB units agree they should be N+1; we produce
  N, matching the CGB unit. This is a **genuine unmodelled AGB TIMA increment** —
  the same `+1`-glitch-one-step-out signature the retired `tac_set_enabled` AGB
  column showed. **OPEN-TARGET, and the single item in this section flagged as
  potentially fixable rather than floored.**
- `0x6B94` — `from=-`, `sym=memset+0x1`, `cc=1787896` against the others'
  `cc≈4.2M`: this byte's **last writer in our run is the ROM's `memset`**, not a
  result store. We never wrote a result there at all; the capture holds 0x01 and
  we hold the memset 0x00. A **different defect** (the ROM's table not reaching
  that offset in our run), unproven whether more frames would close it. It should
  not be lumped in with the two TIMA cells.

**Why this is flagged "potentially fixable" and not "fix it now."** The manifest
header (its `DO NOT model … tac_set_when_inc` note) documents that the AGB
TAC-write increment is, in general, a **device-dependent race**: the two AGB
units disagree by 1145 bytes on this very ROM "in exactly that ±1 TIMA shape", and
TCAGBD 5.5 calls it a race that "cannot be predicted for every device". An
AGB-only TAC quirk was added once on no oracle and removed when a capture
contradicted it. What makes *these two cells* different is that both units agree
on them, so a fix pinned to them is not obviously keyed to one unit — but a
deterministic `+1` rule must not silently re-break the sibling `tac_set_when_inc`
rows or the retired columns. One earlier framing went further and called the row
**LOGIC-IMPOSSIBLE**, arguing the glitch fires at TAC alias `b=12` but not `b=4`
(which differ only in bit 3, nominally a don't-care) — a same-run
self-contradiction *if* bit 3 is truly unused. That argument is **not re-derived
here**, and its load-bearing premise (bit 3 unused on the AGB timer) is exactly
what an AGB extra-increment rule might disprove; it is recorded as an unverified
alternative, not adopted. **Net: 2 cells are an open, tractable AGB accuracy item
— land a fix only with the sibling AGB timer rows and both retired columns
re-checked.**

---

## Floor arithmetic

- **gbmicrotest:** 509/512 is the maximum for any register-level emulator without inventing oracles (`temp` is now excluded as un-gradeable — it writes no verdict, so it was never a real fail). +2 (`500-scx-timing`, `minimal`) become gradeable the day a hardware capture of the absolute byte exists; `halt_op_dupe_delay` requires characterizing analog die physics.
- **gambatte:** 7 (residue tail) + 1 (fexx) + 1 (C113) = **9 is the permanent minimum for any deterministic emulator, including a perfect gate-level one** — every failing byte is pinned oppositely by a *currently-passing* capture of the same physical quantity, and the exhaustive subset search confirms the current choices are globally optimal.
- **gbc_hw_tests:** 338/342 is a ratcheted progress floor, **not** a proven ceiling — and the last time this file called most of the remainder "no emulator change resolves these", 25 of those rows were then resolved by emulator changes. Read this list as "what currently blocks each row", not "what can never move":
  - **2 cells, in 1 row, are the closest thing to a plain modelling gap left.** `tac_set_disabled#agb`'s two `FF05(TIMA)` cells are each exactly one count short (§3), both physical AGB units agree on the value we miss, and this is the one row **flagged as potentially fixable rather than floored** — with the standing caveat that the broader AGB-TAC-write increment is a device-dependent race the manifest warns against fitting. The row's third cell is a `memset` fill (a different, un-reached-table defect).
  - **2 rows are ORACLE-CONFLICT** — `hdma_timing_fine` (§1). We can produce the demanded value at will (`fudge=0`), but doing so breaks 13 currently-passing gambatte rows, and the `+6` is provably a read-phase correction (`42 cc = 10.5 M-cycles` cannot be an elapsed stall), so it cannot simply move to a STAT-keyed bias — 8 of the 13 consumers never read STAT. The clean fix is BLOCKED-ON-ORACLE: it needs a CGB capture of one block's cost measured both immediately and ≥1 frame later, which does not exist in-tree.
  - **1 row is unfittable-from-one-capture** — `sc_change_freq_gbc#agb` (§2), BLOCKED-ON-ORACLE: the single GBA-SP capture cannot separate analog dither from a hidden-bit dependence. (Downgraded from the predecessor's LOGIC-IMPOSSIBLE, which over-claimed.)
- 3 + 9 + 4 = 16: every gbmicrotest and gambatte failure is accounted for byte by byte, and every one of the 4 gbc_hw_tests failures carries a family, a verdict class and a confidence label — 1 row firm (`hdma_timing_fine` arithmetic) and 3 rows (`sc_change_freq_gbc#agb`, `tac_set_disabled#agb`) provisional, with the tac row explicitly flagged as the one that may yet be closable.

**The shape of the remaining work inverted twice, and the second inversion is the
correction this reconciliation records.** The revision before last had 59
failures dominated by one mischaracterized OPEN-TARGET. The last revision cut
that to 29 and re-filed 25 of them as ORACLE-CONFLICTs that "need provenance, not
code" — the single highest-leverage open item, it said, was a CGB-D-vs-E
provenance question gating 21 rows. **That framing was wrong.** All 25 closed with
emulator changes, gated on the CGB-D/E double-speed phase rather than applied
globally, and no counter-oracle moved. The provenance question did not have to be
settled at the bench; the behaviour just had to be phase-gated instead of pinned
to one constant. What is left is genuinely small — 4 rows — but the lesson is the
opposite of the last revision's headline: an `ORACLE-CONFLICT` verdict here has a
poor track record of surviving contact with a phase-aware fix, so the two that
remain (§1) are stated with the specific missing measurement that would settle
them, not as settled floors.

**Corrections this revision makes to its predecessor**, recorded because each was
stated as settled and each was wrong:

1. The predecessor filed 25 of 29 rows as ORACLE-CONFLICT and predicted only silicon-provenance work could move them (`lcd_frame_timings` 21, `dma_timing_lcd_on` 2, `lcd/mode3` 2). All 25 were closed by revision-gated PPU/timer fixes with every counter-oracle still green. The conflict was between a global constant and a phase-gated behaviour, not between two same-silicon oracles.
2. The predecessor's §1b named a CGB-D-vs-E provenance question as "the single highest-leverage open item … because it gates 21 rows". Those 21 rows are now green without that question being answered; it was not load-bearing.
3. `sc_change_freq_gbc#agb` was filed LOGIC-IMPOSSIBLE (one oracle contradicting itself). With only one capture, a hidden-bit dependence cannot be ruled out, so the honest class is BLOCKED-ON-ORACLE (§2). Downgraded.

The three measurement traps documented at the top of the gbc_hw_tests section,
and `RB_SRAM_TRACE` (§ Method), remain the tools that keep a plausible mechanism
from being fitted to a statistic that cannot discriminate — the failure mode that
produced every one of the corrections above.

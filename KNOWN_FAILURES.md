# Known Failures — every failing ROM, with proof

88 of 6843 test cases fail across the 28 suites: 4 in gbmicrotest, 9 in gambatte, 75 in gbc_hw_tests. Every one of the 88 is adjudicated below. Entries make **no assumptions**: every claim is tagged with its provenance, reproducible claims include the command that re-verifies them against this tree, and claims that outrun their evidence are labelled **PROVISIONAL** rather than dressed up.

Counts measured at `360f1b4b` after an explicit `cargo build --release -p rustyboi-test-runner` (this tree has a documented stale-binary trap that has produced false PASS, false FAIL *and* a committed false README count — never quote a number produced without a rebuild) **[R]**:

```sh
$ cargo build --release -p rustyboi-test-runner   # MANDATORY: never skip
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh gbc_hw_tests
Ran 343 total tests.
75 total failures.
Ran 302 CGB tests.
74 CGB failures.
Ran 41 DMG tests.
1 DMG failures.
PASS  gbc_hw_tests         passed=268/343 (floor: passed>=268)
```

The corpus total (6755/6843) is the README suite table's `**Total**` row; 6843 − 6755 = 88 = 4 + 9 + 75, and the three suite splits are each `Total − Passing` in that table.

Provenance tags:

- **[R]** Reproducible here — run the command shown against this checkout.
- **[V]** Verified against third-party references built from source (SameBoy, libgambatte, GateBoy sources) or by instrumented traces/experiments; the method is described where cited.
- **[D]** Documented upstream — Pan Docs, gekkio's gb-ctr, or the test author's own files.

Verdict classes:

- **LOGIC-IMPOSSIBLE** — real-hardware captures pin the *same physical quantity* to different values in different capture sessions. A deterministic emulator must pick one value, so for each such family the failure count is forced by arithmetic, not by a modeling gap.
- **BLOCKED-ON-ORACLE** — a hardware-correct answer exists in principle, but no captured/documented value exists anywhere; grading the emulator's own output would assert nothing. Unblockable with real hardware.
- **UN-GRADEABLE** — the ROM writes no stable verdict anywhere.
- **ANALOG** — the behavior has no register-level correlate; reproducing the value without a physical derivation would be a single-observable fit.
- **OPEN-TARGET** — a genuine modeling gap with a real-silicon (or author-endorsed deterministic) oracle. Unlike the classes above these are *fixable*; they are open accuracy work items, not excused floors.

---

## gbmicrotest — 4 failures (509/513)

The suite's protocol is `FF82==0x01` pass with `FF80`=actual, `FF81`=expected (60 frames). 13 additional no-verdict ROMs are graded via `mem <addr>=<val>` with disassembly-justified bytes (see the manifest header for per-test provenance).

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

### 3. `temp.gb` — UN-GRADEABLE (dev stub)

**Content [R]:**

```sh
$ xxd -s 0x100 -l 4 gb-test-roms/gbmicrotest/temp.gb   # entry: nop; jp $0150
00000100: 00c3 5001
$ xxd -s 0x150 -l 32 gb-test-roms/gbmicrotest/temp.gb  # $0150: all zero bytes (nop sled)
```

**What happens [V]:** PC slides through zeroed ROM as `nop`s, continues into VRAM-as-code, and collapses into an `RST $38` loop whose pushes walk SP through IO/OAM/WRAM. Deterministic on silicon — but the trajectory executes boot-logo VRAM bytes as opcodes and no capture of the end state exists. There is no verdict write of any kind.

### 4. `halt_op_dupe_delay.gb` — ANALOG

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

## gbc_hw_tests — 75 failures (268/343)

Adopted from AntonioND/gbc-hw-tests (real-device SRAM captures; see SUITES.md
for grading provenance and the revision caveat). Each ROM is graded per
*column*: CGB (`rev=cgbe`, vs `real_gbc.sav`), AGB (`rev=agb`, vs `real_gba.sav`
where the dir ships one, else `real_gba_sp.sav`), and for DMG-flagged ROMs a DMG
column. A "row" below is one (ROM, column) case, which is what the 75 counts.

### Method

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

Several verdicts below need *our* SRAM bytes, not just the first mismatch the
runner prints. The runner has no SRAM-dump flag, so those are re-derived with a
~15-line probe built against this checkout's core, mirroring the runner's `sram`
path exactly (`cart=lazy_sram_cs` → `set_cart_sram_cs_lazy(true)`;
`skip_bios_with_boot_residue()`; 800 frames × 70224 cycles;
`cartridge().save_ram()`). The probe is validated: it reproduces the runner's
reported first mismatch byte-for-byte on both `tac_set_disabled` columns
(`0x0E80` exp `0xFC` got `0xFB`; `0x20EE` exp `0xFC` got `0xFB`). The full recipe
is given once in §3 and reused by name afterwards.

### Family breakdown (rows sum to 75)

| # | Family | Rows | Verdict | Confidence |
|---|---|---:|---|---|
| 1 | `lcd/lcd_frame_timings/*` | 42 | OPEN-TARGET | firm |
| 2 | `memory/oam_echo_ram_*` | 8 | OPEN-TARGET | **provisional** |
| 3 | `timers/tac_set_disabled` | 2 | OPEN-TARGET | firm |
| 4 | `timers/tac_set_when_inc_{16,64,256,1024}` | 4 | OPEN-TARGET | firm |
| 5 | `dma/hdma_halt` | 2 | LOGIC-IMPOSSIBLE + harness gap | firm |
| 6 | `dma/dma_timing_lcd_on` | 2 | OPEN-TARGET | **provisional** |
| 7 | `dma/hdma_timing_fine` | 2 | OPEN-TARGET | **provisional** |
| 8 | `dma/hdma_start_3` | 2 | OPEN-TARGET | **provisional** |
| 9 | `dma/dma_valid_sources_dmg_mode` | 1 | OPEN-TARGET | **provisional** |
| 10 | `lcd/mode2` | 2 | OPEN-TARGET | **provisional** |
| 11 | `lcd/mode3` | 2 | OPEN-TARGET | **provisional** |
| 12 | `serial/sc_change_freq_gbc` | 2 | OPEN-TARGET | **provisional** |
| 13 | `timers/sys_clocks_init_dmg_mode` | 2 | OPEN-TARGET | **provisional** |
| 14 | `timers/tac_set_enabled` | 2 | BLOCKED-ON-ORACLE (CGB) / OPEN-TARGET (AGB) | **provisional** |

42+8+2+4+2+2+2+2+1+2+2+2+2+2 = **75**. Four families (1, 3, 4, 5 — 50 of 75
rows) are firmly adjudicated. The other ten are classified from measured
signatures only; each says what is *not* yet established.

### 1. `lcd/lcd_frame_timings/*` — 42 rows — OPEN-TARGET (firm)

The largest group by far, and the cleanest: these are **not wrong values, they
are a displaced edge**. Across all 42 rows there are 5297 mismatching cells:
**5296 of 5297** sit on a transition in the reference table
(`ref[i] != ref[i-1]`), and **5086 of 5297** (96.0%) satisfy `ours[i] ==
ref[i-1]` — our swept result table is the reference table shifted one probe step
later. Per-row spot checks (CGB column) **[V: probe]**:

| row | mismatches | on a ref transition | `ours[i]==ref[i-1]` |
|---|---:|---:|---:|
| `stat_timings_lyc_0_gbc_mode` | 79 | 79 | 79 |
| `mode1/timings_mode1int_gbc_mode` | 512 | 512 | 512 |
| `ly_equals_lyc/ly_timings_lyc_0_gbc_mode` | 126 | 126 | 124 |

That the values themselves are right (only their position is wrong) is what
distinguishes this from a value bug: the per-cell deltas are *not* a constant
offset (e.g. `stat_timings_lyc_0`: +1 ×28, −3 ×25, +2 ×22, +4 ×2), yet
`ours[i] == ref[i-1]` holds at every one of the 79. A constant-offset value error
cannot produce that.

**Step size [D: upstream `.sym`]:** the sweep's delay ladder is a NOP slide —
`lcd_frame_timings_gbc_mode.sym` exports `stat_read_test_delay_gbc_0..3` at four
*consecutive* addresses `01:4000`–`01:4003`, so successive entry points differ by
one `nop` = 1 M-cycle = 4 dots. One probe step late therefore means **we latch
the observable one M-cycle (4 dots) after silicon does**, in CGB-native timing.

**Not yet established:** *which* edge is late. The 42 rows cover LY, LYC/STAT
coincidence, IF-raise and mode-1/mode-2 entry, and this analysis does not
separate a single shared late latch from several independent ones. The upstream
sweep `main.asm` is **not shipped** for these dirs (only `init.asm`), so the
per-entry semantics were inferred from the `.sym` ladder, not read from source.

### 2. `memory/oam_echo_ram_*` — 8 rows — OPEN-TARGET (**provisional**)

Four ROMs × CGB+AGB. The failures localize sharply to the `$FEA0–$FEFF`
unusable-region decode **[V: probe]** — of the mismatching cells, the fraction
whose index falls in the `>= 0xA0` tail window is:

| row (CGB column) | mismatches | in the `$FEA0+` tail |
|---|---:|---:|
| `oam_echo_ram_read` | 96 | 96 (100%) |
| `oam_echo_ram_read_2` | 4868 | 4868 (100%) |
| `oam_echo_ram_read_gbc_in_dmg_mode` | 96 | 96 (100%) |
| `oam_echo_ram_lcd_on` | 435 | 210 (48%) |

**These are not session flicker.** Two of the dirs ship *two independent CGB
captures*, and they agree perfectly — so unlike the gambatte FEXX family above,
the disagreement-forces-a-choice argument is **not** available here **[R]**:

```sh
$ python3 -c '
for d in ["oam_echo_ram_read","oam_echo_ram_read_2"]:
    p=f"gb-test-roms/gbc-hw-tests/memory/{d}/real_gbc"
    a=open(p+".sav","rb").read(); b=open(p+"_2.sav","rb").read()
    n=min(len(a),len(b))
    print(d, "->", sum(1 for i in range(n) if a[i]!=b[i]), "of", n, "cells differ")'
oam_echo_ram_read -> 0 of 1028 cells differ
oam_echo_ram_read_2 -> 0 of 28932 cells differ
```

**Why provisional:** there is a standing claim that AntonioND's captures and the
`cgb-acid-hell` reference are *mutually exclusive* on the same `$FEA0` cell — which
would make this a revision conflict blocked on capture-unit provenance rather
than an emulator bug. **This document does not assert that, because it was not
verified here.** `cgb_acid_hell` is a PNG-graded rendering suite; establishing
that it pins the same physical cell is real work that has not been done. Until
it is, these 8 rows are counted as an open modelling gap in the `$FEA0–$FEFF`
decode, which is the conservative classification (it counts *against* us).

### 3. `timers/tac_set_disabled` — 2 rows — OPEN-TARGET (firm)

This family is **fully adjudicated**, and the adjudication overturns the
"the two AGB units disagree" excuse that its shape invites.

**Signature [R: runner]:** CGB first mismatch at `0x20EE`, AGB at `0x0E80`, both
`expected 0xFC, got 0xFB` — TIMA one lower, i.e. **silicon took a glitch tick we
did not**. Across the family the deltas are `+1` (160 of 176 CGB cells, 227 of
259 AGB) and `+4` (16 CGB, 32 AGB); the "+1" class dominates but is not the whole
story.

#### Unit disagreement excuses none of it [R]

The two AGB captures *do* disagree — at 1145 cells. But **zero** of our failures
lie there; all of them sit where the two physical units **agree**. Run:

```sh
$ mkdir -p /tmp/tacprobe/src && cd /tmp/tacprobe
$ cat > Cargo.toml <<EOF
[package]
name = "tacprobe"
version = "0.0.0"
edition = "2021"
[dependencies]
rustyboi-core = { path = "$REPO/rustyboi-core" }   # $REPO = this checkout
[workspace]
EOF
$ cat > src/main.rs <<'RS'
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use std::io::Write;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut gb = GB::new(if a[2] == "agb" { Hardware::AGB } else { Hardware::CGBE });
    gb.insert(Cartridge::from_bytes(&std::fs::read(&a[1]).unwrap()).unwrap());
    gb.set_cart_sram_cs_lazy(true);   // manifest `cart=lazy_sram_cs`
    gb.skip_bios_with_boot_residue(); // the runner's sram-oracle seeding
    let (mut run, budget) = (0u64, 800u64 * 70224); // suite frame budget
    while run < budget { run += gb.step_instruction(false).1 as u64; }
    std::fs::File::create(&a[3]).unwrap()
        .write_all(gb.cartridge().unwrap().save_ram()).unwrap();
}
RS
$ D=$REPO/gb-test-roms/gbc-hw-tests/timers/tac_set_disabled
$ cargo run --release --quiet -- $D/tac_set_disable.gbc agb /tmp/tacprobe/ours_agb.bin
$ python3 - /tmp/tacprobe/ours_agb.bin $D/real_gba.sav $D/real_gba_sp.sav <<'PY'
import sys
ours, gba, sp = (open(p,'rb').read() for p in sys.argv[1:4])
n = len(gba)
disagree = {i for i in range(n) if gba[i] != sp[i]}
fail_gba = {i for i in range(n) if ours[i] != gba[i]}
fail_sp  = {i for i in range(n) if ours[i] != sp[i]}
both = fail_gba & fail_sp
print("the two AGB units disagree at   :", len(disagree))
print("ours fails vs real_gba          :", len(fail_gba))
print("ours fails vs real_gba_sp       :", len(fail_sp))
print("ours fails vs BOTH units        :", len(both))
print("of those, inside the disagreement:", len(both & disagree))
PY
the two AGB units disagree at   : 1145
ours fails vs real_gba          : 1404
ours fails vs real_gba_sp       : 259
ours fails vs BOTH units        : 259
of those, inside the disagreement: 0
```

The arithmetic closes on itself: `1404 − 259 = 1145`, the failures-vs-SP set is a
strict subset of the failures-vs-GBA set, and at **every** one of the 1145
disagreement cells our byte equals the SP capture. So the units' disagreement is
entirely orthogonal to our error. **Verdict: OPEN-TARGET, not LOGIC-IMPOSSIBLE.**

#### Root cause, with the author's own rule as oracle

`rustyboi-core/src/timer.rs` gates the whole TAC-write glitch path on the **old**
TAC being enabled:

```rust
fn set_tac(&mut self, data: u8) {
    let cc = self.access_cc();
    if (self.tac ^ data) != 0 {
        let mut next = self.next_irq_event_time;

        if self.tac & TAC_ENABLE != 0 {   // <-- old TAC enabled: AntonioND's DMG rule
```

That is exactly AntonioND's **DMG** rule — `timers/tac_set_everything/DMG.txt`
**[D]**:

```
if OLD_TAC disabled
   GLITCH = 0
else
  if NEW_TAC disabled
      GLITCH = (SYS & OLD_TAC.clocks/2) != 0
   else
      GLITCH = (SYS & OLD_TAC.clocks/2) != 0 && (SYS & NEW_TAC.clocks/2) == 0
```

The same author's `GBC_1.txt` marks that first branch **explicitly not the DMG
rule** **[D]**:

```
if OLD_TAC disabled
   XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX
else // Same as DMG
```

and its own `TAC 0 -> 5` table (`D1024 -> E16` — an old-**disabled** write)
annotates the cells with `(SYS & 512) != 0 && (SYS & 8) == 0`, i.e. GBC applies
the *enabled*-branch formula even when the old TAC was disabled, and duly lists
`512 GLITCH / 516 GLITCH / 520 OK / 524 OK / 528 GLITCH …`.

`timers/tac_set_disabled/gbc_changes.txt` then tabulates exactly which
old-TAC-disabled `2 -> 7` writes glitch on GBC (`TAC=2` has the enable bit clear;
`TAC=7` is enable+freq-3), and cites the SRAM addresses **[D]**:

```
20F0|21F0: TIMA FB
2 -> 7 (28) = OK       11100
2 -> 7 (32) = OK      100000
2 -> 7 (36) = OK      100100
2 -> 7 (40) = GLITCH  101000 1x1x00 TIMA xxxx.xxx1
```

`20F0|21F0` is where our CGB row first diverges (`0x20EE`, and the next
mismatches are `0x20F0, 0x20F6, 0x20F8, 0x21EE, 0x21F0, …`) **[V: probe]**. So
the missing rule, its oracle, and the failing addresses all agree. This is a
genuine modelling gap with a documented deterministic oracle — fixable without
hardware.

### 4. `timers/tac_set_when_inc_{16,64,256,1024}` — 4 rows — OPEN-TARGET (firm)

The same missing rule, one step out, on the AGB column only (the CGB column of
all four **passes**). Our AGB output is **byte-identical to the CGB capture**, and
the cells we fail are **exactly** the cells where the CGB and GBA-SP captures
disagree with each other — 6 per ROM, identical across all four **[V: probe]**:

```
inc_16   ours-vs-SP fails=6 at [0x2e,0x3e,0x3f,0x22e,0x23e,0x23f]  ours==CGB capture: True
inc_64   ours-vs-SP fails=6 at [0x2e,0x3e,0x3f,0x22e,0x23e,0x23f]  ours==CGB capture: True
inc_256  ours-vs-SP fails=6 at [0x2e,0x3e,0x3f,0x22e,0x23e,0x23f]  ours==CGB capture: True
inc_1024 ours-vs-SP fails=6 at [0x2e,0x3e,0x3f,0x22e,0x23e,0x23f]  ours==CGB capture: True
```

i.e. we produce CGB behaviour on AGB hardware, and the delta is precisely one
AGB-only extra glitch. The author annotates that cell class directly: in all four
`info_{16,64,256,1024}.txt`, `(GBA 1)` is the **only** annotation in the file,
appears exactly twice, and always on the same cell — section `16` (old TAC =
D16), context `DISABLED, NO INC`, new TAC `E256` **[D]**:

```
  DISABLED, NO INC, 00
     D1024, D16, D64, D256,    E1024, E16, E64, E256(GBA 1)
```

So AGB additionally glitches on `D16 -> E256` where CGB does not — again an
**old-TAC-disabled** write, the same gate as §3. Correct column split, same fix.

**Caveat, stated plainly:** these four dirs ship **no `real_gba.sav`**, only
`real_gba_sp.sav`, so the AGB-only cell is attested by **one** physical unit; and
the `(GBA 1)` note was presumably written from that same capture, so it is
corroborating documentation, not an independent second unit. The manifest header
accordingly says "DO NOT model" these rows. That caution is about *fitting a
constant to one unit* — it does not make the rows un-gradeable, and the correct
fix (§3's missing old-disabled branch, with the AGB column glitching one cell
class more) is not a per-unit constant.

### 5. `dma/hdma_halt` — 2 rows — LOGIC-IMPOSSIBLE, plus a harness gap (firm)

Two separate things are true here, and the first one is not an emulator defect.

**(a) The test never completes under the current manifest row.** `main.asm`
executes `stop` and waits for a keypress; the manifest gives `hdma_halt` **no
`input=` token** (its sibling `dma_halt_stop_speedchange` has `input=0:a`). We
therefore wedge at the STOP and write only 3 of the 10 result bytes — the
"failure" is mostly a truncated run **[V: probe]**:

```
ours NO press  13 13 13 34 56 78 FF FF FF FF FF FF FF FF
ours HOLD a    13 13 13 13 90 13 13 12 94 12 12 34 56 78
ref_gbc        13 13 13 12 69 0D 13 12 94 78 12 34 56 78
ref_gba_sp     13 13 13 13 46 0E 13 12 94 FF 12 34 56 78
```

With a button held the run completes (the `12 34 56 78` terminator lands at
offsets 10–13, matching the captures' structure). **This is a manifest fix, not a
core fix, and it is not made here** — the manifest is owned elsewhere; see the
deliverable note.

**(b) The residual cells are genuinely un-gradeable.** Even with the button held,
our remaining mismatches are at offsets `{0x3, 0x4, 0x5, 0x9}` — which is
**exactly** the set of cells where the two physical captures disagree with each
other **[R]**:

```sh
$ python3 -c '
g=open("gb-test-roms/gbc-hw-tests/dma/hdma_halt/real_gbc.sav","rb").read()[:14]
s=open("gb-test-roms/gbc-hw-tests/dma/hdma_halt/real_gba_sp.sav","rb").read()[:14]
print("CGB vs GBA-SP capture disagree at:", [hex(i) for i in range(14) if g[i]!=s[i]])'
CGB vs GBA-SP capture disagree at: ['0x3', '0x4', '0x5', '0x9']
```

Those cells are the `rHDMA5` progress reads and the `LY` read taken immediately
after the STOP exits — all functions of **when a human pressed the button**. Two
capture sessions, two press moments, two answers for the same physical quantity:
a deterministic emulator cannot satisfy both. **LOGIC-IMPOSSIBLE** for those 4
cells; the other 6 result bytes we already match.

### 6–13. Signature-classified families (**provisional**)

These 15 rows are classified from measured mismatch signatures alone. Each is
counted as an open accuracy work item — the conservative choice — but the
mechanism is **not** established, and none should be quoted as proven.

| Family | Rows | Cells | Measured signature | What it suggests |
|---|---:|---:|---|---|
| `dma/dma_timing_lcd_on` | 2 | 90 | 100% on a ref transition, 51% `ours[i]==ref[i-1]` | edge displacement like §1, but only half the cells shift cleanly |
| `dma/hdma_timing_fine` | 2 | 64 | every cell `expected = ours + 2` | a uniform 2-unit HDMA timing offset, not a displaced edge |
| `dma/hdma_start_3` | 2 | 16 | every cell `expected = ours − 1` (`exp 0x0B got 0x0C`) | one-unit HDMA start offset |
| `dma/dma_valid_sources_dmg_mode` (DMG) | 1 | 108 | every cell `exp 0x80 got 0xC0` | a single wrong decode bit, uniform across the sweep |
| `lcd/mode2` | 2 | 8 | 4 cells/row, `exp 0xFF got 0x00` | OAM readability during mode 2, not a timing shift |
| `lcd/mode3` | 2 | 1293 | only **6.3%** on a ref transition; deltas cluster at −56 (371) and −59 (261) | **not** edge displacement — a value error, unlike §1 |
| `serial/sc_change_freq_gbc` | 2 | 3478 | 100% on a ref transition but **0%** `ours[i]==ref[i-1]` | transitions in the right places, wrong values at them |
| `timers/sys_clocks_init_dmg_mode` | 2 | 2 | 1 cell/row, `expected = ours − 1` | initial SYS-clock counter off by one |

Note that `lcd/mode3` is grouped with `lcd/lcd_frame_timings` in casual
discussion; the measurement above says that is **wrong** — mode3's failures do
not have the displacement signature at all (6.3% vs 100%), so it needs its own
investigation.

### 14. `timers/tac_set_enabled` — 2 rows — BLOCKED-ON-ORACLE / OPEN-TARGET (**provisional**)

The two columns are unrelated problems.

**AGB (112 cells):** every cell `expected = ours + 1` — the §3 signature exactly.
Almost certainly the same missing old-TAC-disabled branch; grouped separately
only because it was not traced to specific `gbc_changes.txt` rows.

**CGB (574 cells, first mismatch at offset `0x0000`):** the CGB capture does not
have the same shape as the run it is grading **[V: probe]**:

```
real_gbc.sav     len= 15364  head=00 00 00 00 00 00 00 00 00 00 00 00
real_gba.sav     len= 31748  head=FB 00 FB 00 FB 00 FB 00 FB 00 FB 00
real_gba_sp.sav  len= 31748  head=FB 00 FB 00 FB 00 FB 00 FB 00 FB 00
ours (cgbe)      len= 32768  head=FB 00 FB 00 FB 00 FB 00 FB 00 FB 00
```

The CGB capture is 15364 bytes — the **DMG-family** length (`real_gb.sav` and
`real_gbp.sav` are also 15364) — and opens with a 256-byte all-zero run where
both AGB units and our CGB run have data. It is not equal to the DMG capture, nor
to either half of a GBA capture. Grading our 32768-byte SRAM's first 15364 bytes
against it is very likely comparing misaligned regions, which is what produces a
mismatch at offset 0. **Until that capture's provenance is resolved this row
asserts little**, so the CGB column is filed BLOCKED-ON-ORACLE. This is an
observation about the oracle, not a claim that our output is right.

---

## Floor arithmetic

- **gbmicrotest:** 509/513 is the maximum for any register-level emulator without inventing oracles. +2 (`500-scx-timing`, `minimal`) become gradeable the day a hardware capture of the absolute byte exists; `temp` is capturable in principle; `halt_op_dupe_delay` requires characterizing analog die physics.
- **gambatte:** 7 (residue tail) + 1 (fexx) + 1 (C113) = **9 is the permanent minimum for any deterministic emulator, including a perfect gate-level one** — every failing byte is pinned oppositely by a *currently-passing* capture of the same physical quantity, and the exhaustive subset search confirms the current choices are globally optimal.
- **gbc_hw_tests:** 268/343 is a ratcheted progress floor and, unlike the two suites above, **nothing like a proven ceiling** — 74 of the 75 failures are classified OPEN-TARGET or BLOCKED-ON-ORACLE, i.e. fixable or oracle-limited, not forced. Only the 4 residual `hdma_halt` cells (§5b) are LOGIC-IMPOSSIBLE, and even those sit behind a manifest gap (§5a) that currently truncates the run before they are reached. The realistic near-term ceiling is well above 268: §1 (42 rows) and §3–§4 (6 rows) share two concrete, oracle-backed root causes.
- 4 + 9 + 75 = 88: every gbmicrotest and gambatte failure is accounted for byte by byte, and every gbc_hw_tests failure now carries a family, a verdict class and a confidence label — with 50 of its 75 rows firmly adjudicated and the remaining 25 explicitly marked provisional rather than overstated.

# Known Failures — every failing ROM, with proof

72 of 6843 test cases fail across the 28 suites: 4 in gbmicrotest, 9 in gambatte, 59 in gbc_hw_tests. Every one of the 72 is adjudicated below. Entries make **no assumptions**: every claim is tagged with its provenance, reproducible claims include the command that re-verifies them against this tree, and claims that outrun their evidence are labelled **PROVISIONAL** rather than dressed up.

Counts measured at `2247d365` after an explicit `cargo build --release -p rustyboi-test-runner` (this tree has a documented stale-binary trap that has produced false PASS, false FAIL *and* a committed false README count — never quote a number produced without a rebuild) **[R]**:

```sh
$ cargo build --release -p rustyboi-test-runner   # MANDATORY: never skip
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh gbc_hw_tests
Ran 343 total tests.
59 total failures.
Ran 302 CGB tests.
58 CGB failures.
Ran 41 DMG tests.
1 DMG failures.
PASS  gbc_hw_tests         passed=284/343 (floor: passed>=284)
```

The other two suites are unchanged at this revision **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh gbmicrotest gambatte
PASS  gbmicrotest          passed=509/513 (floor: passed>=509)
PASS  gambatte             passed=5248/5257 failed=9 (floor: failed<=9)
```

So the corpus stands at **6771/6843**: 6843 − 6771 = 72 = 4 + 9 + 59.

> **The README suite table is stale and is deliberately not the source here.** It currently reads `gbc_hw_tests | 278 | 343` and `**Total** | **6765** | **6843**` — six rows behind the measured 284, because six fixes landed after that table was last regenerated. This document's arithmetic comes from the runs pasted above, not from the README. Fixing the README is a separate change and is out of scope for this file.

**A gitignored-asset warning, because it silently falsified results three times in one day.** `gb-test-roms/`, `bios/` and `test-roms/build/` are gitignored, so a fresh worktree does not have them and the suite will happily "pass" while skipping nearly everything. Before quoting any number, confirm the run reports **`Ran 343 total tests`** for gbc_hw_tests. A row count below that means missing ROMs, not progress.

Provenance tags:

- **[R]** Reproducible here — run the command shown against this checkout.
- **[V]** Verified against third-party references built from source (SameBoy, libgambatte, GateBoy sources) or by instrumented traces/experiments; the method is described where cited.
- **[D]** Documented upstream — Pan Docs, gekkio's gb-ctr, or the test author's own files.

Verdict classes:

- **LOGIC-IMPOSSIBLE** — real-hardware captures pin the *same physical quantity* to different values in different capture sessions. A deterministic emulator must pick one value, so for each such family the failure count is forced by arithmetic, not by a modeling gap.
- **ORACLE-CONFLICT** — two *different* real-hardware oracles, each independently credible, demand incompatible answers for the same behaviour. Distinct from LOGIC-IMPOSSIBLE, which is one oracle disagreeing with *itself* across sessions: here each oracle is internally consistent, so the failure is not forced by arithmetic — it is forced by a choice about *which oracle is authoritative*, and that choice cannot be made from the captures alone. Three of these surfaced on 2026-07-20 and the document previously had no vocabulary for them. Resolving one requires establishing provenance (which silicon revision, which physical unit, which capture is intact) — not emulator work. Note that an ORACLE-CONFLICT can dissolve: the `oam_echo_ram` conflict below was resolved as a **revision mismatch** once both sides' `rev=` pins were compared, and both oracles now pass simultaneously.
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

## gbc_hw_tests — 59 failures (284/343)

Adopted from AntonioND/gbc-hw-tests (real-device SRAM captures; see SUITES.md
for grading provenance and the revision caveat). Each ROM is graded per
*column*: CGB (`rev=cgbe`, vs `real_gbc.sav`), AGB (`rev=agb`, vs `real_gba.sav`
where the dir ships one, else `real_gba_sp.sav`), and for DMG-flagged ROMs a DMG
column. A "row" below is one (ROM, column) case, which is what the 59 counts.

**This section was rewritten on 2026-07-21 after 16 rows were fixed** (75 → 59).
Six families that the previous revision described as failing are now fully green
and their entries have been deleted rather than left standing: `hdma_start_3`
(2 rows), `sys_clocks_init_dmg_mode` (2), `tac_set_when_inc_{16,64,256,1024}`
(4 AGB rows), three of the four `oam_echo_ram_*` dirs (6 rows), plus the CGB
columns of `tac_set_disabled` and `sc_change_freq_gbc` (1 each). Two of the
conclusions below **overturn** what the previous revision claimed; those are
flagged inline.

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

Most verdicts below need *our* SRAM bytes, not just the first mismatch the runner
prints. The runner has no SRAM-dump flag, so those are re-derived with a ~15-line
probe built against this checkout's core, mirroring the runner's `sram` path
exactly (`cart=lazy_sram_cs` → `set_cart_sram_cs_lazy(true)`;
`skip_bios_with_boot_residue()`; **800** frames × 70224 cycles — the suite's flat
budget, see `tools/run-suites.sh` line 169 `echo "284 800"`;
`cartridge().save_ram()`):

```sh
$ mkdir -p /tmp/probe/src/bin && cd /tmp/probe
$ cat > Cargo.toml <<EOF
[package]
name = "probe"
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
    // args: <rom> <cgbe|agb|dmg> <out> [frames]
    let hw = match a[2].as_str() {
        "agb" => Hardware::AGB,
        "dmg" => Hardware::DMG,
        _ => Hardware::CGBE,
    };
    let mut gb = GB::new(hw);
    gb.insert(Cartridge::from_bytes(&std::fs::read(&a[1]).unwrap()).unwrap());
    gb.set_cart_sram_cs_lazy(true);
    gb.skip_bios_with_boot_residue();
    let frames: u64 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(800);
    let (mut run, budget) = (0u64, frames * 70224);
    while run < budget { run += gb.step_instruction(false).1 as u64; }
    std::fs::File::create(&a[3]).unwrap()
        .write_all(gb.cartridge().unwrap().save_ram()).unwrap();
}
RS
$ cargo build --release
```

`src/bin/hold.rs` is the same program with
`gb.set_input_state(ButtonState { a: true, ..Default::default() })` after the
boot skip; it is needed only for `hdma_halt`, whose manifest row carries
`input=0:a`. **The plain probe must not be used on `hdma_halt`** — without the
button the ROM wedges at its `stop` and every byte past offset 2 is unwritten
`0xFF`, which looks like a catastrophic failure and is an artifact.

The probe is validated against the runner: it reproduces the runner's reported
first mismatch byte-for-byte **[R]**:

```sh
$ D=$REPO/gb-test-roms/gbc-hw-tests/timers/tac_set_disabled
$ ./target/release/probe $D/tac_set_disable.gbc agb /tmp/ours_agb.bin
$ python3 -c "
ours=open('/tmp/ours_agb.bin','rb').read(); ref=open('$D/real_gba.sav','rb').read()
n=min(len(ours),len(ref)); m=[i for i in range(n) if ours[i]!=ref[i]]
print('probe first mismatch: 0x%04X expected 0x%02X got 0x%02X' % (m[0], ref[m[0]], ours[m[0]]))
print('total mismatches:', len(m))"
probe first mismatch: 0x0E80 expected 0xFC got 0xFB
total mismatches: 1148
```

which is exactly what the runner prints for that row (`first mismatch at offset
0x0E80: expected 0xFC, got 0xFB`).

Several sections below quote a per-cell dump. They all use this one helper,
referred to as **`cells.py`**; the family sections cite it as **[R: `cells.py`]**
rather than repeating it:

```sh
$ cat > /tmp/cells.py <<'PY'
import sys, collections
ours = open(sys.argv[1], 'rb').read()
ref  = open(sys.argv[2], 'rb').read()
n = min(len(ours), len(ref))
m = [j for j in range(n) if ours[j] != ref[j]]
print("graded=%d mismatches=%d" % (n, len(m)))
print("deltas(exp-ours):", dict(collections.Counter(ref[j] - ours[j] for j in m).most_common(4)))
for j in m[: int(sys.argv[3]) if len(sys.argv) > 3 else 12]:
    print("    off=0x%04X (mod 0x100 = 0x%02X) exp=0x%02X ours=0x%02X"
          % (j, j % 0x100, ref[j], ours[j]))
PY
$ $PROBE <rom> <cgbe|agb|dmg> /tmp/ours.bin && python3 /tmp/cells.py /tmp/ours.bin <ref.sav>
```

### Two measurement traps that invalidate naive triage

Both were discovered on 2026-07-20 after they had already produced wrong
classifications *in the previous revision of this document*. Read this before
quoting any cell count.

**Trap 1 — a mismatch count at the default budget is not a proximity metric.**
The suite runs a flat 800-frame budget. A ROM that has not finished writing its
result table by frame 800 shows its unwritten tail as mismatches, and those
counts swamp the real ones. `lcd/mode3` is the live example **[R]**:

```sh
$ D=$REPO/gb-test-roms/gbc-hw-tests/lcd/mode3/mode3_stat_timing_spr_en_gbc_mode_8x16
$ for f in 800 4000; do
    ./target/release/probe $D/mode3_timing_spr_en.gbc cgbe /tmp/m3_$f.bin $f
    python3 -c "
ours=open('/tmp/m3_$f.bin','rb').read(); r=open('$D/real_gbc.sav','rb').read()
n=min(len(ours),len(r)); m=[j for j in range(n) if ours[j]!=r[j]]
print('frames=$f  graded=%d  mismatches=%d  (of which ours==0xFF unwritten: %d)'
      % (n,len(m),sum(1 for j in m if ours[j]==0xFF)))"
  done
frames=800  graded=1284  mismatches=647  (of which ours==0xFF unwritten: 636)
frames=4000  graded=1284  mismatches=11  (of which ours==0xFF unwritten: 0)
```

636 of the 647 "failures" are simply bytes the ROM had not written yet. The
previous revision reported `lcd/mode3` as "1293 cells, deltas cluster at −56 and
−59" and concluded it was **"a value error, unlike §1"** — that conclusion was
drawn entirely from truncation noise. The real error is 11 cells.

A sweep of every failing family at 800 vs 4000 frames shows `lcd/mode3` is the
**only** truncation-dominated one; all others are stable **[R: probe, both
budgets]**, so this trap is now bounded rather than suspected:

| family | @800 | @4000 |
|---|---:|---:|
| `dma/dma_timing_lcd_on` (both cols) | 45 | 45 |
| `dma/dma_valid_sources_dmg_mode` | 108 | 108 |
| `dma/hdma_timing_fine` (both cols) | 32 | 32 |
| `lcd/mode2` (both cols) | 4 | 4 |
| **`lcd/mode3` (both cols)** | **647 / 646** | **11 / 11** |
| `memory/oam_echo_ram_lcd_on` | 12 / 4 | 12 / 4 |
| `serial/sc_change_freq_gbc#agb` | 1895 | 1895 |
| `timers/tac_set_disabled#agb` | 1148 | 1148 |
| `timers/tac_set_enabled` | 574 / 112 | 574 / 112 |

**Trap 2 — the edge-displacement predicate under-reports contiguous
displacements.** The `ours[i] == ref[i±1]` test only fires on the *outermost*
cell of a run: if an edge moves by *k* steps, the k−1 interior cells have a
same-valued neighbour and score as non-displaced. `lcd/mode2` is the clean
demonstration — the predicate scores it **0%**, yet the failure is a single
contiguous 4-cell edge, i.e. 100% displacement **[R]**:

```sh
$ B=$REPO/gb-test-roms/gbc-hw-tests/lcd/mode2/mode2_read_oam_spr_dis_dmg_mode
$ $PROBE $B/mode2_read_oam_dmg_mode.gbc cgbe /tmp/m2.bin && python3 /tmp/cells.py /tmp/m2.bin $B/real_gbc.sav 4
graded=68 mismatches=4
deltas(exp-ours): {255: 4}
    off=0x0032 (mod 0x100 = 0x32) exp=0xFF ours=0x00
    off=0x0033 (mod 0x100 = 0x33) exp=0xFF ours=0x00
    off=0x0034 (mod 0x100 = 0x34) exp=0xFF ours=0x00
    off=0x0035 (mod 0x100 = 0x35) exp=0xFF ours=0x00
```

Never read a low `ours[i]==ref[i-1]` percentage as "not a displacement" without
plotting the run lengths first.

### Family breakdown (rows sum to 59)

| # | Family | Rows | Verdict | Confidence |
|---|---|---:|---|---|
| 1 | `lcd/lcd_frame_timings/*` | 42 | OPEN-TARGET | firm |
| 2 | `dma/hdma_halt` | 2 | UN-GRADEABLE (press timing) | firm |
| 3 | `timers/tac_set_disabled#agb` | 1 | ORACLE-CONFLICT (1145 cells) + OPEN-TARGET (3 cells) | firm |
| 4 | `timers/tac_set_enabled` | 2 | ORACLE-CONFLICT / BLOCKED-ON-ORACLE (CGB) + OPEN-TARGET (AGB) | firm |
| 5 | `dma/dma_valid_sources_dmg_mode` | 1 | ORACLE-CONFLICT | firm |
| 6 | `lcd/mode3` | 2 | harness (manifest `frames=`) + OPEN-TARGET (11 cells) | firm |
| 7 | `memory/oam_echo_ram_lcd_on` | 2 | OPEN-TARGET (OAM-lock window) | firm |
| 8 | `lcd/mode2` | 2 | OPEN-TARGET (4-cell edge) | firm |
| 9 | `dma/hdma_timing_fine` | 2 | OPEN-TARGET (uniform +2) | firm |
| 10 | `dma/dma_timing_lcd_on` | 2 | OPEN-TARGET (displaced write index) | **provisional** |
| 11 | `serial/sc_change_freq_gbc#agb` | 1 | OPEN-TARGET | **provisional** |

42+2+1+2+1+2+2+2+2+2+1 = **59**. Nine families (1–9 — 56 of 59 rows) are firmly
adjudicated: each has either a measured mechanism, a named oracle conflict, or a
harness cause. Only families 10–11 (3 rows) rest on signature alone.

### 1. `lcd/lcd_frame_timings/*` — 42 rows — OPEN-TARGET (firm)

26 rows under `ly_equals_lyc/`, 10 under `mode1/`, 6 under `mode2/`.

**The previous revision's headline reading of this family was wrong, and the
correction matters.** It reported that "our swept result table is the reference
table shifted one probe step later", inferred a uniform **one M-cycle (4 dot)**
late latch, and rated it *firm*. The underlying statistic still reproduces
exactly — across all 42 rows there are 5297 mismatching cells, **5296** sit on a
reference transition and **5086 (96.0%)** satisfy `ours[i] == ref[i-1]` **[R:
probe]** — but that statistic is **conditioned on the mismatching cells**, and
mismatching cells are by construction at edges, where `ours[i] == ref[i-1]` is
what *any* locally-displaced edge looks like. It therefore cannot distinguish a
global phase error from a narrow boundary error.

**The unconditioned test discriminates, and it refutes the global reading [R].**
If our table really were the reference shifted one step, then comparing our
table against the reference *offset by one* would make it match. It does the
opposite — it makes every single row roughly an order of magnitude worse:

Run from the repo root, with `$PROBE` pointing at the Method-section binary:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh gbc_hw_tests 2>&1 \
    | grep '^FAILED:' | grep lcd_frame_timings > /tmp/ft_fails.txt
$ python3 - <<'PY'
import re, subprocess, os
probe = os.environ['PROBE']
rows = []
for ln in open('/tmp/ft_fails.txt'):
    m = re.match(r'FAILED: (\S+) (CGB|DMG) (\S+?): SRAM dump (\S+?): ', ln)
    rom, mode, refname, refpath = m.groups()
    hw = 'dmg' if mode == 'DMG' else ('agb' if 'gba' in refname else 'cgbe')
    rows.append((rom, hw, refpath))

tot_n = tot_a = tot_s = 0
for i, (rom, hw, ref) in enumerate(rows, 1):
    out = f'/tmp/ft_{i:02d}.bin'
    if not os.path.exists(out):
        subprocess.run([probe, rom, hw, out], check=True)
    ours = open(out, 'rb').read(); r = open(ref, 'rb').read()
    n = min(len(ours), len(r))
    aligned = sum(1 for j in range(n) if ours[j] != r[j])          # graded as-is
    shifted = sum(1 for j in range(1, n) if ours[j] != r[j-1])     # ref offset by one
    tot_n += n; tot_a += aligned; tot_s += shifted
print(f"TOTAL over {len(rows)} rows: graded={tot_n}  "
      f"aligned-mismatches={tot_a}  shifted-by-one-mismatches={tot_s}")
PY
TOTAL over 42 rows: graded=3724768  aligned-mismatches=5297  shifted-by-one-mismatches=52573
```

Per-row, the aligned/shifted pairs run e.g. `stat_timings_lyc_0` 79 → 2062,
`stat_timings_lyc_152_153` 162 → 4141, `ly_timings_lyc_0` 126 → 850,
`mode1/timings_mode1int` 512 → 1646, `mode2/timings_mode2int_ly0_dmg` 293 → 1501
**[R: `cells.py`]**. Not one row improves under the shift.

The correct statement is therefore: **5297 of 3,724,768 graded cells (0.14%) are
wrong; the overwhelming majority of every result table is byte-perfect against
its own reference, and the error is confined to narrow window boundaries.** There
is no shared 4-dot phase error to remove.

**Two candidate fixes were tried and reverted on 2026-07-20 [V: session, not
reproducible here — the core is unmodified at this revision]:** a uniform +4cc LY
window took the suite *down* 268 → 236; an `ly & (ly+1)` fold on CGB-D/E took the
`age` suite 56 → 51. Both are consistent with the refutation above — a uniform
correction cannot help an error that is not uniform.

**Step size [D: upstream `.sym`]:** the sweep's delay ladder is a NOP slide —
`lcd_frame_timings_gbc_mode.sym` exports `stat_read_test_delay_gbc_0..3` at four
*consecutive* addresses `01:4000`–`01:4003`, so successive entry points differ by
one `nop` = 1 M-cycle = 4 dots. This still fixes the *unit* of the sweep; it no
longer supports any claim about a uniform offset.

**A same-revision oracle contradiction blocks most of `ly_equals_lyc` [R + V].**
The reproducible half: `age`'s `ly/ly-cgbE.gb` and `lcd-align-ly/lcd-align-ly-cgbE.gb`
are pinned `rev=cgbe` and **pass**, while AntonioND's `ly_equals_lyc` rows are
pinned `rev=cgbe` and **fail** — both oracles claim CGB-E silicon:

```sh
$ grep -n 'ly-cgbE' rustyboi-test-runner/suites/age.manifest
11:age-test-roms/lcd-align-ly/lcd-align-ly-cgbE.gb|cgb|mooneye|...|rev=cgbe
13:age-test-roms/ly/ly-cgbE.gb|cgb|mooneye|...|rev=cgbe
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh age
PASS  age                  passed=56/56 (floor: passed>=56)
```

The **PROVISIONAL** half, which is what makes it a conflict rather than just two
unrelated results: the reverted experiment above indicates AntonioND `real_gbc`
requires the `ly & (ly+1)` fold at the glitch dot while the `age` roms require
the non-fold, so satisfying one breaks the other **[V: session experiment, 56 →
51]**. If that holds, the fold is conditional on something *not* revision-keyed,
because both oracles assert the same revision. Compounding it: **our `rev=cgbe`
pin is itself an inference**, from SameBoy-built-from-source behaviour (see the
manifest header), not documented unit provenance — AntonioND documents no
revision at all. This is filed under OPEN-TARGET rather than ORACLE-CONFLICT only
because the mutual exclusivity has not been re-demonstrated at this revision.

### 2. `dma/hdma_halt` — 2 rows — UN-GRADEABLE (firm)

**The harness gap the previous revision described is closed.** It reported that
the ROM wedges at its `stop` because the manifest row carried no `input=`; the
row now carries `input=0:a` **and** `skip=0x3-0x6` **[R]**:

```sh
$ grep -n 'hdma_halt' rustyboi-test-runner/suites/gbc_hw_tests.manifest | grep -o '^[0-9]*:.*hdma_halt|\|input=.*'
115:gbc-hw-tests/dma/hdma_halt|
input=0:a|skip=0x3-0x6|rev=cgbe
input=0:a|skip=0x3-0x6|rev=agb
```

With the button held and the skip window applied, **exactly one cell remains
wrong on each column — offset `0x9` [R: `hold` probe]**:

```sh
$ D=$REPO/gb-test-roms/gbc-hw-tests/dma/hdma_halt
$ $HOLD $D/hdma_halt.gbc cgbe /tmp/hh_cgb.bin      # src/bin/hold.rs — holds A
$ $HOLD $D/hdma_halt.gbc agb  /tmp/hh_agb.bin
$ python3 - <<'PY'
import os
D = os.environ['REPO'] + "/gb-test-roms/gbc-hw-tests/dma/hdma_halt/"
g  = open(D+'real_gbc.sav','rb').read()[:14]; s  = open(D+'real_gba_sp.sav','rb').read()[:14]
oc = open('/tmp/hh_cgb.bin','rb').read()[:14]; oa = open('/tmp/hh_agb.bin','rb').read()[:14]
print("offset       :", " ".join("%2X" % i for i in range(14)))
print("ours (cgbe)  :", " ".join("%02X" % b for b in oc))
print("ref real_gbc :", " ".join("%02X" % b for b in g))
print("ours (agb)   :", " ".join("%02X" % b for b in oa))
print("ref gba_sp   :", " ".join("%02X" % b for b in s))
sk = set(range(3, 7))                                   # manifest skip=0x3-0x6
print()
print("graded cells (0x3-0x6 skipped):",
      ",".join(hex(i)[2:].upper() for i in range(14) if i not in sk))
print("ours(cgbe) != real_gbc   at:", [hex(i) for i in range(14) if i not in sk and oc[i] != g[i]])
print("ours(agb)  != real_gba_sp at:", [hex(i) for i in range(14) if i not in sk and oa[i] != s[i]])
PY
offset       :  0  1  2  3  4  5  6  7  8  9  A  B  C  D
ours (cgbe)  : 13 13 13 13 90 13 13 12 94 12 12 34 56 78
ref real_gbc : 13 13 13 12 69 0D 13 12 94 78 12 34 56 78
ours (agb)   : 13 13 13 13 90 13 13 12 94 12 12 34 56 78
ref gba_sp   : 13 13 13 13 46 0E 13 12 94 FF 12 34 56 78

graded cells (0x3-0x6 skipped): 0,1,2,7,8,9,A,B,C,D
ours(cgbe) != real_gbc   at: [0x9]
ours(agb)  != real_gba_sp at: [0x9]
```

**Cell `0x9` is the only capture disagreement the skip window does not already
cover [R]** — the two physical captures read `0x78` (CGB) and `0xFF` (GBA-SP)
there. These cells are `rHDMA5` progress and the `LY` read taken immediately
after `stop` exits: all functions of **when a human pressed the button**. That is
precisely the rationale under which `0x3-0x6` were skipped.

**We match neither capture (we produce `0x12`), so this is not the forced
one-of-two choice the previous revision implied.** Our press moment is the
harness's — held from frame 0 — and corresponds to neither capture session's
human press. There is no value we could produce that is *right*, because the
quantity is not a property of the silicon. **UN-GRADEABLE**, and the consistent
follow-up is a manifest change extending `skip=` to include `0x9` on the same
grounds that justified `0x3-0x6`; that would take both rows green. The manifest
is out of scope for this file.

### 3. `timers/tac_set_disabled#agb` — 1 row — ORACLE-CONFLICT + OPEN-TARGET (firm)

The CGB column, which the previous revision documented at length as an
OPEN-TARGET traced to the missing old-TAC-disabled branch, **now passes** — the
fix landed as `timer: model the CGB/AGB old-TAC-disabled TAC-write glitch`
(`6a79db9e`). Only the AGB column remains, and **the fix inverted its
classification** [R]:

```sh
$ python3 - <<'PY'
D="$REPO/gb-test-roms/gbc-hw-tests/timers/tac_set_disabled/"
ours=open('/tmp/ours_agb.bin','rb').read()          # probe, rev=agb, 800 frames
gba=open(D+'real_gba.sav','rb').read(); sp=open(D+'real_gba_sp.sav','rb').read()
n=len(gba)
dis={i for i in range(n) if gba[i]!=sp[i]}
fg={i for i in range(n) if ours[i]!=gba[i]}; fs={i for i in range(n) if ours[i]!=sp[i]}
print("the two AGB units disagree at    :", len(dis))
print("ours fails vs real_gba (graded)  :", len(fg))
print("ours fails vs real_gba_sp        :", len(fs))
print("ours fails vs BOTH units         :", len(fg&fs))
print("our failures inside the disagreement:", len(fg&dis), "of", len(fg))
PY
the two AGB units disagree at    : 1145
ours fails vs real_gba (graded)  : 1148
ours fails vs real_gba_sp        : 3
ours fails vs BOTH units         : 3
our failures inside the disagreement: 1145 of 1148
```

**1145 of our 1148 failing cells are cells where the two physical AGB units
contradict each other**, and we now agree with the GBA-SP unit everywhere except
3 cells. The row is graded against `real_gba.sav`. So this row is now
overwhelmingly a question of *which AGB unit is authoritative* — an
**ORACLE-CONFLICT** — and not a modelling gap.

This **overturns** the previous revision's §3, which measured `fails vs real_gba
= 1404, vs real_gba_sp = 259, inside the disagreement = 0` and concluded
"the units' disagreement is entirely orthogonal to our error… **Verdict:
OPEN-TARGET, not LOGIC-IMPOSSIBLE**." That was true of the *pre-fix* model. After
the fix the relationship reversed, and the old conclusion must not be quoted.

Note the manifest header is correspondingly stale: it says `tac_set_disabled`
"fails against BOTH units (at 0x0E80 vs the GBA, 0x0EE0 vs the SP), so it is a
genuine unmodelled behaviour, not a unit-selection artifact." Post-fix, it fails
against both units at **3** cells, not wholesale. Manifest edits are out of scope
here.

**The genuine residue is 3 cells**, where both units agree and we are one TIMA
lower — the same `+1` glitch class, one step out **[R]**:

```
0x638E: real_gba=0xFD  real_gba_sp=0xFD  ours=0xFC
0x6796: real_gba=0xFF  real_gba_sp=0xFF  ours=0xFE
0x6B94: real_gba=0x01  real_gba_sp=0x01  ours=0x00
```

Those 3 are OPEN-TARGET. The other 1145 are not adjudicable without the hardware
bench.

### 4. `timers/tac_set_enabled` — 2 rows — ORACLE-CONFLICT / BLOCKED-ON-ORACLE (CGB) + OPEN-TARGET (AGB) (firm)

**CGB column — the capture is structurally defective, and this is now firm
rather than provisional.** The previous revision noticed the anomalous length and
filed it BLOCKED-ON-ORACLE provisionally. Three independent facts now pin it
**[R]**:

```sh
$ cd $REPO/gb-test-roms/gbc-hw-tests/timers
$ for f in tac_set_enabled/real_*.sav; do printf "%-34s %7d bytes  head=%s\n" \
    "$f" "$(stat -c%s $f)" "$(xxd -l6 -p $f)"; done
tac_set_enabled/real_gba.sav         31748 bytes  head=fb00fb00fb00
tac_set_enabled/real_gba_sp.sav      31748 bytes  head=fb00fb00fb00
tac_set_enabled/real_gbc.sav         15364 bytes  head=000000000000
tac_set_enabled/real_gbp.sav         15364 bytes  head=fb00fb00fb00
tac_set_enabled/real_gb.sav          15364 bytes  head=fb00fb00fb00
$ for f in tac_set_disabled/real_*.sav; do printf "%-34s %7d bytes\n" "$f" "$(stat -c%s $f)"; done
tac_set_disabled/real_gba.sav        31748 bytes
tac_set_disabled/real_gba_sp.sav     31748 bytes
tac_set_disabled/real_gbc.sav        31748 bytes     # <-- sibling dir: CGB-shaped
tac_set_disabled/real_gbp.sav        15364 bytes
tac_set_disabled/real_gb.sav         15364 bytes
```

1. **Wrong size class.** For this ROM family DMG-family units produce 15364-byte
   captures and CGB/AGB units produce 31748. `tac_set_enabled/real_gbc.sav` is
   15364 — DMG-shaped — while the sibling `tac_set_disabled/real_gbc.sav` is the
   expected 31748.
2. **Anomalous even among the 15364-byte files.** It opens with a 256-byte
   all-zero run and is 51.6% zero bytes with only 20 distinct values, where both
   *actual* DMG captures of the same length open `fb00fb00…`.
3. **Our run matches the DMG capture exactly, and our failures are precisely the
   DMG-vs-CGB capture delta [R]:**

```sh
$ D=$REPO/gb-test-roms/gbc-hw-tests/timers/tac_set_enabled
$ $PROBE $D/tac_set_enabled.gbc cgbe /tmp/tse_cgb.bin
$ python3 - <<'PY'
import os
D = os.environ['REPO'] + "/gb-test-roms/gbc-hw-tests/timers/tac_set_enabled/"
ours = open('/tmp/tse_cgb.bin','rb').read()
gbc  = open(D+'real_gbc.sav','rb').read()
gb   = open(D+'real_gb.sav','rb').read()
n = len(gbc)
f_gbc = [i for i in range(n) if ours[i] != gbc[i]]
f_gb  = [i for i in range(n) if ours[i] != gb[i]]
d     = [i for i in range(n) if gb[i]  != gbc[i]]
print("ours vs real_gbc (the graded ref): %d mismatches" % len(f_gbc))
print("ours vs real_gb  (DMG capture)   : %d mismatches" % len(f_gb))
print("real_gb vs real_gbc              : %d cells differ" % len(d))
print("our failures == the gb/gbc disagreement set:", set(f_gbc) == set(d))
PY
ours vs real_gbc (the graded ref): 574 mismatches
ours vs real_gb  (DMG capture)   : 0 mismatches
real_gb vs real_gbc              : 574 cells differ
our failures == the gb/gbc disagreement set: True
```

Two readings survive and the captures cannot separate them: **(a)** the file is
mislabeled/damaged, making the row un-gradeable; or **(b)** the file is genuine
and CGB really does differ from DMG at exactly those 574 cells, in which case we
are producing DMG behaviour on CGB and the row is a real gap. Fact 1 is
independent of our emulator and impugns the file on its own, which is why this is
filed **BLOCKED-ON-ORACLE**; but reading (b) is **not excluded**, and this
document does not claim our output is right. Unblock: re-capture on a CGB unit.

**AGB column (112 cells) — OPEN-TARGET.** 104 of 112 cells are `expected = ours +
1` **[R: `cells.py`]**, the same one-glitch-short signature as §3's residue. Almost
certainly the same rule one step out; not traced to specific `gbc_changes.txt`
rows, so the *mechanism* is asserted only at that confidence.

### 5. `dma/dma_valid_sources_dmg_mode` (DMG) — 1 row — ORACLE-CONFLICT (firm)

**This is not a modelling gap, and the previous revision's "single wrong decode
bit" framing understated it.** Two real-hardware oracles disagree about what
DMG `$E000+` OAM-DMA reads:

- **AntonioND** (`info.txt` + `real_gb.sav`): `$E000+` DMA reads **VRAM at
  `$8000`**.
- **mooneye** (`acceptance/oam_dma/sources-GS.gb`): `$E000+` DMA reads **echo
  WRAM** — and that ROM **passes** here **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh mooneye
PASS  mooneye              passed=193/193 (floor: passed>=193)
```

Our failure is uniform: all 108 mismatching cells have `expected = ours − 0x40`,
i.e. bit 6 set in ours and clear in the reference **[R: `cells.py`]** (`exp80/gotC0`,
`exp88/gotC8`, `exp90/gotD0`, …). Note this is a correction of detail: the
previous revision wrote "every cell `exp 0x80 got 0xC0`", which describes only 14
of the 108; the invariant is the uniform −0x40, not one byte pair.

**Implementing AntonioND's reading was tried on 2026-07-20 and reverted [V:
session, not reproducible here — the core is unmodified].** It regressed mooneye
193 → 183, wilbertpol 194 → 185, gbmicrotest 509 → 508 and gambatte 9 → 143
failures, for **0 net rows** (14 diffs remained on this ROM even after the
change). Trading ~30 passing rows across three independent suites for at most 1
is not a defensible move on a contested oracle.

**There is also positive reason to suspect this specific test [V: session,
PROVISIONAL].** Its result table lives at VRAM `$9000-$90FF`, and the `$F000`
fold aliases exactly that page — so under its own model the test reads back its
own partially-written output buffer. If that holds, the ROM is confounded and its
capture is not a clean statement about DMA source decoding. This was not
re-derived from the ROM here (upstream `.asm` is not in-tree), so it is
attributed, not asserted.

### 6. `lcd/mode3` — 2 rows — harness + OPEN-TARGET (firm)

See Trap 1 above: at the suite's 800-frame budget these rows report 647/646
mismatching cells, but **636 of them are unwritten `0xFF`**; at 4000 frames the
count is **11** on each column, with zero unwritten bytes **[R]**. The ROM simply
needs longer than the flat budget.

The suite already supports this: 12 rows carry a per-test `frames=` token, and
these two do not **[R]**:

```sh
$ grep -c 'frames=' rustyboi-test-runner/suites/gbc_hw_tests.manifest
12
$ grep -n 'mode3_stat_timing_spr_en_gbc_mode_8x16' rustyboi-test-runner/suites/gbc_hw_tests.manifest
322:...|cart=lazy_sram_cs|rev=cgbe          # no frames= token
323:...#agb|...|cart=lazy_sram_cs|rev=agb   # no frames= token
```

So the correct next step is a manifest `frames=` token (out of scope for this
file), after which the residual is an 11-cell OPEN-TARGET rather than a 647-cell
mystery. **The previous revision's classification of this family as "a value
error, unlike §1" was an artifact of the truncation and is withdrawn.**

### 7. `memory/oam_echo_ram_lcd_on` — 2 rows — OPEN-TARGET (firm)

Three of the four `oam_echo_ram_*` dirs (6 rows) now pass; only `lcd_on`
remains.

**The standing `cgb-acid-hell` conflict is RESOLVED — as a revision mismatch, not
a trade.** The previous revision declined to assert mutual exclusivity between
AntonioND's `$FEA0` captures and the `cgb-acid-hell` reference, correctly noting
it had not been verified. It is now settled, and the answer is that there was
never a trade to make: the two suites pin **different silicon**, so a
revision-parameterized decode satisfies both **[R]**:

```sh
$ grep -n 'acid-hell' rustyboi-test-runner/suites/cgb_acid_hell.manifest
2:cgb-acid-hell|cgb|png|...          # plain Hardware::CGB — NO rev= token
$ grep -n 'oam_echo_ram_read|' rustyboi-test-runner/suites/gbc_hw_tests.manifest
335:...|cart=lazy_sram_cs|rev=cgbe    # pinned CGB-E
```

`mmio: revision-parameterize the $FEA0-$FEFF OAM-high cell decode` (`0c4f5005`)
routes the region through `oam_high_index(lo, cgb, cgb_de)` gated on
`is_cgb_d_or_later() || is_agb()`, which satisfies the `rev=cgbe` captures
without touching plain-CGB behaviour. Both sides pass simultaneously **[R]**:

```sh
$ RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 tools/run-suites.sh cgb_acid_hell
PASS  cgb_acid_hell        passed=1/1 (floor: passed>=1)
```

That commit took the suite 268 → 274 (6 rows).

**What remains is an OAM-lock window error, not a decode error.** The residual
cells cluster in two runs of three inside result blocks 2 and 3, at a uniform
stride of 0x13, and the reference is `0x00` (hardware **blocks** the read) where
we return the ROM's fill pattern **[R: `cells.py`]**:

```sh
$ B=$REPO/gb-test-roms/gbc-hw-tests/memory/oam_echo_ram_lcd_on
$ $PROBE $B/oam_echo_ram_lcd_on.gbc cgbe /tmp/o.bin && python3 /tmp/cells.py /tmp/o.bin $B/real_gbc.sav 12
graded=1028 mismatches=12
deltas(exp-ours): {-170: 6, -85: 2, -90: 2, -165: 2}
    off=0x0210 (mod 0x100 = 0x10) exp=0x00 ours=0xAA
    off=0x0223 (mod 0x100 = 0x23) exp=0x00 ours=0x55
    off=0x0236 (mod 0x100 = 0x36) exp=0x00 ours=0x5A
    off=0x0290 (mod 0x100 = 0x90) exp=0x00 ours=0xAA
    off=0x02A3 (mod 0x100 = 0xA3) exp=0x00 ours=0xAA
    off=0x02B6 (mod 0x100 = 0xB6) exp=0x00 ours=0xA5
    off=0x0310 (mod 0x100 = 0x10) exp=0x00 ours=0xA5
    off=0x0323 (mod 0x100 = 0x23) exp=0x00 ours=0x5A
    off=0x0336 (mod 0x100 = 0x36) exp=0x00 ours=0x55
    off=0x0390 (mod 0x100 = 0x90) exp=0x00 ours=0xAA
    off=0x03A3 (mod 0x100 = 0xA3) exp=0x00 ours=0xAA
    off=0x03B6 (mod 0x100 = 0xB6) exp=0x00 ours=0xAA

$ $PROBE $B/oam_echo_ram_lcd_on.gbc agb /tmp/o2.bin && python3 /tmp/cells.py /tmp/o2.bin $B/real_gba.sav 4
graded=1028 mismatches=4
deltas(exp-ours): {-170: 3, -165: 1}
    off=0x0210 (mod 0x100 = 0x10) exp=0x00 ours=0xAA
    off=0x0290 (mod 0x100 = 0x90) exp=0x00 ours=0xAA
    off=0x0310 (mod 0x100 = 0x10) exp=0x00 ours=0xA5
    off=0x0390 (mod 0x100 = 0x90) exp=0x00 ours=0xAA
```

We let a read through where silicon has already asserted the mode-2/3 OAM lock,
at the LCD-on turn-on boundary. `ppu: gate the OAM-high cells $FEA0-$FEFF behind
the mode-2/3 OAM lock on CGB` (`133028fa`) implemented that lock and was
row-neutral, so the lock exists but its *assertion window* is a few dots late at
these probe positions. AGB fails only 4 of the 12, so the AGB path is already
partly correct.

**Pairs with §8 in the opposite direction** — there we block a read that silicon
allows. Both are OAM-lock window-boundary errors and are plausibly one fix.

### 8. `lcd/mode2` — 2 rows — OPEN-TARGET (firm)

4 cells per row, a single contiguous run at `0x32-0x35`, `expected 0xFF, got
0x00` **[R: `cells.py`]** — we report OAM as locked where silicon still reads it. See
Trap 2: the displacement predicate scores this 0%, but it is one clean 4-cell
edge. Direction is the mirror of §7.

### 9. `dma/hdma_timing_fine` — 2 rows — OPEN-TARGET (firm)

32 cells per row, **every one** `expected = ours + 2`, at every odd offset
`0x01, 0x03, 0x05, …` **[R: `cells.py`]** — the delta histogram is a single
bucket, `{2: 32}`:

```sh
$ B=$REPO/gb-test-roms/gbc-hw-tests/dma/hdma_timing_fine
$ $PROBE $B/hdma_timing_fine.gbc cgbe /tmp/ht.bin && python3 /tmp/cells.py /tmp/ht.bin $B/real_gbc.sav 4
graded=68 mismatches=32
deltas(exp-ours): {2: 32}
    off=0x0001 (mod 0x100 = 0x01) exp=0xB9 ours=0xB7
    off=0x0003 (mod 0x100 = 0x03) exp=0xB9 ours=0xB7
    off=0x0005 (mod 0x100 = 0x05) exp=0xB9 ours=0xB7
    off=0x0007 (mod 0x100 = 0x07) exp=0xB9 ours=0xB7
```

A uniform 2-unit HDMA timing offset — not a displaced edge, and not
budget-sensitive (§ Trap 1 table). The uniformity makes this the most tractable
open item in the section.

### 10. `dma/dma_timing_lcd_on` — 2 rows — OPEN-TARGET (**provisional**)

45 cells per row. The signature is a *paired* displacement: at one offset the
reference holds a value and we hold `0xFF`, and some cells later the reference
holds `0xFF` and we hold that value **[R: `cells.py`]**:

```sh
$ B=$REPO/gb-test-roms/gbc-hw-tests/dma/dma_timing_lcd_on
$ $PROBE $B/dma_timing_lcd_on.gbc cgbe /tmp/dt.bin && python3 /tmp/cells.py /tmp/dt.bin $B/real_gbc.sav 8
graded=2564 mismatches=45
deltas(exp-ours): {-252: 1, 247: 1, -206: 1, 190: 1}
    off=0x0003 (mod 0x100 = 0x03) exp=0x03 ours=0xFF     # marker missing here ...
    off=0x0008 (mod 0x100 = 0x08) exp=0xFF ours=0x08     # ... and present 5 later
    off=0x0031 (mod 0x100 = 0x31) exp=0x31 ours=0xFF
    off=0x0041 (mod 0x100 = 0x41) exp=0xFF ours=0x41     # +16
    off=0x006A (mod 0x100 = 0x6A) exp=0x6A ours=0xFF
    off=0x007A (mod 0x100 = 0x7A) exp=0xFF ours=0x7A     # +16
    off=0x011A (mod 0x100 = 0x1A) exp=0x1A ours=0xFF
    off=0x012A (mod 0x100 = 0x2A) exp=0xFF ours=0x2A     # +16
```

i.e. the DMA writes its marker at the wrong sweep index — mostly 16 steps late,
but the first pair is 5. That non-constant stride is why the naive
`ours[i]==ref[i-1]` predicate scored only 51% here, and it is also why this stays
**provisional**: a single displaced write index would give a constant stride, so
either the sweep rows are not uniformly sized or more than one effect is present.
Not established.

### 11. `serial/sc_change_freq_gbc#agb` — 1 row — OPEN-TARGET (**provisional**)

The CGB column now passes (`serial: carry the banked half-period across a
mid-transfer clock change`, plus a grading-window fix); the AGB column remains,
1895 cells. Deltas are dominated by ±1 (`+1` ×933, `−1` ×473) with a secondary
±64-ish cluster (`−63` ×252, `+65` ×237) **[R: `cells.py`]** — consistent with a
half-period boundary landing one shift-step off, but the mechanism is not
established. Note this row grades against an in-tree emitted prefix
(`rustyboi-test-runner/suites/refs/gbc-hw-tests/serial/sc_change_freq_gbc.gbasp.sav`)
because the upstream capture is a raw 128K card dump; see the manifest header's
"Grading window" note for why that is a trimmed real capture and never an
emulator output.
---

## Floor arithmetic

- **gbmicrotest:** 509/513 is the maximum for any register-level emulator without inventing oracles. +2 (`500-scx-timing`, `minimal`) become gradeable the day a hardware capture of the absolute byte exists; `temp` is capturable in principle; `halt_op_dupe_delay` requires characterizing analog die physics.
- **gambatte:** 7 (residue tail) + 1 (fexx) + 1 (C113) = **9 is the permanent minimum for any deterministic emulator, including a perfect gate-level one** — every failing byte is pinned oppositely by a *currently-passing* capture of the same physical quantity, and the exhaustive subset search confirms the current choices are globally optimal.
- **gbc_hw_tests:** 284/343 is a ratcheted progress floor and, unlike the two suites above, **nothing like a proven ceiling**. Sorting the 59 by what actually blocks them:
  - **5 rows are reachable without any new hardware or oracle work.** `lcd/mode3` (2) needs a manifest `frames=` token — 636 of its 647 "failures" are unwritten bytes (§6). `dma/hdma_halt` (2) needs `skip=` extended to `0x9`, on exactly the rationale that already justified skipping `0x3-0x6` (§2). Neither is an emulator change; both are manifest edits, out of scope for this file. `dma/hdma_timing_fine` is a uniform +2 (§9) and is the cleanest genuine core fix on the list.
  - **~1720 cells across 3 rows are ORACLE-CONFLICT** — `tac_set_disabled#agb` (1145 of its 1148 cells are cells where the two physical AGB units contradict each other, §3), `tac_set_enabled#gbc` (574 cells against a structurally defective capture, §4), `dma_valid_sources_dmg_mode` (108 cells where AntonioND and mooneye disagree and mooneye currently passes, §5). No emulator change resolves these; they need provenance, not code.
  - **The remaining 42 (§1) are one family with a now-correctly-characterized error.** The previous revision's "uniform 4-dot late latch" reading is **refuted** (§1): the error is 5297 of 3,724,768 graded cells — 0.14% — confined to narrow window boundaries, and every uniform correction tried so far made things worse. This is real accuracy work, not a one-line phase fix.
- 4 + 9 + 59 = 72: every gbmicrotest and gambatte failure is accounted for byte by byte, and every gbc_hw_tests failure carries a family, a verdict class and a confidence label — with **56 of its 59 rows firmly adjudicated** (families 1–9) and only 3 (`dma_timing_lcd_on` ×2, `sc_change_freq_gbc#agb` ×1) resting on signature alone and marked provisional.

**Two corrections that this revision makes to its own predecessor**, recorded because both were stated as settled and both were wrong:

1. §1's global one-M-cycle-displacement claim was an artifact of conditioning the statistic on mismatching cells; the unconditioned test refutes it (5297 → 52573 under the shift).
2. §3's "the units' disagreement is entirely orthogonal to our error — OPEN-TARGET, not LOGIC-IMPOSSIBLE" was true of the pre-fix model and **inverted** once the TAC glitch landed: 1145 of 1148 residual cells now sit inside that disagreement.

Both survived a full write-up because a plausible mechanism was fitted to a statistic that could not discriminate. The two measurement traps documented at the top of the gbc_hw_tests section are the generalization.

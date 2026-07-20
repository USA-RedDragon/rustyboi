# Known Failures — every failing ROM, with proof

86 of 6670 test cases fail across the 28 suites: 4 in gbmicrotest, 9 in gambatte, 73 in gbc_hw_tests. The 13 gbmicrotest + gambatte failures are each adjudicated below, with evidence. The 73 gbc_hw_tests failures are newly adopted and **pending per-ROM adjudication** — that section states plainly what has and has not been proven, rather than pretending. Adjudicated entries make **no assumptions**: every claim is tagged with its provenance, and reproducible claims include the command that re-verifies them against this tree.

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

## gbc_hw_tests — 73 failures (120/193) — PENDING ADJUDICATION

Adopted in bd0ad76e from AntonioND/gbc-hw-tests (real-device SRAM captures;
see SUITES.md for grading provenance and the revision caveat). **These 73
failures have not yet been adjudicated per-ROM** — no entry to the standard of
the sections above exists for any of them, and this document does not pretend
otherwise. SUITES.md's promise that per-ROM adjudication lives here is, for
this suite, an open work item.

What is known only at the suite level [D: SUITES.md]: the captures come from
one unit per class and the CGB unit's silicon revision is undocumented, so
rev-sensitive families (speed-switch sub-timing, STOP sub-dot, mode-2/3 LCD
timing) may encode revision differences against the modeled CGB-04 rather
than emulator bugs — but no individual failure has been classified as such.
Until each ROM gets a verdict class with evidence, every one of the 73 counts
as an open accuracy work item. The 120/193 floor is a ratcheted progress
floor, not a proven ceiling.

---

## Resolved — little_things_extra (now 4/4)

Adopted 2026-07 from the nitro2k01/little-things-gb releases (see SUITES.md
for fetch + grading provenance) at 0/4, with both underlying behaviors
classified OPEN-TARGET — genuine accuracy gaps, not excused floors. Both are
now fixed and the suite floor is ratcheted to 4/4; the entries are kept here
as the resolution record.

### 14. `windesync-validate.gb` (dmg) — RESOLVED

Pre-CGB window-desync glitch: after the window triggers once in a frame and is
disabled via LCDCF_WINON, every later WX hit with `(WX&7)==7-(SCX&7)` emits one
BGP-color-0 glitch pixel and shifts the rest of the line right by one (oracle:
nitro2k01's logic-analyzer capture from a real Super Game Boy). Fixed by
0ab79f24 (`ppu: complete the DMG WE-off window-desync insert glitch`, the
SameBoy #278 behavior); floor ratcheted 3→4 in 1002131b.

### 15–17. `double-halt-cancel.gb` (dmg+cgb) + `double-halt-cancel-gbconly.gb` (cgb) — RESOLVED

Double `halt` with IME=0 is not a lockup: PC-increment inhibition makes the
CPU refetch the second `halt` byte forever, so mode-3 VRAM locking turns the
fetch into `$FF` (`rst $38`) and execution escapes. Fixed by 24398b29
(`cpu: HALT prefetch is a real bus read` — halted wake modeled as a continuous
refetch of the post-HALT byte, sensitive to VRAM lock state), flipping all
three rows; floor ratcheted 0→3 in f11ac25e.

---

## Floor arithmetic

- **gbmicrotest:** 509/513 is the maximum for any register-level emulator without inventing oracles. +2 (`500-scx-timing`, `minimal`) become gradeable the day a hardware capture of the absolute byte exists; `temp` is capturable in principle; `halt_op_dupe_delay` requires characterizing analog die physics.
- **gambatte:** 7 (residue tail) + 1 (fexx) + 1 (C113) = **9 is the permanent minimum for any deterministic emulator, including a perfect gate-level one** — every failing byte is pinned oppositely by a *currently-passing* capture of the same physical quantity, and the exhaustive subset search confirms the current choices are globally optimal.
- **gbc_hw_tests:** 120/193 is a ratcheted progress floor, **not** an adjudicated ceiling — the 73 failures are open work items pending per-ROM adjudication (see that section).
- 4 + 9 + 73 = 86: every gbmicrotest and gambatte failure is accounted for above, byte by byte; the gbc_hw_tests failures are counted honestly but not yet accounted for individually.

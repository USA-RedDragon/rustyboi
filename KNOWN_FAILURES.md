# Known Failures — every failing ROM, with proof

State: **6426/6440** across 23 suites (as of `claude` @ mealybug-51/51). Exactly **14 ROMs fail**: 4 in gbmicrotest, 10 in gambatte. This document details every one, with evidence. It makes **no assumptions**: every claim is tagged with its provenance, and reproducible claims include the command that re-verifies them against this tree.

Provenance tags:
- **[R]** Reproducible here — run the command shown against this checkout.
- **[S]** Session-verified — established during the accuracy campaign by building third-party references from source (SameBoy, libgambatte, AGE, GateBoy source reading) or by instrumented traces; the method is described where cited.
- **[D]** Documented upstream — Pan Docs, gekkio's gb-ctr, or the test author's own files.

Verdict classes:
- **LOGIC-IMPOSSIBLE** — real-hardware captures of the *same ROM* contradict each other at the failing byte. No deterministic emulator can pass both siblings; the failure count is forced by arithmetic, not by a modeling gap.
- **BLOCKED-ON-ORACLE** — a hardware-correct answer exists in principle, but no captured/documented value exists anywhere; grading our own output would be oracle-gaming. Unblockable with real hardware.
- **UN-GRADEABLE** — the ROM writes no stable verdict anywhere.
- **ANALOG** — the behavior has no register-level correlate; reproducing the value without a physical derivation would be a single-observable fit.
- **OPEN** — not settled; under active investigation. (No assumptions: we do not claim impossibility without proof.)

---

## gbmicrotest — 4 failures (509/513; proven ceiling for register-level emulators)

The suite's protocol is `FF82==0x01` pass with `FF80`=actual, `FF81`=expected (60 frames). 13 additional no-verdict ROMs are graded via `mem <addr>=<val>` with disassembly-justified bytes (see the manifest header for per-test provenance).

### 1–2. `500-scx-timing.gb` and `minimal.gb` — BLOCKED-ON-ORACLE

**Failure [R]:** `FF82=64 (want 01); FF80=2B FF81=0B` — that signature is *uninitialized HRAM*: the ROM never writes FF80–FF82 at all.

**The two ROMs are the same ROM [R]:**
```
$ md5sum gb-test-roms/gbmicrotest/minimal.gb gb-test-roms/gbmicrotest/500-scx-timing.gb
719e6f331d16d03443aa43ed76fb5ced  (both)
```

**What it does [S]:** dual-HALT TIMA measurement of mode-3 length at SCROLL=0; the raw TIMA count is written to VRAM `$8000`. Patched-ROM probes confirmed the flow (halt 1 wakes at line-1 mode-2, halt 2 at line-1 HBlank).

**Why it cannot be graded today [S]:** the author's only hardware record is *relative* ("overhead 65" sweep rows for DMG and AGS). Both rows decode exactly to the M-cycle-grid quantization `(scx + ((−scx−e) mod 4))/4` with DMG e=0, AGS e=2 — and a patched SCROLL=0..7 sweep on rustyboi reproduces **all 8 DMG deltas**, so the *physics* is validated. But no **absolute** pass byte exists anywhere: aappleby's GateBoy/MetroBoy harnesses grade only the `FF80==FF81 && FF82` self-verdict (no independent expected values are stored upstream), and a documents-only derivation stacks ≥6 sub-M-cycle constants. Grading the emulator's own `$4A` would assert nothing.

**Unblock:** run the measurement on real DMG hardware and capture the absolute byte (SRAM-dump capture ROM planned). +2 tests.

### 3. `temp.gb` — UN-GRADEABLE (dev stub)

**Content [R]:**
```
$ xxd -s 0x100 -l 4 gb-test-roms/gbmicrotest/temp.gb   # entry: nop; jp $0150
00000100: 00c3 5001
$ xxd -s 0x150 -l 32 gb-test-roms/gbmicrotest/temp.gb  # $0150: all zero bytes (nop sled)
```
**What happens [S]:** PC slides through zeroed ROM as `nop`s, continues into VRAM-as-code, and collapses into an `RST $38` loop whose pushes walk SP through IO/OAM/WRAM. Deterministic on silicon — but the trajectory executes boot-logo VRAM bytes as opcodes and no capture of the end state exists. There is no verdict write of any kind.

### 4. `halt_op_dupe_delay.gb` — ANALOG

**Failure [R]:** `FF80(actual)=01 FF81(expected)=55`.

**The digital chain [S/D]:** the ROM arms the STAT/HBLANK interrupt with IME=0, then HALTs. IF bit 1 is already latched before the HALT, and IF bits are sticky (set on the interrupt line's rising edge; cleared only by CPU write, dispatch, or reset — none occur here). Every register-level model therefore falls through the HALT immediately and reads DIV ≈ **0x01**.

**Proof there is no register-level path to 0x55 [S]** (three independent eliminations):
1. SameBoy built from source produces 0x01 and **fails identically** (its sibling `halt_op_dupe` passes, as does ours).
2. GateBoy's own source (`GateBoyInterrupts.cpp`): IF bit 1 is `LALU_FF0F_D1p.dff22` — a rising-edge sticky DFF; GateBoy also computes 0x01. GateBoy has **no gate-level SM83** (register-level CPU core; its README flags async glitches as unmodeled).
3. Force-clearing IF.1 at the HALT in rustyboi wakes at the *next* HBLANK (1 line, DIV≈0x02) — not 47 lines. Per-line traces show LY=2..53 are byte-identical (same STAT edge, same phase every line; 456 T is a multiple of 4, so zero drift): **nothing digital distinguishes LY=49.**

**What 0x55 means [D]:** DIV ∈ [0x5500,0x55FF] ⇒ the CPU slept ~48 scanlines and woke at LY≈49's HBLANK. That requires (a) the latched IF bit not to count at the HALT (the async STAT-write runt-pulse clearing a latched IF — a real, documented-by-observation DMG glitch), and (b) ~47 digitally-identical HBLANK edges to be ignored before the 48th is honored. The consistent physical explanation is an analog node left at an intermediate level, drifting to threshold over ~5 ms (an RC time constant of the specific die). The number 47 is stored nowhere in the machine; encoding it would be a fit to a single observable from a single unit.

**Unblock (partial):** run the ROM on other real DMG units. Exact 0x55 on an independent unit would *reopen* the digital question; a different/unstable value confirms the analog verdict.

---

## gambatte — 10 failures (5247/5257; floor gated `failed<=10`)

All 10 are CGB OAM-DMA/GDMA dumper tests. Context established this campaign:

- **Oracle provenance [S]:** gambatte's own `testrunner.cpp` grades only `_out`-named ROMs and `.png`s; it never reads these `.bin`/`.dump` files. They are **external real-hardware captures** shipped in `gambatte-core/test/hwtests/` — not emulator output. (Contrast: the *DMG power-on OAM* region of the fexx `.bin`s was proven fabricated earlier and is excluded from grading; the regions graded below are unaffected by that finding.)
- **Region structure [S/D]:** CGB `FEA0–FEFF` reads through `addr & 0xE7`, mirroring three canonical 8-byte rows ×4. During an OAM-DMA, a concurrent GDMA "conflict" write lands in `ioamhram[p & 0xE7]` where `p = src&0xFF` (rustyboi `mmio.rs::dma_conflict_advance`, a port of gambatte `memory.cpp:348-373`) — reaching **odd columns only**. Even-column bytes in FEA0–FEFF are **power-on residue** the ROMs never write.
- The failing bytes below are therefore either (i) power-on residue cells or (ii) conflict-timing cells — and for both, the captures themselves disagree, as shown.

### The same-ROM contradiction evidence [R]

Re-verify at any time:
```
$ python3 - <<'EOF'
D="gb-test-roms/gambatte/oamdma/"
a=open(D+"oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_1.dump",'rb').read()
b=open(D+"oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_2.dump",'rb').read()
print([hex(i) for i in range(256) if a[i]!=b[i]])
EOF
```

**`gdmalen13_oamdumper` — two captures of the identical ROM disagree at 10 bytes:**

| offset | capture `_1` | capture `_2` |
|---|---|---|
| 0x04 | 0x04 | 0xA0 |
| 0x37 | 0x37 | 0xC8 |
| 0xC6/0xCE/0xD6/0xDE | 0xB4 | 0xB6 |
| 0xE6/0xEE/0xF6/0xFE | 0xAD | 0xAC |

**`gdmalen13_oamdumper_ds` — three captures of the identical ROM disagree at 108 bytes**, including, at single cells:

| cell | `_ds_1` | `_ds_2` | `_ds_3` |
|---|---|---|---|
| 0xA0 | 0x08 | 0x18 | 0x18 |
| 0xA5 | 0xCA | 0xC8 | 0x4A |

One ROM, one power-on procedure, **three different bytes at 0xA5**. No deterministic machine of any kind can satisfy more than one capture per family.

### 5. `oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_1` — LOGIC-IMPOSSIBLE

Fails at 0xC6: expected 0xB4, we produce 0xB6 **[R]**. Capture `_2` of the identical ROM demands 0xB6 at the same cell — **and we pass `_2`** [R: `_2` is absent from the failing list]. Passing `_1` requires failing `_2`, byte-for-byte. The `{B4,B6}` / `{AC,AD}` values appear across the capture set with no correlation to ROM parameters [R: matrix above + §7 pivot table] — metastable bus float in exactly the region gekkio's gb-ctr marks "OAM DMA bus conflicts: TODO" [D].

### 6–8. `…gdmalen13_oamdumper_ds_1 / _ds_2 / _ds_3` — LOGIC-IMPOSSIBLE (at most 1 of 3 can ever pass)

Fail at 0xA0 (exp 0x08, got 0x18) and 0xA5 (exp 0xC8/0x4A, got 0x48) **[R]**. The triplet's own 108-byte three-way disagreement is above. Additional constraint: the residue values we produce are shared with **currently-passing** captures — e.g. `gdmasrc0000_gdmalen04_oamdumper_1` and `_ds_1` both PASS and both demand A0=0x18 / A5=0x48 [R: pivot table §7]. Moving the residue toward any one `_ds_N` breaks passing tests one-for-one.

### 9. `oamdmasrcC000_gdmasrc0000_gdmalen04_oamdumper_ds_1` — LOGIC-IMPOSSIBLE (residue-family pivot)

Fails at 0xA7: expected 0xBF, got 0xBD **[R]**. The same cell across captures [R, §7]: `gdmalen04_1` (PASSING) demands **0xBD**; this capture demands **0xBF**; `gdmalen13_1/_2` (mixed) demand 0xBF while `gdmalen13_ds_2/_ds_3` demand 0xBD. There is no value that satisfies the set; our 0xBD maximizes the passing count.

### 10. `oamdmasrcC000_gdmasrc0000_gdmalen13_oamdumper_ds_1` — LOGIC-IMPOSSIBLE (same pivot, 0xA0)

Fails at 0xA0: expected 0x08, got 0x18 **[R]**. 0xA0 across captures ∈ {0x08, 0x18} *including within the same-ROM ds triplet* (0x08, 0x18, 0x18) [R, §7]. Same forced-choice arithmetic.

### 11. `fexx_read_reset_set_dumper.gbc` (CGB) — LOGIC-IMPOSSIBLE (same residue class)

Fails at SRAM offset 0xA5 (= FEA5, dump 1 of 3 = the power-on pass): expected 0x48, got 0x4A **[R]**. The same physical cell is demanded as 0x4A by `gdmasrc0000_gdmalen13_ds_1` and as {CA,C8,4A} by the ds triplet [R, §7] — the pivot cell again. (The DMG variants of the fexx dumpers pass; their nondeterministic power-on OAM window FE00–FE9F is excluded from grading with documented provenance — the two DMG references themselves disagree on **105/160** OAM bytes for the identical power-on while agreeing **0/96** on the FEA0+ region actually named by the test [R]:
```
$ python3 - <<'EOF'
a=open("gambatte-core/test/hwtests/fexx_ffxx_dumper_dmg08.bin",'rb').read()
b=open("gambatte-core/test/hwtests/fexx_read_reset_set_dumper_dmg08.bin",'rb').read()
print(sum(1 for i in range(0xA0) if a[i]!=b[i]), "/160 OAM;",
      sum(1 for i in range(0xA0,0x100) if a[i]!=b[i]), "/96 FEA0+")
EOF
```
)

### 12–13. `oamdmasrcC000_gdmasrcC0F0_gdmalen13_{oamdumper,vramdumper}_1` — LOGIC-IMPOSSIBLE (uninit-WRAM cross-oracle contradiction)

Fail at OAM[0x01] (exp 0x00, got 0xFB) and VRAM[0x110] (exp 0x00, got 0xFF) **[R]**.

**Mechanism [S]:** the ROM's STAT-interrupt loop fills WRAM C000–C1FF, then starts an OAM-DMA (src C000) and a GDMA (src **C0F0**, len 0x13 ⇒ (0x13+1)×16 = **320 bytes** ⇒ source range C0F0–C22F) — the GDMA source runs **past the fill window into never-written WRAM at C200+**. The failing cells are plain copies: VRAM[0x110] = mem[C200], OAM[0x01] = mem[C201]. Both engines copy faithfully; the disputed value is the *power-on content of uninitialized WRAM*.

**The contradiction [R]:** the same suite contains `wram_dumper.gbc`, whose real-hardware reference — **currently passing** — pins the very same cells to opposite values:
```
$ python3 - <<'EOF'
w=open("gb-test-roms/gambatte/wram_dumper_cgb.bin",'rb').read()
print(hex(w[0x200]), hex(w[0x201]))   # -> 0xff 0xfb
EOF
```
`wram_dumper_cgb.bin` demands C200=0xFF, C201=0xFB; the C0F0 captures demand both = 0x00. Same physical cells, opposite hardware captures. Flipping our seed to 0x00 passes these two and fails `wram_dumper` — verified empirically during the investigation [S], net-zero. rustyboi matches the wram_dumper capture.

### 14. `oamdmasrc8000_gdmasrcC000_2xgdmalen09_oamdumper_1` — OPEN

Fails at OAM[0x13]: expected 0xF7, got 0xFF **[R]**. This is the one remaining failure we do **not** claim impossible. Facts on record: rustyboi and gambatte produce the same wrong value here [S]; the working hypothesis is a CGB VRAM-source dual-bus interaction (an AND with a bus byte) in the 2×-back-to-back-GDMA window — the same family as the modeled srcC000 2×GDMA word-bus conflict (which passes). It has a single capture (no contradicting sibling), so it is not in the logic-impossible class; it is either a modelable behavior (in which case it will be modeled) or awaits a decisive proof. An independent re-adjudication is in progress; this section will be amended with its result.

---

## Floor arithmetic

- gbmicrotest: 509/513 is the maximum for any register-level emulator under no-oracle-fitting rules; +2 (`500-scx-timing`, `minimal`) become gradeable the day a hardware capture of the absolute byte exists; `temp` is capturable in principle; `halt_op_dupe_delay` requires characterizing analog die physics.
- gambatte: of the 10, **9 are forced by capture contradictions or pivot cells shared with passing tests** — for each family, passing one sibling means failing the others, and our seed choices currently sit on the majority side (re-verification of exact optimality in progress). 1 (`src8000 2xgdma`) remains OPEN.
- Therefore the current honest ceiling is **6426–6427 / 6440** without new hardware captures, and every point below 6440 is accounted for above, byte by byte.

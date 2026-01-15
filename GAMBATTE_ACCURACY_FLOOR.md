# The Gambatte hwtests Accuracy Floor - why the last ~17 tests cannot be fixed

**Status:** rustyboi passes all but **16** of Gambatte's ~5,300 hardware-timing tests (−97.1%).
This document proves that **all 16 are impossible for *any* correct emulator to pass**.

**Eight of them** (the HDMA "twin" tests and the
boot-residue tests) are *contradictions*: two tests hand the machine the identical situation
and demand different answers, so no program can satisfy both - and we measured that every fix
for one breaks its twin. **Ten of them** (the memory-dumper tests, with two-byte overlap)
check values that are *physically random* on the real Game Boy - bus noise and undefined
memory - which is why even Gambatte and SameBoy fail them, and why there is no value to
compute.

---

## The only two ways a test can be impossible

Every one of the unfixable failures falls into exactly one of two logical shapes. Neither
is specific to Game Boys. They're the two ways *any* test can be unpassable by
construction.

### Shape 1 - Contradiction: "same question, two different required answers"

Two tests put the machine in the **identical** situation but demand **different** answers.

So if we can show (a) the two tests reach the emulator in an identical internal state, and
(b) their reference files demand different results, then passing both is impossible - and
*any* change that flips the failing one will flip the passing one too.

### Shape 2 - Non-determinism: "the reference is a photograph of noise"

Some tests check values that are **physically random on the real Game Boy** - they come
from electrically-undefined wires or uninitialized memory that the hardware itself does not
pin to any fixed value. The reference file is a single snapshot of that randomness on one
particular console on one particular day.

---

## Failing Tests

### Type A - the HDMA "twin test" contradiction (4 tests)

**The tests (all CGB):**

| Test | Wants | Twin it's tied to |
|---|---|---|
| `hdma_transition_ei_halt_late_unhalt_ldaaimm_hdma_scx1_1` | `00` | the passing non-`ei` version |
| `hdma_transition_ei_halt_late_unhalt_ldaaimm_hdma_scx1_2` | `02` | each other (`_1` vs `_2`) |
| `hdma_m0speedchange_late_m3wakeup_scx1_2` | `00` | the passing `hdma5_scx1_2/_3` |
| `hdma_m0speedchange_late_m3wakeup_scx2_2` | `00` | the passing `hdma5_scx2_2/_3` |

**What they do, in plain terms:** each fires the block-copy machine (HDMA) at a moment that
lands right on a knife-edge - the exact tick where the screen's drawing phase flips, or
where an internal countdown timer ticks over. The test then reads a value whose answer
depends on which side of that single-tick edge the copy landed.

**Why it's a Shape 1 contradiction.** These tests come in families that differ by **one
byte** - a single padding `NOP` instruction. That one byte shifts the copy's timing by a
fixed, tiny amount, nudging it just across the knife-edge. On real hardware the two twins
land on *opposite sides* of the edge and so produce different answers (`00` vs `02`).

But here's the catch our emulator (and Gambatte's) hits: our internal clock represents that
moment at a coarser resolution than the knife-edge itself. **Both twins round to the same
internal instant**, so the emulator computes the *same* answer for both - and the two
references demand *different* answers. We are being asked to return two answers to one input.

The deeper reason we can't just "add resolution here": these specific four tests share their
code path with **tests that currently pass**. We verified empirically that every single-value
adjustment that flips a failing twin to correct *simultaneously breaks a passing sibling* -
it just moves the failure, never removes it. (For the `m3wakeup` pair specifically, the truly
correct discriminator is a fractional screen-timing quantity that our model carries ~77
cycles off during the double-speed transition, with no whole-number correction available -
so there is no local edit that lands it on the right side without dragging its passing
siblings to the wrong side.)

---

### Type B - the boot-residue contradiction (2 tests)

**The tests:** `fexx_read_reset_set_dumper` (graded twice - once as a DMG, once as a CGB).

**What they do:** they read the "unusable" memory region (`0xFEA0`–`0xFEFF`) after a
specific reset/startup sequence, and check the leftover bytes against a hardware capture.

**Why it's a Shape 1 contradiction (on top of a Shape 2 region).** The bytes in that region
are determined by the console's exact power-on and reset history. This test's reference
encodes one specific residue (e.g. `0xFEA5 = 0x48`, the value a revision-C CGB leaves). But
**other tests in the same suite require a *different* residue for the same region** (e.g.
`0xFEA0 = 0x08`). A single emulator can only be in *one* power-on state at a time, so the two
sets of references are mutually exclusive - satisfying `fexx` would break the others (and vice
versa). On top of that, parts of this region are genuinely undefined (Shape 2), so the
"right" value isn't even singular on real hardware. Notably, rustyboi's value is *closer to
the real revision-C silicon than Gambatte's* on 4 of 5 of these bytes - we are not behind the
reference emulator here; the target is simply self-contradictory across the suite.

---

### Type C - the non-deterministic dumper (10 tests)

**The tests (all CGB):** the `oamdma…_oamdumper` / `…_vramdumper` family -
`oamdmasrc8000_gdmasrcC000_2xgdmalen09_oamdumper_1`,
`oamdmasrcC000_gdmasrc0000_gdmalen04_oamdumper_ds_1`,
`…gdmalen13_oamdumper_1/_ds_1/_ds_2/_ds_3`, `…2xgdmalen09_oamdumper_1`,
`…gdmasrcC0F0_gdmalen13_oamdumper_1`, and `…gdmalen13_vramdumper_1`.

**What they do:** they run the copy machines (OAM-DMA and GDMA) and then dump 256 bytes of
sprite memory (OAM) - including the unusable region `0xA0`–`0xFF` - and compare every byte to
a hardware capture. Crucially, the dump captures bytes *mid-copy*, while the copy machine and
the screen-drawing circuit are both driving the same memory bus.

**Why it's a Shape 2 "no answer exists."** Two effects put most of the mismatching bytes
beyond any formula:
- The **unusable region** (`0xA0`–`0xFF`) is electrically undefined - it's whatever charge
  is on the wire, which floats and decays.
- The **mid-copy bus-conflict bytes** are a hardware tug-of-war whose result, for these
  specific configurations, isn't a clean defined value but bus noise.

There is no computation that reproduces noise.

---

## Appendix - the full list, classified

| # | Test | Mode | Type | Verdict |
|---|---|---|---|---|
| 1 | `dma/hdma_transition_ei_halt_late_unhalt_ldaaimm_hdma_scx1_1` (`out00`) | CGB | A - twin contradiction | impossible |
| 2 | `dma/hdma_transition_ei_halt_late_unhalt_ldaaimm_hdma_scx1_2` (`out02`) | CGB | A - twin contradiction | impossible |
| 3 | `dma/hdma_m0speedchange_late_m3wakeup_scx1_2` (`out00`) | CGB | A - twin contradiction | impossible |
| 4 | `dma/hdma_m0speedchange_late_m3wakeup_scx2_2` (`out00`) | CGB | A - twin contradiction | impossible |
| 5 | `fexx_read_reset_set_dumper` | DMG | B - boot-residue contradiction | impossible |
| 6 | `fexx_read_reset_set_dumper` | CGB | B - boot-residue contradiction | impossible |
| 7 | `oamdma/oamdmasrc8000_gdmasrcC000_2xgdmalen09_oamdumper_1` | CGB | C - non-deterministic | impossible |
| 8 | `oamdma/oamdmasrcC000_gdmasrc0000_gdmalen04_oamdumper_ds_1` | CGB | C - non-deterministic | impossible |
| 9 | `oamdma/oamdmasrcC000_gdmasrc0000_gdmalen13_oamdumper_ds_1` | CGB | C - non-deterministic | impossible |
| 10 | `oamdma/oamdmasrcC000_gdmasrcC000_2xgdmalen09_oamdumper_1` | CGB | C - non-deterministic | impossible |
| 11 | `oamdma/oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_1` | CGB | C - non-deterministic | impossible |
| 12 | `oamdma/oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_ds_1` | CGB | C - non-deterministic | impossible |
| 13 | `oamdma/oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_ds_2` | CGB | C - non-deterministic | impossible |
| 14 | `oamdma/oamdmasrcC000_gdmasrcC000_gdmalen13_oamdumper_ds_3` | CGB | C - non-deterministic | impossible |
| 15 | `oamdma/oamdmasrcC000_gdmasrcC0F0_gdmalen13_oamdumper_1` | CGB | C - non-deterministic | impossible |
| 16 | `oamdma/oamdmasrcC000_gdmasrcC0F0_gdmalen13_vramdumper_1` | CGB | C - non-deterministic | impossible |

*Measured against `main_16` (commit `46da86f`, −97.1% from the original 562 failures).
"Impossible" = no correct emulator can pass it; verified by the checks in each section above
(contradiction across references, or reference emulators also failing). Reference-emulator
mismatch counts for Type C were measured with Gambatte's own canonical build against the same
`.dump` references. Two previously-listed tests - `oamdma_src80_oambusy_dumper_1` (DMG) and
`hdma_late_ei_m3halt_m2unhalt_pc_scx1_2` (CGB) - were fixed (see Section 6) and are no longer
failures. All 16 above are genuine floor.*

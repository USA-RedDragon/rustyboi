# rustyboi first-party test ROMs

Hand-written Game Boy test ROMs (RGBDS assembly) that pin **observable hardware
behaviors** rustyboi handles which the public suites (mooneye, mealybug, gambatte,
…) don't cover. Unlike the in-code Rust unit tests — which assert emulator
internals and only ever run against rustyboi — these ROMs are portable: the same
`.gb`/`.gbc` runs on real DMG/CGB silicon and on other emulators, so a behavior we
fixed from one game becomes a check we can confirm on hardware.

This is distinct from `../gb-test-roms/` (the external c-sp bundle).

## Layout

```
include/   hardware.inc, rustyboi_test.inc   (clean-room register defs + pass/fail macros)
src/       <category>/<name>.<model>.<grading>.asm
refs/      <category>/<name>.<model>.png     (oracles for png-graded ROMs)
build/     (gitignored) built ROMs
```

The filename carries the ROM's runner metadata, so the manifest can't drift:

- `<model>` ∈ `dmg` | `cgb` | `agb` — the hardware the runner emulates. Built as
  `.gb` (dmg) or `.gbc` (cgb/agb, `rgbfix -C`).
- `<grading>` — the runner oracle:
  - `mooneye`: the ROM self-verifies, loads the Fibonacci signature
    (B=3,C=5,D=8,E=13,H=21,L=34) and runs `LD B,B`; the runner checks the
    registers. Zero reference files, fully hardware-portable. Use for anything the
    CPU can read back.
  - `png`: the ROM renders a frame and reaches `LD B,B`; the runner compares the
    160×144 framebuffer to `refs/<category>/<name>.<model>.png`. Use for
    render-only behavior the CPU cannot read back.

## Build & run

```
make -C test-roms            # assemble all ROMs, regenerate the runner manifest
rustyboi-test-runner --manifest rustyboi-test-runner/suites/rustyboi.manifest --mode dmg,cgb
```

`make manifest` runs the repo's central generator (`tools/gen_manifests.py
--only rustyboi`), which scans `build/` by the filename convention. `run-suites.sh`
builds these ROMs during setup (rgbds is a documented dependency) and gates the
`rustyboi` suite on its floor.

## Provenance rules (important)

Author ROMs **only for silicon-verified behavior** — documented in Pan Docs, or
later confirmed on the hardware bench. Do NOT encode behavior that is only
Gambatte-derived or emulator-reference-derived; park those in `COVERAGE.md` until
the bench confirms them. A `png` oracle must be **derived from the documented
rule** (or captured on real silicon), never screenshotted from rustyboi.

When a ROM assures a behavior (fails on the pre-fix engine, passes after), delete
the paired in-code Rust test — the ROM is the stronger, portable guard.

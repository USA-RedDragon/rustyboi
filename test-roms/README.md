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

- `<model>` — the hardware the runner emulates *and* the cartridge header the
  ROM is fixed with. The two travel together because some behaviors (the SGB
  unlock gate) are only observable as a combination of the two.

  | token | header | runner hardware |
  |---|---|---|
  | `dmg` | plain, `.gb` | DMG |
  | `cgb` / `agb` | `rgbfix -C`, `.gbc` | CGB / AGB |
  | `sgb` / `sgb2` | `rgbfix -s -l 0x33`, `.gb` | SGB / SGB2 (`rev=` pin) |
  | `sgbcart` | `rgbfix -s -l 0x33`, `.gb` | DMG |
  | `sgblocked` | plain, `.gb` | SGB (`rev=` pin) |
  | `dmgoncgb` | plain, `.gb` | CGB (DMG-compat mode) |

  SGB ROMs need **both** header entries Pan Docs "Unlocking SGB Functions"
  requires — SGB flag `$0146 = $03` and old licensee `$014B = $33` — or the SGB
  ignores every command packet. `rgbfix -s` sets the flag and warns that it
  "will be ignored by the SGB unless the old licensee code (-l) is 0x33".

  `sgbcart` and `sgblocked` exist to test the two halves of that gate: an
  SGB-flagged cart in a plain Game Boy (must not multiplex) and an unflagged
  cart in an SGB (must be ignored). Using plain `dmg` for the first is a trap —
  an unflagged build is gated out of SGB behavior anyway, so it passes even on
  an emulator that never gates the SGB joypad path on SGB hardware.

  There is no separate runner *mode* for SGB: the SGB models all run in `dmg`
  mode with a `rev=sgb`/`rev=sgb2` token, exactly as the external `sgb` and
  `samesuite_sgb` manifests do, because Hardware::SGB is a DMG-class machine
  behind an ICD2. `tools/gen_manifests.py` (`RUSTYBOI_MODELS`) owns that mapping.
- `<grading>` — the runner oracle:
  - `mooneye`: the ROM self-verifies, loads the Fibonacci signature
    (B=3,C=5,D=8,E=13,H=21,L=34) and runs `LD B,B`; the runner checks the
    registers. Zero reference files, fully hardware-portable. Use for anything the
    CPU can read back.
  - `png`: the ROM renders a frame and reaches `LD B,B`; the runner compares the
    160×144 framebuffer to `refs/<category>/<name>.<model>.png`. Use for
    render-only behavior the CPU cannot read back.
  - `bench`: **not an oracle at all** — a hardware-bench measurement ROM. See
    below.

## Measurement ROMs (`bench`)

The two gradings above both assert an expected value, which is right for
silicon-verified behavior and wrong for everything else. A `bench` ROM is for the
opposite case: a cell where our model is SameBoy-derived or first-principles and
**no** existing ROM anywhere can discriminate it. Asserting our current behavior
there would freeze an unverified inference into a permanent oracle — the exact
failure mode the provenance rule below exists to prevent.

So a `bench` ROM has no verdict. It records **raw bytes** to cart SRAM behind the
`RBHW` record header (`include/rbhw_capture.inc`: format tag, ROM id, power-on
run counter, silicon fingerprint vector, payload length, CRC16) and an operator
reads the save back off the cart. Interpretation happens off-cart, by a human,
against the header comment of the ROM that produced the bytes. Each ROM's header
comment carries its own operator protocol (which console, what to read out, how
to read each outcome) and its payload format.

Consequently they are **deliberately not manifest rows**. `tools/gen_manifests.py`
emits one graded row per ROM under `build/`, so the Makefile routes `.bench.`
ROMs to `build-bench/` instead — outside the scanned tree. The `rustyboi` suite
count is unaffected by adding one. They are also fixed with an MBC5+RAM+BATTERY
header (cart type `0x1B`, 8 KiB RAM) rather than the plain ROM-only header the
graded ROMs use, because the capture has to survive power-off on a real cart.

Run one by hand — there is no runner integration by design:

```
make -C test-roms roms
# flash test-roms/build-bench/<category>/<name>.<model>.bench.gb to an
# MBC5+RAM+battery cart, run it on the console named in its header comment,
# then read the .sav back with a GBxCart RW.
```

Open questions and their ROMs are tracked in `COVERAGE.md`. A `bench` ROM is
retired as soon as the bench answers it — either into a real graded ROM, or into
a documented fix.

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

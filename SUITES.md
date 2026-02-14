# Public GB/GBC test suites (c-sp / gameboy-test-roms + friends)

rustyboi is graded against the public Game Boy / Game Boy Color hardware-test
suites in addition to the Gambatte hwtests. These are wired into the main
`rustyboi-test-runner` as first-class, regenerable regression gates so accuracy
can be tracked over time (e.g. while attacking mealybug / APU).

The per-suite pass counts shown in the README progress table are refreshed
automatically on every pull request; this document is the human-readable
reference for what each suite is, how it is graded, and where its ROMs come
from. The single entrypoint shared by CI and developers is
[`tools/run-suites.sh`](tools/run-suites.sh):

```
tools/run-suites.sh list       # print the known suites + pass floors
tools/run-suites.sh <suite>    # run one suite, gate against its floor
tools/run-suites.sh all        # run every suite, gate each
tools/run-suites.sh report     # markdown progress table (all suites)
```

Every suite reads its ROMs and reference images from a single local tree,
**`gb-test-roms/`** (override with `RB_ROMS=<dir>`) — all manifest paths are
relative to it. `tools/run-suites.sh setup` populates it idempotently from three
upstream sources:

- **[c-sp/game-boy-test-roms](https://github.com/c-sp/game-boy-test-roms) v7.0**
  — the bulk of the suites; a release zip fetched and extracted flat into
  `gb-test-roms/`.
- **[gbdev/GBEmulatorShootout](https://github.com/gbdev/GBEmulatorShootout)**
  (`sync_shootout_roms`) — the ROMs *not* in the c-sp set: `sgb`
  (`cpp/sgb-ext-test`), `daid`, and `cpp`. Sparse-checked-out from
  `testroms/{cpp,daid}` and copied into `gb-test-roms/{cpp,daid}/`.
- **[pokemon-speedrunning/gambatte-core](https://github.com/pokemon-speedrunning/gambatte-core)**
  (`sync_gambatte_oracles`) — the `gambatte` suite's `.bin`/`.dump` dumper
  oracles (the c-sp set ships the gambatte ROMs but none of the oracles), copied
  into `gb-test-roms/gambatte/`.

`gb-test-roms/` is gitignored (never committed); CI caches it keyed on those
source versions.

## How it works

Each suite is described by a **manifest** under `rustyboi-test-runner/suites/`,
one test per line, `|`-separated:

```
<id>|<dmg|cgb|agb>|<grading>|<rom_path>[|<arg>]
```

`<grading>` selects the oracle; `<arg>` is the reference-PNG path (`png` /
`png_fixed` / `png_layout` / `png_shootout`), `ADDR=VAL` in hex (`mem`),
`frames=<N>` to override the per-line cycle budget, or empty. Run a manifest
with:

```
rustyboi-test-runner --manifest <suites/X.manifest> --mode dmg,cgb --frames <N> [--json out.json]
```

`--manifest` bypasses the name-based Gambatte discovery (the c-sp layout does
not fit it) and takes the model + oracle from each manifest line. The existing
`--suite <dir>` Gambatte path is unchanged and still grades identically.

### Grading methods (Oracle variants)

| grading        | Oracle              | what it checks |
|----------------|---------------------|----------------|
| `png`          | `CspPng`            | final framebuffer vs c-sp reference PNG; steps the CPU and stops on the `LD B,B` (0x40) done-marker. CGB uses `CgbColorConversion::Linear` (bucket-identical to the c-sp shift formula under the 0xF8 mask). |
| `png_fixed`    | `CspPngFixed`       | same comparison but runs a flat `--frames` cycle budget and grades the held framebuffer — for ROMs that turn the LCD off after rendering their result (e.g. blargg oam_bug). |
| `png_layout`   | `CspPngLayout`      | recolor-invariant layout comparison — matches on tile/pixel *structure* regardless of palette, for ROMs whose reference PNG uses a different color mapping than rustyboi's default palette. |
| `png_shootout` | `PngShootout`       | GBEmulatorShootout's own lenient screenshot rule: per-pixel channel diff `<= 50`, with OR-match across a `;`-separated list of acceptable reference PNGs (some tests have several hardware/instance-dependent valid outputs). Used by the `daid` and `cpp` suites. |
| `serial`       | `Serial`            | blargg serial-port text (FF01/FF02); scans for `Passed` / `Failed`. |
| `blargg_mem`   | `BlarggMem`         | blargg 0xA000 cart-RAM protocol (signature `DE B0 61`, code `00` = pass). |
| `memauto`      | `MemValue{FF82,01}` | gbmicrotest convention: FF82==0x01 pass (FF80=actual, FF81=expected). |
| `mem`          | `MemValue{addr,val}`| generic: read `addr`, compare to `val` after the budget. |
| `mooneye`      | `MooneyeFib`        | run to `LD B,B`, require Fibonacci regs B,C,D,E,H,L = 3,5,8,13,21,34. |
| `mooneye_ed`   | `MooneyeFibEd`      | mooneye Fibonacci check reached via the `ED`-style done-marker used by the wilbertpol / age variants. |
| `gambatte`     | (dumper oracle)     | Gambatte `.bin`/`.dump` framebuffer/register oracles from gambatte-core; gated on `failed <= 16`, see below. |

The PNG decoder (`frame.rs`) handles all c-sp reference formats (color types
0/2/3/6 at bit depths 1/2/4/8, with `PLTE`); the original 8-bit-RGBA Gambatte
path is unchanged.

## Suites

The full suite set, in the deterministic order `tools/run-suites.sh` runs them
(the `ORDER` variable). Each suite has a **pass floor** in the `threshold()`
table: CI passes a suite when `passed >= floor`. Floors **auto-ratchet** up to
the measured counts on every `report-update` (the pre-commit hook) — a landed
improvement becomes the new floor, so the gate flags regressions without a
manifest edit. Floors only ever *rise* automatically; a hand-edit is only
needed to *lower* one. The `gambatte` suite is the exception: it is gated on
`failed <= 16` (`GAMBATTE_MAX_FAIL`), not `passed >= floor`.

Live totals are in the README's `<!-- SUITE-PROGRESS -->` table.

### daid — `suites/daid.manifest` (`png_shootout`)

Eight PPU / STOP / double-speed screen tests authored by "daid", pulled from
GBEmulatorShootout (`testroms/daid`, not in the c-sp set) by
`sync_shootout_roms`. Because the reference PNGs are reference-emulator captures
rather than verified silicon, they are graded with the shootout's lenient
`png_shootout` rule (per-pixel diff `<= 50`, `frames=390` ≈ shootout runtime
0.5 s) instead of strict `png`.

- `ppu_scanline_bgp` (dmg + cgb) — mid-scanline BGP register changes. The DMG
  case OR-matches three acceptable outputs (prev BGP / next BGP / OR'd black
  line — hardware- and instance-dependent) via the `;`-separated ref list.
- `stop_instr` (dmg + cgb) and `stop_instr_gbc_mode3` (cgb) — STOP-instruction
  display behavior.
- `speed_switch_timing_{div,ly,stat}` (cgb) — CGB double-speed KEY1 switch
  timing (DIV / LY / STAT observation).

`rom_and_ram.gb` is intentionally omitted (ships no oracle PNG). Currently
**8 / 8** passing.

### cpp — `suites/cpp.manifest` (`png_shootout`)

Three MBC3 / RTC edge-case screen tests, also from GBEmulatorShootout
(`testroms/cpp`) via `sync_shootout_roms`, graded with the same `png_shootout`
rule (diff `<= 50`, `frames=390`):

- `rtc-invalid-banks-test` (dmg) — RTC access through invalid RAM bank numbers.
- `latch-rtc-test` (dmg) — RTC latch-register behavior.
- `ramg-mbc3-test` (dmg) — MBC3 RAM-gate (RAMG) enable/disable.

Note: the SGB `cpp/sgb-ext-test` ROM lives in the separate **`sgb`** suite, not
in `cpp`. Currently **3 / 3** passing.

## Regenerating the manifests

Most manifests embed relative ROM paths and are regenerable, not hand-authored:

```
python3 tools/gen_manifests.py                        # uses --roms gb-test-roms
python3 tools/gen_manifests.py --roms /path/to/roms   # override the ROM dir
python3 tools/gen_manifests.py --only mealybug,age    # regen selected suites
```

Re-run after updating the ROM set (e.g. a new c-sp release) to rebuild the case
lists from scratch. The `sgb`, `daid` and `cpp` suites are curated by hand (their
ROMs are not in the c-sp set) and are not regenerated by this script.

## Relationship to the Gambatte hwtests

These suites are **additive**. The `gambatte` suite carries the existing
Gambatte hwtests gate, unchanged in spirit: instead of a pass floor it asserts
`failed <= 16` (`GAMBATTE_MAX_FAIL`) — the known real-silicon floor documented
in `rustyboi-test-runner/suites/gambatte.manifest`.

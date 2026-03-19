# Public GB/GBC test suites (c-sp / gameboy-test-roms + friends)

> Per-ROM proof and explanation for every currently failing test lives in
> [KNOWN_FAILURES.md](KNOWN_FAILURES.md).

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
- **[alloncm/MagenTests](https://github.com/alloncm/MagenTests) 0.5.0**
  (`sync_magentests_roms`) — eight prebuilt `.gbc` release assets into
  `gb-test-roms/magentests/`.
- **[nitro2k01/little-things-gb](https://github.com/nitro2k01/little-things-gb)**
  (`sync_little_things_extra`) — the `windesync-validate` (Win-desync-v1.0) and
  `double-halt-cancel` (Double-halt-cancel-v1.0) release ROMs plus their
  repo-hosted reference captures (pinned commit), into
  `gb-test-roms/little-things-gb/` beside the c-sp firstwhite/tellinglys files.
- **[Ashiepaws/sketchtests](https://github.com/Ashiepaws/sketchtests) v0.2-alpha**
  (`sync_sketchtests_roms`) — the prebuilt release zip's three ROMs flattened
  into `gb-test-roms/sketchtests/`.
- **[AntonioND/gbc-hw-tests](https://github.com/AntonioND/gbc-hw-tests)**
  (pinned `631e600`, `sync_gbchwtests_roms`) — a **real-silicon** SRAM-capture
  suite: prebuilt `.gbc` ROMs *and* the real-hardware SRAM dumps
  (`real_gb`/`real_gbp`/`real_gbc`/`real_gba_sp` `.sav`, one unit per device
  class) they are graded against, both committed in the upstream repo. Shallow
  single-commit checkout; the ROMs + `.sav` oracles (~17 MB) are copied
  preserving the `category/test/` layout into `gb-test-roms/gbc-hw-tests/`.

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
| `serial_text`  | `SerialText`        | parameterized serial text: pass/early-fail markers from `pass=`/`fail=` manifest tokens. Captures raw SB (FF01) value changes — the sketchtests ROMs print without the SC handshake (disassembly-verified: no FF02 writes). |
| `blargg_mem`   | `BlarggMem`         | blargg 0xA000 cart-RAM protocol (signature `DE B0 61`, code `00` = pass). |
| `memauto`      | `MemValue{FF82,01}` | gbmicrotest convention: FF82==0x01 pass (FF80=actual, FF81=expected). |
| `mem`          | `MemValue{addr,val}`| generic: read `addr`, compare to `val` after the budget. |
| `mooneye`      | `MooneyeFib`        | run to `LD B,B`, require Fibonacci regs B,C,D,E,H,L = 3,5,8,13,21,34. |
| `mooneye_ed`   | `MooneyeFibEd`      | mooneye Fibonacci check reached via the `ED`-style done-marker used by the wilbertpol / age variants. |
| `sram`         | `SramDump`          | cart SRAM compared **byte-exact** to a real-hardware `.sav` capture (AntonioND gbc-hw-tests): the ROM writes its results to `$A000..` and halts; the whole reference file is the oracle. Reuses the gambatte `.bin`-dumper compare path (runs a flat frame-cycle budget, then reads `save_ram`). |
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

### magentests — `suites/magentests.manifest` (`png`)

Seven of the eight [alloncm/MagenTests](https://github.com/alloncm/MagenTests)
CGB-behavior ROMs (prebuilt release 0.5.0, fetched by `sync_magentests_roms`).
Every verdict is a full-screen color documented in the upstream README and
derived from the test *source* (BGR555 constants in `src/common.asm` under the
runner's Linear conversion) — the reference PNGs under `suites/refs/` are
generated by `tools/gen_manifests.py`, never captured from an emulator:

- `bg_oam_priority` (cgb) — the CGB BG/OAM priority matrix (LCDC.0 master ×
  per-tile attr × OAM flag, toggled per-square by LYC handlers). Reference =
  `refs/magentests-bg-oam-priority.png`, derived from the source geometry and
  matching ISSOtm's **real-CGB hardware photo** (in-repo
  `images/hardware_screenshot.jpg`) square-for-square: 5 green squares + 3
  half-green/half-blue squares, no red lines.
- `hblank_vram_dma` (cgb) — HBlank HDMA must halt while the CPU is halted (an
  undocumented behavior found by the author). Solid green = pass; red/blue
  encode the two failure modes.
- `key0_lock_after_boot` (cgb) — KEY0 (FF4C) must be locked after FF50. Green.
- `ppu_disabled_state` (cgb+dmg) — STAT mode reads 0 while the PPU is off.
  Green on CGB, white on DMG (the DMG path sets BGP=$00 on pass).
- `mbc_oob_sram_mbc1/3/5` (cgb+dmg) — out-of-bounds SRAM bank masking. Green
  on CGB, white on DMG.

`oam_internal_priority` is **excluded**: its only published oracle is a
SameBoy-generated screenshot (emulator-derived; below the oracle bar). It can
be adopted later if someone captures it on hardware the way `bg_oam_priority`
was. Currently **11 / 11** passing.

### little_things_extra — `suites/little_things_extra.manifest` (`png_layout`)

The [nitro2k01/little-things-gb](https://github.com/nitro2k01/little-things-gb)
release ROMs that are *not* in the c-sp set (which only carries firstwhite +
tellinglys), fetched by `sync_little_things_extra`:

- `windesync-validate` (dmg, Win-desync-v1.0) — the pre-CGB **window-desync
  glitch** ((WX&7)==7-(SCX&7) after the window was triggered then disabled
  inserts a BGP-color-0 glitch pixel and shifts the line): the on-screen
  checkmark/cross verdict covers should-trigger AND should-not-trigger
  sections. The reference (`windesync-reference-sgb.png`) was **digitally
  captured from a real Super Game Boy with a logic analyzer** — a genuine
  silicon oracle. DMG-graded only (CGB does not exhibit the quirk).
  `png_layout` because the capture rig's three gray levels are its own
  palette; any missing/extra/shifted glitch pixel breaks the 1:1 mapping.
- `double-halt-cancel` (dmg+cgb) + `double-halt-cancel-gbconly` (cgb,
  Double-halt-cancel-v1.0) — double `halt` with IME=0 is **not** a lockup:
  the CPU refetches the second `halt` byte forever, so when mode-3 VRAM
  locking turns the fetch into $FF (`rst $38`) execution escapes; the ROM
  traps every plausible path and prints the taken path + DIV timing. The
  160x144 references are derived (documented 2x-to-1x downscale in
  `gen_manifests.py`) from the author's published BGB captures — the author
  established the hardware behavior and BGB/SameBoy/Gambatte agree on this
  exact screen. The `-gbconly` ROM differs from the base ROM only in the
  header CGB flag + checksums (cmp-verified), so it shares the CGB reference.

Currently **0 / 4** passing — all four are genuine accuracy targets, see
[KNOWN_FAILURES.md](KNOWN_FAILURES.md).

### sketchtests — `suites/sketchtests.manifest` (`serial_text`)

Three [Ashiepaws/sketchtests](https://github.com/Ashiepaws/sketchtests) ROMs
(prebuilt v0.2-alpha release zip, fetched by `sync_sketchtests_roms`),
serial-graded with the `serial_text` oracle (raw SB writes; these ROMs never
touch SC — verified by disassembly):

- `daa` (dmg+cgb) — exhaustive DAA sweep; hardware-verified upstream (MGB
  9638D + CGB CPU-D per the release notes). Pass prints `Test OK!`.
- `interrupt_priority` (dmg+cgb) — simultaneous-IF priority order;
  hardware-verified upstream on the same units. Pass prints `Test OK!`.
- `model_detector` (dmg+cgb) — prints the detected model; graded against the
  emulated model's name (`DMG` / `CGB`). Spec-derived (boot-register +
  capability probing), included with that provenance note.

Currently **6 / 6** passing.

### gbc_hw_tests — `suites/gbc_hw_tests.manifest` (`sram`)

[AntonioND/gbc-hw-tests](https://github.com/AntonioND/gbc-hw-tests) (pinned
`631e600`, fetched by `sync_gbchwtests_roms`) is a **real-silicon** hardware
suite of ~150 tests spanning cpu / dma / interrupts / lcd / memory / serial /
timers. Each test writes its results to cart SRAM (`$A000..`) and halts; the
upstream repo commits the real-hardware SRAM captures — `real_gb.sav` (DMG),
`real_gbp.sav` (Pocket), `real_gbc.sav` (CGB) and `real_gba_sp.sav` (GBA-SP),
one unit per device class — that those results are graded against, so the
oracle is genuine silicon, not another emulator.

**Device-column mapping** (rustyboi is a CGB emulator):

- every test with a `real_gbc.sav` gets a **CGB-vs-`real_gbc.sav`** case (the
  primary grade);
- **DMG-flagged** ROMs (header `0x143 == 0x00` — the `*_dmg_mode` tests plus the
  DMG-valid DMA/timer tests) *also* get a **DMG-vs-`real_gb.sav`** case;
- the GBA-SP / GBP columns are captured upstream but **not graded** (rustyboi
  targets CGB-04 + DMG-08; GBA-SP is a distinct APU/serial revision).

**Grading** reuses the `sram` oracle (the gambatte `.bin`-dumper compare path):
after a flat frame budget the ROM's `save_ram` is compared **byte-exact** to the
capture. Most captures are trimmed upstream to exactly `[results…][magic
12 34 56 78]`, so the whole file is the oracle. `sc_change_freq_gbc` and
`timer_reset_2` are raw 128 KB card dumps whose written region is followed by
per-unit uninitialised-SRAM garbage; for those the generator emits a byte-exact
*deterministic prefix* (through the last magic marker) under
`suites/refs/gbc-hw-tests/` (a slice of the real dump, never an emulator
capture). Two tests are **excluded** as ungradeable-byte-exact:
`corrupted_stop` (raw dump, un-delimited result + garbage tail) and
`tac_set_everything` (upstream committed *two differing* CGB captures →
per-run nondeterministic by their own measurement). `speed_change_cancel`
grades its input-free `not_pressed` capture.

> **Revision caveat.** AntonioND's captures are from one unit per class and the
> CGB unit's silicon revision is undocumented. Rev-sensitive tests
> (speed-switch sub-timing, STOP sub-dot, mode-2/3 LCD timing) may disagree with
> rustyboi's modeled CGB-04 revision — such a mismatch is a *revision
> difference*, not necessarily an emulator bug. The suite is graded honestly
> regardless (no fudging to inflate the count); per-ROM adjudication lives in
> `KNOWN_FAILURES.md`.

Initial adoption score: **87 / 193** (CGB 69 / 152, DMG 18 / 41), run at 800
frames (the mode-2 / echo-RAM tests sweep a full frame across many repetitions
before settling). The CPU category is a clean **10 / 10** — every STOP / HALT /
DAA / undefined-opcode test passes — and the speed-switch trio
(`speed_change_cancel`, `speed_change_timing_coarse`, `speed_change_timing_fine`)
passes on CGB, corroborating the plain-STOP / speed-switch model. The remaining
gaps are concentrated in the sub-dot LCD-frame-timing family (the revision-caveat
zone) plus a handful of DMA-source-validity, echo-RAM and joypad-IRQ behaviours
tracked as work items.

### Census skips (evaluated, not adopted)

- **CasualPokePlayer/test-roms `sgb-mlt-test`** — no oracle exists anywhere:
  the branch is RGBDS source only (no prebuilt ROM, no release artifacts), its
  README documents no expected result, and the GBEmulatorShootout repo (which
  carries `sgb-ext-test` + its real-SGB reference) has never added it.
- **CasualPokePlayer/test-roms `open-bus-ss-test`** — source-only branch whose
  sole real-hardware capture (a GBI/Game Boy Player screenshot in the README)
  is bound to the author's unpublished 2021 build: the ROM embeds
  `__ISO_8601_UTC__` at build time and the open-bus verdict bytes echo fetched
  ROM bytes, so no rebuildable ROM can be verified against that capture (and
  the 2021 Makefile no longer builds under modern RGBDS without unfaithful
  patching).
- **MagenTests `oam_internal_priority`** — SameBoy-screenshot oracle only (see
  above).

## Regenerating the manifests

Most manifests embed relative ROM paths and are regenerable, not hand-authored:

```
python3 tools/gen_manifests.py                        # uses --roms gb-test-roms
python3 tools/gen_manifests.py --roms /path/to/roms   # override the ROM dir
python3 tools/gen_manifests.py --only mealybug,age    # regen selected suites
```

Re-run after updating the ROM set (e.g. a new c-sp release) to rebuild the case
lists from scratch. The `gbc_hw_tests` suite is regenerated here too (from the
fetched `gbc-hw-tests/` tree, including its trimmed `.sav` prefixes under
`suites/refs/gbc-hw-tests/`). The `sgb`, `daid` and `cpp` suites are curated by
hand (their ROMs are not in the c-sp set) and are not regenerated by this script.

## Relationship to the Gambatte hwtests

These suites are **additive**. The `gambatte` suite carries the existing
Gambatte hwtests gate, unchanged in spirit: instead of a pass floor it asserts
`failed <= 16` (`GAMBATTE_MAX_FAIL`) — the known real-silicon floor documented
in `rustyboi-test-runner/suites/gambatte.manifest`.

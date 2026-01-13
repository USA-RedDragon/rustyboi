# Public GB/GBC test suites (c-sp / gameboy-test-roms)

rustyboi is graded against the public Game Boy / Game Boy Color hardware-test
suites in addition to the Gambatte hwtests. These are wired into the main
`rustyboi-test-runner` as first-class, regenerable regression gates so accuracy
can be tracked over time (e.g. while attacking mealybug / APU).

ROM set: [c-sp/gameboy-test-roms](https://github.com/c-sp/gameboy-test-roms)
**v7.0** (2024-02-25), unzipped at `/home/reddragon/gb-test-roms` (override with
`ROMS=<dir>` when regenerating manifests).

## How it works

Each suite is described by a **manifest** under `rustyboi-test-runner/suites/`,
one test per line, `|`-separated:

```
<id>|<dmg|cgb|agb>|<grading>|<rom_path>[|<arg>]
```

`<grading>` selects the oracle; `<arg>` is the reference-PNG path (`png` /
`png_fixed`), `ADDR=VAL` in hex (`mem`), or empty. Run a manifest with:

```
rustyboi-test-runner --manifest <suites/X.manifest> --mode dmg,cgb --frames <N> [--json out.json]
```

`--manifest` bypasses the name-based Gambatte discovery (the c-sp layout does
not fit it) and takes the model + oracle from each manifest line. The existing
`--suite <dir>` Gambatte path is unchanged and still grades identically.

### Grading methods (Oracle variants)

| grading      | Oracle           | what it checks |
|--------------|------------------|----------------|
| `png`        | `CspPng`         | final framebuffer vs c-sp reference PNG; steps the CPU and stops on the `LD B,B` (0x40) done-marker. CGB uses `CgbColorConversion::Linear` (bucket-identical to the c-sp shift formula under the 0xF8 mask). |
| `png_fixed`  | `CspPngFixed`    | same comparison but runs a flat `--frames` cycle budget and grades the held framebuffer — for ROMs that turn the LCD off after rendering their result (e.g. blargg oam_bug). |
| `serial`     | `Serial`         | blargg serial-port text (FF01/FF02); scans for `Passed` / `Failed`. |
| `blargg_mem` | `BlarggMem`      | blargg 0xA000 cart-RAM protocol (signature `DE B0 61`, code `00` = pass). |
| `memauto`    | `MemValue{FF82,01}` | gbmicrotest convention: FF82==0x01 pass (FF80=actual, FF81=expected). |
| `mem`        | `MemValue{addr,val}`| generic: read `addr`, compare to `val` after the budget. |
| `mooneye`    | `MooneyeFib`     | run to `LD B,B`, require Fibonacci regs B,C,D,E,H,L = 3,5,8,13,21,34. |

The PNG decoder (`frame.rs`) handles all c-sp reference formats (color types
0/2/3/6 at bit depths 1/2/4/8, with `PLTE`); the original 8-bit-RGBA Gambatte
path is unchanged.

## Suites, run commands, and current results

Measured on `f-suiteint` with the default synthetic `skip_bios()` seed.

### dmg/cgb-acid2 — `suites/acid2.manifest` (`png`)
```
rustyboi-test-runner --manifest rustyboi-test-runner/suites/acid2.manifest --mode dmg,cgb --frames 60
```
**2 / 2** real cases pass: `dmg-acid2` (DMG) and `cgb-acid2` (CGB). The third
entry, `dmg-acid2-on-cgb`, is a known fail (CGB-compat auto-palette not applied
under skip_bios).

### mealybug-tearoom — `suites/mealybug.manifest` (`png`)
```
rustyboi-test-runner --manifest rustyboi-test-runner/suites/mealybug.manifest --mode dmg,cgb --frames 60
```
**2 / 51** (3 with `--real-bios`). The rest are mid-mode-3 PPU timing. Passing:
`m2_win_en_toggle` (DMG) and `m3_scx_low_3_bits` (DMG; +CGB with the real boot
ROM). DMG refs use `<stem>_dmg_blob.png`, CGB refs use `<stem>_cgb_c.png`.

### blargg (aggregates) — `suites/blargg.manifest` (best oracle per ROM)
```
rustyboi-test-runner --manifest rustyboi-test-runner/suites/blargg.manifest --mode dmg,cgb --frames 4000
```
**13 / 15.** Pass: cpu_instrs, instr_timing, mem_timing (serial); mem_timing-2,
cgb_sound (blargg_mem); halt_bug, interrupt_time (png); oam_bug-cgb (png_fixed —
the OAM bug is absent on CGB). Fail: **dmg_sound** (10/12 subtests — code 08) and
**oam_bug-dmg** (DMG OAM-bug emulation incomplete, 3/8).

### blargg (per-subtest singles) — `suites/blargg_singles.manifest`
```
rustyboi-test-runner --manifest rustyboi-test-runner/suites/blargg_singles.manifest --mode dmg,cgb --frames 2000
```
**39 / 41.** The 2 fails are the dmg_sound subtests `08-len ctr during power`
and `11-regs after power` (NR41). cpu_instrs/mem_timing singles use `serial`;
mem_timing-2 / sound singles use `blargg_mem`.

### gbmicrotest — `suites/gbmicrotest.manifest` (`memauto`, DMG-CPU-08)
```
rustyboi-test-runner --manifest rustyboi-test-runner/suites/gbmicrotest.manifest --mode dmg --frames 60
```
**460 / 513** (FF82==0x01). Of the 53 non-passes, ~24 are real assertion fails
(a HALT+interrupt-timing cluster, DMA timing) and ~29 are testbench/visual ROMs
that never write the FF80–FF82 protocol (not pass/fail ROMs).

### mooneye-test-suite — `suites/mooneye.manifest` (`mooneye`)
```
rustyboi-test-runner --manifest rustyboi-test-runner/suites/mooneye.manifest --mode dmg,cgb
# boot_* cases need the real boot ROM:
rustyboi-test-runner --manifest rustyboi-test-runner/suites/mooneye.manifest --mode dmg,cgb \
    --real-bios --bios-dir /home/reddragon/projects/rustyboi/bios
```
**142 / 192.** (`mooneye` uses an internal cycle cap; `--frames` is ignored.) Of
the 50 non-passes: ~26 real behavioral fails (PPU/timing), ~12 boot-revision
mismatches (rustyboi models DMG-CPU-ABC + CGB-CPU-04; off-revision boot_* tests
legitimately fail — the targeted revisions pass, and `boot_regs-cgb` passes with
`--real-bios`), and ~7 MBC emulator-only gaps. Device-suffix model rule:
`-dmg*/-mgb/-S/-GS` → DMG, `-cgb*/-C/-A` → CGB, `-sgb/-sgb2` skipped, no-suffix →
both.

## Regenerating the manifests

The manifests embed absolute ROM paths and are regenerable, not hand-authored:

```
bash tools/gen_suite_manifests.sh          # uses ROMS=/home/reddragon/gb-test-roms
ROMS=/path/to/roms bash tools/gen_suite_manifests.sh
```

Re-run after updating the ROM set (e.g. a new c-sp release) to rebuild the case
lists from scratch.

## Baselines

Per-suite baseline result JSONs (same shape as the Gambatte `.baselines/*.json`)
live under `.baselines/suites/` so changes are diffable:

```
.baselines/suites/{acid2,mealybug,blargg,blargg_singles,gbmicrotest,mooneye}.json
```

`.baselines/` is gitignored (local tracking only). Regenerate any baseline by
re-running its suite with `--json .baselines/suites/<suite>.json`.

## Relationship to the Gambatte hwtests

These c-sp suites are **additive**. The existing Gambatte hwtests gate is
unchanged: `--suite gambatte-core/test/hwtests --mode dmg,cgb` still reports
**17** failures (identical set vs `.baselines/main_17.json`).

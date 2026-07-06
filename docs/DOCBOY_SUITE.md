# docboy-test-suite adoption — scoping report & integration design

Scoped 2026-07-05 against suite commit `c9f9a7de` (2026-06-30) and docboy emulator
HEAD (shallow). Suite: https://github.com/Docheinstein/docboy-test-suite —
**9,902 test ROMs**, the largest candidate suite ever evaluated (gambatte: 5,257).

## 1. Build

- `make -jN` with **RGBDS 0.9.4**: clean build, **9,902 ROMs in ~34 s** (740 CPU-s),
  340 MB roms + 40 MB symbols. 9,879 ROMs are 32 KiB; 21× 1 MiB, 1× 2 MiB, 1× 8 MiB
  (MBC bank tests).
- **RGBDS 1.0.x does NOT build it**: 153 sources use `ld [c], a` (removed in 1.0;
  needs `ldh`). Pin 0.9.x (prebuilt release tarball
  `rgbds-linux-x86_64.tar.xz` from tag `v0.9.4` works).
- Variants (per-variant `-P` header include): `dmg` 2,349 (`.gb`, DMG hardware),
  `cgb` 6,889 (`.gbc`, CGB), `cgb_dmg_mode` 638 (`.gb` CgbFlag $00, run on CGB =
  DMG-compat), `cgb_dmg_ext_mode` 26 (CgbFlag **$88**, CGB "DMG ext" mode).
- Category census (merged): double_speed 4,246 (of which **apu 3,415** — ch4 alone
  2,565), ppu 2,323, apu 1,663, hdma 648, oam_bug 273, interrupts 185, boot 115,
  dma 109, serial 97, memory 71, mbc 60 (mbc1 12 / mbc3 21 / mbc5 1 / mbc7 17 /
  huc3 9), stat_write_bug 31, mode 20, undocumented (FF72-75) 12, timers 12,
  joypad 12, cpu 12, banks 6, ir 3, key1 2, key0 2.

## 2. Provenance verdict (the gate)

**Verdict: docboy-emulator-derived expectations, anchored per-area to public
hardware-derived research. NOT independently silicon-verified by the author at
scale. Adoptable — as a cross-emulator conformance suite with per-area
confidence tiers, not as a silicon oracle.**

Evidence trail:

- No hardware rig / flashcart / capture statement anywhere in either repo.
  docboy README claims accuracy focus + GBEmulatorShootout membership only.
- Exactly **one** commit in 2.5 years says "tested on real hardware"
  (`a9cc590c`, one halt ROM, Apr 2024). Two sources carry
  `; TODO: test on real hardware` — the author distinguishes, so the default is
  *not* hardware-run.
- docboy's APU git history is explicitly "pass samesuite X" — its model (and
  hence these tests' constants) was calibrated against **SameSuite**
  (hardware-validated by LIJI's captures), then extrapolated into the big
  double-speed matrices that SameSuite does not cover.
- The suite is NOT in GBEmulatorShootout's suite list (no third-party
  hardware row exists for it).
- The grading is self-checking: each ROM embeds its expected constants and
  writes a verdict byte (below). The assertions therefore live in the `.asm`
  sources; sampled tracing:

| Area | Sampled | Constant traces to | Tier |
|---|---|---|---|
| oam_bug (273) | push_round3 expected OAM block | pandocs OAM-corruption **write formula, bit-exact**: `((a^c)&(b^c))^c` gave `$0FE8` = embedded `e8 0f`; words 1-3 copied from preceding row | HIGH (community hardware research) |
| mbc7 (17) | accel_read_initial | pandocs/endrift MBC7: pre-latch accel reads `$8000`; $55/$AA latch protocol; EEPROM 93LC56 command set | HIGH (spec-derivable) |
| huc3 (9) | huc3_tick | HuC-3 RTC command protocol (public RE, 2020-22) | MED-HIGH |
| ir RP (3) | rp_write_read | write $00 → read `$3E`, $FF → $FF (unused bits 1, RE bits R/W, no-signal bit 1) — pandocs/SameBoy consensus | MED-HIGH |
| key0/key1, FF72-75 (16) | key0_write_read, ff72_write_read | pandocs undocumented-registers section | HIGH |
| stat_write_bug (31) | hblank_scx4_round1 | documented DMG STAT-write-$FF glitch; per-SCX edge rounds are author-refined | MED (glitch documented; edges docboy-derived). rustyboi already passes 30/31 sampled-run → consistent with our silicon-calibrated model |
| memory not_usable (CGB) | not_usable_area_write | FEA0-FEBF RAM + FEC0-FEFF 16-byte 4× mirror = SameBoy/pandocs documented CGB behavior | MED-HIGH |
| boot (115) | boot_div_* per-header | CGB boot ROM exit-DIV varies with header bytes (licensee/title) — **derivable from the dumped boot ROM**, extends mooneye's single-header coverage | MED-HIGH |
| double_speed apu (3,415) | ch4 divider-change matrix | PCM12/34 sampling at exact M-cycles after NR43 mid-run changes in double speed — **not publicly documented at this granularity**; docboy-model extrapolation from SameSuite-calibrated core | LOW-MED (flag) |
| hdma remaining_length ×SCX (≈300) | hdma_remaining_length_* | HDMA5 countdown vs mode-3 length per SCX — plausible, author-derived | MED (flag) |

- **No contradiction red-flags found**: nothing sampled contradicts
  rustyboi's hardware-proven behaviors (gambatte/mooneye/samesuite floors).
  Failures land in areas rustyboi genuinely doesn't model (per-header boot DIV,
  CGB not-usable RAM mirror, MBC7/HuC3 mappers, mid-m3 glitch refinements).
- Independent consistency signal: rustyboi (calibrated to real cgb04c/dmg08
  silicon via gambatte/age/mooneye/mealybug) already passes **77.5%** untuned,
  with failures clustering exactly where rustyboi has known open frontiers —
  the suite agrees with silicon wherever we can already measure silicon.

Per-standing-directive: for LOW-MED areas, treat docboy expectations as
*conformance targets* (like "matches SameBoy"), not silicon proof. Where a
docboy expectation ever conflicts with a real capture, the capture wins.

## 3. Grading design (verified on built ROMs)

### 3a. Primary oracle — `$FFF0` verdict byte (8,752 tests)

Every non-visual test ends with `TestSuccess`/`TestFail` (inc/test.inc):
`[$FFF0] = $01` pass, `$02` fail, then draws the verdict screen. docboy's own
harness greps exactly this (config JSONs: `{"address": 65520, "value": 1,
"fail_value": 2}`), polling every 100k T-cycles, early-stop, ceiling
100M T-cycles (=1,424 frames).

Runner mapping: `MemValue`-style with `FFF0=01`, **plus a new `docboy` grading
keyword** that early-stops when `[FFF0]` becomes 1 or 2 (semantics identical to
docboy's MemoryRunner). Measured settle time across 6,785 passes: median 2
frames, p90 5, max 302 — early-stop makes the whole mem side ≈600 CPU-s.
Without runner changes, `mem|FFF0=01|frames=N` works today (fixed budget); use
per-test `frames` = measured settle + margin, default 60, `tick`/`rtc` names
4,600.

### 3b. Verdict-screen pixel signature (verified, backup oracle)

Tile decode + live-frame verification (DMG and CGB dumps):

- **PASS** = horizontal stripes: rows y≡3,4,5 (mod 8) fully dark; screen counts
  8,640 dark / 14,400 light. Row 4 is uniformly dark; column 3 is mixed.
- **FAIL** = vertical stripes: cols x≡3,4 (mod 8) fully dark; 5,760 dark /
  17,280 light. Column 3 uniformly dark; row 4 mixed.
- 2-pixel discriminator: `P(0,4)` dark ∧ `P(3,0)` light ⇒ PASS;
  `P(3,0)` dark ∧ `P(0,4)` light ⇒ FAIL. Dark/light = shade 3/0 on DMG,
  `#000000`/`#FFFFFF` on CGB. CGB verdict screens render ~20 frames after the
  `$FFF0` write (two 8 KiB VRAM memsets); DMG ~8.
- Not needed for adoption ($FFF0 is strictly cheaper) — documented as
  validation/fallback.

### 3c. Visual tests — reference PNGs (1,150 tests)

docboy grades these framebuffer-vs-PNG with per-channel tolerance 5
(`COLOR_TOLERANCE_LOW`). Refs live in docboy repo `tests/results/<variant>/docboy/...`
(the suite repo's own `results/` has only a 627-PNG subset — use the docboy copies).

- **DMG refs** use docboy's green palette, RGB565 {0x84A0,0x4B40,0x2AA0,0x1200}
  → PNG via `v*255/max` = **(131,149,0), (74,105,0), (41,85,0), (16,64,0)** for
  shades 0-3. Map rustyboi's shade indices through this LUT, then exact/tol-5
  compare. Verified: layout-perfect on samples.
- **CGB refs**: docboy uses a raw 555→565 identity table then `v*255/max`;
  rustyboi's default conversion differs ≤1/channel — tolerance 5 (docboy's own
  rule) absorbs it. Verified: e.g. `(0,98,197)` vs ref `(0,97,197)`.
- Budget: fixed 240 frames was enough for every sampled renderer; recommend
  early-stop-on-match polling every ~8 frames.

### 3d. Exclusions

- **interactive** (115: 109 mem + 6 visual): need scripted joypad. docboy
  configs carry `inputs` tick schedules → convertible to the runner's `input=`
  (frame[@ly]) scripts later; excluded from v1 manifest (runner would need
  `input=` support for mem gradings too).
- **serial two-player** (2): dual-instance link (`rom2`/`framebuffer2`) — no
  runner support; excluded.
- Adoptable v1 total: **9,785** (8,643 mem + 1,144 visual — minus the 2 link,
  counted in visual).

## 4. Initial ad-hoc score (rustyboi @ 361731b, skip_bios, DMG/CGB default revs)

Harness: scratchpad `dbts-harness` (rustyboi-core direct, $FFF0 early-stop,
docboy-equal budgets; visual side layout/tol-5 vs docboy refs).

- **Memory-graded: 6,785 / 8,752 = 77.5 %** (non-interactive: 6,748/8,643 = 78.1 %)
- **Visual: 614 / 1,150 = 53.4 %**
- **Combined: 7,399 / 9,902 = 74.7 %** — untuned, zero emulator changes.

Clean sweeps already: stat_write_bug 30/31, cgb/dma 41/41, mbc1 12/12,
cgb oam_bug 5/5 (correctly absent on CGB), serial (single-player) ~93 %,
mode/key0/ir/undocumented (CGB) all green, double_speed core+serial 12/12.

Top failing areas (count ≈ fail+timeout, one example each):

| Area | Fail | Example |
|---|---|---|
| cgb/double_speed/apu | 860 | `ch4/double_speed_apu_ch4_double_retrigger_shift1_divider0_delay0_1_nops2_round2` (double-retrigger LFSR matrix dominates) |
| cgb/hdma | 315 | `hdma_remaining_length_ly0_scx0_*` (HDMA5 countdown vs mode-3/SCX), `gdma_destination_overflow_9fff_then_ffff_hdma5` |
| visual m3 glitch families | 534 | `change_bg_tile_data_glitch_mealybug_*` (cgb_dmg_mode 235, dmg 235, cgb 64) — BGP-phase ×81, window_turn_off ×32, change_wx_glitch |
| dmg/oam_bug | 113 | `oam_bug_ld_a_hl_round11` (pop/push round refinements) |
| dmg/ppu | 99 | `oam_scan_blocked_by_dma_nops1xx` (24 hang = polled condition never occurs) |
| cgb/double_speed/interrupts | 72 | `double_speed_interrupt_serial_halted_a_round2` |
| apu ch1 sweep (dmg+cgb) | 114 | `ch1_period_sweep_change_direction_to_decrease_during_recalc_round1` |
| cgb/boot | 38 | `boot_div_phase_old_license_33_round1` (per-header boot-DIV — spec-derivable, unmodeled) |
| double_speed/hdma | 25 | — |
| cgb_dmg_mode/{ppu,boot,memory} | 91+ | `bcps_write_read`, `boot_hram`, `not_usable_area_write` |
| mbc7 | 8 | `mbc7_accel_read_initial` (no MBC7 mapper — rustyboi supports MBC1/2/3/5 only) |
| huc3 | 8 | all timeout/fail (no HuC-3 mapper) |
| cgb_dmg_ext_mode | 17 | `mode_cgb_flag_88` (CgbFlag $88 ext-DMG mode unmodeled) |

## 5. Integration plan (follow-up agent)

1. **Fetch strategy — prefer building the suite repo from source** (pinned
   commit `c9f9a7de`, RGBDS pinned `v0.9.4` prebuilt tarball): 34 s build, no
   release artifacts exist upstream. Alternative considered: docboy repo ships
   prebuilt copies under `tests/roms/*/docboy/` + all reference PNGs +
   grading JSONs — but it lags/leads the suite (10,143 ROMs ≠ 9,902) and pins
   a second repo; still **fetch docboy once for `tests/results` PNGs and
   `tests/config/*.json`** (input schedules, fb-vs-mem map), or vendor the
   1,150 PNGs into `suites/refs/` (≈8 MB).
2. **Manifest generation** (extend tools/gen_manifests.py — coordinate with
   infra agent): walk built `roms/`, mode = `dmg` for `dmg/`, `cgb` for the
   other three variants; grading `docboy` (new early-stop keyword; interim:
   `mem|FFF0=01` + per-test `frames`) for non-fb tests, `docboy_png` (DMG LUT +
   tol-5, or interim `png_shootout`-style) for the 1,150 fb tests keyed off
   `tests/config/*.json`; skip interactive/link (list them commented, like the
   gbmicrotest exclusions).
3. **Runner additions** (small): `docboy` grading = MemValue(FFF0) + early-stop
   on 1/2 + default 1424-frame ceiling; `docboy_png` = CspPngFixed + DMG LUT
   palette + tol-5 compare (+ optional early-stop-on-match).
4. **Thresholds**: initial ratchet `failed<=2503` (2,503 = 9,902 − 7,399), or
   split manifests per area (docboy_mem / docboy_vis) so APU-frontier churn
   doesn't mask PPU regressions. Given LOW-MED provenance tier for
   double_speed-apu, consider a separate `docboy_ds_apu.manifest` with its own
   floor so the conformance-tier tests never gate silicon-tier work.
5. **Runtime**: measured — mem side 8,752 ROMs ≈ 600 CPU-s (4 min wall at
   24-way with naive sharding; ≈40 s balanced); visual 1,150 × 240 frames ≈
   2,100 CPU-s (≈90 s at 24-way), 4-5× cheaper with early-stop-on-match.
   **Full-suite CI cost ≈ 3-4 min single 8-core job** — no sharding needed.
   Build-from-source adds ~40 s + RGBDS download.
6. **Emulator work unlocked** (discovery list, biggest-first): double-speed
   CH4 double-retrigger model; HDMA5 remaining-length readback timing;
   mid-m3 BGP/tile-sel/WX glitch refinements (dovetails the existing mealybug
   wall); DMG OAM-bug pop/push rounds; per-header CGB boot-DIV; CGB
   FEA0-FEFF RAM+mirror; **MBC7 (accel+93LC56 EEPROM) and HuC-3 mappers**
   (first-ever coverage); CgbFlag $88 ext-DMG mode; oam_scan_blocked_by_dma
   hangs.

## Scratchpad artifacts (this session)

`…/scratchpad/`: `docboy-test-suite/` (built roms), `docboy/` (emulator clone,
refs+configs), `rgbds09/` (0.9.4), `dbts-harness/` (scorer + PPM dump),
`run-list*.txt`, `all-mem.out`, `per-cat.json`, `examples.json`,
`vis-fails2.json`, `framedumps/`, `visdumps/`.

#!/usr/bin/env bash
# Regenerate the c-sp public-suite manifests consumed by
# `rustyboi-test-runner --manifest`. This makes the suite gates reproducible:
# re-run after updating the ROM set to rebuild the manifests from scratch.
#
# ROMs: c-sp/gameboy-test-roms (default v7.0), unzipped at $ROMS.
# Output: one `<suite>.manifest` per suite under $OUT. Manifest line format:
#   <id>|<dmg|cgb|agb>|<grading>|<rom_path>[|<arg>]
# grading: png | serial | blargg_mem | memauto | mem | mooneye
#   - png       <arg> = reference PNG path (decoder handles all c-sp formats)
#   - mem       <arg> = ADDR=VAL in hex
#   - others    no <arg>
#
# Model selection follows rustyboi's modeled revisions (DMG-CPU-ABC, CGB-CPU-04):
# mealybug CGB refs use the `_cgb_c` rev; mooneye device suffixes map -dmg*/-mgb
# to DMG and -cgb*/-C/-A to CGB. gbmicrotest is a DMG-CPU-08 suite (DMG only).
set -euo pipefail

ROMS="${ROMS:-/home/reddragon/gb-test-roms}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${OUT:-$HERE/rustyboi-test-runner/suites}"
mkdir -p "$OUT"

if [ ! -d "$ROMS" ]; then
  echo "error: ROM set not found at $ROMS (set ROMS=<dir>)" >&2
  exit 1
fi

echo "ROMs:    $ROMS"
echo "Output:  $OUT"

# --- acid2 -----------------------------------------------------------------
# dmg-acid2 on DMG and (compat) on CGB; cgb-acid2 on CGB.
{
  echo "# dmg/cgb-acid2 PPU reference screens (c-sp). Run: --frames 60"
  d="$ROMS/dmg-acid2"; c="$ROMS/cgb-acid2"
  [ -f "$d/dmg-acid2.gb" ]  && echo "dmg-acid2|dmg|png|$d/dmg-acid2.gb|$d/dmg-acid2-dmg.png"
  [ -f "$d/dmg-acid2.gb" ]  && echo "dmg-acid2-on-cgb|cgb|png|$d/dmg-acid2.gb|$d/dmg-acid2-cgb.png"
  [ -f "$c/cgb-acid2.gbc" ] && echo "cgb-acid2|cgb|png|$c/cgb-acid2.gbc|$c/cgb-acid2.png"
} > "$OUT/acid2.manifest"
echo "  acid2:     $(grep -vc '^#' "$OUT/acid2.manifest") cases"

# --- mealybug-tearoom ------------------------------------------------------
# ppu/*.gb only (dma/*.gb have no reference PNGs). DMG ref = <stem>_dmg_blob.png;
# CGB ref = <stem>_cgb_c.png (CGB-CPU-04 ≈ rev C). Exact-stem matching avoids
# the m3_bgp_change vs m3_bgp_change_sprites prefix collision.
mb="$ROMS/mealybug-tearoom-tests/ppu"
{
  echo "# mealybug-tearoom PPU mid-mode-3 reference screens. Run: --frames 60"
  if [ -d "$mb" ]; then
    for rom in "$mb"/*.gb; do
      [ -e "$rom" ] || continue
      stem="$(basename "$rom" .gb)"
      dmg="$mb/${stem}_dmg_blob.png"
      cgb="$mb/${stem}_cgb_c.png"
      [ -f "$dmg" ] && echo "mealybug/$stem|dmg|png|$rom|$dmg"
      [ -f "$cgb" ] && echo "mealybug/$stem|cgb|png|$rom|$cgb"
    done
  fi
} > "$OUT/mealybug.manifest"
echo "  mealybug:  $(grep -vc '^#' "$OUT/mealybug.manifest") cases"

# --- blargg (best oracle per ROM) ------------------------------------------
# Aggregate ROMs (the canonical pass/fail set), each with the oracle that ROM
# actually exposes (RESULTS.md "best oracle per ROM"):
#   serial     -> cpu_instrs, instr_timing, mem_timing (print to the serial port)
#   blargg_mem -> mem_timing-2, dmg_sound, cgb_sound  (0xA000 cart-RAM protocol)
#   png        -> halt_bug, interrupt_time            (screen; stops on LD B,B)
#   png_fixed  -> oam_bug   (LCD off after result screen; flat cycle budget)
# DMG+CGB where the ROM supports both; interrupt_time + cgb_sound are CGB-only,
# dmg_sound is DMG-only, by design.
bl="$ROMS/blargg"
emit_blargg() { # id rom grading modes [refpng]
  local id="$1" rom="$2" grading="$3" modes="$4" ref="${5:-}"
  [ -f "$rom" ] || return 0
  for m in $modes; do
    if [ -n "$ref" ]; then echo "$id|$m|$grading|$rom|$ref"; else echo "$id|$m|$grading|$rom"; fi
  done
}
{
  echo "# blargg test ROMs (best oracle per ROM). Run: --frames 4000"
  emit_blargg "cpu_instrs"     "$bl/cpu_instrs/cpu_instrs.gb"        serial     "dmg cgb"
  emit_blargg "instr_timing"   "$bl/instr_timing/instr_timing.gb"   serial     "dmg cgb"
  emit_blargg "mem_timing"     "$bl/mem_timing/mem_timing.gb"       serial     "dmg cgb"
  emit_blargg "mem_timing-2"   "$bl/mem_timing-2/mem_timing.gb"     blargg_mem "dmg cgb"
  emit_blargg "dmg_sound"      "$bl/dmg_sound/dmg_sound.gb"         blargg_mem "dmg"
  emit_blargg "cgb_sound"      "$bl/cgb_sound/cgb_sound.gb"         blargg_mem "cgb"
  emit_blargg "halt_bug"       "$bl/halt_bug.gb"                    png "dmg cgb" "$bl/halt_bug-dmg-cgb.png"
  emit_blargg "oam_bug-dmg"    "$bl/oam_bug/oam_bug.gb"             png_fixed "dmg" "$bl/oam_bug/oam_bug-dmg.png"
  emit_blargg "oam_bug-cgb"    "$bl/oam_bug/oam_bug.gb"             png_fixed "cgb" "$bl/oam_bug/oam_bug-cgb.png"
  emit_blargg "interrupt_time" "$bl/interrupt_time/interrupt_time.gb" png "cgb" "$bl/interrupt_time/interrupt_time-cgb.png"
} > "$OUT/blargg.manifest"
echo "  blargg:    $(grep -vc '^#' "$OUT/blargg.manifest") cases"

# blargg per-subtest singles (finer-grained detail; not the headline number).
{
  echo "# blargg per-subtest singles. Run: --frames 2000"
  for rom in "$bl"/cpu_instrs/individual/*.gb; do
    [ -e "$rom" ] || continue
    echo "cpu_instrs/$(basename "$rom" .gb)|dmg|serial|$rom"
  done
  for rom in "$bl"/mem_timing/individual/*.gb; do
    [ -e "$rom" ] || continue
    echo "mem_timing/$(basename "$rom" .gb)|dmg|serial|$rom"
  done
  for rom in "$bl"/mem_timing-2/rom_singles/*.gb; do
    [ -e "$rom" ] || continue
    echo "mem_timing-2/$(basename "$rom" .gb)|dmg|blargg_mem|$rom"
  done
  for rom in "$bl"/dmg_sound/rom_singles/*.gb; do
    [ -e "$rom" ] || continue
    echo "dmg_sound/$(basename "$rom" .gb)|dmg|blargg_mem|$rom"
  done
  for rom in "$bl"/cgb_sound/rom_singles/*.gb; do
    [ -e "$rom" ] || continue
    echo "cgb_sound/$(basename "$rom" .gb)|cgb|blargg_mem|$rom"
  done
} > "$OUT/blargg_singles.manifest"
echo "  blargg_singles: $(grep -vc '^#' "$OUT/blargg_singles.manifest") cases"

# --- gbmicrotest (DMG-CPU-08; FF82 protocol) -------------------------------
gm="$ROMS/gbmicrotest"
{
  echo "# gbmicrotest (DMG-CPU-08). FF82==0x01 pass. Run: --frames 60"
  if [ -d "$gm" ]; then
    for rom in "$gm"/*.gb; do
      [ -e "$rom" ] || continue
      echo "$(basename "$rom" .gb)|dmg|memauto|$rom"
    done
  fi
} > "$OUT/gbmicrotest.manifest"
echo "  gbmicrotest: $(grep -vc '^#' "$OUT/gbmicrotest.manifest") cases"

# --- mooneye (Fibonacci magic registers) -----------------------------------
# Device-suffix model rule: -dmg*/-mgb/-S/-GS -> DMG, -cgb*/-C/-A -> CGB,
# -sgb/-sgb2 (SGB, not modeled) skipped, no-suffix -> both DMG and CGB.
# -GS = Game Boy + Super Game Boy (a DMG-family target), so DMG-only. -C/-A are
# CGB-family (RESULTS.md notes these are the correctly-modeled CGB revisions).
# Covers acceptance/, emulator-only/, misc/, and madness/ (the c-sp v7.0 layout).
mn="$ROMS/mooneye-test-suite"
classify_modes() { # filename-stem -> echo space-separated modes (empty = skip)
  local s="$1"
  case "$s" in
    *-sgb|*-sgb2)        echo "" ;;          # SGB: not modeled
    *-dmg0|*-dmgABC|*-dmgABCmgb|*-mgb|*-S|*-GS) echo "dmg" ;;
    *-cgb0|*-cgbABCDE|*-cgb|*-C)       echo "cgb" ;;
    *-A)                 echo "cgb" ;;        # AGB ~ CGB-compat for these
    *)                   echo "dmg cgb" ;;    # no suffix: run both
  esac
}
# boot_* revision override: boot_regs/boot_div/boot_hwio check one silicon
# revision's post-boot state, carried as a `rev=<model>` arg token (see the
# multirev commit). Keyed by file stem so re-running this script preserves them.
mooneye_rev() { # filename-stem -> rev token (empty = none)
  case "$1" in
    boot_div2-S|boot_div-S|boot_hwio-S) echo "sgb" ;;
    boot_div-dmg0|boot_hwio-dmg0|boot_regs-dmg0) echo "dmg0" ;;
    boot_regs-mgb)               echo "mgb" ;;
    boot_div-A|boot_regs-A)      echo "agb" ;;
    boot_div-cgb0)               echo "cgb0" ;;
    unused_hwio-C|boot_div-cgbABCDE|boot_hwio-C) echo "cgb" ;;
    *)                           echo "" ;;
  esac
}
{
  echo "# mooneye-test-suite (Fibonacci magic registers). Mooneye uses an"
  echo "# internal cycle cap; --frames is ignored. boot_* may need --real-bios."
  if [ -d "$mn" ]; then
    while IFS= read -r rom; do
      stem="$(basename "$rom" .gb)"
      rel="${rom#"$ROMS"/}"
      rev="$(mooneye_rev "$stem")"
      for m in $(classify_modes "$stem"); do
        if [ -n "$rev" ]; then echo "$rel|$m|mooneye|$rom|rev=$rev"; else echo "$rel|$m|mooneye|$rom"; fi
      done
    done < <(find "$mn" \( -path '*/acceptance/*' -o -path '*/emulator-only/*' -o -path '*/misc/*' -o -path '*/madness/*' \) -name '*.gb' | sort)
  fi
} > "$OUT/mooneye.manifest"
echo "  mooneye:   $(grep -vc '^#' "$OUT/mooneye.manifest") cases"

# --- mooneye-test-suite-wilbertpol (2016-era 0xED done-marker) --------------
# A second Mooneye suite (extra PPU/serial/timer timing tests). Same Fibonacci
# register check but the done-marker is the illegal opcode 0xED (grading
# `mooneye_ed`), NOT `LD B,B`. Device suffixes map like the base mooneye suite;
# -sgb*/-mgb per the wilbertpol naming. manual-only/sprite_priority is the sole
# screenshot test (DMG + CGB-compat refs). We cover acceptance/, emulator-only/,
# misc/, madness/ (auto-gradable); logic-analysis/ and manual-only/ .gb are not
# register-gradable (analog captures / need input), so only the two PNGs there.
# wilbertpol adds plain `-dmg`/`-cgb`/`-G` suffixes the base classifier does not
# handle; use a wilbertpol-local classifier so the base mooneye.manifest stays
# byte-identical. -G = original Game Boy (DMG family) -> DMG.
classify_modes_wp() { # filename-stem -> echo space-separated modes (empty = skip)
  local s="$1"
  case "$s" in
    *-sgb|*-sgb2)                                    echo "" ;;       # SGB: not modeled
    *-dmg|*-dmg0|*-dmgABC|*-dmgABCmgb|*-mgb|*-S|*-GS|*-G) echo "dmg" ;;
    *-cgb|*-cgb0|*-cgbABCDE|*-C|*-A)                 echo "cgb" ;;
    *)                                               echo "dmg cgb" ;;
  esac
}
wp="$ROMS/mooneye-test-suite-wilbertpol"
{
  echo "# mooneye-test-suite-wilbertpol (0xED done-marker; grading mooneye_ed)."
  if [ -d "$wp" ]; then
    while IFS= read -r rom; do
      stem="$(basename "$rom" .gb)"
      rel="${rom#"$ROMS"/}"
      for m in $(classify_modes_wp "$stem"); do
        echo "$rel|$m|mooneye_ed|$rom"
      done
    done < <(find "$wp" \( -path '*/acceptance/*' -o -path '*/emulator-only/*' -o -path '*/misc/*' -o -path '*/madness/*' \) -name '*.gb' | sort)
    # screenshot test (sprite_priority): DMG + CGB-compat "common palette" refs.
    sp="$wp/manual-only/sprite_priority.gb"
    [ -f "$sp" ] && [ -f "$wp/manual-only/sprite_priority-dmg.png" ] && \
      echo "mooneye-test-suite-wilbertpol/manual-only/sprite_priority|dmg|png|$sp|$wp/manual-only/sprite_priority-dmg.png"
    [ -f "$sp" ] && [ -f "$wp/manual-only/sprite_priority-cgb.png" ] && \
      echo "mooneye-test-suite-wilbertpol/manual-only/sprite_priority|cgb|png|$sp|$wp/manual-only/sprite_priority-cgb.png"
  fi
} > "$OUT/mooneye_wilbertpol.manifest"
echo "  mooneye_wilbertpol: $(grep -vc '^#' "$OUT/mooneye_wilbertpol.manifest") cases"

# --- age-test-roms (Gekkio-style: LD B,B + Fibonacci, else screenshot) ------
# age uses the modern `LD B,B` (0x40) marker (per its howto) -> grading
# `mooneye`. Screenshot tests use device-suffixed PNG refs. Device/mode mapping
# from the file-name suffix tokens:
#   -dmg*    -> DMG        (also present in -dmgC-cgb* combos: that ROM is a
#                           no-CGB-flag ROM that runs on DMG *and* CGB-compat)
#   -cgb*    -> CGB
#   -ncm*    -> CGB in non-CGB (DMG-compat) mode: a DMG ROM on CGB hardware.
#              rustyboi runs a no-CGB-flag cart in `cgb` mode as CGB-compat, so
#              ncm PNGs are graded in `cgb` mode.
#   -ds      -> double-speed variant ROM (CGB only)
# For the register (.gb, non-screenshot) tests we emit one case per compatible
# device family named in the file (dmg and/or cgb).
age="$ROMS/age-test-roms"
age_reg_modes() { # stem -> space-separated modes for register-graded .gb
  local s="$1" modes=""
  case "$s" in *dmg*) modes="dmg" ;; esac
  case "$s" in *cgb*|*ncm*) modes="$modes cgb" ;; esac
  # ROMs with neither token (bare m3-bg-*.gb, -ds, -nocgb) are screenshot-only;
  # emit nothing here.
  echo "$modes"
}
{
  echo "# age-test-roms (CGB timing; LD B,B + Fibonacci registers, else PNG)."
  echo "# Register tests: grading mooneye (0x40). Screenshot tests: grading png."
  if [ -d "$age" ]; then
    # Register-graded .gb: those whose sibling has NO matching PNG and whose name
    # carries a device token. Screenshot .gb are handled separately below.
    while IFS= read -r rom; do
      stem="$(basename "$rom" .gb)"
      dir="$(dirname "$rom")"
      rel="${rom#"$ROMS"/}"
      # A ROM is screenshot-graded iff a PNG exists in its dir sharing the stem
      # prefix up to the first device token; detect by any *.png in that dir.
      if ls "$dir"/*.png >/dev/null 2>&1; then
        continue  # screenshot dir; handled in the PNG pass
      fi
      for m in $(age_reg_modes "$stem"); do
        echo "$rel|$m|mooneye|$rom"
      done
    done < <(find "$age" -name '*.gb' | sort)

    # Screenshot pass: for every PNG, pick the ROM in its dir and the mode from
    # the PNG's own suffix token. PNG naming: <romstem>-<device>.png where the
    # ROM is the longest .gb stem that is a prefix of the PNG stem.
    while IFS= read -r png; do
      dir="$(dirname "$png")"
      pstem="$(basename "$png" .png)"
      # Choose the ROM whose stem is the longest prefix of the PNG stem.
      rom=""; best=0
      for cand in "$dir"/*.gb; do
        [ -e "$cand" ] || continue
        cstem="$(basename "$cand" .gb)"
        case "$pstem" in
          "$cstem"|"$cstem"-*)
            if [ "${#cstem}" -gt "$best" ]; then best="${#cstem}"; rom="$cand"; fi ;;
        esac
      done
      [ -n "$rom" ] || { echo "  WARN: no ROM for $png" >&2; continue; }
      # Mode from the PNG device token.
      case "$pstem" in
        *dmg*) mode="dmg" ;;
        *ncm*|*cgb*) mode="cgb" ;;
        *) mode="cgb" ;;
      esac
      relrom="$rom"
      id="age-test-roms/$(basename "$dir")/$pstem"
      echo "$id|$mode|png|$relrom|$png"
    done < <(find "$age" -name '*.png' | sort)
  fi
} > "$OUT/age.manifest"
echo "  age:       $(grep -vc '^#' "$OUT/age.manifest") cases"

# --- cgb-acid-hell (exceed-docboy PPU screen; CGB, png) ---------------------
cah="$ROMS/cgb-acid-hell"
{
  echo "# cgb-acid-hell (CGB PPU reference screen; docboy FAILS this). --frames 60"
  [ -f "$cah/cgb-acid-hell.gbc" ] && [ -f "$cah/cgb-acid-hell.png" ] && \
    echo "cgb-acid-hell|cgb|png|$cah/cgb-acid-hell.gbc|$cah/cgb-acid-hell.png"
} > "$OUT/cgb_acid_hell.manifest"
echo "  cgb_acid_hell: $(grep -vc '^#' "$OUT/cgb_acid_hell.manifest") cases"

# --- PNG-screenshot mini-suites --------------------------------------------
# scribbltests / turtle-tests / little-things-gb / bully / strikethrough.
# Each ROM lives in its own dir with a shipped reference PNG. PNG suffix encodes
# the device: -dmg / -cgb / -cgb-dmg (matches both) / bare. We emit a dmg case
# for a -dmg (or -cgb-dmg or bare) ref and a cgb case for a -cgb (or -cgb-dmg or
# bare) ref. ROMs whose only ref lacks a device suffix are run on both.
emit_png_dir() { # suite_label  rom.gb  [ref.png ...]
  local label="$1" rom="$2"; shift 2
  [ -f "$rom" ] || return 0
  local stem; stem="$(basename "$rom" .gb)"
  for ref in "$@"; do
    [ -f "$ref" ] || continue
    local rp; rp="$(basename "$ref" .png)"
    case "$rp" in
      *-cgb-dmg|*-dmg-cgb) echo "$label/$stem|dmg|png|$rom|$ref"; echo "$label/$stem|cgb|png|$rom|$ref" ;;
      *-dmg)               echo "$label/$stem|dmg|png|$rom|$ref" ;;
      *-cgb)               echo "$label/$stem|cgb|png|$rom|$ref" ;;
      *)                   echo "$label/$stem|dmg|png|$rom|$ref"; echo "$label/$stem|cgb|png|$rom|$ref" ;;
    esac
  done
}

# scribbltests: per-subdir ROM + ref(s). fairylake/winpos have NO refs (skip).
# statcount: ref `statcount_auto-cgb-dmg.png` (underscore) pairs with the ROM
# `statcount-auto.gb` (hyphen) and needs ~270 frames; handled explicitly.
scr="$ROMS/scribbltests"
{
  echo "# scribbltests (PPU screenshots). statcount_auto needs ~270 frames."
  if [ -d "$scr" ]; then
    for sub in "$scr"/*/; do
      [ -d "$sub" ] || continue
      for rom in "$sub"*.gb; do
        [ -e "$rom" ] || continue
        stem="$(basename "$rom" .gb)"
        # Match refs sharing the stem with either '-' or '_' word separators
        # (statcount-auto.gb <-> statcount_auto-*.png). Only add the alt glob when
        # it actually differs, else each ref would be listed twice.
        alt="${stem//-/_}"
        refs=("$sub$stem"-*.png)
        [ "$alt" != "$stem" ] && refs+=("$sub$alt"-*.png)
        emit_png_dir "scribbltests" "$rom" "${refs[@]}"
      done
    done
  fi
} > "$OUT/scribbltests.manifest"
echo "  scribbltests: $(grep -vc '^#' "$OUT/scribbltests.manifest") cases"

# turtle-tests
tur="$ROMS/turtle-tests"
{
  echo "# turtle-tests (window Y-trigger PPU screenshots). --frames 60"
  if [ -d "$tur" ]; then
    for sub in "$tur"/*/; do
      for rom in "$sub"*.gb; do
        [ -e "$rom" ] || continue
        stem="$(basename "$rom" .gb)"
        # ref shares the exact stem (no device suffix) -> run both DMG and CGB.
        emit_png_dir "turtle-tests" "$rom" "$sub$stem.png"
      done
    done
  fi
} > "$OUT/turtle_tests.manifest"
echo "  turtle_tests: $(grep -vc '^#' "$OUT/turtle_tests.manifest") cases"

# little-things-gb: firstwhite (auto), tellinglys (needs button input -> flag).
# firstwhite turns the LCD off after its result screen (no LD B,B), so it needs
# the flat-budget `png_fixed` grading, not the LD-B,B `png` path.
ltg="$ROMS/little-things-gb"
{
  echo "# little-things-gb PPU screenshots. tellinglys needs button input (see notes)."
  if [ -d "$ltg" ]; then
    for ref in "$ltg"/firstwhite-*.png; do
      [ -e "$ref" ] || continue
      echo "little-things-gb/firstwhite|dmg|png_fixed|$ltg/firstwhite.gb|$ref"
      echo "little-things-gb/firstwhite|cgb|png_fixed|$ltg/firstwhite.gb|$ref"
    done
    # tellinglys requires emulated button presses; still emitted so the number is
    # explicit (it will fail without input injection -- documented as feature-work).
    emit_png_dir "little-things-gb" "$ltg/tellinglys.gb" "$ltg"/tellinglys-*.png
  fi
} > "$OUT/little_things_gb.manifest"
echo "  little_things_gb: $(grep -vc '^#' "$OUT/little_things_gb.manifest") cases"

# bully (single ROM; both DMG and CGB per the bully.png ref, which is the CGB
# result -- the howto notes DMG fails with "Bad Echo RAM Reads").
bly="$ROMS/bully"
{
  echo "# bully (all-device conformance screen). bully.png is the CGB result."
  [ -f "$bly/bully.gb" ] && [ -f "$bly/bully.png" ] && {
    echo "bully|dmg|png|$bly/bully.gb|$bly/bully.png"
    echo "bully|cgb|png|$bly/bully.gb|$bly/bully.png"
  }
} > "$OUT/bully.manifest"
echo "  bully:     $(grep -vc '^#' "$OUT/bully.manifest") cases"

# strikethrough
stk="$ROMS/strikethrough"
{
  echo "# strikethrough (PPU screenshot). --frames 60"
  emit_png_dir "strikethrough" "$stk/strikethrough.gb" "$stk"/strikethrough-*.png
} > "$OUT/strikethrough.manifest"
echo "  strikethrough: $(grep -vc '^#' "$OUT/strikethrough.manifest") cases"

# --- same-suite non-APU (ppu/dma/interrupt = mooneye; sgb needs SGB) --------
# SameSuite uses the `LD B,B` (0x40) marker + Fibonacci registers -> grading
# `mooneye`. The sgb/ tests need Super Game Boy hardware (not modeled): emitted
# as a separate manifest and expected to fail (documented feature-work).
ss="$ROMS/same-suite"
{
  echo "# same-suite non-APU (ppu/dma/interrupt). grading mooneye (0x40)."
  if [ -d "$ss" ]; then
    for rom in $(find "$ss/ppu" "$ss/dma" "$ss/interrupt" -name '*.gb' 2>/dev/null | sort); do
      rel="${rom#"$ROMS"/}"
      # No device tokens in these names; SameSuite targets CGB (CPU CGB E).
      echo "$rel|cgb|mooneye|$rom"
    done
  fi
} > "$OUT/samesuite_nonapu.manifest"
echo "  samesuite_nonapu: $(grep -vc '^#' "$OUT/samesuite_nonapu.manifest") cases"

{
  echo "# same-suite sgb/ (needs Super Game Boy hardware; NOT modeled)."
  if [ -d "$ss/sgb" ]; then
    for rom in $(find "$ss/sgb" -name '*.gb' 2>/dev/null | sort); do
      rel="${rom#"$ROMS"/}"
      echo "$rel|dmg|mooneye|$rom"
    done
  fi
} > "$OUT/samesuite_sgb.manifest"
echo "  samesuite_sgb: $(grep -vc '^#' "$OUT/samesuite_sgb.manifest") cases"

# --- rtc3test / mbc3-tester (MBC3 RTC) --------------------------------------
# rtc3test needs button navigation to select each subtest + an RTC clock; it
# cannot be auto-graded without input injection. mbc3-tester loops and shows a
# result screen with no input required, so it IS auto-gradable via PNG.
rtc="$ROMS/rtc3test"
{
  echo "# rtc3test (MBC3 RTC). NEEDS button-input navigation -- see notes; will"
  echo "# fail without input injection. Emitted for an explicit number only."
  if [ -d "$rtc" ]; then
    emit_png_dir "rtc3test-basic" "$rtc/rtc3test.gb" "$rtc"/rtc3test-basic-tests-*.png
  fi
} > "$OUT/rtc3test.manifest"
echo "  rtc3test:  $(grep -vc '^#' "$OUT/rtc3test.manifest") cases"

mbc3="$ROMS/mbc3-tester"
{
  echo "# mbc3-tester (MBC3 bank/RTC; auto result screen after 40 frames)."
  if [ -d "$mbc3" ]; then
    emit_png_dir "mbc3-tester" "$mbc3/mbc3-tester.gb" "$mbc3"/mbc3-tester-*.png
  fi
} > "$OUT/mbc3_tester.manifest"
echo "  mbc3_tester: $(grep -vc '^#' "$OUT/mbc3_tester.manifest") cases"

echo "done."

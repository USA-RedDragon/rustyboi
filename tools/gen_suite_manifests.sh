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
{
  echo "# mooneye-test-suite (Fibonacci magic registers). Mooneye uses an"
  echo "# internal cycle cap; --frames is ignored. boot_* may need --real-bios."
  if [ -d "$mn" ]; then
    while IFS= read -r rom; do
      stem="$(basename "$rom" .gb)"
      rel="${rom#"$ROMS"/}"
      for m in $(classify_modes "$stem"); do
        echo "$rel|$m|mooneye|$rom"
      done
    done < <(find "$mn" \( -path '*/acceptance/*' -o -path '*/emulator-only/*' -o -path '*/misc/*' -o -path '*/madness/*' \) -name '*.gb' | sort)
  fi
} > "$OUT/mooneye.manifest"
echo "  mooneye:   $(grep -vc '^#' "$OUT/mooneye.manifest") cases"

echo "done."

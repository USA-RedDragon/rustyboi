; licensee_boot_div.agbcompat_l33.mooneye — the CGB boot ROM's licensee
; branch, observed as post-boot DIV on AGB silicon.
;
; A DMG-compatibility cartridge (non-Nintendo header) booted on AGB. The
; cart is NOT CGB-flagged, so the boot ROM takes its DMG-compat colourisation
; path and consults the header licensee bytes to decide whether to run the
; title-hash palette lookup. Doing so costs thousands of T cycles, which shows
; up directly in the divider the game inherits.
;
; This variant: old licensee $33 sends the boot ROM to the NEW licensee code at
; $0144-$0145, which here is $00 $00 — not "01", so still not Nintendo and the
; lookup is still skipped. This is the arm an emulator is most likely to get
; wrong by treating $33 itself as the Nintendo marker.
;
; The AGB image, which hands off 4 T cycles later than CGB on both arms — not
; enough to move the DIV byte, so the same expectation, on its own silicon.
;
; Expected DIV $26, derived by executing agb_boot.bin against this exact
; cartridge image — never from rustyboi's own hand-off constants. Full
; derivation, the title-dependence measurements and the failure-code legend are
; in include/boot_licensee_div.inc.
;
; The header bytes below are the independent variable and are written by
; `rgbfix` from the `agbcompat_l33` model token in the Makefile, so the
; four variants of this revision are otherwise byte-identical. The ROM
; re-reads them at run time and fails loudly if the build fixed the wrong cell.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "boot_licensee_div.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    ; expected DIV, expected $014B, expected $0144, expected $0145
    licensee_boot_div_test $26, $33, $00, $00

; licensee_boot_div.cgbcompat_l00.mooneye — the CGB boot ROM's licensee
; branch, observed as post-boot DIV on CPU CGB-A..C silicon.
;
; A DMG-compatibility cartridge (non-Nintendo header) booted on CPU CGB-A..C. The
; cart is NOT CGB-flagged, so the boot ROM takes its DMG-compat colourisation
; path and consults the header licensee bytes to decide whether to run the
; title-hash palette lookup. Doing so costs thousands of T cycles, which shows
; up directly in the divider the game inherits.
;
; This variant: old licensee $00: the boot ROM's `cp $33` fails and its
; `cp $01` fails, so the title-hash lookup is skipped entirely.
;
; The CGB-A..E image. CGB, CGB-D/E and AGB all hand off within 8 T cycles of
; each other here, so they share an expected DIV byte; each is still pinned
; separately so a revision that moved would be caught on its own row.
;
; Expected DIV $26, derived by executing cgb_boot.bin against this exact
; cartridge image — never from rustyboi's own hand-off constants. Full
; derivation, the title-dependence measurements and the failure-code legend are
; in include/boot_licensee_div.inc.
;
; The header bytes below are the independent variable and are written by
; `rgbfix` from the `cgbcompat_l00` model token in the Makefile, so the
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
    licensee_boot_div_test $26, $00, $00, $00

; licensee_boot_div.cgbecompat_l33n01.mooneye — the CGB boot ROM's licensee
; branch, observed as post-boot DIV on CPU CGB-D/E silicon.
;
; A DMG-compatibility cartridge (Nintendo header) booted on CPU CGB-D/E. The
; cart is NOT CGB-flagged, so the boot ROM takes its DMG-compat colourisation
; path and consults the header licensee bytes to decide whether to run the
; title-hash palette lookup. Doing so costs thousands of T cycles, which shows
; up directly in the divider the game inherits.
;
; This variant: old licensee $33 defers to the new licensee code, which is the
; ASCII "01" that means Nintendo — the second route into the same title-hash
; lookup, reached through a different header field.
;
; The CPU-CGB-D/E image. Byte-identical hand-off timing to cgb_boot.bin on
; this path; pinned separately so the two cannot silently diverge.
;
; Expected DIV $37, derived by executing cgbE_boot.bin against this exact
; cartridge image — never from rustyboi's own hand-off constants. Full
; derivation, the title-dependence measurements and the failure-code legend are
; in include/boot_licensee_div.inc.
;
; The header bytes below are the independent variable and are written by
; `rgbfix` from the `cgbecompat_l33n01` model token in the Makefile, so the
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
    licensee_boot_div_test $37, $33, '0', '1'

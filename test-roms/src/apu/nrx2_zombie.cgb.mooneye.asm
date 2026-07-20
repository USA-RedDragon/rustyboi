; nrx2_zombie.cgb.mooneye — an NRx2 write to a PLAYING square channel drags the
; current volume through the "zombie mode" transform, and CPU-CGB-C silicon
; applies that transform TWICE (through an $FF intermediate), not once. CGB
; build pinned to the default cgb04c / CPU-CGB-C model, single speed throughout.
;
; SameBoy (Core/apu.c, MIT) forks on `gb->model <= GB_MODEL_CGB_C`: at or below
; CGB-C it runs `_nrx2_glitch` twice (old -> $FF, then $FF -> new), above it
; once. Core/model.h orders CGB_C=$203 < CGB_D=$204 < CGB_E=$205 < AGB_A=$207,
; so the double application covers all DMG models and CGB revisions 0/A/B/C
; while CGB-D/E and AGB take the single one. This ROM is the CGB-C (double)
; side; nrx2_zombie.cgbe.mooneye is the CGB-D/E (single) side, and the two
; differ on both graded channels, so a build that ignores the fork fails one of
; them whichever way it guesses.
;
; Observable: PCM12 (Pan Docs, "Audio Registers"; $FF76, CGB only) — CH1 in the
; low nibble, CH2 in the high nibble, each the channel's current digital output.
; There is deliberately no DMG variant: on DMG the same double application
; happens but no register exposes the envelope volume, so the DMG side of this
; fork is a hardware-bench item (scope the DAC output), not a ROM.
;
; CH1 triggers at NR12=$A8 (volume 10) and CH2 at NR22=$98 (volume 9); both are
; then rewritten to $10 while playing. Double application gives 5 and 6; single
; application would give 6 and 7. Full derivation in apu_nrx2_zombie.inc.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "apu.inc"
INCLUDE "apu_nrx2_zombie.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    nrx2_zombie_test 5, 6      ; CGB-C: transform applied twice

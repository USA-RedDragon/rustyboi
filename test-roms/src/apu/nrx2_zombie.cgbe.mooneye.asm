; nrx2_zombie.cgbe.mooneye — the CPU-CGB-D/E side of the NRx2 "zombie mode"
; revision fork: on silicon newer than CGB-C an NRx2 write to a playing square
; channel applies the volume transform exactly ONCE. Same CGB cart image as
; nrx2_zombie.cgb.mooneye, run on the CGB-D/E model; single speed throughout.
;
; SameBoy (Core/apu.c, MIT) forks on `gb->model <= GB_MODEL_CGB_C`: at or below
; CGB-C it runs `_nrx2_glitch` twice (old -> $FF, then $FF -> new), above it
; once. Core/model.h orders CGB_C=$203 < CGB_D=$204 < CGB_E=$205 < AGB_A=$207,
; so CGB-D/E and AGB take this single application while all DMG models and CGB
; revisions 0/A/B/C take the double one. SameSuite's own APU tests are captured
; on CPU-CGB-E, which is why the zombie rows of samesuite_apu carry rev=cgbe.
;
; Observable: PCM12 (Pan Docs, "Audio Registers"; $FF76, CGB only) — CH1 in the
; low nibble, CH2 in the high nibble, each the channel's current digital output.
;
; CH1 triggers at NR12=$A8 (volume 10) and CH2 at NR22=$98 (volume 9); both are
; then rewritten to $10 while playing. Single application gives 6 and 7; double
; application would give 5 and 6. Full derivation in apu_nrx2_zombie.inc.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "apu.inc"
INCLUDE "apu_nrx2_zombie.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    nrx2_zombie_test 6, 7      ; CGB-D/E: transform applied once

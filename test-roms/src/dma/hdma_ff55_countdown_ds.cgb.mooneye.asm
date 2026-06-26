; hdma_ff55_countdown_ds.cgb.mooneye — HBlank-DMA FF55 countdown, double speed.
;
; Same invariant as hdma_ff55_countdown_ss (one $10 block per HBlank; full
; 06..00,FF readback countdown, one step per scanline) after the KEY1/STOP
; speed switch. Double speed is the regressing case: the closed-form HDMA
; period rise lands on the odd sub-dot, one dot before the mode-0 STAT commit,
; so the STAT-3->0 fallback re-fired the same edge and burned two blocks per
; HBlank.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "hdma_ff55_countdown.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    ; CGB speed switch: joypad lines idle, arm KEY1, STOP.
    ld a, $30
    ldh [rP1], a
    ld a, $01
    ldh [rKEY1], a
    stop
    ; Confirm double speed engaged before testing anything.
    ldh a, [rKEY1]
    and $80
    jr nz, .switched
    test_failure
.switched:
    hdma_ff55_countdown_test

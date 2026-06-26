; hdma_ff55_countdown.inc — shared body for the HBlank-DMA FF55 countdown ROMs.
;
; Pan Docs ("LCD VRAM DMA Transfers", CGB): an HBlank DMA transfers exactly one
; $10-byte block per HBlank, and reading FF55 returns the remaining length
; (blocks minus 1, bit 7 clear while active); when the transfer completes it
; reads $FF. So an 8-block HBlank DMA armed during VBlank must expose the full
; readback countdown 06,05,04,03,02,01,00,FF — one step per scanline, each value
; (including the $00 "one block remaining" step) persisting a full line.
;
; The ROM logs every FF55 transition together with the LY it happened on, then
; asserts the exact value sequence AND that consecutive transition LYs differ by
; exactly 1. A double-fired block (two blocks in one HBlank) breaks both: values
; step by 2 (skipping $00 for an even block count) and paired transitions share
; a line.
;
; `hdma_ff55_countdown_test` is invoked once per ROM after any speed-switch
; preamble. Uses WRAM C000-C07F as the DMA source and C100+ as the log.

IF !DEF(HDMA_FF55_COUNTDOWN_INC)
DEF HDMA_FF55_COUNTDOWN_INC EQU 1

DEF HFC_LOG EQU $C100

MACRO hdma_ff55_countdown_test
    ; Source buffer: C000-C07F, arbitrary pattern.
    ld hl, $C000
    ld c, $80
    ld a, $A5
.fill\@:
    ld [hl+], a
    inc a
    dec c
    jr nz, .fill\@

    ; LCD on, BG on (post-boot state made explicit; never toggled off).
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; Fresh VBlank, then arm an 8-block HBlank DMA C000 -> 8800 from mode 1:
    ; the first block fires at line 0's HBlank.
.not144\@:
    ldh a, [rLY]
    cp 144
    jr z, .not144\@
.to144\@:
    ldh a, [rLY]
    cp 144
    jr nz, .to144\@
    ld a, $C0
    ldh [rHDMA1], a
    xor a
    ldh [rHDMA2], a
    ldh [rHDMA4], a
    ld a, $88
    ldh [rHDMA3], a
    ld a, HDMA5F_HBLANK | 7
    ldh [rHDMA5], a

    ; Active readback: bit 7 clear, 7 blocks pending after the first -> $07.
    ldh a, [rHDMA5]
    cp $07
    jp nz, .fail\@

    ; Poll FF55; log (value, LY) at every transition until $FF or timeout.
    ld hl, HFC_LOG
    ld b, $07               ; previous value
    ld c, LOW(rHDMA5)
    ld de, 0                ; timeout counter
.poll\@:
    inc de
    ld a, d
    or e
    jp z, .fail\@           ; countdown never completed
    ldh a, [c]
    cp b
    jr z, .poll\@
    ld b, a
    ld a, l
    cp LOW(HFC_LOG + 24)
    jp z, .fail\@           ; too many transitions
    ld a, b
    ld [hl+], a
    ldh a, [rLY]
    ld [hl+], a
    ld a, b
    cp $FF
    jr nz, .poll\@

    ; Exactly 8 transitions (16 log bytes).
    ld a, l
    cp LOW(HFC_LOG + 16)
    jp nz, .fail\@

    ; Values are exactly 06,05,04,03,02,01,00,FF...
    ld hl, HFC_LOG
    ld b, 8
    ld a, $06
.values\@:
    cp [hl]
    jp nz, .fail\@
    inc hl
    inc hl
    dec a                   ; $06,$05,...,$00, then $FF via underflow
    dec b
    jr nz, .values\@

    ; ...and each transition lands exactly one scanline after the previous
    ; (so every value, $00 included, persisted a full line).
    ld hl, HFC_LOG + 1
    ld a, [hl]              ; first transition's LY
    ld b, 7
.lines\@:
    inc hl
    inc hl
    inc a
    cp [hl]
    jp nz, .fail\@
    dec b
    jr nz, .lines\@

    test_success
.fail\@:
    test_failure
ENDM

ENDC

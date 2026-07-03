; hdma_ff55_cancel_after_block.cgb.mooneye — a mid-HBlank FF55=$00 cancel written
; AFTER this line's HBlank-DMA block already fired must simply STOP the transfer:
; no further block is copied and FF55 reads back 0x80|written.
;
; Pan Docs ("FF55 — HDMA5", CGB): "It is also possible to terminate an active
; HBlank transfer by writing zero to Bit 7 of FF55. In that case reading from
; FF55 will return how many $10 blocks remained (minus 1) in the lower 7 bits,
; but Bit 7 will be read as 1." and "Reading Bit 7 of FF55 can be used to confirm
; if the DMA transfer is active (1=Not Active, 0=Active) [...] after manually
; terminating a HBlank Transfer." So after the cancel the transfer stops and no
; more $10 blocks are moved. (SameSuite dma/hdma_lcd_off pins that the lower bits
; read back the WRITTEN length, not the remaining count — so a cancel value of
; $2C reads back 0x80|$2C = $AC.)
;
; The regressing "one block per HBlank" engine never marked the period serviced
; for a block that fired through the single-speed STAT-3->0 fallback (period
; handed off to None a hair before the fire). So a same-HBlank FF55=00 cancel,
; written a few M-cycles after that block ran, hit the disable-vs-m0-edge race
; branch with `!block_done` still true: it fired a SPURIOUS extra block AND
; dropped the disable, leaving HDMA streaming every following HBlank. This is the
; shape of Pokemon Crystal's Elm's-lab cancel.
;
; Oracle (high-entropy, every failure mode distinct). Source blocks carry
; distinct markers $B1/$B2/$B3; the three destination blocks are pre-filled with
; distinct sentinels $E1/$E2/$E3 (LCD off) so a never-copied block is not
; confused with reset $00. The ROM arms a 3-block HBlank DMA from VBlank, spins
; on FF55 until the FIRST block fires (readback $02 -> $01, a few M-cycles into
; line 0's HBlank), writes FF55=$2C, waits two frames for any dropped-disable
; streaming to land, then in VBlank asserts:
;   FF55 immediately after cancel == $AC  (stopped, WRITTEN length latched:
;                                          not $01/$0x active, not $FF completed)
;   block0 dest == $B1  (the pre-cancel block DID copy — guards over-regression
;                        where "everything stops" would false-pass)
;   block1 dest == $E2  (untouched: no spurious extra block this HBlank)
;   block2 dest == $E3  (untouched: disable not dropped, no streaming)

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

DEF SRC        EQU $C000
DEF DST        EQU $8800
DEF CANCEL_VAL EQU $2C                         ; bit7=0 -> cancel; low bits latch
DEF EXPECT_FF55 EQU $80 | CANCEL_VAL           ; $AC

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    ; LCD off for VRAM setup.
    xor a
    ldh [rLCDC], a

    ; Source: block0 $C000-$C00F = $B1, block1 $C010-$C01F = $B2,
    ; block2 $C020-$C02F = $B3 (distinct per-block markers).
    ld hl, SRC
    ld a, $B1
    call FillBlock
    ld a, $B2
    call FillBlock
    ld a, $B3
    call FillBlock

    ; Destination pre-fill (VRAM readable, LCD off): block0 $8800 = $E1,
    ; block1 $8810 = $E2, block2 $8820 = $E3 (distinct sentinels).
    ld hl, DST
    ld a, $E1
    call FillBlock
    ld a, $E2
    call FillBlock
    ld a, $E3
    call FillBlock

    ; HDMA C000 -> 8800.
    ld a, HIGH(SRC)
    ldh [rHDMA1], a
    xor a
    ldh [rHDMA2], a
    ldh [rHDMA4], a
    ld a, HIGH(DST)
    ldh [rHDMA3], a

    ; LCD on, BG on ($8000 tiles). Never toggled off again before the reads.
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; Fresh VBlank.
.not144:
    ldh a, [rLY]
    cp 144
    jr z, .not144
.to144:
    ldh a, [rLY]
    cp 144
    jr nz, .to144

    ; Arm a 3-block HBlank DMA. FF55 immediately reads $02 (bit7 clear, 2 left
    ; after the pending first block).
    ld a, HDMA5F_HBLANK | 2
    ldh [rHDMA5], a
    ldh a, [rHDMA5]
    cp $02
    jp nz, TestFail

    ; Spin until the first block fires (readback steps $02 -> $01): we are then a
    ; few M-cycles into line 0's HBlank, in the "space" between blocks.
    ld b, $02
.spin:
    ldh a, [rHDMA5]
    cp b
    jr z, .spin

    ; Cancel, same HBlank, a few M-cycles after the block fired.
    ld a, CANCEL_VAL
    ldh [rHDMA5], a
    ; Capture the immediate post-cancel readback.
    ldh a, [rHDMA5]
    ld c, a                     ; c = FF55 after cancel

    ; Let two frames pass so any dropped-disable streaming copies block1/block2.
    call WaitFrame
    call WaitFrame

    ; Read the three destination blocks during VBlank (VRAM accessible).
.rv1:
    ldh a, [rLY]
    cp 145
    jr nz, .rv1
    ld a, [DST + $00]           ; block0
    ld d, a
    ld a, [DST + $10]           ; block1
    ld e, a
    ld a, [DST + $20]           ; block2
    ld h, a

    ; Grade.
    ld a, c
    cp EXPECT_FF55
    jp nz, TestFail
    ld a, d
    cp $B1                      ; block0 copied
    jp nz, TestFail
    ld a, e
    cp $E2                      ; block1 untouched
    jp nz, TestFail
    ld a, h
    cp $E3                      ; block2 untouched
    jp nz, TestFail
TestPass:
    test_success
TestFail:
    test_failure

; Fill 16 bytes at HL with A, advancing HL past the block.
FillBlock:
    push bc
    ld b, $10
.f:
    ld [hl+], a
    dec b
    jr nz, .f
    pop bc
    ret

; Block until the next LY=144 VBlank edge.
WaitFrame:
    push af
.n:
    ldh a, [rLY]
    cp 144
    jr z, .n
.t:
    ldh a, [rLY]
    cp 144
    jr nz, .t
    pop af
    ret

; hdma_ff55_ssds_stop_after_block.cgb.mooneye — a single->double-speed STOP
; executed mid-HBlank, AFTER this line's HBlank-DMA block already fired, must NOT
; re-fire the already-serviced block. Right after the switch FF55 must still show
; that block's successor pending (one block short of the count), not a premature
; extra step.
;
; Pan Docs ("FF55 — HDMA5", CGB): "The HBlank DMA transfers $10 bytes of data
; during each HBlank" — exactly one block per HBlank. Pan Docs ("KEY1", CGB):
; "Upon halting the CPU (using the halt instruction), the transfer will also be
; halted and will be resumed only when the CPU resumes execution"; the STOP
; speed-switch halts the CPU for its ~0x20000-cycle window, so it suspends HDMA
; the same way. So a speed-switch STOP taken after a line's block has run cannot
; add a second block to that HBlank: block N+1 still waits for the NEXT HBlank,
; and FF55 read immediately after the switch shows N+1 pending.
;
; The regressing "one block per HBlank" engine never marked the period serviced
; for a block that fired through the single-speed STAT-3->0 fallback, so the
; SS->DS STOP's synchronous-fire branch (guarded by `!hdma_block_done_this_period`)
; wrongly re-fired the already-serviced line's block during the switch, stepping
; the transfer an extra block: FF55 read $FF (completed early) instead of $01.
;
; Oracle. A 3-block HBlank DMA (source blocks $B1/$B2/$B3, dest pre-filled with
; sentinels $E1/$E2/$E3) is armed from VBlank. The ROM spins on FF55 until the
; FIRST block fires ($02 -> $01, a few M-cycles into line 0's HBlank), then arms
; KEY1 and executes STOP (SS->DS) in that same HBlank. Discriminators:
;   FF55 immediately after the switch == $01  (block1 still pending — NOT
;                                              re-fired; bug reads $FF)
;   KEY1 bit7 set                             (the speed switch really happened)
;   block0 dest == $B1                        (block0 DID copy — guards a
;                                              never-ran false pass)
;   after 3 more frames: FF55 == $FF and blocks 1/2 dest == $B2/$B3
;                                             (the transfer still completes
;                                              correctly — guards over-regression)

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

DEF SRC EQU $C000
DEF DST EQU $8800

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    xor a
    ldh [rLCDC], a

    ; Source blocks $B1/$B2/$B3.
    ld hl, SRC
    ld a, $B1
    call FillBlock
    ld a, $B2
    call FillBlock
    ld a, $B3
    call FillBlock

    ; Destination sentinels $E1/$E2/$E3.
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

    ; Idle joypad so STOP routes to the speed switch, not the button branch.
    ld a, $30
    ldh [rP1], a

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

    ; Arm a 3-block HBlank DMA.
    ld a, HDMA5F_HBLANK | 2
    ldh [rHDMA5], a
    ldh a, [rHDMA5]
    cp $02
    jp nz, TestFail

    ; Spin until the first block fires ($02 -> $01): now mid line-0 HBlank.
    ld b, $02
.spin:
    ldh a, [rHDMA5]
    cp b
    jr z, .spin

    ; Arm the switch and STOP in this same HBlank.
    ld a, $01
    ldh [rKEY1], a
    stop
    ; FF55 immediately after the switch.
    ldh a, [rHDMA5]
    ld c, a                     ; c = FF55 after switch

    ; Confirm double speed engaged.
    ldh a, [rKEY1]
    and $80
    jp z, TestFail

    ; Let three frames pass so the transfer completes normally at DS.
    call WaitFrame
    call WaitFrame
    call WaitFrame

    ; Read dest + steady FF55 in VBlank.
.rv:
    ldh a, [rLY]
    cp 145
    jr nz, .rv
    ld a, [DST + $00]
    ld d, a
    ld a, [DST + $10]
    ld e, a
    ld a, [DST + $20]
    ld h, a
    ldh a, [rHDMA5]
    ld l, a

    ; Grade.
    ld a, c
    cp $01                      ; block1 still pending after the switch
    jp nz, TestFail
    ld a, d
    cp $B1                      ; block0 copied
    jp nz, TestFail
    ld a, e
    cp $B2                      ; block1 copied (transfer completed)
    jp nz, TestFail
    ld a, h
    cp $B3                      ; block2 copied
    jp nz, TestFail
    ld a, l
    cp $FF                      ; transfer complete
    jp nz, TestFail
TestPass:
    test_success
TestFail:
    test_failure

FillBlock:
    push bc
    ld b, $10
.f:
    ld [hl+], a
    dec b
    jr nz, .f
    pop bc
    ret

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

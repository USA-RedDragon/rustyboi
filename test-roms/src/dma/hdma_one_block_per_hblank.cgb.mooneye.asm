; hdma_one_block_per_hblank.cgb.mooneye — an HBlank DMA moves EXACTLY ONE
; $10-byte block per HBlank, observed directly in the destination bytes.
;
; Pan Docs, "LCD VRAM DMA Transfers" (CGB): an HBlank DMA copies "0x10 bytes ...
; during each HBlank", one block per scanline, until the length runs out. The
; FF55 countdown ROMs pin this through the FF55 readback; this ROM pins it
; through what actually LANDS IN VRAM, the stronger observable — a countdown can
; read right while the copy engine is wrong, but the destination bytes cannot.
;
; Phase A (completion cadence): a 16-block HBlank DMA armed during VBlank runs to
; completion (FF55 -> $FF) only after 16 HBlanks, i.e. around LY 15. An engine
; that moved two blocks per HBlank would finish in 8 HBlanks, around LY 7 — so
; the completion-LY window [12,20] rejects it.
;
; Phase B (per-HBlank granularity): re-armed the same way, then cancelled (FF55
; bit7=0) at the START of line K=8 — after the HBlanks of lines 0..7 fired blocks
; 0..7, and before line 8's HBlank could fire block 8. The cancel simply stops
; the transfer, moving no further block (the sibling hdma_ff55_cancel_after_block
; pins that a cancel copies no spurious block; a plain LCD-off does flush one, so
; it is deliberately NOT used here). The frozen destination is read back in the
; following VBlank: blocks 0..7 must hold their per-block source markers
; ($A0..$A7) and block 8 must STILL HOLD THE SENTINEL ($E5):
;
;   Correct (one block per HBlank): 8 HBlanks -> 8 blocks -> block 8 untouched.
;   Two blocks per HBlank:          8 HBlanks -> 16 blocks -> block 8 already
;                                   copied as $A8, sentinel gone -> FAIL.
;
; Source ($C000): block j is 16 bytes of $A0+j, so a copied block reveals WHICH
; source block landed (guards a mis-strided copy), never just "something moved".
; Destination ($8800) is pre-filled with the sentinel $E5 (distinct from every
; source marker and from the $FF a completed transfer reads) under LCD-off, so
; an uncopied block is never confused with a blank/completed value.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

DEF SRC       EQU $C000
DEF DST       EQU $8800
DEF NBLOCKS   EQU 16
DEF ARMCODE   EQU HDMA5F_HBLANK | (NBLOCKS - 1)   ; $8F: 16-block HBlank DMA
DEF SRC_BASE  EQU $A0                             ; block j source byte = $A0 + j
DEF SENTINEL  EQU $E5
DEF FREEZE_K  EQU 8                               ; freeze at the start of line K
DEF COMP_LO   EQU 12                              ; completion-LY window
DEF COMP_HI   EQU 20

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    ; LCD off for source (WRAM) and destination (VRAM) setup.
    xor a
    ldh [rLCDC], a
    call FillSource
    call FillDestSentinel
    call ArmAddrs

    ; LCD on, BG on ($8000 tiles).
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; --- Phase A: the full 16-block transfer completes near LY 15, not LY 7. ---
    call FreshVBlank
    ld a, ARMCODE
    ldh [rHDMA5], a
    ld de, 0                    ; timeout guard
.compl:
    inc de
    ld a, d
    or e
    jp z, TestFail             ; transfer never completed
    ldh a, [rHDMA5]
    cp $FF
    jr nz, .compl
    ldh a, [rLY]
    cp COMP_LO
    jp c, TestFail             ; finished too early (blocks per HBlank > 1)
    cp COMP_HI + 1
    jp nc, TestFail            ; finished implausibly late

    ; --- Phase B: after exactly K HBlanks, exactly K blocks are in VRAM. ---
    ; A completed transfer consumed the sentinel and the HDMA address regs, so
    ; refill (LCD off) and re-arm the source/dest before the second run.
    xor a
    ldh [rLCDC], a
    call FillDestSentinel
    call ArmAddrs
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    call FreshVBlank
    ld a, ARMCODE
    ldh [rHDMA5], a
.toK:
    ldh a, [rLY]
    cp FREEZE_K
    jr nz, .toK
    xor a
    ldh [rHDMA5], a            ; cancel (bit7=0) before line K's HBlank fires block K
.tovb:
    ldh a, [rLY]
    cp 145
    jr nz, .tovb              ; VBlank: VRAM readable

    ; Blocks 0..K-1 must each carry their own source marker ($A0 + block index).
    ld hl, DST
    ld b, FREEZE_K
    ld c, SRC_BASE
.chk:
    ld a, [hl]
    cp c
    jp nz, TestFail
    ld a, l
    add $10                    ; next block (DST..DST+K*$10 stays within one page)
    ld l, a
    inc c
    dec b
    jr nz, .chk

    ; Block K must still hold the sentinel: exactly K blocks copied, no more.
    ld a, [hl]
    cp SENTINEL
    jp nz, TestFail

    test_success
TestFail:
    test_failure

; Fill 16 bytes at HL with A, advancing HL past the block. Clobbers B.
FillBlock:
    ld b, $10
.f:
    ld [hl+], a
    dec b
    jr nz, .f
    ret

; Source: NBLOCKS blocks, block j = 16 bytes of SRC_BASE + j.
FillSource:
    ld hl, SRC
    ld a, SRC_BASE
    ld c, NBLOCKS
.s:
    call FillBlock
    inc a
    dec c
    jr nz, .s
    ret

; Destination: NBLOCKS blocks of SENTINEL.
FillDestSentinel:
    ld hl, DST
    ld a, SENTINEL
    ld c, NBLOCKS
.d:
    call FillBlock
    dec c
    jr nz, .d
    ret

; Program the HDMA source/dest address registers for SRC -> DST.
ArmAddrs:
    ld a, HIGH(SRC)
    ldh [rHDMA1], a
    xor a
    ldh [rHDMA2], a
    ldh [rHDMA4], a
    ld a, HIGH(DST)
    ldh [rHDMA3], a
    ret

; Block until a fresh LY==144 VBlank edge, so the arm lands at the top of VBlank
; and the first block fires at line 0's HBlank.
FreshVBlank:
.n:
    ldh a, [rLY]
    cp 144
    jr z, .n
.t:
    ldh a, [rLY]
    cp 144
    jr nz, .t
    ret

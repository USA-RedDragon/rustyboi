; hdma_mode2_arm_no_kick.cgb.mooneye — a CGB HBlank-DMA armed during a line's
; mode 2 must NOT transfer a block on that same line; the first block waits for
; the coming mode-0 (HBlank) edge.
;
; Pan Docs ("LCD VRAM DMA Transfers"): an HBlank DMA moves exactly one $10-byte
; block per HBlank, and FF55 reads back the remaining length ($FF once done).
; Arming FF55 <- $80|(len-1) during a line's mode 2 schedules the first block to
; that SAME line's upcoming mode-0 edge — the mode-2 write itself transfers
; nothing. So a 4-block HBlank DMA armed at the very top of line 50 (mode 2)
; moves its blocks at the HBlanks of lines 50,51,52,53; reading FF55 the instant
; it returns $FF must see LY == 53.
;
; The regressing engine bracketed the FF55-arm immediate-kick against a mode-3
; anchor (`m0_time_master`) that is rebased at the mode-3 arm and thus, during
; the next line's mode 2, still holds the PREVIOUS line's mode-0 time. When that
; previous line ran a LONG mode 3 (fullscreen window + a full row of sprites, so
; its mode 0 starts past dot ~264) an early mode-2 arm landed inside the stale
; bracket and fired a spurious immediate block. The kick's one-block-per-HBlank
; marker was then wiped by the arm-line's LY-change reset, so the line's mode-0
; edge armed a SECOND block: two blocks on the arm line, so the transfer finished
; one line early and FF55 read $FF at LY == 52. Pokemon Crystal's 37-block
; tilemap transfer (Elm's-lab textbox over the fullscreen-window freeze frame)
; hit exactly this and turned its read-modify-write cancel into a 2KB GDMA over
; the displayed map.
;
; Every line is forced long (WY=0/WX=7 fullscreen window + a full row of 10
; sprites on lines 44..59). The ROM syncs to the fresh top of line 50, arms a
; 4-block HBlank DMA in the first M-cycle of mode 2, then reads FF55 until it
; returns $FF and asserts the completion line is 53, not 52.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

DEF ARM_LINE   EQU 50
DEF LEN        EQU 4
DEF ARM_VAL    EQU HDMA5F_HBLANK | (LEN - 1)
DEF EXPECT_LY  EQU ARM_LINE + LEN - 1          ; 53 (one block per HBlank)
DEF SLIDE      EQU 0                            ; mode-2 phase trim (nops)

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    ; LCD off for OAM + VRAM setup.
    xor a
    ldh [rLCDC], a

    ; HDMA source buffer C000-C0FF, arbitrary constant (contents unchecked).
    ld hl, $C000
    ld c, 0                     ; 256 bytes
    ld a, $A5
.fill:
    ld [hl+], a
    dec c
    jr nz, .fill

    ; Clear OAM (FE00-FE9F) to 0 (Y=0 parks unused sprites off-screen).
    ld hl, $FE00
    ld c, $A0
    xor a
.clroam:
    ld [hl+], a
    dec c
    jr nz, .clroam

    ; 10 sprites (8x16) on Y=60 -> screen lines 44..59, spread across X so the
    ; whole row is fetched: lengthens mode 3 for lines 44..59 (incl. line 49, the
    ; arm line's predecessor whose stale mode-0 anchor the bug reused).
    ld hl, $FE00
    ld c, 10
    ld d, 8                     ; first X
.spr:
    ld a, 60
    ld [hl+], a                 ; Y
    ld a, d
    ld [hl+], a                 ; X
    ld a, 1
    ld [hl+], a                 ; tile
    xor a
    ld [hl+], a                 ; attr
    ld a, d
    add 15
    ld d, a
    dec c
    jr nz, .spr

    ; HDMA C000 -> 8800, armed below.
    ld a, $C0
    ldh [rHDMA1], a
    xor a
    ldh [rHDMA2], a
    ldh [rHDMA4], a
    ld a, $88
    ldh [rHDMA3], a

    ; Window fullscreen (WX=7, WY=0) so every line runs a long mode 3.
    ld a, 7
    ldh [rWX], a
    xor a
    ldh [rWY], a

    ; LCD on: BG+window+sprites, 8x16, $8000 tiles.
    ld a, LCDCF_ON | LCDCF_BGON | LCDCF_OBJON | LCDCF_OBJ16 | LCDCF_WINON | LCDCF_BG8000
    ldh [rLCDC], a

    ; Sync to the FRESH top of the arm line (wait for its predecessor first so we
    ; catch the LY flip, not a mid-line entry), then arm in the first mode-2
    ; M-cycle. On a correct engine this mode-2 write transfers nothing; the first
    ; block waits for the arm line's own mode-0 edge.
.pre:
    ldh a, [rLY]
    cp ARM_LINE - 1
    jr nz, .pre
.on:
    ldh a, [rLY]
    cp ARM_LINE
    jr nz, .on
    ld a, ARM_VAL
    REPT SLIDE
    nop
    ENDR
    ldh [rHDMA5], a

    ; Read FF55 until the transfer completes ($FF); capture the completion line.
    ld de, 20000                ; timeout iterations
.wait:
    ldh a, [rHDMA5]
    inc a                       ; $FF -> 0 sets Z
    jr z, .done
    dec de
    ld a, d
    or e
    jr nz, .wait
    jp TestFail
.done:
    ldh a, [rLY]
    cp EXPECT_LY
    jp nz, TestFail
TestPass:
    test_success
TestFail:
    test_failure

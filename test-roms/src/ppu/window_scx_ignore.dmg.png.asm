; window_scx_ignore.dmg.png — the window ignores the SCX fine-scroll discard.
;
; The window is locked to screen coordinates and must ignore SCX/SCY (Pan Docs,
; "Window"). A full-width window (WX=7, WY=0) that triggers at LX==0 draws its
; first pixel *after* the SCX&7 fine-scroll discard has consumed the leading
; BACKGROUND pixels, so its content is not shifted by SCX.
;
; This fills the whole screen with the window using a tile that darkens column 0
; of every 8-px tile, sets SCX=3, and holds a frame. With correct behavior the
; dark columns stay tile-aligned (x = 0,8,16,...); the pre-fix bug shifts them to
; x = 5,13,21,... The `png` grader compares the held frame to the derived oracle
; refs/ppu/window_scx_ignore.dmg.png (dark column at x%8==0)

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    ; Wait for VBlank so LCD writes are safe, then turn the LCD off.
.wait_vblank:
    ldh a, [rLY]
    cp 144
    jr nz, .wait_vblank
    xor a
    ldh [rLCDC], a

    ; Tile #2 at $8020: 8 rows of low=$80, high=$80 -> pixel 0 (leftmost) is
    ; color 3, pixels 1..7 are color 0. A single dark mark at column 0.
    ld hl, $8020
    ld c, 8
    ld a, $80
.tile:
    ld [hl+], a
    ld [hl+], a
    dec c
    jr nz, .tile

    ; Fill BG/window map $9800..$9BFF (1 KiB) with tile index 2.
    ld hl, $9800
    ld bc, $0400
    ld d, 2
.map:
    ld a, d
    ld [hl+], a
    dec bc
    ld a, b
    or c
    jr nz, .map

    ; Registers: window covers the whole screen (WY=0, WX=7), SCX=3 (the value
    ; the window must ignore), BGP identity, LCD on with window enabled using the
    ; $9800 map and $8000 tile data.
    xor a
    ldh [rWY], a
    ld a, 3
    ldh [rSCX], a
    ld a, 7
    ldh [rWX], a
    ld a, $E4
    ldh [rBGP], a
    ld a, LCDCF_ON | LCDCF_WINON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; Let the picture settle past the first-frame-after-enable warmup, then hand
    ; the frame to the grader with the LD B,B marker (during VBlank so the last
    ; completed frame is the stable one).
    ld c, 6
.settle:
    call WaitVBlankEdge
    dec c
    jr nz, .settle

    ; No register signature needed for a `png` ROM; the marker just says
    ; "frame ready". Reuse test_success purely for the LD B,B + spin.
    test_success

; Wait for a fresh VBlank: spin until LY leaves 144, then spin until it returns.
WaitVBlankEdge:
.not144:
    ldh a, [rLY]
    cp 144
    jr z, .not144
.to144:
    ldh a, [rLY]
    cp 144
    jr nz, .to144
    ret

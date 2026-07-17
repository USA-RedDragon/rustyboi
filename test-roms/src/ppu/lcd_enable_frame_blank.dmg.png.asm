; lcd_enable_frame_blank.dmg.png — the DMG panel shows BLANK (white) for the
; skipped first frame after an LCDC.7 enable.
;
; The PPU never displays the first frame it renders after the LCD turns on;
; on DMG the panel shows the LCD-off blank (white) for that frame (Pan Docs,
; "LCDC.7"). This is the DMG half of the CGB/DMG asymmetry: the CGB panel
; REPEATS the previous image instead (see lcd_enable_frame_repeat.cgb.png).
; This ROM pins the asymmetry so the CGB repeat behavior is never overreached
; onto DMG.
;
; Same script as the CGB ROM: paint the screen solid black (BGP=$FF, tile 0
; everywhere), display it for 6 frames, LCD off inside VBlank for ~2
; scanlines, LCD back on, and hand the frame to the grader (`LD B,B`) at
; LY=72 of the skipped first frame. The DMG panel shows white
; (refs/ppu/lcd_enable_frame_blank.dmg.png, all white — derived from the
; Pan Docs blank rule), NOT the black image and NOT the frame in flight.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    ; Wait for VBlank, then turn the LCD off for setup.
.wait_vblank:
    ldh a, [rLY]
    cp 144
    jr nz, .wait_vblank
    xor a
    ldh [rLCDC], a

    ; All four BGP shades = 3 (black). VRAM is zeroed (tile 0, map 0), so the
    ; whole BG resolves to shade 3.
    ld a, $FF
    ldh [rBGP], a

    ; LCD on, BG enabled: solid black screen.
    ld a, LCDCF_ON | LCDCF_BGON
    ldh [rLCDC], a

    ; Display the image for 6 full frames.
    ld c, 6
.settle:
    call WaitVBlankEdge
    dec c
    jr nz, .settle

    ; LCD off at the LY=144 edge, ~2 scanlines, LCD on.
    xor a
    ldh [rLCDC], a
    ld c, 60
.off_spin:
    dec c
    jr nz, .off_spin
    ld a, LCDCF_ON | LCDCF_BGON
    ldh [rLCDC], a

    ; Mid-way through the skipped first frame: the DMG panel shows blank.
.wait_mid_frame:
    ldh a, [rLY]
    cp 72
    jr nz, .wait_mid_frame

    test_success

; Wait for a fresh VBlank edge: spin until LY leaves 144, then until it returns.
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

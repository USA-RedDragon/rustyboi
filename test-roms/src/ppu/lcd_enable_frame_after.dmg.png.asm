; lcd_enable_frame_after.dmg.png — after the skipped post-enable frame, the
; DMG panel resumes DISPLAYING new frames (the blank lasts exactly one frame).
;
; Companion to lcd_enable_frame_blank.dmg.png (the skipped frame IS blank) and
; DMG mirror of lcd_enable_frame_after.cgb.png: the blank ROM's all-white
; oracle cannot distinguish a correct one-frame blank from an "always blank"
; over-regression — this ROM pins the blank's END.
;
; Script: identical to the blank ROM — paint the shared signature pattern
; (lcd_enable_pattern.inc) with the identity palette BGP=$E4, display it for
; 6 frames, EA-style LCD off/on inside VBlank — except that DURING the off
; window BGP is rewritten to $1B (the exact per-color reversal of $E4: color
; i renders shade 3-i, so every non-uniform pixel changes). The skipped frame
; renders in the $1B shades but is never displayed; the first genuinely
; displayed post-skip frame is the first $1B image. Wait three VBlank edges
; after the re-enable so that frame has completed and swapped to the front
; buffer, then hand it to the grader (`LD B,B`).
;
; Oracle: refs/ppu/lcd_enable_frame_after.dmg.png = the pattern rendered
; through BGP=$1B, derived from the layout constants + the DMG shade map
; (never captured from an emulator; see test-roms/gen_lcd_enable_refs.py).
; Discrimination: a panel stuck blanking shows white (fail); one wrongly
; repeating the held $E4 image shows every shade inverted (fail); the
; correct one-frame blank then resumed display shows the $1B pattern (pass).

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

    call PaintSignature
    ; Identity palette: color i renders shade i.
    ld a, $E4
    ldh [rBGP], a

    ; LCD on, BG enabled ($8000 tiles): the signature pattern displays.
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; Display the image for 6 full frames.
    ld c, 6
.settle:
    call WaitVBlankEdge
    dec c
    jr nz, .settle

    ; EA-style flip: LCD off at the LY=144 edge; while off, invert the
    ; palette (BGP=$1B, color i -> shade 3-i); LCD back on.
    xor a
    ldh [rLCDC], a
    ld c, 60
.off_spin:
    dec c
    jr nz, .off_spin
    ld a, $1B
    ldh [rBGP], a
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; The PPU restarted at LY=0; the frame now rendering (in $1B shades) is
    ; the skipped one. Edge 1 = the skipped frame's VBlank; edge 2 = the
    ; first displayed $1B frame's VBlank (still being presented — the
    ; front-buffer swap happens at the frame wrap, LY 153 -> 0); edge 3 =
    ; one frame later, when the $1B frame is guaranteed presented (the
    ; following frame is $1B-identical, so the exact swap dot is moot).
    call WaitVBlankEdge
    call WaitVBlankEdge
    call WaitVBlankEdge

    test_success

INCLUDE "lcd_enable_pattern.inc"

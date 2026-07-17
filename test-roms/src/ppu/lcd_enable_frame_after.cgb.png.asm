; lcd_enable_frame_after.cgb.png — after the skipped post-enable frame, the
; CGB panel resumes DISPLAYING new frames (the repeat lasts exactly one frame).
;
; Companion to lcd_enable_frame_repeat.cgb.png (see there for the hardware
; rule and provenance). That ROM pins the repeat itself; this one pins its
; END, guarding against an over-regression that leaves the panel stuck
; blanking or stuck repeating the held image forever.
;
; Script: identical to the repeat ROM — paint the shared signature pattern
; with palette table PalA, display it for 6 frames, EA-style LCD off/on
; inside VBlank, rewriting BG palettes 0-3 to PalB during the off window
; (PalA reversed per palette — every color entry differs). The skipped frame
; renders in PalB but is never displayed; the first genuinely displayed
; post-skip frame is the first PalB image. Wait three VBlank edges after the
; re-enable so that frame has completed and swapped to the front buffer,
; then hand it to the grader (`LD B,B`).
;
; Oracle: refs/ppu/lcd_enable_frame_after.cgb.png = the pattern rendered with
; PalB, derived from the layout + palette constants (never captured from an
; emulator). Discrimination: a panel stuck blanking shows white (fail); one
; stuck repeating shows the PalA colors (fail — every pixel differs); the
; correct one-frame repeat then resumed display shows PalB (pass).

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
DEF PATTERN_CGB EQU 1

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

    ; LCD on, BG enabled ($8000 tiles): the signature pattern displays (PalA).
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; Display the image for 6 full frames so the panel holds it.
    ld c, 6
.settle:
    call WaitVBlankEdge
    dec c
    jr nz, .settle

    ; EA-style flip, same as the repeat ROM: LCD off, PalB while off, LCD on.
    xor a
    ldh [rLCDC], a
    ld c, 60
.off_spin:
    dec c
    jr nz, .off_spin
    call WritePalB
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; The PPU restarted at LY=0; the frame now rendering (in PalB) is the
    ; skipped one. Edge 1 = the skipped frame's VBlank; edge 2 = the first
    ; displayed PalB frame's VBlank (still being presented — the front-buffer
    ; swap happens at the frame wrap, LY 153 -> 0); edge 3 = one frame later,
    ; when the PalB frame is guaranteed presented (the following frame is
    ; PalB-identical, so the exact swap dot is moot).
    call WaitVBlankEdge
    call WaitVBlankEdge
    call WaitVBlankEdge

    test_success

INCLUDE "lcd_enable_pattern.inc"

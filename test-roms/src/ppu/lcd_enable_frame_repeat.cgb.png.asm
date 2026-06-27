; lcd_enable_frame_repeat.cgb.png — the CGB panel REPEATS the previous image
; for the skipped first frame after an LCDC.7 enable.
;
; The PPU never displays the first frame it renders after the LCD turns on.
; On DMG the panel shows the LCD-off blank (white) for that frame (Pan Docs,
; "LCDC.7"); on CGB the panel keeps showing whatever it displayed last, and
; only decays to blank when left undriven for about a full frame period
; (SameBoy `frame_repeat_countdown`, measured on CGB-E: one frame + 3640
; 8 MHz cycles; see https://www.reddit.com/r/EmuDev/comments/6exyxu/).
; The EA CGB middleware (Madden NFL 2000, NHL 2000, Men in Black - The
; Series) relies on this: it flips its double-buffered tilemap through a
; ~2-line LCD off/on inside VBlank every ~7 frames — blanking the skipped
; frame instead of repeating strobes the whole screen white at ~9 Hz.
;
; This ROM paints a NON-UNIFORM signature pattern (4 tile shapes keyed by
; (tx+ty)&3, 4 CGB palettes keyed by (tx^ty)&3 — solid, stripe, band and
; diagonal tiles in 16 distinct colors), displays it with palette table PalA
; for 6 frames, then does the EA-style flip: LCD off inside VBlank, LCD back
; on a few scanlines later. DURING the off window it rewrites the palettes
; to PalB (every color entry differs from PalA), so the skipped in-flight
; frame renders in colors the panel has never displayed. It hands the frame
; to the grader (`LD B,B`) at LY=72 of the SKIPPED first frame. A correct
; panel still shows the PalA signature pattern — the image it last
; DISPLAYED (refs/ppu/lcd_enable_frame_repeat.cgb.png, derived from the
; repeat rule + this ROM's own pattern constants, never captured from an
; emulator). A solid-color oracle would be weak; the PalA pattern
; discriminates every plausible presentation failure: blanking shows white,
; presenting a zeroed buffer shows black, presenting the back buffer /
; in-flight render shows PalB rows, and a partial or shifted render
; mismatches too. On real hardware the screen must never flash.
; Companions: lcd_enable_frame_blank.dmg.png (DMG half of the asymmetry),
; lcd_enable_frame_after.cgb.png (display resumes after the skip).
;
; Pattern spec (must stay in byte-parity with the derived oracle):
;   tile 0 = solid color 3; tile 1 = 2px vertical stripes colors 1,1,2,2;
;   tile 2 = 2px horizontal bands colors 1,2,3,0; tile 3 = diagonal color 3
;   map[$9800] tile = (tx+ty)&3, attr bank1 palette = (tx^ty)&3, SCX=SCY=0
;   PalA below; oracle channel = (v5*255)/31 (grader masks to top 5 bits)

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

    ; LCD on, BG enabled ($8000 tiles): the signature pattern displays.
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; Display the image for 6 full frames so the panel holds it.
    ld c, 6
.settle:
    call WaitVBlankEdge
    dec c
    jr nz, .settle

    ; EA-style flip: we are at the LY=144 edge. LCD off; while it is off,
    ; swap the palettes to PalB (palette RAM is freely accessible) so the
    ; skipped frame renders in never-displayed colors; LCD back on after a
    ; few scanlines' worth of cycles — well inside the ~1-frame panel
    ; persistence window.
    xor a
    ldh [rLCDC], a
    ld c, 60
.off_spin:
    dec c
    jr nz, .off_spin
    call WritePalB
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; The PPU restarted at LY=0 and is now rendering (in PalB) the frame it
    ; will never display. Hand the framebuffer to the grader mid-way through
    ; it: the panel must still show the repeated PalA signature pattern — not
    ; blank white, not a zeroed (black) buffer, and not the PalB in-flight
    ; render or back buffer.
.wait_mid_frame:
    ldh a, [rLY]
    cp 72
    jr nz, .wait_mid_frame

    ; No register signature needed for a `png` ROM; the marker just says
    ; "frame ready" (and is a no-op spin on real hardware).
    test_success

INCLUDE "lcd_enable_pattern.inc"

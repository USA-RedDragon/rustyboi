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
; This ROM paints the screen solid BLACK (BG palette 0 color 0 = $0000, tile
; 0 everywhere), displays it for 6 frames, then does the EA-style flip: LCD
; off inside VBlank, ~2 scanlines later LCD back on. It hands the frame to
; the grader (`LD B,B`) at LY=72 of the SKIPPED first frame. A correct panel
; still shows the black image (refs/ppu/lcd_enable_frame_repeat.cgb.png, all
; black — derived from the repeat rule); an emulator that blanks the skipped
; frame shows all white. On real hardware the screen must never flash white.
; DMG counterpart: lcd_enable_frame_blank.dmg.png.

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

    ; BG palette 0 color 0 = $0000 (black). VRAM is zeroed (tile 0, map 0,
    ; attributes 0), so the whole BG resolves to this color.
    ld a, $80
    ldh [rBCPS], a
    xor a
    ldh [rBCPD], a
    ldh [rBCPD], a

    ; LCD on, BG enabled: solid black screen.
    ld a, LCDCF_ON | LCDCF_BGON
    ldh [rLCDC], a

    ; Display the image for 6 full frames so the panel holds it.
    ld c, 6
.settle:
    call WaitVBlankEdge
    dec c
    jr nz, .settle

    ; EA-style flip: we are at the LY=144 edge. LCD off, ~2 scanlines, LCD on.
    xor a
    ldh [rLCDC], a
    ld c, 60
.off_spin:
    dec c
    jr nz, .off_spin
    ld a, LCDCF_ON | LCDCF_BGON
    ldh [rLCDC], a

    ; The PPU restarted at LY=0 and is now rendering the frame it will never
    ; display. Hand the framebuffer to the grader mid-way through it: the
    ; panel must still show the repeated black image, not blank white.
.wait_mid_frame:
    ldh a, [rLY]
    cp 72
    jr nz, .wait_mid_frame

    ; No register signature needed for a `png` ROM; the marker just says
    ; "frame ready" (and is a no-op spin on real hardware).
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

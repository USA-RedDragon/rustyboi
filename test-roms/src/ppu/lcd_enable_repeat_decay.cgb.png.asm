; lcd_enable_repeat_decay.cgb.png — the CGB panel-repeat window DECAYS: an
; LCD off longer than the measured margin blanks the skipped frame.
;
; The panel-drive countdown (SameBoy `frame_repeat_countdown`, measured on
; CGB-E: 144*456*2 + 3640 cycles at 8 MHz, i.e. 144 lines + 1820 4 MHz cc)
; is re-armed at the start of EVERY VBlank line 144-152 and runs down in
; real time while the LCD is off. The 144-line budget is consumed by the
; skipped frame's own render (the repeat verdict is taken at its VBlank
; entry), so the off itself — measured from the start of the VBlank line it
; begins on — may only last ~1820 cc (just under 4 lines) for the panel to
; still repeat. lcd_enable_frame_repeat.cgb pins the repeat side of that
; boundary (off ≈ a few hundred cc); this ROM pins the DECAY side: the LCD
; goes off at the start of line 145 for ~4.7 lines (~2160 cc > 1820), so
; the skipped first frame after the re-enable must be blank WHITE — not the
; previously displayed PalA pattern (a too-permissive persistence window),
; and not the never-displayed PalB in-flight render or back buffer.
;
; Same signature-pattern protocol as the other lcd_enable_frame_* ROMs
; (include/lcd_enable_pattern.inc): display the PalA pattern for 6 frames,
; turn the LCD off at LY=145, rewrite the palettes to PalB during the off
; window, re-enable, and hand the frame to the grader (`LD B,B`) at LY=72
; of the frame AFTER the skipped one. (The repeat ROM grades mid-skipped-
; frame because a granted repeat presents the held image throughout; the
; decay verdict, by contrast, is only PRESENTED at the skipped frame's
; VBlank — on hardware the countdown expiry itself lands near the skipped
; frame's last line — so this ROM lets the skipped frame complete first.
; The next PalB frame has not completed at the marker, so the blank is
; still what a correct panel shows.) Oracle
; refs/ppu/lcd_enable_repeat_decay.cgb.png (all white) is derived from the
; decay rule alone, never captured from an emulator. On real hardware the
; screen shows a single white flash. A too-permissive persistence window
; (the pre-fix ~178-line one) grants the repeat instead and shows PalA here.

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

    ; The drive countdown re-arms at each VBlank line start; anchor the off
    ; at the START of line 145 (poll granularity ~32 cc) so the off duration
    ; below is measured against a freshly armed window.
.wait145:
    ldh a, [rLY]
    cp 145
    jr nz, .wait145
    xor a
    ldh [rLCDC], a

    ; While the LCD is off: palettes to PalB (the skipped frame renders in
    ; never-displayed colors), then pad the off to ~4.7 lines total —
    ; comfortably past the 1820 cc decay margin, far under the pre-decay
    ; repeat window any plausible over-permissive model would grant.
    call WritePalB
    ld c, 40
.off_pad:
    dec c
    jr nz, .off_pad
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a

    ; The PPU restarted at LY=0 and is rendering (in PalB) the frame it will
    ; never display. Let that skipped frame run through its VBlank — where
    ; the expired drive countdown denies the repeat and the blank is
    ; presented — then grade mid-way through the following frame: the panel
    ; must show blank white, not the repeated PalA pattern (an
    ; over-permissive window), not the PalB render (skipped frame or back
    ; buffer presented), and not a zeroed (black) buffer.
    call WaitVBlankEdge
.wait_mid_frame:
    ldh a, [rLY]
    cp 72
    jr nz, .wait_mid_frame

    ; No register signature needed for a `png` ROM; the marker just says
    ; "frame ready" (and is a no-op spin on real hardware).
    test_success

INCLUDE "lcd_enable_pattern.inc"

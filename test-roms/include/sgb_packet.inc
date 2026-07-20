; sgb_packet.inc — SGB command-packet transmission and MLT_REQ joypad-ID
; readback, shared by the ROMs under src/sgb/.
;
; Everything here is transcribed from Pan Docs, not from any emulator:
;
;   * "Command Packet Transfers": a transfer "must be initiated by setting JOYP
;     bits 4 and 5 both to 0; this will reset and start the ICD2 packet
;     receiving circuit. Data is then transferred (LSB first), setting bit 4 to
;     0 will indicate a `0` bit, and setting bit 5 to 0 will indicate a `1`
;     bit." Each packet is a start pulse, 16 bytes (128 bits) LSB-first, then a
;     `0` stop bit. Byte 0 is Command*8+Length.
;   * Same page, timing: licensed software "keep[s] data and reset pulses LOW
;     for at least 5 M-cycles and leave[s] bit 4 and 5 both to 1 for at least 15
;     M-cycles after each pulse". We use those figures rather than the 2
;     M-cycle hardware minimum, so the ROMs are reliable on real silicon.
;   * "Multiplayer Command (MLT_REQ)": "First set P14 and P15 both HIGH
;     (deselect both Buttons and Cursor keys), you can now read the lower 4 bits
;     of P1, which indicate the joypad ID" — $xF=player 1, $xE=2, $xD=3, $xC=4 —
;     and "The next joypad is automatically selected when P15 goes from LOW (0)
;     to HIGH (1)".
;
; Only the P15-low-then-both-high ($10 -> $30) advance is used. The gbdev ICD2
; register page describes the advance as "P15 goes low and then both P15 and
; P14 go high", which is the same edge for this sequence; the two sources only
; disagree about $10 -> $20, which no ROM here exercises.

IF !DEF(SGB_PACKET_INC)
DEF SGB_PACKET_INC EQU 1

; Joypad IDs reported in the P1 low nibble while both select lines are HIGH.
DEF SGB_ID_P1 EQU $0F
DEF SGB_ID_P2 EQU $0E
DEF SGB_ID_P3 EQU $0D
DEF SGB_ID_P4 EQU $0C

; Terminator for the expected-ID tables fed to SgbCheckSequence. Joypad IDs
; only ever occupy $0C-$0F, so $FF can never collide with a real reading.
DEF SGB_SEQ_END EQU $FF

; Scratch used by the result strip and the per-subtest pass mask.
DEF SGB_RES_MASK  EQU $C0F0
DEF SGB_RES_COUNT EQU $C0F1
DEF SGB_RESULT    EQU $C0F2

; P1 select-line states (bit 4 = P14 / directions, bit 5 = P15 / buttons; both
; active LOW).
DEF P1_BOTH_HIGH EQU $30   ; deselected: joypad ID readable here
DEF P1_P14_LOW   EQU $20   ; transmits a "0" bit
DEF P1_P15_LOW   EQU $10   ; transmits a "1" bit
DEF P1_BOTH_LOW  EQU $00   ; start / reset pulse

; Pan Docs: pulses LOW for at least 5 M-cycles.
MACRO sgb_pulse_delay
    REPT 5
    nop
    ENDR
ENDM

; Pan Docs: both lines HIGH for at least 15 M-cycles after each pulse.
MACRO sgb_space_delay
    REPT 15
    nop
    ENDR
ENDM

; Drive one select-line state, hold it low, then return to both-HIGH and hold.
MACRO sgb_pulse   ; \1 = P1 value for the low phase
    ld a, \1
    ldh [rP1], a
    sgb_pulse_delay
    ld a, P1_BOTH_HIGH
    ldh [rP1], a
    sgb_space_delay
ENDM

; Record a subtest verdict: A = 0 (pass) / non-zero (fail); \1 = its bit in
; SGB_RESULT. A failing subtest leaves its bit clear and the ROM keeps running,
; so the result strip shows exactly which subtests failed.
MACRO sgb_record   ; \1 = bit mask
    or a
    jr nz, .recorded\@
    ld a, [SGB_RESULT]
    or \1
    ld [SGB_RESULT], a
.recorded\@:
ENDM

SECTION "sgb_packet", ROM0

; Transmit one 16-byte command packet at HL. Leaves P1 = $30 (both select lines
; deselected), which is exactly the state the joypad-ID readback needs.
; Clobbers A, B, C, E, HL.
SgbSendPacket:
    sgb_pulse P1_BOTH_LOW          ; start: reset + start the ICD2 receiver
    ld c, 16                       ; 16 bytes = 128 bits
.byte:
    ld e, [hl]
    inc hl
    ld b, 8
.bit:
    srl e                          ; LSB first; carry = the bit to send
    ld a, P1_P14_LOW               ; carry clear -> "0"
    jr nc, .drive
    ld a, P1_P15_LOW               ; carry set   -> "1"
.drive:
    ldh [rP1], a
    sgb_pulse_delay
    ld a, P1_BOTH_HIGH
    ldh [rP1], a
    sgb_space_delay
    dec b
    jr nz, .bit
    dec c
    jr nz, .byte
    sgb_pulse P1_P14_LOW           ; stop bit: a single "0" pulse
    ret

; As SgbSendPacket, but drives an extra both-LOW pulse after the 64th data bit.
; Pan Docs says both lines LOW "will reset and start the ICD2 packet receiving
; circuit", so mid-transfer it resynchronises the receiver: the 128 bits sent
; never assemble into one complete packet and no command is dispatched.
; Clobbers A, B, C, D, E, HL.
SgbSendPacketResetMidway:
    sgb_pulse P1_BOTH_LOW
    ld c, 16
    ld d, 0                        ; data bits sent so far
.byte:
    ld e, [hl]
    inc hl
    ld b, 8
.bit:
    srl e
    ld a, P1_P14_LOW
    jr nc, .drive
    ld a, P1_P15_LOW
.drive:
    ldh [rP1], a
    sgb_pulse_delay
    ld a, P1_BOTH_HIGH
    ldh [rP1], a
    sgb_space_delay
    inc d
    ld a, d
    cp 64
    jr nz, .no_reset
    sgb_pulse P1_BOTH_LOW          ; mid-transfer reset
.no_reset:
    dec b
    jr nz, .bit
    dec c
    jr nz, .byte
    sgb_pulse P1_P14_LOW
    ret

; Advance the multiplex index: drive P15 LOW then HIGH again. Clobbers A;
; leaves P1 = $30.
SgbNextPlayer:
    sgb_pulse P1_P15_LOW
    ret

; Read the joypad ID nibble. Requires both select lines already deselected
; (every routine above leaves P1 = $30), so this performs no write and cannot
; itself clock the multiplex index. The repeated reads are the standard
; settle-the-line idiom Pan Docs describes for P1. Returns A = low nibble.
SgbReadId:
    ldh a, [rP1]
    ldh a, [rP1]
    ldh a, [rP1]
    and $0F
    ret

; Walk the SGB_SEQ_END-terminated table of expected joypad IDs at HL. The first
; entry is read with no preceding pulse (the caller left P1 deselected); every
; later entry is preceded by one P15 LOW->HIGH advance. Returns A = 0 when the
; whole sequence matched, A = 1 on the first mismatch.
; Clobbers A, C, HL.
SgbCheckSequence:
    ld c, 0                        ; 0 = first entry: do not advance yet
.next:
    ld a, [hl]
    cp SGB_SEQ_END
    jr z, .matched
    ld a, c
    or a
    call nz, SgbNextPlayer
    call SgbReadId
    cp [hl]
    jr nz, .mismatch
    inc hl
    ld c, 1
    jr .next
.matched:
    xor a
    ret
.mismatch:
    ld a, 1
    ret

; Busy-wait roughly A frames. Cycle-counted rather than LY-polled so it cannot
; hang if the LCD is off. Clobbers A, B, C, D.
SgbWaitFrames:
    ld d, a
.frame:
    ld bc, 2500                    ; ~7 M-cycles/iteration ~= 17500 M ~= 1 frame
.spin:
    dec bc
    ld a, b
    or c
    jr nz, .spin
    dec d
    jr nz, .frame
    ret

; Draw the per-subtest verdict strip: A = number of subtests, E = pass bitmask
; (bit 0 = first subtest). Each subtest becomes a 3x3-tile block on a
; checkerboard field — solid black = passed, solid white = failed. The
; checkerboard keeps the screen non-uniform whatever the outcome, so a real SGB
; gives a readable answer without a debugger, and any wrong subtest is visible
; as a white hole in the strip.
; Clobbers everything except the caller's stack.
SgbShowResults:
    ld [SGB_RES_COUNT], a
    ld a, e
    ld [SGB_RES_MASK], a

    ; Bounded wait for VBlank, so switching the LCD off is well-timed on real
    ; hardware but a stopped LCD cannot wedge the ROM.
    ld bc, 0
.vblank:
    ldh a, [rLY]
    cp 144
    jr nc, .vblank_done
    dec bc
    ld a, b
    or c
    jr nz, .vblank
.vblank_done:
    xor a
    ldh [rLCDC], a                 ; LCD off: VRAM is freely writable
    ldh [rSCY], a
    ldh [rSCX], a

    ; Tile 0: white ($00). Tile 1: black ($FF). Tile 2: checkerboard.
    ld hl, $8000
    ld b, 16
    xor a
.tile_white:
    ld [hl+], a
    dec b
    jr nz, .tile_white
    ld b, 16
    ld a, $FF
.tile_black:
    ld [hl+], a
    dec b
    jr nz, .tile_black
    ld b, 4                        ; 4 iterations * 2 rows = 8 rows
.tile_checker:
    ld a, $AA
    ld [hl+], a
    ld [hl+], a
    ld a, $55
    ld [hl+], a
    ld [hl+], a
    dec b
    jr nz, .tile_checker

    ; Checkerboard field across the whole map.
    ld hl, $9800
    ld bc, $0400
.fill:
    ld a, 2
    ld [hl+], a
    dec bc
    ld a, b
    or c
    jr nz, .fill

    ; One 3x3 block per subtest: map row 4, from column 1, pitch 4 columns.
    ld hl, $9800 + 4 * 32 + 1
.block:
    ld a, [SGB_RES_MASK]
    srl a                          ; carry = this subtest's pass bit
    ld [SGB_RES_MASK], a
    ld a, 0
    adc a, 0                       ; A = 1 (black, passed) or 0 (white, failed)
    ld b, 3
.row:
    ld [hl], a
    inc hl
    ld [hl], a
    inc hl
    ld [hl], a
    push af
    ld de, 30                      ; back to column 0 of the next map row
    add hl, de
    pop af
    dec b
    jr nz, .row
    ld de, -(3 * 32) + 4           ; up 3 rows, right one block pitch
    add hl, de
    ld a, [SGB_RES_COUNT]
    dec a
    ld [SGB_RES_COUNT], a
    jr nz, .block

    ld a, %11100100
    ldh [rBGP], a
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a
    ; Let the panel settle before returning to the verdict marker. The first
    ; frame after LCDC.7 goes high is not displayed (blank on DMG, a repeat of
    ; the previous image on CGB), so a caller that halts immediately would leave
    ; the strip invisible to a screenshot taken at the done-marker.
    ld a, 3
    call SgbWaitFrames
    ret

; --- MLT_REQ command packets -------------------------------------------------
; Byte 0 = Command*8 + Length; MLT_REQ is command $11, one packet -> $89.
; Byte 1 selects the player count. Pan Docs lists only 0 = one player,
; 1 = two players, 3 = four players; the value 2 is undocumented on every
; register-level source (Pan Docs, the ICD2 page, fullsnes), so no ROM here
; sends it.

SECTION "sgb_packets_data", ROM0

SgbPktOnePlayer:
    db $89, $00
    ds 14, $00
SgbPktTwoPlayers:
    db $89, $01
    ds 14, $00
SgbPktFourPlayers:
    db $89, $03
    ds 14, $00

ENDC

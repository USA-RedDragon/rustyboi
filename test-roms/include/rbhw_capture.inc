; rbhw_capture.inc — raw-measurement capture harness for the hardware-bench
; ("RBHW") ROMs under src/*/*.bench.asm.
;
; These ROMs are NOT pass/fail tests. They exist to settle cells where our model
; is SameBoy-derived or first-principles rather than silicon-verified, so they
; must not encode a verdict: a ROM that graded its own subject would freeze the
; unverified inference into a permanent oracle. They record RAW BYTES to cart
; SRAM and an operator reads the save back off the cart (GBxCart RW) after the
; run. Interpretation happens off-cart, by a human, against the header comment
; of the ROM that produced the bytes.
;
; Record layout at $A000 (little-endian words):
;
;   $A000  4  tag "RBHW"
;   $A004  1  format revision (RBHW_FMT)
;   $A005  1  ROM id — which bench ROM wrote this record
;   $A006  2  run counter: 1 on a fresh cart, +1 each power-on that finds its
;             own tag+format+ROM id already there. Lets an operator tell a
;             re-run from a stale save, and spot a cart whose battery is dead
;             (the counter never advances past 1).
;   $A008  8  fingerprint vector — see RbhwBeginRecord. Records WHICH silicon
;             produced the payload, so a save file is self-identifying even if
;             it is separated from the unit passport.
;   $A010  2  payload length in bytes
;   $A012  2  CRC16/XMODEM over $A000..$A012 and $A014..($A020+payload length),
;             i.e. everything except the CRC field itself. Guards against a
;             half-written record, a bad cart read, or SRAM decay.
;   $A014 12  reserved, zero
;   $A020  .. payload — the ROM's own format, documented in its header comment
;
; Every value written to the payload is a byte the ROM read back from hardware.
; No helper here compares, thresholds, or grades anything.
;
; Cart requirements: MBC5 + RAM + BATTERY (cart type $1B, 8 KiB RAM). The
; Makefile fixes every `.bench.` ROM that way; see test-roms/README.md.

IF !DEF(RBHW_CAPTURE_INC)
DEF RBHW_CAPTURE_INC EQU 1

DEF RBHW_SRAM        EQU $A000
DEF RBHW_FMT         EQU $01

DEF RBHW_OFF_TAG     EQU $00
DEF RBHW_OFF_FMT     EQU $04
DEF RBHW_OFF_ROMID   EQU $05
DEF RBHW_OFF_RUNS    EQU $06
DEF RBHW_OFF_FINGER  EQU $08
DEF RBHW_OFF_PAYLEN  EQU $10
DEF RBHW_OFF_CRC     EQU $12
DEF RBHW_OFF_RSVD    EQU $14
DEF RBHW_PAYLOAD     EQU $20

; HRAM scratch. $FF80/$FF81 hold A and B as the boot ROM left them — the caller
; must stash them in its first instructions, before anything can clobber them.
DEF RBHW_BOOT_A      EQU $FF80
DEF RBHW_BOOT_B      EQU $FF81
DEF RBHW_ROMID       EQU $FF82

; Stash the boot-ROM register handoff. Must be the ROM's first two actions.
; A = boot A (DMG $01, MGB $FF, CGB/AGB $11 — a CGB reports $11 even for a
; DMG-header cart, which is exactly how a ROM detects DMG-compat mode), B = the
; CGB-family discriminator (bit 0 set on AGB).
MACRO rbhw_boot_capture
    ldh [RBHW_BOOT_A], a
    ld a, b
    ldh [RBHW_BOOT_B], a
ENDM

SECTION "rbhw_capture", ROM0

; MBC5: unlock cart RAM and select bank 0.
RbhwSramOn:
    ld a, $0A
    ld [$0000], a
    xor a
    ld [$4000], a
    ret

; Re-lock cart RAM. Leaving it unlocked risks a stray write corrupting the
; capture on the walk between the console and the cart reader.
RbhwSramOff:
    xor a
    ld [$0000], a
    ret

; Open a record: A = ROM id, DE = payload length. Enables SRAM, writes the
; header, and leaves SRAM enabled for the caller's payload writes.
;
; The fingerprint vector is eight raw reads, no interpretation:
;   +0 boot A          +1 boot B          +2 KEY1 $FF4D      +3 OPRI $FF6C
;   +4 SVBK $FF70      +5 RP   $FF56      +6 NR52 $FF26      +7 VBK  $FF4F
; On DMG/MGB the CGB-only registers read back as open bus, which is itself part
; of the signature. NR52 is sampled here, before the caller touches the APU.
;
; Clobbers A, BC, HL. Preserves DE.
RbhwBeginRecord:
    ldh [RBHW_ROMID], a
    call RbhwSramOn
    ; Run counter: only continue a record this same ROM wrote in this same
    ; format. Anything else (fresh cart, other bench ROM, garbage) restarts at 1.
    ld hl, RBHW_SRAM
    ld a, [hl+]
    cp 'R'
    jr nz, .fresh
    ld a, [hl+]
    cp 'B'
    jr nz, .fresh
    ld a, [hl+]
    cp 'H'
    jr nz, .fresh
    ld a, [hl+]
    cp 'W'
    jr nz, .fresh
    ld a, [hl+]
    cp RBHW_FMT
    jr nz, .fresh
    ld a, [hl+]
    ld c, a
    ldh a, [RBHW_ROMID]
    cp c
    jr nz, .fresh
    ld a, [hl+]
    ld c, a
    ld b, [hl]
    inc bc
    jr .counted
.fresh:
    ld bc, 1
.counted:
    ld hl, RBHW_SRAM
    ld a, 'R'
    ld [hl+], a
    ld a, 'B'
    ld [hl+], a
    ld a, 'H'
    ld [hl+], a
    ld a, 'W'
    ld [hl+], a
    ld a, RBHW_FMT
    ld [hl+], a
    ldh a, [RBHW_ROMID]
    ld [hl+], a
    ld a, c
    ld [hl+], a
    ld a, b
    ld [hl+], a
    ldh a, [RBHW_BOOT_A]
    ld [hl+], a
    ldh a, [RBHW_BOOT_B]
    ld [hl+], a
    ldh a, [$FF4D]
    ld [hl+], a
    ldh a, [$FF6C]
    ld [hl+], a
    ldh a, [$FF70]
    ld [hl+], a
    ldh a, [$FF56]
    ld [hl+], a
    ldh a, [$FF26]
    ld [hl+], a
    ldh a, [$FF4F]
    ld [hl+], a
    ld a, e
    ld [hl+], a
    ld a, d
    ld [hl+], a
    xor a
    ld c, RBHW_PAYLOAD - RBHW_OFF_CRC   ; CRC field + reserved, all zero
.zero:
    ld [hl+], a
    dec c
    jr nz, .zero
    ret

; Close a record: DE = payload length. Stamps the CRC and re-locks SRAM.
; Clobbers A, BC, DE, HL.
RbhwFinish:
    push de
    ld de, 0
    ld hl, RBHW_SRAM
    ld bc, RBHW_OFF_CRC                 ; header up to (not incl.) the CRC field
    call RbhwCrc16
    pop bc                              ; BC = payload length
    push de
    ld hl, RBHW_PAYLOAD - RBHW_OFF_RSVD ; reserved bytes ...
    add hl, bc                          ; ... plus the payload
    ld b, h
    ld c, l
    pop de
    ld hl, RBHW_SRAM + RBHW_OFF_RSVD
    call RbhwCrc16
    ld hl, RBHW_SRAM + RBHW_OFF_CRC
    ld a, e
    ld [hl+], a
    ld a, d
    ld [hl], a
    jp RbhwSramOff

; CRC16/XMODEM (poly $1021, MSB-first) over BC bytes at HL, accumulating into
; DE. DE = 0 starts a fresh sum; feeding the result back in chains two spans.
; Returns the running CRC in DE. Clobbers A, BC, HL.
RbhwCrc16:
    ld a, b
    or c
    ret z
.byte:
    ld a, [hl+]
    xor d
    ld d, a
    push bc
    ld b, 8
.bit:
    sla e
    rl d
    jr nc, .nopoly
    ld a, d
    xor $10
    ld d, a
    ld a, e
    xor $21
    ld e, a
.nopoly:
    dec b
    jr nz, .bit
    pop bc
    dec bc
    ld a, b
    or c
    jr nz, .byte
    ret

ENDC

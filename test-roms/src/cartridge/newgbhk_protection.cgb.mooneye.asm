; newgbhk_protection.cgb.mooneye — "New GB Color" HK-PCB read protection.
;
; The HK0701/HK0819 cartridges (Monster Go! Go! II, a CGB colourisation hack of
; Kirby's Dream Land 2 wearing a `KIRBY2` header; Pokemon Action Chapter) are
; electrically plain MBC5 with a truthful MBC5-family header, plus exactly one
; deviation: while the MBC5 ROM-bank register (low 8 bits OR'd with the
; $3000-$3FFF high bit) holds a value >= $80, the whole $4000-$7FFF window stops
; being ROM and becomes the protection chip. $5000-$7FFF then reads back $FF,
; and $4000-$4FFF returns a byte derived purely from the address: take
; `digits = (addr >> 4) & $FF` and dispatch on `digits & 7` --
;   0: digits              4: digits rotated left 1
;   1: digits XOR $AA      5: digits bit-reversed
;   2: digits XOR $55      6: OR each bit pair into the high nibble, AND into low
;   3: digits ror 1        7: XNOR each bit pair into the high nibble, XOR into low
; (taizou's hhugboy `MbcUnlNewGbHk`, CC0). The cart's boot trampoline sets bit 7
; with `or $80`, reads two of those derived bytes as a little-endian pointer,
; dereferences it to obtain the bank holding its CGB palette/tilemap init, calls
; it, then reads two more to restore the caller's bank. On a plain MBC5 bit 7 is
; masked away, the reads return ordinary ROM, and the derived "bank" is wrong --
; the cart lands on a $10 (`stop`) opcode and freezes on a white screen.
;
; rustyboi content-detects the board from the CRC32 of the 46-byte protection
; trampoline at $0091 (0x53C0_8E9D), gated on an MBC5-family header type.
;
; This ROM asserts, with DATA reads only (`ld a,[nn]`; it never executes a byte
; out of the banked window or an injected sequence, so the verdict cannot depend
; on how many cartridge reads the CPU issues per fetch):
;   1. with the bank register < $80 the window is ordinary ROM;
;   2. with it >= $80 (via the low register) each of the eight transforms
;      returns its derived value, and $5000+ returns $FF;
;   3. the high bank-register bit alone also engages the protection, so the gate
;      really is the 9-bit bank value and not bit 7 of the low byte;
;   4. bank 0 ($0000-$3FFF) is never affected;
;   5. dropping the bank register back below $80 restores ordinary ROM reads.
; Run as a plain MBC5 (detection disabled) the ROM FAILS at step 2's first
; assertion -- verified by commenting out the detection rule.
;
; PROVENANCE: emulator-algorithm-anchored + game-boot-anchored. The transform
; set is taizou's hhugboy `MbcUnlNewGbHk` (CC0), which names this exact PCB
; family; the local mGBA 0.10.5 does NOT implement it, so that binary is not a
; runnable oracle. Independently corroborated from the cart itself: the three
; derived values the trampoline consumes are the only ones that make its own
; anti-tamper arithmetic close (the 28-iteration checksum it runs over the
; continuation must yield exactly $BC so that the bank it then selects is the
; graphics bank and the copy destination is $8000, and the bank it restores is
; the only one in the image with a `ret` at the return address). Treat this as a
; regression pin against hhugboy, not a first-principles silicon truth.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Truthful MBC5 header ($19): detection is by the $0091 stub CRC32, and decode
; falls through to this header type (rgbfix -v recomputes the header checksum;
; -p sets the real ROM-size byte over the placeholder here).
SECTION "header", ROM0[$147]
    db $19    ; MBC5
    db $00    ; ROM size placeholder (rgbfix -p sets the real value)
    db $00    ; RAM size: none

; Protection-trampoline signature at $0091: detection keys ONLY on the CRC32 of
; these 46 bytes, never their meaning. This is a CLEAN-ROOM stand-in — a plain
; ASCII banner (42 bytes) plus a 4-byte suffix computed so the block's CRC32
; equals the detection constant 0x53C0_8E9D — so it carries the signature
; without embedding any of the cartridge's own code.
SECTION "newgbhksig", ROM0[$91]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $4E, $45, $57, $47, $42, $48, $4B
    db $20, $48, $4B, $30, $37, $30, $31, $20, $43, $4C, $45, $41, $4E, $52, $4F, $4F
    db $4D, $20, $53, $49, $47, $21, $21, $21, $21, $21
    db $38, $CB, $84, $40   ; CRC32-forcing suffix -> $53C0_8E9D

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. bank register < $80: the window is ordinary ROM. ---
    ld a, $01
    ld [$2000], a
    ld a, [$4096]
    cp $5A
    jp nz, .fail
    ld a, [$5000]
    cp $3C
    jp nz, .fail

    ; --- 2. bank register >= $80: the window is the protection chip. Each read
    ;     below picks a different `digits & 7` transform. A plain MBC5 folds
    ;     $81 back to bank 1 and returns the ROM markers, failing immediately. ---
    ld a, $81
    ld [$2000], a

    ld a, [$4000]        ; digits $00, case 0: identity
    cp $00
    jp nz, .fail
    ld a, [$4010]        ; digits $01, case 1: XOR $AA
    cp $AB
    jp nz, .fail
    ld a, [$4020]        ; digits $02, case 2: XOR $55
    cp $57
    jp nz, .fail
    ld a, [$4030]        ; digits $03, case 3: rotate right 1
    cp $81
    jp nz, .fail
    ld a, [$4040]        ; digits $04, case 4: rotate left 1
    cp $08
    jp nz, .fail
    ld a, [$4050]        ; digits $05, case 5: bit reversal
    cp $A0
    jp nz, .fail
    ld a, [$4060]        ; digits $06, case 6: OR/AND bit pairs
    cp $30
    jp nz, .fail
    ld a, [$4070]        ; digits $07, case 7: XNOR/XOR bit pairs
    cp $D2
    jp nz, .fail
    ld a, [$4096]        ; digits $09, case 1 — the address the cart itself reads
    cp $A3
    jp nz, .fail
    ld a, [$4FF0]        ; digits $FF, case 7: last protected address
    cp $F0
    jp nz, .fail

    ; $5000-$7FFF reads back $FF rather than a derived value.
    ld a, [$5000]
    cp $FF
    jp nz, .fail
    ld a, [$7FFF]
    cp $FF
    jp nz, .fail

    ; --- 3. bank 0 is never affected while the protection is engaged. ---
    ld a, [$0A00]
    cp $C3
    jp nz, .fail

    ; --- 4. the gate is the 9-bit bank value: low register $00 with the high
    ;     bit set is $100, still >= $80, so the protection stays engaged. ---
    ld a, $00
    ld [$2000], a
    ld a, $01
    ld [$3000], a
    ld a, [$4096]
    cp $A3
    jp nz, .fail

    ; --- 5. dropping back below $80 restores ordinary ROM reads. ---
    ld a, $00
    ld [$3000], a
    ld a, $01
    ld [$2000], a
    ld a, [$4096]
    cp $5A
    jp nz, .fail
    ld a, [$5000]
    cp $3C
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank-0 marker (read through the $0A00 window; never protected).
SECTION "bank0marker", ROM0[$0A00]
    db $C3

; Bank-1 markers. Every one differs from the protection value the same address
; yields, so a plain MBC5 (which folds bank $81 to bank 1 and serves these)
; fails every step-2 assertion.
SECTION "bank1_0000", ROMX[$4000], BANK[1]
    db $11
SECTION "bank1_0010", ROMX[$4010], BANK[1]
    db $12
SECTION "bank1_0020", ROMX[$4020], BANK[1]
    db $13
SECTION "bank1_0030", ROMX[$4030], BANK[1]
    db $14
SECTION "bank1_0040", ROMX[$4040], BANK[1]
    db $15
SECTION "bank1_0050", ROMX[$4050], BANK[1]
    db $16
SECTION "bank1_0060", ROMX[$4060], BANK[1]
    db $17
SECTION "bank1_0070", ROMX[$4070], BANK[1]
    db $18
SECTION "bank1_0096", ROMX[$4096], BANK[1]
    db $5A
SECTION "bank1_0FF0", ROMX[$4FF0], BANK[1]
    db $6B
SECTION "bank1_1000", ROMX[$5000], BANK[1]
    db $3C
SECTION "bank1_3FFF", ROMX[$7FFF], BANK[1]
    db $7E

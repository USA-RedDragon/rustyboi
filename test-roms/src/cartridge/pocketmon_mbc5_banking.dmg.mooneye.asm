; pocketmon_mbc5_banking.dmg.mooneye — Hong Kong "POCKETMON" Pokemon Red bootleg
; mapper (rustyboi UnlMapper::ForceMbc5).
;
; The cart (No-Intro "Pokemon - Red Version (Hong Kong) (SGB Enhanced) (Pirate)",
; 1MB, title "POCKETMON BE") wears an MBC1+RAM+BATTERY header ($03) but was
; re-linked by the bootlegger from MBC1 to MBC5-style LINEAR banking: the game
; writes a FULL-WIDTH bank number to $2000-$2FFF (`ld a,$21 / ld [$2000],a`),
; where a real 5-bit MBC1 register masks $21 down to bank 1. Bank 1 holds the
; game's relocated code whose byte at $4004 is $F4 (an illegal opcode), so a
; plain-MBC1 decode of this cart executes into it and dies during the intro
; (mGBA fails identically). Presenting the full byte selects the intended bank —
; e.g. physical bank $21 (33) byte-matches Pocket Monsters Aka bank 1 $4672, only
; the re-linked jump/call targets differ — and the game boots into its Chinese
; intro. There is NO data or address scrambling: the board is electrically a
; plain MBC5+RAM+BATTERY. rustyboi content-detects it from the CRC32 of the 48
; bytes at $0184 (0x0864_AF13) plus the MBC1-family header guard, then decodes
; MBC5+RAM+BATTERY.
;
; This ROM asserts the one behaviour that separates ForceMbc5 from the plain
; MBC1 decode its header would otherwise get:
;   1. MBC5 8-bit banking — a single write of $21 to $2000 must reach bank 33.
;      A 5-bit MBC1 mask folds $21 to bank 1, so it fails here (the exact fold
;      that crashes the real cart).
;   2. full 6-bit width — a write of $3F must reach bank 63, not the MBC1-masked
;      bank 31.
;   3. banking still latches back — a write of $01 reselects bank 1, proving we
;      did not disable banking to pass steps 1-2.
; Run as a plain MBC1 (detection disabled) the ROM FAILS at step 1; run as MBC5
; (the correct electrical board) it passes.
;
; PROVENANCE: real-ROM-anchored. The pirate's physical banks byte-match Pocket
; Monsters Aka (its base game) once addressed with full-width banking (bank 33 =
; Aka bank 1 $4672, byte-identical bar relocated absolute addresses), and the
; cart boots into real gameplay only under this decode. This is a clean-room
; regression pin (synthetic data + a CRC32-forcing suffix carrying the detection
; signature); no copyrighted bytes are embedded.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; The header type byte reads MBC1+RAM+BATTERY ($03) exactly like the real cart:
; detection is by the $0184 CRC32, and the loader must apply the MBC5 override to
; the MBC1-typed cart. rgbfix (-v -p) recomputes the header checksum and pads;
; the ROM-size byte here declares 1MB (64 banks) so bank $3F is reachable.
SECTION "header", ROM0[$147]
    db $03    ; MBC1+RAM+BATTERY (the POCKETMON header lie)
    db $05    ; ROM size: 1MB / 64 banks
    db $03    ; RAM size: 32KB (matches the real cart header)

; Detection signature at $0184: rustyboi keys ONLY on the CRC32 of these 48
; bytes, never their meaning. CLEAN-ROOM stand-in — a plain ASCII banner (44
; bytes) plus a 4-byte suffix computed so the block's CRC32 equals the detection
; constant 0x0864_AF13 — so it carries the signature without embedding any
; copyrighted bytes from the real cart.
SECTION "pmlogo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $50, $4F, $43, $4B, $45, $54, $4D
    db $4F, $4E, $2D, $42, $45, $20, $4D, $42, $43, $35, $20, $43, $4C, $45, $41, $4E
    db $52, $4F, $4F, $4D, $20, $53, $49, $47, $21, $21, $21, $21
    db $5A, $93, $73, $AA   ; CRC32-forcing suffix -> 0x0864_AF13

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. MBC5 8-bit ROM bank: a single $2000 write of $21 reaches bank 33.
    ;     (A plain MBC1 header would 5-bit-mask $21 down to bank 1 = $B1 — the
    ;     exact fold that lands the real cart on its illegal $F4 opcode.) ---
    ld a, $21
    ld [$2000], a
    ld a, [$4000]
    cp $A5
    jp nz, .fail

    ; --- 2. full 6-bit width: $3F reaches bank 63, not the MBC1-masked 31. ---
    ld a, $3F
    ld [$2000], a
    ld a, [$4000]
    cp $6F
    jp nz, .fail

    ; --- 3. banking still latches back: reselect bank 1. ---
    ld a, $01
    ld [$2000], a
    ld a, [$4000]
    cp $B1
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank markers at offset 0 of each bank (read through the fixed $4000 window).
; Banks $21 (33) and $3F (63) are only reachable via MBC5's 8-bit low bank
; register; a 5-bit MBC1 mask would fold them to banks 1 and 31.
SECTION "bank1marker",  ROMX[$4000], BANK[$01]
    db $B1
SECTION "bank31marker", ROMX[$4000], BANK[$1F]
    db $9F
SECTION "bank33marker", ROMX[$4000], BANK[$21]
    db $A5
SECTION "bank63marker", ROMX[$4000], BANK[$3F]
    db $6F

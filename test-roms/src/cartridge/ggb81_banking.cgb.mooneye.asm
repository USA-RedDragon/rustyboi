; ggb81_banking.cgb.mooneye — GGB81 (Vast Fame family) data-line scrambler.
;
; The GGB81 carts (Muchang Wuyu GB 6, the "DIGIMON" Shuma Baolong bootleg) are
; electrically plain MBC5 wearing a truthful MBC5-family header ($19 = MBC5,
; $1B = MBC5+RAM+BATTERY), with one deviation: a write whose address satisfies
; addr & 0xF0FF == 0x2001 latches a 3-bit "swap mode" (the same write also acts
; as the MBC5 low-bank register), and every read from the $4000-$7FFF bank
; window returns the ROM byte with its data lines permuted through that mode's
; table (mGBA `_GBGGB81` / `_ggb81DataReordering`; output bit i = input bit
; table[i]). Bank 0 reads are left unscrambled. rustyboi content-detects the
; board from the CRC32 of the 48-byte secondary logo at $0184 (0x79F3_4594),
; then maps UnlMapper::Ggb81 and decodes it via the truthful header.
;
; This ROM asserts the behaviours that separate GGB81 from the plain MBC5 it
; would otherwise be:
;   1. default swap mode 0 is the identity — a bank-1 read of $0F stays $0F.
;   2. mode 2 (latched by writing $02 to $2001, then reselecting the bank via
;      $2000 so the mode survives) reorders that same $0F to $69. A plain MBC5
;      does not scramble reads, so it still returns $0F and FAILS here.
;   3. bank 0 reads are never reordered — a bank-0 $C3 stays $C3 under mode 2.
;   4. returning to mode 0 restores the identity read.
; Run as a plain MBC5 (detection disabled) the ROM FAILS at step 2 — confirmed
; by temporarily disabling the GGB81 detection rule.
;
; PROVENANCE: emulator-algorithm-anchored + game-boot-anchored. The swap
; protocol and the 8 reorder tables are mGBA upstream's `_GBGGB81` /
; `_ggb81DataReordering` (the GGB81 mapper was added to mGBA AFTER the 0.10.5
; build at /usr/bin/mgba, which does NOT implement it, so the local binary is
; not a runnable oracle for this cart). The two real GGB81 dumps boot iff the
; mapper reproduces these tables. Treat this as a regression pin against those
; two references, not a first-principles silicon truth.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Truthful MBC5 header ($19): detection is by the $0184 logo CRC32, and decode
; falls through to this header type (rgbfix -v recomputes the header checksum;
; -p sets the real ROM-size byte over the placeholder here).
SECTION "header", ROM0[$147]
    db $19    ; MBC5
    db $04    ; ROM size placeholder (rgbfix -p sets the real value)
    db $00    ; RAM size: none

; Secondary Vast Fame logo at $0184: detection keys ONLY on the CRC32 of these
; 48 bytes, never their meaning. This is a CLEAN-ROOM stand-in — a plain ASCII
; banner (44 bytes) plus a 4-byte suffix computed so the block's CRC32 equals
; the GGB81 detection constant 0x79F3_4594 — so it carries the signature
; without embedding any copyrighted logo bytes.
SECTION "ggb81logo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $47, $47, $42, $38, $31, $20, $44
    db $41, $54, $41, $20, $43, $4C, $45, $41, $4E, $52, $4F, $4F, $4D, $20, $53, $54
    db $41, $4E, $44, $49, $4E, $20, $53, $49, $47, $47, $42, $21, $BC, $5B, $9C, $21

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. default swap mode 0 (identity): bank-1 offset 0 marker reads raw. ---
    ld a, $01
    ld [$2000], a        ; MBC5 low bank -> 1
    ld a, [$4000]
    cp $0F
    jp nz, .fail

    ; --- 2. latch swap mode 2 (addr & 0xF0FF == 0x2001). This write also drives
    ;     the MBC5 low-bank register to 2, so reselect bank 1 via $2000 (which
    ;     does not disturb the mode). GGB81 then reorders $0F -> $69; a plain
    ;     MBC5 leaves it $0F and fails here. ---
    ld a, $02
    ld [$2001], a        ; mode 2 (and bank -> 2)
    ld a, $01
    ld [$2000], a        ; reselect bank 1; mode stays 2
    ld a, [$4000]
    cp $69
    jp nz, .fail

    ; --- 3. bank 0 reads are never reordered, even with a mode active. ---
    ld a, [$0A00]
    cp $C3
    jp nz, .fail

    ; --- 4. returning to mode 0 restores the identity read. ---
    ld a, $00
    ld [$2001], a        ; mode 0 (and bank -> 0)
    ld a, $01
    ld [$2000], a        ; reselect bank 1
    ld a, [$4000]
    cp $0F
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank-0 marker (read through the $0A00 window; never reordered).
SECTION "bank0marker", ROM0[$0A00]
    db $C3

; Bank-1 marker at offset 0 (read through the fixed $4000 window; reordered
; when a swap mode is active).
SECTION "bank1marker", ROMX[$4000], BANK[1]
    db $0F

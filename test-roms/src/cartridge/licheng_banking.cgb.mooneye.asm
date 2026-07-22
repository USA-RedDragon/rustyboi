; licheng_banking.cgb.mooneye — LiCheng / Niutoude (Vast Fame family) mapper.
;
; The LiCheng carts (Rockman DX6/DX8, the "POKEMON YELLOW" bootleg, several
; csmGameEngine titles...) wear an MBC1 header ($01) but are electrically MBC5.
; The only deviation from MBC5 is that the board IGNORES bank-register writes in
; $2101-$2FFF: the games spray garbage there that would otherwise corrupt MBC5's
; low-8 ROM-bank register (mGBA `_GBLiCheng`). There is no data or address
; scrambling. rustyboi content-detects the board from the CRC32 of the 48-byte
; secondary logo at $0184 (0xD2B5_7657), then maps UnlMapper::LiCheng and
; decodes it as MBC5+RAM+BATTERY.
;
; This ROM asserts the three behaviours that separate LiCheng from the plain
; decodes it would otherwise fall into:
;   1. MBC5 8-bit banking — a single write of $21 to $2000 must reach bank 33.
;      A plain MBC1 (the header) 5-bit-masks $21 to bank 1, so it fails here.
;   2. $2101-$2FFF ignore — a garbage bank write to $2500 must NOT change the
;      selected bank. A plain MBC5 (or the MBC1 header) would latch it and fail.
;   3. the honored $2000-$2100 window still works — a write of $01 to $2000 must
;      reselect bank 1, proving we did not simply disable banking.
; Run as a plain MBC1/MBC5 (detection disabled) the ROM FAILS at step 1 — I
; confirmed this by temporarily disabling the LiCheng detection rule.
;
; PROVENANCE: mGBA-oracle-anchored (mGBA 0.10.5 at /usr/bin/mgba implements
; LiCheng as MBC5 + the $2101-$2FFF write-ignore) AND game-boot-anchored (the 9
; real LiCheng dumps boot iff the mapper reproduces this behaviour). This is a
; regression pin against those two references, not a silicon-bench oracle.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; The header type byte reads MBC1 ($01) exactly like the real LiCheng carts:
; detection is by the $0184 logo CRC32, and the loader must apply the MBC5
; override to an MBC1-typed cart. rgbfix (-v -C -p) recomputes the header
; checksum and pads/sets the ROM-size byte, so these bytes are safe to set here.
SECTION "header", ROM0[$147]
    db $01    ; MBC1 (the LiCheng header lie)
    db $05    ; ROM size placeholder (rgbfix -p sets the real value; 1MB here)
    db $01    ; RAM size: 2KB (matches the real carts)

; Secondary Vast Fame logo at $0184: detection keys ONLY on the CRC32 of these
; 48 bytes, never their meaning. This is a CLEAN-ROOM stand-in — a plain ASCII
; banner (44 bytes) plus a 4-byte suffix computed so the block's CRC32 equals
; the LiCheng detection constant 0xD2B5_7657 — so it carries the signature
; without embedding any copyrighted logo bytes.
SECTION "lclogo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $4C, $49, $43, $48, $45, $4E, $47
    db $20, $4E, $49, $55, $54, $4F, $55, $44, $45, $20, $43, $4C, $45, $41, $4E, $52
    db $4F, $4F, $4D, $20, $53, $54, $41, $4E, $44, $49, $4E, $21
    db $C9, $37, $57, $41   ; CRC32-forcing suffix -> 0xD2B5_7657

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. MBC5 8-bit ROM bank: a single $2000 write of $21 reaches bank 33.
    ;     (A plain MBC1 header would 5-bit-mask $21 down to bank 1 = $B1.) ---
    ld a, $21
    ld [$2000], a
    ld a, [$4000]
    cp $A5
    jp nz, .fail

    ; --- 2. $2101-$2FFF ignore: a garbage bank number written into this window
    ;     must be dropped, leaving bank 33 selected. (A plain MBC5 would latch
    ;     it and read the wrong bank.) ---
    ld a, $C3
    ld [$2500], a
    ld a, [$4000]
    cp $A5
    jp nz, .fail

    ; --- 3. the honored $2000-$2100 window still latches: reselect bank 1. ---
    ld a, $01
    ld [$2000], a
    ld a, [$4000]
    cp $B1
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank markers at offset 0 of each bank (read through the fixed $4000 window).
; Bank $21 (33) is only reachable via MBC5's 8-bit low bank register.
SECTION "bank1marker", ROMX[$4000], BANK[1]
    db $B1
SECTION "bank33marker", ROMX[$4000], BANK[$21]
    db $A5

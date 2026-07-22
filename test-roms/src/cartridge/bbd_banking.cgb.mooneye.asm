; bbd_banking.cgb.mooneye — BBD (Vast Fame family) bit-scrambling mapper.
;
; The BBD carts (Gedou Qi Long Qiu 2002 / "DRAGON BALL"...) are electrically an
; MBC5 whose $2000-$2FFF register block also carries a bit-scrambling protocol
; (mGBA `_GBBBD`): a write to $2001 latches the DATA swap mode, $2080 the BANK
; swap mode; the bank number written to $2000 is reordered through the current
; bank table before it latches, and every $4000-$7FFF ROM read is reordered
; through the current data table. rustyboi content-detects the board from the
; CRC32 of the 48-byte secondary logo at $0184 (0x6D1E_A662), gated on
; $7FFF != $01 (a matching $7FFF is a cracked dump that runs plain), then maps
; UnlMapper::Bbd and decodes it as MBC5+RAM+BATTERY.
;
; This ROM asserts the two transforms that separate BBD from a plain MBC5:
;   1. DATA reorder (mode 7 = Digimon, table [0,1,5,3,4,2,6,7]): with the mode
;      latched, a read of bank 1 offset 0 (raw $04) must descramble to $20.
;      A plain MBC5 returns the raw $04, so it fails here.
;   2. BANK reorder (mode 3 = [3,4,2,0,1,5,6,7]): with data reorder disabled,
;      writing $01 to $2000 must reorder bit0 -> bit3 and select bank 8, whose
;      offset-0 marker is $A5. A plain MBC5 would select bank 1 (marker $04)
;      and fail.
; Run as a plain MBC5 (detection disabled) the ROM FAILS at step 1 — I confirmed
; this by temporarily disabling the BBD detection rule.
;
; PROVENANCE: mGBA-oracle-anchored (mGBA 0.10.5 at /usr/bin/mgba implements BBD
; via `_GBBBD` / `_GBBBDRead` with these exact `_bbdDataReordering` /
; `_bbdBankReordering` tables) AND game-boot-anchored (the real BBD dump boots
; iff the mapper reproduces this behaviour). This is a regression pin against
; those two references, not a silicon-bench oracle.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Header: MBC5+RAM+BATTERY, matching the real cart's truthful $1B header. rgbfix
; (-v -C -p) recomputes the header checksum and sets the ROM-size byte; the RAM
; size and cart type are honoured from here.
SECTION "header", ROM0[$147]
    db $1B    ; MBC5+RAM+BATTERY
    db $03    ; ROM size placeholder (rgbfix -p sets the real value; 256KB here)
    db $02    ; RAM size: 8KB (matches the real cart)

; Secondary Vast Fame logo at $0184: detection keys ONLY on the CRC32 of these
; 48 bytes, never their meaning. This is a CLEAN-ROOM stand-in — a plain ASCII
; banner (44 bytes) plus a 4-byte suffix computed so the block's CRC32 equals
; the BBD detection constant 0x6D1E_A662 — so it carries the signature without
; embedding any copyrighted logo bytes.
SECTION "bbdlogo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $42, $42, $44, $20, $56, $41, $53
    db $54, $46, $41, $4D, $45, $20, $43, $4C, $45, $41, $4E, $52, $4F, $4F, $4D, $20
    db $53, $54, $41, $4E, $44, $49, $4E, $20, $42, $42, $44, $21   ; 44-byte ASCII banner
    db $1B, $F7, $14, $A4   ; CRC32-forcing suffix -> 0x6D1E_A662

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. DATA reorder, mode 7 (Digimon). Latch the mode, then re-select
    ;     bank 1 ($2001 also clobbers the MBC5 low bank register, per mGBA).
    ;     bank1[0] = $04 must descramble to $20. ---
    ld a, $07
    ld [$2001], a       ; dataSwapMode = 7
    ld a, $01
    ld [$2000], a       ; select bank 1 (bankSwapMode 0 = identity)
    ld a, [$4000]
    cp $20
    jp nz, .fail

    ; --- 2. BANK reorder, mode 3. Latch bankSwapMode=3, disable data reorder,
    ;     then write $01 to $2000: bit0 -> bit3 selects bank 8 (marker $A5).
    ;     A plain MBC5 would select bank 1 ($04) and fail. ---
    ld a, $03
    ld [$2080], a       ; bankSwapMode = 3
    xor a
    ld [$2001], a       ; dataSwapMode = 0 (identity: isolate the bank reorder)
    ld a, $01
    ld [$2000], a       ; reorder $01 -> bank 8
    ld a, [$4000]
    cp $A5
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank markers at offset 0, read through the fixed $4000 window.
SECTION "bank1marker", ROMX[$4000], BANK[1]
    db $04              ; mode-7 data reorder turns this into $20
; The $7FFF guard byte (file offset $7FFF = bank 1 offset $3FFF): != $01 marks a
; still-protected dump, exactly like the real cart's $7FFF = $08.
SECTION "bbdguard", ROMX[$7FFF], BANK[1]
    db $08
SECTION "bank8marker", ROMX[$4000], BANK[8]
    db $A5              ; mode-3 bank reorder target

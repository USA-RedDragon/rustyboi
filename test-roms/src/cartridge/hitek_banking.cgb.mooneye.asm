; hitek_banking.cgb.mooneye — HITEK (Vast Fame family) protected mapper.
;
; The HITEK carts (Terrifying 911, Shuihu Zhuan) are electrically a plain
; MBC5+RAM+BATTERY with two Vast Fame protections layered on top (mGBA
; `_GBHitek`): a boot-programmed DATA-swap mode bit-reorders every switchable
; -bank ($4000-$7FFF) ROM read, and a BANK-swap mode bit-reorders each
; bank-select value written to $2000. rustyboi content-detects the board from
; the CRC32 of the 48-byte secondary logo at $0184 (0x4FDA_B691, gated on
; $7FFF != $01 so a cracked dump is left as plain MBC5), then maps
; UnlMapper::Hitek and decodes it as MBC5+RAM+BATTERY.
;
; This ROM asserts the two behaviours that separate HITEK from the plain MBC5
; it would otherwise be:
;   1. BANK-bit reorder — with bank_swap_mode = 1, a $2000 write of $02 selects
;      bank 4 (reorder of $02 through HITEK_BANK_REORDERING[1]); data_swap_mode
;      is still 0, so the fixed $4000 window reads bank 4's raw marker $A4. A
;      plain MBC5 selects bank 2 and reads $B2, so it fails here.
;   2. DATA-bit reorder — with data_swap_mode = 1 (the $2001 write also reselects
;      bank 1 with the raw value), bank 1's marker $B1 reads back reordered to
;      $95 (reorder of $B1 through HITEK_DATA_REORDERING[1]). A plain MBC5 reads
;      the raw $B1, so it fails here too.
; Run as a plain MBC5 (detection disabled) the ROM FAILS at step 1 — I
; confirmed this against the equivalent Rust unit test with the mapper removed.
;
; mGBA also lists a `case 0x300` early-return in `_GBHitek`, but `addr & 0xF0FF`
; can never be 0x300 (bits 8-9 are masked out), so it is dead on hardware too
; and there is nothing observable to assert for it.
;
; PROVENANCE: mGBA-oracle-anchored (mGBA 0.10.5 at /usr/bin/mgba implements
; HITEK; the reorder tables in the core are its `_hitekBankReordering` /
; `_hitekDataReordering`) AND game-boot-anchored (the two real HITEK dumps boot
; iff the mapper reproduces this behaviour). This is a regression pin against
; those two references, not a silicon-bench oracle.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; The header type byte reads MBC5+RAM+BATTERY ($1B), which is truthful on the
; real HITEK carts: detection is by the $0184 logo CRC32, and the board is
; electrically MBC5. rgbfix (-v -C -p) recomputes the header checksum and writes
; the Nintendo logo, so these bytes are safe to set here.
SECTION "header", ROM0[$147]
    db $1B    ; MBC5+RAM+BATTERY
    db $03    ; ROM size placeholder (rgbfix normalises to the padded size)
    db $03    ; RAM size: 32KB (matches the real carts)

; Secondary Vast Fame logo at $0184: detection keys ONLY on the CRC32 of these
; 48 bytes, never their meaning. This is a CLEAN-ROOM stand-in — a plain ASCII
; banner (44 bytes) plus a 4-byte suffix computed so the block's CRC32 equals
; the HITEK detection constant 0x4FDA_B691 — so it carries the signature
; without embedding any copyrighted logo bytes.
SECTION "hitlogo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $48, $49, $54, $45, $4B, $20, $56
    db $41, $53, $54, $46, $41, $4D, $45, $20, $43, $4C, $45, $41, $4E, $52, $4F, $4F
    db $4D, $20, $53, $54, $41, $4E, $44, $49, $4E, $21, $21, $21
    db $8C, $13, $02, $75   ; CRC32-forcing suffix -> 0x4FDA_B691

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. BANK-bit reorder: bank_swap_mode = 1, then a $2000 write of $02
    ;     selects bank 4. data_swap_mode is still 0, so the read is the raw
    ;     bank-4 marker $A4. (A plain MBC5 selects bank 2 and reads $B2.) ---
    ld a, $01
    ld [$2080], a      ; bank_swap_mode = 1
    ld a, $02
    ld [$2000], a      ; reorder($02, BANK[1]) = 4 -> bank 4
    ld a, [$4000]
    cp $A4
    jp nz, .fail

    ; --- 2. DATA-bit reorder: data_swap_mode = 1 (the $2001 write also reselects
    ;     bank 1 with the raw value); bank 1's marker $B1 reads back reordered to
    ;     $95. (A plain MBC5 reads the raw $B1.) ---
    ld a, $01
    ld [$2001], a      ; data_swap_mode = 1, bank_low = 1
    ld a, [$4000]
    cp $95
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank markers at offset 0 of each bank (read through the fixed $4000 window).
; Bank 4 is only reached via the HITEK bank-bit reorder of a $02 write; bank 2
; is the bank a plain MBC5 would land on instead.
SECTION "bank1marker", ROMX[$4000], BANK[1]
    db $B1
SECTION "bank2marker", ROMX[$4000], BANK[2]
    db $B2
SECTION "bank4marker", ROMX[$4000], BANK[4]
    db $A4

; sintax_scramble.cgb.mooneye — Sintax (Vast Fame family) data-scramble mapper.
;
; The Sintax carts (Menghuan Moni Zhan II, Langrisser II, ...) are electrically
; MBC5 plus a boot-programmed scramble driven by three register windows
; (mGBA `_GBSintax`):
;   $5x1x           select a 4-bit ROM-bank reorder mode.
;   $2xxx           bank number, bit-permuted through the active mode's table
;                   into the MBC5 low-8 bank register; the RAW value's low 2 bits
;                   also pick one of four XOR bytes.
;   $7020/30/40/50  program the four per-bank XOR bytes.
; Reads of the switchable $4000-$7FFF window return `rom_byte XOR active_xor`;
; bank 0 is never scrambled. rustyboi content-detects the board from the CRC32
; of the 48-byte secondary logo at $0184 (0x6C1D_CF2D) with mGBA's "not a fixed
; dump" guard ($7FFF != $01), then maps UnlMapper::Sintax and decodes MBC5.
;
; This ROM EXERCISES THE SCRAMBLE, not just banking. It:
;   1. sets reorder mode 1 ($5010 <- $01),
;   2. writes RAW bank $04 to $2000. Under mode 1 the reorder table
;      [3,2,5,4,7,6,1,0] permutes $04 -> physical bank $02 (a plain MBC5 would
;      select bank $04 verbatim),
;   3. programs XOR byte 0 ($7020 <- $5A). RAW bank $04 & 3 = 0 selects it,
;   4. reads $4000. Physical bank 2 stores the SCRAMBLED byte $9C; the mapper
;      XORs it with the key $5A, so a correct Sintax read DESCRAMBLES to $C6.
; PASS requires BOTH the bank reorder (bank 2, not 4) AND the read XOR. On a
; plain MBC5 the $5010/$7020 writes are inert, $2000 selects physical bank 4
; (which stores $3D), and no XOR is applied -> the read is $3D, not $C6 -> FAIL.
; Confirmed FAIL-without-mapper by disabling the Sintax detection rule.
;
; PROVENANCE: mGBA-source-anchored. The reorder table, the $5x1x/$2xxx/$7xxx
; register protocol, and the $4000-$7FFF read XOR are a verbatim port of mGBA's
; `_GBSintax` / `_GBSintaxRead` / `_sintaxReordering[16][8]` and its
; `_detectUnlMBC` CRC32 constants + $7FFF guard (master branch; note Sintax
; support POSTDATES the locally-installed mGBA 0.10.5, which lacks it and would
; run these dumps as plain MBC5). Also game-boot-anchored: the 2 real Sintax
; dumps (Menghuan Moni Zhan II, Langrisser II) flip from a static/blank frame to
; live gameplay iff the mapper reproduces this behaviour. A regression pin
; against those references, not a silicon-bench oracle.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Header type reads MBC5+RAM+BATTERY, exactly like the real Sintax carts (their
; $147 is a truthful $1B). rgbfix -v recomputes the header checksum over these
; bytes, so setting them here is safe.
SECTION "header", ROM0[$147]
    db $1B    ; MBC5+RAM+BATTERY
    db $02    ; ROM size: 128KB / 8 banks (bank 4 marker below)
    db $03    ; RAM size: 32KB (matches the real carts)

; Secondary Vast Fame logo at $0184: detection keys ONLY on the CRC32 of these
; 48 bytes, never their meaning. This is a CLEAN-ROOM stand-in — a plain ASCII
; banner (44 bytes) plus a 4-byte suffix computed so the block's CRC32 equals
; the Sintax detection constant 0x6C1D_CF2D — so it carries the signature
; without embedding any copyrighted logo bytes.
SECTION "sxlogo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $53, $49, $4E, $54, $41, $58, $20
    db $56, $41, $53, $54, $46, $41, $4D, $45, $20, $43, $4C, $45, $41, $4E, $52, $4F
    db $4F, $4D, $20, $53, $54, $41, $4E, $44, $49, $4E, $21, $21   ; "...STANDIN!!"
    db $FE, $DF, $1B, $3C   ; CRC32-forcing suffix -> 0x6C1D_CF2D

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. select reorder mode 1 via a $5x1x write. ---
    ld a, $01
    ld [$5010], a

    ; --- 2. write RAW bank $04. Mode 1 permutes it to physical bank $02;
    ;        a plain MBC5 would select bank $04 (which stores $3D). ---
    ld a, $04
    ld [$2000], a

    ; --- 3. program XOR byte 0 ($7020). RAW bank $04 & 3 = 0 selects it. ---
    ld a, $5A
    ld [$7020], a

    ; --- 4. read $4000: physical bank 2 stores $9C; the mapper XORs the key
    ;        $5A, descrambling to $C6. ---
    ld a, [$4000]
    cp $C6
    jp nz, .fail

    ; Re-program the XOR to a different key and re-read: proves the read XOR is
    ; live per the register, not a one-off. Bank 2 byte $9C ^ $FF = $63.
    ld a, $FF
    ld [$7020], a
    ld a, [$4000]
    cp $63
    jp nz, .fail

    test_success
.fail:
    test_failure

; Protected-dump guard byte at file offset $7FFF (bank 1's last byte). The real
; Sintax dumps read $7B here; detection requires it != $01 (a "fixed"/cracked
; dump). Set explicitly so detection does not depend on the linker's gap fill.
SECTION "guardbyte", ROMX[$7FFF], BANK[1]
    db $7B

; Physical bank 2 holds the SCRAMBLED byte ($C6 ^ $5A = $9C): the value a
; correct Sintax read must descramble back to $C6.
SECTION "bank2scrambled", ROMX[$4000], BANK[2]
    db $9C

; Physical bank 4 holds a distinct marker ($3D): the bank a plain MBC5 selects
; from the un-reordered $04 write, so an unscrambled decode reads $3D != $C6.
SECTION "bank4plain", ROMX[$4000], BANK[4]
    db $3D

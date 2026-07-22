; vf001_protection.cgb.mooneye — Vast Fame VF001-class protection register file.
;
; Legend of Heroes (Unl) ships on a Vast Fame board that is electrically a
; normal MBC5+RAM+BATTERY plus a 4-port protection register file decoded from
; A10-A11: config writes land at $7080/$7480/$7880/$7C80 and derived values
; read back through the cart-RAM window at $A000/$A800/$AFFF. Port 0 is a
; command port (its last three bytes form the command); ports 1-3 latch a
; "select" byte that picks which derived value the next protection read serves.
; rustyboi content-detects the board from the $0184 secondary-logo sum (4593)
; plus the boot protection stub bytes at $32FC (both embedded below), then maps
; UnlMapper::Vf001. This ROM drives the exact RE'd op table the game's three
; boot gates rely on and asserts every served value.
;
; On a plain MBC5 (protection layer absent/wrong) the config writes are inert
; and the $Axxx reads return open-bus / RAM instead of the transform values, so
; every assertion fails — the ROM PASSES only with the VF001 mapper active.
;
; PROVENANCE: this is a GAME-BOOT-ANCHORED regression pin, not a silicon-bench
; oracle. The served values are the reverse-engineered op table the real Legend
; of Heroes (Unl) cart drives through these exact three boot gates on hardware —
; the game boots iff the mapper reproduces them — but they have not been
; independently confirmed on a logic-analyser bench. Treat it as a guard that
; the game keeps booting, not as a first-principles hardware truth.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; The header type byte must read MBC5+RAM+BATTERY so `from_bytes` sizes the RAM
; and battery domain like the real cart (rgbfix -v recomputes the header
; checksum over these bytes, so setting them here is safe).
SECTION "header", ROM0[$147]
    db $1B    ; MBC5+RAM+BATTERY
    db $03    ; ROM size: 128KB / 8 banks (bank 6 marker below)
    db $02    ; RAM size: 8KB

; Secondary Vast Fame logo at $0184: detection keys ONLY on the 48-byte sum
; (4593), never the individual bytes, so this stand-in carries the sum without
; embedding any copyrighted logo. 47*$60 + $51 = 4512 + 81 = 4593.
SECTION "vflogo", ROM0[$184]
    REPT 47
    db $60
    ENDR
    db $51

; Boot protection stub signature at $32FC (`ld de,$7080; ld a,$9a; ld [de],a`).
; Detection requires these exact bytes; they are data here, never executed.
SECTION "vfstub", ROM0[$32FC]
    db $11, $80, $70, $3E, $9A, $12

SECTION "main", ROM0[$1000]
Start:
    ; --- Boot gate: command $9A,$B8,$B9 on port 0 ($7080) ---
    ld a, $9A
    ld [$7080], a
    ld a, $B8
    ld [$7080], a
    ld a, $B9
    ld [$7080], a
    ; select $B9 (port 1, $7480) -> $A800 serves $C1
    ld a, $B9
    ld [$7480], a
    ld a, [$A800]
    cp $C1
    jp nz, .fail
    ; select $83 -> $A800 serves $F8
    ld a, $83
    ld [$7480], a
    ld a, [$A800]
    cp $F8
    jp nz, .fail

    ; --- Second gate: command $37,$52,$CD on port 0 ---
    ld a, $37
    ld [$7080], a
    ld a, $52
    ld [$7080], a
    ld a, $CD
    ld [$7080], a
    ; select $BA (port 2, $7880) -> $A800 serves $82
    ld a, $BA
    ld [$7880], a
    ld a, [$A800]
    cp $82
    jp nz, .fail
    ; select $A9 -> $A800 serves $8F
    ld a, $A9
    ld [$7880], a
    ld a, [$A800]
    cp $8F
    jp nz, .fail

    ; --- Bank-switch command $7E,$29,$79: drives the MBC5 bank to 6 and the
    ;     port-3 window ($AFFF) serves the decoy constant $31. ---
    ld a, $7E
    ld [$7080], a
    ld a, $29
    ld [$7080], a
    ld a, $79
    ld [$7080], a
    ld a, [$AFFF]
    cp $31
    jp nz, .fail
    ; The command also selected ROM bank 6: the fixed $4000 window now reads
    ; bank 6's marker byte (placed at $18000 + $0000 below).
    ld a, [$4000]
    cp $B6
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank 6 marker: proves the $7E,$29,$79 command really re-banked the MBC5 ROM
; window (only the protection side effect selects bank 6).
SECTION "bank6marker", ROMX[$4000], BANK[6]
    db $B6

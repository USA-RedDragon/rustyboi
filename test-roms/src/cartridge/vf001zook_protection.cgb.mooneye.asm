; vf001zook_protection.cgb.mooneye — Vast Fame VF001, challenge-response dialect.
;
; Zook Z (USA) (Unl) ships on a Vast Fame board that wears the same "V.fame"
; $0184 secondary logo as the VF001 carts hhugboy calls `MbcUnlVf001`, but is
; driven through a completely different protocol. Instead of hhugboy's $7000
; config-register file, the board presents ONE protection port across
; $6000-$7FFF that behaves as a byte shift register, and answers in two ways:
;
;   * BANK SELECT — four consecutive bytes written to $7081 make the board
;     switch the MBC5 ROM-bank register. The cart's two thunks ($3ED9/$3EF5)
;     emit exactly that, then `ld a,($7FFF)` to read the selected bank back
;     (every bank's last byte is its own number on this cart).
;   * CHALLENGE-RESPONSE — a byte stream written to the port, then a read of
;     $A080/$A180/$A280/$A380/$A680/$A880 (the register is A8-A11 of the
;     address) that returns the board's answer for that stream.
;
; rustyboi content-detects the board from the CRC32 of the 48 bytes at $0184
; plus the exact opcode bytes of the $3EF5 bank thunk (both embedded below),
; then maps UnlMapper::Vf001Zook. Everything else on the board is plain MBC5 —
; the cart's own `rst $28` is a bare `ld ($2000),a`.
;
; This ROM asserts the protocol with `ld a,[nn]` DATA reads only. It never
; jumps into a banked or protection-served byte, so nothing here depends on how
; many cartridge reads the CPU issues per instruction fetch.
;
;   1. a bank select really re-banks: three different four-byte challenges put
;      three different banks in the $4000 window, read back as marker bytes.
;   2. the FOURTH byte of a bank challenge is ignored — two challenges that
;      differ only there select the same bank.
;   3. $31 is NOT a reset: it is the byte the cart writes to close every
;      sequence, but a bank challenge whose key CONTAINS $31 still works, so
;      the port's shift register must survive it.
;   4. challenge-response reads answer on four different registers, and the
;      register is part of the key (A8-A11 select it).
;
; Run without the board (detection disabled) the cart decodes from its MBC1
; header: the $7081 writes become an inert MBC1 banking-mode register so the
; $4000 window never leaves bank 1, and the $Axxx reads see no cart RAM and
; return open bus — every assertion below fails. Verified by temporarily
; disabling the Vf001Zook detection rule.
;
; PROVENANCE: GAME-ANCHORED OBSERVATION, NOT A SILICON MODEL. Every value
; asserted here comes from diffing Zook Z against Rockman DX8 (China) (En)
; (Unl) — a 99.7%-byte-identical, DE-PROTECTED build of the same game in which
; each protection site is replaced by the plain instruction it stands in for
; (`ld a,bank / call $3EF1` for a bank select, `ld a,value` for a challenge).
; The de-protected build therefore states outright what the board must answer.
; The board's decode FUNCTION is unsolved: see VF001Z_BANK_RESPONSES in
; rustyboi-core/src/cartridge/unlicensed.rs for the model classes that were
; exhaustively eliminated. Treat this as a pin that the observed responses stay
; wired up, not as first-principles hardware truth.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Header: the real cart lies with an MBC1 type byte over an electrically-MBC5
; board, and declares no RAM. Mirrored here so the "detection disabled" run
; degrades exactly the way the real cart does (rgbfix -v recomputes the header
; checksum over these bytes).
SECTION "header", ROM0[$147]
    db $01    ; MBC1 (a lie the board does not honour)
    db $04    ; ROM size: 256KB / 16 banks (bank $0B marker below)
    db $00    ; RAM size: none

; Secondary Vast Fame logo at $0184. Detection keys ONLY on the CRC32 of these
; 48 bytes, never their meaning, so this is a CLEAN-ROOM stand-in: the ASCII
; banner "RUSTYBOI VF001 ZOOK CLEANROOM STANDIN SIG!!!" (44 bytes) plus a
; 4-byte suffix computed so the block's CRC32 equals the detection constant
; 0x42B7_73B8. No copyrighted logo bytes are embedded.
SECTION "vfzlogo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $56, $46, $30, $30, $31, $20, $5A
    db $4F, $4F, $4B, $20, $43, $4C, $45, $41, $4E, $52, $4F, $4F, $4D, $20, $53, $54
    db $41, $4E, $44, $49, $4E, $20, $53, $49, $47, $21, $21, $21, $DF, $D4, $43, $D7

; The bank-select thunk at $3EF5 that the detection also requires:
;   ld hl,$7081 / (ld a,(de) / ld (hl),a) x4 / ld a,($7FFF)
; Data here, never executed — this ROM drives the port with its own stores.
SECTION "vfzthunk", ROM0[$3EF5]
    db $21, $81, $70
    db $1A, $77, $13, $1A, $77, $13, $1A, $77, $13, $1A, $77
    db $FA, $FF, $7F

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. bank select: $46,$58,$54,$5F -> bank 4 ---
    ; Deliberately never bank 1 first: an undetected cart sits on bank 1, so a
    ; bank-1 assertion could pass for the wrong reason.
    ld a, $46
    ld [$7081], a
    ld a, $58
    ld [$7081], a
    ld a, $54
    ld [$7081], a
    ld a, $5F
    ld [$7081], a
    ld a, [$4000]
    cp $B4
    jp nz, .fail

    ; $A4,$BA,$D5,$44 -> bank 2
    ld a, $A4
    ld [$7081], a
    ld a, $BA
    ld [$7081], a
    ld a, $D5
    ld [$7081], a
    ld a, $44
    ld [$7081], a
    ld a, [$4000]
    cp $B2
    jp nz, .fail

    ; $34,$40,$5A,$33 -> bank 3
    ld a, $34
    ld [$7081], a
    ld a, $40
    ld [$7081], a
    ld a, $5A
    ld [$7081], a
    ld a, $33
    ld [$7081], a
    ld a, [$4000]
    cp $B3
    jp nz, .fail

    ; --- 2+3. $0A,$31,$18,$57 -> bank $0B. The key CONTAINS $31, the byte the
    ; cart writes to close every protection sequence, so a port that treated
    ; $31 as a reset could not answer this at all. ---
    ld a, $0A
    ld [$7081], a
    ld a, $31
    ld [$7081], a
    ld a, $18
    ld [$7081], a
    ld a, $57
    ld [$7081], a
    ld a, [$4000]
    cp $BB
    jp nz, .fail

    ; Back to bank 2, so the repeat below has somewhere to move from.
    ld a, $A4
    ld [$7081], a
    ld a, $BA
    ld [$7081], a
    ld a, $D5
    ld [$7081], a
    ld a, $44
    ld [$7081], a
    ld a, [$4000]
    cp $B2
    jp nz, .fail

    ; Same key, DIFFERENT fourth byte ($99 instead of $57): still bank $0B, so
    ; the fourth byte is not part of the key.
    ld a, $0A
    ld [$7081], a
    ld a, $31
    ld [$7081], a
    ld a, $18
    ld [$7081], a
    ld a, $99
    ld [$7081], a
    ld a, [$4000]
    cp $BB
    jp nz, .fail

    ; --- 4. challenge-response on four registers ---
    ; stream $A8,$B6 -> register 0 ($A080) answers $6E
    ld a, $A8
    ld [$7080], a
    ld a, $B6
    ld [$7080], a
    ld a, [$A080]
    cp $6E
    jp nz, .fail

    ; stream $20,$96 -> register 0 answers $19
    ld a, $20
    ld [$7080], a
    ld a, $96
    ld [$7080], a
    ld a, [$A080]
    cp $19
    jp nz, .fail

    ; stream $77,$13,$B4 -> register 6 ($A680) answers $22
    ld a, $77
    ld [$7A80], a
    ld a, $13
    ld [$7A80], a
    ld a, $B4
    ld [$7A80], a
    ld a, [$A680]
    cp $22
    jp nz, .fail

    ; stream $11,$81,$70,$F7,$EA,$98 -> register 8 ($A880) answers $D0
    ld a, $11
    ld [$7B80], a
    ld a, $81
    ld [$7B80], a
    ld a, $70
    ld [$7B80], a
    ld a, $F7
    ld [$7B80], a
    ld a, $EA
    ld [$7B80], a
    ld a, $98
    ld [$7B80], a
    ld a, [$A880]
    cp $D0
    jp nz, .fail

    ; stream $87,$5F,$16,$82 -> register 3 ($A380) answers $66
    ld a, $87
    ld [$7F80], a
    ld a, $5F
    ld [$7F80], a
    ld a, $16
    ld [$7F80], a
    ld a, $82
    ld [$7F80], a
    ld a, [$A380]
    cp $66
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank markers. Only the protection port can select these banks: nothing here
; ever writes the MBC bank register, so reading the right marker proves the
; board answered the challenge.
SECTION "bank2marker", ROMX[$4000], BANK[2]
    db $B2
SECTION "bank3marker", ROMX[$4000], BANK[3]
    db $B3
SECTION "bank4marker", ROMX[$4000], BANK[4]
    db $B4
SECTION "bankBmarker", ROMX[$4000], BANK[11]
    db $BB

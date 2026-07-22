; vf001gen_protection.cgb.mooneye — general Vast Fame VF001 protection board.
;
; The MBC5-header Vast Fame carts Nv Wang Gedou 2000 and the Gedou Jian Shen
; (Soul Falchion) pair ship on taizou's "VF001" board (hhugboy MbcUnlVf001, CC0):
; electrically MBC5 plus a $6000-$7FFF config register file decoded `addr&$F00F`
; and driven by a rotate-right-then-XOR running accumulator that the boot code
; seeds by writing $96 to $7000. Two protection effects hang off the latched
; config: (1) a byte-sequence injection — a read of a configured (bank,address)
; makes that read and the next few ROM reads return programmed bytes instead of
; the real ROM contents; (2) a bank-0 partial replacement — reads of bank 0 from
; a configured address on are served from a configured high bank. rustyboi
; content-detects the board from the $0184 secondary-logo CRC32 (0x42B7_73B8,
; the "V.fame" logo, embedded below) plus an MBC5-family header, and maps
; UnlMapper::Vf001Gen.
;
; This ROM drives the exact accumulator protocol and asserts BOTH effects with
; DATA reads (`ld a,[nn]`), never by executing injected code:
;   * injection: $3E00 physically holds $A5, but a 1-byte injection is armed at
;     bank 0 $3E00 programming $5A, so the read returns $5A.
;   * replacement: bank 0 $3F00 physically holds $AA, bank 2's corresponding
;     byte holds $55, and the board is configured to overlay bank 2 from $3F00,
;     so the read returns $55.
; Deliberately DATA reads: the injection consumes one programmed byte per ROM
; read, so an injected multi-byte *instruction* would depend on exactly how many
; cartridge reads the CPU issues per fetch — an emulator-internal detail, not
; board behaviour. A single-byte injection observed through one `ld a,[nn]` is
; read-count independent and tests the board itself.
;
; On a plain MBC5 the $6000-$7FFF config writes are inert, so $3E00 reads $A5
; and $3F00 reads $AA — both assertions fail. The ROM PASSES only with the
; Vf001Gen mapper active.
;
; PROVENANCE: the accumulator + two effects are a faithful port of taizou's CC0
; hhugboy MbcUnlVf001 algorithm (no copyrighted bytes; the $0184 block is an
; ASCII banner plus a computed 4-byte CRC32-forcing suffix). The board is proven
; against real cart behaviour by Nv Wang Gedou 2000 and the Soul Falchion pair
; booting to real gameplay iff the mapper reproduces it; it has not been captured
; on a logic-analyser bench. Treat as a game-boot-anchored regression pin for the
; protocol, not a first-principles silicon oracle.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Header: MBC5 ($19), matching the Nv Wang / Soul Falchion header family that the
; Vf001Gen detection gates on ($19-$1E). rgbfix -v recomputes the checksum.
SECTION "header", ROM0[$147]
    db $19    ; MBC5
    db $01    ; ROM size: 64KB / 4 banks (bank 2 is the replacement source)
    db $00    ; RAM size: none

; Secondary Vast Fame logo at $0184: detection keys ONLY on the 48-byte CRC32
; (0x42B7_73B8), never the individual bytes, so this is an ASCII banner (44
; bytes) plus a 4-byte suffix computed so the block's CRC32 equals the "V.fame"
; constant — no copyrighted logo bytes. (Byte sum 3252, distinct from the 4593
; the Legend-of-Heroes VF001 board keys on, so the two never cross-detect.)
SECTION "vflogo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $56, $46, $30, $30, $31, $20, $43
    db $4C, $45, $41, $4E, $52, $4F, $4F, $4D, $20, $56, $41, $53, $54, $2D, $46, $41
    db $4D, $45, $20, $53, $49, $47, $21, $21, $21, $21, $21, $21
    db $8C, $9A, $74, $1F   ; CRC32-forcing suffix -> 0x42B7_73B8

SECTION "main", ROM0[$1000]
Start:
    ; --- open config mode (seeds the running accumulator to 0) ---
    ld a, $96
    ld [$7000], a

    ; --- program a 1-byte injection: bank 0 $3E00 -> $5A. Each data byte below
    ;     is computed so the rotate-right-then-XOR accumulator latches the
    ;     intended value into the addressed $700x port. ---
    ld a, $00
    ld [$7001], a   ; -> $00  cur700x[1] = seq addr lo ($3E00)
    ld a, $3E
    ld [$7002], a   ; -> $3E  cur700x[2] = seq addr hi
    ld a, $1F
    ld [$7003], a   ; -> $00  cur700x[3] = seq start bank 0
    ld a, $5A
    ld [$7004], a   ; -> $5A  cur700x[4] = injected byte
    ld a, $29
    ld [$7000], a   ; -> $04  cur700x[0] = len cmd 4 (len 1); ACTIVATE injection

    ; --- program bank-0 replacement: overlay bank 2 from $3F00 on ---
    ld a, $02
    ld [$7009], a   ; -> $00  cur700x[9]  = replace addr lo ($3F00)
    ld a, $3F
    ld [$700A], a   ; -> $3F  cur700x[10] = replace addr hi
    ld a, $9D
    ld [$6000], a   ; -> $02  cur6000     = replace source bank 2
    ld a, $0E
    ld [$7008], a   ; -> $0F  cur700x[8]  = enable; ACTIVATE replacement

    ; --- close config mode ---
    ld a, $96
    ld [$700F], a

    ; --- injection: one data read of $3E00 must return the programmed $5A,
    ;     not the physical $A5. ---
    ld a, [$3E00]
    cp $5A
    jp nz, FailPath

    ; --- replacement: $3F00 must read bank 2's $55, not bank 0's physical $AA.
    ld a, [$3F00]
    cp $55
    jp nz, FailPath

    test_success

SECTION "failsec", ROM0[$2100]
FailPath:
    test_failure

; Physical bank-0 byte at the injection address (returned when no board is
; present, so a plain MBC5 fails the first assertion).
SECTION "inj_target", ROM0[$3E00]
    db $A5

; Physical bank-0 byte at the replacement address (must differ from bank 2's).
SECTION "repl_bank0", ROM0[$3F00]
    db $AA

; Replacement source: bank 2's byte for bank-0 address $3F00. Bank 2 spans file
; offset $8000-$BFFF and is addressed at $4000-$7FFF, so bank-0 $3F00 maps to
; file $8000+$3F00 = $BF00 = ROMX $7F00 in BANK[2].
SECTION "repl_src", ROMX[$7F00], BANK[2]
    db $55

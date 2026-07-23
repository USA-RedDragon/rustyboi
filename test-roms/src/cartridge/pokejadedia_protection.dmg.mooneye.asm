; pokejadedia_protection.dmg.mooneye — "Pokemon Jade / Diamond" protection board.
;
; The Telefang bootlegs (Pokemon Jade, Koudai Guaishou Da Jihe) ship on a board
; that is electrically MBC3+TIMER+RAM+BATTERY (header type $10, RTC left
; unpopulated) with a weak three-register challenge handshake layered over
; MBC3's otherwise-unused RTC-register-select register (taizou's hhugboy
; MbcUnlPokeJadeDia, CC0; mGBA _GBPKJD):
;
;   * A write to $4000-$5FFF latches a "register selector" from the value (this
;     also drives MBC3's own RAM/RTC-bank register, so plain SRAM/RTC banking is
;     unaffected).
;   * In the $A000-$BFFF window, while RAM is enabled: selector $0D reads back /
;     writes register D, $0E register E, and $0F is a write-only command port
;     whose value mutates D and E ($11 D--, $12 E--, $41 D+=E, $42 E+=D, $51 D++,
;     $52 E--). Reads of the real (unpopulated) RTC registers $08-$0C return 0.
;
; rustyboi content-detects the board from the header type $10 plus the 48-byte
; $0184 CRC32 signature (0x65BBF1FC), and maps UnlMapper::PokeJadeDia.
;
; This ROM drives the D/E/F protocol and asserts every register effect with DATA
; reads (`ld a,[$A000]`), never by executing injected code. On a plain MBC3 the
; selector $0D/$0E/$0F just picks a nonexistent RAM/RTC bank, so $A000 reads back
; open bus ($FF) and the very first assertion fails; the ROM PASSES only with the
; PokeJadeDia mapper active.
;
; PROVENANCE: the D/E/F protocol is a faithful port of taizou's CC0 hhugboy
; MbcUnlPokeJadeDia (also matching mGBA's `_GBPKJD`); no copyrighted bytes (the
; $0184 block is an ASCII banner plus a computed 4-byte CRC32-forcing suffix).
; The board is proven against real cart behaviour by Pokemon Jade booting to real
; gameplay iff the mapper reproduces it. Treat as a game-boot-anchored regression
; pin for the protocol, not a logic-analyser silicon oracle.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Header: MBC3+TIMER+RAM+BATTERY ($10, truthful), 32KB ROM, 32KB RAM (4 banks),
; matching the real Telefang carts' class. rgbfix -v recomputes the checksum.
SECTION "header", ROM0[$147]
    db $10    ; MBC3+TIMER+RAM+BATTERY
    db $00    ; ROM size: 32KB / 2 banks
    db $03    ; RAM size: 32KB / 4 banks

; Secondary signature at $0184: detection keys ONLY on the 48-byte CRC32
; (0x65BBF1FC), never the individual bytes, so this is an ASCII banner (44 bytes)
; plus a 4-byte suffix computed so the block's CRC32 equals the PKJD constant --
; no copyrighted bytes.
SECTION "pkjdsig", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $43, $4C, $45, $41, $4E, $52, $4F
    db $4F, $4D, $20, $50, $4B, $4A, $44, $20, $54, $45, $4C, $45, $46, $41, $4E, $47
    db $20, $44, $45, $46, $20, $52, $45, $47, $53, $20, $21, $21, $74, $F4, $F7, $90

SECTION "main", ROM0[$1000]
Start:
    ; enable cart RAM (RAMG = $0A) so the protection window is live
    ld a, $0A
    ld [$0000], a

    ; --- register D round-trip: select D ($0D), write $5A, read it back ---
    ld a, $0D
    ld [$4000], a
    ld a, $5A
    ld [$A000], a
    ld a, [$A000]
    cp $5A
    jp nz, FailPath

    ; --- register E round-trip: select E ($0E), write $2D, read it back ---
    ld a, $0E
    ld [$4000], a
    ld a, $2D
    ld [$A000], a
    ld a, [$A000]
    cp $2D
    jp nz, FailPath

    ; --- D must be unchanged by the E write ---
    ld a, $0D
    ld [$4000], a
    ld a, [$A000]
    cp $5A
    jp nz, FailPath

    ; --- command $41 (D += E): $5A + $2D = $87 ---
    ld a, $0F
    ld [$4000], a
    ld a, $41
    ld [$A000], a
    ld a, $0D
    ld [$4000], a
    ld a, [$A000]
    cp $87
    jp nz, FailPath

    ; --- command $51 (D++): $87 + 1 = $88 ---
    ld a, $0F
    ld [$4000], a
    ld a, $51
    ld [$A000], a
    ld a, $0D
    ld [$4000], a
    ld a, [$A000]
    cp $88
    jp nz, FailPath

    ; --- command $12 (E--): $2D - 1 = $2C ---
    ld a, $0F
    ld [$4000], a
    ld a, $12
    ld [$A000], a
    ld a, $0E
    ld [$4000], a
    ld a, [$A000]
    cp $2C
    jp nz, FailPath

    ; --- command $42 (E += D): $2C + $88 = $B4 ---
    ld a, $0F
    ld [$4000], a
    ld a, $42
    ld [$A000], a
    ld a, $0E
    ld [$4000], a
    ld a, [$A000]
    cp $B4
    jp nz, FailPath

    ; --- command $11 (D--): $88 - 1 = $87 ---
    ld a, $0F
    ld [$4000], a
    ld a, $11
    ld [$A000], a
    ld a, $0D
    ld [$4000], a
    ld a, [$A000]
    cp $87
    jp nz, FailPath

    ; --- F is write-only: reads return 0 ---
    ld a, $0F
    ld [$4000], a
    ld a, [$A000]
    cp $00
    jp nz, FailPath

    ; --- the real RTC registers $08-$0C are unpopulated: reads return 0 ---
    ld a, $08
    ld [$4000], a
    ld a, [$A000]
    cp $00
    jp nz, FailPath

    test_success

SECTION "failsec", ROM0[$2100]
FailPath:
    test_failure

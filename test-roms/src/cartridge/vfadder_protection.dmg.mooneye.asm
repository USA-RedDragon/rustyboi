; vfadder_protection.dmg.mooneye — Vast Fame "operand adder" protection board.
;
; Dragon Ball - Final Bout (Taiwan) is a DECRYPTED BBD-family dump: the last
; byte of every bank equals that bank's own number, so $7FFF==$01 / $BFFF==$02
; and both hhugboy and mGBA correctly decline to re-apply the BBD data/bank
; scramble. What neither of them models is that the cart's protection chip is
; still live and unpatched, so on a plain MBC5 the boot builds a garbage jump
; target and dies executing WRAM.
;
; The entire protocol is one 40-byte thunk at $3F50:
;     ld a,[$7FFF] / push af,de,hl / ld de,$4000 / ld hl,$2300
;     ld a,$C0 / ld [hl],a          ; bank register := $C0
;     ld a,b   / ld [de],a          ; $4000 <- operand X
;     ld a,$80 / ld [hl],a          ; bank register := $80
;     ld a,c   / inc de / inc de / ld [de],a    ; $4002 <- operand Y
;     dec de / dec de / ld a,[de]   ; read $4000 -> answer
;     ld b,a / xor a / ld [de],a    ; clear
;     pop hl,de,af / ld [$2000],a   ; restore the caller's bank
; The cart is 1 MiB (64 banks), so $C0 and $80 address nothing: parking the
; MBC5 ROM-bank register OUT OF THE CART'S RANGE is the protection enable, the
; same convention the "New GB Color" HK PCB uses. While it is engaged a write
; to $4000-$5FFF latches an operand instead of the RAM bank (A1 picks which),
; and a read of that window returns
;       (X >> 1) + Y        (mod 256)
; i.e. the board sums X with Y shifted up one place and presents bits 8..1.
;
; rustyboi content-detects the board from the CRC32 of those 40 thunk bytes
; (0x02A3_6288), gated on an MBC5-family header type.
;
; This ROM asserts, with DATA accesses only:
;   1. with the bank register inside the cart's range the window is ordinary
;      ROM and $4000-$5FFF writes are ordinary RAM-bank selects;
;   2. with it parked out of range, $4000 latches X, $4002 latches Y and a read
;      of $4000 returns (X >> 1) + Y -- checked on the four operand pairs the
;      cart itself issues plus the two 8-bit edge cases;
;   3. the odd bit of X is dropped and the sum wraps mod 256;
;   4. bank 0 ($0000-$3FFF) is never affected;
;   5. bringing the bank register back into range restores ordinary ROM reads.
; Run as a plain MBC5 (detection disabled) the ROM FAILS at step 2's first
; assertion: an out-of-range bank folds back into the image and the read
; returns a ROM byte instead of the sum.
;
; PROVENANCE: cart-derived, and cross-checked against the cart's own anti-tamper
; arithmetic. Neither hhugboy nor mGBA implements this board, so there is no
; runnable oracle; the transfer function was recovered from the ROM instead:
;   * the boot at $0200 queries (X=$00,Y=$02) and (X=$2A,Y=$08), builds `hl`
;     from the two answers and does `jp hl`. $021D -- (0>>1)+2 = $02 and
;     ($2A>>1)+8 = $1D -- is the only instruction boundary in that page and
;     continues `di / call $1628 / ld hl,$0DEC / ...`;
;   * INDEPENDENTLY, the routine at $3F30 builds a pointer from the answers to
;     (X=$00,Y=$02) and (X=$00,Y=$00), folds the 29 bytes at it into a
;     carry-folded checksum and compares that with the answer to (X=$08,Y=$82).
;     The formula puts the pointer at $0200; the real ROM bytes at $0200-$021C
;     fold to $86; and ($08>>1)+$82 = $86. A wrong transfer function has a
;     1-in-256 chance of closing that check.
; With it the cart reaches its title screen and in-game fighting; without it it
; hangs in WRAM. Only the $4000 readback and the $4000/$4002 operand writes are
; cart-observed, so this ROM asserts nothing about $6000-$7FFF.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Truthful MBC5+RAM+BATTERY header ($1B), as the real cart carries: detection is
; by the $3F50 thunk CRC32 and the type decode falls through to this header.
SECTION "header", ROM0[$147]
    db $1B    ; MBC5+RAM+BATTERY
    db $01    ; 64KB / 4 banks
    db $02    ; 8KB RAM

; Protection-thunk signature at $3F50: detection keys ONLY on the CRC32 of these
; 40 bytes, never on their meaning. This is a CLEAN-ROOM stand-in -- a plain
; ASCII banner (36 bytes) plus a 4-byte suffix computed so the block's CRC32
; equals the detection constant 0x02A3_6288 -- so the ROM carries the signature
; without embedding any of the cartridge's own code.
SECTION "vfaddersig", ROM0[$3F50]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $56, $46, $41, $44, $44, $45, $52
    db $20, $50, $52, $4F, $54, $20, $43, $4C, $45, $41, $4E, $52, $4F, $4F, $4D, $20
    db $53, $49, $47, $21, $D7, $3F, $E7, $D1

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. bank register in range: ordinary MBC5. Bank 3 exists (4 banks), so
    ;     the window is ROM and a $4000 write is a plain RAM-bank select. ---
    ld a, $03
    ld [$2000], a
    ld a, $2A
    ld [$4000], a        ; would be operand X if the protection were engaged
    ld a, [$4000]
    cp $B3               ; bank-3 marker, not a protection sum
    jp nz, .fail
    ld a, [$5000]
    cp $C4
    jp nz, .fail
    xor a
    ld [$4000], a        ; put the RAM bank back

    ; --- 2. park the bank register out of range and run the cart's own four
    ;     operand pairs through the ports exactly as its thunk does. ---
    ; (X=$00, Y=$02) -> $02: the high byte of the boot's `jp hl` target.
    ld a, $C0
    ld [$2300], a
    ld a, $00
    ld [$4000], a
    ld a, $80
    ld [$2300], a
    ld a, $02
    ld [$4002], a
    ld a, [$4000]
    cp $02
    jp nz, .fail

    ; (X=$2A, Y=$08) -> $1D: its low byte. Target $021D.
    ld a, $C0
    ld [$2300], a
    ld a, $2A
    ld [$4000], a
    ld a, $80
    ld [$2300], a
    ld a, $08
    ld [$4002], a
    ld a, [$4000]
    cp $1D
    jp nz, .fail

    ; (X=$00, Y=$00) -> $00: the low byte of the $3F30 checksum pointer.
    ld a, $C0
    ld [$2300], a
    ld a, $00
    ld [$4000], a
    ld a, $80
    ld [$2300], a
    ld a, $00
    ld [$4002], a
    ld a, [$4000]
    cp $00
    jp nz, .fail

    ; (X=$08, Y=$82) -> $86: the checksum the cart compares that pointer's
    ; 29-byte fold against.
    ld a, $C0
    ld [$2300], a
    ld a, $08
    ld [$4000], a
    ld a, $80
    ld [$2300], a
    ld a, $82
    ld [$4002], a
    ld a, [$4000]
    cp $86
    jp nz, .fail

    ; --- 3. the odd bit of X is dropped, and the sum wraps mod 256. ---
    ; (X=$55, Y=$55) -> $2A + $55 = $7F: bit 0 of $55 is discarded.
    ld a, $C0
    ld [$2300], a
    ld a, $55
    ld [$4000], a
    ld a, $80
    ld [$2300], a
    ld a, $55
    ld [$4002], a
    ld a, [$4000]
    cp $7F
    jp nz, .fail

    ; (X=$FF, Y=$FF) -> $7F + $FF = $17E -> $7E.
    ld a, $C0
    ld [$2300], a
    ld a, $FF
    ld [$4000], a
    ld a, $80
    ld [$2300], a
    ld a, $FF
    ld [$4002], a
    ld a, [$4000]
    cp $7E
    jp nz, .fail

    ; Each latch holds independently: rewrite only Y and the sum tracks it.
    ld a, $01
    ld [$4002], a
    ld a, [$4000]
    cp $80               ; ($FF >> 1) + $01
    jp nz, .fail

    ; --- 4. bank 0 is never affected while the protection is engaged. ---
    ld a, [$0A00]
    cp $C3
    jp nz, .fail

    ; --- 5. back in range: the window is ROM again and the latches are inert. ---
    ld a, $03
    ld [$2000], a
    ld a, [$4000]
    cp $B3
    jp nz, .fail
    ld a, [$5000]
    cp $C4
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank-0 marker (read through the $0A00 window; never protected).
SECTION "bank0marker", ROM0[$0A00]
    db $C3

; Bank-3 markers. Both differ from every protection sum asserted above, so a
; plain MBC5 -- which folds the out-of-range bank $80 back into the image and
; serves ROM -- fails step 2 immediately.
SECTION "bank3_0000", ROMX[$4000], BANK[3]
    db $B3
SECTION "bank3_1000", ROMX[$5000], BANK[3]
    db $C4

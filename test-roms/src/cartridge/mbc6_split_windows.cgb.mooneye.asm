; mbc6_split_windows.cgb.mooneye — MBC6 ($20) split ROM/RAM windows + flash ID.
;
; MBC6 is the one-off Nintendo board behind "Net de Get - Minigame @ 100"
; (Konami, CGB-BMVJ-JPN), the Mobile Adapter cart that downloads minigames into
; an on-cart 8 Mbit Macronix MX29F008TC flash chip. It is the only licensed
; mapper whose switchable areas are each SPLIT IN HALF, with an independent bank
; register per half (Pan Docs "MBC6"):
;
;   $4000-$5FFF  ROM/flash bank A, 8 KiB   $A000-$AFFF  SRAM bank A, 4 KiB
;   $6000-$7FFF  ROM/flash bank B, 8 KiB   $B000-$BFFF  SRAM bank B, 4 KiB
;
; and whose register file is decoded at 1 KiB granularity, not the usual
; quarters of $0000-$7FFF:
;
;   $0000-$03FF RAM enable      $0400-$07FF SRAM bank A   $0800-$0BFF SRAM bank B
;   $0C00-$0FFF flash enable    $1000-$13FF flash /WP
;   $2000-$27FF ROM bank A      $2800-$2FFF ROM bank A select ($00 ROM / $08 flash)
;   $3000-$37FF ROM bank B      $3800-$3FFF ROM bank B select
;
; This ROM asserts, in order:
;   1. The two ROM halves are independent 8 KiB pages: it programs a pair that
;      NO 16 KiB bank register can express (A <- page 5, B <- page 2; a 16 KiB
;      bank always yields the consecutive pair 2b, 2b+1), then moves only B and
;      checks that A stayed put.
;   2. The register block really is 1 KiB-decoded: bank A is programmed through
;      $27FF, the last byte of its block, and the select register through $2800,
;      the first byte of the next. An MBC5-shaped decode ($2000-$2FFF = one low
;      bank register) would take the $2800 write as bank $08 and lose page 5.
;      The real cart's boot loop does exactly this ($27FF then $2800).
;   3. The cart-RAM halves are 4 KiB, independently banked: a byte written
;      through $B000 with RAM bank B = 1 must read back through $A000 once RAM
;      bank A is moved to 1. An 8 KiB-banked board maps those two windows to
;      different bytes and fails.
;   4. RAM enable gates the window ($0A on, anything else off).
;   5. The flash chip: with window A switched to flash ($2800 <- $08) and the
;      chip enabled ($0C00 <- $01), the AA/55/$90 command sequence puts it in ID
;      mode, where it reports the JEDEC ID $C2 (Macronix) / $81 (MX29F008TC) at
;      the two lowest addresses of the window; $F0 leaves ID mode and an erased
;      chip reads $FF. The unlock addresses are the chip's own $5555/$2AAA, so
;      they are reached as bank 2 : $5555 and bank 1 : $4AAA.
;
; Every assertion is a DATA read (`ld a,[nn]`), never a jump into a banked
; window, so the verdict never depends on how many cartridge fetches an
; instruction issues.
;
; Without the $20 decode the cart falls through to a bankless board: $4000 then
; reads the ROM image's own $4000 (page 2's marker $11, not page 5's $5A) and
; assertion 1 fails on its first read. I confirmed that by running this ROM
; against the pre-MBC6 decode.
;
; PROVENANCE: Pan-Docs-anchored (the register map, the window geometry and the
; JEDEC ID are all stated there) AND game-boot-anchored (Net de Get boots
; through the Konami / Mobile21 / "Mobile System GB" logos into its title
; animation and first interactive scene iff the mapper reproduces this). The
; flash program/erase state machine is NOT exercised here: no ROM can reach it
; without a Mobile Adapter download session to program, so there is nothing a
; test could assert that is not just the emulator restating itself.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; The MBC6 type byte. rgbfix (-v -p) recomputes the header checksum and writes
; the real ROM-size byte; the RAM size is declared here as $03 (32 KiB), which
; is what the real cart declares -- eight 4 KiB half-banks.
SECTION "header", ROM0[$147]
    db $20    ; MBC6
    db $01    ; ROM size placeholder (rgbfix -p sets the real value)
    db $03    ; RAM size: 32 KiB

SECTION "main", ROM0[$1000]
Start:
    ; --- 1+2. two independent 8 KiB ROM pages, 1 KiB-decoded registers -------
    ld a, $05
    ld [$27FF], a     ; last byte of the bank-A number block
    xor a
    ld [$2800], a     ; first byte of the bank-A SELECT block: $00 = ROM
    ld a, $02
    ld [$3000], a     ; bank B number
    xor a
    ld [$3800], a     ; bank B select: ROM

    ld a, [$4000]
    cp $5A            ; page 5 = bank 2's $6000
    jp nz, FailPath
    ld a, [$6000]
    cp $11            ; page 2 = bank 1's $4000
    jp nz, FailPath

    ; move ONLY the high half; the low half must not follow
    ld a, $03
    ld [$3000], a
    ld a, [$4000]
    cp $5A
    jp nz, FailPath
    ld a, [$6000]
    cp $22            ; page 3 = bank 1's $6000
    jp nz, FailPath

    ; --- 4. the RAM window is gated ------------------------------------------
    xor a
    ld [$0000], a     ; RAM disable
    ld a, [$A000]
    cp $FF
    jp nz, FailPath

    ; --- 3. 4 KiB RAM half-banks, independently selected ---------------------
    ld a, $0A
    ld [$0000], a     ; RAM enable
    xor a
    ld [$0400], a     ; RAM bank A = 0
    ld a, $01
    ld [$0800], a     ; RAM bank B = 1

    ld a, $3C
    ld [$A000], a     ; -> RAM half-bank 0
    ld a, $C5
    ld [$B000], a     ; -> RAM half-bank 1

    ld a, $01
    ld [$0400], a     ; RAM bank A = 1: the window B just wrote
    ld a, [$A000]
    cp $C5
    jp nz, FailPath
    xor a
    ld [$0400], a     ; back to half-bank 0
    ld a, [$A000]
    cp $3C
    jp nz, FailPath

    ; --- 5. the flash chip's JEDEC ID ----------------------------------------
    ld a, $01
    ld [$0C00], a     ; flash enable (/CE)
    ld a, $08
    ld [$2800], a     ; window A shows the flash, not the ROM

    ld a, $02
    ld [$27FF], a     ; bank 2 -> $5555 is reachable at $5555
    ld a, $AA
    ld [$5555], a
    ld a, $01
    ld [$27FF], a     ; bank 1 -> $2AAA is reachable at $4AAA
    ld a, $55
    ld [$4AAA], a
    ld a, $02
    ld [$27FF], a
    ld a, $90
    ld [$5555], a     ; ID mode

    ld a, [$4000]
    cp $C2            ; Macronix
    jp nz, FailPath
    ld a, [$4001]
    cp $81            ; MX29F008TC
    jp nz, FailPath

    ld a, $F0
    ld [$4000], a     ; leave ID mode
    ld a, [$4000]
    cp $FF            ; an erased chip reads $FF
    jp nz, FailPath
    ld a, [$4001]
    cp $FF
    jp nz, FailPath

    ; the mask ROM is still there behind the select bit
    xor a
    ld [$2800], a     ; window A back to ROM
    ld a, $05
    ld [$27FF], a
    ld a, [$4000]
    cp $5A
    jp nz, FailPath

    test_success

SECTION "failsec", ROM0[$1800]
FailPath:
    test_failure

; --- 8 KiB page markers ------------------------------------------------------
; Page p lives at file offset p*$2000, so 16 KiB bank b holds pages 2b (at its
; $4000) and 2b+1 (at its $6000).

; page 2 = bank 1 $4000
SECTION "p2", ROMX[$4000], BANK[1]
    db $11
; page 3 = bank 1 $6000
SECTION "p3", ROMX[$6000], BANK[1]
    db $22
; page 4 = bank 2 $4000
SECTION "p4", ROMX[$4000], BANK[2]
    db $C3
; page 5 = bank 2 $6000
SECTION "p5", ROMX[$6000], BANK[2]
    db $5A

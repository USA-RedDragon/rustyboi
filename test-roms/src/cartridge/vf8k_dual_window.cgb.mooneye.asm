; vf8k_dual_window.cgb.mooneye — Vast Fame 8 KiB dual-window ROM banking.
;
; Jieba Tianwang 4 (Taiwan) ships on a Vast Fame board that is electrically an
; MBC5+RUMBLE except for one thing: the switchable ROM area is not one 16 KiB
; bank but two independent 8 KiB pages, programmed through the same $2000-$3FFF
; register block with A10 choosing which page moves:
;
;   $2000-$3FFF, A10 low  -> 8 KiB page mapped at $4000-$5FFF
;   $2000-$3FFF, A10 high -> 8 KiB page mapped at $6000-$7FFF
;
; $0000-$3FFF stays on bank 0 and every other register ($0000-$1FFF RAM enable,
; $4000-$5FFF RAM bank / rumble) is plain MBC5. rustyboi maps UnlMapper::Vf8k.
;
; The game's far-call thunk shows the geometry directly — it keeps the 16 KiB
; bank number in a variable, doubles it, and programs the pair:
;     ld a,n / ld [$C242],a / sla a / ld [$2000],a x2 / inc a
;     / ld [$2400],a x2 / call $400c
; so $2000 receives 2n and $2400 receives 2n+1. An emulator that treats both as
; one 16 KiB bank register keeps only the last value (2n+1), which lands on one
; of the ROM's 34 decoy banks — every one of them is filled with `jp $0000`, so
; the cart resets forever and never draws a frame.
;
; This ROM asserts the split with DATA reads (`ld a,[nn]`), never by executing
; banked code, and deliberately programs a page pair that is NOT (2n, 2n+1):
;   * $2000 <- page 5 (the UPPER half of 16 KiB bank 2)
;   * $2400 <- page 6 (the LOWER half of 16 KiB bank 3)
; No single 16 KiB bank register can produce that map, so the assertion cannot
; pass by coincidence. It also checks the power-on map (pages 2/3 = bank 1,
; matching MBC5's power-on bank 1) before touching any register.
;
; On a plain MBC5 both writes hit the one low-bank register, leaving bank
; 6 & 3 = 2 across $4000-$7FFF: $4000 then reads bank 2's own $4000 (pad $FF),
; not the page-5 marker, so the ROM FAILS without the mapper.
;
; PROVENANCE: derived by static disassembly of the cart's own far-call thunk and
; of the mid-routine bank switch at bank 1 $44F8 (`ld a,$0A / ld [$242D],a /
; ld a,$00 / ...`), which runs from the $4000-$5FFF half while reprogramming the
; $6000-$7FFF half — a single 16 KiB register would swap the code out from under
; the program counter. Confirmed end-to-end: with the split the cart boots to
; real gameplay (character-select and fight screens); without it, it resets in a
; loop. Not captured on a logic-analyser bench — treat as a game-boot-anchored
; regression pin for the banking geometry.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

; The board's power-on handshake is executed straight out of the header: the
; entry point jumps INTO the title field, which holds `push af / ld a,$AA /
; ld [$7000],a`. rustyboi detects the board from exactly that shape (a licensed
; cart's $0134-$0143 is its ASCII title, and no licensed entry point jumps
; there), so the fixture must reproduce it byte for byte.
SECTION "entry", ROM0[$100]
    nop
    jp HeaderStub

SECTION "hdrstub", ROM0[$134]
HeaderStub:
    push af
    ld a, $AA
    ld [$7000], a     ; board unlock write; inert on a plain MBC5
    jp Start

; Header: MBC5+RUMBLE ($1C), as the real cart declares. The Vf8k detection gates
; on the MBC5 family ($19-$1E). rgbfix -v recomputes the checksums.
SECTION "header", ROM0[$147]
    db $1C    ; MBC5+RUMBLE
    db $01    ; ROM size: 64KB / 4 banks = 8 pages of 8 KiB
    db $00    ; RAM size: none

SECTION "main", ROM0[$1000]
Start:
    pop af

    ; --- power-on map: pages 2 and 3, i.e. 16 KiB bank 1 across $4000-$7FFF ---
    ld a, [$4000]
    cp $11
    jp nz, FailPath
    ld a, [$6000]
    cp $22
    jp nz, FailPath

    ; --- program a page pair no 16 KiB register can express ---
    ld a, $05
    ld [$2000], a     ; A10 low  -> $4000-$5FFF window <- page 5
    ld a, $06
    ld [$2400], a     ; A10 high -> $6000-$7FFF window <- page 6

    ; page 5 is the upper half of bank 2, so $4000 must read bank 2's $6000
    ld a, [$4000]
    cp $5A
    jp nz, FailPath
    ; page 6 is the lower half of bank 3, so $6000 must read bank 3's $4000
    ld a, [$6000]
    cp $A5
    jp nz, FailPath

    ; --- the two windows really are independent: move only the high one ---
    ld a, $04
    ld [$2400], a     ; high window <- page 4 (lower half of bank 2)
    ld a, [$4000]
    cp $5A            ; low window must NOT have moved
    jp nz, FailPath
    ld a, [$6000]
    cp $C3
    jp nz, FailPath

    test_success

SECTION "failsec", ROM0[$2100]
FailPath:
    test_failure

; --- page markers ------------------------------------------------------------
; 8 KiB page p lives at file offset p*$2000. Banks are addressed at $4000-$7FFF,
; so bank b's $4000 is page 2b and its $6000 is page 2b+1.

; page 2 = bank 1 $4000 (power-on low window)
SECTION "p2", ROMX[$4000], BANK[1]
    db $11
; page 3 = bank 1 $6000 (power-on high window)
SECTION "p3", ROMX[$6000], BANK[1]
    db $22
; page 4 = bank 2 $4000
SECTION "p4", ROMX[$4000], BANK[2]
    db $C3
; page 5 = bank 2 $6000
SECTION "p5", ROMX[$6000], BANK[2]
    db $5A
; page 6 = bank 3 $4000
SECTION "p6", ROMX[$4000], BANK[3]
    db $A5

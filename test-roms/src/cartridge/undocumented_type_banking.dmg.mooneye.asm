; undocumented_type_banking.dmg.mooneye — a >32KB cart whose $0147 names no
; board at all must still be banked, not decoded as a bankless "No MBC" cart.
;
; $0147 holds a cartridge TYPE, and the Pan Docs table defines every value that
; is one. A byte outside that table is therefore not a claim of "bankless" — it
; is a header that was never finalized, or a field that game data / title text
; overran. Both happen: "Mofa Qiu - Magic Ball (Taiwan) (Unl)" is 64KB with
; $0147 = $30, because its title string "GOWIN MAGIC BALL 930920" is 23
; characters and runs straight through the 16-byte title field into $0144-$014A
; ($30 is the ASCII '0' of "930920"). "Yelu Wangzi (Prince Yeh Rude)" (64KB) and
; "Binary Monsters II" (128KB, $0147 = $B0) are the same overrun.
;
; The physical argument is what settles it, and it is the same one rustyboi
; already applies to a bankless header ($00/$08/$09) on a >32KB ROM: a Game Boy
; cart edge exposes only A0-A14, so without a mapper chip the CPU can reach
; exactly 32KB. A 64KB+ ROM that is genuinely bankless cannot exist — three
; quarters of the chip would be unbonded. So the cart HAS a mapper; only the
; header failed to say which. rustyboi infers the DMG-era standard MBC1, which
; is what all three carts above are, and what boots them.
;
; This ROM is a 64KB (4-bank) cart carrying $0147 = $30 — the real Mofa Qiu
; type byte — and asserts the banking through DATA reads only:
;   1. writing bank 2 to $2000 makes $4000 read bank 2's marker;
;   2. writing bank 3 reaches bank 3 (so it is real banking, not a fixed
;      alternate window);
;   3. writing bank 1 latches back (so banking was not simply left wide open);
;   4. $0000-$3FFF stays bank 0 throughout, as MBC1 mode 0 requires.
; Every check is `ld a,[nn]` against a marker byte, never a jump or call into a
; banked region: which bank a FETCH sees depends on how many cartridge reads the
; CPU issues per instruction, an emulator-internal detail this ROM must not pin.
;
; Decoded as a bankless board the upper window is nailed to bank 1, so step 1
; reads $B1 instead of $B2 and the ROM FAILS. It passes only when the
; undocumented type byte is inferred to a live banked board.
;
; PROVENANCE: first-principles + real-ROM-anchored. The 32KB reach of an
; unmapped cart edge is the documented DMG bus (Pan Docs "Memory Map" /
; "Cartridge Header"), and the header-overrun cause is visible in the three
; dumps named above, all of which boot into gameplay under this decode and show
; a blank screen without it. Clean-room: synthetic markers only.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; $0147 = $30: not a Pan Docs cartridge type. This is the byte the real Mofa
; Qiu cart carries, for the reason described above. The ROM-size byte is left
; VALID ($01 = 64KB / 4 banks) so the undocumented type byte is the only
; independent variable — the geometry is not in question, only the board.
; rgbfix (-v -p 0xFF) recomputes the header checksum and the logo but does not
; touch these three bytes.
SECTION "header", ROM0[$147]
    db $30    ; cartridge type: undocumented (Mofa Qiu's title-overrun byte)
    db $01    ; ROM size: 64KB / 4 banks
    db $00    ; RAM size: none

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. bank 2 is reachable at all. A bankless decode pins the upper
    ;     window to bank 1 and reads $B1 here. ---
    ld a, $02
    ld [$2000], a
    ld a, [$4000]
    cp $B2
    jp nz, .fail

    ; --- 2. bank 3 too, so the window really follows the register rather than
    ;     sitting on one fixed alternate bank. ---
    ld a, $03
    ld [$2000], a
    ld a, [$4000]
    cp $B3
    jp nz, .fail

    ; --- 3. and it latches back to bank 1. ---
    ld a, $01
    ld [$2000], a
    ld a, [$4000]
    cp $B1
    jp nz, .fail

    ; --- 4. $0000-$3FFF stayed bank 0 the whole time (MBC1 mode 0). ---
    ld a, [Bank0Marker]
    cp $B0
    jp nz, .fail

    test_success
.fail:
    test_failure

SECTION "bank0marker", ROM0[$3FFF]
Bank0Marker:
    db $B0

; One marker per bank at the same offset, read through the $4000 window.
SECTION "bank1marker", ROMX[$4000], BANK[$01]
    db $B1
SECTION "bank2marker", ROMX[$4000], BANK[$02]
    db $B2
SECTION "bank3marker", ROMX[$4000], BANK[$03]
    db $B3

; ntnew_split_window.cgb.mooneye — NT "new" split-window ROM banking.
;
; Capcom vs SNK - Millennium Fight 2001 (Taiwan) ships on the NT "new" board
; (taizou's hhugboy `MbcUnlNtNew`, CC0), the split-window successor to the NT
; "old" boards. It is electrically MBC5 until the cart ARMS it by writing $55 to
; $1400 — an address inside MBC5's RAM-enable block, where $55 is not the $0A
; magic and so is a no-op for a real MBC5. From that write on, the switchable
; ROM area stops being one 16 KiB bank and becomes two INDEPENDENT 8 KiB pages:
;
;   $1400 <- $55        arm the split window (port decoded `addr & $FF00`)
;   $2000 <- page       8 KiB page mapped at $4000-$5FFF   (`addr & $FF00`)
;   $2400 <- page       8 KiB page mapped at $6000-$7FFF   (`addr & $FF00`)
;
; Each page number is taken at 8 KiB granularity (`page << 13`), wrapped to the
; ROM, and — mirroring MBC5's "bank 0 reads as bank 1" — a result that lands
; inside the first 16 KiB is pushed up by 16 KiB, so pages 0 and 1 present pages
; 2 and 3. $0000-$3FFF stays on bank 0 throughout, and every register the board
; does not claim ($0000-$1FFF RAM enable, $4000-$5FFF RAM bank) is plain MBC5.
; rustyboi maps UnlMapper::NtNew.
;
; The game's boot shows the geometry directly: it arms the board at $00B9, then
; runs `$2000 <- $06` / `$2400 <- $27` and immediately `call`s into $6000-$7FFF.
; An emulator that treats both writes as one 16 KiB bank register keeps only
; $27, so $6000-$7FFF presents bank $27's SECOND half instead of page $27, the
; call lands in compressed graphics data, and the cart white-screens before its
; title.
;
; This ROM asserts the split with DATA reads (`ld a,[nn]`), never by executing
; banked code, so the verdict cannot depend on how many cartridge reads the CPU
; issues per fetch. It checks, in order:
;   1. un-armed the board is a plain MBC5 — a $2000 bank write moves the WHOLE
;      16 KiB window, and a $2400 write is just another write to that same
;      register (both halves move together);
;   2. $1400 <- $54 does NOT arm it (only the exact $55 magic does);
;   3. armed, $2000 and $2400 program a page pair that is NOT (2n, 2n+1) —
;      page 5 (the UPPER half of 16 KiB bank 2) at $4000 and page 6 (the LOWER
;      half of bank 3) at $6000. No single 16 KiB bank register can produce that
;      map, so the assertion cannot pass by coincidence;
;   4. the two windows are independent — moving only the high one leaves the low
;      one where it was;
;   5. the page-0/1 fold: pages 0 and 1 present pages 2 and 3, not bank 0.
; Run as a plain MBC5 (detection disabled) the ROM FAILS at step 3 — verified by
; commenting out the detection rule.
;
; PROVENANCE: emulator-algorithm-anchored + game-boot-anchored. The register map
; and the page arithmetic are taizou's hhugboy `MbcUnlNtNew` (CC0), which names
; this PCB family; hhugboy ships the board but only as a MANUAL menu pick, and
; the local mGBA does not implement it at all, so neither binary is a runnable
; auto-detect oracle. Corroborated from the cart itself: the byte the boot's
; `call` targets is a real instruction boundary only under the split map (page
; $27 offset $010B continues `bit 5,a / ld a,[$CB21] / bit 4,a / ... /
; call $6392`, a further call INTO the high window), and with the split the cart
; boots through its Capcom-vs-SNK title to the main menu, game select and player
; select; without it the screen stays white. Not captured on a logic-analyser
; bench — treat as a regression pin for the banking geometry, not silicon truth.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; Truthful MBC5 header ($19), as the real cart declares. Detection gates on the
; MBC5 family ($19-$1E) and on the image being self-consistent (file size ==
; the size the header declares), which rgbfix -p guarantees.
SECTION "header", ROM0[$147]
    db $19    ; MBC5
    db $00    ; ROM size placeholder (rgbfix -p sets the real value)
    db $00    ; RAM size: none

; Board-driver signature at $00B9, in the dead space between the interrupt
; vectors and the header. Detection keys ONLY on the CRC32 of these 37 bytes,
; never their meaning. This is a CLEAN-ROOM stand-in — a plain ASCII banner
; (33 bytes, "RUSTYBOI NTNEW SPLIT CLEANROOM!!!") plus a 4-byte suffix computed
; so the block's CRC32 equals the detection constant 0x24FD_EE7B — so it carries
; the signature without embedding any of the cartridge's own code.
SECTION "ntnewsig", ROM0[$B9]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $4E, $54, $4E, $45, $57, $20, $53
    db $50, $4C, $49, $54, $20, $43, $4C, $45, $41, $4E, $52, $4F, $4F, $4D, $21, $21
    db $21, $A6, $85, $5F, $CF

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. un-armed: a plain MBC5 16 KiB bank register ----------------------
    ld a, $02
    ld [$2000], a     ; MBC5 bank 2 across the whole window
    ld a, [$4000]
    cp $C3            ; bank 2 $4000 = page 4 marker
    jp nz, FailPath
    ld a, [$6000]
    cp $5A            ; bank 2 $6000 = page 5 marker: BOTH halves moved
    jp nz, FailPath

    ; A $2400 write un-armed is just the same bank register again.
    ld a, $03
    ld [$2400], a
    ld a, [$4000]
    cp $A5            ; bank 3 $4000 = page 6 marker
    jp nz, FailPath

    ; --- 2. only the exact $55 magic arms the board --------------------------
    ld a, $54
    ld [$1400], a     ; wrong magic: still a plain MBC5
    ld a, $02
    ld [$2000], a
    ld a, $03
    ld [$2400], a     ; would be the HIGH page if this had armed the board
    ld a, [$4000]
    cp $A5            ; still one register: bank 3 won, so $4000 is bank 3's
    jp nz, FailPath

    ; --- 3. arm, then program a pair no 16 KiB register can express ----------
    ld a, $55
    ld [$1400], a     ; arm the split window
    ld a, $05
    ld [$2000], a     ; low window  <- page 5 (UPPER half of bank 2)
    ld a, $06
    ld [$2400], a     ; high window <- page 6 (LOWER half of bank 3)

    ld a, [$4000]
    cp $5A
    jp nz, FailPath
    ld a, [$6000]
    cp $A5
    jp nz, FailPath

    ; --- 4. the two windows are independent ----------------------------------
    ld a, $04
    ld [$2400], a     ; move only the high window, to page 4
    ld a, [$4000]
    cp $5A            ; low window must NOT have moved
    jp nz, FailPath
    ld a, [$6000]
    cp $C3
    jp nz, FailPath

    ; --- 5. the page-0/1 fold: 0 and 1 present pages 2 and 3 -----------------
    xor a
    ld [$2000], a     ; low window  <- page 0 -> folds to page 2
    ld a, $01
    ld [$2400], a     ; high window <- page 1 -> folds to page 3
    ld a, [$4000]
    cp $11            ; page 2 marker (bank 1 $4000), NOT bank 0's $0000
    jp nz, FailPath
    ld a, [$6000]
    cp $22            ; page 3 marker (bank 1 $6000)
    jp nz, FailPath

    test_success

SECTION "failsec", ROM0[$2100]
FailPath:
    test_failure

; --- page markers ------------------------------------------------------------
; 8 KiB page p lives at file offset p*$2000. Banks are addressed at $4000-$7FFF,
; so bank b's $4000 is page 2b and its $6000 is page 2b+1.

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
; page 6 = bank 3 $4000
SECTION "p6", ROMX[$4000], BANK[3]
    db $A5

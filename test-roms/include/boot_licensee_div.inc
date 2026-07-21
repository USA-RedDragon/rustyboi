; boot_licensee_div.inc — the CGB boot ROM's licensee branch, read back as DIV.
;
; WHAT THE BOOT ROM DOES
;
; A DMG cartridge ($0143 != $80/$C0) booted on CGB-family silicon takes the
; boot ROM's DMG-compatibility path, which picks a colourisation palette for the
; monochrome game. Pan Docs, "CGB Registers" / "Compatibility Palettes": the CGB
; boot ROM applies a built-in palette to a subset of black-and-white games,
; selected by hashing the cartridge title, and it only does so for
; Nintendo-published carts.
;
; That gate is a three-way branch on the header's licensee bytes:
;
;   $014B == $33  -> "see the new licensee code"; read $0144-$0145. Nintendo
;                    iff those two ASCII bytes are "01".
;   $014B == $01  -> Nintendo (old-style licensee code).
;   anything else -> not Nintendo; skip the lookup entirely.
;
; All four shipped CGB-family boot ROM images (CGB0, CGB-A..E, CGB-D/E, AGB)
; contain the same opcode sequence at the head of that block:
;
;   21 4B 01   ld hl, $014B
;   7E         ld a, [hl]
;   FE 33      cp $33
;   20 0B      jr nz, +$0B
;
; at image offset 0x0579 (0x0573 on CGB0, whose image is shifted but carries the
; identical sequence).
;
; WHY THIS IS OBSERVABLE AT ALL
;
; The branch changes how much WORK the boot ROM does, therefore how LONG it
; takes, therefore the value of the divider the game inherits. DIV is not reset
; at hand-off: it is a free-running counter that has been ticking since power-on,
; so post-boot DIV is a direct read-out of boot duration. That is the same
; observable mooneye's `boot_div-*` family uses, and it is fully
; hardware-runnable: nothing here is an emulator construct.
;
; A Nintendo cart's extra work is the title hash plus a linear search of the
; boot ROM's checksum table, and it is LARGE compared to a DIV tick (256 T
; cycles): between +808 and +5476 T cycles depending on the title, i.e. 3 to 21
; whole DIV increments. Every ROM in this family therefore pins DIV to a value
; that differs by 0x11 between the Nintendo and non-Nintendo arms.
;
; WHY THE TITLE IS PART OF THE FIXTURE
;
; The lookup cost is TITLE-DEPENDENT, not constant — the boot ROM scans its
; checksum table linearly, so an early hit costs far less than a late one, and a
; MISS costs the most because it walks the whole table. Measured on cgb_boot.bin
; (deltas identical on all four images):
;
;   YOSHI    +808    DR.MARIO +1112   TETRIS   +1608   POKEMON RED  +1992
;   ZELDA   +2768    SUPER MARIOLAND +3264   POKEMON BLUE +4304
;   miss    +4360    METROID2 +5476  (ambiguous checksum: also walks the
;                                     4th-letter disambiguation rows)
;
; So these ROMs cannot carry a title-agnostic expected value. Every variant is
; fixed with the SAME title, `RBLICENSEE`, which is a table MISS — the most
; stable choice, since a miss costs the full table walk regardless of what is in
; the table. The title lives in the Makefile (`rgbfix -t`), not here, so it is
; identical across the family by construction.
;
; WHY THE HEADER IS THE INDEPENDENT VARIABLE
;
; The four variants of each revision are assembled from the SAME source and
; differ ONLY in the bytes `rgbfix` writes:
;
;   _l00     $014B = $00                       not Nintendo
;   _l33     $014B = $33, $0144-$0145 = $00    new-licensee route, not "01"
;   _l01     $014B = $01                       Nintendo, old-style
;   _l33n01  $014B = $33, $0144-$0145 = "01"   Nintendo, new-style
;
; The boot ROM hashes $0134-$0143 only, so $0144-$0145 and $014B do not perturb
; the hash: the two Nintendo variants and the two non-Nintendo variants each pay
; the identical lookup cost, and the ONLY thing that moves DIV is which arm was
; taken. That is the whole experimental design.
;
; WHAT IS AND IS NOT GRADED
;
; _l00 and _l33 carry the SAME expected DIV, and that is deliberate. Those two
; arms really do differ — a cart with $014B != $33 hands off exactly 4 T cycles
; (one M-cycle) later, because the `jr nz` above is taken (12 T) rather than
; fallen through (8 T) and the two arms reconverge 4 T apart. But 4 T is 1/64 of
; a DIV increment, so no DIV-graded ROM can resolve it, and a phase-resolving
; ROM would be measuring one M-cycle through an oracle whose own hand-off
; reference point is ambiguous at that scale. It is left ungraded rather than
; faked.
;
; What _l00 vs _l33 DOES assert is still load-bearing: they must not differ by a
; DIV tick. An emulator that sent a $33 cart with a non-"01" new licensee into
; the title-hash path — a plausible way to get the three-way branch wrong —
; fails _l33 while passing _l00. Likewise every single-byte neighbour pair in
; the family has a DIFFERENT expectation: _l00 vs _l01 differ only in $014B, and
; _l33 vs _l33n01 differ only in $0144-$0145.
;
; SELF-CHECKING FIXTURE
;
; The header bytes are the experiment, so the ROM verifies its OWN header before
; grading DIV. If a build ever fixes the wrong licensee in, the ROM reports a
; header failure (sentinels $42) instead of silently grading the wrong cell.
;
; FAILURE DIAGNOSTICS
;   B=C=D=E=H=L=$42 : header mismatch — the cart is not the variant it claims.
;   B=C=D=H=L=$AA   : DIV mismatch; E carries the DIV byte actually observed.

IF !DEF(BOOT_LICENSEE_DIV_INC)
DEF BOOT_LICENSEE_DIV_INC EQU 1

; \1 expected DIV byte, \2 expected $014B, \3 expected $0144, \4 expected $0145.
;
; TIMING. The sample point is fixed by construction and must stay so: from
; PC=$0100 the ROM executes `di` (4 T) and `jp` (16 T), then this `ldh a,[rDIV]`
; is the FIRST instruction of the body. Every expected value below is quoted at
; that point. Do not add work before the read.
MACRO licensee_boot_div_test
    ldh a, [rDIV]
    ld e, a             ; stash the observation before anything can clobber it

    ; The fixture must be the variant this source was written for.
    ld a, [$014B]
    cp \2
    jr nz, .badheader\@
    ld a, [$0144]
    cp \3
    jr nz, .badheader\@
    ld a, [$0145]
    cp \4
    jr nz, .badheader\@

    ld a, e
    cp \1
    jr nz, .baddiv\@
    test_success

.badheader\@:
    test_failure

    ; Like test_failure, but leaves E holding the observed DIV so the verdict
    ; reports WHICH boot duration the cart actually saw.
.baddiv\@:
    ld b, $AA
    ld c, $AA
    ld d, $AA
    ld h, $AA
    ld l, $AA
    ld b, b
.spin\@:
    jr .spin\@
ENDM

ENDC

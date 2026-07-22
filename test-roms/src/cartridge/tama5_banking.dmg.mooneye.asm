; tama5_banking.dmg.mooneye — Bandai TAMA5 ($FD) register-file protocol.
;
; TAMA5 is the licensed three-chip Tamagotchi board (TAMA5 gate array + TAMA6
; MCU/RTC + TAMA7 mask ROM), used by the three "Game de Hakken!! Tamagotchi"
; carts. It is the only licensed mapper whose register file is reached through
; the cart-RAM window $A000-$BFFF instead of $0000-$7FFF, and every transfer is
; a NIBBLE: an ODD address latches the register index, an EVEN address carries a
; 4-bit payload. A byte of save RAM is therefore assembled from two nibble
; registers on the way in and read back as two nibble halves.
;
; Registers: $0 BANK_LO, $1 BANK_HI, $4 WRITE_LO, $5 WRITE_HI, $6 ADDR_HI,
; $7 ADDR_LO, $A ACTIVE, $C READ_LO, $D READ_HI. Indices $8 and up are read
; ports: an even-address write with one of them selected latches nothing.
; A write to ADDR_LO executes the command in `ADDR_HI >> 1` (0 = RAM write,
; 1 = arm a RAM read, 2 = TAMA6 command); the accessed address is
; `((ADDR_HI << 4) & $10) | ADDR_LO`, i.e. 5 bits = the board's 32 save bytes.
;
; This ROM asserts, in order:
;   1. ACTIVE ($A) reads $F1 — the readiness flag every real transfer spins on.
;   2. An ODD-address read is $FF (only the even half of the window is a port).
;   3. 8-bit ROM banking through the two nibble registers: BANK_LO=$4/BANK_HI=$0
;      maps bank 4 at $4000, then BANK_LO=$2/BANK_HI=$1 maps bank $12 — proving
;      BANK_HI really is the high nibble and not an ignored write.
;   4. A save-RAM byte round-trips: write $3C to address $05 (ADDR_HI=$0) and
;      $A9 to address $15 (ADDR_HI=$1), then read both back as READ_LO/READ_HI
;      nibble halves with the upper nibble reading high. Two addresses that
;      differ only in bit 4 must hold different bytes, which pins ADDR_HI bit 0
;      to address bit 4.
;   5. The command decode: a transfer completed with ADDR_HI=$2 (>>1 = 1, "arm a
;      RAM read") must NOT write, so address $05 still reads $3C afterwards.
;
; Every assertion is a DATA read (`ld a,[nn]`), never a jump into banked code:
; the verdict must not depend on how many cartridge fetches the CPU issues for
; an instruction.
;
; Without the $FD decode the cart falls through to a bankless board with no RAM
; array, so $A000 reads $FF and assertion 1 fails immediately. I confirmed this
; by temporarily removing the TAMA5 arm from the header-type decode.
;
; PROVENANCE: mGBA-oracle-anchored (mGBA's `src/gb/mbc/tama5.c` implements
; exactly this register file) AND game-boot-anchored (Game de Hakken!!
; Tamagotchi - Osutchi to Mesutchi boots into gameplay iff the mapper
; reproduces it). This is a regression pin against those two references, not a
; silicon-bench oracle: the TAMA6 command space ($2) is not exercised here
; because rustyboi only stubs it.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

; Latch a register index (odd address).
MACRO t5_sel
    ld a, \1
    ld [$A001], a
ENDM

; Latch a register index, then push its 4-bit payload (even address).
MACRO t5_put
    t5_sel \1
    ld a, \2
    ld [$A000], a
ENDM

; Complete a transfer: stage the byte in WRITE_LO/WRITE_HI, then the address in
; ADDR_HI (whose bits 3-1 are the command and bit 0 is address bit 4) and
; ADDR_LO, whose write executes the command.
MACRO t5_ram_write ; \1 = ADDR_HI nibble, \2 = ADDR_LO nibble, \3 = byte
    t5_put $04, (\3) & $0F
    t5_put $05, ((\3) >> 4) & $0F
    t5_put $06, \1
    t5_put $07, \2
ENDM

SECTION "entry", ROM0[$100]
    di
    jp Start

; The TAMA5 type byte. rgbfix (-v -p) recomputes the header checksum and sets
; the real ROM-size byte, so these are safe to write here. RAM size stays $00
; exactly as on the real carts: the 32 save bytes are the board's, not an
; external RAM chip's, so they are allocated from the type byte.
SECTION "header", ROM0[$147]
    db $FD    ; BANDAI TAMA5
    db $05    ; ROM size placeholder (rgbfix -p sets the real value)
    db $00    ; RAM size: none declared

SECTION "main", ROM0[$1000]
Start:
    ; --- 1. ACTIVE reads $F1 -------------------------------------------------
    t5_sel $0A
    ld a, [$A000]
    cp $F1
    jp nz, .fail

    ; --- 2. an odd-address read is open bus ----------------------------------
    ld a, [$A001]
    cp $FF
    jp nz, .fail

    ; --- 3. 8-bit ROM banking from the two nibble registers ------------------
    t5_put $00, $04     ; BANK_LO = 4
    t5_put $01, $00     ; BANK_HI = 0  -> bank $04
    ld a, [$4000]
    cp $A5
    jp nz, .fail

    t5_put $00, $02     ; BANK_LO = 2
    t5_put $01, $01     ; BANK_HI = 1  -> bank $12
    ld a, [$4000]
    cp $5A
    jp nz, .fail

    ; --- 4. save-RAM round trip, both halves of the 5-bit address ------------
    t5_ram_write $0, $5, $3C    ; address $05 = $3C
    t5_ram_write $1, $5, $A9    ; address $15 = $A9

    ; Arm a read of address $05 (ADDR_HI = $2: command 1, address bit 4 clear).
    t5_put $06, $02
    t5_put $07, $05
    t5_sel $0C
    ld a, [$A000]
    cp $FC              ; low nibble of $3C, upper nibble driven high
    jp nz, .fail
    t5_sel $0D
    ld a, [$A000]
    cp $F3              ; high nibble of $3C
    jp nz, .fail

    ; Same for address $15 (ADDR_HI = $3: command 1, address bit 4 set).
    t5_put $06, $03
    t5_put $07, $05
    t5_sel $0C
    ld a, [$A000]
    cp $F9              ; low nibble of $A9
    jp nz, .fail
    t5_sel $0D
    ld a, [$A000]
    cp $FA              ; high nibble of $A9
    jp nz, .fail

    ; --- 5. the "arm a read" command must not write --------------------------
    t5_ram_write $2, $5, $00    ; command 1 at address $05: a no-op write
    t5_put $06, $02
    t5_put $07, $05
    t5_sel $0C
    ld a, [$A000]
    cp $FC              ; still $3C
    jp nz, .fail
    t5_sel $0D
    ld a, [$A000]
    cp $F3
    jp nz, .fail

    test_success
.fail:
    test_failure

; Bank markers at offset 0 of each bank, read through the $4000 window. Bank $12
; is only reachable once BANK_HI is honored as the high nibble.
SECTION "bank4marker", ROMX[$4000], BANK[$04]
    db $A5
SECTION "bank18marker", ROMX[$4000], BANK[$12]
    db $5A

; gowin_banking.dmg.mooneye — Gowin "Story of Lasama" (GS-04) outer-bank board.
;
; The raw 128KB Story of Lasama dump (published by Gowin, developed by people
; connected to Vast Fame) is electrically a plain MBC1, with one addition: the
; MBC1 banking-mode port ($6000-$7FFF) is repurposed as a two-write OUTER-BANK
; handshake. The first write latches a parameter; the second (a commit strobe)
; sets an outer ROM base of `parameter << 1` 16KB banks, added to BOTH the fixed
; $0000-$3FFF window and the switchable $4000-$7FFF window. Power-on base 0 runs
; the decoy bank 0 (which carries the real Nintendo logo and a stub that writes
; $6000<-$02 / $6000<-$BE, then restarts at $0100 in the real game half). A plain
; MBC1 leaves the low bank fixed at the decoy and spins at the logo forever.
;
; rustyboi content-detects the board from the CRC32 of the 48-byte signature at
; $0184 (0xDD1165F1) AND the exact 30-byte boot stub at $02D7, then maps
; UnlMapper::Gowin over a plain-MBC1 board.
;
; This ROM asserts the outer-bank handshake as OBSERVABLE DATA reads:
;   1. Baseline (base 0): the switchable window ($4000, inner bank 1) reads
;      bank 1's marker, and the fixed window ($0000) reads bank 0's marker.
;   2. After the handshake ($6000<-$02 then $6000<-$BE => base = 2<<1 = 4): the
;      switchable window reads bank 5 (base 4 + inner 1) and the fixed window
;      reads bank 4 (base 4 + 0) -- proving the base is applied to BOTH windows.
;   3. A second handshake with parameter 0 restores base 0, so the two windows
;      read bank 1 and bank 0 again.
; Because the base remaps the very bank the CPU executes from, the handshake +
; captures run from a 30-byte driver copied to HRAM (mirroring the real cart's
; boot stub), then the verdict compares the captured marker bytes. Run as a
; plain MBC1 (detection disabled) this FAILS at step 2 -- I confirmed it by
; temporarily removing the Gowin detection rule (the $6000 writes then only set
; the MBC1 mode bit, so both windows stay on banks 0/1).
;
; PROVENANCE: reverse-engineered from the one known cart (the boot stub at $02D7
; drives exactly this $6000<-$02/$BE handshake, and the game's 64KB de-protected
; sibling is byte-for-byte the raw dump's upper 64KB half -- physical banks 4-7,
; i.e. base 4). A regression pin against that RE, not a silicon-bench oracle.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

; MBC1 header ($01). The real Lasama header type is title-overrun garbage ($38);
; detection is by the $0184 CRC32 + the $02D7 stub and overrides the type byte.
; $01 makes a detection MISS decode to plain MBC1, which fails this test --
; proving the Gowin board is load-bearing. rgbfix (-v -p) recomputes the header
; checksum and the ROM-size byte, so these bytes are safe to set here.
SECTION "header", ROM0[$147]
    db $01    ; MBC1
    db $02    ; ROM size placeholder (rgbfix -p sets the real 128KB value)
    db $00    ; no RAM

; Fixed-window marker for bank 0, read at $0000 while base = 0.
SECTION "bank0low", ROM0[$0000]
    db $B0

; The 30-byte Gowin boot-protection stub at $02D7 is half the detection key (the
; other half is the $0184 CRC32). These are plain banking opcodes -- the
; copy-to-HRAM thunk that writes the $6000 outer-bank handshake and restarts at
; $0100 -- not a logo, so they carry no copyrighted bytes. Placed as DATA; this
; test never executes them (it drives the handshake from its own HRAM driver).
SECTION "gowinstub", ROM0[$2D7]
    db $11, $F4, $02, $0E, $20, $21, $FF, $DF, $1A, $32, $1D, $0D, $20, $FA, $C3, $F3
    db $DF, $3E, $02, $EA, $00, $60, $3E, $BE, $EA, $00, $60, $C3, $00, $01

; Secondary "logo" at $0184: detection keys ONLY on the CRC32 of these 48 bytes,
; never their meaning. A CLEAN-ROOM ASCII banner (44 bytes) plus a 4-byte suffix
; computed so the block's CRC32 equals the Gowin detection constant 0xDD1165F1,
; carrying the signature without embedding any copyrighted logo bytes.
SECTION "gowinlogo", ROM0[$184]
    db $52, $55, $53, $54, $59, $42, $4F, $49, $20, $47, $4F, $57, $49, $4E, $20, $4C
    db $41, $53, $41, $4D, $41, $20, $47, $53, $30, $34, $20, $43, $4C, $45, $41, $4E
    db $52, $4F, $4F, $4D, $20, $53, $49, $47, $21, $21, $21, $21
    db $B3, $97, $A2, $17   ; CRC32-forcing suffix -> 0xDD1165F1

SECTION "main", ROM0[$400]
Start:
    ld sp, $DFFF

    ; Copy the outer-bank driver into HRAM ($FF80). It must run from RAM because
    ; the base remaps the $0000-$3FFF window the CPU otherwise executes from.
    ld hl, Driver
    ld c, LOW($FF80)
    ld b, DriverEnd - Driver
.copy:
    ld a, [hl+]
    ldh [c], a
    inc c
    dec b
    jr nz, .copy

    ; Inner ROM bank = 1 (MBC1 5-bit register). base is still 0.
    ld a, $01
    ld [$2000], a

    ; --- 1. baseline (base 0): switchable = bank 1, fixed = bank 0 ---
    ld a, [$4000]
    cp $D1
    jp nz, .fail
    ld a, [$0000]
    cp $B0
    jp nz, .fail

    ; --- 2. handshake to base 4 (driver captures $4000 and $0000, restores 0) ---
    call $FF80
    ldh a, [$FFA0]      ; captured switchable window: base 4 + inner 1 = bank 5
    cp $D5
    jp nz, .fail
    ldh a, [$FFA1]      ; captured fixed window: base 4 = bank 4
    cp $B4
    jp nz, .fail

    ; --- 3. base restored to 0 by the driver: banks 1 and 0 again ---
    ld a, [$4000]
    cp $D1
    jp nz, .fail
    ld a, [$0000]
    cp $B0
    jp nz, .fail

    test_success
.fail:
    test_failure

; Driver executed from HRAM. Position-independent (absolute addresses only).
;   $6000<-$02, $6000<-$BE  => outer base = 2<<1 = 4
;   capture $4000 (bank 5) and $0000 (bank 4) into $FFA0/$FFA1
;   $6000<-$00, $6000<-$BE  => outer base = 0<<1 = 0 (restore before ret)
SECTION "driver", ROM0[$500]
Driver:
    ld a, $02
    ld [$6000], a
    ld a, $BE
    ld [$6000], a
    ld a, [$4000]
    ldh [$FFA0], a
    ld a, [$0000]
    ldh [$FFA1], a
    xor a
    ld [$6000], a
    ld a, $BE
    ld [$6000], a
    ret
DriverEnd:

; Bank markers, each at offset 0 of its bank (read through the $4000 window, or
; through the fixed $0000 window for the base-remapped banks 4/5).
SECTION "bank1mark", ROMX[$4000], BANK[1]
    db $D1
SECTION "bank4mark", ROMX[$4000], BANK[4]
    db $B4
SECTION "bank5mark", ROMX[$4000], BANK[5]
    db $D5

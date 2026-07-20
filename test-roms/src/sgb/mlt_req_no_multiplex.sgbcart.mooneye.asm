; mlt_req_no_multiplex.sgbcart.mooneye — a plain Game Boy never multiplexes.
;
; Runs the identical MLT_REQ script as mlt_req_player_cycle.sgb, but asserts the
; opposite result: on a DMG there is no ICD2 behind JOYP, so the packets are
; just select-line writes nobody decodes and the low nibble stays $F forever.
;
; The `sgbcart` model matters and is not cosmetic. This build carries the full
; SGB header ($0146=$03, $014B=$33) and runs on DMG hardware, so it is literally
; the cart from mlt_req_player_cycle.sgb moved into a Game Boy. Built unflagged
; instead, the SGB unlock gate would suppress multiplexing on its own and the
; ROM would pass even on an emulator that wired the SGB joypad multiplexer into
; the shared JOYP path with no hardware gate — the exact bug it exists to catch.
; That was confirmed empirically: the unflagged build passed with SGB support
; force-enabled on DMG; the flagged build fails.
;
; Pan Docs "Joypad Input": "If neither buttons nor d-pad is selected ($30 was
; written), then the low nibble reads $F (all buttons released)." Pan Docs
; "Unlocking SGB Functions" makes the contrast explicit — an SGB "will return
; incrementing joypad IDs each time when deselecting keypad lines", whereas "a
; normal Game Boy would typically always return $0F as the ID". That is the
; documented SGB-detection method, so this ROM is really asserting that
; detection still works in the negative direction.
;
; This is the discriminator for the SGB build: an emulator that wired the SGB
; joypad multiplexer into the shared JOYP path without gating it on SGB
; hardware passes mlt_req_player_cycle.sgb and fails here.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "sgb_packet.inc"
INCLUDE "sgb_mlt_req_cycle.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start
    ; Reserve the cartridge header. Without this the linker floats the shared
    ; sgb_packet code into $104-$14F and rgbfix overwrites it with the logo and
    ; checksums.
    ds $150 - @, 0

SECTION "main", ROM0[$1000]
Start:
    ; Every subtest expects the same thing: player 1, always.
    sgb_mlt_req_cycle_test SeqNoMultiplex, SeqNoMultiplex, SeqNoMultiplex, SeqNoMultiplex

SECTION "sequences", ROM0

SeqNoMultiplex:
    db SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1
    db SGB_SEQ_END

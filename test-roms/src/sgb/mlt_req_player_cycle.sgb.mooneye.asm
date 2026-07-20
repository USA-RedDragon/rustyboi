; mlt_req_player_cycle.sgb.mooneye — MLT_REQ joypad multiplexing on an SGB.
;
; Sends real SGB command packets over JOYP and reads back the joypad ID the
; SGB reports while both select lines are deselected. Asserts the documented
; enumeration for one, four and two players, including the wrap back to player
; 1, and that one-player mode never advances at all. See
; include/sgb_mlt_req_cycle.inc for the per-subtest grounding.
;
; Header: this build is fixed with `rgbfix -s -l 0x33`, i.e. SGB flag $0146=$03
; and old licensee $014B=$33 — Pan Docs "Unlocking SGB Functions" requires both
; before the SGB will honour any command packet. The paired
; mlt_req_header_locked ROM is the same hardware with those bytes absent.
;
; The DMG counterpart (mlt_req_no_multiplex.dmg) runs the identical script and
; asserts the ID never moves, which is what makes the multiplexing here a real
; discrimination rather than a self-consistent tautology.

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
    sgb_mlt_req_cycle_test SeqOnePlayer, SeqFourPlayers, SeqOnePlayer, SeqTwoPlayers

SECTION "sequences", ROM0

; One-player mode: the index never advances, so every read is player 1.
SeqOnePlayer:
    db SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1
    db SGB_SEQ_END

; Four players: enumerate 1,2,3,4 then wrap to 1 and continue.
SeqFourPlayers:
    db SGB_ID_P1, SGB_ID_P2, SGB_ID_P3, SGB_ID_P4, SGB_ID_P1, SGB_ID_P2
    db SGB_SEQ_END

; Two players: alternate 1,2 — players 3 and 4 must never appear.
SeqTwoPlayers:
    db SGB_ID_P1, SGB_ID_P2, SGB_ID_P1, SGB_ID_P2, SGB_ID_P1, SGB_ID_P2
    db SGB_SEQ_END

; mlt_req_player_cycle.sgb2.mooneye — MLT_REQ multiplexing on an SGB2.
;
; Byte-for-byte the same script as the SGB build; only the model token differs,
; so the runner constructs SGB2 hardware instead. The SGB2 differs from the SGB1
; in its clock source (a dedicated crystal instead of the SNES-derived clock),
; not in the ICD2 command interface, so the documented MLT_REQ enumeration must
; be identical. This ROM pins that: a model gate that accidentally scoped SGB
; command handling to SGB1 only would pass the sgb build and fail here.

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

SeqOnePlayer:
    db SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1
    db SGB_SEQ_END

SeqFourPlayers:
    db SGB_ID_P1, SGB_ID_P2, SGB_ID_P3, SGB_ID_P4, SGB_ID_P1, SGB_ID_P2
    db SGB_SEQ_END

SeqTwoPlayers:
    db SGB_ID_P1, SGB_ID_P2, SGB_ID_P1, SGB_ID_P2, SGB_ID_P1, SGB_ID_P2
    db SGB_SEQ_END

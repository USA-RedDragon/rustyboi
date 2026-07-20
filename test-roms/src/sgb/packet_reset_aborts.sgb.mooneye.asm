; packet_reset_aborts.sgb.mooneye — a mid-transfer reset pulse aborts the packet.
;
; Pan Docs "Command Packet Transfers": "A command packet transfer must be
; initiated by setting JOYP bits 4 and 5 both to 0; this will reset and start
; the ICD2 packet receiving circuit." The receiver's response to both-lines-LOW
; is therefore a *reset*, not merely a "begin if idle" — which is what lets a
; game that garbled a transfer resynchronise by starting the next one, rather
; than the ICD2 staying permanently one bit out of phase.
;
; Subtest 0 sends a well-formed MLT_REQ four-player packet but drives an extra
; both-LOW pulse after the 64th data bit. If that pulse resets the bit counter,
; the 128 bits sent never assemble into one complete 128-bit packet, no command
; is dispatched, and the joypad ID stays at player 1. A receiver that treated
; both-LOW as a no-op once already started would clock all 128 bits straight
; through, dispatch MLT_REQ, and enumerate $F,$E,$D,$C instead.
;
; Subtest 1 then sends the same packet cleanly and asserts the four-player
; enumeration does appear. That half matters as much as the first: it proves the
; abort left the receiver resynchronised and ready rather than wedged, and it
; stops subtest 0 from being satisfiable by an implementation that simply
; ignores MLT_REQ altogether.
;
; Grounding note: the mid-transfer case is an application of the documented
; "reset and start" wording rather than a separately documented behaviour. It is
; corroborated by the public sgb-ext-test protocol stress ROM, whose real-SGB
; reference screenshot shows $10->$00 and $20->$00 aborting a packet in flight.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "sgb_packet.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start
    ; Reserve the cartridge header. Without this the linker floats the shared
    ; sgb_packet code into $104-$14F and rgbfix overwrites it with the logo and
    ; checksums.
    ds $150 - @, 0

SECTION "main", ROM0[$1000]
Start:
    ld a, 30                       ; SGB warm-up before command packets
    call SgbWaitFrames
    ld a, P1_BOTH_HIGH
    ldh [rP1], a
    xor a
    ld [SGB_RESULT], a

    ; --- Subtest 0: reset after bit 64 -> the command never lands. ---
    ld hl, SgbPktFourPlayers
    call SgbSendPacketResetMidway
    ld a, 4                        ; Pan Docs: 4 frames between packets
    call SgbWaitFrames
    ld hl, SeqStillOnePlayer
    call SgbCheckSequence
    sgb_record %01

    ; --- Subtest 1: the receiver recovered; a clean packet still works. ---
    ld hl, SgbPktFourPlayers
    call SgbSendPacket
    ld a, 4
    call SgbWaitFrames
    ld hl, SeqFourPlayers
    call SgbCheckSequence
    sgb_record %10

    ld a, [SGB_RESULT]
    ld e, a
    ld a, 2
    call SgbShowResults
    ld a, [SGB_RESULT]
    cp %11
    jr nz, .failed
    test_success
.failed:
    test_failure

SECTION "sequences", ROM0

; The aborted packet must leave the SGB in its one-player power-on mode.
SeqStillOnePlayer:
    db SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1
    db SGB_SEQ_END

; ...and a clean retry must then enumerate all four players and wrap.
SeqFourPlayers:
    db SGB_ID_P1, SGB_ID_P2, SGB_ID_P3, SGB_ID_P4, SGB_ID_P1, SGB_ID_P2
    db SGB_SEQ_END

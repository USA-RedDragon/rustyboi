; sgb_mlt_req_cycle.inc — shared body for the MLT_REQ joypad-multiplexing ROMs.
;
; Four subtests, each a table of joypad IDs expected from successive P15
; LOW->HIGH advances (Pan Docs "Multiplayer Command"):
;
;   0. Power-on state. No MLT_REQ has been sent, so the SGB is in one-player
;      mode and the ID never moves off player 1. The gbdev ICD2 register page
;      gates the advance on "If the number of controllers is greater than one",
;      and a one-player SGB has to look like a plain Game Boy here or every
;      single-player SGB title would misread its joypad.
;   1. MLT_REQ four players -> the ID enumerates $F,$E,$D,$C and wraps back to
;      $F. Pan Docs gives the ID table ($xF..$xC = players 1..4) and, for the
;      starting point, "going from one player to two-or-four player ... will
;      always start with player 1 active".
;   2. MLT_REQ one player -> back to a fixed player 1. Pan Docs: "Changing the
;      number of active players ANDs the currently selected player minus one
;      with the number of players in that mode minus one", so any index ANDed
;      into one-player mode collapses to player 1.
;   3. MLT_REQ two players -> the ID alternates $F,$E,$F,$E. Same two rules.
;
; Subtests 1-3 are each entered from a *known* player count, which is what makes
; them deterministic: Pan Docs warns that "sending the MLT_REQ command will
; increment the counter several times" (every `1` bit in a packet is a P15
; LOW->HIGH pulse), so subtests 1 and 3 are entered from one-player mode, where
; the counter cannot move during the transfer, and subtest 2's AND-to-zero holds
; regardless of where the counter ended up.
;
; The same body drives the DMG build with all-player-1 tables: a Game Boy with
; no SGB behind it must never multiplex, which is what pins the asymmetry.

IF !DEF(SGB_MLT_REQ_CYCLE_INC)
DEF SGB_MLT_REQ_CYCLE_INC EQU 1

; \1..\4 = expected-ID tables for subtests 0..3.
MACRO sgb_mlt_req_cycle_test
    ; The SGB system software needs a moment after power-on before it answers
    ; command packets (libbet, hardware-validated, waits ~30 frames before
    ; attempting MLT_REQ detection at all).
    ld a, 30
    call SgbWaitFrames

    ; Deselect both lines: the state the joypad-ID readback is defined in. Safe
    ; to do blind — the SGB is still in one-player mode, where no P15 edge can
    ; advance the index.
    ld a, P1_BOTH_HIGH
    ldh [rP1], a
    xor a
    ld [SGB_RESULT], a

    ; --- Subtest 0: power-on one-player mode never multiplexes ---
    ld hl, \1
    call SgbCheckSequence
    sgb_record %0001

    ; --- Subtest 1: MLT_REQ four players ---
    ld hl, SgbPktFourPlayers
    call SgbSendPacket
    ld a, 4                        ; Pan Docs: wait 4 frames between packets
    call SgbWaitFrames
    ld hl, \2
    call SgbCheckSequence
    sgb_record %0010

    ; --- Subtest 2: MLT_REQ back to one player ---
    ld hl, SgbPktOnePlayer
    call SgbSendPacket
    ld a, 4
    call SgbWaitFrames
    ld hl, \3
    call SgbCheckSequence
    sgb_record %0100

    ; --- Subtest 3: MLT_REQ two players ---
    ld hl, SgbPktTwoPlayers
    call SgbSendPacket
    ld a, 4
    call SgbWaitFrames
    ld hl, \4
    call SgbCheckSequence
    sgb_record %1000

    ld a, [SGB_RESULT]
    ld e, a
    ld a, 4
    call SgbShowResults
    ld a, [SGB_RESULT]
    cp %1111
    jr nz, .cycle_failed\@
    test_success
.cycle_failed\@:
    test_failure
ENDM

ENDC

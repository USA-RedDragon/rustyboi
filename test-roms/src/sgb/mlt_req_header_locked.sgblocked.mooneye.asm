; mlt_req_header_locked.sgblocked.mooneye — the SGB header unlock gate.
;
; Real SGB hardware, but this build is deliberately fixed WITHOUT the SGB header
; entries: the `sgblocked` model token drops `rgbfix -s -l 0x33`, leaving SGB
; flag $0146 = $00 and old licensee $014B = $00.
;
; Pan Docs "Unlocking SGB Functions": "two special entries must be set in order
; to unlock SGB functions: SGB flag: Must be set to $03 ... Old licensee code:
; Must be set to $33 ... When these entries aren't set, the game will still work
; just like all 'monochrome' Game Boy games, but it cannot access any of the
; special SGB functions." Pan Docs "The Cartridge Header" restates it from the
; hardware side: "The SGB will ignore any command packets if this byte is set to
; a value other than $03".
;
; So this cart may transmit a perfectly well-formed MLT_REQ and the SGB must
; drop it on the floor. Subtest 0 establishes the baseline (one player at
; power-on); subtest 1 sends MLT_REQ four-players and asserts the ID *still*
; never leaves player 1 across a full six-advance sweep — an unlocked receiver
; would run $F,$E,$D,$C,$F,$E and miss on the second reading.
;
; Note the negative form is unavoidable here: the gate's whole observable effect
; is the absence of SGB behaviour. What keeps it honest is the sibling ROM —
; mlt_req_player_cycle.sgb runs the same hardware and, in its SUBTEST 1, the
; identical four-player MLT_REQ packet followed by the identical six-read sweep
; this ROM's subtest 1 uses. That sibling is a four-subtest program and this is
; a bespoke two-subtest one, so they are NOT the same ROM minus two header
; bytes; the shared element is exactly that packet + sweep, and on it the
; unlocked sibling demands the opposite answer — the enumeration
; $F,$E,$D,$C,$F,$E where this locked build must stay pinned to player 1.

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

    ; --- Subtest 0: baseline. One player at power-on, no packets sent yet. ---
    ld hl, SeqLockedOut
    call SgbCheckSequence
    sgb_record %01

    ; --- Subtest 1: a well-formed MLT_REQ four-player packet is ignored. ---
    ld hl, SgbPktFourPlayers
    call SgbSendPacket
    ld a, 4
    call SgbWaitFrames
    ld hl, SeqLockedOut
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

; A locked receiver leaves the joypad path exactly as a plain Game Boy's.
SeqLockedOut:
    db SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1, SGB_ID_P1
    db SGB_SEQ_END

; apu_len_extra_clock.inc — shared body for the NRx4 extra-length-clock ROMs.
;
; Pan Docs, Audio Details, Obscure Behavior
; (https://gbdev.io/pandocs/Audio_details.html):
;
;   "Extra length clocking occurs when writing to NRx4 when the DIV-APU next
;   step is one that doesn't clock the length timer. In this case, if the
;   length timer was PREVIOUSLY disabled and now enabled and the length timer
;   is not zero, it is decremented. If this decrement makes it zero and trigger
;   is clear, the channel is disabled. On the CGB-02, the length timer only has
;   to have been disabled before; the current length enable state doesn't
;   matter. This breaks at least one game (Prehistorik Man), and was fixed on
;   CGB-04 and CGB-05."
;
; Both subtests exercise the revision-independent core of that rule on CH2 (no
; sweep coupling): length previously DISABLED, counter non-zero, enable bit
; written without a trigger. That case extra-clocks on every revision — the
; CGB-02 difference only widens the condition — so one body serves DMG and CGB.
;
; Timeline (M-cycles, single speed; t=0 = the DIV reset in apu_power_cycle,
; which also resets the DIV-APU step, so ticks land at t = 2048k and the odd
; ticks k=1,3,5,... are the length steps at t = 2048, 6144, 10240, ...):
;
;   setup      t=5   APU on; t=10 NR22=$F0 (DAC on); t=15 NR21 -> counter = 2
;              (NR21 is always rewritten after the power cycle: the ROM never
;              depends on the counter's power-on/power-off retention)
;   case 1     t=2565 NR24=$40. Between the length steps at 2048 and 6144 the
;   (1st half) next DIV-APU step (t=4096) does NOT clock length -> the enable
;              write clocks the counter once: 2 -> 1. t=2570 NR24=$C0 triggers
;              (counter non-zero, no reload); the length step at t=6144 takes
;              1 -> 0 and shuts the channel off.
;   case 2     t=605 NR24=$40. The next DIV-APU step (t=2048) IS a length step
;   (2nd half) -> no extra clock, counter stays 2. t=610 NR24=$C0 triggers; the
;              length steps at t=2048 and t=6144 take 2 -> 1 -> 0, so the
;              channel again shuts off at t=6144.
;
; Both cases then time the shutoff with ApuPollChannelOff (15 M-cycles per
; iteration, first NR52 read ending 17 M-cycles after the trigger write):
;
;   case 1: polls from t=2587, expiry t=6144 -> HL = 238. A missing extra
;           clock leaves counter 2, expiry t=10240 -> HL = 508 (rejected).
;   case 2: polls from t=627, expiry t=6144 -> HL = 368. A spurious extra
;           clock leaves counter 1, expiry t=2048 -> HL = 95 (rejected).
;
; The +/-60-iteration windows (900 M-cycles) absorb sub-instruction write/read
; placement while rejecting every wrong outcome above and any DIV-APU phase
; displaced by half a period (1024 M-cycles = 68 iterations) or more.

IF !DEF(APU_LEN_EXTRA_CLOCK_INC)
DEF APU_LEN_EXTRA_CLOCK_INC EQU 1

DEF LEC_LEN     EQU 2      ; length counter loaded before each case
DEF LEC_BOUND   EQU 1500   ; poll bound; far outside every expectation window
DEF LEC_TOL     EQU 60     ; window half-width, poll iterations

DEF LEC_DELAY1  EQU 2545   ; t=15 -> t=2560: enable write lands at t=2565
DEF LEC_EXPECT1 EQU 238    ; poll iterations, extra clock applied (2->1)
DEF LEC_DELAY2  EQU 585    ; t=15 -> t=600: enable write lands at t=605
DEF LEC_EXPECT2 EQU 368    ; poll iterations, no extra clock (counter stays 2)

DEF LEC_MEAS1   EQU $C0F4  ; measured HL, case 1 (debug aid, not graded)
DEF LEC_MEAS2   EQU $C0F6  ; measured HL, case 2

; One phase case: pin the DIV-APU phase, load the counter, write the length
; enable at t = 15 + \1 (+5), trigger, and grade the measured shutoff time.
; \1 = apu_delay_mcycles argument, \2 = expected iterations, \3 = APU_RESULT
; bit, \4 = measurement store address.
MACRO lec_case
    apu_power_cycle
    ld a, $F0
    ldh [rNR22], a                       ; DAC on, constant full volume
    ld a, 64 - LEC_LEN
    ldh [rNR21], a                       ; counter := LEC_LEN, fresh after power-on
    apu_delay_mcycles \1
    ld a, AUDHIGH_LEN_ON
    ldh [rNR24], a                       ; enable only, no trigger: the write under test
    ld a, AUDHIGH_TRIGGER | AUDHIGH_LEN_ON
    ldh [rNR24], a                       ; trigger; counter non-zero so no reload
    ld c, AUDENA_CH2
    ld de, LEC_BOUND
    call ApuPollChannelOff
    ld a, l
    ld [\4], a
    ld a, h
    ld [\4 + 1], a
    ; A := 0 iff \2 - LEC_TOL <= HL <= \2 + LEC_TOL.
    ld a, l
    sub LOW(\2 - LEC_TOL)
    ld a, h
    sbc HIGH(\2 - LEC_TOL)
    jr c, .out\@
    ld a, LOW(\2 + LEC_TOL)
    sub l
    ld a, HIGH(\2 + LEC_TOL)
    sbc h
    jr c, .out\@
    xor a
    jr .grade\@
.out\@:
    ld a, 1
.grade\@:
    apu_record \3
ENDM

MACRO len_extra_clock_test
    xor a
    ld [APU_RESULT], a
    lec_case LEC_DELAY1, LEC_EXPECT1, $01, LEC_MEAS1
    lec_case LEC_DELAY2, LEC_EXPECT2, $02, LEC_MEAS2
    ld a, [APU_RESULT]
    cp $03
    jr nz, .fail\@
    test_success
.fail\@:
    test_failure
ENDM

ENDC

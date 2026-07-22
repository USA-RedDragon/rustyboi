; apu_power_cycle_len.inc — shared body for the power-cycle length-counter ROM.
;
; An NRx1 length-load register is writable while the APU is off ONLY on the
; monochrome models; on CGB it ignores the write while off. Pan Docs, "Audio
; Registers" (https://gbdev.io/pandocs/Audio_Registers.html): with the APU off
; every register but NR52 is read-only, "except on monochrome models" for the
; length timers; gbdev wiki "Power Control"
; (https://gbdev.gg8.se/wiki/articles/Gameboy_sound_hardware) makes the same
; monochrome-only exception. So a while-off NR21 write STICKS on DMG and is
; DROPPED on CGB — the documented DMG/CGB fork this ROM pins.
;
; It is read out WITHOUT the trigger reload that would hide it. A trigger
; reloads a length counter only when it is already zero (Pan Docs, "Audio
; details", Triggering), so a non-zero counter survives the trigger untouched.
; That is what makes the fork observable:
;
;   NR21 = $30 written while OFF, then power on and trigger CH2 with length
;   enabled and NR21 never rewritten:
;     DMG: the while-off write survived -> counter 64 - $30 = 16, non-zero, so
;          the trigger does NOT reload it -> the channel lives 16 length ticks.
;     CGB: the write was rejected -> counter 0 -> the trigger reloads the full
;          64 -> 64 ticks.
;   The +/-3 window around each expectation separates 16 from 64 cleanly.
;
; CH2 is used deliberately: the monochrome writable-while-off exception names
; the pulse-channel length loads, so CH2 is where model and documentation agree
; without extrapolation.
;
; This is not merely an old-model/new-model discriminator: a model that reloaded
; counters to max at power-OFF would still measure the same 16 on DMG (the
; while-off write overwrites whatever the power-off left) and the same 64 on CGB
; (reload-to-max and trigger-reload-from-zero coincide). It is instead the guard
; on the documented fork itself, and it also catches an implementation that
; zeroes the counters at power-ON rather than at power-off: that wipes the DMG
; while-off write and measures 64 where 16 is required.
;
; Timeline (M-cycles, single speed; t=0 = the DIV reset, so the DIV-APU steps
; land at t = 2048k and the length steps — every second one, because power-on
; resets the step counter — at t = 2048 + 4096k):
;
;   t=0  DIV reset with the APU off; t=5 NR21=$30 written WHILE OFF (length load
;        64 - $30 = 16 on DMG, dropped on CGB); t=10 APU on; t=15 NR22=$F0;
;        t=20 NR24=$C0, trigger + length enable, with NR21 never rewritten. The
;        next DIV-APU step (t=2048) is a length step, so the extra-length-clock
;        quirk does not fire here.
;
; NR52 is sampled once per length tick (ApuPollChannelOffTicks), so the result
; is in length ticks and is at most one tick under the counter the trigger
; armed. The +/-3 window absorbs that, plus a one-tick extra clock in any phase
; not accounted for above, while still rejecting the alternative outcome (16 and
; 64 are far more than 3 ticks apart).

IF !DEF(APU_POWER_CYCLE_LEN_INC)
DEF APU_POWER_CYCLE_LEN_INC EQU 1

DEF PCL_BOUND   EQU 200    ; sample bound in length ticks; past every window
DEF PCL_TOL     EQU 3      ; window half-width, length ticks

DEF PCL_NR21    EQU $30    ; while-off length load: counter = 64 - $30 = 16
DEF PCL_EXPECT2_DMG EQU 64 - PCL_NR21   ; 16: the while-off write survives
DEF PCL_EXPECT2_CGB EQU 64              ; write rejected -> 0 -> trigger reload

DEF PCL_MEAS2   EQU $C0F6  ; measured ticks (debug aid, not graded)

; Power off, load NR21 while off, power on, trigger with length enabled and no
; NR21 rewrite. \1 = expected ticks, \2 = APU_RESULT bit.
MACRO pcl_write_while_off_case
    xor a
    ldh [rNR52], a                       ; APU off
    ldh [rDIV], a                        ; t=0
    ld a, PCL_NR21
    ldh [rNR21], a                       ; t=5: DMG accepts, CGB drops
    ld a, AUDENA_ON
    ldh [rNR52], a                       ; t=10: APU on
    ld a, $F0
    ldh [rNR22], a                       ; t=15: DAC on
    ld a, AUDHIGH_TRIGGER | AUDHIGH_LEN_ON
    ldh [rNR24], a                       ; t=20: trigger + enable, no NR21 write
    ld c, AUDENA_CH2
    ld de, PCL_BOUND
    call ApuPollChannelOffTicks
    ld a, l
    ld [PCL_MEAS2], a
    ld a, h
    ld [PCL_MEAS2 + 1], a
    apu_grade_window \1, PCL_TOL
    apu_record \2
ENDM

; \1 = the expected ticks (PCL_EXPECT2_DMG / PCL_EXPECT2_CGB).
MACRO power_cycle_len_test
    xor a
    ld [APU_RESULT], a
    pcl_write_while_off_case \1, $01
    ld a, [APU_RESULT]
    cp $01
    jr nz, .fail\@
    test_success
.fail\@:
    test_failure
ENDM

ENDC

; apu_power_cycle_len.inc — shared body for the power-cycle length-counter ROMs.
;
; What the length counters hold after an NR52 power cycle, probed WITHOUT the
; trigger reload that normally hides the answer. A trigger reloads a zero
; counter to its maximum (Pan Docs, "Audio details", Triggering), so a plain
; "power cycle, trigger, time the death" measures 64 whether the counter came
; out of the power cycle holding 0 or holding 64. Only the length-enable path,
; which decrements but never reloads, can tell the two apart.
;
; Model, from gbdev wiki "Power Control"
; (https://gbdev.gg8.se/wiki/articles/Gameboy_sound_hardware): the length
; counters are "always zero at power on (CGB-02, CGB-04, CGB-05)", and on the
; monochrome models the NRx1 length-load registers stay writable while the APU
; is off (Pan Docs, "Audio Registers", makes the same monochrome exception). So
; a counter comes out of a power cycle at 0, except on DMG where an NRx1 write
; made while the APU was off is still in it.
;
; SameBoy (Core/apu.c, the NR52 write handler) is the reference implementation
; of exactly that: powering off `memset`s the APU state, zeroing every length
; counter, powering on runs `GB_apu_init`, and
; `if (!GB_is_cgb(gb) && (value & 0x80))` restores the four pulse-channel
; lengths captured at handler entry — i.e. on DMG the counters that existed
; just before the power-on write survive it, which is what makes a while-off
; NRx1 write stick.
;
; PROVENANCE: subtest 1's cell — what a counter holds after a power cycle with
; NO NRx1 write anywhere — is model-derived, not silicon-measured. The gbdev
; wiki sentence above covers the CGB revisions it names; the DMG leg and the
; exact "zero at power-off rather than reload-to-max" mechanism are taken from
; SameBoy. Per ../README.md's provenance rule this ROM is the written-down form
; of the model rustyboi implements, and it is queued for hardware-bench
; confirmation; the DMG/CGB asymmetry in subtest 2 is the documented half.
;
; Both subtests use CH2. That is deliberate: SameBoy's DMG restore names the
; PULSE channel lengths specifically, so CH2 is the one channel where model and
; documentation agree without extrapolation.
;
; Timeline (M-cycles, single speed; t=0 = the DIV reset, so the DIV-APU steps
; land at t = 2048k and the length steps — every second one, because power-on
; resets the step counter — at t = 2048 + 4096k):
;
;   subtest 1  t=5    APU on. t=10 NR22=$F0 (DAC on).
;              t=15   NR24=$40: length ENABLE, no trigger, and NR21 is never
;                     written. The next DIV-APU step (t=2048) is a length step,
;                     so the extra-length-clock quirk does not fire here.
;              t=15..33800: the length steps at t = 2048, 6144, ... 30720 —
;                     exactly 8 of them — pass.
;              t=33800 NR24=$C0: trigger. The next DIV-APU step (t=34816) is
;                     again a length step, so no extra clock, and the reload of
;                     a zero counter is the undecremented 64.
;
;              A counter that came out of the power cycle at 0 is not armed by
;              the enable (the decrement rule needs a non-zero timer) and is not
;              touched by the 8 length steps, so the trigger reloads the full
;              64 -> the channel lives 64 ticks. A counter that came out at 64
;              IS armed, the 8 steps take it to 56, and the trigger finds it
;              non-zero and does not reload -> 56 ticks. The +/-3 window around
;              64 rejects 56.
;
;   subtest 2  t=0 DIV reset with the APU off; t=5 NR21=$30 written WHILE OFF
;              (length load 64 - $30 = 16 on DMG, dropped on CGB); t=10 APU on;
;              t=15 NR22=$F0; t=20 NR24=$C0, trigger + length enable, with NR21
;              never rewritten. The next DIV-APU step (t=2048) is a length step,
;              so again no extra clock.
;
;              DMG: the while-off write survived the power-on -> counter 16,
;              non-zero, so the trigger does not reload -> 16 ticks.
;              CGB: the write was rejected -> counter 0 -> the trigger reloads
;              64 -> 64 ticks.
;
;              Subtest 2 is not an old-model/new-model discriminator (a model
;              that reloads counters to max at power-off measures the same 16 on
;              DMG, because the while-off write overwrites whatever the power-off
;              left, and the same 64 on CGB, because reload-to-max and
;              trigger-reload-from-zero coincide). It is the guard on the other
;              half of the rule: an implementation that zeroes the counters at
;              power-ON instead of at power-off wipes the DMG while-off write
;              and measures 64 where 16 is required.
;
; Both subtests sample NR52 once per length tick (ApuPollChannelOffTicks), so
; the result is in length ticks and is at most one tick under the counter the
; trigger armed. The +/-3 windows absorb that, plus a one-tick extra clock in
; any phase not accounted for above, while still rejecting every alternative
; outcome named here (16 / 56 / 64 are 8 or more ticks apart).

IF !DEF(APU_POWER_CYCLE_LEN_INC)
DEF APU_POWER_CYCLE_LEN_INC EQU 1

DEF PCL_BOUND   EQU 200    ; sample bound in length ticks; past every window
DEF PCL_TOL     EQU 3      ; window half-width, length ticks

; t=15 (end of the enable write) -> t=33795, so the trigger write ends at
; t=33800: after the 8th length step (t=30720) and before the 9th (t=34816).
DEF PCL_DELAY1  EQU 33780
DEF PCL_TICKS1  EQU 8      ; length steps between the enable and the trigger
DEF PCL_EXPECT1 EQU 64     ; power-cycled counter 0 -> full trigger reload

DEF PCL_NR21    EQU $30    ; while-off length load: counter = 64 - $30 = 16
DEF PCL_EXPECT2_DMG EQU 64 - PCL_NR21   ; 16: the while-off write survives
DEF PCL_EXPECT2_CGB EQU 64              ; write rejected -> 0 -> trigger reload

DEF PCL_MEAS1   EQU $C0F4  ; measured ticks (debug aid, not graded)
DEF PCL_MEAS2   EQU $C0F6

; Subtest 1: power cycle, arm length without triggering, let 8 length steps
; pass, then trigger. \1 = expected ticks, \2 = APU_RESULT bit.
MACRO pcl_no_trigger_case
    apu_power_cycle                      ; t=0 DIV reset, APU on at t=5
    ld a, $F0
    ldh [rNR22], a                       ; t=10: DAC on
    ld a, AUDHIGH_LEN_ON
    ldh [rNR24], a                       ; t=15: enable only, NR21 never written
    apu_delay_mcycles PCL_DELAY1
    ld a, AUDHIGH_TRIGGER | AUDHIGH_LEN_ON
    ldh [rNR24], a                       ; t=33800: trigger, past 8 length steps
    ld c, AUDENA_CH2
    ld de, PCL_BOUND
    call ApuPollChannelOffTicks
    ld a, l
    ld [PCL_MEAS1], a
    ld a, h
    ld [PCL_MEAS1 + 1], a
    apu_grade_window \1, PCL_TOL
    apu_record \2
ENDM

; Subtest 2: power off, load NR21 while off, power on, trigger with length
; enabled and no NR21 rewrite. \1 = expected ticks, \2 = APU_RESULT bit.
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

; \1 = subtest 2's expected ticks (PCL_EXPECT2_DMG / PCL_EXPECT2_CGB).
MACRO power_cycle_len_test
    xor a
    ld [APU_RESULT], a
    pcl_no_trigger_case PCL_EXPECT1, $01
    pcl_write_while_off_case \1, $02
    ld a, [APU_RESULT]
    cp $03
    jr nz, .fail\@
    test_success
.fail\@:
    test_failure
ENDM

ENDC

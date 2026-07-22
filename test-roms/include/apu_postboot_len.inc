; apu_postboot_len.inc — shared body for the post-boot length-counter ROMs.
;
; The hidden length counters a game inherits from the boot ROM are the POWER-ON
; values, modified only by the NRx1 writes the boot ROM itself made. Pan Docs,
; "Sound Controller" / "Audio Registers"
; (https://gbdev.io/pandocs/Audio_Registers.html) documents the post-boot
; register bytes; of the NRx1 length-load registers, only NR11 is among them
; (NR11 = $80 on DMG and CGB). NR21, NR31 and NR41 are never written, so CH2,
; CH3 and CH4 hand off holding whatever power-on left in them.
;
; gbdev wiki, "Power Control"
; (https://gbdev.gg8.se/wiki/articles/Gameboy_sound_hardware): the length
; counters are "always zero at power on (CGB-02, CGB-04, CGB-05)". So the
; post-boot counters are 0 for CH2/CH3/CH4 and 64 for CH1.
;
; The other half of the rule is the trigger reload, Pan Docs "Audio details",
; Triggering: a trigger with the length timer already at zero reloads it to the
; channel's maximum (64 for the pulse and noise channels, 256 for the wave
; channel). Put together, the observable this ROM pins is:
;
;   trigger CH2/CH3/CH4 without EVER writing NRx1  ->  full 64/256/64 reload
;
; and not the one-tick death a counter of 1 would give.
;
; Each case, per channel:
;
;   1. turn the DAC on through NRx2 (NR30 bit 7 for CH3) — never NRx1;
;   2. NRx4 = $80: trigger with length DISABLED. A zero counter reloads to the
;      maximum here, and with bit 6 clear the extra-length-clock quirk cannot
;      fire, so the reload is exactly 64/256/64 in every DIV-APU phase;
;   3. NRx4 = $40: enable the length timer without retriggering. Nothing has
;      decremented the counter in between (length was disabled), so this arms
;      the full reload. This write CAN extra-clock (Pan Docs "Audio details",
;      Obscure Behavior: previously disabled, now enabled, counter non-zero),
;      costing at most one tick;
;   4. sample NR52 once per length tick until the channel bit clears.
;
; Expected samples = the loaded counter, minus up to one tick for the extra
; clock in step 3 and up to one more for the sub-tick offset of the first
; sample (see ApuPollChannelOffTicks), hence the +/-4 windows. A counter that
; was NOT the power-on 0 — the 1 a stray NRx1 length load leaves — dies at the
; first or second tick and is rejected by every window here.
;
; CH1 is a pipeline/sanity cell, NOT a boot-preservation discriminator. It runs
; the identical trigger-with-length-off sequence as the other channels, so its
; step-2 trigger reloads the counter to the full 64 regardless of whatever the
; boot ROM's NR11 = $80 left in it — the reload overwrites the boot value either
; way. So it only confirms the measurement path reads back the full 64 on the
; one channel the boot ROM did write; it cannot tell a preserved boot length
; from any other value.

IF !DEF(APU_POSTBOOT_LEN_INC)
DEF APU_POSTBOOT_LEN_INC EQU 1

DEF PBL_BOUND   EQU 400    ; sample bound in length ticks; past every window
DEF PBL_TOL     EQU 4      ; window half-width, length ticks
DEF PBL_SETTLE  EQU 24     ; M-cycles between trigger and length-enable

DEF PBL_EXPECT_CH1 EQU 64  ; boot ROM's NR11 = $80
DEF PBL_EXPECT_CH2 EQU 64  ; power-on 0, reloaded by the trigger
DEF PBL_EXPECT_CH3 EQU 256 ; power-on 0, reloaded by the trigger (wave: 256)
DEF PBL_EXPECT_CH4 EQU 64  ; power-on 0, reloaded by the trigger

DEF PBL_MEAS_CH1 EQU $C0F4 ; measured ticks (debug aid, not graded)
DEF PBL_MEAS_CH2 EQU $C0F6
DEF PBL_MEAS_CH3 EQU $C0F8
DEF PBL_MEAS_CH4 EQU $C0FA

; One channel: DAC on, trigger with length off, enable length, time the death.
; \1 = DAC register, \2 = DAC value, \3 = NRx4, \4 = NR52 channel bit,
; \5 = expected ticks, \6 = APU_RESULT bit, \7 = measurement store.
MACRO pbl_case
    ld a, \2
    ldh [\1], a                          ; DAC on; NRx1 is never touched
    ld a, AUDHIGH_TRIGGER
    ldh [\3], a                          ; trigger, length OFF: 0 -> full reload
    apu_delay_mcycles PBL_SETTLE         ; let CH4's deferred DMG trigger land
    ld a, AUDHIGH_LEN_ON
    ldh [\3], a                          ; enable length, no retrigger
    ld c, \4
    ld de, PBL_BOUND
    call ApuPollChannelOffTicks
    ld a, l
    ld [\7], a
    ld a, h
    ld [\7 + 1], a
    apu_grade_window \5, PBL_TOL
    apu_record \6
ENDM

MACRO postboot_len_test
    xor a
    ld [APU_RESULT], a
    pbl_case rNR12, $F0, rNR14, AUDENA_CH1, PBL_EXPECT_CH1, $01, PBL_MEAS_CH1
    pbl_case rNR22, $F0, rNR24, AUDENA_CH2, PBL_EXPECT_CH2, $02, PBL_MEAS_CH2
    pbl_case rNR30, $80, rNR34, AUDENA_CH3, PBL_EXPECT_CH3, $04, PBL_MEAS_CH3
    pbl_case rNR42, $F0, rNR44, AUDENA_CH4, PBL_EXPECT_CH4, $08, PBL_MEAS_CH4
    ld a, [APU_RESULT]
    cp $0F
    jr nz, .fail\@
    test_success
.fail\@:
    test_failure
ENDM

ENDC

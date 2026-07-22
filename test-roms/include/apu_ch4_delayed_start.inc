; apu_ch4_delayed_start.inc — a channel-4 trigger always starts the channel,
; whatever ripple phase the write lands on, and however soon it is repeated.
;
; Pan Docs, "Audio Registers" (https://gbdev.io/pandocs/Audio_Registers.html):
; writing NRx4 with bit 7 set "triggers" the channel, and NR52's per-channel
; bits report "whether each channel is turned on" — a trigger with the DAC on
; (NR42 upper 5 bits non-zero) turns channel 4 on, so its NR52 bit reads 1
; afterwards. That is the whole assertion here; it holds on every model.
;
; What makes it worth a ROM is the non-CGB ripple-phase deferral underneath it.
; On DMG the channel-4 start is not applied at the write: a trigger landing on
; an unaligned ripple phase is held and applied a few cycles later (SameBoy
; models it as `dmg_delayed_start`, 6 APU cycles). The deferral is only a
; delay — the start it defers must still happen, exactly once. An emulator that
; re-enters the deferral instead of completing it never starts the channel at
; all: NR52 bit 3 stays 0 forever and the game hears a channel that never
; retriggers (a drum hit that latches into continuous noise instead of
; decaying). This ROM pins the delay's OBSERVABLE end state, never its length —
; the 6-cycle figure is emulator-model-derived and deliberately not asserted.
;
; The CPU can only place a write on the M-cycle grid, so a NOP sweep is how a
; ROM reaches the different ripple sub-phases a trigger can land on. Subtests
; 0-3 sweep four consecutive M-cycle placements of the trigger write; hardware
; must start the channel on every one of them.
;
; Subtest 4 issues a second trigger 2 M-cycles (8 T-cycles) after the first —
; closer than any deferral window, so it lands while the first start is still
; pending. That is the tightest retrigger the CPU can express (`ldh [c], a`
; twice), and it must still leave the channel on.
;
; Every subtest is self-checking in both directions: it first turns the DAC off
; and requires NR52 bit 3 to read CLEAR (so a channel left on by the previous
; subtest cannot pass this one for free), then triggers and requires it SET.

IF !DEF(APU_CH4_DELAYED_START_INC)
DEF APU_CH4_DELAYED_START_INC EQU 1

; NR42 = volume 15, decreasing, sweep pace 0: the DAC is on (upper 5 bits
; non-zero), so a trigger must turn the channel on.
DEF CH4_ENV_DAC_ON EQU $F0

; Result bits, one per subtest.
DEF CH4_BIT_PHASE0   EQU $01
DEF CH4_BIT_PHASE1   EQU $02
DEF CH4_BIT_PHASE2   EQU $04
DEF CH4_BIT_PHASE3   EQU $08
DEF CH4_BIT_RETRIG   EQU $10
DEF CH4_BIT_ALL      EQU $1F

; Leave channel 4 off with the DAC disabled and NR43 at its fastest ripple
; setting, and put the "was it really off?" canary in B (0 = off, as required).
MACRO ch4_arm_off
    xor a
    ldh [rNR42], a         ; DAC off -> channel 4 off
    ldh [rNR43], a         ; divisor 0, shift 0: fastest ripple clock
    ldh a, [rNR52]
    and AUDENA_CH4
    ld b, a                ; canary: non-zero here fails the subtest
    ld a, CH4_ENV_DAC_ON
    ldh [rNR42], a         ; DAC back on; only a trigger can start the channel
ENDM

; Read the verdict: A = 0 when channel 4 is on AND the canary saw it off
; beforehand. Left in `apu_record`'s calling convention.
MACRO ch4_check_started
    ldh a, [rNR52]
    and AUDENA_CH4
    xor AUDENA_CH4         ; 0 = on (pass), non-zero = never started
    or b                   ; fold in the "was off first" canary
ENDM

; Trigger channel 4 with the write placed \1 M-cycles later than subtest 0,
; then require the channel to be on. \2 = result bit.
MACRO ch4_phase_subtest    ; \1 = M-cycle offset, \2 = result bit
    ch4_arm_off
    REPT \1
    nop
    ENDR
    ld a, AUDHIGH_TRIGGER
    ldh [rNR44], a
    REPT 8                 ; well past any deferral the start can be held for
    nop
    ENDR
    ch4_check_started
    apu_record \2
ENDM

; Two triggers 2 M-cycles apart — the second lands while the first start is
; still pending — must still leave the channel on. \1 = result bit.
MACRO ch4_retrigger_subtest   ; \1 = result bit
    ch4_arm_off
    ld c, LOW(rNR44)
    ld a, AUDHIGH_TRIGGER
    ldh [c], a             ; 2 M-cycles
    ldh [c], a             ; ...so this one lands inside the pending start
    REPT 8
    nop
    ENDR
    ch4_check_started
    apu_record \1
ENDM

MACRO ch4_delayed_start_test
    xor a
    ld [APU_RESULT], a
    apu_power_cycle
    ld a, $FF
    ldh [rNR51], a         ; both channel-4 outputs panned on
    ld a, $77
    ldh [rNR50], a         ; full volume, VIN off

    ch4_phase_subtest 0, CH4_BIT_PHASE0
    ch4_phase_subtest 1, CH4_BIT_PHASE1
    ch4_phase_subtest 2, CH4_BIT_PHASE2
    ch4_phase_subtest 3, CH4_BIT_PHASE3
    ch4_retrigger_subtest CH4_BIT_RETRIG

    ld a, [APU_RESULT]
    cp CH4_BIT_ALL
    jr nz, .failed\@
    test_success
.failed\@:
    test_failure
ENDM

ENDC

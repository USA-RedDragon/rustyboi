; apu_nrx2_zombie.inc — shared body for the NRx2 "zombie mode" revision ROMs.
;
; An NRx2 write to a channel that is already playing does not simply reload the
; envelope: it drags the CURRENT volume through a bit-twiddling transform
; ("zombie mode"). How many times that transform runs is a silicon-revision
; fork, and that fork is what these ROMs pin.
;
; SameBoy (Core/apu.c, MIT) applies the transform TWICE on `model <=
; GB_MODEL_CGB_C` -- once from the old NRx2 to $FF, then from $FF to the new
; value -- and ONCE on everything newer:
;
;     if (gb->model <= GB_MODEL_CGB_C) {
;         _nrx2_glitch(volume, 0xFF, old_value, ...);
;         _nrx2_glitch(volume, value, 0xFF, ...);
;     }
;     else {
;         _nrx2_glitch(volume, value, old_value, ...);
;     }
;
; Core/model.h orders the models DMG_B=$002 ... CGB_C=$203, CGB_D=$204,
; CGB_E=$205, AGB_A=$207, so the DOUBLE application covers every DMG-class
; model AND CGB revisions 0/A/B/C, while CGB-D/E and AGB take the single one.
;
; Observable: PCM12 (Pan Docs, "Audio Registers"; $FF76, CGB only) reports each
; square channel's CURRENT digital output -- the envelope volume while the duty
; step is high, 0 while it is low. So this is a CGB-only probe by construction:
; on DMG the transform is real but has no register-visible tap at all, and the
; DMG side of the fork is therefore queued for the hardware bench rather than
; tested here.
;
; Read strategy. Both channels run at frequency $700, so one duty step lasts
; 2048-$700 = 256 M-cycles and one full 8-step duty period lasts 2048. The
; sampler takes 256 PCM12 readings at ~22 M-cycles each (~5600 M-cycles, ~2.7
; full periods) and keeps the per-nibble MAXIMUM, which is therefore exactly
; the envelope volume regardless of the duty phase the trigger left behind.
; Both NRx2 values used here have envelope period 0, so the envelope never
; ticks and the sampled volume cannot drift during the window -- the reading is
; a plateau, not a race. Subtests 1-2 assert the pre-write volumes for exactly
; that reason: they prove the sampler reads a known value before any zombie
; write is in play, so a subtest 3-4 failure can only mean the transform.
;
; Derivation of the four graded volumes (transform per SameBoy's _nrx2_glitch;
; at trigger the envelope is idle -- volume = NRx2>>4, env_clock and env_locked
; both clear -- because both trigger values carry period 0):
;
;   CH1  trigger NR12 = $A8 (volume 10, direction up, period 0) -> volume 10
;        zombie  NR12 = $10 (volume  1, direction down, period 0; $10 & $F8 != 0
;                            so the DAC stays on and the transform really runs)
;     single (CGB-D/E, AGB):  direction bit flips 1->0, so volume = $10 - 10 = 6
;     double (DMG, CGB<=C):   old->$FF ticks up   10 -> 11
;                             $FF->new inverts    $10 - 11 = 5
;
;   CH2  trigger NR22 = $98 (volume 9, direction up, period 0) -> volume 9
;        zombie  NR22 = $10 (as above)
;     single (CGB-D/E, AGB):  $10 - 9 = 7
;     double (DMG, CGB<=C):   9 -> 10, then $10 - 10 = 6
;
; The two models therefore differ on BOTH channels (6/7 single vs 5/6 double),
; and every graded value is non-zero and distinct from its own trigger volume,
; so neither a silent channel nor a stale reading can masquerade as a pass.

IF !DEF(APU_NRX2_ZOMBIE_INC)
DEF APU_NRX2_ZOMBIE_INC EQU 1

DEF ZOM_DUTY      EQU $80   ; NRx1: duty 2 (50%), length unused (never enabled)
DEF ZOM_FREQ_LO   EQU $00   ; NRx3: frequency $700 -> 256 M-cycles per duty step
DEF ZOM_TRIG_HI   EQU $87   ; NRx4: trigger, length disable, frequency high = 7

DEF ZOM_CH1_TRIG  EQU $A8   ; volume 10, direction up, period 0
DEF ZOM_CH2_TRIG  EQU $98   ; volume  9, direction up, period 0
DEF ZOM_WRITE     EQU $10   ; the zombie write, identical on both channels

DEF ZOM_CH1_VOL0  EQU 10    ; pre-write volumes (ZOM_Cx_TRIG >> 4)
DEF ZOM_CH2_VOL0  EQU 9

DEF ZOM_SAMPLES   EQU 256   ; PCM12 readings per measurement (~2.7 duty periods)

DEF ZOM_BIT_PRE1  EQU $01   ; subtest bits in APU_RESULT
DEF ZOM_BIT_PRE2  EQU $02
DEF ZOM_BIT_POST1 EQU $04
DEF ZOM_BIT_POST2 EQU $08
DEF ZOM_ALL       EQU $0F

; Trigger both squares with a known volume, grade the pre-write reading, apply
; the zombie write to both, then grade the transformed reading.
; \1 = expected CH1 volume, \2 = expected CH2 volume (both model-dependent).
MACRO nrx2_zombie_test
    apu_power_cycle
    xor a
    ld [APU_RESULT], a

    ; PCM12 taps the pre-mix digital output, so neither register below can
    ; change a reading; they are set so the ROM is also audible on real
    ; hardware, where these tests are meant to be re-run by ear/scope.
    ld a, $77
    ldh [rNR50], a
    ld a, $FF
    ldh [rNR51], a

    xor a
    ldh [rNR10], a            ; CH1 sweep off: no frequency drift, no self-disable
    ld a, ZOM_DUTY
    ldh [rNR11], a
    ld a, ZOM_CH1_TRIG
    ldh [rNR12], a
    ld a, ZOM_FREQ_LO
    ldh [rNR13], a
    ld a, ZOM_TRIG_HI
    ldh [rNR14], a            ; CH1 playing

    ld a, ZOM_DUTY
    ldh [rNR21], a
    ld a, ZOM_CH2_TRIG
    ldh [rNR22], a
    ld a, ZOM_FREQ_LO
    ldh [rNR23], a
    ld a, ZOM_TRIG_HI
    ldh [rNR24], a            ; CH2 playing

    ; Subtests 1-2: the sampler returns the triggered volumes untouched.
    call ZomPcmMax            ; D = CH1 volume, E = CH2 volume
    ld a, d
    xor ZOM_CH1_VOL0
    apu_record ZOM_BIT_PRE1
    ld a, e
    xor ZOM_CH2_VOL0
    apu_record ZOM_BIT_PRE2

    ; The zombie writes: NRx2 rewritten while both channels are active.
    ld a, ZOM_WRITE
    ldh [rNR12], a
    ld a, ZOM_WRITE
    ldh [rNR22], a

    ; Subtests 3-4: the transformed volumes -- the revision fork itself.
    call ZomPcmMax
    ld a, d
    xor (\1)
    apu_record ZOM_BIT_POST1
    ld a, e
    xor (\2)
    apu_record ZOM_BIT_POST2

    ld a, [APU_RESULT]
    cp ZOM_ALL
    jr nz, .zomfail\@
    test_success
.zomfail\@:
    test_failure
ENDM

SECTION "apu_nrx2_zombie_helpers", ROM0

; Sample PCM12 ZOM_SAMPLES times and return the per-nibble maximum:
; D = CH1 (low nibble) volume, E = CH2 (high nibble) volume. Spans several full
; duty periods, so the maximum is the envelope volume rather than whatever the
; duty step happened to be at any single read. Clobbers A, B, C.
ZomPcmMax:
    ld d, 0
    ld e, 0
    ld b, ZOM_SAMPLES % 256    ; 0 = 256 iterations
.loop:
    ldh a, [rPCM12]
    ld c, a
    and $0F                    ; CH1 nibble
    cp d
    jr c, .keep_ch1
    ld d, a
.keep_ch1:
    ld a, c
    swap a
    and $0F                    ; CH2 nibble
    cp e
    jr c, .keep_ch2
    ld e, a
.keep_ch2:
    dec b
    jr nz, .loop
    ret

ENDC

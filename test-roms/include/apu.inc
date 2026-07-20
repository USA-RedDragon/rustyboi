; apu.inc — APU register definitions and helpers shared by the ROMs under
; src/apu/. Register map from Pan Docs ("Audio Registers"): NR10-NR52 at
; $FF10-$FF26, wave RAM at $FF30-$FF3F.
;
; Timing facts the helpers encode, all from Pan Docs ("Audio details"):
;
;   * DIV-APU: "A 'DIV-APU' counter is increased every time DIV's bit 4 (5 in
;     double-speed mode) goes from 1 to 0, therefore at a frequency of 512 Hz".
;     "Sound length" is clocked every 2 DIV-APU ticks (256 Hz), so one length
;     tick spans 16384 T-cycles = 4096 M-cycles at single speed.
;   * Writing any value to DIV clears it, so a DIV write pins the DIV-APU phase:
;     the next DIV-APU tick lands 8192 T-cycles (2048 M-cycles) later.
;   * `apu_power_cycle` resets DIV while the APU is off, then powers the APU on
;     within a few M-cycles — while DIV bit 4 is still low. Powering on resets
;     the DIV-APU step, so the next tick is the step sequence's first length
;     step, and the power-on happens on a low DIV-APU bit (no model straddles a
;     falling edge there). All src/apu/ cycle counting anchors t=0 at the DIV
;     reset inside this macro.
;
; Length-counter power-off/power-on retention is deliberately NOT relied on
; anywhere here: every ROM must write NRx1 after `apu_power_cycle` before using
; a counter value.

IF !DEF(APU_INC)
DEF APU_INC EQU 1

DEF rNR10 EQU $FF10   ; CH1 sweep
DEF rNR11 EQU $FF11   ; CH1 length timer / duty
DEF rNR12 EQU $FF12   ; CH1 volume / envelope
DEF rNR13 EQU $FF13   ; CH1 period low
DEF rNR14 EQU $FF14   ; CH1 period high / control
DEF rNR21 EQU $FF16   ; CH2 length timer / duty
DEF rNR22 EQU $FF17   ; CH2 volume / envelope
DEF rNR23 EQU $FF18   ; CH2 period low
DEF rNR24 EQU $FF19   ; CH2 period high / control
DEF rNR30 EQU $FF1A   ; CH3 DAC enable
DEF rNR31 EQU $FF1B   ; CH3 length timer
DEF rNR32 EQU $FF1C   ; CH3 output level
DEF rNR33 EQU $FF1D   ; CH3 period low
DEF rNR34 EQU $FF1E   ; CH3 period high / control
DEF rNR41 EQU $FF20   ; CH4 length timer
DEF rNR42 EQU $FF21   ; CH4 volume / envelope
DEF rNR43 EQU $FF22   ; CH4 frequency / randomness
DEF rNR44 EQU $FF23   ; CH4 control
DEF rNR50 EQU $FF24   ; master volume / VIN panning
DEF rNR51 EQU $FF25   ; sound panning
DEF rNR52 EQU $FF26   ; audio master control
DEF rWAVE EQU $FF30   ; wave RAM base ($FF30-$FF3F)

; CGB-only digital-output taps (Pan Docs, "Audio Registers"): PCM12 reports
; CH1 in the low nibble and CH2 in the high nibble, PCM34 the same for CH3/CH4.
; Each nibble is the channel's CURRENT digital output -- the envelope volume
; while the duty step is high, 0 while it is low -- so a reader that wants the
; volume itself must sample across a full duty period and take the maximum.
DEF rPCM12 EQU $FF76
DEF rPCM34 EQU $FF77

; NR52 bits.
DEF AUDENA_ON  EQU $80
DEF AUDENA_CH1 EQU $01
DEF AUDENA_CH2 EQU $02
DEF AUDENA_CH3 EQU $04
DEF AUDENA_CH4 EQU $08

; NRx4 bits.
DEF AUDHIGH_TRIGGER EQU $80
DEF AUDHIGH_LEN_ON  EQU $40

; One length tick in single-speed M-cycles (16384 T-cycles at 256 Hz).
DEF APU_LEN_TICK_MCYCLES EQU 4096

; Scratch: the per-subtest pass bitmask (sgb_packet.inc's SGB_RESULT pattern).
DEF APU_RESULT EQU $C0F2

; Power-cycle the APU with a pinned DIV-APU phase. t=0 (the timebase every
; caller counts M-cycles from) is the end of the DIV reset; the APU comes on
; ~5 M-cycles later, well inside the low half of DIV bit 4. From t=0 the
; DIV-APU ticks at t = 2048, 4096, 6144, ... M-cycles, and the ticks at
; t = 2048 + k*4096 clock the length counters. Ends with A = AUDENA_ON.
MACRO apu_power_cycle
    xor a
    ldh [rNR52], a         ; APU off: sequencer stopped, registers cleared
    ldh [rDIV], a          ; DIV = 0 -> t = 0
    ld a, AUDENA_ON
    ldh [rNR52], a         ; APU on at t ~= 5: next DIV-APU step clocks length
ENDM

; Burn exactly \1 M-cycles (constant, >= 19), including all macro overhead.
; Clobbers A, BC.
MACRO apu_delay_mcycles
    ASSERT (\1) >= 19
    ld bc, ((\1) - 12) / 7
    call ApuDelayBC
    REPT ((\1) - 12) % 7
    nop
    ENDR
ENDM

; Record a subtest verdict: A = 0 (pass) / non-zero (fail); \1 = its bit in
; APU_RESULT. A failing subtest leaves its bit clear and the ROM keeps running,
; so APU_RESULT names exactly which subtests failed.
MACRO apu_record   ; \1 = bit mask
    or a
    jr nz, .recorded\@
    ld a, [APU_RESULT]
    or \1
    ld [APU_RESULT], a
.recorded\@:
ENDM

SECTION "apu_helpers", ROM0

; Burn 7*BC + 3 M-cycles including the ret (apu_delay_mcycles wraps this).
; Clobbers A, BC.
ApuDelayBC:
.spin:
    dec bc                 ; 2
    ld a, b                ; 1
    or c                   ; 1
    jr nz, .spin           ; 3 taken / 2 fall-through
    ret                    ; 4

; Poll NR52 until the channel bit(s) in C read clear, bounded by DE iterations.
; The loop body is exactly 15 M-cycles, so the return value measures the time
; until the channel shut off: HL = iterations that still saw the bit set
; (HL ~= elapsed M-cycles / 15, 0 if the bit was already clear). On timeout
; HL = the DE bound, which no in-window expectation can equal.
; Clobbers A, DE, HL.
ApuPollChannelOff:
    ld hl, 0
.poll:
    ldh a, [rNR52]         ; 3
    and c                  ; 1
    ret z                  ; 2 not taken
    inc hl                 ; 2
    dec de                 ; 2
    ld a, d                ; 1
    or e                   ; 1
    jr nz, .poll           ; 3
    ret                    ; timeout: HL = bound

ENDC

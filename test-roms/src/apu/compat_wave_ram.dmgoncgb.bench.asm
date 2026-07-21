; compat_wave_ram.dmgoncgb.bench — T13. Does KEY0 / DMG-compatibility mode on
; CGB silicon change the APU, or does the sound die ignore the compat bit?
;
; THIS ROM DOES NOT GRADE ANYTHING. It records raw bytes to cart SRAM. The
; answer it is asking about is not silicon-verified, so asserting an expected
; value here would freeze an unverified inference into a permanent oracle —
; exactly what test-roms/README.md's provenance rule forbids. See "Readout" and
; "Operator protocol" below.
;
; ---------------------------------------------------------------------------
; The question
; ---------------------------------------------------------------------------
;
; Pan Docs, "Audio Registers" / "Audio details" (wave RAM):
;
;     "On monochrome consoles, wave RAM can only be accessed on the same cycle
;      that CH3 does; otherwise, reads return $FF and writes have no effect.
;      On the CGB, the read/write is instead redirected to the byte CH3 is
;      currently reading."
;
; So the two rule sets differ only while CH3 is enabled and playing:
;
;   DMG rules — access outside CH3's own fetch cycle: read $FF, write dropped.
;   CGB rules — access at ANY time: redirected to the byte CH3 is reading.
;
; A CGB running a DMG-header cart is in DMG-compatibility mode. Which rule set
; applies there is UNVERIFIED. rustyboi currently gates the CH3 wave-RAM quirk
; on SILICON (`cgb`), not on cart mode — see the `cgb` argument threaded into
; `audio.sync_cc` in rustyboi-core/src/memory/mmio.rs — so our model predicts
; CGB rules in compat mode. That choice rests on two things, neither of them a
; measurement:
;
;   * SameBoy Core/apu.c gates the same behaviour on the silicon predicate
;     `GB_is_cgb` (12 uses) and never on the cart-mode predicate
;     `GB_is_cgb_in_cgb_mode` (0 uses); and
;   * first principles: the sound die has no CGB/DMG mode bit to see.
;
; Instrumenting all four mode-gated sites across the entire 4,950-row suite
; corpus found ZERO accesses whose outcome differs between the two rules. Only
; two rows even qualify (SameSuite channel_3_wave_ram_dac_on_rw.gb and
; channel_3_wave_ram_locked_write.gb — DMG-header ROMs run on CGB hardware) and
; neither ever reaches a divergent access. No published ROM can settle this;
; that is why this one exists.
;
; ---------------------------------------------------------------------------
; Fetch cadence (derived from Pan Docs, "Audio details" — CH3)
; ---------------------------------------------------------------------------
;
; CH3's frequency timer produces one 4-bit SAMPLE every
;
;     (2048 - freq) * 2 T-cycles
;
; and the waveform is 32 samples. Wave RAM holds those 32 samples as 16 BYTES,
; two samples per byte (high nibble first), and the hardware fetches a byte once
; per two samples, so a wave-RAM BYTE FETCH happens every
;
;     (2048 - freq) * 4 T-cycles  =  (2048 - freq) M-cycles
;
; — the byte-fetch period is an exact whole number of M-cycles for every
; frequency, which matters twice below. The four cadences this ROM uses:
;
;   freq $000  fetch every 2048 M-cycles   (full waveform 131072 T ~ 32 Hz)
;   freq $700  fetch every  256 M-cycles
;   freq $7FF  fetch every    1 M-cycle
;   CH3 off    no fetches at all
;
; ---------------------------------------------------------------------------
; Measurement
; ---------------------------------------------------------------------------
;
; One measurement = one phase of one repeat of one block:
;
;   1. APU off, refill all 16 wave bytes with the pattern  byte[i] = (i<<4)|1
;      ($01,$11,$21,...,$F1). Every byte is distinct, its HIGH NIBBLE IS ITS OWN
;      INDEX, and none of them is $FF — so a read of $FF is unambiguously the
;      DMG "not accessible" answer and can never be a pattern byte.
;   2. APU on, CH3 DAC on, output level 100%, length disabled (so the channel
;      never stops), frequency per the block, trigger via NR34 bit 7.
;   3. Busy-wait a per-phase number of M-cycles.
;   4. `ldh a, [$FF30]`   — the READ
;      `ldh [$FF3F], a`   — the WRITE, sentinel $A5 (low nibble 5, so it can
;                           never be confused with a pattern byte)
;   5. APU off — wave RAM is freely accessible again — and dump all 16 bytes.
;
;   CGB rules  -> the read is the byte CH3 was fetching, whose high nibble names
;                 the index; the dump shows $A5 at that same index.
;   DMG rules  -> the read is $FF and the dump is the untouched pattern.
;
; Blocks (each repeated 3 times so an operator can see instability on real
; silicon; phases sweep the busy-wait across a full byte-fetch period):
;
;   $01 IDLE  CH3 never triggered.        1 phase.
;             Both rule sets agree: wave RAM is ordinary RAM. This is the
;             harness control — it proves the pattern fill, the read path, the
;             write path and the dump path all work. If IDLE does not read $01
;             and show $A5 at index 15, nothing else in the record means
;             anything.
;
;   $02 FAST  freq $7FF, fetch every 1 M-cycle. 4 phases.
;             The in-window control. At this cadence no CPU access can be more
;             than one M-cycle from a fetch, so both rule sets predict a
;             redirected access. Note the honest limit: because every fetch
;             period is a whole number of M-cycles (see above), the CPU cannot
;             steer the SUB-M-cycle offset between its own bus access and the
;             fetch — that offset is fixed silicon geometry. If FAST reads $FF
;             on hardware, that is not a broken ROM, it is a measurement of that
;             geometry, and it is recorded rather than judged. The 4 phases are
;             a stability check, not a sweep: phase is degenerate at $7FF.
;
;   $03 MID   freq $700, fetch every 256 M-cycles. 8 phases, 42 M-cycles apart,
;             spanning 294 M-cycles — a full period plus margin.
;
;   $04 SLOW  freq $000, fetch every 2048 M-cycles. 16 phases, 140 M-cycles
;             apart, spanning 2100 M-cycles — a full period plus margin.
;
; MID and SLOW are the discriminators. The sweep is what makes them sound: it
; covers the WHOLE fetch period, so the result does not depend on trusting any
; phase arithmetic. Under DMG rules at most one phase per sweep could land in
; the (1-2 T-cycle) window and the rest must read $FF; under CGB rules every
; phase must read a pattern byte. A single unlucky alignment cannot manufacture
; either answer.
;
; For the record, the busy-wait for a phase whose loop count is BC ends
; 7*BC + 22 M-cycles after the NR34 trigger write (ApuDelayBC is 7*BC + 3, plus
; 10 M-cycles reloading BC from WRAM, 6 for the call and 3 for the read). This
; constant is documented for completeness only — nothing in the design depends
; on it, precisely because each block sweeps its entire period.
;
; Designed-around hazard: on DMG, triggering CH3 while it is already playing
; corrupts wave RAM. Every measurement powers the APU fully off and refills the
; pattern before it triggers, so CH3 is always idle at the trigger.
;
; ---------------------------------------------------------------------------
; Readout — payload format at $A020 (header format in include/rbhw_capture.inc)
; ---------------------------------------------------------------------------
;
; Four blocks, in the order IDLE, FAST, MID, SLOW. Each block is:
;
;   8-byte block header:
;     +0 block id   +1 freq low   +2 freq high   +3 phase count
;     +4 delay base (word, LE)    +6 delay step   +7 CH3 enabled (0/1)
;   then 3 repeats, each of <phase count> 17-byte records, in phase order:
;     +0    the byte read from $FF30
;     +1..  all 16 wave bytes, dumped after the APU was powered off
;
; Payload length 1511 bytes; total record 1543 bytes, well inside the 8 KiB.
;
; ---------------------------------------------------------------------------
; Operator protocol
; ---------------------------------------------------------------------------
;
;   Console: any CGB revision. Note the revision on the unit passport — the
;   fingerprint vector in the record header stores the raw boot A/B handoff and
;   the CGB-only register reads, but it does not resolve CPU-CGB-C from -E, so
;   the passport is the authority. This is NOT caution-class; a primary unit is
;   fine. Running it on a DMG as well is useful: a DMG must show pure DMG rules
;   and so calibrates the readout.
;
;   Cart: any MBC5 + RAM + battery cart. Flash the .gb as-is — do NOT re-fix the
;   header with the CGB flag, the DMG header IS the experiment.
;
;   Run: power on, wait for the screen to turn BLACK (the completion signal —
;   the whole capture takes well under a second), power off, read the save back
;   with a GBxCart RW. Repeat the whole power-cycle at least three times; the
;   run counter at $A006 must advance 1,2,3 (if it stays at 1 the cart battery
;   is dead and the capture is worthless). Verify the CRC16 at $A012 before
;   believing any byte.
;
;   Interpretation, per phase of the MID and SLOW blocks:
;     read byte $FF and the dump still all-pattern
;         -> DMG rules apply in compat mode. rustyboi's silicon gate is WRONG
;            and the wave-RAM quirk must be gated on cart mode instead.
;     read byte = a pattern byte and $A5 present in the dump at the index named
;     by that byte's high nibble
;         -> CGB rules apply in compat mode. rustyboi's silicon gate is right.
;     anything else (mixed across phases, $A5 at an unrelated index, unstable
;     across the 3 repeats)
;         -> do not average it, report it. A partial window is a new finding.
;
;   The IDLE and FAST blocks are read FIRST. If IDLE is wrong the run is void.
;
; ---------------------------------------------------------------------------
; Wiring
; ---------------------------------------------------------------------------
;
; Deliberately NOT a row in rustyboi-test-runner/suites/rustyboi.manifest. The
; manifest generator emits one graded row per ROM found in test-roms/build/, and
; every grading it offers asserts an expected value — which is the one thing
; this ROM must not do. So the Makefile routes `.bench.` ROMs to
; test-roms/build-bench/ instead, outside the generator's scan. Build with
; `make -C test-roms roms`; run it by hand (see test-roms/README.md).

INCLUDE "hardware.inc"
INCLUDE "apu.inc"
INCLUDE "rbhw_capture.inc"

DEF T13_ROM_ID  EQU $13

DEF WAVE_LEN    EQU 16
DEF SENTINEL    EQU $A5
DEF REPEATS     EQU 3
DEF REC_BYTES   EQU 1 + WAVE_LEN
DEF BLK_HDR     EQU 8

DEF IDLE_PHASES EQU 1
DEF FAST_PHASES EQU 4
DEF MID_PHASES  EQU 8
DEF SLOW_PHASES EQU 16
DEF ALL_PHASES  EQU IDLE_PHASES + FAST_PHASES + MID_PHASES + SLOW_PHASES
DEF BLOCKS      EQU 4

DEF PAYLEN      EQU BLOCKS * BLK_HDR + REPEATS * REC_BYTES * ALL_PHASES
ASSERT PAYLEN == 1511
ASSERT RBHW_PAYLOAD + PAYLEN <= $2000      ; fits the 8 KiB cart RAM bank

; Working state. The measurement path keeps everything in WRAM rather than in
; registers so the timed section is a fixed instruction sequence.
SECTION "t13_state", WRAM0[$C000]
Cursor:      ds 2       ; next SRAM payload address
Delay:       ds 2       ; ApuDelayBC loop count for the current phase
PhaseIndex:  ds 1
RepeatIndex: ds 1
; The block header, loaded verbatim from BlockTable and also copied to SRAM.
BlkId:       ds 1
BlkFreqLo:   ds 1
BlkFreqHi:   ds 1
BlkPhases:   ds 1
BlkBase:     ds 2
BlkStep:     ds 1
BlkCh3On:    ds 1

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "t13_main", ROM0[$150]
Start:
    rbhw_boot_capture
    ld sp, $CFFF

    ld a, T13_ROM_ID
    ld de, PAYLEN
    call RbhwBeginRecord

    ld hl, RBHW_SRAM + RBHW_PAYLOAD
    call StoreCursor

    ld hl, BlockTable
    ld b, BLOCKS
.blocks:
    push bc
    call RunBlock
    pop bc
    dec b
    jr nz, .blocks

    ld de, PAYLEN
    call RbhwFinish

    ; Completion signal for the operator: the screen goes black.
    ld a, $FF
    ldh [rBGP], a
.spin:
    jr .spin

; Block table, in the exact byte order the block header takes in the payload.
BlockTable:
    ;  id  freqlo freqhi phases  base(LE)  step  ch3on
    db $01, $00,  $00,  IDLE_PHASES, LOW(100), HIGH(100),  0, 0   ; IDLE
    db $02, $FF,  $07,  FAST_PHASES, LOW(100), HIGH(100),  1, 1   ; FAST  $7FF
    db $03, $00,  $07,  MID_PHASES,  LOW(100), HIGH(100),  6, 1   ; MID   $700
    db $04, $00,  $00,  SLOW_PHASES, LOW(100), HIGH(100), 20, 1   ; SLOW  $000

; Run one block. HL = its BlockTable entry; returns HL past the entry.
RunBlock:
    push hl
    call LoadCursor                     ; DE = cursor
    ld c, BLK_HDR
.toSram:
    ld a, [hl+]
    ld [de], a
    inc de
    dec c
    jr nz, .toSram
    ld h, d
    ld l, e
    call StoreCursor
    pop hl
    push hl
    ld de, BlkId
    ld c, BLK_HDR
.toVars:
    ld a, [hl+]
    ld [de], a
    inc de
    dec c
    jr nz, .toVars
    pop hl
    ld bc, BLK_HDR
    add hl, bc
    push hl
    call RunSweeps
    pop hl
    ret

; Three repeats of the block's phase sweep.
RunSweeps:
    ld a, REPEATS
    ld [RepeatIndex], a
.repeat:
    xor a
    ld [PhaseIndex], a
    ld a, [BlkBase]
    ld [Delay], a
    ld a, [BlkBase + 1]
    ld [Delay + 1], a
.phase:
    call MeasurePhase
    ld a, [BlkStep]
    ld c, a
    ld b, 0
    ld a, [Delay]
    ld l, a
    ld a, [Delay + 1]
    ld h, a
    add hl, bc
    ld a, l
    ld [Delay], a
    ld a, h
    ld [Delay + 1], a
    ld a, [PhaseIndex]
    inc a
    ld [PhaseIndex], a
    ld c, a
    ld a, [BlkPhases]
    cp c
    jr nz, .phase
    ld a, [RepeatIndex]
    dec a
    ld [RepeatIndex], a
    jr nz, .repeat
    ret

; One measurement. Leaves the APU off and appends a 17-byte record.
MeasurePhase:
    xor a
    ldh [rNR52], a                      ; APU off: wave RAM is plain RAM
    call FillWave
    ld a, AUDENA_ON
    ldh [rNR52], a
    ld a, $77
    ldh [rNR50], a
    ld a, $FF
    ldh [rNR51], a
    ld a, [BlkCh3On]
    or a
    jr z, .armed                        ; IDLE block: CH3 stays off entirely
    ld a, $80
    ldh [rNR30], a                      ; CH3 DAC on
    xor a
    ldh [rNR31], a                      ; length 0, but length is disabled below
    ld a, $20
    ldh [rNR32], a                      ; output level 100%
    ld a, [BlkFreqLo]
    ldh [rNR33], a
    ld a, [BlkFreqHi]
    or AUDHIGH_TRIGGER                  ; trigger, length counter disabled
    ldh [rNR34], a
.armed:
    ld a, [Delay]
    ld c, a
    ld a, [Delay + 1]
    ld b, a
    call ApuDelayBC
    ldh a, [rWAVE]                      ; THE READ
    ld b, a
    ld a, SENTINEL
    ldh [rWAVE + 15], a                 ; THE WRITE
    xor a
    ldh [rNR52], a                      ; APU off: wave RAM readable again

    call LoadCursor                     ; DE = cursor
    ld a, b
    ld [de], a
    inc de
    ld hl, rWAVE
    ld c, WAVE_LEN
.dump:
    ld a, [hl+]
    ld [de], a
    inc de
    dec c
    jr nz, .dump
    ld h, d
    ld l, e
    ; fall through to StoreCursor

; HL -> Cursor.
StoreCursor:
    ld a, l
    ld [Cursor], a
    ld a, h
    ld [Cursor + 1], a
    ret

; Cursor -> DE.
LoadCursor:
    ld a, [Cursor]
    ld e, a
    ld a, [Cursor + 1]
    ld d, a
    ret

; byte[i] = (i << 4) | 1. The APU must be off. Clobbers A, BC, HL.
FillWave:
    ld hl, rWAVE
    ld c, WAVE_LEN
    ld b, $01
.fill:
    ld a, b
    ld [hl+], a
    ld a, b
    add $10
    ld b, a
    dec c
    jr nz, .fill
    ret

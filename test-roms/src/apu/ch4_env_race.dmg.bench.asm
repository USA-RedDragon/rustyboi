; ch4_env_race.dmg.bench — T15. On DMG, is channel 4's envelope frame-escape
; race anchored at the NR44 WRITE, or at the delayed start's CROSSING? And how
; wide is the escape window really?
;
; THIS ROM DOES NOT GRADE ANYTHING, and unlike every other ROM in this tree it
; does not even produce a machine-readable result: there is no DMG register that
; exposes a channel's envelope volume, so the measurement is AUDIBLE and an
; operator captures it with a line-out recording. What the ROM writes to cart
; SRAM is its own PARAMETER TABLE — the sweep's base, step, ordering and marker
; spec — so the capture can be aligned to the sweep and so the save is
; self-identifying (silicon fingerprint) like every other RBHW record.
;
; ---------------------------------------------------------------------------
; The question
; ---------------------------------------------------------------------------
;
; Two independent unverified claims meet in this ROM.
;
; (1) THE ANCHOR. rustyboi models a DMG-only deferred noise trigger: an NR44
;     trigger arriving while the ripple counter's phase accumulator satisfies
;     `alignment & 3 != 0` is deferred by 6 APU cycles and the whole start runs
;     at the crossing (rustyboi-core/src/audio/noise.rs). When it re-applies the
;     deferred trigger it anchors the envelope-race timestamp `env_trigger_cc`
;     at the ACTUAL +6 CROSSING, not at the end of the batch that contained the
;     write. Whether the race is anchored at the crossing or at the write is not
;     measured anywhere; it is a first-principles inference ("the start belongs
;     to its crossing").
;
; (2) THE WINDOW WIDTH. The race itself: a trigger that lands close enough
;     before a 64 Hz envelope frame boundary escapes that frame's decrement, so
;     the envelope walk starts one frame late. rustyboi's `env_frame_countdown`
;     (rustyboi-core/src/audio/envelope.rs) uses
;
;         if event_cc.wrapping_sub(self.env_trigger_cc) <= 2 { return; }
;
;     i.e. an escape window exactly 2 APU cycles wide. That constant has NO
;     published source. It is inherited, not derived and not measured.
;
; A sweep of the trigger's position across a 64 Hz frame answers both at once.
;
; ---------------------------------------------------------------------------
; Why this is audible, and what the operator hears
; ---------------------------------------------------------------------------
;
; NR42 = $F1: initial volume 15, direction 0 (decreasing), period 1. So the
; envelope steps on EVERY 64 Hz frame and the volume walks 15 -> 0 in 15 steps,
; after which channel 4 is silent. NR43 = $00 (divisor 0, shift 0) makes it
; broadband noise — the loudest, most obviously-present thing the DMG can emit,
; and its cessation is a sharp, easily-timed edge in a recording.
;
; So the elapsed time from the NR44 write to SILENCE ONSET takes exactly two
; values, ONE 64 Hz FRAME APART:
;
;     1 / 64 s = 15.625 ms.
;
; That is enormous next to any capture jitter, and it is a two-valued readout —
; there is no third answer to confuse it with. Under rustyboi's model the two
; land at roughly 219 ms and 234 ms after the NR44 write; that pair is stated
; only so an operator knows what ballpark to expect, it is NOT the measurement
; and nothing here depends on it. The absolute value also picks up a few ms of
; the DAC's own decay tail, which is why only the STEP between adjacent sweep
; positions is read.
;
; ---------------------------------------------------------------------------
; Anchoring the 64 Hz grid
; ---------------------------------------------------------------------------
;
; Established facts (Pan Docs, "Audio details"; see include/apu.inc, which
; encodes the same arithmetic):
;
;   * the DIV-APU counter advances every time DIV bit 4 falls, 512 Hz;
;   * writing any value to DIV clears it, so a DIV write pins the DIV-APU phase:
;     from t = 0 (the DIV write) the ticks land at t = 2048, 4096, 6144, ...
;     M-cycles;
;   * the envelope is clocked on one DIV-APU step in eight, so envelope events
;     are 8 * 2048 = 16384 M-cycles apart (= 1/64 s, as they must be).
;
; The PERIOD is certain; the PHASE is not. Which of the eight steps the first
; tick after an APU power-on performs is a convention that differs between
; references, and it decides whether the first envelope event after the anchor
; lands at 7 * 2048 = 14336 M-cycles or at 8 * 2048 = 16384. rustyboi puts it at
; 14336. See "Two candidate centres" below: the ROM sweeps both rather than
; betting the whole bench run on our own phase model being right.
;
; Each measurement resets DIV while the APU is off (the `apu_power_cycle`
; pattern, which also resets the DIV-APU step so that the first tick really is
; step 0), powers the APU on, and then busy-waits a CYCLE-COUNTED delay before a
; SINGLE NR44 trigger write. The sweep variable is
;
;     D = M-cycles from the DIV write's access cycle
;         to the NR44 write's access cycle
;
; and the boundary of interest sits at D = 16384.
;
; ---------------------------------------------------------------------------
; Decoupling the two knobs — the reason for the split nop pair
; ---------------------------------------------------------------------------
;
; The APU cycle counter runs at exactly 2x the M-cycle rate and `alignment` is
; zeroed at the APU power-on write, which is itself a CPU write. So at any
; CPU-written NR44 trigger `alignment & 3` is 2 * (A & 1), where
;
;     A = M-cycles from the APU power-on write to the NR44 write,
;
; and can only be 0 or 2 — never 1 or 3. The deferral engages only at 2.
;
; The naive sweep is broken: stepping D by one M-cycle also steps A by one, so
; the alignment would flip on every step and the two effects could never be
; separated. This ROM fixes A's parity independently of D by splitting a single
; `nop` around the APU power-on write:
;
;     ldh [rDIV], a          ; t = 0
;     REPT P     / nop       ; P nops BEFORE the power-on write
;     ld a, AUDENA_ON
;     ldh [rNR52], a         ; alignment = 0 here
;     REPT 1 - P / nop       ; and 1 - P AFTER it
;
; The two REPTs always sum to exactly 1 M-cycle, so D is untouched, while P
; moves the power-on write inside the sequence and therefore flips A's parity.
; With
;
;     P = (D + 1 + ALIGN) & 1
;
; the arithmetic gives A & 1 == ALIGN for every D. Full accounting, from the DIV
; write's access cycle (the third and last M-cycle of `ldh [rDIV], a`) to the
; NR44 write's access cycle (the second and last M-cycle of `ld [hl], a`):
;
;     REPT P / nop        P        ld a, $F1             2
;     ld a, AUDENA_ON     2        ldh [rNR42], a        3
;     ldh [rNR52], a      3        xor a                 1
;     REPT 1-P / nop      1-P      ldh [rNR43], a        3
;     ld a, $FF           2        ld hl, rNR44          3
;     ldh [rNR51], a      3        apu_delay_mcycles     PADM
;     ld a, $77           2        ld a, $80             2
;     ldh [rNR50], a      3        ld [hl], a            2
;
;     D = 32 + PADM,  independent of P; the power-on write sits at 5 + P.
;     A = D - 5 - P,  so A & 1 = (D + 1 + P) & 1 = ALIGN.  QED.
;
; ALIGN = 1 is therefore the DEFERRING series and ALIGN = 0 the control — under
; rustyboi's own accounting; a capture that disagrees about which series defers
; is itself a finding.
;
; ---------------------------------------------------------------------------
; Two candidate centres
; ---------------------------------------------------------------------------
;
; A sweep is worthless if the boundary it is centred on is not inside it, and a
; hardware bench run is far too expensive to spend on a null result caused by
; our own unverified phase convention. So the ROM sweeps BOTH candidate
; positions for the first envelope event after the anchor:
;
;     centre 0   14336 M-cycles = 7 * 2048   (where rustyboi puts it)
;     centre 1   16384 M-cycles = 8 * 2048
;
; Four blocks in all: centre 0 / ALIGN 0, centre 0 / ALIGN 1, centre 1 / ALIGN 0,
; centre 1 / ALIGN 1, in that order. EXACTLY ONE CENTRE SHOULD SHOW AN EDGE. The
; other centre's two blocks sit in the middle of a frame, far from any boundary,
; and must come out UNIFORM — every step the same, no transition. A uniform
; block is not a failure, it is the expected reading for the wrong centre, and
; it doubles as a control: a block that shows an edge where no boundary exists
; would mean something other than the frame race is moving the result.
;
; If BOTH centres come out uniform, the envelope grid is at neither, i.e. the
; frame-sequencer phase is a third thing. Report it and re-run with the base
; shifted; do not try to infer the answer from a null.
;
; ---------------------------------------------------------------------------
; The sweep
; ---------------------------------------------------------------------------
;
; Per block: 40 steps of 1 M-cycle (2 APU cycles), spanning the block's centre
; plus and minus 20 M-cycles. That is ample margin around a window rustyboi
; believes is 2 APU cycles wide and an anchor offset it believes is 6.
;
; What the recording should show, in the blocks of the centre that has the
; boundary: the SHORT silence-onset interval for the early steps, the LONG one
; (15.625 ms more) for the late ones, with ONE transition — the escape edge.
; Then:
;
;   * the ALIGN = 1 edge sits 6 APU cycles (3 sweep steps) EARLIER in D than the
;     ALIGN = 0 edge of the same centre
;         -> the race is anchored at the +6 crossing. rustyboi's model is right.
;   * both edges sit at the SAME D
;         -> the race is anchored at the write (or at the enclosing batch), and
;            rustyboi's crossing anchor is wrong.
;   * some other separation
;         -> the deferral is not 6 APU cycles on this silicon. Report the number.
;
; and independently, in EITHER of the two edge-bearing blocks:
;
;   * the number of consecutive steps that escape measures the ESCAPE WINDOW
;     WIDTH directly. rustyboi's 2-APU-cycle window means at most one step (the
;     sweep samples every 2 APU cycles); a wider window shows as a run of
;     escaping steps and gives the true width in APU cycles as 2 x that run.
;
; Note the honest limit: because CPU writes land only on even APU cycles, this
; sweep cannot resolve the window's odd-cycle edge. It measures the width to
; +/-1 APU cycle. That is still the first measurement of it that exists.
;
; ---------------------------------------------------------------------------
; Markers — how the operator segments the recording
; ---------------------------------------------------------------------------
;
; Every tone below is channel 2, duty 50% (NR21 = $80), NR22 = $F0 (volume 15,
; envelope off, so the amplitude is flat for the whole tone), both terminals
; (NR51 = $FF), master volume 7/7 (NR50 = $77). Tones end by powering the APU
; off, which is a clean hard cut.
;
;   LEAD    500 ms, followed by 500 ms of silence, at the start of each of the
;           four blocks. The only tones longer than 50 ms in the whole run, so
;           they are unmistakable, and their PITCH NAMES THE CENTRE:
;               256 Hz  (period value $600)  centre 0 (14336 M-cycles)
;               128 Hz  (period value $400)  centre 1 (16384 M-cycles)
;           Exactly four LEADs exist, in the order 256, 256, 128, 128 Hz, and
;           the two blocks under each LEAD pitch are ALIGN 0 then ALIGN 1.
;
;   BLIP    50 ms, followed by 50 ms of silence, then the measurement. One BLIP
;           precedes every sweep step, and its PITCH NAMES THE SERIES:
;               512 Hz  (period value $700)  ALIGN = 0
;              1024 Hz  (period value $780)  ALIGN = 1
;           so a mis-segmented recording is self-diagnosing — a series whose
;           blips change pitch partway through has been spliced wrongly.
;
; Exact tone frequency, for the operator's reference: the channel-2 period value
; X gives 131072 / (2048 - X) Hz, so $400 -> 128 Hz, $600 -> 256 Hz,
; $700 -> 512 Hz, $780 -> 1024 Hz, all exact.
;
; Per sweep step the timeline is:
;
;     BLIP 50 ms  |  silence 50 ms  |  APU power cycle, DIV = 0
;                 |  D M-cycles (~15.6 ms)  |  NR44 trigger
;                 |  noise, 234.4 or 250.0 ms  |  silence to 400 ms  |  next
;
; The operator does NOT need absolute timing: the NR44 write is a FIXED offset
; from the end of each BLIP (50 ms of silence plus D, and D varies by at most
; 40 M-cycles ~ 38 us within a block, which is nothing next to 15.6 ms). So
; measure BLIP-END to SILENCE-ONSET for every step and look for the 15.6 ms
; jump. Only the STEP between consecutive sweep positions carries the result;
; the absolute value of the interval does not, and the two centres differ in it
; by 2048 M-cycles (~2 ms) for reasons that have nothing to do with the race.
;
; Beware one artefact: each measurement ends by powering the APU off, which is
; an audible click. It is an impulse, not a tone, and it cannot be confused with
; a 50 ms BLIP — but do not mistake it for the silence onset. Silence onset is
; where the broadband noise stops, which is always EARLIER than that click.
;
; ---------------------------------------------------------------------------
; Readout — payload format at $A020 (header format in include/rbhw_capture.inc)
; ---------------------------------------------------------------------------
;
; A 22-byte parameter table:
;
;   +0  block id ($01)
;   +1  sweep steps per block (40)
;   +2  series (ALIGN values) per centre (2)
;   +3  centres (2)
;   +4  sweep step size in M-cycles (1)
;   +5  reserved (0), so the words below stay even-aligned
;   +6  centre 0 sweep base D, word LE (14316)
;   +8  centre 1 sweep base D, word LE (16364)
;   +10 BLIP length in M-cycles, word LE
;   +12 gap after a BLIP, in M-cycles, word LE
;   +14 measurement window in M-cycles, dword LE
;   +18 ALIGN = 0 BLIP period value, word LE ($0700)
;   +20 ALIGN = 1 BLIP period value, word LE ($0780)
;
; then 160 two-byte step records in emission order — centre 0/ALIGN 0 steps
; 0..39, centre 0/ALIGN 1, centre 1/ALIGN 0, centre 1/ALIGN 1 — each:
;
;   +0 NR52 read immediately after that step's NR44 write
;   +1 NR52 read at the end of that step's measurement window
;
; Payload length 342 bytes; total record 374 bytes.
;
; Those two bytes are the only CPU-visible measured values in the record and
; they are NOT the experiment — the experiment is the recording. They are a
; liveness check:
; NR52 bits 4-6 are unused and read back as 1 (Pan Docs, "Audio Registers"), so
; with only channel 4 triggered both bytes should read $F8 in every step. Bit 3
; stays set at the end of the window because the length counter is disabled and
; the DAC (NR42 upper nibble 15) never switches off — a volume of 0 is silent
; but still "enabled". A step whose bytes are not $F8 means that step did not
; run as designed and its segment of the recording must be discarded. All 320
; step bytes should read $F8.
;
; ---------------------------------------------------------------------------
; Operator protocol
; ---------------------------------------------------------------------------
;
;   Console: DMG. Primary unit DMG-08. Worth repeating on an early DMG and on an
;   MGB for revision spread, since the deferral is a DMG-class behaviour. Note
;   the exact unit on the passport. NOT caution-class.
;
;   Cart: any MBC5 + RAM + battery cart. Flash the .gb as-is.
;
;   Capture rig: line out from the headphone jack into an audio interface at
;   44.1 kHz or better, 16-bit or better, recording MONO or either channel (the
;   ROM pans everything to both terminals, so the two channels are identical and
;   either will do). Set the console volume wheel high enough that the noise is
;   well above the noise floor but NOT clipping — clipping smears the silence
;   onset, which is the one edge that has to stay sharp. Do a test run and check
;   the waveform before trusting a capture.
;
;   Run: power on with the recorder already rolling. The screen turns BLACK when
;   the whole sweep is finished — total run time is about 90 seconds. Stop the
;   recording, power off, and read the save back with a GBxCart RW; check the
;   run counter at $A006 and the CRC16 at $A012 exactly as for any other RBHW
;   record, and check that all 320 step bytes read $F8.
;
;   Analysis: locate the four LEAD tones and note their pitches — the two 256 Hz
;   LEADs open the centre-0 blocks, the two 128 Hz LEADs the centre-1 blocks,
;   and within each pair the first block is ALIGN 0 and the second ALIGN 1.
;   Within each block, for each of the 40 BLIPs, measure the interval from BLIP
;   END to the point where the broadband noise stops, giving 40 intervals per
;   block. Exactly one centre's two blocks should each show a single step of
;   about 15.6 ms — the escape edge — and the other centre's two blocks should
;   be uniform. Discard the uniform centre and compare the two edge positions,
;   in sweep-step index:
;
;     edge(ALIGN=1) = edge(ALIGN=0) - 3 steps
;         -> the envelope race is anchored at the +6 APU-cycle crossing.
;            rustyboi's model is right.
;     edge(ALIGN=1) = edge(ALIGN=0)
;         -> the race is anchored at the write, not the crossing. rustyboi's
;            crossing anchor is wrong and must be moved.
;     any other offset N
;         -> the deferral is 2N APU cycles, not 6. Report N.
;
;   And, in each series independently, count the consecutive steps on the LATE
;   (250.0 ms) side of the edge that are adjacent to it — the escape window is
;   twice that count, in APU cycles. rustyboi assumes 2.
;
;   If a block shows TWO steps, if BOTH centres show edges, or if NEITHER does,
;   do not average or guess: report the raw table. Two steps would mean a second
;   effect is in play at this frame boundary; two centres with edges would mean
;   the envelope grid is denser than 64 Hz; neither means the grid sits at a
;   phase this ROM did not anticipate. Each of those retires the design as it
;   stands and is worth more than a forced reading.
;
; ---------------------------------------------------------------------------
; Wiring
; ---------------------------------------------------------------------------
;
; Deliberately NOT a row in rustyboi-test-runner/suites/rustyboi.manifest — the
; manifest generator emits one GRADED row per ROM found in test-roms/build/, and
; every grading it offers asserts an expected value, which is the one thing this
; ROM must not do (and in this case could not do: the result is not visible to
; the CPU at all). The Makefile routes `.bench.` ROMs to test-roms/build-bench/
; instead, outside the generator's scan. Build with `make -C test-roms roms`;
; run it by hand (see test-roms/README.md).
;
; As a second line of defence the ROM ends with `ld b, b` while every register
; holds $FF — deliberately NOT the Fibonacci handoff the `rustyboi`/mooneye
; grading looks for — so if it is ever wired into the graded suite by accident
; it fails loudly instead of passing silently.
;
; `halt` appears nowhere in this ROM. rustyboi's HALT batcher can resume the CPU
; off the M-cycle grid, which would put `alignment & 3` at a value real silicon
; cannot produce; every delay here is a cycle-counted busy-wait instead.

INCLUDE "hardware.inc"
INCLUDE "apu.inc"
INCLUDE "rbhw_capture.inc"

DEF T15_ROM_ID   EQU $15

; --- sweep ------------------------------------------------------------------
; The first 64 Hz envelope event after a DIV reset (see the header): DIV-APU
; tick 8, at 8 * 2048 M-cycles.
; The DIV-APU tick period; ticks land at 2048, 4096, ... M-cycles after a DIV
; reset taken while the APU is off.
DEF DIVAPU_MC    EQU 2048

; The two candidate positions for the first 64 Hz envelope event after the
; anchor, swept independently — see "Two candidate centres" in the header.
DEF CENTRES      EQU 2
DEF CENTRE_0_MC  EQU 7 * DIVAPU_MC
DEF CENTRE_1_MC  EQU 8 * DIVAPU_MC
ASSERT CENTRE_0_MC == 14336
ASSERT CENTRE_1_MC == 16384

DEF SWEEP_STEPS  EQU 40
DEF SWEEP_STEP   EQU 1
DEF SWEEP_HALF   EQU SWEEP_STEPS / 2
DEF SWEEP_BASE_0 EQU CENTRE_0_MC - SWEEP_HALF * SWEEP_STEP
DEF SWEEP_BASE_1 EQU CENTRE_1_MC - SWEEP_HALF * SWEEP_STEP
DEF SERIES       EQU 2

; --- markers ----------------------------------------------------------------
; Channel-2 period value X plays 131072 / (2048 - X) Hz.
DEF TONE_128HZ   EQU $0400
DEF TONE_256HZ   EQU $0600
DEF TONE_512HZ   EQU $0700
DEF TONE_1024HZ  EQU $0780

; 1 M-cycle is 1 / 1048576 s.
DEF MC_PER_MS    EQU 1048576 / 1000

DEF LEAD_MC      EQU 500 * MC_PER_MS
DEF BLIP_MC      EQU 50 * MC_PER_MS
DEF GAP_MC       EQU 50 * MC_PER_MS
; 400 ms: comfortably past the 250.0 ms worst case plus the ~15.6 ms pre-trigger
; delay, with room for the silence onset to be unambiguous.
DEF WINDOW_MC    EQU 400 * MC_PER_MS

; --- payload ----------------------------------------------------------------
DEF PARAM_BYTES  EQU 22
DEF STEP_BYTES   EQU 2
DEF PAYLEN       EQU PARAM_BYTES + CENTRES * SERIES * SWEEP_STEPS * STEP_BYTES
ASSERT PAYLEN == 342
ASSERT RBHW_PAYLOAD + PAYLEN <= $2000   ; fits the 8 KiB cart RAM bank

; Burn exactly \1 M-cycles for any constant >= 19. `apu_delay_mcycles` alone
; tops out where ApuDelayBC's loop counter overflows BC (7 * $FFFF + 12), which
; the ~500 ms waits here exceed; splitting into equal parts is exact because
; every part is itself exact. Clobbers A, BC.
MACRO t15_wait
    IF (\1) > 400000
    apu_delay_mcycles ((\1) / 4)
    apu_delay_mcycles ((\1) / 4)
    apu_delay_mcycles ((\1) / 4)
    apu_delay_mcycles ((\1) - 3 * ((\1) / 4))
    ELIF (\1) > 200000
    apu_delay_mcycles ((\1) / 2)
    apu_delay_mcycles ((\1) - (\1) / 2)
    ELSE
    apu_delay_mcycles (\1)
    ENDC
ENDM

; Play a flat channel-2 tone of \1 M-cycles at period value \2, then power the
; APU off (a hard cut) and hold silence for \3 M-cycles. Clobbers A, BC.
MACRO t15_tone
    xor a
    ldh [rNR52], a
    ld a, AUDENA_ON
    ldh [rNR52], a
    ld a, $FF
    ldh [rNR51], a
    ld a, $77
    ldh [rNR50], a
    ld a, $80                           ; duty 50%, length timer unused
    ldh [rNR21], a
    ld a, $F0                           ; volume 15, envelope off: flat tone
    ldh [rNR22], a
    ld a, LOW(\2)
    ldh [rNR23], a
    ld a, HIGH(\2) | AUDHIGH_TRIGGER    ; length counter disabled
    ldh [rNR24], a
    t15_wait \1
    xor a
    ldh [rNR52], a                      ; APU off: tone stops
    t15_wait \3
ENDM

; One sweep step. DE = the SRAM cursor; appends two NR52 reads and advances it.
;   \1 = ALIGN (0 -> alignment&3 == 0, 1 -> alignment&3 == 2, i.e. deferring)
;   \2 = D, M-cycles from the DIV reset to the NR44 trigger write
;
; The split nop pair around the APU power-on write holds A's parity at \1 while
; leaving D untouched; see "Decoupling the two knobs" in the header.
MACRO t15_step
    ; Pitch names the series: 512 Hz for ALIGN 0, 1024 Hz for ALIGN 1. The blip
    ; and the post-measurement wait are identical for every step and are called
    ; rather than inlined — 160 inlined copies do not fit in ROM0. Neither is
    ; timing-critical: the DIV anchor is reset after the blip returns.
    IF (\1) == 0
    call T15Blip0
    ELSE
    call T15Blip1
    ENDC

    xor a
    ldh [rNR52], a                      ; APU off: DIV-APU stopped
    ldh [rDIV], a                       ; DIV = 0 -> D is measured from here
    REPT (((\2) + 1 + (\1)) & 1)
    nop
    ENDR
    ld a, AUDENA_ON
    ldh [rNR52], a                      ; APU on -> alignment = 0 at this write
    REPT (1 - (((\2) + 1 + (\1)) & 1))
    nop
    ENDR
    ld a, $FF
    ldh [rNR51], a                      ; both terminals
    ld a, $77
    ldh [rNR50], a                      ; master volume, no VIN
    ld a, $F1                           ; volume 15, decreasing, period 1
    ldh [rNR42], a
    xor a
    ldh [rNR43], a                      ; divisor 0, shift 0: broadband noise
    ld hl, rNR44
    apu_delay_mcycles ((\2) - 32)
    ld a, AUDHIGH_TRIGGER               ; trigger, length counter disabled
    ld [hl], a                          ; THE WRITE, at D M-cycles

    ldh a, [rNR52]                      ; liveness: did channel 4 start?
    ld [de], a
    inc de
    call T15Window
    ldh a, [rNR52]                    ; liveness: still enabled at the end?
    ld [de], a
    inc de
ENDM

; Append the byte \1 at DE.
MACRO t15_db
    ld a, \1
    ld [de], a
    inc de
ENDM

SECTION "t15_subs", ROM0[$180]

; One measurement's leading marker: a 50 ms blip at the series' pitch, then
; 50 ms of silence. Clobbers A, BC.
T15Blip0:
    t15_tone BLIP_MC, TONE_512HZ, GAP_MC
    ret

T15Blip1:
    t15_tone BLIP_MC, TONE_1024HZ, GAP_MC
    ret

; The post-trigger measurement window. Clobbers A, BC.
T15Window:
    t15_wait WINDOW_MC
    ret

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "t15_main", ROM0
Start:
    rbhw_boot_capture
    di
    ld sp, $CFFF

    ; The LCD is left as the boot ROM had it and never re-enabled; no OAM DMA
    ; ever runs, no interrupt source is armed and `halt` is never executed.
    ; Nothing but the CPU touches the bus during a measurement.
    xor a
    ldh [rIE], a
    ldh [rIF], a

    ld a, T15_ROM_ID
    ld de, PAYLEN
    call RbhwBeginRecord                ; leaves cart RAM unlocked

    ld de, RBHW_SRAM + RBHW_PAYLOAD

    t15_db $01                          ; block id
    t15_db SWEEP_STEPS
    t15_db SERIES
    t15_db CENTRES
    t15_db SWEEP_STEP
    t15_db 0                            ; reserved, keeps the words aligned
    t15_db LOW(SWEEP_BASE_0)
    t15_db HIGH(SWEEP_BASE_0)
    t15_db LOW(SWEEP_BASE_1)
    t15_db HIGH(SWEEP_BASE_1)
    t15_db LOW(BLIP_MC)
    t15_db HIGH(BLIP_MC)
    t15_db LOW(GAP_MC)
    t15_db HIGH(GAP_MC)
    t15_db LOW(WINDOW_MC & $FFFF)
    t15_db HIGH(WINDOW_MC & $FFFF)
    t15_db LOW(WINDOW_MC >> 16)
    t15_db HIGH(WINDOW_MC >> 16)
    t15_db LOW(TONE_512HZ)
    t15_db HIGH(TONE_512HZ)
    t15_db LOW(TONE_1024HZ)
    t15_db HIGH(TONE_1024HZ)

FOR CEN, 0, CENTRES
FOR SER, 0, SERIES
    ; LEAD pitch names the centre: 256 Hz for centre 0, 128 Hz for centre 1.
    t15_tone LEAD_MC, (TONE_256HZ * (1 - CEN) + TONE_128HZ * CEN), LEAD_MC
FOR SIDX, 0, SWEEP_STEPS
    t15_step SER, ((SWEEP_BASE_0 * (1 - CEN) + SWEEP_BASE_1 * CEN) + SIDX * SWEEP_STEP)
ENDR
ENDR
ENDR

    xor a
    ldh [rNR52], a                      ; APU off for good

    ld de, PAYLEN
    call RbhwFinish                     ; CRC16 + re-lock cart RAM

    ; Completion signal for the operator: the screen goes black.
    ld a, $FF
    ldh [rBGP], a

    ; Anti-grading trap: $FF everywhere is not the Fibonacci handoff.
    ld a, $FF
    ld b, a
    ld c, a
    ld d, a
    ld e, a
    ld h, a
    ld l, a
    ld b, b
.spin:
    jr .spin

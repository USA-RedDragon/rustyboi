; ch4_retrigger_deferral.dmg.bench — T14. On DMG, what does a SECOND NR44
; trigger do while the first one's 6-cycle delayed start is still in flight?
;
; THIS ROM DOES NOT GRADE ANYTHING. It records raw NR52 bytes to cart SRAM. The
; cell it asks about is not silicon-verified — SameBoy explicitly flags it as
; incompletely emulated — so asserting an expected value here would freeze an
; unverified inference into a permanent oracle, which test-roms/README.md's
; provenance rule forbids. See "Readout" and "Operator protocol" below.
;
; ---------------------------------------------------------------------------
; The question
; ---------------------------------------------------------------------------
;
; rustyboi models a DMG-ONLY deferred noise trigger: an NR44 trigger that
; arrives while the channel-4 ripple counter's phase accumulator `alignment`
; satisfies `alignment & 3 != 0` does not start the channel at the write.
; It arms a 6-APU-cycle countdown (`dmg_delayed_start = 6`, see
; rustyboi-core/src/audio/noise.rs) and the whole start — LFSR reseed, ripple
; counter setup, volume reload, `enabled = true` — runs at the crossing.
;
; What happens if a SECOND trigger write lands inside those 6 cycles is the
; subject. rustyboi currently RESTARTS the deferral: the second write re-arms
; the countdown to 6 from itself, so the burst produces exactly ONE eventual
; start, 6 cycles after the LAST write. The alternative — the behaviour
; rustyboi had before — is that the second trigger performs a complete
; immediate start of its own AND leaves the original crossing armed, producing
; a second (ghost) start a few cycles later.
;
; Neither is measured. SameBoy models the 6-cycle delay but marks this exact
; cell unfinished (Core/apu.c, ~line 1213):
;
;     "TODO: When restarting a channel right after starting it, the channel
;      isn't restarted... Only certain behaviors of this edge case are
;      emulated."
;
; So the reference implementation declines to answer, and no published test ROM
; discriminates the two. That is why this ROM exists.
;
; ---------------------------------------------------------------------------
; Why NR52 bit 3 can see it
; ---------------------------------------------------------------------------
;
; NR52 bit 3 is channel 4's "enabled" status flag (Pan Docs, "Audio Registers"
; — NR52). Under either model the bit is 0 while a start is merely pending and
; 1 once a start has actually run. So the two models differ in exactly one
; observable place: the window between the second trigger write and its
; crossing.
;
;   RESTART model    — the second write only re-arms; nothing has started yet,
;                      and the single start lands 6 cycles after write #2.
;   IMMEDIATE model  — the second write starts the channel there and then.
;
; NR42 is $F0 (initial volume 15, envelope direction 0, period 0), so the DAC
; is on, the envelope never steps, and the channel can never switch itself off
; during the measurement. NR44 is written as $80 — trigger with the length
; counter DISABLED — so the length unit can never clear the bit either. Bit 3
; therefore reports the start pipeline and nothing else.
;
; ---------------------------------------------------------------------------
; Alignment arithmetic — how the two phases are reached
; ---------------------------------------------------------------------------
;
; Established facts this ROM leans on:
;
;   * the APU cycle counter runs at exactly 2x the CPU M-cycle rate, so one
;     M-cycle is 2 APU cycles;
;   * `alignment` is zeroed by the APU power-on write to NR52, which is itself
;     a CPU write and therefore itself on the M-cycle grid.
;
; Consequently, at ANY CPU-written NR44 trigger, `alignment & 3` can only be
; 0 or 2 — never 1 or 3 — and a single `nop` inserted before the write toggles
; between the two. The deferral engages only at `alignment & 3 == 2`.
;
; This ROM counts M-cycles from the NR52 power-on write's memory-access cycle
; (the third and last M-cycle of `ldh [rNR52], a`) to the trigger's access
; cycle (the second and last M-cycle of `ld [hl], a`):
;
;     ld a, $FF          2      ld hl, rNR44          3
;     ldh [rNR51], a     3      ld c, LOW(rNR52)      2
;     ld a, $77          2      ld a, $80             2
;     ldh [rNR50], a     3      REPT PAD / nop        PAD
;     ld a, $F0          2      ld [hl], a            2   <- W1 access
;     ldh [rNR42], a     3
;     xor a              1      total = 28 + PAD M-cycles
;     ldh [rNR43], a     3
;
; 28 is even, so `alignment & 3` at W1 is 0 for PAD = 0 and 2 for PAD = 1.
; PAD = 1 is therefore the DEFERRING variant and PAD = 0 the control — under
; rustyboi's own accounting. Do NOT take that mapping on faith when reading a
; capture: the control identifies itself (see "Readout"), and if silicon
; disagrees about which parity defers, that is itself a finding.
;
; The measured window uses `ld [hl], a` rather than `ldh [c], a` for the two
; trigger writes so that every instruction in it is exactly 2 M-cycles and
; every memory access falls on its instruction's last cycle:
;
;     ld [hl], a      ; W1  trigger #1      HL = $FF23 (NR44)
;     ld [hl], a      ; W2  trigger #2      = W1 + 2 M-cycles = W1 + 4 APU cc
;     REPT EXTRA / nop / ENDR
;     ldh a, [c]      ; R1  the read        C  = $26   (LOW of NR52)
;
; With EXTRA = 0 the read lands at W2 + 4 APU cc; with EXTRA = 2 at W2 + 8.
;
; ---------------------------------------------------------------------------
; The four nop-sled cells
; ---------------------------------------------------------------------------
;
;   id   PAD  read at    what rustyboi's RESTART model implies
;   $01   0   W2 + 4     control  — no deferral at all, bit 3 = 1
;   $02   0   W2 + 8     control  — no deferral at all, bit 3 = 1
;   $03   1   W2 + 4     DISCRIMINATOR. Under the restart model the single
;                        start is due at W2 + 6, so the read at W2 + 4 is
;                        still inside the pipeline: bit 3 = 0.
;                        Under the immediate model write #2 already started
;                        the channel: bit 3 = 1.
;   $04   1   W2 + 8     past the crossing under either model: bit 3 = 1.
;
; The table is deliberately NON-UNIFORM: only cell $03 can read 0 under any
; model considered, so an implementation (or a cart read) that returns a stuck
; value is visible on its face. Cells $01/$02 are the built-in control — they
; must read 1. If they do not, the run is void and nothing else in the record
; means anything.
;
; Each cell is repeated 3 times so instability on real silicon is visible
; rather than averaged away.
;
; A second read R2 is taken ~7 M-cycles after R1, well past every crossing in
; play. R2 must be 1 in all eight cells: it proves the channel really did start
; and that the ROM is reading a live NR52, not a dead bus.
;
; ---------------------------------------------------------------------------
; Why there is no HALT anywhere in this ROM
; ---------------------------------------------------------------------------
;
; The alignment arithmetic above assumes the CPU never leaves the M-cycle grid.
; rustyboi VIOLATES that assumption: its HALT batcher can resume the CPU at an
; off-grid cycle offset, which would put `alignment & 3` at 1 or 3 — a phase
; real silicon cannot produce. Whether that off-grid resume is a genuine
; accuracy defect is an open question tracked separately, and this ROM must not
; depend on its answer. Every delay and every phase step here is therefore a
; plain instruction sled; `halt` appears nowhere.
;
; ---------------------------------------------------------------------------
; Readout — payload format at $A020 (header format in include/rbhw_capture.inc)
; ---------------------------------------------------------------------------
;
; Four cells, in the order $01, $02, $03, $04. Each cell is 10 bytes:
;
;   +0 cell id
;   +1 PAD (0 or 1)
;   +2 read offset after W2, in APU cycles (4 or 8)
;   +3 repeat count (3)
;   +4.. 3 repeats of { R1 byte, R2 byte } — both raw NR52 reads
;
; Payload length 40 bytes; total record 72 bytes.
;
; The bytes are whole NR52 reads, not extracted bits: bit 7 (APU on) and the
; other channels' status bits come along for free and are worth eyeballing.
; NR52 bits 4-6 are unused and read back as 1 (Pan Docs, "Audio Registers"), so
; with only channel 4 ever triggered the byte should read $F0 (bit 3 clear) or
; $F8 (bit 3 set). Anything else — $FF, $00, a byte with another channel's
; status bit set — means the read did not come off a live APU and the run is
; void.
;
; ---------------------------------------------------------------------------
; Operator protocol
; ---------------------------------------------------------------------------
;
;   Console: DMG. Primary unit DMG-08. Also worth running on an early DMG
;   (DMG-01/CPU-DMG, no letter suffix) and on an MGB, since the delayed start
;   is a DMG-class behaviour and a revision spread would be informative. Note
;   the exact unit on the passport; the fingerprint vector in the record header
;   stores the raw boot A/B handoff (DMG $01 vs MGB $FF) but not the revision.
;   NOT caution-class: nothing here stresses the bus, a primary unit is fine.
;   Do NOT run it on a CGB — the deferral is DMG-only and a CGB capture would
;   only show the control values.
;
;   Cart: any MBC5 + RAM + battery cart. Flash the .gb as-is; the DMG header is
;   correct as built.
;
;   Run: power on, wait for the screen to turn BLACK (the completion signal —
;   the whole capture takes a few hundredths of a second), power off, read the
;   save back with a GBxCart RW. Repeat the whole power-cycle at least three
;   times; the run counter at $A006 must advance 1, 2, 3. If it stays at 1 the
;   cart battery is dead and the capture is worthless. Verify the CRC16 at
;   $A012 before believing any byte.
;
;   Read the CONTROL CELLS FIRST. Cells $01 and $02 must show bit 3 set in
;   every R1 and R2. If they do not, stop — the run is void.
;
;   Then read cell $03's three R1 bytes:
;
;     bit 3 = 0 (NR52 = $F0)
;         -> the second trigger did NOT start the channel; the burst has ONE
;            pipeline and it restarted. rustyboi's current model is right.
;     bit 3 = 1 (NR52 = $F8)
;         -> the second trigger executed a start of its own. rustyboi's model
;            is wrong and the pre-change behaviour (immediate start plus an
;            armed crossing) is closer — though "plus a ghost second start"
;            is a separate claim this ROM does not test.
;     mixed across the three repeats
;         -> do not average it, report it. An unstable cell is a new finding
;            and probably means the phase is marginal on this unit.
;
;   Cell $04 must read bit 3 = 1 under either model; if it reads 0 the delay is
;   longer than 6 APU cycles on this silicon, which is itself a finding worth
;   reporting (and would call for a re-run with more read offsets).
;
; ---------------------------------------------------------------------------
; Wiring
; ---------------------------------------------------------------------------
;
; Deliberately NOT a row in rustyboi-test-runner/suites/rustyboi.manifest — the
; manifest generator emits one GRADED row per ROM found in test-roms/build/,
; and every grading it offers asserts an expected value, which is the one thing
; this ROM must not do. The Makefile routes `.bench.` ROMs to
; test-roms/build-bench/ instead, outside the generator's scan. Build with
; `make -C test-roms roms`; run it by hand (see test-roms/README.md).
;
; As a second line of defence the ROM ends with `ld b, b` while every register
; holds $FF — deliberately NOT the Fibonacci handoff the `rustyboi`/mooneye
; grading looks for — so if it is ever wired into the graded suite by accident
; it fails loudly instead of passing silently.

INCLUDE "hardware.inc"
INCLUDE "apu.inc"
INCLUDE "rbhw_capture.inc"

DEF T14_ROM_ID  EQU $14

DEF REPEATS     EQU 3
DEF CELL_BYTES  EQU 4 + REPEATS * 2
DEF CELLS       EQU 4

DEF PAYLEN      EQU CELLS * CELL_BYTES
ASSERT PAYLEN == 40
ASSERT RBHW_PAYLOAD + PAYLEN <= $2000      ; fits the 8 KiB cart RAM bank

; One measurement. DE = the SRAM cursor; appends R1 then R2 and advances it.
;   \1 = PAD   (nops before W1: 0 -> alignment&3 == 0, 1 -> alignment&3 == 2)
;   \2 = EXTRA (nops between W2 and R1: 0 -> read at W2+4 cc, 2 -> at W2+8)
MACRO t14_one
    xor a
    ldh [rNR52], a                      ; APU off
    ld a, AUDENA_ON
    ldh [rNR52], a                      ; APU on -> alignment = 0 at this write
    ld a, $FF
    ldh [rNR51], a                      ; both channels to both terminals
    ld a, $77
    ldh [rNR50], a                      ; master volume, no VIN
    ld a, $F0
    ldh [rNR42], a                      ; volume 15, envelope off, DAC on
    xor a
    ldh [rNR43], a                      ; divisor 0, shift 0
    ld hl, rNR44
    ld c, LOW(rNR52)

    ld a, AUDHIGH_TRIGGER
    REPT (\1)
    nop
    ENDR

    ld [hl], a                          ; W1
    ld [hl], a                          ; W2  = W1 + 4 APU cc
    REPT (\2)
    nop
    ENDR
    ldh a, [c]                          ; R1  = W2 + 4 (+ 2*EXTRA) APU cc
    ld [de], a
    inc de
    ldh a, [rNR52]                      ; R2, 7 M-cycles later: did it start?
    ld [de], a
    inc de
ENDM

; One cell: its 4-byte header, then REPEATS measurements.
;   \1 = cell id, \2 = PAD, \3 = EXTRA
MACRO t14_cell
    ld a, \1
    ld [de], a
    inc de
    ld a, \2
    ld [de], a
    inc de
    ld a, 4 + 2 * (\3)                  ; read offset after W2, in APU cycles
    ld [de], a
    inc de
    ld a, REPEATS
    ld [de], a
    inc de
    REPT REPEATS
    t14_one \2, \3
    ENDR
ENDM

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "t14_main", ROM0[$150]
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

    ld a, T14_ROM_ID
    ld de, PAYLEN
    call RbhwBeginRecord                ; leaves cart RAM unlocked

    ld de, RBHW_SRAM + RBHW_PAYLOAD

    ;         id  PAD EXTRA
    t14_cell $01,  0,   0               ; control,       read W2+4
    t14_cell $02,  0,   2               ; control,       read W2+8
    t14_cell $03,  1,   0               ; DISCRIMINATOR, read W2+4
    t14_cell $04,  1,   2               ; deferring,     read W2+8

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

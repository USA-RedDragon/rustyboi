; stop_ly_glitch.cgb.bench — T19. After a double-speed -> single-speed STOP
; speed switch, what does FF44 (LY) read on the LY-increment "glitch dot"?
;
; THIS ROM DOES NOT GRADE ANYTHING. It records raw bytes to cart SRAM. The
; behaviour it is asking about is not silicon-verified for AGB, so asserting an
; expected value here would freeze an unverified inference into a permanent
; oracle -- exactly what test-roms/README.md's provenance rule forbids. See
; "Readout" and "Operator protocol" below.
;
; ---------------------------------------------------------------------------
; The question
; ---------------------------------------------------------------------------
;
; In the last few cycles of a scanline the FF44 register does not simply hold
; the renderer's LY: it anticipates the next line. On ONE dot of that window --
; the "glitch dot" -- silicon has been observed to return neither the old LY nor
; the new one but the PARTIAL LATCH `ly & (ly+1)`, the half-updated counter
; caught mid-increment. For LY 143 that is `$8F & $90 = $80`; for LY 139,
; `$8B & $8C = $88`. Note the fold is INVISIBLE when LY is EVEN
; (`ly & (ly+1) == ly` whenever bit 0 is clear), so only ODD LY discriminates.
;
; Which revisions fold is revision-dependent. rustyboi models:
;
;   CPU-CGB-B/C  folds unconditionally on the glitch dot.
;   CPU-CGB-D/E  folds only when the accumulated post-STOP sub-dot parity lands
;                the read ON the boundary; otherwise it reads the stale `ly`.
;
; AGB (Game Boy Advance in GBC-compat mode) is currently routed to the CGB-B/C
; side. That placement is INHERITED, not measured: it falls out of a bare
; `is_cgb_de()` predicate in rustyboi-core/src/ppu/controller.rs, and the
; LY-glitch fold is outside the four families `Mmio::set_cgb_de` documents as
; deliberate (LY-153 window, end-of-vblank STAT, OAM read windows, speed-switch
; TIMA edge). No oracle anywhere covers it.
;
; The existing corpus CANNOT settle it. Flipping both arms of the fold to put
; AGB on the D/E side leaves the pass/fail set of all 150 graded AGB capture
; rows completely unchanged (84/150 either way), because the on-point captures
; (lcd/last_ly_ly_change, lcd/last_ly_clocks) are BYTE-IDENTICAL between
; real_gba.sav, real_gba_sp.sav and real_gbc.sav -- those ROMs never reach the
; fold at all. That is why this ROM exists.
;
; ---------------------------------------------------------------------------
; Reaching the fold -- why a plain mode-3 STOP is not enough
; ---------------------------------------------------------------------------
;
; The glitch dot is one specific sub-dot offset before the LY increment. A CPU
; read can only land on the M-cycle grid, and the scanline (456 dots) is an
; exact multiple of the M-cycle, so the offset a read samples at is FIXED --
; padding the read stream by whole M-cycles slides it by whole M-cycles and can
; never change which sub-dot it lands on. Delay alone cannot reach the glitch
; dot.
;
; What DOES move it is the STOP speed switch itself. Each DS->SS switch applies
; a half-dot re-anchor, so the accumulated number of switches since power-on --
; and whether they were taken during mode 3 or outside it -- is what slides the
; read phase onto (or off) the glitch dot. Two switch classes behave
; differently, and BOTH are needed:
;
;   * DS->SS taken OUTSIDE mode 3 (OAM/HBlank) -- these are what arm the
;     post-STOP phase state at all.
;   * DS->SS taken DURING mode 3 -- these contribute the extra STAT-phase carry
;     that shifts the sampled offset by a further dot.
;
; So the stimulus is a MIX. This ROM therefore does not fire one switch and
; hope: it sweeps the accumulated switch state across 48 rounds while sweeping
; the read alignment across 4 M-cycles, and records every byte. That sweep is
; the whole point -- it removes any dependence on the mode-3 timing arithmetic
; below being exactly right, which is the main risk in a measurement like this.
;
; ---------------------------------------------------------------------------
; Mode-3 timing arithmetic, and its assumptions
; ---------------------------------------------------------------------------
;
; Pan Docs, "Pixel FIFO" / "Rendering": a scanline is 456 dots; mode 2 (OAM
; scan) is the first 80 dots; mode 3 (pixel transfer) then runs a MINIMUM of
; 172 dots, i.e. dots 80..252; mode 0 (HBlank) is the remainder, dots 252..456.
; Mode 3 is LENGTHENED by, and only by:
;
;   * SCX % 8 -- up to 7 dots of discarded pixels at the left edge;
;   * the window -- ~6 dots when WX activates mid-line;
;   * sprites -- 6..11 dots per sprite fetched on the line.
;
; This ROM keeps the scene trivial so all three penalties are exactly ZERO and
; mode 3 is the flat 172-dot minimum: SCX=0 and SCY=0 (no fine-scroll
; discard), OBJ disabled in LCDC (no sprite fetches -- the OAM contents are
; irrelevant once bit 1 is clear), and the window disabled in LCDC with WX/WY
; parked at 0. ASSUMPTION STATED: mode 3 == dots 80..252 on every line. The
; ROM does not depend on this being exact -- it waits for the mode-2 -> mode-3
; transition by POLLING STAT rather than by counting dots, and the read
; alignment is swept -- but it is the basis for the two claims below.
;
;   Mid-mode-3 STOP: the ROM polls for mode 2, then polls for mode 3, then
;   issues the switch. Detection-to-STOP is 7 M-cycles; in double speed an
;   M-cycle is 4 master cycles and a dot is 2, so that is ~14 dots into a
;   172-dot mode 3. Margin is ~158 dots. Even if every penalty above applied at
;   once (~7 + 6 + 10*11 dots) the STOP would still land inside mode 3.
;
;   Mid-mode-3 baseline read (block $02): starts ~4 M-cycles (16 dots) after the
;   mode-3 transition and runs 8 reads at 4 M-cycles each, spanning dots
;   ~101..229 of the line -- entirely inside mode 3, and ~227 dots from the
;   line-456 boundary, so nowhere near any anticipation window.
;
; ---------------------------------------------------------------------------
; Measurement
; ---------------------------------------------------------------------------
;
; The read train is a flat unrolled sequence of
;
;     ldh a, [c]      ; c = $44 (LY) or $41 (STAT), 2 M-cycles
;     ld [hl+], a     ; straight to cart SRAM,      2 M-cycles
;
; so samples are exactly 4 M-cycles (16 dots at single speed) apart, with no
; loop overhead and no branch to perturb the phase. 32 samples span 512 dots,
; longer than a 456-dot line, so EVERY train is guaranteed to cross at least one
; LY increment whatever the starting alignment.
;
; A "round" is: optionally perform one SS->DS + one DS->SS switch (so each round
; ends back in single speed), then run the train once at each of 4 pad
; alignments -- 0, 1, 2 and 3 NOP M-cycles ahead of the train. Since samples are
; 4 M-cycles apart, those 4 pads exhaustively cover every sample position within
; the train's own period. Rounds sweep the accumulated switch state.
;
; Blocks:
;
;   $01 CTRL   3 rounds, NO speed switch anywhere. The single-speed control:
;              the machine never leaves single speed, so no post-STOP phase can
;              exist and every read must be either the plain LY or the ordinary
;              (non-post-STOP) end-of-line anticipation. This is what proves the
;              harness's timing lands where intended -- if CTRL does not show a
;              clean LY ramp with one increment per 456 dots, nothing else in
;              the record means anything. READ THIS BLOCK FIRST.
;
;   $02 BASE   3 repeats of a mid-mode-3 LY read far from any boundary (see the
;              arithmetic above). Sanity baseline: every byte in a repeat must
;              be the SAME LY, or one clean increment at most. Also runs before
;              any switch has been fired, so it doubles as the pre-stimulus
;              reference.
;
;   $03 SWEEP  48 rounds, each firing one SS->DS + one DS->SS pair. The DS->SS
;              of round i is taken DURING MODE 3 when bit 1 of i is set, and
;              outside mode 3 (HBlank) otherwise -- the repeating pattern
;              non, non, mode3, mode3. That walks the accumulated
;              (non-mode-3, mode-3) switch counts through a wide range of
;              combinations, and each round's 4-pad sweep re-measures at every
;              alignment. THIS IS THE PRIMARY MEASUREMENT.
;
;   $04 STAT   8 further rounds, identical in every respect to $03 except that
;              the train reads $FF41 (STAT) instead of $FF44 (LY). Recording
;              STAT makes the LY record far easier to interpret -- the mode bits
;              say exactly where in the line each sample fell.
;
;              It is a SEPARATE BLOCK, and deliberately so. Reading both
;              registers in one train would cost 2 extra M-cycles per sample,
;              changing the sample spacing from 4 to 6 M-cycles and moving every
;              sample off the alignment the LY measurement was swept over. That
;              would compromise the primary measurement. As a separate block the
;              train is instruction-for-instruction identical to $03's, so its
;              samples land on the same grid, one register later in the sweep.
;
; The ROM never resets the accumulated switch state, because there is no way to:
; the half-dot re-anchor is physical, and a power cycle is the only reset. That
; is why the "x3 repeats" here take the form of (a) 4 pad alignments per round,
; of which pads 0-3 already re-measure the same sample positions, (b) each
; (non-mode-3, mode-3) parity class recurring many times across the 48 rounds,
; and (c) the operator power-cycle protocol below, which the run counter at
; $A006 makes auditable. An in-ROM repeat of a byte-identical stimulus is not
; constructible.
;
; ---------------------------------------------------------------------------
; Readout -- payload format at $A020 (header format in include/rbhw_capture.inc)
; ---------------------------------------------------------------------------
;
; Four blocks, in the order CTRL, BASE, SWEEP, STAT. Each block is:
;
;   8-byte block header:
;     +0 block id   +1 rounds   +2 pads   +3 reads per train
;     +4 repeats    +5 register read ($44 LY / $41 STAT)
;     +6 switch schedule (0 = none, 1 = one pair per round, mode-3 on bit 1 of
;        the round index)                                     +7 reserved, zero
;   then <rounds> rounds, each:
;     4-byte round header:
;       +0 round index
;       +1 cumulative count of DS->SS switches taken OUTSIDE mode 3
;       +2 cumulative count of DS->SS switches taken DURING mode 3
;       +3 reserved, zero
;     then <pads> trains of <reads> raw bytes, in pad order 0,1,2,3.
;
; BASE ($02) uses the same shape with pads = 1 and reads = 8.
;
; Payload length 7856 bytes; total record 7888 bytes, inside the 8 KiB.
;
; ---------------------------------------------------------------------------
; Operator protocol
; ---------------------------------------------------------------------------
;
;   Console: run this ROM on BOTH of, separately:
;     * a CGB -- any revision, but note the revision from the unit passport, as
;       the whole question is revision-shaped and the fingerprint vector in the
;       record header does NOT resolve CPU-CGB-C from -E. A CPU-CGB-C unit and a
;       CPU-CGB-D or -E unit are both wanted; they are the two known sides.
;     * an AGB (GBA or GBA SP) -- this is the unit the question is actually
;       about.
;   Not caution-class; primary units are fine. Nothing here writes VRAM
;   mid-frame or drives the LCD out of spec.
;
;   Cart: any MBC5 + RAM + battery cart. Flash the .gbc as-is.
;
;   Run: power on, wait for the screen to turn BLACK (the completion signal --
;   the whole capture takes well under a second), power off, read the save back
;   with a GBxCart RW. Repeat the whole power-cycle at least three times; the
;   run counter at $A006 must advance 1,2,3 (if it stays at 1 the cart battery
;   is dead and the capture is worthless). Verify the CRC16 at $A012 before
;   believing any byte. Label each save with the console AND its revision.
;
;   Interpretation:
;     1. CTRL ($01) first. It must show a clean LY ramp, one increment per 456
;        dots (one increment every 28.5 samples). If it does not, the run is
;        void -- do not read further.
;     2. BASE ($02) next. Each repeat must be a flat LY (at most one increment).
;        If it is not, the mode-3 assumption above does not hold on this unit and
;        the mode-3 STOP placement in $03 is suspect.
;     3. SWEEP ($03) is the answer. Walk each train looking for the sample where
;        LY increments. At an ODD LY, the byte immediately before the increment
;        is the discriminator:
;          reads the stale odd LY (e.g. $8B before $8C)
;              -> this unit does NOT fold on the glitch dot in this phase.
;          reads the partial latch `ly & (ly+1)` (e.g. $88 where $8B was
;          expected, or $80 before $90)
;              -> this unit DOES fold.
;          reads the already-incremented LY (e.g. $8C)
;              -> this sample missed the glitch dot; look at the other pads and
;                 the neighbouring rounds.
;        At an EVEN LY the two answers are identical by construction (see "The
;        question"), so even-LY boundaries carry no information -- do not read
;        anything into them.
;     4. Use STAT ($04) to confirm where in the line each sample fell.
;     5. Compare AGB against the CGB units -- but compare the ANSWER (does this
;        unit fold on its glitch dot, yes or no), NOT the raw bytes. Do NOT
;        diff an AGB save against a CGB save expecting them to line up: AGB
;        differs from CGB in unrelated FF41 / line-153 behaviour, which shifts
;        its read alignment, so the two records put their glitch dots at
;        different rounds and pads and disagree in many bytes for reasons that
;        have nothing to do with this question. Find each unit's own glitch-dot
;        samples per step 3, then compare the verdicts.
;        If AGB folds like CPU-CGB-C, the current bare `is_cgb_de()` routing is
;        right. If it behaves like CPU-CGB-D/E, both arms of the fold must
;        become `is_agb() || is_cgb_de()`.
;     6. Do NOT average across rounds or pads. A pattern that changes with the
;        accumulated switch count IS the signal, not noise.
;
; ---------------------------------------------------------------------------
; Wiring
; ---------------------------------------------------------------------------
;
; Deliberately NOT a row in rustyboi-test-runner/suites/rustyboi.manifest. The
; manifest generator emits one graded row per ROM found in test-roms/build/, and
; every grading it offers asserts an expected value -- which is the one thing
; this ROM must not do. So the Makefile routes `.bench.` ROMs to
; test-roms/build-bench/ instead, outside the generator's scan. Build with
; `make -C test-roms roms`; run it by hand (see test-roms/README.md).
;
; The ROM ends on a DELIBERATELY NON-FIBONACCI `LD B,B`: if it is ever wired
; into a graded suite by accident, the mooneye-convention register check fails
; loudly instead of silently "passing".

INCLUDE "hardware.inc"
INCLUDE "rbhw_capture.inc"

DEF T19_ROM_ID    EQU $19

DEF READS         EQU 32          ; samples per train (32 * 4 M-cycles = 512 dots)
DEF PADS          EQU 4           ; pad alignments, 0..3 M-cycles
DEF CTRL_ROUNDS   EQU 3
DEF SWEEP_ROUNDS  EQU 48
DEF STAT_ROUNDS   EQU 8
DEF BASE_REPEATS  EQU 3
DEF BASE_READS    EQU 8

DEF BLK_HDR       EQU 8
DEF RND_HDR       EQU 4

DEF TRAIN_BYTES   EQU RND_HDR + PADS * READS
DEF BASE_BYTES    EQU RND_HDR + BASE_READS

DEF PAYLEN        EQU 4 * BLK_HDR \
                    + CTRL_ROUNDS * TRAIN_BYTES \
                    + BASE_REPEATS * BASE_BYTES \
                    + SWEEP_ROUNDS * TRAIN_BYTES \
                    + STAT_ROUNDS * TRAIN_BYTES
ASSERT PAYLEN == 7856
ASSERT RBHW_PAYLOAD + PAYLEN <= $2000      ; fits the 8 KiB cart RAM bank

DEF LY_REG        EQU $44
DEF STAT_REG      EQU $41

; Working state. The measurement path keeps everything in WRAM rather than in
; registers so the timed section is a fixed instruction sequence.
SECTION "t19_state", WRAM0[$C000]
Cursor:      ds 2       ; next SRAM payload address
RoundIdx:    ds 1
PadIdx:      ds 1
PCount:      ds 1       ; cumulative DS->SS switches taken OUTSIDE mode 3
MCount:      ds 1       ; cumulative DS->SS switches taken DURING mode 3
; The block header, loaded from the caller and also copied verbatim to SRAM.
BlkId:       ds 1
BlkRounds:   ds 1
BlkPads:     ds 1
BlkReads:    ds 1
BlkRepeats:  ds 1
BlkReg:      ds 1
BlkSwitch:   ds 1
BlkRsvd:     ds 1

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "t19_main", ROM0[$150]
Start:
    rbhw_boot_capture
    ld sp, $CFFF

    ld a, T19_ROM_ID
    ld de, PAYLEN
    call RbhwBeginRecord

    ld hl, RBHW_SRAM + RBHW_PAYLOAD
    call StoreCursor

    xor a
    ld [PCount], a
    ld [MCount], a

    call SetupLcd

    ld hl, BlockCtrl
    call RunTrainBlock
    call RunBaseBlock
    ld hl, BlockSweep
    call RunTrainBlock
    ld hl, BlockStat
    call RunTrainBlock

    ld de, PAYLEN
    call RbhwFinish

    ; Completion signal for the operator: the screen goes black.
    ld a, $FF
    ldh [rBGP], a

    ; Deliberately NON-Fibonacci done marker. The mooneye convention wants
    ; B,C,D,E,H,L = 3,5,8,13,21,34; these are none of those, so any accidental
    ; wiring of this measurement ROM into a graded suite fails loudly.
    ld b, $FF
    ld c, $FF
    ld d, $FF
    ld e, $FF
    ld h, $FF
    ld l, $FF
    ld b, b
.spin:
    jr .spin

; Block headers, in the exact byte order they take in the payload.
;    id   rounds        pads  reads  repeats       reg        switch rsvd
BlockCtrl:
    db $01, CTRL_ROUNDS,  PADS, READS, CTRL_ROUNDS,  LY_REG,   0, 0
BlockBase:
    db $02, BASE_REPEATS, 1,    BASE_READS, BASE_REPEATS, LY_REG, 0, 0
BlockSweep:
    db $03, SWEEP_ROUNDS, PADS, READS, 1,            LY_REG,   1, 0
BlockStat:
    db $04, STAT_ROUNDS,  PADS, READS, 1,            STAT_REG, 1, 0

; ---------------------------------------------------------------------------
; LCD setup. Trivial scene so mode 3 is the flat 172-dot minimum: SCX/SCY 0,
; OBJ off, window off. See the timing arithmetic in the header comment.
; ---------------------------------------------------------------------------
SetupLcd:
    ld a, $30
    ldh [rP1], a                        ; no button rows selected, for STOP
    xor a
    ldh [rSCY], a
    ldh [rSCX], a
    ldh [rWY], a
    ldh [rWX], a
    ldh [rIF], a
    ldh [rIE], a
    ldh [$FF26], a                      ; APU off: one fewer moving part
    ; BG map stays $9800 and the window stays off: both are the cleared-bit
    ; default, so only the enable / tile-data bits are set here.
    ld a, LCDCF_ON | LCDCF_BG8000 | LCDCF_BGON
    ldh [rLCDC], a                      ; OBJ and window stay OFF
    call WaitFrame
    call WaitFrame
    ret

; ---------------------------------------------------------------------------
; Generic train block. HL = its block-header entry.
; ---------------------------------------------------------------------------
RunTrainBlock:
    call EmitBlockHeader
    xor a
    ld [RoundIdx], a
.round:
    ld a, [BlkSwitch]
    or a
    jr z, .noswitch
    call DoSwitchPair
.noswitch:
    call EmitRoundHeader
    xor a
    ld [PadIdx], a
.pad:
    call RunTrain
    ld a, [PadIdx]
    inc a
    ld [PadIdx], a
    ld c, a
    ld a, [BlkPads]
    cp c
    jr nz, .pad
    ld a, [RoundIdx]
    inc a
    ld [RoundIdx], a
    ld c, a
    ld a, [BlkRounds]
    cp c
    jr nz, .round
    ret

; ---------------------------------------------------------------------------
; BASE block: mid-mode-3 LY reads far from any boundary. Runs before any speed
; switch has been fired, so it is also the pre-stimulus reference.
; ---------------------------------------------------------------------------
RunBaseBlock:
    ld hl, BlockBase
    call EmitBlockHeader
    xor a
    ld [RoundIdx], a
.repeat:
    call EmitRoundHeader
    call WaitMode3Start
    nop                                 ; 4 M-cycles into mode 3 (see arithmetic)
    nop
    nop
    nop
    call LoadCursor
    ld h, d
    ld l, e
    ld c, LY_REG
    REPT BASE_READS
    ldh a, [c]
    ld [hl+], a
    ENDR
    call StoreCursor
    ld a, [RoundIdx]
    inc a
    ld [RoundIdx], a
    ld c, a
    ld a, [BlkRounds]
    cp c
    jr nz, .repeat
    ret

; ---------------------------------------------------------------------------
; One train: PadIdx pad M-cycles, then READS samples of BlkReg, straight to
; SRAM. Entered by jumping into the NOP sled at (ReadTrain - pad), so the pad is
; real executed NOPs rather than a loop, and the train itself has no branch.
; ---------------------------------------------------------------------------
RunTrain:
    call LoadCursor
    ld h, d
    ld l, e
    ld a, [BlkReg]
    ld c, a
    ld a, [PadIdx]
    ld d, HIGH(ReadTrain)
    ld e, LOW(ReadTrain)
    ; DE = ReadTrain - pad
    ld b, a
    ld a, e
    sub b
    ld e, a
    jr nc, .nocarry
    dec d
.nocarry:
    call JumpDE
    call StoreCursor
    ret

; `call JumpDE` runs the train and returns here: the pushed DE becomes the ret
; target while the original return address stays on the stack for the train's
; own `ret`.
JumpDE:
    push de
    ret

; Pad sled. Entry points, from the top: pad 3, 2, 1, then the train itself for
; pad 0. PADS is 4, so exactly 3 NOPs are needed.
PadSled:
    nop
    nop
    nop
ReadTrain:
    REPT READS
    ldh a, [c]
    ld [hl+], a
    ENDR
    ret

; ---------------------------------------------------------------------------
; Speed switching
; ---------------------------------------------------------------------------

; One SS->DS + one DS->SS pair, so the machine ends back in single speed. The
; SS->DS leg is always taken in HBlank; the DS->SS leg -- the one that matters --
; is taken DURING MODE 3 when bit 1 of the round index is set, and in HBlank
; otherwise, giving the repeating pattern non, non, mode3, mode3.
DoSwitchPair:
    call WaitMode0
    call SpeedSwitch                    ; SS -> DS
    ld a, [RoundIdx]
    and 2
    jr z, .outside
    call WaitMode3Start
    call SpeedSwitch                    ; DS -> SS, during mode 3
    ld a, [MCount]
    inc a
    ld [MCount], a
    ret
.outside:
    call WaitMode0
    call SpeedSwitch                    ; DS -> SS, in HBlank
    ld a, [PCount]
    inc a
    ld [PCount], a
    ret

SpeedSwitch:
    ld a, $01
    ldh [$FF4D], a                      ; KEY1 bit 0: prepare speed switch
    stop
    ret

WaitMode0:
    ldh a, [rSTAT]
    and 3
    jr nz, WaitMode0
    ret

; Wait for the mode-2 -> mode-3 transition, so the caller lands at the START of
; mode 3 with the full ~172-dot window ahead of it (rather than anywhere inside
; it, which a bare "is mode 3" poll would give).
WaitMode3Start:
    ldh a, [rSTAT]
    and 3
    cp 2
    jr nz, WaitMode3Start
.wait3:
    ldh a, [rSTAT]
    and 3
    cp 3
    jr nz, .wait3
    ret

WaitFrame:
    ldh a, [rLY]
    cp 144
    jr nz, WaitFrame
.tail:
    ldh a, [rLY]
    cp 144
    jr z, .tail
    ret

; ---------------------------------------------------------------------------
; Payload bookkeeping
; ---------------------------------------------------------------------------

; Copy the 8-byte block header at HL to SRAM and into the Blk* variables.
EmitBlockHeader:
    push hl
    call LoadCursor
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
    ld de, BlkId
    ld c, BLK_HDR
.toVars:
    ld a, [hl+]
    ld [de], a
    inc de
    dec c
    jr nz, .toVars
    ret

; Emit the 4-byte round header: round index, cumulative outside-mode-3 DS->SS
; count, cumulative during-mode-3 DS->SS count, reserved.
EmitRoundHeader:
    call LoadCursor
    ld a, [RoundIdx]
    ld [de], a
    inc de
    ld a, [PCount]
    ld [de], a
    inc de
    ld a, [MCount]
    ld [de], a
    inc de
    xor a
    ld [de], a
    inc de
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

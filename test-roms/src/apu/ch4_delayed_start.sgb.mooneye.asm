; ch4_delayed_start.sgb.mooneye — the DMG-side channel-4 start invariant, run on
; SGB hardware.
;
; Same body as ch4_delayed_start.dmg.mooneye (derivation in
; apu_ch4_delayed_start.inc): a trigger with the DAC on must turn channel 4 on
; at every M-cycle placement of the write, and when a second trigger follows
; 2 M-cycles later.
;
; The SGB is a DMG-class machine behind an ICD2, so it takes the same non-CGB
; APU path with its deferred channel-4 start — and the drumroll latch this
; guards against was reported on DMG *and* SGB while CGB was clean. The SGB
; build pins that second platform; it needs no SGB command packets, only the
; SGB header entries the Makefile's `sgb` model supplies.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "apu.inc"
INCLUDE "apu_ch4_delayed_start.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    ch4_delayed_start_test

; ch4_delayed_start.dmg.mooneye — a channel-4 trigger with the DAC on always
; turns the channel on (NR52 bit 3), at every M-cycle placement of the write
; and even when a second trigger follows 2 M-cycles later.
;
; Pan Docs, "Audio Registers" (https://gbdev.io/pandocs/Audio_Registers.html):
; NRx4 bit 7 triggers the channel; NR52's low bits report whether each channel
; is on. On the non-CGB APU the channel-4 start is deferred to a later ripple
; phase rather than applied at the write — a delay, not a cancellation, so the
; start must still land. The ROM asserts only that end state, never the delay's
; length (that figure is emulator-model-derived).
;
; Derivation, subtest list and the anti-vacuity canary in
; apu_ch4_delayed_start.inc. The CGB APU has no such deferral, so this is a
; DMG-side ROM; the same file grades an SGB build of the behavior.

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

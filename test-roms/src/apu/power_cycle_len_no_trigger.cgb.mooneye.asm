; power_cycle_len_no_trigger.cgb.mooneye — on CGB an NRx1 length load written
; while the APU is off is REJECTED, so nothing survives the power-on and the
; trigger reloads the counter from zero. CGB build, single speed throughout.
;
; gbdev wiki, "Power Control"
; (https://gbdev.gg8.se/wiki/articles/Gameboy_sound_hardware), and Pan Docs,
; "Audio Registers" (https://gbdev.io/pandocs/Audio_Registers.html): with the
; APU off every register but NR52 is read-only; the writable-while-off exception
; is monochrome only, so on CGB the NR21 write is dropped.
;
; Write NR21 = $30 while off, power on, then trigger CH2 with length enabled and
; no NR21 rewrite. CGB rejected the write, so the counter is the power-cycle 0
; and the trigger reloads the full 64 (Pan Docs "Audio details", Triggering):
; expect 64 (the DMG build expects 16: the write is accepted there, so the
; counter is 16 and the trigger leaves it). A CGB measuring 16 is running the
; monochrome accept-while-off exception.
;
; Full timeline and window derivation in apu_power_cycle_len.inc.

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "apu.inc"
INCLUDE "apu_power_cycle_len.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    power_cycle_len_test PCL_EXPECT2_CGB

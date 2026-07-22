; power_cycle_len_no_trigger.dmg.mooneye — on DMG an NRx1 length load written
; while the APU is off survives the power-on, so the length counter it set is
; still there when the channel is triggered.
;
; gbdev wiki, "Power Control"
; (https://gbdev.gg8.se/wiki/articles/Gameboy_sound_hardware), and Pan Docs,
; "Audio Registers" (https://gbdev.io/pandocs/Audio_Registers.html): with the
; APU off every register but NR52 is read-only, "except on monochrome models"
; for the length timers — so the NR21 write is accepted on DMG and dropped on
; CGB.
;
; The counter is read WITHOUT a trigger reload that would hide it: write
; NR21 = $30 while off (counter 64 - $30 = 16), power on, then trigger CH2 with
; length enabled and no NR21 rewrite. A trigger reloads a length counter only
; when it is already zero (Pan Docs "Audio details", Triggering), so the
; non-zero 16 is left in place and the channel lives 16 length ticks — expect
; 16 (the CGB build expects 64: the write is rejected there, so the trigger
; reloads a zero counter).
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
    power_cycle_len_test PCL_EXPECT2_DMG

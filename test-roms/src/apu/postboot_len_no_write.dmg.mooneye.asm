; postboot_len_no_write.dmg.mooneye — triggering a channel the boot ROM never
; wrote an NRx1 for must reload the FULL length counter (64 / 256 / 64), because
; the counter it inherits is the power-on 0.
;
; Pan Docs, "Audio Registers" (https://gbdev.io/pandocs/Audio_Registers.html)
; lists the post-boot APU register bytes. The only NRx1 among them is NR11 = $80;
; NR21, NR31 and NR41 are never written by the boot ROM, so CH2/CH3/CH4 hand off
; with the counters power-on left.
;
; gbdev wiki, "Power Control"
; (https://gbdev.gg8.se/wiki/articles/Gameboy_sound_hardware): the length
; counters are "always zero at power on (CGB-02, CGB-04, CGB-05)".
;
; Pan Docs, "Audio details", Triggering: a trigger reloads a length timer that
; has expired (is zero) to its maximum — 64 for CH1/CH2/CH4, 256 for CH3. So the
; post-boot chain "counter 0 -> trigger -> 64/256/64" is fully documented; each
; channel below is triggered with NRx1 never written, and its lifetime measured
; in length ticks.
;
; This is the DMG side of a boot-path asymmetry worth pinning on both models: on
; DMG an NRx1 write while the APU is off IS accepted as a length load, so a boot
; table (or a game) that writes NR31/NR41 while the APU is still off leaves a
; counter of 1 and the channel dies one tick after any length-enabled trigger.
; Every window here rejects that outcome.
;
; CH1 is the control cell: NR11 = $80 loads 64 and post-boot NR14 = $BF leaves
; length disabled, so CH1 must read the same 64 as the never-written channels
; (derivation and the exact write sequence in apu_postboot_len.inc).

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "apu.inc"
INCLUDE "apu_postboot_len.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    postboot_len_test

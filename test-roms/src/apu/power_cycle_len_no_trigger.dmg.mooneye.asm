; power_cycle_len_no_trigger.dmg.mooneye — an NR52 power cycle leaves the length
; counters at zero, and on DMG an NRx1 write made while the APU was off survives
; the power-on.
;
; gbdev wiki, "Power Control"
; (https://gbdev.gg8.se/wiki/articles/Gameboy_sound_hardware): the length
; counters are "always zero at power on (CGB-02, CGB-04, CGB-05)"; the same
; article notes the length counters are writable while the APU is off on DMG.
; Pan Docs, "Audio Registers"
; (https://gbdev.io/pandocs/Audio_Registers.html), makes the same monochrome
; exception: with the APU off every register but NR52 is read-only, "except on
; monochrome models" for the length timers.
;
; The model source for the mechanism — zero the counters when the APU is powered
; OFF, and on DMG restore the pulse-channel lengths across the power-ON write —
; is SameBoy, Core/apu.c, the NR52 write handler
; (`memset` on power-off; `if (!GB_is_cgb(gb) && (value & 0x80))` restores the
; pulse lengths captured at handler entry).
;
; PROVENANCE: subtest 1 (what a counter holds after a power cycle with no NRx1
; write at all) is model-derived and queued for hardware-bench confirmation —
; see the note in apu_power_cycle_len.inc. Subtest 2 is the documented DMG half
; of the rule.
;
; Subtest 1 reads the counter WITHOUT a trigger reload, using the length-enable
; path, which decrements but never reloads: enable length on CH2 with no NR21
; write, let 8 length steps pass, then trigger. A counter that came out of the
; power cycle at 0 is untouched by all of that and the trigger reloads the full
; 64; a counter that came out at 64 is decremented to 56 and the trigger finds
; it non-zero, so it is not reloaded. Expect 64, not 56.
;
; Subtest 2 writes NR21 = $30 while the APU is off, powers on, and triggers with
; no NR21 rewrite. DMG accepts the write, so the counter is 16 and the trigger
; does not reload it: expect 16 (the CGB build of this ROM expects 64 there,
; from the same sequence with the write rejected).
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

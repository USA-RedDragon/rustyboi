; len_extra_clock.cgb.mooneye — a length-enable NRx4 write in the half of the
; length period whose next DIV-APU step does not clock the length timer must
; clock the timer once immediately; the same write in the other half must not.
; CGB build, single speed throughout.
;
; Pan Docs, Audio Details, Obscure Behavior
; (https://gbdev.io/pandocs/Audio_details.html): "Extra length clocking occurs
; when writing to NRx4 when the DIV-APU next step is one that doesn't clock the
; length timer. In this case, if the length timer was PREVIOUSLY disabled and
; now enabled and the length timer is not zero, it is decremented." The same
; paragraph's CGB-02 note only widens the condition (the current enable state
; stops mattering); the disabled->enabled transition probed here extra-clocks
; on every revision, so the expectations match CGB-02 and CGB-04/05 alike.
;
; Both halves are probed on CH2 from a DIV-pinned APU power-on; the ROM
; measures ticks-until-NR52-bit-1-clears after a trigger and grades both phase
; cases (full derivation in apu_len_extra_clock.inc).

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "apu.inc"
INCLUDE "apu_len_extra_clock.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    len_extra_clock_test

; hdma_ff55_countdown_ss.cgb.mooneye — HBlank-DMA FF55 countdown, single speed.
;
; Exactly one $10 block per HBlank: an 8-block HBlank DMA armed during VBlank
; must read back 06,05,04,03,02,01,00,FF on FF55, stepping once per scanline,
; with the $00 step persisting a full line before $FF (Pan Docs, "LCD VRAM DMA
; Transfers"). Guards the rise/STAT-fallback double-fire that stepped the
; count by 2 per line and made $00 unobservable (the Stuart Little middleware
; family's boot spin on FF55 & $7F == 0).

INCLUDE "hardware.inc"
INCLUDE "rustyboi_test.inc"
INCLUDE "hdma_ff55_countdown.inc"

SECTION "entry", ROM0[$100]
    di
    jp Start

SECTION "main", ROM0[$150]
Start:
    hdma_ff55_countdown_test

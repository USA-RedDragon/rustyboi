; hardware.inc — minimal Game Boy I/O register and bit definitions.

IF !DEF(HARDWARE_INC)
DEF HARDWARE_INC EQU 1

; --- I/O registers -----------------------------------------------------------
DEF rP1    EQU $FF00   ; joypad
DEF rSB    EQU $FF01   ; serial data
DEF rSC    EQU $FF02   ; serial control
DEF rDIV   EQU $FF04
DEF rIF    EQU $FF0F   ; interrupt flag
DEF rLCDC  EQU $FF40   ; LCD control
DEF rSTAT  EQU $FF41   ; LCD status
DEF rSCY   EQU $FF42
DEF rSCX   EQU $FF43
DEF rLY    EQU $FF44   ; LCD Y (current scanline)
DEF rLYC   EQU $FF45
DEF rBGP   EQU $FF47   ; BG palette (DMG)
DEF rWY    EQU $FF4A   ; window Y
DEF rWX    EQU $FF4B   ; window X (+7)
DEF rKEY1  EQU $FF4D   ; CGB speed switch
DEF rVBK   EQU $FF4F   ; CGB VRAM bank
DEF rHDMA1 EQU $FF51   ; HDMA source high
DEF rHDMA2 EQU $FF52   ; HDMA source low
DEF rHDMA3 EQU $FF53   ; HDMA dest high
DEF rHDMA4 EQU $FF54   ; HDMA dest low
DEF rHDMA5 EQU $FF55   ; HDMA length/mode/start
DEF rIE    EQU $FFFF   ; interrupt enable

; --- LCDC bits ---------------------------------------------------------------
DEF LCDCF_ON      EQU %10000000 ; LCD on
DEF LCDCF_WIN9C00 EQU %01000000 ; window map $9C00 (else $9800)
DEF LCDCF_WINON   EQU %00100000 ; window enable
DEF LCDCF_BG8000  EQU %00010000 ; BG/window tile data $8000 (else $8800)
DEF LCDCF_BG9C00  EQU %00001000 ; BG map $9C00 (else $9800)
DEF LCDCF_OBJ16   EQU %00000100 ; 8x16 sprites
DEF LCDCF_OBJON   EQU %00000010 ; sprite enable
DEF LCDCF_BGON    EQU %00000001 ; BG/window enable

; --- STAT ---------------------------------------------------------------------
DEF STATF_LYC     EQU %01000000 ; LY==LYC interrupt source
DEF STATF_MODE2   EQU %00100000 ; mode 2 (OAM) interrupt source
DEF STATF_MODE1   EQU %00010000 ; mode 1 (VBlank) interrupt source
DEF STATF_MODE0   EQU %00001000 ; mode 0 (HBlank) interrupt source
DEF STATF_BUSY    EQU %00000011 ; current mode mask

; --- IE/IF bits --------------------------------------------------------------
DEF IEF_VBLANK    EQU %00000001
DEF IEF_STAT      EQU %00000010

; --- HDMA5 --------------------------------------------------------------------
DEF HDMA5F_HBLANK EQU %10000000 ; bit 7 set = HBlank DMA (else general DMA)
DEF HDMA5F_BUSY   EQU %10000000 ; read-back: bit 7 clear = HBlank DMA active

ENDC

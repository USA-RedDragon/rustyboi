; lcd_enable_pattern.inc — the shared signature pattern for the
; lcd_enable_frame_* ROM family (ppu/lcd_enable_frame_repeat.cgb,
; lcd_enable_frame_after.cgb, lcd_enable_frame_blank.dmg).
;
; The pattern is deliberately NON-UNIFORM (4 tile shapes x 4 palettes across
; the whole map) so the png oracles discriminate every plausible presentation
; failure: an all-white blank, a wrongly presented zeroed (all-black) back
; buffer, and any partial or shifted render all mismatch. The derived-oracle
; generator recomputes these exact constants from the layout formulas; keep
; the two in byte-parity:
;   tile = (tx+ty)&3, palette = (tx^ty)&3 (CGB attr bank 1), SCX=SCY=0
;   tile 0 solid c3 / tile 1 2px vstripes 1,1,2,2 / tile 2 hbands 1,2,3,0
;   tile 3 diagonal c3
;
; Define PATTERN_CGB before INCLUDE to paint the CGB attribute map and the
; PalA palette table (and to get WritePalB for the post-skip ROM). Without
; it, only tiles + map are painted (DMG: set BGP in the ROM). Include from
; inside a ROM0 code section, after the final `test_success`.

; Paint the signature pattern with the LCD off: tiles 0-3 at $8000, the full
; $9800 map, and (PATTERN_CGB) the bank-1 attribute map + PalA. Clobbers
; A/C/D/E/HL.
PaintSignature:
    ; Tiles 0-3: 64 bytes at $8000.
    ld de, TileData
    ld hl, $8000
    ld c, 64
.tiles:
    ld a, [de]
    inc de
    ld [hl+], a
    dec c
    jr nz, .tiles

    ; Map $9800..$9BFF: tile index = (tx+ty)&3.
    ld hl, $9800
    ld d, 0
.map_row:
    ld e, 0
.map_col:
    ld a, d
    add e
    and 3
    ld [hl+], a
    inc e
    ld a, e
    cp 32
    jr nz, .map_col
    inc d
    ld a, d
    cp 32
    jr nz, .map_row

IF DEF(PATTERN_CGB)
    ; Attribute map (VRAM bank 1): palette = (tx^ty)&3, all other bits 0.
    ld a, 1
    ldh [rVBK], a
    ld hl, $9800
    ld d, 0
.attr_row:
    ld e, 0
.attr_col:
    ld a, d
    xor e
    and 3
    ld [hl+], a
    inc e
    ld a, e
    cp 32
    jr nz, .attr_col
    inc d
    ld a, d
    cp 32
    jr nz, .attr_row
    xor a
    ldh [rVBK], a

    ; BG palettes 0-3 = PalA (auto-increment).
    ld de, PalA
    call WritePalettes
ENDC

    ; The pattern must not scroll.
    xor a
    ldh [rSCY], a
    ldh [rSCX], a
    ret

IF DEF(PATTERN_CGB)
; Write 32 palette bytes from DE to BCPD with auto-increment. Safe with the
; LCD off or during VBlank (mode 1). Clobbers A/C/DE.
WritePalettes:
    ld a, $80
    ldh [rBCPS], a
    ld c, 32
.loop:
    ld a, [de]
    inc de
    ldh [rBCPD], a
    dec c
    jr nz, .loop
    ret

; Swap to the second palette table (PalA reversed per palette; every color
; entry differs from PalA). The CGB ROMs call this during the LCD-off window
; so the skipped post-enable frame renders in never-displayed colors: the
; repeat ROM's PalA oracle then rejects any presentation of the in-flight
; render or back buffer, and the after ROM's PalB oracle rejects a panel
; stuck repeating the PalA image.
WritePalB:
    ld de, PalB
    jr WritePalettes
ENDC

; Wait for a fresh VBlank edge: spin until LY leaves 144, then until it returns.
WaitVBlankEdge:
.not144:
    ldh a, [rLY]
    cp 144
    jr z, .not144
.to144:
    ldh a, [rLY]
    cp 144
    jr nz, .to144
    ret

; 4 tiles x 16 bytes 2bpp (low, high per row).
TileData:
    DB $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF, $FF
    DB $CC, $33, $CC, $33, $CC, $33, $CC, $33, $CC, $33, $CC, $33, $CC, $33, $CC, $33
    DB $FF, $00, $FF, $00, $00, $FF, $00, $FF, $FF, $FF, $FF, $FF, $00, $00, $00, $00
    DB $80, $80, $40, $40, $20, $20, $10, $10, $08, $08, $04, $04, $02, $02, $01, $01

IF DEF(PATTERN_CGB)
; BGR555 little-endian, 4 palettes x 4 colors.
PalA:
    DB $1F, $00, $E0, $03, $00, $7C, $FF, $03
    DB $1F, $7C, $E0, $7F, $EF, $3D, $FF, $01
    DB $0C, $60, $FF, $7F, $00, $00, $EF, $3F
    DB $E0, $7D, $9F, $31, $EF, $01, $E0, $3F
; PalA with the color order reversed inside each palette.
PalB:
    DB $FF, $03, $00, $7C, $E0, $03, $1F, $00
    DB $FF, $01, $EF, $3D, $E0, $7F, $1F, $7C
    DB $EF, $3F, $00, $00, $FF, $7F, $0C, $60
    DB $E0, $3F, $EF, $01, $9F, $31, $E0, $7D
ENDC

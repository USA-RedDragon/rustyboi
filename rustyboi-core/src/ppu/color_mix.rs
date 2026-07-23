use crate::memory::mmio;
use crate::memory::Addressable;
use super::controller::{
    lcdc_has, ColorCorrection, LCDCFlags, Ppu, Sprite, SpriteAttributes, SpriteFetchPhase,
    SpriteFetchRec, LY, MAX_SPRITES_PER_LINE, OAMDMA_CHANGE_CC_OFFSET, OAM_BYTES_PER_SPRITE,
    OAM_SPRITE_COUNT, OBJ_READ_HIGH_BACK, OBJ_READ_HIGH_BACK_CGB, OBJ_READ_LOW_BACK,
    OBJ_READ_LOW_BACK_CGB, SCX,
};

/// Game Boy Advance LCD colour curve as a 15-bit-colour -> RGB888 table, built
/// once. Ported from ares' `GameBoyAdvance` `color()` (ISC-licensed; Talarubi &
/// byuu's measured GBA characterisation): lcdGamma 4.0, outGamma 2.2, the
/// channel-mix matrix, scaled to 8-bit. Built with pure-Rust `libm` so the
/// table is bit-identical on every platform — the AGB frame output must stay
/// machine-independent for the deterministic regression gate.
fn agb_lcd_lut() -> &'static [[u8; 3]; 32768] {
    static LUT: std::sync::OnceLock<Box<[[u8; 3]; 32768]>> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = Box::new([[0u8; 3]; 32768]);
        let (lcd_gamma, out_gamma) = (4.0f64, 2.2f64);
        let scale = 255.0 * 255.0 / 280.0;
        for (word, slot) in lut.iter_mut().enumerate() {
            let lr = libm::pow((word & 0x1F) as f64 / 31.0, lcd_gamma);
            let lg = libm::pow(((word >> 5) & 0x1F) as f64 / 31.0, lcd_gamma);
            let lb = libm::pow(((word >> 10) & 0x1F) as f64 / 31.0, lcd_gamma);
            let ch = |mix: f64| -> u8 {
                (libm::pow(mix / 255.0, 1.0 / out_gamma) * scale).round().clamp(0.0, 255.0) as u8
            };
            *slot = [
                ch(50.0 * lg + 240.0 * lr),
                ch(30.0 * lb + 230.0 * lg + 10.0 * lr),
                ch(220.0 * lb + 10.0 * lg + 50.0 * lr),
            ];
        }
        lut
    })
}

impl Ppu {
    /// DMG-compatibility mode on CGB hardware: a DMG cart running on a CGB
    /// (`is_cgb()` true, but CGB features OFF because the cart is not CGB-aware).
    /// The PPU still produces RGB color output, indexing the boot ROM's
    /// DMG-compat palette in CGB palette RAM via BGP/OBP shade remap.
    pub(in crate::ppu) fn is_cgb_compat_dmg(&self, mmio: &mmio::Mmio) -> bool {
        mmio.is_cgb() && !mmio.is_cgb_features_enabled()
    }

    /// True when this frame should be rendered to the RGB color framebuffer:
    /// either full CGB mode or DMG-compat-on-CGB.
    pub(crate) fn renders_color(&self, mmio: &mmio::Mmio) -> bool {
        mmio.is_cgb_features_enabled() || self.is_cgb_compat_dmg(mmio)
    }

    // BG palette shade for color index `idx` at display column `sx`. On CGB hardware
    // resolves BGP per column from `bgp_history` so a mid-mode-3 BGP write remaps only
    // the pixels drawn at/after its apply column (the DMG-compat-on-CGB path). On DMG
    // hardware the per-dot `bgp_delayed` latch (refreshed at the end of every dot,
    // with a phase-dependent hold for late-phase writes — see `on_bgp_write`) yields
    // the exact DMG latch column, so DMG keeps it. With no mid-line write the CGB
    // history is a single seed == the delayed register, so the steady-state output is
    // identical either way.
    pub(crate) fn get_palette_color(&self, mmio: &mmio::Mmio, idx: u8, sx: u8) -> u8 {
        let bgp = if mmio.is_cgb() {
            Self::pal_at(&self.bgp_history, sx, self.bgp_delayed)
        } else {
            self.bgp_delayed
        };
        Self::bgp_shade(bgp, idx)
    }

    // As `get_palette_color` but resolves BGP at the pixel's pop DOT rather than its
    // display column. Used by the CGB / DMG-compat BG color path: a sprite-fetch
    // stall between a BGP write and a column delays that column's pop, so the
    // dot-space model (write applies at `ticks+latency`; pixel pops later) is exact
    // where the column model over/under-shoots.
    pub(crate) fn get_palette_color_at_tick(&self, idx: u8, pop_tick: u128) -> u8 {
        let bgp = Self::pal_at_tick(&self.bgp_dot_history, pop_tick, self.bgp_delayed);
        Self::bgp_shade(bgp, idx)
    }

    fn bgp_shade(bgp: u8, idx: u8) -> u8 {
        match idx {
            0 => bgp & 0x03,
            1 => (bgp >> 2) & 0x03,
            2 => (bgp >> 4) & 0x03,
            3 => (bgp >> 6) & 0x03,
            _ => 0x00,
        }
    }

    // Sprite palette shade at display column `sx` (CGB: per-pixel OBP sample from the
    // true-color palette-RAM pipeline). Used by the
    // CGB and DMG-compat sprite mixers. DMG-hardware sprites use
    // `dmg_sprite_palette_shade` (a per-SPRITE latch, not per-pixel).
    pub(crate) fn get_sprite_palette_color(&self, _mmio: &mmio::Mmio, idx: u8, palette: bool, sx: u8) -> u8 {
        if idx == 0 {
            return 0; // Transparent for sprites
        }
        let obp = if palette {
            Self::pal_at(&self.obp1_history, sx, self.obp1_delayed)
        } else {
            Self::pal_at(&self.obp0_history, sx, self.obp0_delayed)
        };
        Self::obp_shade(obp, idx)
    }

    // DMG sprite shade: OBP is sampled at the pixel's POP DOT (the OAM-FIFO
    // pop reads the register live), via the dot-keyed history — the column
    // model diverges wherever a sprite stall delays the pops, and the pop-dot
    // model naturally covers the off-left-edge sprites (their pixels pop before
    // any mid-mode-3 write applies).
    fn dmg_sprite_palette_shade(&self, idx: u8, palette: bool, pop_tick: u128) -> u8 {
        if idx == 0 {
            return 0; // Transparent for sprites
        }
        let hist = if palette { &self.obp1_dot_history } else { &self.obp0_dot_history };
        let fallback = if palette { self.obp1_delayed } else { self.obp0_delayed };
        let obp = Self::pal_at_tick(hist, pop_tick, fallback);
        Self::obp_shade(obp, idx)
    }

    #[inline]
    fn obp_shade(obp: u8, idx: u8) -> u8 {
        match idx {
            1 => (obp >> 2) & 0x03, // Light Gray
            2 => (obp >> 4) & 0x03, // Dark Gray
            3 => (obp >> 6) & 0x03, // Black
            _ => 0x00,              // Default to transparent for invalid indices
        }
    }

    // CGB color conversion functions. `is_agb`: under `Lcd`, GBA hardware uses
    // its own (dimmer, warmer) LCD curve instead of the CGB matrix.
    pub(in crate::ppu) fn cgb_color_to_rgb(&self, low_byte: u8, high_byte: u8, is_agb: bool) -> (u8, u8, u8) {
        // CGB color format: GGGRRRRR BBBBBGGG (little endian)
        let color_word = (high_byte as u16) << 8 | low_byte as u16;

        // Extract 5-bit RGB components
        let r = color_word & 0x1F ;
        let g = (color_word >> 5) & 0x1F ;
        let b = (color_word >> 10) & 0x1F ;

        match self.cgb_color_conversion {
            ColorCorrection::Linear => {
                let r8 = ((r * 255) / 31) as u8;
                let g8 = ((g * 255) / 31) as u8;
                let b8 = ((b * 255) / 31) as u8;
                (r8, g8, b8)
            }
            ColorCorrection::Lcd if is_agb => {
                let [r8, g8, b8] = agb_lcd_lut()[(color_word & 0x7FFF) as usize];
                (r8, g8, b8)
            }
            ColorCorrection::Lcd => {
                let r8 = ((r * 13 + g * 2 + b) / 2) as u8;
                let g8 = ((g * 3 + b) * 2) as u8;
                let b8 = ((r * 3 + g * 2 + b * 11) / 2) as u8;
                (r8, g8, b8)
            }
        }
    }

    fn get_cgb_bg_color(&self, mmio: &mmio::Mmio, palette_idx: u8, color_idx: u8, sx: u8) -> (u8, u8, u8) {
        if !mmio.is_cgb_features_enabled() {
            // Fallback to monochrome conversion
            let mono_color = self.get_palette_color(mmio, color_idx, sx);
            let intensity = match mono_color {
                0 => 255, // White
                1 => 170, // Light gray
                2 => 85,  // Dark gray
                _ => 0,   // Black
            };
            return (intensity, intensity, intensity);
        }

        // Read CGB palette data from palette RAM
        let (low_byte, high_byte) = mmio.read_bg_palette_data(palette_idx, color_idx);
        self.cgb_color_to_rgb(low_byte, high_byte, mmio.is_agb())
    }

    fn get_cgb_obj_color(&self, mmio: &mmio::Mmio, palette_idx: u8, color_idx: u8, sx: u8) -> (u8, u8, u8) {
        if color_idx == 0 {
            return (0, 0, 0); // Transparent - will be handled by caller
        }

        if !mmio.is_cgb_features_enabled() {
            // Fallback to monochrome conversion
            let mono_color = self.get_sprite_palette_color(mmio, color_idx, palette_idx != 0, sx);
            let intensity = match mono_color {
                0 => 0,   // Transparent
                1 => 170, // Light gray
                2 => 85,  // Dark gray
                _ => 0,   // Black
            };
            return (intensity, intensity, intensity);
        }

        // Read CGB palette data from palette RAM
        let (low_byte, high_byte) = mmio.read_obj_palette_data(palette_idx, color_idx);
        self.cgb_color_to_rgb(low_byte, high_byte, mmio.is_agb())
    }

    // Check a single sprite during distributed OAM search
    pub(in crate::ppu) fn check_single_sprite_for_scanline(&mut self, mmio: &mut mmio::Mmio, sprite_index: usize) {
        // Skip if we already have the maximum sprites for this line
        if self.sprites_on_line.len() >= MAX_SPRITES_PER_LINE {
            return;
        }

        let ly = mmio.read(LY);

        // OAM scan (the hardware sprite mapper) builds the per-line
        // sprite list regardless of the OBJ-enable bit (LCDC.1). The enable bit
        // only gates the M3 sprite fetch and the final pixel mix, so a sprite
        // enabled mid-mode-3 still incurs its fetch penalty. Do not early-out
        // here on OBJ-disable.

        // Determine sprite height (8x8 or 8x16). Use the per-line scan latch
        // (lags the live LCDC by one OAM slot) so a mid-mode-2 OBJ-size write
        // affects only entries scanned strictly after it commits, matching
        // the hardware per-entry size latch.
        let large = self.scan_obj_size_large;
        let sprite_height = if large { 16 } else { 8 };

        let oam_offset = sprite_index * OAM_BYTES_PER_SPRITE;
        let sprite_y = mmio.read(0xFE00 + oam_offset as u16);
        let sprite_x = mmio.read(0xFE00 + oam_offset as u16 + 1);
        let tile_index = mmio.read(0xFE00 + oam_offset as u16 + 2);
        let attributes_byte = mmio.read(0xFE00 + oam_offset as u16 + 3);

        // Sprites use offset coordinates: Y=0 is at line -16, X=0 is at column -8.
        // Compare widened (no u8 wrap): a sprite with y < 16 straddles the top
        // screen edge and is visible on lines 0 .. y+height-17 (hardware scans
        // LY+16 against [y, y+height)).
        let top = sprite_y as i32 - 16;

        // Check if sprite is visible on current scanline
        if (ly as i32) >= top && (ly as i32) < top + sprite_height {
            let sprite = Sprite {
                y: sprite_y,
                x: sprite_x,
                tile_index,
                attributes: SpriteAttributes::from_byte(attributes_byte),
                oam_index: sprite_index as u8,
            };

            self.sprites_on_line.push(sprite);
        }
    }

    /// Per-dot driver for the lazy OAM sprite snapshot. Mirrors the hardware
    /// `OAM-DMA start`/`OAM-DMA end`/`OAM change` plus the implicit `update(cc)` the
    /// mode-2 the event dispatch performs. Run after `abs_cc` is folded to the current dot,
    /// before the mode-2 scan reads the snapshot.
    /// Per-dot gate for the OAM snapshot snoop. Inlined so the common no-event
    /// dot pays one flag compare — the outlined body's stack frame (the
    /// 80-byte pos buffer) was the hot loop's single biggest fixed cost.
    #[inline]
    pub(in crate::ppu) fn process_oam_reader_events(&mut self, mmio: &mut mmio::Mmio) {
        // Fast path: with no OAM-DMA active, no pending CPU OAM write, no DMA
        // window seen last dot, and the snapshot already seeded, neither
        // `change()` trigger in the body can fire.
        if self.oam_reader_seeded
            && !self.prev_dma_writing
            && !mmio.oam_snoop_event_possible()
        {
            return;
        }
        self.process_oam_reader_events_slow(mmio);
    }

    fn process_oam_reader_events_slow(&mut self, mmio: &mut mmio::Mmio) {
        let cc = self.abs_cc;

        // Lazy seed for the current LCD-on session.
        if !self.oam_reader_seeded {
            let cgb = mmio.is_cgb_features_enabled();
            let mut pos = [0u8; 80];
            mmio.peek_oam_pos(&mut pos);
            self.oam_reader.reset(&pos, cgb);
            self.oam_reader.lu = cc;
            self.oam_reader.large_src = self.lcdc_has(LCDCFlags::SpriteSize);
            self.prev_dma_writing =
                mmio.oam_dma_window_active() && !mmio.mgb_frozen_merge_active();
            self.oam_reader_seeded = true;
            return;
        }

        // Keep large-sprites source tracking the live LCDC OBJ-size bit (hardware
        // sets it on the LCDC write; the walk latches it into lsbuf per slot).
        self.oam_reader.large_src = self.lcdc_has(LCDCFlags::SpriteSize);

        // `pos` (the 80-byte Y/X snapshot) is only consumed by the `change()`
        // calls below, which fire only on a DMA-window edge or a pending OAM
        // write. The common per-dot case hits neither, so build it lazily.
        let mut pos = [0u8; 80];
        let mut pos_filled = false;

        // OAM-DMA window edges: at start the source becomes disabled RAM (0xFF);
        // at end it returns to the real OAM. `change(cc)` flushes the snapshot up
        // to `cc` with the OLD source, then caps the next walk, then we toggle.
        // The MGB OAM-DMA-during-HALT merge freezes the DMA mid-transfer; the
        // frozen OAM bus is stuck (not the normal disabled-RAM window), so the
        // Y/X scan reads the merged OAM rather than the ghost pair. Treat the
        // merge window as a non-writing (readable) source.
        let dma_writing = mmio.oam_dma_window_active() && !mmio.mgb_frozen_merge_active();
        if dma_writing != self.prev_dma_writing {
            let lc = self.ly_counter(mmio);
            mmio.peek_oam_pos(&mut pos);
            pos_filled = true;
            // The DMA window edge is observed at the PPU dot, but hardware fires
            // OAM-DMA start/OAM-DMA end at the M-cycle's master cc, which precedes the
            // PPU's observation by a fixed sub-M-cycle amount. Shift the change cc
            // back by this offset so the position-walk cap lands on the same OAM
            // slot hardware does. Calibrated against the late_sp{00,01,39}x/y
            // `_1`/`_2` and `_ds_1`/`_ds_2` bracket pairs (which straddle this
            // boundary); scaled by the speed so it is a fixed line cycle amount.
            let cc = cc.saturating_sub((OAMDMA_CHANGE_CC_OFFSET as u64) << lc.ds as u32);
            // change() under the pre-toggle source (the hardware OAM change uses the
            // pointer in effect for the just-completed span).
            self.oam_reader.change(cc, &lc, &pos);
            // DMA start: latch the scan's retained Y/X bus pair (the last pair
            // walked before the cap) for the ghost sampling inside the window.
            if dma_writing {
                let line_has_fetches = !self.sprites_on_line.is_empty();
                self.oam_reader.capture_ghost(line_has_fetches);
            }
            // Toggle source for the new span (OAM-DMA start -> disabled,
            // OAM-DMA end -> real OAM).
            self.oam_reader.src_disabled = dma_writing;
            self.prev_dma_writing = dma_writing;
        }

        // CPU OAM write this M-cycle (the hardware OAM change at cc).
        if mmio.take_oam_write_pending() {
            let lc = self.ly_counter(mmio);
            if !pos_filled {
                mmio.peek_oam_pos(&mut pos);
            }
            self.oam_reader.change(cc, &lc, &pos);
        }
        // The snapshot is flushed only at `change` (above) and at the mode-2-end
        // `the event dispatch` (build_sprites_from_snapshot). A per-dot flush would consume
        // the `last_change` cap before the DMA-start `change`, losing the
        // load-bearing `_1`/`_2` bracket distinction.
    }

    /// Flush the snapshot to the mode-2-end cc (the hardware OAM-scan-end event's
    /// `the OAM reader.update(time)`), then rebuild `sprites_on_line` from the posbuf
    /// in one pass (sprite mapping). Replaces the per-dot live OAM scan.
    pub(in crate::ppu) fn build_sprites_from_snapshot(&mut self, mmio: &mut mmio::Mmio) {
        let lc = self.ly_counter(mmio);
        let cc = self.abs_cc;
        // Re-derive the walk's OBJ-size source here (the per-dot refresh in
        // `process_oam_reader_events` is skipped on its no-event fast path).
        // `lcdc` is constant within a dot, so this matches the old per-dot value.
        self.oam_reader.large_src = self.lcdc_has(LCDCFlags::SpriteSize);
        let mut pos = [0u8; 80];
        mmio.peek_oam_pos(&mut pos);
        self.oam_reader.update(cc, &lc, &pos);

        self.sprites_on_line.clear();
        let ly = mmio.read(LY);
        for i in 0..OAM_SPRITE_COUNT {
            if self.sprites_on_line.len() >= MAX_SPRITES_PER_LINE {
                break;
            }
            let sprite_y = self.oam_reader.buf[2 * i];
            let sprite_x = self.oam_reader.buf[2 * i + 1];
            // Per-sprite OBJ size from the calibrated incremental scan (preserves
            // the late_sizechange per-slot size-latch timing); the snapshot only
            // governs Y/X visibility.
            let large = self.scan_slot_large[i];
            let sprite_height: u8 = if large { 16 } else { 8 };
            // Widened compare (no u8 wrap): y < 16 sprites straddle the top
            // screen edge and are visible on lines 0 .. y+height-17 (hardware
            // scans LY+16 against [y, y+height); windesync-validate's y=15
            // strike-tip erase sprite).
            let top = sprite_y as i32 - 16;
            if (ly as i32) >= top && (ly as i32) < top + sprite_height as i32 {
                // A ghost-sampled slot (Y/X-bus retention during an OAM-DMA
                // window) exists only while the DMA owns OAM; its hardware tile/
                // attribute fetch sees the DMA's in-flight data, so read the live
                // progressively-written OAM rather than the CPU view (0xFF while
                // a DMA runs). Real-sampled slots keep the CPU-view read.
                let (tile_index, attributes_byte) = if let Some(ta) =
                    mmio.mgb_frozen_oam_tile_attr(i as u8)
                {
                    // MGB OAM-DMA-during-HALT merge: the frozen OAM bus feeds the
                    // PPU merged tile/attr for this entry (see mmio).
                    ta
                } else if self.oam_reader.ghost_slot[i] {
                    (
                        mmio.ppu_read_oam_live(0xFE00 + (i as u16) * 4 + 2),
                        mmio.ppu_read_oam_live(0xFE00 + (i as u16) * 4 + 3),
                    )
                } else {
                    (
                        mmio.read(0xFE00 + (i as u16) * 4 + 2),
                        mmio.read(0xFE00 + (i as u16) * 4 + 3),
                    )
                };
                self.sprites_on_line.push(Sprite {
                    y: sprite_y,
                    x: sprite_x,
                    tile_index,
                    attributes: SpriteAttributes::from_byte(attributes_byte),
                    oam_index: i as u8,
                });
            }
        }
        // Ghost propagation stop: any sprite fetched on THIS line while the DMA
        // window is still open rewrites the Y bus with a mid-DMA tile byte
        // (on hardware a mid-DMA sprite fetch clobbers the Y bus), so the retained scan pair does not survive
        // into the NEXT line's walk (strikethrough: the ghost bar renders on
        // line 68 only; line 69's scan — still inside the ~1.4-line window —
        // sees the clobbered bus and stays clean).
        if self.oam_reader.src_disabled && !self.sprites_on_line.is_empty() {
            self.oam_reader.ghost = (0xFF, 0xFF);
        }
    }

    // A sprite whose fetch has not yet run and whose x-match column is `col`
    // (it will arm a pixel-pop stall when the pipeline head reaches that
    // column). Mirrors `sprite_fetch_penalty_for_current_x`'s trigger match;
    // used by the DMG stall-aware LCDC.0 boundary.
    pub(in crate::ppu) fn dmg_unfetched_sprite_at(&self, col: u8) -> bool {
        if !self.lcdc_has(LCDCFlags::SpriteDisplayEnable) {
            return false;
        }
        self.sprites_on_line
            .get(self.next_sprite_fetch_index..)
            .unwrap_or(&[])
            .iter()
            .any(|s| s.x.saturating_sub(8) == col)
    }

    pub(in crate::ppu) fn sprite_fetch_penalty_for_current_x(&mut self, mmio: &mmio::Mmio) -> Option<u8> {
        let lcdc = self.lcdc;
        if !lcdc_has(lcdc, LCDCFlags::SpriteDisplayEnable) && !mmio.is_cgb_features_enabled() {
            return None;
        }

        while self.next_sprite_fetch_index < self.sprites_on_line.len() {
            let sprite_x = self.sprites_on_line[self.next_sprite_fetch_index].x;
            let trigger_x = sprite_x.saturating_sub(8);

            if trigger_x < self.x {
                // The sprite's x-match dot passed without a fetch (OBJ was
                // disabled when the head crossed it): dropped for the line —
                // no stall, and (DMG) its pixels never reach the mixer.
                if let Some(rec) = self
                    .sprite_fetch_recs
                    .get_mut(self.next_sprite_fetch_index)
                    && rec.phase == SpriteFetchPhase::Pending
                {
                    rec.phase = SpriteFetchPhase::Aborted;
                }
                self.next_sprite_fetch_index += 1;
                continue;
            }

            if trigger_x > self.x {
                return None;
            }

            self.next_sprite_fetch_index += 1;
            // Record the dot this sprite's stall arms (its first dot is consumed this
            // tick) so the OBJ-disable recompute can refund the not-yet-counted-down
            // remainder of an in-progress sprite (see `remaining_sprite_cost`).
            self.m3_last_sprite_commit_tick = self.ticks;

            // Same per-object tile-walk cost the length model uses (see
            // `sprite_tile_walk_cost`): the FIRST sprite in each BG tile costs
            // `max(11 - dist, 6)`; every further sprite sharing that tile costs a
            // flat 6. On DMG `dist = (spx + scx) & 7` — the raw
            // OAM x, NOT the clamped trigger column: a left-clipped sprite
            // (spx 1..7) matches during the first-tile prologue and costs
            // max(11-spx, 6) (i.e. 10,9,8,7,6,6,6 for spx 1..7; a `self.x`-based
            // dist would collapse them all to 11). On CGB keep the clamped-trigger
            // dist: left-clipped sprites pay the full 11-dot stall there. For spx >= 8 the
            // two are congruent mod 8, and the tile id differs from the
            // closed-form's `(spx - first-tile xpos) & -8` only by a per-line
            // constant, so the equality grouping (first-vs-rest) is identical.
            let scx = mmio.read(SCX);
            let dist_x = if mmio.is_cgb_features_enabled() { self.x } else { sprite_x };
            let pixel_in_tile = dist_x.wrapping_add(scx) & 0x07;
            let tile_no = (dist_x as i32 + scx as i32) & !7;
            let first_in_tile = tile_no != self.m3_sprite_prev_tile;
            self.m3_sprite_prev_tile = tile_no;

            let penalty = if sprite_x == 0 {
                11
            } else if first_in_tile {
                // pixel_in_tile 0..7 -> leading rate 11,10,9,8,7,6,6,6
                // (= max(11-dist,6)); a non-leading sprite in the same tile is
                // always a flat 6.
                let wait_for_bg_fetch = (7u8 - pixel_in_tile).saturating_sub(2);
                wait_for_bg_fetch + 6
            } else {
                6
            };
            // Per-sprite fetch record: a left-clipped sprite (spx < 8) matched
            // (8 - spx) dots before the head reached column 0 (during the
            // first-tile prologue), so its byte-fetch dots are earlier by that
            // amount than the arm tick observed here.
            if let Some(rec) = self
                .sprite_fetch_recs
                .get_mut(self.next_sprite_fetch_index - 1)
            {
                let left_adj = (8u128).saturating_sub(sprite_x as u128).min(self.ticks);
                rec.phase = SpriteFetchPhase::Fetched;
                rec.arm_tick = self.ticks - if sprite_x < 8 { left_adj } else { 0 };
                rec.penalty = penalty;
            }
            return Some(penalty);
        }

        None
    }

    // Mix background pixel with sprites at the given screen coordinates (CGB color version)
    pub(in crate::ppu) fn mix_background_and_sprites_color(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, bg_attrs: u8, screen_x: u8, screen_y: u8, bg_enabled_col: bool) -> (u8, u8, u8) {
        let lcdc = self.lcdc;
        // Per-pixel BG-master-priority: on CGB, LCDC.0 off keeps BG/window
        // visible but drops BG master priority over sprites for this column
        // (the hardware BG-priority mask `lcdc << 7`, evaluated live per tile). Use
        // the column's BG-enable rather than the final once-per-line value.
        let bg_priority_master = bg_enabled_col;

        // Background attributes captured at fetch time travel with the pixel.
        let tile_attributes = bg_attrs;
        let palette_idx = tile_attributes & 0x07; // Bits 0-2 = palette index
        let bg_color_rgb = self.get_cgb_bg_color(mmio, palette_idx, bg_pixel_idx, screen_x);

        // Check if sprites are enabled
        if !lcdc_has(lcdc, LCDCFlags::SpriteDisplayEnable) {
            return bg_color_rgb;
        }

        // First, resolve object-to-object priority to find the highest priority opaque sprite pixel
        let mut selected_sprite: Option<(&Sprite, u8, (u8, u8, u8))> = None; // (sprite, pixel_idx, color)

        for sprite in &self.sprites_on_line {
            // Sprite X coordinate is offset by 8, Y coordinate is offset by 16
            let sprite_actual_x = sprite.x as i16 - 8;
            let sprite_actual_y = sprite.y as i16 - 16;

            // Check if this screen pixel is within the sprite bounds
            let relative_x = screen_x as i16 - sprite_actual_x;
            let relative_y = screen_y as i16 - sprite_actual_y;

            // Sprite is 8 pixels wide
            if (0..8).contains(&relative_x) {
                let sprite_height = if lcdc_has(lcdc, LCDCFlags::SpriteSize) { 16 } else { 8 };
                if relative_y >= 0 && relative_y < sprite_height as i16 {
                    // Get sprite pixel data
                    if let Some(sprite_pixel_idx) = self.get_sprite_pixel(mmio, sprite, relative_x as u8, relative_y as u8)
                        && sprite_pixel_idx != 0 { // Sprite pixel is not transparent

                            // Get sprite palette (in CGB mode, sprite attributes can specify palette)
                            let sprite_palette_idx = if mmio.is_cgb_features_enabled() {
                                // CGB mode: Use bits 2-0 for palette selection (0-7)
                                sprite.attributes.raw & 0x07
                            } else {
                                // DMG mode: Use bit 4 for palette selection (0-1)
                                if sprite.attributes.palette { 1 } else { 0 }
                            };

                            let sprite_color_rgb = self.get_cgb_obj_color(mmio, sprite_palette_idx, sprite_pixel_idx, screen_x);

                            // Check if this sprite has higher priority than the currently selected one
                            let is_higher_priority = if let Some((current_sprite, _, _)) = selected_sprite {
                                if mmio.is_cgb_features_enabled() {
                                    // CGB mode: Only OAM position matters (lower index = higher priority)
                                    sprite.oam_index < current_sprite.oam_index
                                } else {
                                    // DMG mode: X coordinate first, then OAM position
                                    sprite.x < current_sprite.x ||
                                    (sprite.x == current_sprite.x && sprite.oam_index < current_sprite.oam_index)
                                }
                            } else {
                                true // First opaque sprite found
                            };

                            if is_higher_priority {
                                selected_sprite = Some((sprite, sprite_pixel_idx, sprite_color_rgb));
                            }
                        }
                }
            }
        }

        // Now resolve BG vs OBJ priority using the selected sprite (if any)
        if let Some((sprite, _, sprite_color_rgb)) = selected_sprite {
            if mmio.is_cgb_features_enabled() {
                // CGB priority rules
                // If BG color index is 0, OBJ always has priority
                if bg_pixel_idx == 0 {
                    return sprite_color_rgb;
                }

                // In CGB mode LCDC bit 0 keeps BG/window visible, but disables BG priority over OBJ.
                if !bg_priority_master {
                    return sprite_color_rgb;
                }

                // Check BG attributes bit 7 and OAM attributes bit 7
                let bg_priority = (tile_attributes & 0x80) != 0; // BG attr bit 7
                let obj_priority = sprite.attributes.priority;   // OAM attr bit 7 (note: priority=true means "behind BG")

                // If both BG and OAM attributes have bit 7 clear, OBJ has priority
                // Otherwise, BG has priority (when BG color is 1-3)
                if !bg_priority && !obj_priority {
                    return sprite_color_rgb; // OBJ priority
                } else {
                    return bg_color_rgb; // BG priority for colors 1-3
                }
            } else {
                // DMG mode: Simple priority check
                if !sprite.attributes.priority || bg_pixel_idx == 0 {
                    return sprite_color_rgb;
                }
            }
        }

        bg_color_rgb
    }

    /// DMG-compat-on-CGB pixel mix. Uses the DMG palette/priority rules (BGP/OBP
    /// shade remap, DMG sprite X-then-OAM priority, single OBP-selected palette),
    /// but resolves the final shade through CGB palette RAM so the output is the
    /// boot ROM's DMG-compat color instead of grayscale. The shade->RGB lookups
    /// read BG palette 0 and OBJ palette 0/1 (the slots the boot ROM fills).
    // BG-only CGB-compat color for a BG color index (no sprite mix): the shade
    // via BGP then BG palette 0 in CGB palette RAM. Used to detect BG-won columns
    // and to re-plot them in cgb_train_reresolve.
    pub(in crate::ppu) fn compat_bg_color(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8) -> (u8, u8, u8) {
        let bg_shade = self.get_palette_color_at_tick(bg_pixel_idx, self.ticks);
        let (lo, hi) = mmio.bg_palette_pair_raw(0, bg_shade);
        self.cgb_color_to_rgb(lo, hi, mmio.is_agb())
    }

    pub(in crate::ppu) fn mix_background_and_sprites_compat(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, screen_x: u8, screen_y: u8, bg_enabled_col: bool) -> (u8, u8, u8) {
        let bg_enabled = bg_enabled_col;

        // BG shade via BGP at this pixel's pop dot, then look up BG palette 0 in CGB
        // palette RAM.
        let idx = if bg_enabled { bg_pixel_idx } else { 0 };
        let bg_shade = self.get_palette_color_at_tick(idx, self.ticks);
        let (lo, hi) = mmio.bg_palette_pair_raw(0, bg_shade);
        let bg_color_rgb = self.cgb_color_to_rgb(lo, hi, mmio.is_agb());

        let effective_bg_pixel_idx = if bg_enabled { bg_pixel_idx } else { 0 };

        // The DMG-compat renderer runs on CGB hardware but through the same
        // fetch/FIFO machinery, so every DMG mid-mode-3 sprite consumer applies
        // here too — only the final color lookup differs (CGB palette RAM vs
        // grayscale). The one exception is the stale-FIFO pop quirk, a DMG-CPU
        // artifact that a CGB in compat mode does not reproduce.
        let stale_pop_quirk = !mmio.is_cgb() || mmio.is_cgb_features_enabled();
        let Some((sprite, sprite_pixel_idx)) = self.first_winning_sprite_pixel(
            mmio,
            screen_x,
            screen_y,
            effective_bg_pixel_idx,
            stale_pop_quirk,
        ) else {
            return bg_color_rgb;
        };

        // DMG-compat: OBP0/OBP1 selected by attr bit 4, shade sampled at THIS
        // pixel's pop dot (dot-keyed history, like the DMG mixer), then the
        // shade is looked up in OBJ palette 0/1 of CGB palette RAM.
        let use_obp1 = sprite.attributes.palette;
        let obj_shade = self.dmg_sprite_palette_shade(sprite_pixel_idx, use_obp1, self.ticks);
        let pal = if use_obp1 { 1 } else { 0 };
        let (slo, shi) = mmio.obj_palette_pair_raw(pal, obj_shade);
        self.cgb_color_to_rgb(slo, shi, mmio.is_agb())
    }

    // Mix background pixel with sprites at the given screen coordinates
    pub(in crate::ppu) fn mix_background_and_sprites(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, screen_x: u8, screen_y: u8, bg_enabled_col: bool) -> u8 {
        // Per-pixel BG-enable: DMG BG-off forces this column's BG layer to white
        // (palette color 0) for the exact span the toggle covers. Use the
        // column's BG-enable from the line history, not the final LCDC.0.
        let bg_enabled = bg_enabled_col;

        // Get background color - if BG display is disabled, force to white (color 0)
        let bg_color = if bg_enabled {
            self.get_palette_color(mmio, bg_pixel_idx, screen_x)
        } else {
            // When BG display is disabled, background becomes white (palette color 0)
            self.get_palette_color(mmio, 0, screen_x)
        };

        // For sprite priority calculation, we need the original bg_pixel_idx
        let effective_bg_pixel_idx = if bg_enabled { bg_pixel_idx } else { 0 };

        // The 15-dot stale-FIFO pop quirk is a DMG-CPU artifact and applies
        // unconditionally on this path.
        let Some((sprite, sprite_pixel_idx)) =
            self.first_winning_sprite_pixel(mmio, screen_x, screen_y, effective_bg_pixel_idx, true)
        else {
            return bg_color;
        };

        if mmio.is_cgb() {
            // CGB: OBP sampled per pixel (true-color palette-RAM pipeline).
            self.get_sprite_palette_color(mmio, sprite_pixel_idx, sprite.attributes.palette, screen_x)
        } else {
            // DMG mid-mode-3 OBP-write model: OBP sampled at this pixel's pop
            // dot from the dot-keyed history (see dmg_sprite_palette_shade).
            self.dmg_sprite_palette_shade(sprite_pixel_idx, sprite.attributes.palette, self.ticks)
        }
    }

    // Get a specific pixel from a sprite's tile data
    // The per-sprite walk shared by the DMG and DMG-compat mixers: scan
    // `sprites_on_line` in list order and return the first sprite whose pixel at
    // (screen_x, screen_y) is opaque AND wins the BG-priority test, with that
    // pixel's colour index. `None` means no sprite contributes and the caller
    // keeps its background colour — which is what both callers did on both the
    // OBJ-disabled fast path and on falling out of the loop.
    //
    // `stale_pop_quirk` carries the ONE behavioural difference between the two
    // callers. The 15-dot stale-FIFO pop quirk is a DMG-CPU artifact: the DMG
    // mixer applies it unconditionally, while the DMG-compat mixer passes
    // `!is_cgb() || is_cgb_features_enabled()` (De Morgan of `!(is_cgb &&
    // !cgb_features_enabled)`) because a CGB running DMG-compat samples LCDC.1
    // at the plain pop dot with no quirk.
    //
    // NOT usable by `mix_background_and_sprites_color`. That mixer resolves
    // object-to-object priority across the WHOLE list (CGB OAM-index order, or
    // DMG x-then-OAM) and only then tests BG priority, where these two
    // early-return on the first opaque sprite that beats BG. That is a different
    // algorithm, not a different colour tail, so it keeps its own walk.
    #[inline(always)]
    fn first_winning_sprite_pixel(
        &self,
        mmio: &mmio::Mmio,
        screen_x: u8,
        screen_y: u8,
        effective_bg_pixel_idx: u8,
        stale_pop_quirk: bool,
    ) -> Option<(&Sprite, u8)> {
        // OBJ-enable gate. With a mid-mode-3 LCDC.1 toggle this line, hardware
        // gates each sprite pixel on the bit AT THAT PIXEL'S pop dot — resolve
        // per column from the history. Otherwise keep the live-LCDC fast path
        // (identical to the single seeded entry).
        let objen_toggled = self.objen_history.len() > 1;
        if !objen_toggled && !self.lcdc_has(LCDCFlags::SpriteDisplayEnable) {
            return None;
        }

        for (spr_i, sprite) in self.sprites_on_line.iter().enumerate() {
            // Mid-mode-3 OBJ-enable toggle:
            // - per-sprite FETCH gate: a sprite whose fetch was aborted
            // (disable landed mid-fetch) or whose x-match dot passed while
            // OBJ was disabled (skip-marked by the live walk before its
            // columns pop) never contributes pixels this line, even where
            // OBJ is re-enabled;
            // - per-pixel POP gate: OBJ-enable sampled at this pixel's pop
            // dot (hardware reads LCDC.1 live per popped pixel). A pixel
            // popping 15+ dots after its sprite's fetch match samples the
            // gate one dot LATE (stale-FIFO quirk — pinned by the
            // m3_lcdc_obj_en_change spx=1/2 bands, whose trailing pixels
            // go dark one dot before the disable's normal apply dot; the
            // spx>=8 bands' first-pop pixels at the same dot stay lit).
            if objen_toggled {
                let rec = self.sprite_fetch_recs.get(spr_i);
                if rec.map(|r| r.phase) == Some(SpriteFetchPhase::Aborted) {
                    continue;
                }
                let stale = stale_pop_quirk
                    && rec
                        .filter(|r| r.phase == SpriteFetchPhase::Fetched)
                        .is_some_and(|r| self.ticks >= r.arm_tick + 15);
                if !self.objen_at_tick(self.ticks + stale as u128) {
                    continue;
                }
            }
            // Sprite X coordinate is offset by 8, Y coordinate is offset by 16
            let sprite_actual_x = sprite.x as i16 - 8;
            let sprite_actual_y = sprite.y as i16 - 16;
            let relative_x = screen_x as i16 - sprite_actual_x;
            let relative_y = screen_y as i16 - sprite_actual_y;
            if (0..8).contains(&relative_x) {
                // Mid-mode-3 OBJ-size (LCDC.2) toggle this line: hardware
                // samples the size bit at each tile-data byte's own fetch dot
                // (per-byte row addressing, see obj_pixel_sized); list
                // membership already implies the sprite was scanned y-visible,
                // so the bound is the scan range (0..16), not the live size.
                let objsize_toggled = self.objsize_dot_history.len() > 1;
                let sprite_height = if self.lcdc_has(LCDCFlags::SpriteSize) { 16 } else { 8 };
                let y_in_range = if objsize_toggled {
                    (0..16).contains(&relative_y)
                } else {
                    relative_y >= 0 && relative_y < sprite_height as i16
                };
                if y_in_range {
                    let px = if objsize_toggled {
                        self.obj_pixel_sized(
                            mmio,
                            sprite,
                            self.sprite_fetch_recs.get(spr_i),
                            relative_x as u8,
                            screen_y,
                        )
                    } else {
                        self.get_sprite_pixel(mmio, sprite, relative_x as u8, relative_y as u8)
                    };
                    // The colour lookups both callers run here are pure (&self /
                    // &Mmio), so deferring them to the winner alone — rather than
                    // computing one per opaque sprite and discarding the losers —
                    // is unobservable.
                    if let Some(sprite_pixel_idx) = px
                        && sprite_pixel_idx != 0
                        && (!sprite.attributes.priority || effective_bg_pixel_idx == 0)
                    {
                        return Some((sprite, sprite_pixel_idx));
                    }
                }
            }
        }

        None
    }

    fn get_sprite_pixel(&self, mmio: &mmio::Mmio, sprite: &Sprite, sprite_x: u8, sprite_y: u8) -> Option<u8> {
        let lcdc = self.lcdc;
        let sprite_height = if lcdc_has(lcdc, LCDCFlags::SpriteSize) { 16 } else { 8 };

        if sprite_x >= 8 || sprite_y >= sprite_height {
            return None;
        }

        // Handle Y flipping
        let actual_y = if sprite.attributes.y_flip {
            sprite_height - 1 - sprite_y
        } else {
            sprite_y
        };

        // For 8x16 sprites, the tile index is different
        let tile_index = if sprite_height == 16 {
            if actual_y < 8 {
                sprite.tile_index & 0xFE // Top tile (even)
            } else {
                sprite.tile_index | 0x01  // Bottom tile (odd)
            }
        } else {
            sprite.tile_index
        };

        let tile_line = actual_y % 8;

        // Sprite tiles always use the $8000 addressing method
        let tile_addr = 0x8000 + (tile_index as u16) * 16 + (tile_line as u16) * 2;

        // In CGB mode the sprite tile-data bank is fixed by OAM attr bit 3,
        // independent of the CPU's live VRAM-bank select (FF4F). The PPU must
        // read bank 0 when the bit is clear; using the live `mmio.read` here
        // returns whatever bank the CPU left selected (bank 1 in the
        // scx_attrib tests), corrupting the left-edge sprite color.
        let (low_byte, high_byte) = if mmio.is_cgb_features_enabled() {
            let bank = if (sprite.attributes.raw & 0x08) != 0 { 1 } else { 0 };
            (mmio.read_vram_bank(bank, tile_addr), mmio.read_vram_bank(bank, tile_addr + 1))
        } else {
            // DMG: single bank (the live read is correct).
            (mmio.read(tile_addr), mmio.read(tile_addr + 1))
        };

        // Handle X flipping
        let bit_index = if sprite.attributes.x_flip {
            sprite_x
        } else {
            7 - sprite_x
        };

        let low_bit = (low_byte >> bit_index) & 1;
        let high_bit = (high_byte >> bit_index) & 1;

        Some((high_bit << 1) | low_bit)
    }

    // OBJ-enable (LCDC.1) as-of dot `tick`, resolved from the per-dot history
    // (see `objen_history`).
    fn objen_at_tick(&self, tick: u128) -> bool {
        let mut on = self
            .objen_history
            .first()
            .map(|&(_, b)| b)
            .unwrap_or(self.lcdc_has(LCDCFlags::SpriteDisplayEnable));
        for &(apply_tick, b) in self.objen_history.iter() {
            if apply_tick <= tick {
                on = b;
            } else {
                break;
            }
        }
        on
    }

    // OBJ-size (LCDC.2) as-of dot `tick`, resolved from the per-dot history.
    fn objsize_large_at_tick(&self, tick: u128) -> bool {
        let mut large = self
            .objsize_dot_history
            .first()
            .map(|&(_, l)| l)
            .unwrap_or(self.lcdc_has(LCDCFlags::SpriteSize));
        for &(apply_tick, l) in self.objsize_dot_history.iter() {
            if apply_tick <= tick {
                large = l;
            } else {
                break;
            }
        }
        large
    }

    // DMG sprite pixel with per-byte OBJ-size resolution (mid-mode-3 LCDC.2
    // toggle lines). Hardware computes the object line address separately for
    // the tile-data LOW and HIGH byte reads, sampling LCDC.2 live each time
    // (hardware computes the object line address before both vram reads), so a
    // toggle landing between them mixes two row addressings:
    // tile_y = (ly - oam_y) & (large ? 15 : 7) [y-flip XORs the mask]
    // tile = large ? index & 0xFE : index
    // The byte fetch dots come from the sprite's live fetch record: the stall
    // spans [arm, arm + penalty); the LOW byte reads at end-3, HIGH at end-1.
    // Sprites without a live record (not walked: m0-flush burst) fall back to
    // the live-LCDC path.
    fn obj_pixel_sized(
        &self,
        mmio: &mmio::Mmio,
        sprite: &Sprite,
        rec: Option<&SpriteFetchRec>,
        rel_x: u8,
        screen_y: u8,
    ) -> Option<u8> {
        let Some(rec) = rec.filter(|r| r.phase == SpriteFetchPhase::Fetched) else {
            // No per-fetch record: resolve both bytes with the live size.
            let large = self.lcdc_has(LCDCFlags::SpriteSize);
            return self.obj_pixel_with_sizes(mmio, sprite, rel_x, screen_y, large, large);
        };
        let fetch_end = rec.arm_tick + rec.penalty as u128;
        // CGB DMG-compat shifts both object tile-data read dots 3 dots earlier
        // in the stall than DMG-CPU silicon (see OBJ_READ_*_BACK_CGB).
        let (low_back, high_back) = if mmio.is_cgb() && !mmio.is_cgb_features_enabled() {
            (OBJ_READ_LOW_BACK_CGB, OBJ_READ_HIGH_BACK_CGB)
        } else {
            (OBJ_READ_LOW_BACK, OBJ_READ_HIGH_BACK)
        };
        let low_large = self.objsize_large_at_tick(fetch_end.saturating_sub(low_back));
        let high_large = self.objsize_large_at_tick(fetch_end.saturating_sub(high_back));
        self.obj_pixel_with_sizes(mmio, sprite, rel_x, screen_y, low_large, high_large)
    }

    fn obj_pixel_with_sizes(
        &self,
        mmio: &mmio::Mmio,
        sprite: &Sprite,
        rel_x: u8,
        screen_y: u8,
        low_large: bool,
        high_large: bool,
    ) -> Option<u8> {
        let line_addr = |large: bool| -> u16 {
            let mask: u8 = if large { 15 } else { 7 };
            // (ly - oam_y) & mask == (ly - (oam_y - 16)) & mask (16 ≡ 0 mod both).
            let mut tile_y = screen_y.wrapping_sub(sprite.y) & mask;
            if sprite.attributes.y_flip {
                tile_y ^= mask;
            }
            let tile = if large { sprite.tile_index & 0xFE } else { sprite.tile_index };
            0x8000 + (tile as u16) * 16 + (tile_y as u16) * 2
        };
        let low_byte = mmio.read(line_addr(low_large));
        let high_byte = mmio.read(line_addr(high_large) + 1);
        let bit_index = if sprite.attributes.x_flip { rel_x } else { 7 - rel_x };
        Some((((high_byte >> bit_index) & 1) << 1) | ((low_byte >> bit_index) & 1))
    }
}

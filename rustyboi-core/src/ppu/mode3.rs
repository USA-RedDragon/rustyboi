use crate::memory::mmio;
use crate::memory::Addressable;
use crate::ppu::fetcher;
use super::controller::{
    lcdc_has, sprite_tile_walk_cost, CapturedBgTile, CapturedWinTile, LCDCFlags, Ppu, State,
    CGB_PIXEL_TRANSFER_WARMUP, DMG_PIXEL_TRANSFER_WARMUP, LY, SCX, SPRITE_TILE_NONE, WIN_M3_PENALTY,
    WX, WXEN_COMMIT_DELAY, WYTRIG_COMMIT_DELAY,
};

impl Ppu {
    // Window-Y activation latch. Hardware compares LY against WY at three fixed
    // checkpoints per frame; once any comparison hits, the window is armed for the
    // rest of the frame (`window_y_triggered` is sticky, cleared only at frame
    // start). The three checkpoints are line 0 mode-2 (line cycle 1 + cgb), and
    // the prior line's HBlank at line cycles 450 (compare LY) and 454 (compare
    // LY+1). WX only decides where the armed window begins drawing, not whether it
    // arms; this handles the Y side.
    pub(in crate::ppu) fn update_window_y_latch(&mut self, mmio: &mmio::Mmio) {
        if self.disabled {
            return;
        }
        let is_cgb = mmio.is_cgb_features_enabled();
        // Window-enable bit (LCDC.5) as of THIS checkpoint dot. A window-enable
        // write commits `write_cc + 2` dots after the write; the checkpoint
        // resolves the bit BEFORE that commit, so a write landing exactly on the
        // checkpoint dot still reads the OLD bit here even though the live
        // `self.lcdc.reg` was committed one dot early by pending_lcdc_events.
        let win_en = match self.lcdc.we_win_bit_exact {
            Some((commit_cc, _new, old)) if self.abs_cc <= commit_cc => old,
            _ => self.lcdc_has(LCDCFlags::WindowDisplayEnable),
        };
        if !win_en {
            return;
        }
        let ly = mmio.read(LY) as i32;
        // The checkpoints compare against WY as applied `1 + cgb` cc after the
        // write, not the live mmio value; `wy1` is that delayed copy, so a
        // mid-frame WY write reaches these checkpoints with the correct phase.
        let wy = self.latch.wy1 as i32;

        // ly0 check (only valid during the active frame's line 0 mode-2), at line
        // cycle 1 + cgb. Also runs on the first line after enable (where ly is
        // held at 0 and there is no mode-2 phase).
        if ly == 0
            && self.state == State::OAMSearch
            && self.ticks == (1 + is_cgb as u128)
        {
            if wy == 0 {
                self.window_y_triggered = true;
            }
            return;
        }

        // The remaining checks ride the previous line's HBlank; on the first
        // line after enable there is no such prior line.
        if self.first_line_after_enable {
            return;
        }

        // Prior-to-LY-inc check at line cycle 450: window-enable master |= (ly == wy).
        if self.ticks == 450 {
            if ly == wy {
                self.window_y_triggered = true;
            }
            return;
        }
        // After-LY-inc check at line cycle 454: window-enable master |= (ly + 1 == wy).
        if self.ticks == 454 && ly + 1 == wy {
            self.window_y_triggered = true;
        }
    }

    // Pop one pixel from the BG/window FIFO, mix sprites, write it to the
    // framebuffer at the current x and advance x. Returns true if a pixel was
    // drawn (FIFO non-empty).
    fn draw_fifo_pixel(&mut self, mmio: &mmio::Mmio) -> bool {
        // Window-reactivation insert: render a color-0 pixel without popping
        // (driven by the window-reactivation pixel-insert flag set below).
        let (bg_pixel_idx, bg_attrs) = if self.insert_bg_pixel {
            self.insert_bg_pixel = false;
            (0u8, 0u8)
        } else {
            let Ok(bg_pixel) = self.fetcher.pixel_fifo.pop() else {
                return false;
            };
            (bg_pixel.color, bg_pixel.attrs)
        };
        self.win_being_fetched = false;
        let ly = mmio.ppu_io_reg(LY) as u16;
        let fb_offset = (ly * 160) + self.x as u16;

        // Per-pixel BG-enable. The per-dot draw is
        // flushed in bursts (the mode-0 time flush at mode-3 end draws all remaining
        // FIFO pixels in one pass), so reading the LIVE `self.lcdc.reg` would apply
        // the final BG-enable to every flushed column. Instead evaluate BG-enable
        // as-of THIS column's plot cc from the line's `bgen_history`, so a
        // mid-mode-3 LCDC.0 toggle (BG off then on) covers exactly the pixel span
        // it should — matching the live per-tile `lcdc & lcdc_bgen` read.
        // With no mid-line toggle `bgen_at` returns the single seeded value
        // (== live `lcdc & 1`), so the common case is unchanged.
        let bg_enabled_col = self.bgen_at(mmio, mmio.is_cgb_features_enabled(), self.x);
        if mmio.is_cgb_features_enabled() {
            let final_color_rgb =
                self.mix_background_and_sprites_color(mmio, bg_pixel_idx, bg_attrs, self.x, ly as u8, bg_enabled_col);
            self.record_pixel_debug_event(
                ly as u8,
                bg_pixel_idx,
                [final_color_rgb.0, final_color_rgb.1, final_color_rgb.2],
            );
            let color_offset = fb_offset as usize * 3;
            self.out.color_fb_a[color_offset] = final_color_rgb.0;
            self.out.color_fb_a[color_offset + 1] = final_color_rgb.1;
            self.out.color_fb_a[color_offset + 2] = final_color_rgb.2;
        } else if self.is_cgb_compat_dmg(mmio) {
            // DMG cart on CGB: color output via the boot ROM's DMG-compat palette.
            let final_color_rgb =
                self.mix_background_and_sprites_compat(mmio, bg_pixel_idx, self.x, ly as u8, bg_enabled_col);
            self.record_pixel_debug_event(
                ly as u8,
                bg_pixel_idx,
                [final_color_rgb.0, final_color_rgb.1, final_color_rgb.2],
            );
            // Record BG-won + BG index for the CGB-compat train re-resolve
            // (cgb_train_reresolve): a column BG won iff its final color equals
            // the BG-only compat color of its index (a sprite otherwise overrode
            // it, or the index-independent sprite result differs).
            if (self.x as usize) < self.plot.line_bg_idx.len() {
                let bg_only = self.compat_bg_color(mmio, if bg_enabled_col { bg_pixel_idx } else { 0 });
                self.plot.line_bg_idx[self.x as usize] =
                    if bg_enabled_col && final_color_rgb == bg_only { bg_pixel_idx as i8 } else { -1 };
            }
            let color_offset = fb_offset as usize * 3;
            self.out.color_fb_a[color_offset] = final_color_rgb.0;
            self.out.color_fb_a[color_offset + 1] = final_color_rgb.1;
            self.out.color_fb_a[color_offset + 2] = final_color_rgb.2;
        } else {
            let final_color = self.mix_background_and_sprites(mmio, bg_pixel_idx, self.x, ly as u8, bg_enabled_col);
            // DMG mid-mode-3 BGP-write glitch: record the BG color index of THIS pixel so
            // the mode-3-end `resolve_bgp_spikes` post-pass can re-map it through the
            // OR-glitched palette. Only BG-won pixels are eligible (a sprite that won the
            // mix is untouched). A per-write glitch here cannot see a SET write's FUTURE
            // RESTORE neighbor (the SET column draws before the RESTORE write lands), so
            // the two-write cadence gate is deferred to the post-pass. See `bgp_writes`.
            if (self.x as usize) < self.plot.line_bg_idx.len() {
                let bg_won = bg_enabled_col && final_color == self.get_palette_color(mmio, bg_pixel_idx, self.x);
                self.plot.line_bg_idx[self.x as usize] = if bg_won { bg_pixel_idx as i8 } else { -1 };
            }
            let intensity = match final_color {
                0 => 255,
                1 => 170,
                2 => 85,
                _ => 0,
            };
            self.record_pixel_debug_event(ly as u8, bg_pixel_idx, [intensity, intensity, intensity]);
            self.out.fb_a[fb_offset as usize] = final_color;
        }
        self.x += 1;
        true
    }

    // Compute the 8 BG pixels for tile-map column `tile_col` on pixel
    // row `bg_y`, reproducing the fetcher's addressing. Shared by the fine-scroll
    // first-tile rewrite and the sub-cc SCX column re-key.
    fn bg_pixels_at_col(&self, mmio: &mmio::Mmio, tile_col: u16, bg_y: u16) -> [crate::ppu::fifo::BgPixel; 8] {
        let lcdc = self.lcdc.reg;
        let cgb = mmio.is_cgb_features_enabled();
        let map_base: u16 = if lcdc_has(lcdc, LCDCFlags::BGTileMapDisplaySelect) {
            0x9C00
        } else {
            0x9800
        };
        let map_y = (bg_y / 8) & 0x1F;
        let map_addr = map_base + (map_y * 32 + (tile_col & 0x1F));
        let tile_num = mmio.read_vram_bank(0, map_addr);
        let tile_attrs = if cgb { mmio.read_vram_bank(1, map_addr) } else { 0 };
        let y_flip = cgb && (tile_attrs & 0x40) != 0;
        let x_flip = cgb && (tile_attrs & 0x20) != 0;
        let tile_line = (bg_y % 8) as u8;
        let eff_line = if y_flip { 7 - tile_line } else { tile_line };
        let data_addr = self.fetcher.get_tile_data_address(tile_num, eff_line, lcdc);
        let bank = if cgb && (tile_attrs & 0x08) != 0 { 1 } else { 0 };
        let low = mmio.read_vram_bank(bank, data_addr);
        let high = mmio.read_vram_bank(bank, data_addr + 1);
        let mut pixels = [crate::ppu::fifo::BgPixel::default(); 8];
        for (i, px) in pixels.iter_mut().enumerate() {
            let bit = if x_flip { i as u8 } else { 7 - i as u8 };
            let idx = (((high >> bit) & 1) << 1) | ((low >> bit) & 1);
            *px = crate::ppu::fifo::BgPixel { color: idx, attrs: tile_attrs };
        }
        pixels
    }

    // Replace the 8 oldest BG-FIFO entries with the tile at BG tile-map column
    // `tile_col` (0..32) on the pixel row `bg_y` (already SCY+LY, 0..256),
    // reproducing the fetcher's BG addressing (LCDC tile-map/tile-data select,
    // CGB attribute bank + x/y flip). Used by the mode-3-start fine-scroll re-fetch
    // when a mid-discard SCX write moves the first displayed tile's column.
    #[inline(always)]
    fn rewrite_first_fifo_tile(&mut self, mmio: &mmio::Mmio, tile_col: u16, bg_y: u16) {
        let pixels = self.bg_pixels_at_col(mmio, tile_col, bg_y);
        self.fetcher.pixel_fifo.overwrite_oldest(&pixels);
    }

    // The hardware plot/predictor window-Y gate: `window-enable-master || (wy2 == ly &&
    // window-enable)`. `wy2` is WY delayed ~2 dots after a write; we read WY live, which
    // matches by the time the fetcher reaches WX. This `wy2 == ly` fallback
    // catches late-frame WY writes that land after the three window-enable master
    // checkpoints (e.g. WY=ly written during the same line's mode 3).
    pub(in crate::ppu) fn window_y_active(&self, mmio: &mmio::Mmio) -> bool {
        self.window_y_active_with(mmio, self.lcdc_has(LCDCFlags::WindowDisplayEnable))
    }

    // window_y_active with an explicit window-enable sample. The DMG mid-mode-3
    // trigger paths pass the DELAYED per-dot tap (we_dot_hist[2]) instead of the
    // live bit — hardware's comparator sees a WE write later than our visible
    // lcdc commit does (see we_dot_hist).
    fn window_y_active_with(&self, mmio: &mmio::Mmio, win_en: bool) -> bool {
        if !win_en {
            return false;
        }
        if self.window_y_triggered {
            return true;
        }
        self.latch.wy2 == mmio.read(LY)
    }

    pub(in crate::ppu) fn window_will_start(&self, mmio: &mmio::Mmio, is_cgb: bool) -> bool {
        if !self.window_y_active(mmio) {
            return false;
        }
        let wx = mmio.read(WX) as i32;
        // WX=166 (lcd_hres+6): the window starts on the CGB PPU but not the DMG PPU.
        // This follows the HARDWARE PPU (real CGB silicon, even in DMG-compat/ncm),
        // not the CGB-features flag — age stat-mode-window-ncm keys WX=166 on DEF(CGB)
        // (hardware) and extends mode-3 there, matching cgbBCE not dmgC.
        let _ = is_cgb;
        (0..=166).contains(&wx) && (mmio.is_cgb() || wx != 166)
    }

    // The window-draw decision evaluated at the END of mode 3, where the
    // fetcher's xpos reaches wx==166 (lcd_hres+6) on DMG with WX==166. The
    // window cannot draw a visible pixel this line (the line ends at xpos 166)
    // but it still mutates the window-draw state exactly as the hardware does when xpos hits
    // wx. The OUTER gate is `wx==xpos && (window-enable-master || (wy2==ly && window-enable)) &&
    // xpos<167`; window-enable-master alone is sufficient (does NOT require window-enable). INNER:
    // branch A (886): window-draw-state==0 && window-enable -> start now
    // (window-draw-state = win_draw_start|win_draw_started, the window-Y increment)
    // branch B (889): !cgb && (window-draw-state==0 || xpos==166) -> |= win_draw_start
    // The xpos==166 term makes branch B fire on EVERY qualifying line (even with
    // WE off), arming win_draw_start. That bit survives into the next mode-3-start window checkpoint
    // (and across the frame boundary, since the window-draw state is not reset at frame
    // end) where it is consumed (the window-Y increment, window draws from x0). Running this at
    // line end — AFTER the mid-mode-3 WE-off cleared win_draw_started — is what
    // gives the wxA6 steady state TWO window Y position increments per line (f0 + the HBlank
    // WE-on, which now sees window-draw-state==win_draw_start) and lets the WE-off
    // actually revert the right columns to BG. Idempotent within a line: it only
    // runs once at the mode-3->HBlank transition (the two transition call sites
    // are mutually exclusive per line).
    fn apply_dmg_wxa6_lineend_windraw(&mut self, mmio: &mmio::Mmio, is_cgb: bool) {
        if self.wxa6_lineend_applied {
            return;
        }
        if is_cgb || self.first_line_after_enable || mmio.read(WX) != 166 {
            return;
        }
        self.wxa6_lineend_applied = true;
        let win_en_now = self.lcdc_has(LCDCFlags::WindowDisplayEnable);
        let we_gate = self.window_y_triggered
            || (self.latch.wy2 == mmio.read(LY) && win_en_now);
        if !we_gate {
            return;
        }
        let win_draw_state_zero = !self.win_draw_start && !self.win_draw_started;
        if win_draw_state_zero && win_en_now {
            // branch A (886): start now (no visible window at xpos 166).
            self.win_draw_start = true;
            self.win_draw_started = true;
            self.win_y_pos = self.win_y_pos.wrapping_add(1);
        } else {
            // branch B (889): arm win_draw_start (xpos==166 term, fires
            // regardless of window-enable) for the next line's mode-3-start window-checkpoint consume.
            self.win_draw_start = true;
        }
    }

    pub(in crate::ppu) fn compute_m3_length(&self, mmio: &mmio::Mmio, is_cgb: bool) -> u128 {
        let (len, _win) = self.compute_m3_length_win(mmio, is_cgb);
        len
    }

    // Per-pixel BG-enable. Returns the LCDC.0
    // (BG-enable) bit in effect for display column `sx`, from the line's
    // `bgen_history` (boundary_col, bgen) entries. The last entry whose boundary
    // column is <= `sx` wins. With no mid-mode-3 LCDC.0 toggle the history is a
    // single seed (boundary 0) and this always returns the seeded value —
    // byte-identical to a once-per-pixel live `lcdc & 1` read.
    fn bgen_at(&self, _mmio: &mmio::Mmio, _is_cgb: bool, sx: u8) -> bool {
        if self.plot.bgen_history.len() <= 1 {
            return self
                .plot.bgen_history
                .last()
                .map(|&(_, b)| b)
                .unwrap_or(self.lcdc_has(LCDCFlags::BGDisplay));
        }
        let mut bgen = self.plot.bgen_history[0].1;
        for &(boundary_col, b) in self.plot.bgen_history.iter() {
            if boundary_col <= sx as u64 {
                bgen = b;
            } else {
                break;
            }
        }
        bgen
    }

    // Closed-form mode-3 length to reach an arbitrary `targetx`, mirroring
    // The hardware cycles-until-xpos length: the window penalty (+6) is charged
    // only when `wx < targetx`, and a sprite contributes only when `spx <=
    // targetx`. `compute_m3_length_win` is the `targetx == 167` (mode-0 time, STAT resolve)
    // case; the mode-0 STAT IRQ fires at the xpos-(lcd_hres+6) advance time =
    // the xpos-166 advance time, one xpos earlier. When a window starts at
    // WX=166 and/or a sprite sits at the right edge (spx > 166), that final
    // xpos step carries the whole window+sprite penalty, so xpos 166 lands many
    // dots before xpos 167 — not the usual single dot.
    fn compute_m3_length_to_target(&self, mmio: &mmio::Mmio, is_cgb: bool, targetx: i32) -> u128 {
        let scx = (mmio.read(SCX) & 0x07) as i32;
        let mut cycles: i32 = scx + (1 - is_cgb as i32);
        cycles += targetx; // targetx - xpos, xpos = 0 at tile-loop start

        let mut nwx: i32 = 0xFF;
        if self.window_will_start(mmio, is_cgb) {
            let wx = mmio.read(WX) as i32;
            // On hardware: window penalty only if `wx < targetx` (`wx - xpos <
            // targetx - xpos`). At targetx == 167 this matches the +6 in
            // `compute_m3_length_win` (any in-range WX <= 166 < 167).
            if wx < targetx {
                nwx = wx;
                cycles += WIN_M3_PENALTY;
                if is_cgb && scx == 5 && self.sprites_on_line.is_empty() {
                    let dflt = if mmio.is_double_speed_mode() { 0 } else { -1 };
                    cycles += dflt;
                }
            }
        }

        let obj_enabled = self.lcdc_has(LCDCFlags::SpriteDisplayEnable);
        let mut sprite_xs: Vec<i32> = self.sprites_on_line.iter().map(|s| s.x as i32).collect();
        sprite_xs.sort_unstable();
        cycles += sprite_tile_walk_cost(&sprite_xs, scx, nwx, targetx, obj_enabled || mmio.is_cgb());

        cycles.max(0) as u128
    }

    /// The extra dots (beyond the usual single dot) that the final xpos step
    /// (166 -> 167) carries on this line, i.e. how many dots earlier the mode-0
    /// STAT IRQ (the xpos-166 advance time) fires relative to the mode-0 time
    /// (the xpos-167 advance time) closed form. Zero for plain BG lines, so
    /// the calibrated `M0IRQ_OFFSET` arm is unchanged; non-zero only when a
    /// window starts at WX=166 or a sprite sits at the right edge.
    pub(in crate::ppu) fn m0irq_xpos166_advance(&self, mmio: &mmio::Mmio, is_cgb: bool) -> i64 {
        let len167 = self.compute_m3_length_to_target(mmio, is_cgb, 167) as i64;
        let len166 = self.compute_m3_length_to_target(mmio, is_cgb, 166) as i64;
        (len167 - len166 - 1).max(0)
    }

    // Returns (mode-3 length in dots past base, whether the window contributed).
    fn compute_m3_length_win(&self, mmio: &mmio::Mmio, is_cgb: bool) -> (u128, bool) {
        let scx = (self.m3.first_line_scx_override.unwrap_or_else(|| mmio.read(SCX)) & 0x07) as i32;
        // Fine-scroll discard prefix: the mode-3-start fine-scroll phase consumes scx%8 dots, then
        // the next call(1-cgb) before the tile loop (167-base) begins.
        let mut cycles: i32 = scx + (1 - is_cgb as i32);
        cycles += 167; // targetx - xpos, xpos=0 at tile-loop start

        // Window: if it will start on this line in range. Hardware sets
        // `nwx = wx` and adds 6; sprites then split into a `spx <= nwx` group
        // (first-tile xpos = endx%8) and a `spx > nwx` group (first-tile xpos =
        // nwx+1, previous tile number reset). nwx stays 0xFF when no window starts.
        let mut nwx: i32 = 0xFF;
        let mut win = false;
        if self.window_will_start(mmio, is_cgb) {
            nwx = mmio.read(WX) as i32;
            cycles += WIN_M3_PENALTY;
            // CGB window lines at SCX%8 == 5: the closed-form mode-3 window
            // penalty runs one dot long versus the hardware mode-3-start fine-scroll
            // dispatch at this phase, flipping the sampled STAT mode on the
            // m2int_*_scx5 window probes — but only at single speed; at double
            // speed the hardware phase agrees, so the -1 over-corrects (the DS
            // m2int_wx*_scx5_m3stat reads flip mode3->mode0).
            // A window that starts at WX=0 extends mode-3 one dot longer than the
            // flat StartWindowDraw +6 (the hardware predictor charges +6 for every
            // in-range WX including 0, but real DMG/CGB silicon runs WX=0 one dot
            // long — age stat-mode-window WX=0 rows on CPU-DMG-C / CPU-CGB-B/C/E).
            // Single speed only: at double speed the hardware WX=0 mode-0 time phase
            // already agrees (the +1 would flip 10spritesPrLine_wx0_m3stat_ds /
            // m2int_wxDefault_m3stat_ds), same speed asymmetry as the scx==5 case.
            // The scx==5 CGB SS -1 (below) is a fine-scroll-dispatch correction for
            // a window that starts mid-tile; at WX=0 the window starts at the tile
            // grid origin so that dispatch penalty does not apply (age
            // stat-mode-window-cgbBCE WX=0 scx5 row reads mode 3, not mode 0).
            if is_cgb && scx == 5 && self.sprites_on_line.is_empty() && nwx != 0 {
                let dflt = if mmio.is_double_speed_mode() { 0 } else { -1 };
                cycles += dflt;
            }
            // WX=0 window init runs one dot long when the SCX fine-scroll discard is
            // active (age stat-mode-window WX=0 rows: the AGE fetcher inits the window
            // at 8 clks instead of 7 when `alignment_x >= 1`). Speed-independent in
            // dots — the previous `!ds` gate left the DS WX=0 scx>0 rows one dot short.
            if nwx == 0 && scx > 0 {
                cycles += 1;
            }
            win = true;
        }

        // Sprites. The single faithful tile-walk model (shared with the live
        // renderer via `sprite_tile_walk_cost`). Only count if OBJ enabled (or
        // CGB always evaluates them).
        let obj_enabled = self.lcdc_has(LCDCFlags::SpriteDisplayEnable);
        let target_x = 167;
        let mut sprite_xs: Vec<i32> = self.sprites_on_line.iter().map(|s| s.x as i32).collect();
        sprite_xs.sort_unstable();
        // The CGB "OBJ-disable does not shorten mode 3" quirk is a property of the
        // CGB PPU SILICON, not of CGB mode: a CGB running a DMG cart in compat mode
        // still pays the OBJ fetch cost with LCDC.1 clear (gbc-hw-tests
        // mode2_read_oam_spr_dis_dmg_mode -- the mode-3 end sits 16 dots later than
        // an obj-free line). Real DMG silicon does skip it, so key on `mmio.is_cgb()`
        // (hardware) rather than the KEY0 compat flag threaded in as `is_cgb`.
        cycles += sprite_tile_walk_cost(&sprite_xs, scx, nwx, target_x, obj_enabled || mmio.is_cgb());

        (cycles.max(0) as u128, win)
    }

    /// Runtime-only mode-3 extension when a sprite sits at spx == 0. A sprite
    /// whose X is exactly 0 straddles the fine-scroll discard, so the fetch
    /// stalls `min(scx&7, 5)` extra dots before the tile loop begins.
    ///
    /// This cost lives ONLY in the runtime fetch loop that drives the real
    /// mode-3 -> mode-0 transition (and therefore the STAT-mode read the CPU
    /// polls). The closed-form m0-STAT-IRQ length model does NOT include it, so
    /// `compute_m3_length` (which arms `sched_m0irq`) must stay clean — the m0
    /// IRQ fires at the predicted time, the mode transition one `min(scx&7,5)`
    /// dot later. Applied
    /// to `m0_time_master` (the renderer/STAT boundary) and subtracted back out in
    /// `m0_irq_event_cc_master`. Mooneye intr_2_mode0_timing_sprites_scx{1..4}.
    pub(in crate::ppu) fn sprite0_scx_extra(&self, mmio: &mmio::Mmio, is_cgb: bool) -> i64 {
        let obj_enabled = self.lcdc_has(LCDCFlags::SpriteDisplayEnable);
        if !(obj_enabled || is_cgb) {
            return 0;
        }
        if !self.sprites_on_line.iter().any(|s| s.x == 0) {
            return 0;
        }
        let scx = (self.m3.first_line_scx_override.unwrap_or_else(|| mmio.read(SCX)) & 0x07) as i64;
        scx.min(5)
    }
    /// Mode 3 (pixel transfer) for one dot: the fetcher/FIFO advance, the
    /// SCX fine-scroll rekeys, the window-activation paths and the mode-3 ->
    /// mode-0 transition. Lifted verbatim out of `step`'s
    /// `State::PixelTransfer` arm.
    ///
    /// That arm was a `'label: { .. }` block whose 16 `break 'label;` sites mean
    /// "this dot is done, but `step` still runs its trailing DMG palette latch".
    /// As a method each of those is a plain `return;` with the same meaning:
    /// control resumes at the caller after the `match`, which is where the
    /// labelled break landed. No early exit is added or dropped.
    /// Mid-mode-3 WX-change rekey: a WX write (or a window-will-start flip)
    /// after the closed-form mode-0 schedule was captured at mode-3 arm either
    /// re-keys that schedule with/without the `StartWindowDraw` penalty, or
    /// drops it so the live emergent `x == 160` transition takes over.
    ///
    /// Lifted verbatim out of `step_mode3_dot`. The block had no early exit of
    /// its own, so it needs no "stop this dot" signal back to the caller.
    #[inline(always)]
    fn mode3_rekey_wx_change(&mut self, mmio: &mmio::Mmio, fast: bool) {
        if !fast
            && self.m0.scheduled_mode0_dot.is_some()
            && !self.window_started_this_line
            && !self.win_wx_enable_resolved
            && (mmio.read(WX) != self.m0.m3_scheduled_wx
                || self.window_will_start(mmio, mmio.is_cgb_features_enabled())
                    != self.m0.m3_scheduled_win)
        {
            // WX-write-ENABLE: the window was out of range at M3 arm
            // (`!m3_scheduled_win`, so m0_time_master has NO StartWindowDraw
            // penalty) and a mid-mode-3 WX write brings it into range so the
            // window will now start this line. The hardware next-mode-0 prediction
            // re-runs with the window included, moving the mode-3 end
            // WIN_M3_PENALTY dots later. ADD that penalty (symmetric to the
            // LCDC window-enable path) iff the write lands before the window
            // tile commits — otherwise the fetcher already passed the window
            // start and no penalty accrues. Scoped CGB / no sprites; the live
            // pipeline is untouched, only the read-at-cc mode-0 time is shifted.
            let now_will_start =
                self.window_will_start(mmio, mmio.is_cgb_features_enabled());
            // Only the WX-into-range case: WX itself changed from out of range
            // (arm WX > 166, no window scheduled) to in range. A window that
            // newly starts for any OTHER reason (a mid-mode-3 WY trigger with
            // WX unchanged and already in range) is NOT this lever and must
            // keep nulling (the late_wy / late_scx_late_wy cluster).
            let arm_wx = self.m0.m3_scheduled_wx as i32;
            let wx_now = mmio.read(WX) as i32;
            let wx_into_range = arm_wx > 166 && (0..=166).contains(&wx_now);
            let wx_enable_clean = !self.m0.m3_scheduled_win
                && now_will_start
                && wx_into_range
                && mmio.is_cgb_features_enabled()
                && !mmio.is_double_speed_mode()
                && self.sprites_on_line.is_empty();
            let mut keep_schedule = false;
            if wx_enable_clean && let Some(m0t) = self.m0.m0_time_master {
                // Latch: this clean WX-enable is now resolved for the line, so
                // later dots (WX still != arm) do not re-enter and null.
                self.win_wx_enable_resolved = true;
                keep_schedule = true;
                let wx = mmio.read(WX) as i32;
                let x_at_start = (wx - 7).max(0);
                let warmup = CGB_PIXEL_TRANSFER_WARMUP as i64;
                // SCX>3 / scx5 fine-scroll: the x==0 window-tile commit runs
                // two dots later per extra discarded SCX dot, mirroring the
                // late-WX-disable accrual shift.
                let win_fine = if wx <= 7 {
                    2 * (((self.m3.m3_arm_scx & 7) as i64) - 3).max(0)
                } else {
                    0
                };
                let commit_dot = self.m3.m3_arm_dot as i64
                    + warmup
                    + 8
                    + self.m3.m3_arm_scx as i64
                    + x_at_start as i64
                    + win_fine
                    + WXEN_COMMIT_DELAY;
                if (self.ticks as i64) < commit_dot {
                    let pen = (WIN_M3_PENALTY as i64) << (mmio.is_double_speed_mode() as i64);
                    self.m0.m0_time_master = Some((m0t as i64 + pen).max(0) as u64);
                    // Keep the closed-form schedule (mode-3 end shifts with
                    // the penalty); only the master mode-0 time moved.
                }
                // else: window starts but the write is past the commit dot, so
                // no penalty is added — the no-window mode-0 time captured at arm is
                // the correct (mode-0-earlier) boundary; keep the schedule.
            }
            // WY-trigger ENABLE (symmetric to the WX-into-range branch above):
            // WX is UNCHANGED and already in range, but the window newly starts
            // this line because a mid-mode-3 WY write made `window_y_active`
            // true (the window-enable master / `wy2 == ly` gate flipped). The hardware
            // next-mode-0 prediction then runs with the window included, moving the
            // mode-3 end WIN_M3_PENALTY dots later — BUT only if the WY trigger
            // lands before the fetcher reaches the window-start xpos. For an
            // x==0 window (the late_wy / late_scx_late_wy cluster, WX in 0..=7)
            // that commit dot is `m3_arm_dot + scx&7 + COMMIT`: the f0/f1
            // dispatch reaches xpos 0 (the window tile) `scx&7` dots into M3.
            // (Measured byte-exact via cctracer: mode-0 time = no-window + 6 for the
            // `_1` reps that trigger 1 dot in, == no-window for the `_2`/`_3`
            // reps that trigger 5+ dots in; the boundary is m3_arm_dot+scx+3 at
            // both scx=0 and scx=4.) If the trigger lands at/after the commit
            // dot, the fetcher already passed xpos 0 so no penalty accrues and
            // the no-window mode-0 time (captured at arm) is the correct boundary.
            // Scoped CGB / single speed / no sprites / x==0 window; the live
            // pipeline is untouched, only the read-at-cc mode-0 time is shifted.
            if !keep_schedule
                && !self.m0.m3_scheduled_win
                && now_will_start
                && arm_wx == wx_now
                && (0..=7).contains(&wx_now)
                && mmio.is_cgb_features_enabled()
                && !mmio.is_double_speed_mode()
                && self.sprites_on_line.is_empty()
                && let Some(m0t) = self.m0.m0_time_master
            {
                // This WY-trigger enable is resolved for the line; suppress
                // re-entry on later dots (window_will_start stays != arm).
                self.win_wx_enable_resolved = true;
                keep_schedule = true;
                // Commit dot = the M3 dot at which the fetcher reaches the
                // window-start xpos. For an x==0 window (WX 0..=7) that is
                // `m3_arm_dot + scx&7 + WX + 3`: the SCX fine-scroll discard
                // (scx&7 dots) then the WX-pixel BG prefix before the window
                // tile, plus the fixed f0/f1 dispatch lead (3). A WY trigger
                // before this dot adds the StartWindowDraw penalty (mode 3
                // runs WIN_M3_PENALTY longer); at/after it the fetcher already
                // passed xpos 0, so no penalty accrues. (cctracer: the `_1`
                // reps of late_wy_*_wx00 / late_wy_*_wx07 / late_scx_late_wy
                // keep the +6 mode-0 time, the `_2`/`_3` reps drop it; the WX-shift
                // separates the wx00 `_1` boundary from the wx07 `_1`.)
                let commit_dot = self.m3.m3_arm_dot as i64
                    + (self.m3.m3_arm_scx & 7) as i64
                    + wx_now as i64
                    + WYTRIG_COMMIT_DELAY;
                if (self.ticks as i64) < commit_dot {
                    self.m0.m0_time_master =
                        Some((m0t as i64 + WIN_M3_PENALTY as i64).max(0) as u64);
                }
                // else: no penalty — keep the no-window mode-0 time captured at arm.
            }
            // DMG WY-trigger enable (mirror of the CGB branch above). A
            // mid-mode-3 WY==LY trigger with an x==0 window (WX 0..=7,
            // unchanged) brings the window into play this line. Hardware keeps
            // a finite (window-inclusive or no-window) mode-0 time, so the FF41
            // line-tail read resolves a concrete mode 0/3 boundary; nulling
            // m0_time_master here would defer to the renderer register (always
            // mode 3), passing the out3 `_1`/`_2` reps but FAILING the out0
            // `_3` rep (late_wy_FFto2_ly2_wx00_3 / late_scx_late_wy_FFto4_ly4
            // _wx00_3). Keep the no-window mode-0 time and add WIN_M3_PENALTY iff the
            // WY trigger lands before the window-tile commit dot. The DMG commit
            // dot is the CGB form (`m3_arm_dot + scx&7 + WX + 3`) plus the
            // DMG pixel-transfer warmup less one (`DMG_WARMUP - 1` = 3):
            // measured ticks at the WY block bracket it across WX/SCX (wx00:
            // pen@84,no-pen@88; scx4: pen@84/88,no-pen@92; wx07: pen@88/92,
            // no-pen@96; scx3+wx07: pen@88/92,no-pen@96), so commit_dot =
            // m3_arm_dot + scx&7 + WX + 3 + 3 separates pen vs no-pen at every
            // rep. Scoped DMG / SS / no sprites / x==0 (WX 0..=7).
            if !keep_schedule
                && !self.m0.m3_scheduled_win
                && now_will_start
                && arm_wx == wx_now
                && (0..=7).contains(&wx_now)
                && !mmio.is_cgb_features_enabled()
                && !mmio.is_double_speed_mode()
                && self.sprites_on_line.is_empty()
                && let Some(m0t) = self.m0.m0_time_master
            {
                self.win_wx_enable_resolved = true;
                keep_schedule = true;
                let commit_dot = self.m3.m3_arm_dot as i64
                    + (self.m3.m3_arm_scx & 7) as i64
                    + wx_now as i64
                    + WYTRIG_COMMIT_DELAY
                    + (DMG_PIXEL_TRANSFER_WARMUP as i64 - 1);
                if (self.ticks as i64) < commit_dot {
                    self.m0.m0_time_master =
                        Some((m0t as i64 + WIN_M3_PENALTY as i64).max(0) as u64);
                }
                // else: no penalty — keep the no-window mode-0 time captured at arm.
            }
            // WX-DISABLE of a WX<7 (visible x==0) window that WAS scheduled at
            // M3 arm: the immediate-start window's StartWindowDraw penalty
            // locks the moment the fetcher fetches the window tile (the hardware
            // `xpos == wx` compare uses the WX register, so a smaller WX commits
            // earlier). A WX-write moving WX out of range at/after that commit
            // dot keeps the window-inclusive m0_time_master (mode 3 persists ->
            // out3); before it the existing null applies (refund -> mode 0). The
            // commit dot is `m3_arm_dot + DMG_WARMUP + 5 + scx&7 + WX` (the first
            // BG tile fill plus the WX-pixel BG prefix before the window tile,
            // less the f0/f1 dispatch lead). The late_wx_wx03_{1,2} DMG reps
            // bracket it at WX=3 (write at dot 88 = before -> out0; dot 92 =
            // at commit -> out3); WX=7 (late_wx_1) commits 4 dots later (dot
            // 96) so the same dot-92 disable still nulls (out0). Scoped DMG /
            // single speed / no sprites / WX<7; the WX>=7 reps keep the existing
            // `>= 7` graduated branch below. window_started_this_line is still
            // false at this dot (the latch lags the closed-form commit).
            if !keep_schedule
                && self.m0.m3_scheduled_win
                && (self.m0.m3_scheduled_wx as i32) < 7
                && !now_will_start
                && !mmio.is_cgb_features_enabled()
                && !mmio.is_double_speed_mode()
                && self.sprites_on_line.is_empty()
                && self.m0.m0_time_master.is_some()
            {
                let commit_dot = self.m3.m3_arm_dot as i64
                    + DMG_PIXEL_TRANSFER_WARMUP as i64
                    + 5
                    + (self.m3.m3_arm_scx & 7) as i64
                    + self.m0.m3_scheduled_wx as i64;
                if (self.ticks as i64) >= commit_dot {
                    keep_schedule = true;
                    self.win_wx_penalty_resolved = true;
                }
            }
            if !keep_schedule {
                self.m0.scheduled_mode0_dot = None;
                self.m0.m0_time_master = None;
            }
        }
    }

    /// Window activation during mode 3: the live `x + 7 == WX` (and WX<7
    /// immediate) comparator that switches the fetcher over to window tiles,
    /// with the DMG/CGB WX-range rules and the fetch restart.
    ///
    /// Lifted verbatim out of `step_mode3_dot`. Three sites inside it ended the
    /// dot early (`break 'label` originally, `return` after that extraction), so
    /// the helper returns `bool` and the caller re-issues the `return` — the
    /// early exits are preserved explicitly rather than lost to the code motion.
    #[inline(always)]
    fn mode3_activate_window(&mut self, mmio: &mut mmio::Mmio, trigger_we: bool) -> bool {
        if self.window_y_active_with(mmio, trigger_we)
            && !self.fetcher.is_fetching_window()
        {
            let wx = mmio.read(WX);
            let is_cgb = mmio.is_cgb_features_enabled();
            // DMG never starts the window drawing at WX==166; CGB does.
            let wx_allowed = wx <= 166 && (is_cgb || wx != 166);
            // WX=0-6 can trigger immediately, WX=7+ needs exact match with X+7.
            // On DMG, WX 1..6 activates ONLY via the exact pos==WX-7
            // prologue match (the EARLY check above); reaching pos 0 with
            // WX 1..6 means the match was missed (WX rewritten
            // mid-prologue) and the window does not start this line.
            // WX=0 and CGB keep the immediate x==0 start.
            let is_dmg = !is_cgb;
            let scx_fine = if self.m3.m3_discard_target >= 0 {
                self.m3.m3_discard_target as u8
            } else {
                mmio.read(SCX) & 0x07
            };
            // CGB WX=0 with a fine SCX scroll: the window takes the stream over
            // at once, its grid anchored at screen x = -(8 + scx&7) as on DMG,
            // but the fetches covering its first two columns are GLITCHED --
            // they skip the tile-MAP read, so they draw the PREVIOUS scanline's
            // last tile number (and attributes) at this line's window row (see
            // Fetcher::wx0_glitch_fetches). SameBoy Core/display.c
            // GB_update_wx_glitch sets cgb_wx_glitch for WX==0 while
            // position_in_line is in [-16,-8] (extended to -7 with fractional
            // scrolling), which gates GET_TILE_T2; the two tile-data reads
            // still run. Column 0 is entirely off-screen, so only the on-screen
            // part of column 1 shows the glitch: 8-(scx&7) pixels at x = 0..,
            // with column 2 the first real window column at x = 8 - scx&7.
            // scx&7 == 7 discards one pixel fewer (SameBoy's `(position_in_line
            // & 7) == 6 && scx&7 == 7` shortcut while the window is being
            // fetched), so it lands like scx&7 == 6.
            // Evidence: on a probe whose window map row 0 uses a different tile
            // from rows 1+, and whose tile rows alternate colour, x0..6 renders
            // the previous row's TILE at the current row's LINE -- a colour
            // present in neither that scanline's background nor its window. The
            // background cannot supply it. SameBoy CGB-C and CGB-E agree
            // pixel-for-pixel over the whole probe set (the window_wx0_scx1
            // fine-SCX WX=0 case).
            // scx&7 == 0 does not glitch (the window activates at
            // position_in_line == -7, past the glitch window) and keeps the
            // plain 7-WX chop.
            let cgb_wx0_fine = is_cgb && wx == 0 && scx_fine != 0;
            // DMG WX=0 with a fine SCX scroll: same anchor, but the window
            // takes the stream over immediately, so window column 1 pixel
            // scx&7 is what lands at x0 -- the first fetched column is 1, not
            // 0. This is a COLUMN advance, not extra discard pops: the
            // prologue's dot budget is unchanged (mealybug m3_window_timing_wx_0
            // measures exactly that budget through a mid-line BGP write).
            // Not modelled: the scx&7 == 7 one-pixel case. SameBoy shortens the
            // prologue by a dot there, which would move mealybug's BGP edge;
            // rustyboi keeps the plain column advance and stays one pixel off
            // on the WX=0 / scx&7==7 case (DMG only).
            let dmg_wx0_fine = is_dmg && wx == 0 && scx_fine != 0;
            // DMG one-dot-late activation (the position+6 check):
            // when the exact x+7==WX dot did not activate (the comparator
            // read the WE-off pulse), the very next dot still matches via
            // WX == x+6 and starts the window one pixel late (at WX-6).
            let should_start_window = wx_allowed
                && if wx < 7 {
                    self.x == 0 && !(is_dmg && (1..7).contains(&wx))
                } else {
                    self.x + 7 == wx || (is_dmg && self.x >= 1 && self.x + 6 == wx)
                };

            // DMG WX=0 + SCX&7>0 quirk: the window activates one T-cycle
            // later. The would-be trigger dot is dead (no pop, no
            // activation); trigger next dot.
            if should_start_window
                && !is_cgb
                && wx == 0
                && !self.win_wx0_delayed
                && scx_fine != 0
            {
                self.win_wx0_delayed = true;
                return true;
            }

            if should_start_window {
                // DMG exact-match mid-line trigger: defer the commit one
                // dot so a WX store landing on the commit dot is seen by
                // the comparator (see dmg_wx_trigger_pending).
                if is_dmg && wx >= 7 && self.x + 7 == wx {
                    self.dmg_wx_trigger_pending = Some((self.ticks, wx));
                    return true;
                }
                // Window draw-start (the mode-3-start window checkpoint /
                // plot win_draw_start).
                if cgb_wx0_fine {
                    // Column 0 lies entirely left of the screen, and rustyboi's
                    // prologue -- unlike hardware's -- does not run a discarded
                    // dummy first fetch, so the first FETCHED column here is the
                    // first DISPLAYED one: column 1. Both hardware fetches over
                    // columns 0 and 1 are glitched, but they reuse the same
                    // stale tile number, so eliding column 0 leaves one armed
                    // glitch and keeps the prologue's dot budget identical to
                    // the background's.
                    self.begin_window_draw_at_tile(self.x, 1);
                    self.fetcher.arm_wx0_glitch(1);
                } else if dmg_wx0_fine {
                    self.begin_window_draw_at_tile(self.x, 1);
                } else {
                    self.begin_window_draw(self.x);
                }
                // DMG: hardware restarts the fetcher ON the trigger dot
                // (TileNumber now; low/high/push at t+2/t+4/t+6), so the
                // first window pixel pops exactly 6 dots after the
                // trigger regardless of the global fetch parity. Run the
                // TileNumber substep immediately and phase-lock the rest
                // of the startup to this dot (see win_fetch_anchor).
                if !is_cgb {
                    // WX 1..6: the comparator matched chop = (7-WX) dots
                    // into the discard prologue, so the activation lies
                    // chop dots in the PAST. Catch the fetch up by
                    // running every substep whose anchored phase
                    // (0,2,4,6) has already elapsed, anchor the cadence
                    // at ticks - chop, and pace the chop discard pops
                    // 1/dot from the x==0 prologue below. WX=0 keeps the
                    // plain trigger (separate activation-position quirk
                    // cluster; see win_wx0_delayed).
                    let chop = if (1..7).contains(&wx) { 7 - wx } else { 0 };
                    self.win_first_tile_chop = chop;
                    // DMG window bus-glitch grid origin (see wg_apply):
                    // this TileNumber's conceptual dot is `chop` dots in
                    // the past; a pre-window sprite stall delayed the
                    // anchored trigger by its live charged penalty
                    // (SpriteFetchRec) that hardware does NOT share
                    // (its own delay is D_pre, folded in at read
                    // evaluation).
                    self.wg_set_anchor(chop as u64);
                    let mut phase = 0u8;
                    loop {
                        let fls = self.wg_apply(self.fetcher_lcdc_state());
                        if let Some(event) = self.fetcher.step(
                            mmio,
                            fls,
                            crate::ppu::fetcher::FetchPos {
                                window_line: self.win_y_pos,
                                display_x: self.x,
                                pending_discard: 0,
                                scy: self.latch.scy_delayed,
                                scx: self.latch.scx_delayed,
                            },
                        ) {
                            if matches!(
                                event.kind,
                                crate::ppu::fetcher::FetcherDebugEventKind::TileNumber
                            ) {
                                self.m3.subcc_last_tn_cc = self.abs_cc;
                            }
                            self.record_fetch_debug_event(event, mmio);
                        }
                        phase += 2;
                        if phase > chop {
                            break;
                        }
                    }
                    // chop >= 6: the first tile's push already elapsed
                    // (phase 6), so its first discard pop is due on this
                    // very dot.
                    if chop >= 6 && self.fetcher.pixel_fifo.pop().is_ok() {
                        self.win_first_tile_chop -= 1;
                    }
                    self.win_fetch_anchor =
                        Some(self.ticks.wrapping_sub(chop as u128));
                } else if cgb_wx0_fine {
                    // The SCX fine-scroll discard below trims scx&7 pixels off
                    // the glitched column 1, leaving 8-(scx&7) of it on screen
                    // and column 2 at x = 8 - scx&7. scx&7 == 7 stops one pixel
                    // early (SameBoy's `(position_in_line & 7) == 6 && scx&7 ==
                    // 7` shortcut while the window is being fetched), so it
                    // lands like scx&7 == 6.
                    self.win_first_tile_chop = 0;
                    if scx_fine == 7 {
                        self.m3.m3_pixels_discarded = 1;
                    }
                } else if wx < 7 {
                    // CGB window left-clip chop (window_wx0..6):
                    // a WX<7 window activates at LX==0 with chop = 7-WX
                    // pixels of its first tile off the left edge. SameBoy
                    // (CGB-C/E) DRAWS the window's own chopped
                    // leading pixels; rustyboi currently drew the full
                    // first window tile and shifted the content one tile
                    // right (window_wx1..6 / window_wx0_scx0 leftmost
                    // columns render BG-ish instead of window). Pop the
                    // chopped leading window pixels through the x==0
                    // prologue (win_first_tile_chop); the CGB fetcher fills
                    // on the normal cadence, so no DMG-style fetch-anchor
                    // phase-lock is needed. Leaves the WX=0+SCX fine-scroll
                    // discard (win_x0_locked / m3_window_timing_wx_0) intact.
                    self.win_first_tile_chop = 7 - wx;
                }
                // The post-window sprite group restarts the BG-tile grid
                // (hardware resets the previous sprite tile number to none after
                // the window split), so the first post-window sprite in a
                // tile is again charged the leading rate.
                self.m3_sprite_prev_tile = SPRITE_TILE_NONE;
                if self.win_start_dot.is_none() {
                    self.win_start_dot = Some(self.ticks);
                }
                return true; // Skip this cycle to let window fetching start
            }
        }
        false
    }

    /// One BG/window fetcher step and everything that reacts to the event it
    /// emits: the tile-fetch bookkeeping, the DMG window bus-glitch journal, the
    /// sub-dot SCX/SCY re-resolves and the FIFO overwrites they drive.
    ///
    /// Lifted verbatim out of `step_mode3_dot`. The block has no early exit, so
    /// there is no "stop this dot" signal to plumb. It did write one of the
    /// caller's locals — `push_this_dot`, which it only ever sets to `true` —
    /// so that becomes the return value and the caller re-applies it.
    #[inline(always)]
    fn mode3_fetch_step(&mut self, mmio: &mut mmio::Mmio, cadence_even: bool, fetcher_lcdc_state: fetcher::FetcherLcdcState, pending_discard: u8) -> bool {
        let mut push_this_dot = false;
        if cadence_even
            && let Some(event) = self.fetcher.step(mmio, fetcher_lcdc_state, crate::ppu::fetcher::FetchPos {
                window_line: self.win_y_pos,
                display_x: self.x,
                pending_discard,
                scy: self.latch.scy_delayed,
                scx: self.latch.scx_delayed,
            }) {
                if matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::TileNumber) {
                    self.m3.subcc_last_tn_cc = self.abs_cc;
                }
                if matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo) {
                    push_this_dot = true;
                    // The display-x at which this tile's first pixel will
                    // pop (the hardware push-at-empty dot), SIGNED: during
                    // the SCX fine-scroll discard prologue the boundary
                    // sits at the hardware position -(pending discards) < 0.
                    if !mmio.is_cgb_features_enabled() {
                        let first_x = self.x as i32 + event.fifo_size as i32
                            - 8
                            - pending_discard as i32;
                        if (0..160).contains(&first_x) {
                            // Visible boundary: queue for the pop-side
                            // WE-off zero-pixel insert check.
                            if let Some(slot) = self
                                .we_glitch_tile_starts
                                .iter_mut()
                                .find(|s| s.is_none())
                            {
                                *slot = Some(first_x as u8);
                            }
                        } else if first_x < 0 && !mmio.is_cgb() {
                            // Discard-prologue boundary (a known hardware
                            // quirk): evaluate the WE-off insert HERE, at
                            // the push dot. logical position = first_x+7
                            // (hardware clamps out-of-range to 0, matching
                            // WX==0). A hit inserts a color-0 pixel that
                            // the prologue itself swallows — one discard
                            // dot consumes it instead of a real pixel
                            // (see we_glitch_discard_insert). Pre-CGB
                            // MACHINES only (non-CGB hardware): the CGB
                            // PPU has no insert glitch even in DMG-compat.
                            let logical = first_x + 7;
                            let logical =
                                if (0..=167).contains(&logical) { logical } else { 0 };
                            if self.window_y_triggered
                                && !self.fetcher.is_fetching_window()
                                && !self.we_dot_hist[2]
                                && !self.we_insert_suppressed
                                && mmio.read(WX) as i32 == logical
                            {
                                self.we_glitch_discard_insert = true;
                            }
                        }
                    }
                    // CGB-compat up-pulse LCDC.4 train: buffer each BG tile
                    // so a line-end re-resolve against the COMPLETE journal
                    // can fix the tiles fetched before the whole pulse train
                    // was journaled (see cgb_train_reresolve).
                    if self.wg.wg_cgb && !event.fetching_window && !self.wg.wg_hist.is_empty() {
                        let first_x = (self.x as i32 + event.fifo_size as i32
                            - 8
                            - pending_discard as i32)
                            .max(0);
                        if (0..160).contains(&first_x) {
                            self.wg.bg_tile_buf.push(CapturedBgTile {
                                n: event.tile_index as u64,
                                tn: event.tile_num,
                                first_x: first_x as u8,
                                y: self.fetcher.latched_y(),
                                live_low_tds: self.fetcher.last_low_tds(),
                                live_high_tds: self.fetcher.last_high_tds(),
                            });
                        }
                    }
                    // WINDOW analog (win_train_reresolve): the window internal
                    // line is win_y_pos (not latched_y, which the window fetch
                    // does not update).
                    if self.wg.wg_cgb && event.fetching_window && !self.wg.wg_hist.is_empty() {
                        let first_x = (self.x as i32 + event.fifo_size as i32
                            - 8
                            - pending_discard as i32)
                            .max(0);
                        if (0..160).contains(&first_x) {
                            self.wg.win_tile_buf.push(CapturedWinTile {
                                n: event.tile_index as u64,
                                tn: event.tile_num,
                                first_x: first_x as u8,
                                y: self.win_y_pos,
                                live_low_tds: self.fetcher.last_low_tds(),
                                live_high_tds: self.fetcher.last_high_tds(),
                            });
                        }
                    }
                }
                // The window fetch anchor persists for the rest of
                // the line — the hardware fetch grid stays phase-locked
                // to the restart (pushes every 8 dots from the anchor),
                // so the reactivation-insert columns stay at
                // window_start + 8k. It resets at the next M3 arm or window
                // restart.
                // Sub-cc column adjustment: a BG tile whose column was committed
                // at TileNumber under the OLD scx, but whose pixels are
                // PLOTTED after the write's apply cc (write_cc + 2*cgb),
                // must render under the NEW scx (a mid-mode-3 SCX write
                // samples the column at plot time, not fetch time). Only the single in-flight straddle
                // tile (armed at the write) is corrected, and only at the
                // exact plot-vs-apply phase (gap == 4); see the gap comment
                // below.
                let mut armed_this_event = false;
                if self.m3.subcc_rekey_armed
                    && matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo)
                {
                    // The single in-flight tile (column committed under the
                    // OLD scx before the write) just pushed. Its first
                    // displayed pixel sits at display column == the xpos the
                    // fetcher used (xpos == display_x + fifo - pending); its
                    // plot cc is abs_cc + (xpos - current display x). If that
                    // plot cc is strictly after the apply cc the tile must
                    // render under the NEW scx (the hardware SCX change samples
                    // the column at plot, not fetch); re-key the 8 newest
                    // FIFO entries with the NEW-scx column using the
                    // fetcher's exact xpos/cgb_adj. Disarm afterwards.
                    self.m3.subcc_rekey_armed = false;
                    let dsf = mmio.is_double_speed_mode() as u32;
                    let (xpos, cgb_adj, _) = self.fetcher.subcc_last_column_inputs();
                    // plot cc = abs_cc + the dot distance to this tile's
                    // first displayed pixel. The dot delta must be scaled
                    // to master cc (1 dot = 1<<ds cc) so the gap resonance
                    // is in master cc at both speeds.
                    let plot_cc = self.abs_cc as i64
                        + ((xpos as i64 - self.x as i64) << dsf);
                    // SS (validated Stage 1b, broke-0 across the full
                    // suite incl. DMG): the in-flight straddle flips to NEW
                    // at the exact plot-vs-apply phase gap==4.
                    let gap = plot_cc - self.m3.subcc_scx_apply_cc as i64;
                    // DMG SS + low-X sprite: the sprite-fetch dot during the
                    // discard prologue shifts the whole line's BG-fetch phase
                    // one tile, so a steady-state mid-line SCX write's
                    // OLD->NEW column boundary also lands one tile LATER than
                    // the no-sprite cadence the gap==4 rekey assumes. The
                    // in-flight tile plots just before the boundary, so keep
                    // it OLD (suppress the flip); the NEXT tile, fetched after
                    // the write, is already NEW. Mirrors the CGB gap==1
                    // first-line revert. Without the sprite (scx_during_m3_4/5)
                    // gap==4 stays as the validated steady-state flip.
                    let dmg_ss_lowx_sprite = dsf == 0
                        && !mmio.is_cgb_features_enabled()
                        && self.lcdc_has(LCDCFlags::SpriteDisplayEnable)
                        && self.sprites_on_line.iter().any(|s| s.x <= 8);
                    // DS (Stage 2): the gap proxy is ambiguous across
                    // initial-scx, but the underlying resonance is that the
                    // write's apply cc lands at the MIDPOINT of the armed
                    // tile's fetcher step. The BG fetcher advances one step
                    // every 2 dots == (2<<ds) cc; the armed tile's column
                    // was latched at TileNumber (subcc_last_tn_cc) and
                    // The hardware SCX-write handling re-derives that
                    // single tile NEW only when apply falls half a step
                    // (1<<ds cc) past the latch, modulo the step:
                    // (apply_cc - tn_cc) % (2<<ds) == (1<<ds)
                    // At DS this is (apply-tn)%4==2, which flips ds_3/4/5
                    // across every initial-scx (0761/0360/...) where the
                    // cruder gap/span proxies disagree. SS keeps gap==4
                    // (the DMG cadence differs and the mod phase regresses
                    // the DMG SS set, so SS is left exactly as Stage 1b).
                    let flip = if dsf == 0 {
                        gap == 4 && !dmg_ss_lowx_sprite
                    } else {
                        let step = 2i64 << dsf;
                        let phase = (self.m3.subcc_scx_apply_cc as i64
                            - self.m3.subcc_last_tn_cc as i64).rem_euclid(step);
                        phase == (1i64 << dsf)
                    };
                    // DS two-tile straddle gate: a low-X sprite on the line
                    // shifts the BG fetch phase one tile while the DS FIFO
                    // carries an extra tile, so the OLD->NEW scx boundary lands
                    // one tile LATER than the non-sprite DS cadence and the
                    // in-flight straddle tile stays OLD instead of flipping to
                    // NEW (with a further one-tile LY0 shift handled below).
                    // The non-sprite DS cases (lowspr==0) are a single-tile
                    // straddle handled correctly by the NEW rewrite below and
                    // MUST keep it.
                    let ds_two_tile = dsf == 1
                        && mmio.is_cgb_features_enabled()
                        && self.sprites_on_line.iter().any(|s| s.x <= 16);
                    if flip {
                        let new_col = (((self.m3.subcc_scx_new as u16) + xpos + cgb_adj as u16) / 8) % 32;
                        let old_col = (((self.m3.subcc_scx_old as u16) + xpos + cgb_adj as u16) / 8) % 32;
                        if ds_two_tile {
                            // DS spx straddle: a low-X sprite shifts the BG
                            // fetch phase one tile while the DS FIFO carries an
                            // extra tile, so the OLD->NEW scx boundary lands one
                            // tile LATER than the non-sprite DS cadence. The
                            // in-flight straddle tile -- which the non-sprite DS
                            // flip would push to the NEW scx -- actually plots
                            // just before the boundary, so it stays the OLD scx
                            // (natural xpos column) on EVERY line. On the first
                            // rendered line (LY==0) the boundary lands one tile
                            // later still, so the NEXT tile (already fetched
                            // under the NEW scx) must also revert to the OLD scx;
                            // on LY>=1 that next tile keeps the NEW scx.
                            if old_col != new_col {
                                let bg_y = (self.latch.scy_delayed as u16
                                    + mmio.read(LY) as u16) & 0xFF;
                                let pixels = self.bg_pixels_at_col(mmio, old_col, bg_y);
                                let off = (xpos as usize).saturating_sub(self.x as usize);
                                self.fetcher.pixel_fifo.overwrite_at(off, &pixels);
                            }
                            // First-line second-tile revert: on LY==0 the
                            // fetcher dispatch can land the OLD->NEW boundary
                            // one tile later than on LY>=1, so the second
                            // straddle tile (already fetched NEW) reverts to
                            // OLD. Whether that one-tile shift happens depends
                            // on the sprite-fetch sub-tile phase: an even
                            // shifting sprite x consumes the extra dot that
                            // pushes the second tile's fetch past the apply on
                            // LY0 (sprite x==2), an odd one does not (x==1),
                            // so the revert is gated on the low sprite x parity.
                            let lowspr_even = self
                                .sprites_on_line
                                .iter()
                                .filter(|s| s.x <= 16)
                                .map(|s| s.x)
                                .min()
                                .is_some_and(|x| x % 2 == 0);
                            if mmio.read(LY) == 0 && lowspr_even {
                                self.m3.ds_straddle_next_old = true;
                                armed_this_event = true;
                            }
                        } else if new_col != old_col {
                            let bg_y = (self.latch.scy_delayed as u16
                                + mmio.read(LY) as u16) & 0xFF;
                            let pixels = self.bg_pixels_at_col(mmio, new_col, bg_y);
                            self.fetcher.pixel_fifo.overwrite_newest(&pixels);
                        }
                    } else if dsf == 0
                        && mmio.is_cgb_features_enabled()
                        && gap == 1
                        && self.sprites_on_line.iter().any(|s| s.x >= 1 && s.x <= 8)
                    {
                        // First rendered line (LY=0) straddle, CGB SS: the
                        // line after LCD-enable runs its mode-3 fetcher
                        // through a different warmup/dispatch phase, so the
                        // write's apply lands one fetcher step EARLIER
                        // relative to the in-flight tile (gap==1 here vs
                        // gap==5 on LY>=1, same xpos). The armed tile stays
                        // OLD (it plots just before the boundary), AND the
                        // NEXT tile -- which the per-dot fetcher already
                        // read NEW because the first-line dispatch lags the
                        // boundary by one tile -- must be reverted to OLD so
                        // the OLD->NEW boundary lands one tile later, exactly
                        // as the hardware first-line xpos
                        // does. On LY>=1 (gap==5) this revert does NOT fire,
                        // so those lines keep the boundary one tile earlier.
                        self.m3.subcc_revert_next_old = true;
                        armed_this_event = true;
                    }
                }
                // Sprite-shifted revert: the tile pushed right after the
                // armed straddle tile was fetched with the NEW scx one tile
                // too early (FIFO depth 8 vs 9 due to a sprite-fetch dot);
                // rewrite its 8 entries back to the OLD-scx column so the
                // OLD->NEW boundary lands one tile later (matching the hardware
                // fetcher-xpos boundary).
                if self.m3.subcc_revert_next_old
                    && !armed_this_event
                    && matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo)
                {
                    self.m3.subcc_revert_next_old = false;
                    let (xpos, cgb_adj, _) = self.fetcher.subcc_last_column_inputs();
                    let new_col = (((self.m3.subcc_scx_new as u16) + xpos + cgb_adj as u16) / 8) % 32;
                    let old_col = (((self.m3.subcc_scx_old as u16) + xpos + cgb_adj as u16) / 8) % 32;
                    if new_col != old_col {
                        let bg_y = (self.latch.scy_delayed as u16
                            + mmio.read(LY) as u16) & 0xFF;
                        let pixels = self.bg_pixels_at_col(mmio, old_col, bg_y);
                        self.fetcher.pixel_fifo.overwrite_newest(&pixels);
                    }
                }
                // DS two-tile straddle, SECOND tile (LY0 only): this tile was
                // fetched under the NEW scx (the per-dot fetcher advanced past
                // the apply) but on the first rendered line the OLD->NEW
                // boundary lands one tile later, so it plots under the OLD scx
                // at its natural column. Rewrite it in place by exact display
                // offset (xpos - self.x) so the low-X sprite's FIFO shift does
                // not misplace it.
                if self.m3.ds_straddle_next_old
                    && !armed_this_event
                    && matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo)
                {
                    self.m3.ds_straddle_next_old = false;
                    let (xpos, cgb_adj, _) = self.fetcher.subcc_last_column_inputs();
                    let new_col2 = (((self.m3.subcc_scx_new as u16) + xpos + cgb_adj as u16) / 8) % 32;
                    let old_col2 = (((self.m3.subcc_scx_old as u16) + xpos + cgb_adj as u16) / 8) % 32;
                    if new_col2 != old_col2 {
                        let bg_y = (self.latch.scy_delayed as u16
                            + mmio.read(LY) as u16) & 0xFF;
                        let pixels = self.bg_pixels_at_col(mmio, old_col2, bg_y);
                        let off = (xpos as usize).saturating_sub(self.x as usize);
                        self.fetcher.pixel_fifo.overwrite_at(off, &pixels);
                    }
                }
                // First-tile (f1) prologue straddle (DMG SS): the in-flight
                // 2nd tile -- whose column was latched under the OLD scx one
                // dot before a mid-prologue (x==0) SCX write -- just pushed.
                // On hardware it plots after the write, so re-key its 8 newest
                // FIFO entries to the NEW scx column (the first queued tile,
                // pushed before the write, keeps OLD). Uses the fetcher's exact
                // latched xpos/cgb_adj so the column matches the hardware
                // plot-time sample.
                if self.m3.prologue_rekey_armed
                    && matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo)
                {
                    self.m3.prologue_rekey_armed = false;
                    let (xpos, cgb_adj, _) = self.fetcher.subcc_last_column_inputs();
                    let new_col = (((self.m3.subcc_scx_new as u16) + xpos + cgb_adj as u16) / 8) % 32;
                    let old_col = (((self.m3.subcc_scx_old as u16) + xpos + cgb_adj as u16) / 8) % 32;
                    if new_col != old_col {
                        let bg_y = (self.latch.scy_delayed as u16
                            + mmio.read(LY) as u16) & 0xFF;
                        let pixels = self.bg_pixels_at_col(mmio, new_col, bg_y);
                        self.fetcher.pixel_fifo.overwrite_newest(&pixels);
                    }
                }
                self.record_fetch_debug_event(event, mmio);
        }
        push_this_dot
    }

    #[inline(always)]
    pub(in crate::ppu) fn step_mode3_dot(&mut self, mmio: &mut mmio::Mmio, fast: bool) {
        // Shift the DMG WE per-dot visibility history (see we_dot_hist).
        self.we_dot_hist = [
            self.lcdc_has(LCDCFlags::WindowDisplayEnable),
            self.we_dot_hist[0],
            self.we_dot_hist[1],
            self.we_dot_hist[2],
            self.we_dot_hist[3],
        ];
        // A mid-mode-3 WX change before the window starts invalidates the
        // closed-form schedule; fall back to the live emergent transition.
        // The `win_wx_enable_resolved` latch suppresses re-entry on the dots
        // after a clean WX-enable was handled (the WX != arm-WX condition
        // stays true every subsequent dot until the window draws).
        self.mode3_rekey_wx_change(mmio, fast);
        // late_wx: a mid-mode-3 WX write AFTER the window has started,
        // moving WX out of range, cancels the remaining window draw and
        // refunds the unaccrued StartWindowDraw penalty from the
        // read-at-cc mode-0 time. Graduated like late_disable (one accrued dot
        // per drawn window dot, capped at WIN_M3_PENALTY); a nonzero SCX
        // fine-scroll prefix advances the accrual one dot. WX<7 windows
        // (immediate x==0 start) lock at win_start (no refund once
        // started). CGB single-speed / no sprites; live pipeline
        // untouched; applied once per line.
        // DMG late-WX window-disable refund. DMG is BINARY (not graduated like
        // CGB): a WX-out-of-range write that lands BEFORE the window-tile
        // commit (`ws + scx&7 + 2` dots into the x==0 window draw) fully
        // refunds WIN_M3_PENALTY from the read-at-cc mode-0 time so the FF41 read
        // resolves the no-window mode-0 boundary; at/after the commit the
        // window-inclusive mode-0 time captured at M3 arm is kept (mode 3). The
        // late_wx_scx{2,3,5}_{1,2} DMG reps bracket the per-SCX commit: at the
        // 4-dots-in write, scx0/scx2 already committed (out3, keep) while
        // scx3/scx5 have not (out0, refund); the 8-dots-in write is always
        // committed (out3). WX<7 immediate-start windows lock at win_start
        // (no refund). DMG / no sprites / SS.
        if !fast
            && self.m0.m0_time_master.is_some()
            && self.window_started_this_line
            && !mmio.is_cgb_features_enabled()
            && self.sprites_on_line.is_empty()
            && mmio.read(WX) != self.m0.m3_scheduled_wx
            && !self.win_wx_penalty_resolved
            && (self.m0.m3_scheduled_wx as i32) >= 7
        {
            let wx_now = mmio.read(WX) as i32;
            let wx_in_range = (0..=166).contains(&wx_now);
            if let (Some(ws), Some(m0t)) = (self.win_start_dot, self.m0.m0_time_master)
                && !wx_in_range
            {
                let commit = ws as i64 + (self.m3.m3_arm_scx & 7) as i64 + 2;
                if (self.ticks as i64) < commit {
                    self.m0.m0_time_master =
                        Some((m0t as i64 - WIN_M3_PENALTY as i64).max(0) as u64);
                }
                self.win_wx_penalty_resolved = true;
            }
        }
        else if self.m0.m0_time_master.is_some()
            && self.window_started_this_line
            && mmio.is_cgb_features_enabled()
            && !mmio.is_double_speed_mode()
            && self.sprites_on_line.is_empty()
            && mmio.read(WX) != self.m0.m3_scheduled_wx
            && !self.win_wx_penalty_resolved
        {
            let wx_now = mmio.read(WX) as i32;
            let wx_in_range = (0..=166).contains(&wx_now);
            if let (Some(ws), Some(m0t)) = (self.win_start_dot, self.m0.m0_time_master)
                && !wx_in_range
            {
                if (self.m0.m3_scheduled_wx as i32) < 7 {
                    // Immediate-start window: penalty already locked.
                    self.win_wx_penalty_resolved = true;
                } else {
                    let scx_bias = if (self.m3.m3_arm_scx & 7) != 0 { 1 } else { 0 };
                    // SCX > 3 fine-scroll: the x==0 window's StartWindowDraw
                    // penalty accrual begins later than win_start_dot by two
                    // dots per extra discarded SCX dot (the mode-3-start dispatch
                    // runs the window-tile fetch that much later). Without
                    // this the scx5 boundary is 4 dots too early and the
                    // late_wx_scx5_1 refund is fully accrued (drops to 0).
                    let scx_late = 2 * (((self.m3.m3_arm_scx & 7) as i64) - 3).max(0);
                    let drawn = (self.ticks as i64) - ws as i64 + scx_bias - scx_late;
                    let accrued = drawn.clamp(0, WIN_M3_PENALTY as i64);
                    let refund = WIN_M3_PENALTY as i64 - accrued;
                    self.m0.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                    self.win_wx_penalty_resolved = true;
                }
            }
        }
        // Double-speed late-WX window-disable refund. Unlike single speed
        // (graduated per drawn dot), the DS StartWindowDraw penalty is BINARY:
        // a WX-out-of-range write that lands BEFORE the window-tile commits
        // (`ws + scx&7 + 1` dots into the window draw) fully refunds the
        // WIN_M3_PENALTY (<<1 cc at DS), so the FF41 read resolves the
        // no-window mode-0 boundary; at/after the commit the penalty is locked
        // and the window-inclusive mode-0 time (captured at arm) is kept. cctracer
        // ground truth: late_wx_scx5_ds_1 (write 2 dots into the x==0 window,
        // scx5) takes the full 12-cc refund -> mode 0 (out0); the `_ds_2` reps
        // (write 2 dots later, or scx0 1 dot in) keep the full mode-0 time -> mode 3
        // (out3). CGB / no sprites; live pipeline untouched, only read-at-cc.
        else if self.m0.m0_time_master.is_some()
            && self.window_started_this_line
            && mmio.is_cgb_features_enabled()
            && mmio.is_double_speed_mode()
            && self.sprites_on_line.is_empty()
            && mmio.read(WX) != self.m0.m3_scheduled_wx
            && !self.win_wx_penalty_resolved
            && (self.m0.m3_scheduled_wx as i32) >= 7
        {
            let wx_now = mmio.read(WX) as i32;
            let wx_in_range = (0..=166).contains(&wx_now);
            if let (Some(ws), Some(m0t)) = (self.win_start_dot, self.m0.m0_time_master)
                && !wx_in_range
            {
                let commit = ws as i64 + (self.m3.m3_arm_scx & 7) as i64 + 1;
                if (self.ticks as i64) < commit {
                    let refund = (WIN_M3_PENALTY as i64) << 1;
                    self.m0.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                }
                self.win_wx_penalty_resolved = true;
            }
        }
        // ATOMIC mode-3 END: mode 3 ends at the exact closed-form mode-0 time
        // (master cc), and EVERYTHING (eager FF41 mode register, mode-0
        // STAT check, VRAM/OAM/cgbp unblock, m0 IRQ) is driven off this one
        // boundary. The pixel pipeline is now image-only: at the transition
        // we flush any remaining FIFO pixels to x==160 so the visible line
        // is complete, and the pipeline's own x==160 push no longer drives
        // timing. When no closed-form mode-0 time exists (first line after
        // enable / mid-M3 invalidation), fall back to the live x==160 push.
        if let Some(m0t) = self.m0.m0_time_master
            && mmio.master_cc() >= m0t {
                self.m0.scheduled_mode0_dot = None;
                // Timing report (FF41 mode-0, STAT/m0 IRQ) fires at the exact
                // mode-0 time regardless of pixel progress.
                if !self.m0.mode0_reported_this_line {
                    self.m0.mode0_reported_this_line = true;
                    Self::set_lcd_status_mode(mmio, 0);
                }
                // Flush remaining FIFO pixels to fill all 160 columns; the
                // pipeline may lag the closed-form boundary by a few dots.
                while self.x < 160 && self.draw_fifo_pixel(mmio) {}
                // On window-start lines the window fetch restart can leave
                // the FIFO momentarily empty at mode-0 time (the last 1-2 window
                // pixels are still being fetched). The timing has already
                // been reported above; keep the renderer alive (image-only)
                // until x==160 so the final window pixel is drawn, then enter
                // HBlank via the x==160 fallback below. For all other lines
                // the flush completed the line, so end mode 3 now.
                if !((self.window_started_this_line || self.win_weoff_deferred_tail)
                    && self.x < 160)
                {
                    // DMG wx==166 pixel output-at-xpos166 (mode-3 end). See
                    // apply_dmg_wxa6_lineend_windraw.
                    self.apply_dmg_wxa6_lineend_windraw(mmio, mmio.is_cgb_features_enabled());
                    self.cgb_train_reresolve(mmio);
                    self.win_train_reresolve(mmio);
                    self.resolve_bgp_spikes(mmio);
                    // Leaving mode 3: drop any leftover preamble fast budget so the
                    // next line recomputes against the fresh schedule.
                    self.fast_dots_left = 0;
                    self.state = State::HBlank;
                    return;
                }
            }

        // The hardware mode-3-start fine-scroll break resolution. The f1 loop
        // runs xpos = 0,1,2,... one per M3 dot, re-reading p.scx each
        // step, and breaks (fixing the discard count) at the first xpos
        // with xpos%8 == scx%8. xpos == ticks - arm dot, so reading SCX
        // here samples it at the same early M3 dots hardware does -
        // independent of the FIFO/warmup latency that delays the pops.
        // Once resolved the target is frozen, so a later SCX write past
        // the break has no effect (matching the single-write tests).
        if self.x == 0 && self.m3.m3_discard_target < 0 {
            const F1_OFFSET: i64 = -1;
            let xpos = ((self.ticks as i64 - self.m3.m3_arm_dot as i64 + F1_OFFSET).max(0)) as u32;
            // Exact-cc SCX read: sample SCX as-of this f1 dot's abs_cc
            // (honoring the CGB +2cc SCX change delay) so a mid-discard
            // write lands on the correct iteration, instead of the
            // immediate register read whose visibility depends on the
            // per-dot PPU-step-vs-CPU-write ordering within a dot.
            let scx_break_full = self.scx_f1_pending_at_cc(self.abs_cc);
            let scx_live = (scx_break_full & 0x07) as u32;
            if xpos % 8 == scx_live || xpos >= 80 {
                // The hardware mode-3-start fine-scroll phase re-reads SCX live at its case-0 tile
                // fetch, so a mid-discard SCX write that crosses a tile-column
                // boundary makes the FIRST displayed tile come from the new
                // column (scx_break/8), not the column queued into the FIFO at
                // M3 arm. When that happens, discard the whole stale first tile
                // and refetch from the live column: reset the fetcher/FIFO and
                // set the discard to scx_break%8 so the next BG fetch (which
                // derives its column from scx_delayed at x==0) lands on the
                // correct column, then trims the fine-scroll prefix. The mode-3
                // length / timing is owned by the STAT resolve (m0_time_master), so this
                // is render-only.
                // The displayed first tile's COLUMN is read at the hardware's
                // last case-0 (the greatest multiple-of-8 xpos <= break),
                // NOT at the break dot: the mode-3-start fine-scroll phase only reloads `reg1`
                // (tile number, from scx/8) when `xpos % tile_len == 0`.
                // For a break inside the first tile (xpos < 8) that is
                // xpos==0 -> the M3-arm column, so no re-fetch is needed
                // even if a later f1 dot saw a column-crossing SCX. Only a
                // break that loops PAST tile_len (xpos >= 8) reloads at
                // xpos==8 from the then-live SCX. Sample SCX at that dot.
                let case0_xpos = (xpos / 8) * 8;
                let ds_u = mmio.is_double_speed_mode() as u32;
                let back = ((xpos - case0_xpos) as u64) << ds_u;
                let scx_col_full =
                    self.scx_f1_pending_at_cc(self.abs_cc.wrapping_sub(back));
                let arm_col = ((self.m3.m3_arm_scx_full.max(0) as u16) >> 3) & 0x1F;
                let brk_col = (scx_col_full as u16 >> 3) & 0x1F;
                // CGB f1 first-tile re-fetch (both single and double speed):
                // a mid-f1 SCX write whose break column differs from the
                // armed column rewrites the first queued BG tile. The
                // sub-cc clock carries the DS sub-dot phase via the
                // `delta << ds` mode0/mode-0 time nudge below, so the same
                // re-fetch applies at double speed (the DMG mode-3-start
                // fine-scroll uses a different +1 tile-column phase the
                // discard model already matches, so it stays excluded).
                if mmio.is_cgb_features_enabled()
                    && self.m3.m3_arm_scx_full >= 0
                    && brk_col != arm_col
                {
                    // Only the FIRST queued BG tile is stale: rewrite the
                    // 8 oldest FIFO entries in place with the tile at the
                    // break column, then discard scx_break%8 fine pixels.
                    // Subsequent tiles keep their live-SCX columns (the
                    // fetcher re-reads scx_delayed), so a later SCX write
                    // that moves the steady-state column is preserved.
                    let bg_y = (self.latch.scy_delayed as u16
                        + mmio.read(LY) as u16) & 0xFF;
                    self.rewrite_first_fifo_tile(mmio, brk_col, bg_y);
                    self.m3.m3_pixels_discarded = 0;
                    self.m3.m3_discard_target = (scx_break_full & 0x07) as i8;
                    if let Some(dot) = self.m0.scheduled_mode0_dot {
                        let delta = xpos as i64 - self.m3.m3_arm_scx as i64;
                        self.m0.scheduled_mode0_dot = Some((dot as i64 + delta).max(0) as u128);
                        if let Some(m0t) = self.m0.m0_time_master {
                            let ds = mmio.is_double_speed_mode() as u32;
                            self.m0.m0_time_master =
                                Some((m0t as i64 + (delta << ds)).max(0) as u64);
                        }
                    }
                    return;
                }
                // Discard the full xpos count: a mid-discard SCX change can
                // push the break past tile_len (hardware loops on to the
                // next matching xpos), discarding more than 7 pixels.
                self.m3.m3_discard_target = xpos as i8;
                // The closed-form mode-0 schedule assumed m3_arm_scx dots
                // of discard; nudge it by the actual difference so M3 ends
                // at the right dot (the extra discards lengthen M3).
                if let Some(dot) = self.m0.scheduled_mode0_dot {
                    let delta = xpos as i64 - self.m3.m3_arm_scx as i64;
                    self.m0.scheduled_mode0_dot = Some((dot as i64 + delta).max(0) as u128);
                    if let Some(m0t) = self.m0.m0_time_master {
                        let ds = mmio.is_double_speed_mode() as u32;
                        self.m0.m0_time_master =
                            Some((m0t as i64 + (delta << ds)).max(0) as u64);
                    }
                }
            }
        }

        if self.sprite_fetch_stall > 0 {
            self.sprite_fetch_stall -= 1;
            return;
        }

        if self.fetcher.pixel_fifo.size() != 0 && self.pixel_transfer_warmup == 0 {
            self.sprite_fetch_stall = self.sprite_fetch_penalty_for_current_x(mmio).unwrap_or(0);
            if self.sprite_fetch_stall > 0 {
                self.sprite_fetch_stall -= 1;
                return;
            }
        }

        // DMG WX 1..6 EARLY window activation: the WX comparator matches
        // during the discard prologue at position WX-7 (activating while
        // position_in_line is still negative), i.e. (7-WX) dots
        // BEFORE the first visible pop. Evaluating it there — not at the
        // pos-0 trigger below — matters when WX is rewritten mid-prologue:
        // hardware activates with the OLD WX (a WX=4 activation beats a
        // WX=LY rewrite by 1-3 dots on every row). pos = ticks - (m3_arm_dot + 12 + scx&7) maps our
        // pipeline's pop timeline (even arm: TN arm+2 .. push arm+8,
        // warmup 4, first visible pop arm+12+scx). The activation then
        // runs the restart fetch on real dots (anchored cadence) and the
        // remaining (7-WX) prologue pops chop the first window tile, so
        // the first VISIBLE pixel still lands at pos-0 + 6. Exact-match
        // only; any miss falls back to the pos-0 trigger below.
        if !mmio.is_cgb_features_enabled()
            && self.x == 0
            && !self.fetcher.is_fetching_window()
            && !self.first_line_after_enable
            && self.m3.m3_discard_target >= 0
            // Comparator WE tap (see we_dot_hist): delayed, not live.
            && self.window_y_active_with(mmio, self.we_dot_hist[1] && self.we_dot_hist[2])
        {
            let wx = mmio.read(WX);
            // WX==0 with SCX&7==0 takes the same early-comparator
            // activation with chop 7 (window column 7 lands at screen
            // x0 — the WX=0 window's left 7 columns are off-screen).
            // SCX&7>0 keeps the pos-0 trigger + one-dot delay quirk
            // (win_wx0_delayed).
            if (1..7).contains(&wx) || (wx == 0 && self.m3.m3_discard_target == 0) {
                let s = self.m3.m3_discard_target as i64;
                // pos-0 dot (first visible pop absent windows): TN runs
                // at the first even dot after arm, push +6, warmup 4,
                // + the scx fine discard pops.
                let base = self.m3.m3_arm_dot as i64 + 12 - (self.m3.m3_arm_dot & 1) as i64
                    + s;
                // The comparator's activation dot is pos == WX-7, but a
                // CPU WX store's new value reaches the comparator within
                // the same dot on hardware while our mmio only exposes it
                // to the NEXT dot — so evaluate one dot later (pos ==
                // WX-6) with the then-visible WX. This brackets the
                // rewrite race: a WX=6->LY rewrite one dot after the pos -1
                // match must WIN (no window starts), while a WX=4/5 must
                // LOSE (window starts with the old WX 4/5).
                let pos = self.ticks as i64 - base;
                if pos == wx as i64 - 6 {
                    self.begin_window_draw(0);
                    self.m3_sprite_prev_tile = SPRITE_TILE_NONE;
                    if self.win_start_dot.is_none() {
                        self.win_start_dot = Some(self.ticks);
                    }
                    // Remaining prologue pops become the first-tile chop;
                    // the warmup/scx-discard bookkeeping is superseded
                    // (their dots are consumed by the restart fetch).
                    self.win_first_tile_chop = 7 - wx;
                    self.pixel_transfer_warmup = 0;
                    self.m3.m3_pixels_discarded = self.m3.m3_discard_target as u8;
                    // The activation dot itself was one dot ago: its
                    // TileNumber is due now (catch-up), low/high/push at
                    // +1/+3/+5 via the anchored cadence.
                    self.wg_set_anchor(0);
                    let fls = self.wg_apply(self.fetcher_lcdc_state());
                    if let Some(event) = self.fetcher.step(
                        mmio,
                        fls,
                        crate::ppu::fetcher::FetchPos {
                            window_line: self.win_y_pos,
                            display_x: self.x,
                            pending_discard: 0,
                            scy: self.latch.scy_delayed,
                            scx: self.latch.scx_delayed,
                        },
                    ) {
                        if matches!(
                            event.kind,
                            crate::ppu::fetcher::FetcherDebugEventKind::TileNumber
                        ) {
                            self.m3.subcc_last_tn_cc = self.abs_cc;
                        }
                        self.record_fetch_debug_event(event, mmio);
                    }
                    self.win_fetch_anchor = Some(self.ticks.wrapping_sub(1));
                    return;
                }
            }
        }

        // Whether this dot executed a PushToFIFO fetch substep — the
        // Fetcher cadence: on CGB, decouple from absolute self.ticks so that
        // sprite-fetch stall dots don't flip the fetcher's even/odd phase
        // (matches hardware). On DMG, keep the original self.ticks gate.
        let cadence_even = if mmio.is_cgb_features_enabled() {
            let even = self.fetcher_cadence_tick.is_multiple_of(2);
            self.fetcher_cadence_tick = self.fetcher_cadence_tick.wrapping_add(1);
            even
        } else if let Some(anchor) = self.win_fetch_anchor {
            // Window-startup fetch: phase-locked to the trigger dot so
            // the first window pixel pops exactly 6 dots after it.
            self.ticks.wrapping_sub(anchor).is_multiple_of(2)
        } else {
            self.ticks.is_multiple_of(2)
        };

        // DMG mid-mode-3 WE-off window kill (the hardware TileNumber-T1
        // window-trigger clear): the window fetcher re-samples the
        // window-enable bit at each TileNumber step with a one-dot
        // delayed sample (we_dot_hist[2]); reading OFF reverts the fetch
        // to BG from THIS tile on (the already-pushed window pixels in
        // the FIFO drain out, so a killed window always shows a multiple
        // of 8 pixels). A WE-off pulse short enough that its delayed
        // sample misses every TileNumber dot leaves the window running.
        // (An implementation that latched the window-draw state at the write would
        // instead kill the window on any pulse.)
        if cadence_even
            && !mmio.is_cgb_features_enabled()
            && self.fetcher.is_fetching_window()
            && self.fetcher.fetch_state_is_tile_number()
            && !self.we_dot_hist[if self.win_kill_tap_late { 3 } else { 2 }]
        {
            self.fetcher.stop_window_with_extra(0);
            self.window_started_this_line = false;
            self.win_being_fetched = false;
        }

        // DMG BG fetch-grid origin (see bg_wg_apply): the line's first
        // BG TileNumber read runs on this dot, before any sprite stall.
        if cadence_even
            && self.wg.bg_anchor_cc.is_none()
            && self.wg.bg_anchor_dot.is_none()
            && !self.fetcher.is_fetching_window()
            && self.fetcher.fetch_state_is_tile_number()
            && self.fetcher.get_tile_index() == 0
        {
            // Line-relative twin of bg_anchor_cc, recorded on every model:
            // the CGB WE-off revert column (see handle_lcdc_write) resolves
            // the fetch grid in dots-since-line-start, so it needs `ticks`,
            // not the master clock.
            self.wg.bg_anchor_dot = Some(self.ticks);
            if !mmio.is_cgb_features_enabled() {
                self.wg.bg_anchor_cc = Some(self.abs_cc);
            }
        }
        let fetcher_lcdc_state =
            self.bg_wg_apply(self.wg_apply(self.fetcher_lcdc_state()), mmio.read(LY));
        // Pixels still to be discarded for SCX fine-scroll: they sit in
        // the FIFO but won't be displayed, so the BG tile column (derived
        // from display_x + FIFO depth) must not count them.
        let pending_discard = if self.x == 0 {
            (self.m3.m3_discard_target.max(0) as u8).saturating_sub(self.m3.m3_pixels_discarded)
        } else {
            0
        };
        self.mode3_fetch_step(mmio, cadence_even, fetcher_lcdc_state, pending_discard);

        if self.fetcher.pixel_fifo.size() == 0 {
            return;
        }

        if self.pixel_transfer_warmup > 0 {
            self.pixel_transfer_warmup -= 1;
            return;
        }

        // DMG deferred WX-comparator commit (see dmg_wx_trigger_pending):
        // the exact x+7==wx match armed on the previous dot commits now
        // iff WX still reads the matched value — the hardware comparator
        // samples WX through the end of the CPU store's M-cycle, so a
        // store landing on the commit dot kills the match. The restart is
        // executed as-of the arm dot (TileNumber catch-up + anchor one
        // dot back), byte-identical to the immediate start for stable WX.
        if !mmio.is_cgb_features_enabled()
            && let Some((arm_dot, arm_wx)) = self.dmg_wx_trigger_pending.take()
            && self.ticks == arm_dot.wrapping_add(1)
                && mmio.read(WX) == arm_wx
                && self.x + 7 == arm_wx
                && !self.fetcher.is_fetching_window()
            {
                self.begin_window_draw(self.x);
                self.win_first_tile_chop = 0;
                // The activation dot was one dot ago: its TileNumber is
                // due now (catch-up); low/high/push at +1/+3/+5 via the
                // anchored cadence.
                self.wg_set_anchor(1);
                let fls = self.wg_apply(self.fetcher_lcdc_state());
                if let Some(event) = self.fetcher.step(
                    mmio,
                    fls,
                    crate::ppu::fetcher::FetchPos {
                        window_line: self.win_y_pos,
                        display_x: self.x,
                        pending_discard: 0,
                        scy: self.latch.scy_delayed,
                        scx: self.latch.scx_delayed,
                    },
                ) {
                    if matches!(
                        event.kind,
                        crate::ppu::fetcher::FetcherDebugEventKind::TileNumber
                    ) {
                        self.m3.subcc_last_tn_cc = self.abs_cc;
                    }
                    self.record_fetch_debug_event(event, mmio);
                }
                self.win_fetch_anchor = Some(self.ticks.wrapping_sub(1));
                self.m3_sprite_prev_tile = SPRITE_TILE_NONE;
                if self.win_start_dot.is_none() {
                    self.win_start_dot = Some(self.ticks.wrapping_sub(1));
                }
                return;
            }
            // else: canceled — the WX store on the commit dot rewrote the
            // comparator input; no window starts (fall through).

        // Check if we should start window rendering. On DMG the
        // window-enable bit feeding the WX comparator is the DELAYED
        // per-dot tap (we_dot_hist, samples one and two dots back) —
        // an 8-cycle WE-off pulse blocks 9 consecutive comparator dots
        // on hardware. CGB keeps the live bit. When the x==0 trigger
        // fires with SCX fine discards still pending, our check runs
        // `pending` dots BEFORE the hardware comparator dot (position 0
        // pops that much later), so the taps shift toward the present
        // accordingly (a disable right before the x==0 check dot must
        // still block the start).
        let trigger_we = if mmio.is_cgb_features_enabled() {
            self.lcdc_has(LCDCFlags::WindowDisplayEnable)
        } else {
            let pending = if self.x == 0 && self.m3.m3_discard_target >= 0 {
                (self.m3.m3_discard_target as u8)
                    .saturating_sub(self.m3.m3_pixels_discarded)
            } else {
                0
            };
            match pending {
                0 => self.we_dot_hist[1] && self.we_dot_hist[2],
                1 => self.we_dot_hist[0] && self.we_dot_hist[1],
                _ => self.we_dot_hist[0],
            }
        };
        if self.mode3_activate_window(mmio, trigger_we) {
            return;
        }

        // WX<7 chopped window start: the prologue discard pops that ran
        // past the (earlier) activation position chop the first window
        // tile's leading pixels, one per dot (see win_first_tile_chop).
        if self.x == 0 && self.win_first_tile_chop > 0 {
            if self.fetcher.pixel_fifo.pop().is_ok() {
                self.win_first_tile_chop -= 1;
                self.win_being_fetched = false;
            }
            return;
        }

        // SCX fine-scroll discard (the mode-3-start fine-scroll per-dot loop):
        // while x == 0, re-read the LIVE SCX each dot. If we have not
        // yet discarded `scx % 8` BG pixels, pop one and consume the
        // dot. A mid-M3 SCX write changes this count (and the fetched
        // tile column, since TileNumber re-reads SCX live).
        if self.x == 0 {
            // Hold output until the f1 break is resolved (target latched).
            if self.m3.m3_discard_target < 0 {
                return;
            }
            let target = self.m3.m3_discard_target as u8;
            // WE-off insert glitch, prologue variant: the inserted
            // color-0 pixel sits at the FRONT of the stream and is the
            // first pixel this discard dot drops — no real FIFO pixel
            // is consumed, so one extra leading BG pixel survives and
            // the visible line shifts right by one.
            if self.m3.m3_pixels_discarded < target && self.we_glitch_discard_insert {
                self.we_glitch_discard_insert = false;
                self.m3.m3_pixels_discarded += 1;
                self.win_being_fetched = false;
                return;
            }
            // A full-width HUD window (WX==7) triggers at LX==0 via the
            // live x+7==wx match and resets the FIFO. On hardware the
            // SCX&7 fine-scroll discard consumes the leading BACKGROUND
            // pixels before LX reaches 0, so a window activating exactly
            // at LX==0 is unaffected by it and draws from window-x 0 —
            // the bar stays locked to screen coordinates regardless of
            // SCX. rustyboi's trigger fires just before this discard and
            // clears the FIFO, so without this guard the discard wrongly
            // pops window pixels and the bar shifts left by SCX&7 (moving
            // with the camera one frame per horizontal scroll).
            //
            // Narrowly WX==7: WX<7 triggers at LX<0, inside the discard
            // region, so it legitimately keeps the discard (mealybug
            // m3_window_timing_wx_0 shifts the WX=0 window); the DMG wxA6
            // (WX==166) checkpoint window comes through the mode-3-start
            // path — flagged by win_draw_started_at_x0 — and keeps it too
            // (gambatte wxA6_scx7).
            let win_x0_locked = self.fetcher.is_fetching_window()
                && !self.win_draw_started_at_x0
                && mmio.read(WX) == 7;
            if self.m3.m3_pixels_discarded < target
                && !win_x0_locked
                && let Ok(_) = self.fetcher.pixel_fifo.pop() {
                    self.m3.m3_pixels_discarded += 1;
                    self.win_being_fetched = false;
                    return;
            }
        }

        // Put a pixel from the FIFO on screen with sprite mixing.
        // Stop visible output at x==160; the scheduled dot ends Mode 3.
        if self.x >= 160 {
            return;
        }
        // DMG window reactivation zero pixel (the hardware BG-pixel insert):
        // the WX comparator matches again with the window already active
        // (past its startup fetch), exactly at the pop of a window tile's
        // FIRST pixel. That pop is the dot on which the FIFO still holds all 8
        // pushed pixels; it is NOT necessarily the push dot itself, because a
        // sprite fetch can stall the renderer across the push (the insert
        // diagonal sits at x == 8k + (8 - chop)). The pop below then renders a
        // color-0 pixel WITHOUT consuming the FIFO, inserting one pixel into
        // the line.
        if !mmio.is_cgb_features_enabled()
            && self.window_started_this_line
            && self.fetcher.is_fetching_window()
            && !self.win_being_fetched
            && self.fetcher.pixel_fifo.size() == 8
            && mmio.read(WX) == self.x + 7
        {
            self.insert_bg_pixel = true;
        }
        // DMG WE-off zero-pixel insertion glitch: with the window Y-latch
        // triggered but the window enable OFF (delayed tap, see
        // we_dot_hist), a tile-boundary pop (the push-at-empty dot; our
        // queued first-pixel x) where WX == x+7 renders one color-0 pixel
        // WITHOUT consuming the FIFO (a single white pixel at x = WX-7 on
        // the trigger-missed rows).
        // Pan Docs: Window mid-frame behavior — https://gbdev.io/pandocs/Window.html
        let mut at_tile_boundary = false;
        for slot in self.we_glitch_tile_starts.iter_mut() {
            if let Some(fx) = *slot {
                if fx == self.x {
                    at_tile_boundary = true;
                    *slot = None;
                } else if fx < self.x {
                    // Stale (chop/discard consumed the boundary pop).
                    *slot = None;
                }
            }
        }
        // Pre-CGB machines only (!is_cgb): the CGB PPU has no WE-off
        // insert glitch even in DMG-compat mode (the line is unshifted).
        if !mmio.is_cgb()
            && self.window_y_triggered
            && !self.fetcher.is_fetching_window()
            && !self.we_dot_hist[2]
            && !self.we_insert_suppressed
            && at_tile_boundary
            && mmio.read(WX) == self.x + 7
        {
            self.insert_bg_pixel = true;
            // The inserted pixel shifts every later boundary one to the
            // right.
            for fx in self.we_glitch_tile_starts.iter_mut().flatten() {
                *fx = fx.saturating_add(1);
            }
        }
        if self.draw_fifo_pixel(mmio) && self.x == 160 {
            // Fallback end-of-mode-3 at the x==160 pixel push, used in two
            // distinct cases:
            // (a) no closed-form mode-0 time exists (first line after enable /
            // mid-M3 invalidation): report mode 0 here and end mode 3.
            // (b) the mode-0 time timing report ALREADY fired above, but the
            // window fetch restart left the FIFO short, so the renderer
            // was kept alive to draw the final window pixel; now that
            // x==160 we end mode 3 WITHOUT re-reporting (the FF41 mode-0
            // poke / STAT IRQ already fired at the exact mode-0 time).
            // When mode-0 time is known and the FIFO was complete, the transition
            // is driven off master_cc above and the renderer never reaches
            // this x==160 fallback before that boundary, so we must NOT end
            // mode 3 early here on ordinary (non-window) lines.
            let window_deferred = (self.window_started_this_line
                || self.win_weoff_deferred_tail)
                && self.m0.mode0_reported_this_line;
            if self.m0.m0_time_master.is_none() {
                self.apply_dmg_wxa6_lineend_windraw(mmio, mmio.is_cgb_features_enabled());
                self.resolve_bgp_spikes(mmio);
                // Leaving mode 3: drop any leftover preamble fast budget so the
                // next line recomputes against the fresh schedule.
                self.fast_dots_left = 0;
                self.state = State::HBlank;
                if !self.m0.mode0_reported_this_line {
                    self.m0.mode0_reported_this_line = true;
                    Self::set_lcd_status_mode(mmio, 0);
                }
            } else if window_deferred {
                self.apply_dmg_wxa6_lineend_windraw(mmio, mmio.is_cgb_features_enabled());
                self.resolve_bgp_spikes(mmio);
                // Leaving mode 3: drop any leftover preamble fast budget so the
                // next line recomputes against the fresh schedule.
                self.fast_dots_left = 0;
                self.state = State::HBlank;
            }
        }
    }
}

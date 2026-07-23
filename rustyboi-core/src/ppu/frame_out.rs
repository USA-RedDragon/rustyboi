use crate::memory::{boxed_filled, mmio, Addressable};
use crate::ppu::fetcher;
use super::controller::{
    rgb555_to_rgb888, FetchDebugEvent, FetchDebugEventKind, LCDCFlags, PixelDebugEvent, Ppu,
    RenderedFrame, SgbBorderLayers, State, FRAMEBUFFER_SIZE, LY, SGB_FRAME_HEIGHT, SGB_FRAME_SIZE,
    SGB_FRAME_WIDTH, SGB_WINDOW_X, SGB_WINDOW_Y,
};

/// Rasterize the SGB border's 32x28 tilemap, handing every non-transparent
/// pixel to `put(px, py, rgb555)`. Shared by `Ppu::sgb_composited_frame` and
/// `Ppu::sgb_border_frame`, which differ only in what lies underneath.
///
/// Map entries with bits 8-9 set reference tiles beyond the 256 that exist and
/// are not drawn (the hardware `tile & 0x300` skip). 4bpp pixel bits come from
/// byte pairs (plane 0/1) at row*2 and (plane 2/3) at row*2+16; bit 7 = leftmost
/// pixel when not X-flipped. Color 0 is transparent and is not passed to `put`.
///
/// The tilemap's palette field is 3 bits (SNES BG palettes 0-7), but a PCT_TRN
/// can only deliver palettes 4-7, so for a game-supplied border `pals` holds
/// exactly those four and the field's low 2 bits index them (4->0 .. 7->3). The
/// firmware's own border is not so constrained — SGB1's map selects palettes 0
/// and 4, SGB2's 0, 4 and 5 — so `Sgb::seed_default_border` hands over all eight
/// palettes and the full 3-bit field applies. The slice length distinguishes the
/// two.
fn draw_sgb_border_tiles(tiles: &[u8], map: &[u8], pals: &[u16], mut put: impl FnMut(usize, usize, u16)) {
    let pal_mask = if pals.len() >= 128 { 7 } else { 3 };
    for tile_y in 0..28usize {
        for tile_x in 0..32usize {
            let e = (tile_y * 32 + tile_x) * 2;
            let entry = u16::from_le_bytes([map[e], map[e + 1]]);
            if entry & 0x300 != 0 {
                continue;
            }
            let tile = (entry & 0xFF) as usize;
            let pal = ((entry >> 10) & pal_mask) as usize;
            let xf: usize = if entry & 0x4000 != 0 { 0 } else { 7 };
            let yf: usize = if entry & 0x8000 != 0 { 7 } else { 0 };
            for y in 0..8usize {
                let base = tile * 32 + (y ^ yf) * 2;
                for x in 0..8usize {
                    let bit = 1u8 << (x ^ xf);
                    let color = usize::from(tiles[base] & bit != 0)
                        | usize::from(tiles[base + 1] & bit != 0) << 1
                        | usize::from(tiles[base + 16] & bit != 0) << 2
                        | usize::from(tiles[base + 17] & bit != 0) << 3;
                    if color == 0 {
                        continue;
                    }
                    put(tile_x * 8 + x, tile_y * 8 + y, pals[pal * 16 + color]);
                }
            }
        }
    }
}

impl Ppu {
    /// Current PPU master clock (`abs_cc`). Used by the interrupt-service LCD
    /// ack to position the IF clear at the exact dot (see
    /// `Bus::interrupt_low_push_ack`).
    pub fn abs_cc(&self) -> u64 { self.abs_cc }

    /// The accumulated STAT-phase carry (master-cc). The bus
    /// SUBTRACTS this from a CPU VRAM/OAM access cc so the render-visibility gate
    /// (`ppu_blocks` / `get_stat` fallback mode + `cpu_access_blocked`) sees the
    /// access in the un-carried fetcher geometry (the carry moved the LY time
    /// boundaries but not the fetcher's lock window). 0 when no carry is live.
    pub(crate) fn render_carry_skew(&self) -> i64 {
        self.render_carry_skew_cc
    }

    pub fn set_fetch_debug_events_enabled(&mut self, enabled: bool) {
        self.fetch_debug_events_enabled = enabled;
        if !enabled {
            self.fetch_debug_events.clear();
            self.pixel_debug_events.clear();
        }
    }

    pub fn take_fetch_debug_events(&mut self) -> Vec<FetchDebugEvent> {
        std::mem::take(&mut self.fetch_debug_events)
    }

    pub fn take_pixel_debug_events(&mut self) -> Vec<PixelDebugEvent> {
        std::mem::take(&mut self.pixel_debug_events)
    }

    #[inline]
    pub(in crate::ppu) fn record_fetch_debug_event(&mut self, event: fetcher::FetcherDebugEvent, mmio: &mmio::Mmio) {
        if !self.fetch_debug_events_enabled {
            return;
        }
        self.record_fetch_debug_event_slow(event, mmio);
    }

    fn record_fetch_debug_event_slow(&mut self, event: fetcher::FetcherDebugEvent, mmio: &mmio::Mmio) {
        let kind = match event.kind {
            fetcher::FetcherDebugEventKind::TileNumber => FetchDebugEventKind::TileNumber,
            fetcher::FetcherDebugEventKind::TileDataLow => FetchDebugEventKind::TileDataLow,
            fetcher::FetcherDebugEventKind::TileDataHigh => FetchDebugEventKind::TileDataHigh,
            fetcher::FetcherDebugEventKind::PushToFifo => FetchDebugEventKind::PushToFifo,
        };

        self.fetch_debug_events.push(FetchDebugEvent {
            kind,
            ppu_ticks: self.ticks,
            x: self.x,
            ly: mmio.read(LY),
            fifo_size: event.fifo_size,
            tile_index: event.tile_index,
            tile_num: event.tile_num,
            tile_attributes: event.tile_attributes,
            tile_line: event.tile_line,
            addr: event.addr,
            value: event.value,
            lcdc: event.lcdc,
            tile_index_is_tile_data: event.tile_index_is_tile_data,
            fetching_window: event.fetching_window,
        });
    }

    pub(in crate::ppu) fn record_pixel_debug_event(&mut self, ly: u8, bg_pixel_idx: u8, rgb: [u8; 3]) {
        if !self.fetch_debug_events_enabled {
            return;
        }

        self.pixel_debug_events.push(PixelDebugEvent {
            ppu_ticks: self.ticks,
            x: self.x,
            ly,
            bg_pixel_idx,
            rgb,
            lcdc: self.lcdc,
        });
    }

    pub fn frame_ready(&self) -> bool {
        self.have_frame
    }

    /// The completed DMG shade-index frame (the back buffer `get_frame`
    /// serves). The SGB *_TRN readout captures from this: the real SGB
    /// re-digitizes the displayed video signal, not the GB's VRAM.
    pub(crate) fn dmg_shade_frame(&self) -> &[u8; FRAMEBUFFER_SIZE] {
        &self.fb_b
    }

    /// The *presented* DMG shade-index frame: the mono output `get_frame` would
    /// serve, as palette/correction-independent shade indices, with the panel
    /// blank (LCD off / first frame after enable) and SGB mask applied — unlike
    /// [`dmg_shade_frame`](Self::dmg_shade_frame), which is the RAW rendered back
    /// buffer (what the SGB *_TRN readout re-digitizes and the STOP checks read).
    /// This is the grading-correct mono domain: it mirrors the non-colour
    /// branches of [`get_frame`](Self::get_frame). Colour models (incl. colorized
    /// SGB) are graded by RGB instead and never take this path.
    pub(crate) fn presented_dmg_shades(&self, mmio: &mmio::Mmio) -> Box<[u8; FRAMEBUFFER_SIZE]> {
        if let Some(sgb) = mmio.sgb() {
            return match self.sgb_frame(sgb) {
                RenderedFrame::Monochrome(m) => m,
                RenderedFrame::Color(_) => self.fb_b.clone(),
            };
        }
        if self.disabled || self.frames_since_enable < 2 {
            boxed_filled(0)
        } else {
            self.fb_b.clone()
        }
    }

    /// Plain-STOP (low-power) panel effect, Pan Docs "Reducing Power
    /// Consumption": entering STOP with the LCD enabled blanks a DMG panel to
    /// white (the real panel also burns a single horizontal black line —
    /// panel physics with an unpinned row, left unmodeled; the shootout
    /// reference renders plain white) and turns a CGB panel black — "Except
    /// if the LCD is in Mode 3, where it will keep drawing the current
    /// screen", so a mid-mode-3 STOP keeps the picture. The clock freeze
    /// (`gb::step_instruction`) then holds the painted back buffer on screen
    /// for the whole STOP; drawing resumes into the live front buffer on
    /// wake. LCD-off STOP (the recommended sequence) leaves the already-blank
    /// panel untouched.
    pub(crate) fn enter_stop_mode_panel(&mut self, mmio: &mmio::Mmio) {
        if self.disabled || !self.lcdc_has(LCDCFlags::DisplayEnable) {
            return;
        }
        if self.renders_color(mmio) {
            if !self.is_in_pixel_transfer() {
                self.color_fb_b.fill(0x00);
            }
        } else {
            self.fb_b.fill(0);
        }
    }

    /// Whether the CGB panel was driven with a displayed frame recently enough
    /// to still hold that image. SameBoy's `frame_repeat_countdown` (measured
    /// on CGB-E) is 144*456*2 + 3640 cycles at 8 MHz — AGB: 5982 instead of
    /// 3640 — re-armed at the start of every VBlank line 144-152 and run down
    /// in real time regardless of LCD state. `last_drive_cc` is that per-line
    /// anchor, and the repeat verdict is taken at the skipped frame's VBlank
    /// entry, so in 4 MHz-dot terms the window is 144*456 + 1820 (AGB 2991)
    /// from the anchored line start: the 144-line budget spans the skipped
    /// frame's own render and the ~4-line margin is all an LCD off may
    /// consume. A continuously running LCD re-anchors at latest on line 152,
    /// 145 lines before the next VBlank entry, so it never decays; anything
    /// off longer than the margin has decayed to blank (little-things-gb
    /// `firstwhite`'s alternating one-frame enables stay blank via
    /// `panel_holds_image`). Master cc runs at 4 MHz single speed / 8 MHz
    /// double speed, hence the shift.
    pub(in crate::ppu) fn panel_recently_driven(&self, mmio: &mmio::Mmio) -> bool {
        let margin = if mmio.is_agb() { 5982 / 2 } else { 3640 / 2 };
        let window = (144 * 456 + margin) << (mmio.is_double_speed_mode() as u32);
        self.panel_holds_image
            && mmio.master_cc().wrapping_sub(self.last_drive_cc) <= window
    }

    pub(crate) fn get_frame(&mut self, mmio: &mmio::Mmio) -> RenderedFrame {
        self.have_frame = false;
        // Hardware panel blank: the LCD off state and the first frame after an
        // enable both show "whiter than white" (blank), not the framebuffer. The
        // panel needs one fully-displayed frame after enable to resync, so a frame
        // is only shown once at least two frame boundaries have passed since the
        // enable (frames_since_enable >= 2). A ROM that enables the LCD for a single
        // frame each cycle (little-things-gb `firstwhite`, Pokemon Pinball) never
        // reaches that, so the panel stays blank. SGB keeps its own mask/border
        // compositing (handled in sgb_frame), so this blanking is gated off there.
        let blank_panel =
            mmio.sgb().is_none() && (self.disabled || self.frames_since_enable < 2);
        if self.renders_color(mmio) {
            if blank_panel {
                // CGB panel persistence: a panel whose drive countdown has not
                // expired (LCD just turned off, or re-enabled with the skipped
                // first frame still in flight) keeps showing the previous
                // image; the blank sets in 144*456 + 1820 cc (AGB 2991) after
                // the last driven VBlank-line start (see
                // `panel_recently_driven`).
                if self.panel_recently_driven(mmio) {
                    return RenderedFrame::Color(self.color_fb_b.clone());
                }
                // CGB white == RGB 0xFFFFFF.
                return RenderedFrame::Color(boxed_filled(0xFF));
            }
            RenderedFrame::Color(self.color_fb_b.clone())
        } else if let Some(sgb) = mmio.sgb() {
            // MASK_EN Freeze: latch the frame completed at the freeze and keep
            // showing it (the transfer screens games draw behind the mask stay
            // hidden); drop the latch as soon as the mask leaves Freeze.
            if matches!(sgb.mask, crate::sgb::MaskMode::Freeze) {
                if self.sgb_freeze_fb.is_none() {
                    self.sgb_freeze_fb = Some(self.fb_b.to_vec());
                }
            } else if self.sgb_freeze_fb.is_some() {
                self.sgb_freeze_fb = None;
            }
            self.sgb_frame(sgb)
        } else {
            if blank_panel {
                // DMG white == shade index 0.
                return RenderedFrame::Monochrome(boxed_filled(0));
            }
            RenderedFrame::Monochrome(self.fb_b.clone())
        }
    }

    /// Post-process the DMG shade-index framebuffer for Super Game Boy output:
    /// apply the MASK_EN screen mask and, when a palette command has run, map
    /// each pixel's DMG shade (0-3) through the SGB palette assigned to its 8x8
    /// attribute cell (producing RGB888). When no palette command has run the
    /// frame stays monochrome, matching plain-GB (grayscale) behavior — which is
    /// what the `sgb-ext-test` grayscale reference expects.
    fn sgb_frame(&self, sgb: &crate::sgb::Sgb) -> RenderedFrame {
        use crate::sgb::MaskMode;
        // MASK_EN: Freeze shows the latched pre-freeze frame; Black shows pure
        // black (the SNES blanks to color 0x0000); Color0 blanks to the shared
        // backdrop color (color 0).
        let blank = matches!(sgb.mask, MaskMode::Black | MaskMode::Color0);
        let src: &[u8] = match self.sgb_freeze_fb.as_deref() {
            Some(f) if f.len() == FRAMEBUFFER_SIZE => f,
            _ => &self.fb_b[..],
        };

        if !sgb.colorized {
            if blank {
                // Blank to shade 0 (Color0) / darkest for Black.
                let fill = if matches!(sgb.mask, MaskMode::Black) { 3 } else { 0 };
                return RenderedFrame::Monochrome(boxed_filled(fill));
            }
            let mut out: Box<[u8; FRAMEBUFFER_SIZE]> = boxed_filled(0);
            out.copy_from_slice(src);
            return RenderedFrame::Monochrome(out);
        }

        // Colorized: build an RGB888 frame from the SGB palettes.
        let mut out: Box<[u8; FRAMEBUFFER_SIZE * 3]> = boxed_filled(0);
        if matches!(sgb.mask, MaskMode::Black) {
            return RenderedFrame::Color(out);
        }
        for y in 0..144usize {
            for x in 0..160usize {
                let idx = y * 160 + x;
                let shade = if blank { 0 } else { src[idx] };
                let rgb555 = sgb.color_for(x / 8, y / 8, shade).unwrap_or(0);
                let (r, g, b) = rgb555_to_rgb888(rgb555);
                out[idx * 3] = r;
                out[idx * 3 + 1] = g;
                out[idx * 3 + 2] = b;
            }
        }
        RenderedFrame::Color(out)
    }

    /// Compose the full 256x224 Super Game Boy output: the SGB border
    /// (CHR_TRN tiles + PCT_TRN map/palettes) around the 160x144 GB screen
    /// centered at (48, 40) — border tiles x 6..26, y 5..23. RGB888,
    /// row-major.
    ///
    /// Returns None on non-SGB hardware or until the game has transferred a
    /// border (both CHR_TRN and PCT_TRN), so callers fall back to the
    /// standard 160x144 frame. This is a SEPARATE off-screen accessor:
    /// `get_frame` and the whole 160x144 path are untouched (the suite
    /// graders keep reading those), and calling this does not consume
    /// `frame_ready`.
    ///
    /// Layering (per real hardware): the SNES backdrop (shared
    /// color 0) fills everything; the GB picture (masked/frozen/colorized
    /// exactly like `sgb_frame`) sits in the center window; border pixels
    /// with a non-zero 4bpp color index draw OVER both — transparent border
    /// pixels show the GB picture inside the window and the backdrop outside.
    pub fn sgb_composited_frame(
        &self,
        mmio: &mmio::Mmio,
        uncolorized: [u16; 4],
    ) -> Option<Box<[u8; SGB_FRAME_SIZE * 3]>> {
        let sgb = mmio.sgb()?;
        let (tiles, map, pals) = sgb.border()?;
        use crate::sgb::MaskMode;

        let mut out = vec![0u8; SGB_FRAME_SIZE * 3];
        let put = |out: &mut [u8], px: usize, py: usize, rgb555: u16| {
            let (r, g, b) = rgb555_to_rgb888(rgb555);
            let i = (py * SGB_FRAME_WIDTH + px) * 3;
            out[i] = r;
            out[i + 1] = g;
            out[i + 2] = b;
        };

        // 1. Backdrop: the shared color 0.
        let backdrop = sgb.backdrop();
        for py in 0..SGB_FRAME_HEIGHT {
            for px in 0..SGB_FRAME_WIDTH {
                put(&mut out, px, py, backdrop);
            }
        }

        // 2. GB screen at (48, 40), mirroring sgb_frame's mask semantics.
        // Until a palette command runs, `uncolorized` supplies the four
        // shades — the caller passes the SGB system palette the firmware
        // would have picked for this cart, so a non-aware game shows its
        // 1-A/Auto colours inside the border instead of grey.
        let src: &[u8] = match self.sgb_freeze_fb.as_deref() {
            Some(f) if f.len() == FRAMEBUFFER_SIZE => f,
            _ => &self.fb_b[..],
        };
        for y in 0..144usize {
            for x in 0..160usize {
                let rgb555 = match sgb.mask {
                    MaskMode::Black => 0x0000,
                    MaskMode::Color0 => backdrop,
                    _ => {
                        let shade = src[y * 160 + x] & 3;
                        sgb.color_for(x / 8, y / 8, shade)
                            .unwrap_or(uncolorized[shade as usize])
                    }
                };
                put(&mut out, 48 + x, 40 + y, rgb555);
            }
        }

        // 3. Border tiles, drawn over both the window and the backdrop.
        draw_sgb_border_tiles(tiles, map, pals, |px, py, rgb555| put(&mut out, px, py, rgb555));

        Some(out.into_boxed_slice().try_into().expect("SGB frame size"))
    }

    /// The SGB border artwork split into the two layers a caller that draws
    /// its own live GB screen needs — see `SgbBorderLayers`. Screen-free by
    /// construction, so the bytes depend only on the cart's (or firmware's)
    /// CHR_TRN/PCT_TRN upload: identical artwork yields identical images.
    ///
    /// Takes no `uncolorized` shades — those only ever fed the GB-screen step
    /// that this omits. Same None conditions as `sgb_composited_frame`, and
    /// likewise a non-consuming off-screen read. The tilemap is walked once.
    pub fn sgb_border_layers(&self, mmio: &mmio::Mmio) -> Option<SgbBorderLayers> {
        let sgb = mmio.sgb()?;
        let (tiles, map, pals) = sgb.border()?;

        let put = |out: &mut [u8], i: usize, rgb555: u16| {
            let (r, g, b) = rgb555_to_rgb888(rgb555);
            out[i] = r;
            out[i + 1] = g;
            out[i + 2] = b;
            out[i + 3] = 0xFF;
        };

        // 1. Backdrop, outside the center window only: inside it the GB screen
        // would be, and the ring leaves that to the caller (zeroed = alpha 0,
        // which the buffer already is).
        let mut ring = vec![0u8; SGB_FRAME_SIZE * 4];
        let backdrop = sgb.backdrop();
        for py in 0..SGB_FRAME_HEIGHT {
            for px in 0..SGB_FRAME_WIDTH {
                if SGB_WINDOW_X.contains(&px) && SGB_WINDOW_Y.contains(&py) {
                    continue;
                }
                put(&mut ring, (py * SGB_FRAME_WIDTH + px) * 4, backdrop);
            }
        }

        // 2. Border tiles, identical to the composited path, but routed by
        // where each pixel lands: outside the window into the ring, inside it
        // into the window-local overlay (which on hardware draws OVER the GB
        // picture). A pixel goes to exactly one layer.
        let mut overlay = vec![0u8; 160 * 144 * 4];
        let mut any_overlay = false;
        draw_sgb_border_tiles(tiles, map, pals, |px, py, rgb555| {
            if SGB_WINDOW_X.contains(&px) && SGB_WINDOW_Y.contains(&py) {
                any_overlay = true;
                let (lx, ly) = (px - SGB_WINDOW_X.start, py - SGB_WINDOW_Y.start);
                put(&mut overlay, (ly * 160 + lx) * 4, rgb555);
            } else {
                put(&mut ring, (py * SGB_FRAME_WIDTH + px) * 4, rgb555);
            }
        });

        Some(SgbBorderLayers {
            ring: ring.into_boxed_slice().try_into().expect("SGB frame size"),
            overlay: any_overlay
                .then(|| overlay.into_boxed_slice().try_into().expect("SGB window size")),
        })
    }

    // Debug methods
    pub(crate) fn get_fetcher_pixel_buffer(&self) -> [u8; 8] {
        self.fetcher.get_pixel_buffer()
    }

    pub fn get_fetcher_fifo_size(&self) -> usize {
        self.fetcher.get_fifo_size()
    }

    pub fn get_fetcher_tile_index(&self) -> u8 {
        self.fetcher.get_tile_index()
    }

    pub fn get_sprite_fetch_stall(&self) -> u8 {
        self.sprite_fetch_stall
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    pub fn get_state(&self) -> &State {
        &self.state
    }

    pub fn get_ticks(&self) -> u128 {
        self.ticks
    }

    /// Whether the PPU has processed its LCD-off transition. False means the PPU
    /// still holds its running state (used to force the disable dot before an
    /// idle bulk-skip so the transition is never jumped over).
    pub(crate) fn is_lcd_disabled(&self) -> bool { self.disabled }

    /// DMG OAM-bug support: the OAM row (0..19) the PPU is scanning when a CPU
    /// OAM-bus access COMPLETES, else None. During mode 2 the PPU reads one of the
    /// 20 OAM rows per M-cycle; `line_cycle` is the speed-independent within-line
    /// dot, so the row is `dot / 4`.
    ///
    /// The trigger sites sample at the START of the access M-cycle (the persistent
    /// `line_cycle` before this M-cycle's 4 dots tick), but the OAM access on the
    /// bus lands at the END of that M-cycle — so add `OAM_BUG_ACCESS_DOT` (4, one
    /// M-cycle) to align the scan position to the completion dot. This makes the
    /// mode-2 trigger window M-cycle-exact (validated by blargg 4-scanline_timing's
    /// 1-M-cycle "just before / at first corruption" boundary). Returns None when
    /// the LCD is off or the PPU is not in mode 2. This is the WRITE/IDU path row.
    pub(crate) fn oam_bug_mode2_row(&self) -> Option<u8> {
        if self.disabled || !self.lcdc_has(LCDCFlags::DisplayEnable) {
            return None;
        }
        if self.state != State::OAMSearch {
            return None;
        }
        const OAM_BUG_ACCESS_DOT: u32 = 4;
        let dot = self.line_cycle + OAM_BUG_ACCESS_DOT;
        // Mode 2 is the first 80 dots of the line (20 rows * 4 dots/M-cycle).
        if dot >= 80 {
            return None;
        }
        Some((dot / 4) as u8)
    }

    /// DMG OAM-bug row for a CPU OAM *read* access (as opposed to a write/IDU).
    /// Hardware holds the accessed-OAM-row at 0 across the whole mode-2 prologue (the
    /// three sleep steps before the object-scan loop advances it to 8), and both the
    /// read and write trigger sites guard on `accessed_oam_row >= 8` — row 0 is the
    /// exempt "first two objects" row, so a mode-2-prologue access corrupts nothing.
    /// A CPU read landing at the mode-2 entry samples this prologue window (age's
    /// timed oam-read boundary reads at `line_cycle` 0/4 hit it), so it must return
    /// row 0 (clean). The write/IDU path in `oam_bug_mode2_row` does NOT get this
    /// exemption: blargg oam_bug's INC/DEC-through-OAM writes probe those same early
    /// `line_cycle`s from a different M-cycle phase and observe the deeper scanned
    /// row (their `(line_cycle + 4)/4` mapping is hardware-correct and must stay).
    /// Splitting the exemption by access type reconciles age oam-read (read prologue
    /// clean) with blargg oam_bug (write prologue corrupts) — the row-only function
    /// alone cannot satisfy both.
    pub(crate) fn oam_bug_mode2_row_read(&self) -> Option<u8> {
        let base = self.oam_bug_mode2_row()?;
        // Mode-2 prologue: reads sample the held row-0 (accessed_oam_row < 8), clean.
        if self.line_cycle < 6 {
            return Some(0);
        }
        Some(base)
    }

    /// True when the PPU is currently in PixelTransfer (STAT mode 3, active
    /// rendering). Used by the CGB STOP speed-switch bridge to gate the
    /// mode-3-specific dot correction.
    pub(crate) fn is_in_pixel_transfer(&self) -> bool {
        !self.disabled && self.state == State::PixelTransfer
    }

    /// True when the renderer is on an ACTIVE rendering line (LCD on, LY 0..143):
    /// OAMSearch / PixelTransfer / HBlank of a visible line. An SS->DS speed switch
    /// here makes the per-dot renderer overshoot the post-window mode-3->mode-0
    /// boundary by 2 dots (the same overshoot the PixelTransfer bridge already
    /// compensates), so the STOP bridge drops 2 dots and arms the pullback marker.
    /// VBlank lines (LY 143-tail..152) and the LCD-off path keep the full 8 — there
    /// the renderer is not advancing a mode-3 window, so no overshoot occurs.
    pub(crate) fn is_on_rendering_line(&self) -> bool {
        !self.disabled
            && self.lcdc_has(LCDCFlags::DisplayEnable)
            && self.internal_ly_val < 144
            && self.state != State::VBlank
    }

    pub fn get_x(&self) -> u8 {
        self.x
    }

    pub fn has_frame(&self) -> bool {
        self.have_frame
    }

    pub fn get_sprites_on_line_count(&self) -> usize {
        self.sprites_on_line.len()
    }
}

use crate::cpu::registers;
use crate::memory::mmio;
use crate::memory::Addressable;
use crate::ppu::stat_irq;
use super::controller::{
    wy2_disabled, LCDCFlags, Ppu, SpriteFetchRec, State, BGP, CGB_PIXEL_TRANSFER_ARM_DOT,
    CGB_PIXEL_TRANSFER_WARMUP, DMG_PIXEL_TRANSFER_ARM_DOT, DMG_PIXEL_TRANSFER_WARMUP, LCD_STATUS,
    LINE_153_LY_ZERO_DOT, LY, LYC, OAM_SPRITE_COUNT, SCX, SCY, SPRITE_TILE_NONE, WX, WY,
};

// First line after LCDC.7 0->1: hardware sets the PPU's internal cycle
// counter to -(mode-3-start line cycle + 2), so the first M3 begins
// (mode-3-start line cycle + 2) dots after enable. mode-3-start line cycle = 83 + cgb,
// giving 85 (DMG) / 86 (CGB) dots from enable to first M3.
const DMG_FIRST_FRAME_ARM_DOT: u128 = 85;
// The documented first-M3 start is mode-3-start line cycle+2 = 86 (CGB), but the
// emulated first-line pixel pipeline (warmup + arm) lands the mode-0 transition
// two dots late versus hardware. Arming two dots earlier aligns the
// first-line mode-0 IRQ.
const CGB_FIRST_FRAME_ARM_DOT: u128 = 84;
// On the first line after enable, VRAM/OAM lock (PPU reports mode 3) at the
// same line-cycle as a normal line (on hardware: line cycles >= ~79), even though
// the actual pixel fetch (mode-3 start) begins later at FIRST_FRAME_ARM_DOT.
const DMG_FIRST_FRAME_LOCK_DOT: u128 = 80;
const CGB_FIRST_FRAME_LOCK_DOT: u128 = 82;
// At double speed the CGB first-frame VRAM/OAM lock engages one dot earlier than
// the single-speed boundary.
const CGB_FIRST_FRAME_LOCK_DOT_DS: u128 = 81;
fn cgb_first_frame_lock_dot(double_speed: bool) -> u128 {
    if double_speed { CGB_FIRST_FRAME_LOCK_DOT_DS } else { CGB_FIRST_FRAME_LOCK_DOT }
}
const MODE2_STAT_PRETRIGGER_DOT: u128 = 452;
const LINE153_LY0_DOT_DS: i64 = 6;

impl Ppu {
    pub(in crate::ppu) fn set_lcd_status_mode(mmio: &mut mmio::Mmio, mode: u8) {
        mmio.write_lcd_status_from_ppu((mmio.read(LCD_STATUS) & !0x03) | (mode & 0x03));
    }

    fn reset_lcd_pipeline(&mut self) {
        self.fetcher.reset();
        self.ticks = 0;
        self.x = 0;
        self.objs.sprites_on_line.clear();
        self.objs.current_oam_sprite_index = 0;
        self.objs.next_sprite_fetch_index = 0;
        self.objs.m3_sprite_prev_tile = SPRITE_TILE_NONE;
        self.objs.m3_last_sprite_commit_tick = 0;
        self.objs.sprite_fetch_stall = 0;
        self.plot.objen_history.clear();
        self.plot.objsize_dot_history.clear();
        self.objs.sprite_fetch_recs.clear();
        self.objs.pixel_transfer_warmup = 0;
        self.win.win_y_pos = 0xFF;
        self.win.win_draw_start = false;
        self.win.window_y_triggered = false;
        self.win.window_started_this_line = false;
        self.win.win_weoff_deferred_tail = false;
        self.clk.first_line_after_enable = false;
        self.clk.line_153_ly_zeroed = false;
        self.m3.m3_pixels_discarded = 0;
        self.m0.scheduled_mode0_dot = None;
        self.m0.m0_time_master = None;
        self.m0.cgbp_block_start_cc = None;
    }

    fn enter_scheduled_mode2(&mut self, mmio: &mut mmio::Mmio) {
        // Mode 2 holds no HDMA period edges, LY changes, or block fires; the
        // tracker can sleep until just before the pixel-transfer arm (80/82),
        // which installs the next (mode-3) sleep bound.
        if mmio.is_cgb_features_enabled() && !self.clk.first_line_after_enable {
            let ds = mmio.is_double_speed_mode() as u32;
            mmio.set_hdma_tracker_sleep(mmio.master_cc().wrapping_add(76 << ds));
        }
        // Seed the per-line OBJ-size scan latch from the LCDC as of the mode-2
        // entry boundary. A size write in the prior line's HBlank/VBlank is
        // captured here (affects this line); a write after this boundary (this
        // line's mode2) is applied per-slot after the scan, so sprite-0 keeps
        // the pre-boundary size. This is the late_sizechange 1-cc M2-boundary
        // discriminator (the hardware OAM scanner's per-entry size latch).
        self.objs.scan_obj_size_large = self.lcdc_has(LCDCFlags::SpriteSize);
        // Clear any exact-cc OBJ-size latch left from a prior line so it cannot
        // leak into this line's OAM scan; a mid-mode-2 size write rearms it.
        self.objs.objsize_apply_cc = wy2_disabled();
        Self::set_lcd_status_mode(mmio, 2);
        // Arm the cgbp begin boundary (the hardware CGB-palette-accessible window: blocked once
        // `line cycles(cc) + ds >= 80`) as soon as the line's mode 2 begins, so a
        // BCPD/OCPD write landing in late mode 2 (before M3 is armed) sees it.
        // Derive the exact begin cc from the LY time anchor (same closed form as
        // `m0_time_exact`, but at line-cycle `80 - ds` instead of mode-0):
        // begin = the LY time − ((456 − (80 − ds)) << ds)
        // This is byte-exact at both speeds; the old tick-block heuristic landed
        // ~2 cc late at double speed because its `(4 − cgb)` ticks->line cycles
        // term was not shifted by `ds`.
        self.m0.cgbp_block_start_cc = Some(self.cgbp_begin_exact(mmio));
    }

    /// Byte-exact hardware cgbp-block BEGIN cc for the current line, anchored on
    /// the same LY time as `m0_time_exact`. The hardware CGB-palette-accessible window blocks once
    /// `line cycles(cc) + ds >= 80`, i.e. at line-cycle `80 - ds`.
    fn cgbp_begin_exact(&self, mmio: &mmio::Mmio) -> u64 {
        let ds = mmio.is_double_speed_mode() as i64;
        let plus1 = self.ly_plus1();
        let ly_time = self.clk.p_now as i64 + self.ly_counter(mmio).time as i64 + plus1;
        (ly_time - ((456 - (80 - ds)) << ds)).max(0) as u64
    }

    pub(crate) fn step_scheduled_stat_events(&mut self, mmio: &mut mmio::Mmio) {
        // FF41 mode-bit read-back anticipation: in the last 3 dots of an
        // HBlank line (or of line 153) FF41 reports mode 2 (the next line's
        // mode). Match the hardware STAT resolve's `line cycles >= 453` threshold by
        // writing the anticipated mode at dot 453 and re-syncing the STAT
        // edge latch so the bit change does not produce a duplicate IRQ
        // rising edge — the actual mode-2 IRQ has already been delivered by
        // the pretrigger above when its conditions were met.
        let mode2_anticipate_dot = MODE2_STAT_PRETRIGGER_DOT + 1; // 453
        // The only work-doing path needs `ticks == 453`; bail on every other
        // dot before touching state/mmio. (`disabled` freezes ticks, so it can
        // never sit at 453 while disabled — this subsumes the disabled guard.)
        if self.disabled || self.ticks != mode2_anticipate_dot {
            return;
        }

        let should_anticipate_mode2 = match self.state {
            State::HBlank => self.ticks == mode2_anticipate_dot && mmio.read(LY) < 143,
            State::VBlank => self.ticks == mode2_anticipate_dot
                && (mmio.read(LY) == 153 || self.clk.line_153_ly_zeroed),
            _ => false,
        };
        if should_anticipate_mode2 && (mmio.read(LCD_STATUS) & 0x03) != 2 {
            Self::set_lcd_status_mode(mmio, 2);
        }
    }

    /// Body of the LCD off->on transition in `step`. Cold: runs only on the
    /// dot an LCDC DisplayEnable rising edge is observed, so it is kept out of
    /// the hot per-dot path to keep `step`'s layout tight.
    #[cold]
    #[inline(never)]
    pub(in crate::ppu) fn enter_lcd_enabled(&mut self, mmio: &mut mmio::Mmio) {
            self.sync_lcdc_from_mmio(mmio);
            self.disabled = false;
            mmio.write_ly_from_ppu(0);
            self.reset_lcd_pipeline();
            self.state = State::OAMSearch;
            // First line after enable: STAT reports mode 0 (not 2), no
            // Mode 2 STAT IRQ fires, and M3 starts later than usual.
            self.clk.first_line_after_enable = true;
            // First-frame-after-enable blanking: the panel shows the LCD-off
            // blank for the frame produced immediately after this enable.
            self.out.frames_since_enable = 0;
            // The OAM snapshot at enable holds inactive until `cc + (2*40 << ds) + 1`.
            // the STAT resolve reports mode 0 (suppresses mode 2/3) for `cc < lu_`.
            {
                let ds_u = mmio.is_double_speed_mode() as u32;
                self.clk.display_enable_inactive_until =
                    mmio.master_cc().wrapping_add((80u64 << ds_u) + 1);
            }
            // Carried-edge LYC=0 IRQ on enable (the LCDC-enable write): when
            // the LYC IRQ source is enabled, LYC==0 and the pre-enable STAT
            // did NOT already hold the LYC=LY coincidence flag, enabling the
            // LCD flags a STAT IRQ immediately. The pre-enable lycflag is
            // bit 2 of the stored FF41 (untouched by the mode write below).
            let pre_enable_stat = mmio.read(LCD_STATUS);
            if pre_enable_stat & (1 << 6) != 0
                && mmio.read(LYC) == 0
                && pre_enable_stat & (1 << 2) == 0
            {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            Self::set_lcd_status_mode(mmio, 0);
            // Initialize the event-scheduled IRQ clock at enable: LY=0,
            // line_cycle=0. Mirror the hardware LCDC-change enable branch.
            self.clk.line_cycle = 0;
            self.clk.internal_ly_val = 0;
            // Anchor the PPU dot-clock onto the master cc at LCD enable
            // (hardware seeds the PPU-clock base here). `abs_cc` keeps its accumulated
            // value across an off/on cycle. The derive at the end of THIS step
            // must reproduce the old post-increment value (pre + 1<<ds), so the
            // anchor subtracts that one dot the old accumulator added below.
            let ds_inc = 1u64 << mmio.is_double_speed_mode() as u32;
            self.clk.p_now = mmio.master_cc().wrapping_sub(self.clk.abs_cc + ds_inc);
            self.speed.lytime_no_plus1 = false;
            self.speed.sc_mode3_pullback_pending = false;
            self.latch.wy2 = mmio.read(WY);
            self.latch.wy2_apply_cc = wy2_disabled();
            self.latch.wy1 = mmio.read(WY);
            self.latch.wy1_apply_cc = wy2_disabled();
            self.latch.scy_delayed = mmio.read(SCY);
            self.latch.scy_apply_cc = wy2_disabled();
            self.latch.scx_delayed = mmio.read(SCX);
            self.latch.scx_apply_cc = wy2_disabled();
            self.clk.stat_reg_committed = mmio.read(LCD_STATUS);
            // See note in `enable_display`: LYC/STAT timing follows the CGB
            // LCD controller on CGB hardware regardless of DMG-compat mode.
            self.clk.lyc_irq.set_cgb(mmio.is_cgb());
            self.clk.lyc_irq.seed(mmio.read(LCD_STATUS), mmio.read(LYC));
            self.clk.mstat_irq.seed(mmio.read(LCD_STATUS), mmio.read(LYC));
            self.clk.lyc_irq.lcd_reset();
            self.clk.mstat_irq.lcd_reset(self.clk.lyc_irq.lyc_reg_src());
            self.reschedule_all_stat_events(mmio);
            self.clk.sched_m0irq = stat_irq::DISABLED_TIME;
            self.clk.sched_oneshot_statirq = stat_irq::DISABLED_TIME;
            // OAM snapshot at LCD enable: zero the snapshot and
            // hold it inactive (no sprites) until `cc + (80<<ds) + 1`. abs_cc
            // is re-derived below; display-enable is anchored to that dot.
            {
                let ds = mmio.is_double_speed_mode();
                let cc = mmio.master_cc().wrapping_sub(self.clk.p_now);
                self.objs.oam_reader.cgb = mmio.is_cgb_features_enabled();
                self.objs.oam_reader.large_src =
                    self.lcdc_has(LCDCFlags::SpriteSize);
                let dma_writing =
                    mmio.oam_dma_window_active() && !mmio.mgb_frozen_merge_active();
                self.objs.oam_reader.src_disabled = dma_writing;
                self.objs.oam_reader.enable_display(cc, ds);
                self.objs.prev_dma_writing = dma_writing;
                self.objs.oam_reader_seeded = true;
            }
    }

    /// Body of the LCD on->off transition in `step`. Cold for the same reason
    /// as `enter_lcd_enabled`.
    #[cold]
    #[inline(never)]
    pub(in crate::ppu) fn enter_lcd_disabled(&mut self, mmio: &mut mmio::Mmio) {
        // AGB lets an all-but-committed mode-1 edge through an LCD disable; CGB
        // does not. vbl_mode1_lcdoff_{dmg,gbc}_mode sweeps the LCD-off write across
        // the end of LY=143 (line_cycle 436+4i) and reads IF one M-cycle later: the
        // AGB capture's E0->E3 step lands one probe earlier than CGB's, at the probe
        // whose disable sits inside the last M-cycle before the m1 event (line_cycle
        // 454). The STAT half of the same capture — the mode-1 *entry* — is
        // byte-identical across CGB and AGB, so AGB's LCD phase is NOT shifted; only
        // this edge survives. The bound is one M-cycle rather than the 2 dots the
        // DMG-compat build alone would suggest because the two builds probe the same
        // hardware boundary on grids one dot apart (compat lands 2 dots short of the
        // event, CGB-native 3); `<= 4` is the bound that partitions both, and the
        // preceding probe is a full M-cycle further out on either grid.
        //
        // Modelled here, on the disable, rather than by firing the m1 event early:
        // moving the event itself also moves every ordinary post-enable VBlank raise,
        // which re-times the HALT wake and costs dma/hdma_halt on AGB. Confining it to
        // the disable leaves free-running AGB byte-identical.
        let ds = mmio.is_double_speed_mode();
        if mmio.is_agb()
            && !ds
            && self.clk.internal_ly_val >= 143
            && self.clk.sched_m1irq.saturating_sub(self.clk.abs_cc) <= 4
        {
            if self.clk.mstat_irq.do_m1_event(self.clk.stat_reg_committed) {
                mmio.stage_lcd_raise_kind(mmio::LCD_RAISE_M1);
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            mmio.request_interrupt(registers::InterruptFlag::VBlank);
            self.clk.m1_vblank_fired = true;
            self.clk.sched_m1irq = self
                .clk
                .sched_m1irq
                .wrapping_add(stat_irq::LCD_CYCLES_PER_FRAME << ds as u32);
        }
        mmio.write_ly_from_ppu(0);
        self.reset_lcd_pipeline();
        Self::set_lcd_status_mode(mmio, 0);
        self.disabled = true;
        // Re-arm the sprite snapshot for the next display-enable.
        self.objs.oam_reader_seeded = false;
        let _ = mmio.take_oam_write_pending();
    }

    /// Mode 2 (OAM search) for one dot: the per-line reset at dot 0, the
    /// two-dots-per-slot sprite scan, and the mode-2 -> mode-3 arm. Lifted
    /// verbatim out of `step`'s `State::OAMSearch` arm.
    #[inline(always)]
    pub(in crate::ppu) fn step_mode2(&mut self, mmio: &mut mmio::Mmio) {
        // Window line-counter bookkeeping at the start of Mode 2. The WY
        // trigger latch (`window_y_triggered`/window-enable master) is handled by the
        // hardware-style three-point check in `update_window_y_latch`,
        // which runs near the previous line's end.
        if self.ticks == 0 {
            // window Y position is incremented at window draw-start (see the
            // PixelTransfer start_window site), matching the hardware
            // mode-3-start window-checkpoint semantics.
            // Reset window line flag for new scanline
            self.win.window_started_this_line = false;
            self.win.win_weoff_deferred_tail = false;
            self.win.win_start_dot = None;
            self.win.predicted_win_start_dot = None;
            self.win.win_wx_penalty_resolved = false;
            self.win.win_wx_enable_resolved = false;

            // Initialize OAM search state
            self.objs.sprites_on_line.clear();
            self.objs.current_oam_sprite_index = 0;
            self.objs.next_sprite_fetch_index = 0;
            self.objs.sprite_fetch_stall = 0;
            self.objs.pixel_transfer_warmup = 0;
        }

        // First line after enable: VRAM/OAM lock (PPU reports mode 3)
        // at the normal mode-2->3 boundary, even though the real pixel
        // fetch starts later at FIRST_FRAME_ARM_DOT. Matches the hardware
        // VRAM/OAM writability (line cycles-based, not mode-3 start).
        if self.clk.first_line_after_enable {
            let is_cgb = mmio.is_cgb_features_enabled();
            let lock_dot = if is_cgb { cgb_first_frame_lock_dot(mmio.is_double_speed_mode()) } else { DMG_FIRST_FRAME_LOCK_DOT };
            if self.ticks == lock_dot && (mmio.read(LCD_STATUS) & 0x03) != 3 {
                Self::set_lcd_status_mode(mmio, 3);
            }
            // Install the closed-form master-cc anchors for the first line
            // BEFORE M3 arms, so the CPU-access gates (OAM/VRAM/cgbp) resolve
            // the mode-3 END boundary (`cc + 2 >= mode-0 time`) during this pre-M3
            // OAMSearch phase too. On hardware the PPU machine is fully seeded
            // at enable (`cycles = -(mode-3-start line cycle + 2)`), so
            // `the current line's mode-0 (HBlank) time` is predictable from the start of the line;
            // here it is enable-anchored (`p_now`) and uses the first-line
            // m3-start (+2). OAM is blocked from line start to mode-0 time (mode 2
            // and mode 3 alike) — the inactive-period guard above keeps it
            // accessible until `lu_`. Recomputed each tick so a mid-line SCX/
            // window change tracks (the M3-arm site re-installs the final
            // value). No closed-form anchor existed here before (the gates
            // fell back to the first-line FF41 mode register, which reports
            // mode 0 and wrongly unblocked OAM in this window).
            let m3_len = self.compute_m3_length(mmio, is_cgb);
            self.m0.m0_time_master = Some(self.m0_time_exact(mmio, m3_len, is_cgb, true));
            self.m0.cgbp_block_start_cc = Some(self.cgbp_begin_exact(mmio));
        }

        // Perform sprite search distributed across 80 ticks
        // Check one sprite every 2 ticks (40 sprites × 2 ticks = 80 ticks)
        // Skipped on the first scanline after LCD enable (no Mode 2 phase).
        if !self.clk.first_line_after_enable
            && self.ticks.is_multiple_of(2)
            && self.objs.current_oam_sprite_index < OAM_SPRITE_COUNT
        {
            // Exact-cc OBJ-size override: when a mid-mode-2 size write is
            // pending, this slot's size is the value visible as-of its own
            // abs_cc (write_cc + 2*cgb), instead of the one-slot-lagged
            // snapshot. With no pending change `objsize_large_at_cc` falls
            // back to the lagged snapshot semantics (the steady state is
            // unchanged). Sampled BEFORE the OAM read so this entry uses
            // the size effective at its read cc (the hardware per-entry size latch).
            if self.objs.objsize_apply_cc != wy2_disabled() {
                self.objs.scan_obj_size_large = self.objsize_large_at_cc(self.clk.abs_cc);
            }
            // Record this slot's size for the snapshot rebuild, set for
            // every scanned slot (even once 10 sprites are found, so the
            // rebuild has a valid size for all 40 entries).
            {
                let idx = self.objs.current_oam_sprite_index;
                self.objs.scan_slot_large[idx] = self.objs.scan_obj_size_large;
            }
            self.check_single_sprite_for_scanline(mmio, self.objs.current_oam_sprite_index);
            self.objs.current_oam_sprite_index += 1;
            // Latch the OBJ-size for the NEXT scan slot from the live LCDC
            // (DMG: write applies to entries scanned after it commits, not
            // the one just read; the hardware per-slot size latch).
            self.objs.scan_obj_size_large = self.lcdc_has(LCDCFlags::SpriteSize);
        }

        let is_cgb = mmio.is_cgb_features_enabled();
        let pixel_transfer_arm_dot = if self.clk.first_line_after_enable {
            if is_cgb {
                CGB_FIRST_FRAME_ARM_DOT
            } else {
                DMG_FIRST_FRAME_ARM_DOT
            }
        } else if is_cgb {
            CGB_PIXEL_TRANSFER_ARM_DOT
        } else {
            DMG_PIXEL_TRANSFER_ARM_DOT
        };

        if self.ticks == pixel_transfer_arm_dot {
            // Rebuild the sprite list from the lazy OAM snapshot (the hardware
            // OAM-scan-end snapshot flush + sprite mapping). This replaces
            // the incremental per-dot scan's `sprites_on_line` so visibility
            // honors the DMA-disabled-source window via the posbuf cap.
            // Rebuild the sprite list from the lazy OAM snapshot (the hardware
            // OAM-scan-end snapshot flush + sprite mapping). On
            // the first line after enable there is no mode-2 scan; the
            // snapshot is held inactive (display-enable) so skip the rebuild.
            if !self.clk.first_line_after_enable {
                self.build_sprites_from_snapshot(mmio);
            }
            // Sort sprites by priority after OAM search is complete
            if is_cgb {
                // CGB mode: Sort by OAM index only (already in order, but ensure it)
                self.objs.sprites_on_line.sort_by_key(|sprite| sprite.oam_index);
            } else {
                // DMG mode: Sort by X coordinate first, then OAM index
                self.objs.sprites_on_line.sort_by(|a, b| {
                    a.x.cmp(&b.x).then(a.oam_index.cmp(&b.oam_index))
                });
            }

            self.x = 0;
            self.fetcher.reset();
            // Clear any pending sub-cc scx column lever from the previous
            // line; a new write this line re-arms it.
            self.m3.subcc_scx_apply_cc = wy2_disabled();
            self.m3.prologue_rekey_armed = false;
            self.objs.next_sprite_fetch_index = 0;
            self.objs.m3_sprite_prev_tile = SPRITE_TILE_NONE;
            self.objs.m3_last_sprite_commit_tick = 0;
            self.objs.sprite_fetch_stall = 0;
            self.objs.fetcher_cadence_tick = 0;
            self.win.win_fetch_anchor = None;
            self.win.win_first_tile_chop = 0;
            self.win.win_being_fetched = false;
            self.win.insert_bg_pixel = false;
            self.win.win_wx0_delayed = false;
            self.win.dmg_wx_trigger_pending = None;
            {
                let we_now =
                    self.lcdc_has(LCDCFlags::WindowDisplayEnable);
                self.win.we_dot_hist = [we_now; 5];
                self.win.we_glitch_tile_starts = [None; 2];
                self.win.we_glitch_discard_insert = false;
                self.win.we_insert_suppressed = false;
            }
            // CGB arms two dots later, so use a shorter warmup to keep the first visible pixel aligned.
            self.objs.pixel_transfer_warmup = if is_cgb {
                CGB_PIXEL_TRANSFER_WARMUP
            } else {
                DMG_PIXEL_TRANSFER_WARMUP
            };
            Self::set_lcd_status_mode(mmio, 3);
            self.state = State::PixelTransfer;
            // The hardware mode-3-start window checkpoint: if win_draw_start was armed from the
            // previous line (DMG wx==166 case) and the window is enabled,
            // the window draws from xpos 0 this line (the window-Y increment), even
            // though WX is unchanged. Otherwise the window-draw state clears to 0.
            {
                let win_en = self.lcdc_has(LCDCFlags::WindowDisplayEnable);
                // The hardware mode-3-start window checkpoint: if win_draw_start is set and
                // the window is enabled, the window-draw state becomes win_draw_started
                // and window Y position increments; otherwise the window-draw state clears.
                if self.win.win_draw_start && win_en && !self.clk.first_line_after_enable {
                    self.win.win_y_pos = self.win.win_y_pos.wrapping_add(1);
                    self.win.win_draw_started = true;
                    self.win.win_draw_started_at_x0 = true;
                    // The window is `started` from line begin: fetch
                    // window tiles from xpos 0 (after the SCX discard
                    // prefix), not BG. The hardware mode-3-start checkpoint seeds
                    // wscx = tile_len + scx%8, so the first window tile
                    // column is wscx/8 == 1 (for scx<8).
                    let scx = (mmio.read(SCX) & 0x07) as u32;
                    let start_tile = ((8 + scx) / 8) as u8;
                    self.fetcher.start_window_at_tile(0, start_tile);
                    self.win.win_kill_tap_late = false;
                    self.win.window_started_this_line = true;
                    self.win.win_start_dot = Some(self.ticks);
                } else {
                    self.win.win_draw_started_at_x0 = false;
                    // The hardware mode-3-start checkpoint: when win_draw_start was
                    // NOT armed, the window-draw state clears to 0 (win_draw_started
                    // bit dropped). Normal (non-wxA6) windows re-set this on
                    // the same line via the live x+7==wx start below, so this
                    // only persistently clears the bit on lines where the
                    // window does not (re)start — which is what lets the DMG
                    // wxA6 START-NOW branch fire again when WY next matches.
                    if win_en && !self.clk.first_line_after_enable {
                        self.win.win_draw_started = false;
                    }
                }
                self.win.win_draw_start = false;
            }
            // DMG wx==166 (lcd_hres+6): the hardware pixel-output runs at EVERY
            // xpos as the fetcher walks the line; the wx==xpos==166 branch
            // therefore fires at the END of mode 3 (xpos reaches
            // 166), AFTER the line's mid-mode-3 WE-off has had its effect on
            // the window-draw state — NOT at M3 start. Relocating this branch to the
            // mode-3 -> HBlank transition (where xpos==166) is what lets the
            // steady-state wxA6 sequence converge: f0(the window-Y increment, state->2) ->
            // WE-off(state==2 -> clears started, state->0, stops window) ->
            // THIS branch B at xpos==166(state |= win_draw_start, state->1) ->
            // HBlank WE-on(state==win_draw_start -> the window-Y increment, state->3). That
            // is the TWO window Y position increments per line (8px/4rows) the window
            // diagonal needs, and the WE-off now actually reverts the right
            // columns to BG (it no longer sees win_draw_start pre-armed). See
            // the relocated block at the mode-3 -> HBlank boundary below.
            // First scanline after enable is now armed; subsequent
            // lines use normal Mode 2 timing.
            let was_first_line = self.clk.first_line_after_enable;
            self.clk.first_line_after_enable = false;
            self.m0.mode0_reported_this_line = false;
            self.m3.line_rendered_this_line = false;
            self.win.wxa6_lineend_applied = false;
            // SCX fine-scroll discard target (the mode-3-start fine-scroll phase): the
            // break xpos is resolved over the first M3 dots by re-reading
            // SCX live (see the early-window loop in PixelTransfer). Seed
            // it unlatched (-1) and record the arm dot for xpos tracking.
            self.m3.m3_pixels_discarded = 0;
            self.m3.m3_arm_dot = self.ticks;
            // Per-pixel BG-enable history: anchor the
            // plot-cc origin at mode-3 entry and seed the line's history
            // with the BG-enable bit in effect now. Mid-mode-3 LCDC.0
            // writes append (commit_cc, bgen) entries (handle_lcdc_write).
            self.plot.bgen_history.clear();
            // Seed at boundary column 0 (applies to all columns until the
            // first mid-mode-3 toggle).
            self.plot.bgen_history.push((
                0,
                self.lcdc_has(LCDCFlags::BGDisplay),
            ));
            // Per-line tile-index-is-tile-data glitch targets (the hardware
            // tile-select glitch); mid-mode-3 falling LCDC.4 writes append the
            // single (cc, k) read each arms (see handle_lcdc_write).
            self.wg.tidxtd_glitch.clear();
            // DMG window bus-glitch state is per-line (see wg_apply).
            self.wg.wg_hist.clear();
            self.wg.bg_tile_buf.clear();
            self.wg.win_tile_buf.clear();
            self.wg.wg_anchor_cc = None;
            self.wg.wg_dpre = 0;
            self.wg.bg_anchor_cc = None;
            self.wg.bg_anchor_dot = None;
            self.wg.bg_scy_hist.clear();
            self.wg.bg_scx_hist.clear();
            // CGB-compat journal flavor (see the CGBWG_* consts): DMG cart on
            // CGB hardware (compat mode runs with CGB features OFF, so
            // it shares the DMG render paths; the journals resolve
            // with the CGB grid/transition rules instead).
            self.wg.wg_cgb = mmio.is_cgb() && !mmio.is_cgb_features_enabled();
            // Per-pixel DMG palette histories: seed each at boundary 0 with
            // the 1-dot-delayed register value (`*_delayed`, refreshed at the
            // end of every dot), NOT the live register. A BGP/OBP write on the
            // dot the PPU enters mode 3 has already updated mmio but must not
            // yet color column 0 — the column-0 pixel sees the prior dot's
            // value (the hardware DMG-palette-during-mode-3 behavior: the write at mode-3 entry
            // leaves column 0 white). Mid-mode-3 writes after entry append
            // (boundary_col, value) entries via on_{bgp,obp0,obp1}_write, which
            // land at column >= 1 so column 0 keeps this seed.
            self.plot.bgp_history.clear();
            self.plot.bgp_history.push((0, self.plot.bgp_delayed));
            self.plot.bgp_dot_history.clear();
            // CGB-compat (wg_cgb) resolves BGP per dot from this history; unlike
            // the DMG per-dot `bgp_delayed` latch, real CGB silicon colors the
            // mode-3 column-0 pixel with the LIVE BGP register (age m3-bg-bgp-ncm:
            // the pre-frame BGP is already latched at mode-3 arm). DMG keeps the
            // 1-dot-delayed seed (dmgpalette_during_m3, via bgp_history).
            let bgp_dot_seed = if self.wg.wg_cgb { mmio.read(BGP) } else { self.plot.bgp_delayed };
            self.plot.bgp_dot_history.push((0, bgp_dot_seed));
            // Clear any leftover DMG BGP phase-hold from the previous line.
            self.plot.bgp_defer_countdown = 0;
            self.plot.obp0_history.clear();
            self.plot.obp0_history.push((0, self.plot.obp0_delayed));
            self.plot.obp1_history.clear();
            self.plot.obp1_history.push((0, self.plot.obp1_delayed));
            self.plot.obp0_dot_history.clear();
            self.plot.obp0_dot_history.push((0, self.plot.obp0_delayed));
            self.plot.obp1_dot_history.clear();
            self.plot.obp1_dot_history.push((0, self.plot.obp1_delayed));
            // DMG mid-mode-3 OBJ-enable/OBJ-size toggle model: seed the
            // per-column OBJ-enable history and the per-dot OBJ-size
            // history with the bits in effect at mode-3 entry, and reset
            // the per-sprite live fetch records (all Pending).
            self.plot.objen_history.clear();
            self.plot.objen_history.push((
                0,
                self.lcdc_has(LCDCFlags::SpriteDisplayEnable),
            ));
            self.plot.objsize_dot_history.clear();
            self.plot.objsize_dot_history.push((
                0,
                self.lcdc_has(LCDCFlags::SpriteSize),
            ));
            self.objs.sprite_fetch_recs.clear();
            self.objs.sprite_fetch_recs
                .resize(self.objs.sprites_on_line.len(), SpriteFetchRec::default());
            self.plot.bgp_writes.clear();
            // Carry a mode-2 BGP write into this line's spike cadence as a
            // neighbor-only entry (see on_bgp_write); a mode-3 partner within
            // BGP_SPIKE_CADENCE_CC then paints its spike (age m3-bg-bgp).
            if let Some((cc, v)) = self.plot.bgp_mode2_pending.take()
                && !mmio.is_cgb()
            {
                self.plot.bgp_writes.push((cc, 0xFF, v));
                // The mode-2 write is the true settled BGP entering mode 3
                // (bgp_delayed lags a dot and can miss a late-mode-2 write),
                // so re-seed column 0's palette + the spike's `old` baseline
                // with it — the restore's glitch then ORs against FF, painting
                // its victim column with the pre-restore (glitch) shade.
                self.plot.bgp_history.clear();
                self.plot.bgp_history.push((0, v));
                self.plot.bgp_delayed = v;
            }
            // 160-entry per-column BG-index scratch; ensure sized (deserialized
            // saves may carry an empty vec) and clear to -1 (no BG pixel yet).
            self.plot.line_bg_idx.clear();
            self.plot.line_bg_idx.resize(160, -1);
            self.m3.m3_arm_scx = mmio.read(SCX) & 0x07 ;
            self.m3.m3_arm_scx_full = mmio.read(SCX) as i16;
            // First line after enable: resolve the SCX value the fine-scroll
            // discard actually samples. The mode-3-start fine-scroll phase reads SCX once
            // at the M3-start dot; a mid-discard SCX write (visible at
            // `write_cc + 2*cgb`) counts only if it lands at/before that
            // sample dot, which sits `prev_scx % 8` dots past M3-arm (the
            // discard prefix of the value in effect at M3-start). Evaluate the
            // pending f1 latch (from on_scx_write, still intact here) at
            // `arm_cc + prev_scx%8`. Matches hardware byte-exact on the
            // ly0_late_scx7 SCX-write sweep (initial-SCX shifts the sample
            // dot, flipping whether the SCX=7 write enters the mode-0 time).
            if was_first_line {
                let ds = mmio.is_double_speed_mode() as u32;
                let prev_scx = (self.latch.scx_prev_f1 & 0x07) as u64;
                // `prev_scx` is a count of PPU dots; convert to master cc
                // (1 dot = 1<<ds cc) so the sample dot is phase-correct at
                // double speed (where the f1 latch's apply cc is write_cc+4).
                let sample_cc = self.clk.abs_cc + (prev_scx << ds);
                self.m3.first_line_scx_override = Some(self.scx_f1_pending_at_cc(sample_cc));
            } else {
                self.m3.first_line_scx_override = None;
            }
            // Seed the exact-cc f1 latch at the SCX value live at M3
            // start; clear any pending write latch left from a prior
            // line so it cannot leak into this line's discard.
            self.latch.scx_prev_f1 = mmio.read(SCX);
            self.latch.scx_f1_apply_cc = wy2_disabled();
            // The first line after display enable has bespoke warmup/arm
            // timing; the live f1 xpos mapping does not align there, so
            // latch the discard immediately (pre-write SCX), as before.
            self.m3.m3_discard_target = if was_first_line { self.m3.m3_arm_scx as i8 } else { -1 };

            if was_first_line {
                // First line after LCD enable: install the SAME closed-form
                // master-cc anchors the normal-line path uses, computed for
                // this line, so the CPU-access gates (cgbp/oam/vram) and the
                // STAT-resolve mode reads resolve at the access cc instead of
                // falling back to the hand-tuned FIRST_FRAME per-dot pipeline.
                //
                // On hardware the LCDC-write handling seeds the PPU at enable with `now =
                // enable_cc`, resets the LY counter to (0, enable_cc), no sprites
                // (display-enable clears the buffer), and `cycles =
                // -(mode-3-start line cycle + 2)` — so the first M3 begins 2 dots
                // later than a normal line. `m0_time_exact(.., first_line)`
                // adds that +2 to the mode-0 line-cycle; `cgbp_begin_exact`
                // (the line cycles+ds>=80 begin boundary) is enable-anchored
                // already (it shares the same the LY time as a normal line).
                // The inactive-period gate (`display_enable_inactive_until`,
                // the hardware OAM-reader lookup-until was seeded at enable.
                let m3_len = self.compute_m3_length(mmio, is_cgb);
                let m0t = self.m0_time_exact(mmio, m3_len, is_cgb, true);
                self.m0.m0_time_master = Some(m0t);
                // The override applied only to this first-line mode-0 time anchor;
                // clear it so the per-tick / next-frame m3_len reads live SCX.
                self.m3.first_line_scx_override = None;
                self.m0.cgbp_block_start_cc = Some(self.cgbp_begin_exact(mmio));
                // The within-line reported mode-0 dot / m0 IRQ arm keep the
                // calibrated FIRST_FRAME timing (the first-line pixel
                // pipeline arms later than a normal line); only the
                // closed-form access/STAT-resolve anchors above are installed.
                self.m0.scheduled_mode0_dot = None;
            } else {
                // Closed-form mode-0 schedule, including window-start lines
                // (compute_m3_length applies the window penalty). Mid-mode-3
                // window-enable toggles (set_lcdc_visible) and WX changes
                // (PixelTransfer) invalidate it, falling back to the live
                // emergent x==160 transition.
                let m3_len = self.compute_m3_length(mmio, is_cgb);
                let ds = mmio.is_double_speed_mode() as u32;
                // Byte-exact mode-0 time, the LY time-anchored (ENGINE_LAZY_PPU.md):
                // mode-0 time = (p_now + ly_counter().time + 1)
                // − ((456 − (m3_len + BASE)) << ds)
                // BASE = 84 (CGB SS+DS), 83 (DMG — the `1−cgb` term already
                // lives in m3_len). `p_now + ly_counter().time` is the
                // next-LY master cc; +1 corrects rustyboi's LY counter.time
                // running 1 master-cc below the hardware LY time.
                // The runtime sprite0-at-scx fine-scroll stall (the hardware
                // mode-3-start fine-scroll) extends the real mode-3 -> mode-0 transition
                // past the predictor's mode-0 time; fold it into the renderer /
                // STAT-read boundary here (m0_irq_event_cc_master subtracts
                // it back for the predictor-timed m0 STAT IRQ).
                let m0t = self.m0_time_exact(mmio, m3_len, is_cgb, false)
                    + ((self.sprite0_scx_extra(mmio, is_cgb) as u64) << ds);
                self.m0.m0_time_master = Some(m0t);
                // Deep mode 3 is HDMA-tracker-quiet until the closed-form
                // period can lead the mode-0 entry (m0t - 8).
                if is_cgb {
                    mmio.set_hdma_tracker_sleep(m0t.saturating_sub(8));
                }
                // The within-line mode-0 dot is DERIVED from the same exact
                // mode-0 time (master cc) so the eager-grid consumers (reported
                // FF41 mode poke, m0 IRQ arm, cgbp tick fallback) ride the
                // identical boundary: dot = arm_ticks + (m0t − arm_cc) >> ds.
                let arm_cc = mmio.master_cc() as i64;
                let dot = self.ticks as i64 + (((m0t as i64) - arm_cc) >> ds);
                self.m0.scheduled_mode0_dot = Some(dot.max(0) as u128);
                self.m0.m3_scheduled_wx = mmio.read(WX);
                self.m0.m3_scheduled_win = self.window_will_start(mmio, is_cgb);
                // Predict the DMG dot at which the window's StartWindowDraw
                // mode-3 penalty commits, so a disable landing on it (one
                // PPU step before the PixelTransfer latch sets
                // `win_start_dot`) is still treated as "started". The window
                // draws when visible x reaches max(0, WX-7); x begins
                // advancing `WARMUP + 8` dots past the M3 arm (the first BG
                // tile fill) plus the SCX fine-scroll discard. The penalty
                // commits at the fetcher's window-tile boundary, one dot
                // ahead of the first window pixel reaching x (the `-1`), so
                // a disable on the dot before the visible start still keeps
                // it (late_disable_*_wx11 vs the same-tile wx10).
                self.win.predicted_win_start_dot =
                    if !is_cgb && self.m0.m3_scheduled_win {
                        let wx = self.m0.m3_scheduled_wx as i64;
                        let x_at_start = (wx - 7).max(0);
                        Some(
                            (self.m3.m3_arm_dot as i64
                                + DMG_PIXEL_TRANSFER_WARMUP as i64
                                + 8
                                + (self.m3.m3_arm_scx as i64)
                                + x_at_start
                                - 1)
                                .max(0) as u128,
                        )
                    } else {
                        None
                    };
                // cgbp begin boundary (the hardware CGB-palette-accessible window: blocked once
                // `line cycles(cc) + ds >= 80`), byte-exact from the LY time
                // anchor — see `cgbp_begin_exact`.
                self.m0.cgbp_block_start_cc = Some(self.cgbp_begin_exact(mmio));
            }
            // Arm the mode-0 (HBlank) STAT IRQ event at the predicted
            // mode-0 start, in absolute clock terms. Hardware schedules
            // memevent_m0irq only when m0 is enabled, but keeps the time
            // current for FF41/FF45 immediate-trigger checks; we always
            // arm it (dispatch gates on the enable in mstat_irq).
            self.arm_m0irq_for_current_line(mmio, was_first_line);
        }
    }

    /// Mode 0 (HBlank) for one dot. Lifted verbatim out of `step`'s
    /// `State::HBlank` arm.
    ///
    /// Returns `true` when the line ended on this dot. In `step` that path was
    /// a bare `return`, so the caller must return immediately and skip the
    /// trailing DMG palette latch — the early exit is preserved, not dropped.
    #[inline(always)]
    pub(in crate::ppu) fn step_hblank(&mut self, mmio: &mut mmio::Mmio) -> bool {
        if self.ticks == 455 {
            self.ticks = 0;
            let current_ly = mmio.read(LY);

            if current_ly >= 143 {
                mmio.write_ly_from_ppu(144);
                self.state = State::VBlank;
                // Panel drive marker: SameBoy re-arms
                // `frame_repeat_countdown` at the start of EVERY VBlank
                // line 144-152 (including the skipped frame's), not once
                // per frame; this is the line-144 anchor and the VBlank
                // else-branch below advances it through line 152. The
                // skipped frame's repeat decision samples the window
                // BEFORE this entry re-arms it (SameBoy checks the
                // countdown before the re-arm on the same line); a
                // skipped frame denied the repeat (panel already
                // decayed) does not re-arm — the panel stays undriven
                // until a displayed frame.
                if self.out.frames_since_enable == 0 {
                    self.out.repeat_skip_pending =
                        self.renders_color(mmio) && self.panel_recently_driven(mmio);
                }
                if self.out.frames_since_enable != 0 || self.out.repeat_skip_pending {
                    self.out.last_drive_cc = mmio.master_cc();
                }
                Self::set_lcd_status_mode(mmio, 1);
                // The m1 event already flagged VBlank (line_cycle 454, ~3cc
                // earlier); re-flagging here would re-set bit 0 after a CPU
                // IF-write between the two cc cleared it (lycint143_m1irq_ifw
                // `_2`, m2m1irq_ifw `_3`). Only flag if the m1 event did not
                // (e.g. LCD enabled mid-frame with no armed m1 schedule).
                if !self.clk.m1_vblank_fired {
                    mmio.request_interrupt(registers::InterruptFlag::VBlank);
                }
                self.clk.m1_vblank_fired = false;
            } else {
                // Continue to next visible scanline
                let next_ly = current_ly.saturating_add(1);
                mmio.write_ly_from_ppu(next_ly);
                self.state = State::OAMSearch;
                self.enter_scheduled_mode2(mmio);
                self.objs.next_sprite_fetch_index = 0;
                self.objs.sprite_fetch_stall = 0;
                self.objs.pixel_transfer_warmup = 0;
            }
            return true;
        }
        false
    }

    /// Mode 1 (VBlank) for one dot: the line-153 early LY=0 flip and the
    /// end-of-line advance / frame swap. Lifted verbatim out of `step`'s
    /// `State::VBlank` arm.
    ///
    /// Returns `true` when the line ended on this dot. In `step` that path was
    /// a bare `return`, so the caller must return immediately and skip the
    /// trailing DMG palette latch — the early exit is preserved, not dropped.
    #[inline(always)]
    pub(in crate::ppu) fn step_vblank(&mut self, mmio: &mut mmio::Mmio) -> bool {
        // Partway through line 153, FF44 reads as 0 even though the
        // line itself has not ended. Update LYC=LY immediately so the
        // STAT line for LYC==0 fires one line earlier than the
        // visible LY=0 scanline.
        // The hardware LYC-compare-LY calc anticipates the line-153 LY=0 compare by
        // `line time - 6 - 6*double_speed`. At DS line time=912cc, so the
        // LY->0 flip lands 12cc = dot 6 into line 153 -- the same dot as
        // single speed (whose `line time-6` likewise resolves to dot 6 in its
        // own dot units). So both speeds use dot 6; the DS probes
        // (lyc0flag_ds / lyc153flag_ds) read C5 at line cycles>=6, C1 before.
        let line_153_zero_dot = if mmio.is_double_speed_mode() {
            LINE153_LY0_DOT_DS.max(0) as u128
        } else {
            LINE_153_LY_ZERO_DOT
        };
        if !self.clk.line_153_ly_zeroed
            && self.ticks == line_153_zero_dot
            && mmio.read(LY) == 153
        {
            mmio.write_ly_from_ppu(0);
            self.clk.line_153_ly_zeroed = true;
            if mmio.read(LYC) == 0 {
                mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2));
            } else {
                mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2));
            }
        }

        if self.ticks == 455 {
            self.ticks = 0;
            let current_ly = mmio.read(LY);
            let end_of_frame = current_ly >= 153 || self.clk.line_153_ly_zeroed;

            if end_of_frame {
                mmio.write_ly_from_ppu(0);
                self.clk.line_153_ly_zeroed = false;
                self.state = State::OAMSearch;
                // Arm the DMG "line 154" STAT-write VBlank-IF glitch window
                // at the exact frame-wrap dot (LY 153->0, VBlank exit). A
                // FF41 write within this window clears the still-pending
                // VBlank IF (see `l154_vblank_glitch_window`). Disarmed a few
                // dots into the new frame by `step` (below).
                self.clk.l154_vblank_glitch_window = true;
                self.enter_scheduled_mode2(mmio);
                self.objs.next_sprite_fetch_index = 0;
                self.objs.sprite_fetch_stall = 0;
                self.objs.pixel_transfer_warmup = 0;
                self.win.win_y_pos = 0xFF;
                // NOTE: win_draw_start / win_draw_started are intentionally
                // NOT reset here. The hardware resets window Y position at the line-0 mode-2 checkpoint but
                // leaves the window-draw state (both bits) untouched across the frame
                // boundary, so a window armed on the last visible line (e.g.
                // DMG wx==166 on line 143, where pixel output branch B arms
                // win_draw_start even with the window then disabled) carries
                // through vblank and activates the window on the next frame's
                // line 0 (the mode-3-start window checkpoint consumes win_draw_start, the window-Y increment).
                // This is the wxA6 window-enable-master-persistence path.
                self.win.window_y_triggered = false;
                self.win.window_started_this_line = false;

                // CGB panel repeat (see `panel_holds_image`): the first
                // frame completed after an LCDC.7 enable is never driven
                // to the panel. When the drive countdown had not expired
                // at this frame's VBlank entry (a brief LCD off — under
                // ~4 lines from its VBlank-line start), it REPEATS the
                // previously displayed image for that skipped frame:
                // discard the rendered pixels, keep the front buffer, and
                // treat the panel as resynced (the next frame displays).
                // A panel undriven for longer has decayed to blank: fall
                // through to the normal swap, and get_frame blanks it.
                // DMG panels show the blank for the skipped frame instead
                // of repeating (SameBoy: CGB-only REPEAT vblank type).
                let repeat_skip =
                    self.out.frames_since_enable == 0 && self.out.repeat_skip_pending;
                self.out.repeat_skip_pending = false;
                if repeat_skip {
                    self.out.color_fb_a.fill(0);
                    self.out.frames_since_enable = 2;
                } else if self.renders_color(mmio) {
                    // CGB / DMG-compat-on-CGB: swap color framebuffers
                    std::mem::swap(&mut self.out.color_fb_b, &mut self.out.color_fb_a);
                    self.out.color_fb_a.fill(0);
                } else {
                    // DMG mode: swap monochrome framebuffers
                    std::mem::swap(&mut self.out.fb_b, &mut self.out.fb_a);
                    self.out.fb_a.fill(0);
                }

                self.out.have_frame = true;
                // Count this completed frame toward post-enable resync so
                // get_frame stops blanking once a full frame has displayed.
                if !repeat_skip {
                    self.out.frames_since_enable = self.out.frames_since_enable.saturating_add(1);
                }
                // The panel holds a real image only while completed frames
                // are actually displayed (not blanked by the resync rule);
                // a blanked skipped frame means the panel decayed to white.
                self.out.panel_holds_image = self.out.frames_since_enable >= 2;
                // The SS->DS-mode3 the LY counter re-anchor is a phase artifact
                // local to the frame(s) right after the switch; once two
                // frame wraps have re-settled the line phase (age lcd-align-ly:
                // multiple STOP windows push its LY reads several frames past
                // the switch) it no longer applies and the LY-register reads
                // resolve through the standard DS window. The age `ly`
                // mode-3-switch probes read within 0-1 wraps and keep it.
                if self.speed.ssds_mode3_ly_advance {
                    self.speed.ssds_mode3_frames = self.speed.ssds_mode3_frames.saturating_add(1);
                    if self.speed.ssds_mode3_frames >= 2 {
                        self.speed.ssds_mode3_ly_advance = false;
                    }
                }
            } else if (144..153).contains(&current_ly) {
                let next_ly = current_ly.saturating_add(1);
                mmio.write_ly_from_ppu(next_ly);
                // Panel drive re-arm at the start of every VBlank line
                // through 152 (SameBoy re-arms `frame_repeat_countdown`
                // per vblank line, not per frame): an LCD off STARTING
                // mid-VBlank measures its decay from the most recent
                // line start, so late-VBlank offs (the EA flip at
                // LY 145+) still repeat. Line 153 does not re-arm.
                if next_ly <= 152
                    && (self.out.frames_since_enable != 0 || self.out.repeat_skip_pending)
                {
                    self.out.last_drive_cc = mmio.master_cc();
                }
            }
            return true;
        }
        false
    }
}

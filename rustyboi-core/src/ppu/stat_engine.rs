use crate::cpu::registers;
use crate::memory::mmio;
use crate::memory::Addressable;
use crate::ppu::stat_irq;
use super::controller::{
    wy2_disabled, LCDCFlags, Ppu, State, BGP, CGB_PIXEL_TRANSFER_ARM_DOT, DMG_PIXEL_TRANSFER_ARM_DOT,
    LCD_STATUS, LYC, M0IRQ_DMG_FIRST_FRAME_OFFSET, M0IRQ_OFFSET, M0IRQ_SCX2_CGB_OFFSET,
    M2IRQ_OFFSET, OAM_SPRITE_COUNT, OBP0, OBP1, SCX,
};

// Offset between rustyboi's `ticks` at M3 arm and the hardware line-cycle frame
// for the scheduled Mode 3 -> Mode 0 transition. Swept against the full suite.
const DMG_MODE0_OFFSET: i32 = 4;
const CGB_MODE0_OFFSET: i32 = 4;

impl Ppu {
    pub(in crate::ppu) fn ly_counter(&self, mmio: &mmio::Mmio) -> stat_irq::LyCounter {
        let ds = mmio.is_double_speed_mode();
        // `abs_cc` is in machine cycles (advances by 1<<ds per dot). `time` is
        // the machine-cycle clock at the next LY increment.
        let dots_to_next_line = (stat_irq::LCD_CYCLES_PER_LINE - self.clk.line_cycle) as u64;
        stat_irq::LyCounter {
            ly: self.internal_ly() as u32,
            time: self.clk.abs_cc + (dots_to_next_line << ds as u32),
            ds,
        }
    }
    /// The LY counter as the CPU READ path must observe it —
    /// sub-dot (master_cc) exact. At double speed the renderer's `abs_cc`/
    /// `line_cycle` are advanced on the even-render-dot grid, which sits one
    /// master_cc below the reference even line phase, so the bare `the LY time` (next-LY
    /// master cc) runs 1 cc low and `line cycles = 456 - ((the LY time-cc)>>1)` reads 1
    /// high. Carry the missing sub-dot here so the observed `the LY time`/`line cycles`/
    /// LY/LYC-flag are master_cc-exact at DS. At single speed the bare phase is already
    /// exact (no flooring), so the correction is DS-only; `lytime_no_plus1` (post
    /// DS->SS-switch line) already drops the +1. Flag-OFF this is identical to
    /// `ly_counter`. SCOPE: only the CPU-visible read observers call this; the
    /// internal STAT-event SCHEDULE still keys off the un-corrected `ly_counter`
    /// (its fire-cc anchors are re-anchored in Stages 2-4, not here).
    pub(in crate::ppu) fn ly_counter_obs(&self, mmio: &mmio::Mmio) -> stat_irq::LyCounter {
        let mut lc = self.ly_counter(mmio);
        if lc.ds && !self.speed.lytime_no_plus1 {
            lc.time += 1;
        }
        lc
    }
    // The internal (clean) LY derived from the line clock, independent of the
    // LY register's mid-line transients (line 153 ly=0, etc.).
    pub(in crate::ppu) fn internal_ly(&self) -> u8 {
        self.clk.internal_ly_val
    }
    /// Byte-exact hardware `mode-0 time` (master-cc) for the current line, given the
    /// closed-form mode-3 length `m3_len` (= the cycles-until-xpos-167 length in dots).
    /// mode-0 time = (p_now + ly_counter().time + 1) − ((456 − (m3_len + BASE)) << ds)
    /// BASE = 84 (CGB SS+DS), 83 (DMG). `p_now + ly_counter().time` is the next-LY
    /// master cc; the +1 corrects rustyboi's LY counter.time running one master-cc
    /// below the hardware LY time. STAT-resolve boundary: mode3 iff `master_cc + 2 < mode-0 time`.
    ///
    /// `first_line` selects the first line after LCD enable: hardware seeds the PPU
    /// at enable with `cycles = -(mode-3-start line cycle + 2)` (the LCDC-write handling), so the
    /// first M3 begins TWO dots later than the normal-line m3-start anchor encoded
    /// in BASE (which == `mode-3-start line cycle`). The mode-0 line-cycle is therefore
    /// `m3_len + BASE + 2`. (`p_now + ly_counter().time` is enable-anchored on this
    /// line — `the LCDC-write handling` reset `now = enable_cc`, `the LY counter.reset(0, enable_cc)`.)
    pub(in crate::ppu) fn m0_time_exact(&self, mmio: &mmio::Mmio, m3_len: u128, is_cgb: bool, first_line: bool) -> u64 {
        let ds = mmio.is_double_speed_mode() as u32;
        let base: i64 = if is_cgb { 84 } else { 83 };
        let plus1 = self.ly_plus1();
        let ly_time = self.clk.p_now as i64 + self.ly_counter(mmio).time as i64 + plus1;
        let m0_line_cycle = m3_len as i64 + base + if first_line { 2 } else { 0 };
        (ly_time - ((456 - m0_line_cycle) << ds)).max(0) as u64
    }
    /// Arm `sched_m0irq` for the current line from the renderer's predicted
    /// mode-0 start (`scheduled_mode0_dot`, a within-line dot). Converted to the
    /// absolute clock. If no closed-form mode-0 dot is available (window/first
    /// line), fall back to the m0 prediction from the m3 length.
    pub(in crate::ppu) fn arm_m0irq_for_current_line(&mut self, mmio: &mmio::Mmio, first_frame: bool) {
        let is_cgb = mmio.is_cgb_features_enabled();
        // The mode-0 (HBlank) STAT IRQ time is co-calibrated with the
        // `ticks + m3_len + offset` mode-0 dot, NOT the exact STAT-resolve `mode-0 time`.
        // The lazy-PPU rewrite re-derived `scheduled_mode0_dot` from the exact
        // STAT-resolve mode-0 time (which the CPU read resolves at `cc + 2 < mode-0 time`),
        // landing it 1-3 dots earlier than the eager mode-0 grid the m0 IRQ
        // offset (M0IRQ_OFFSET) was tuned against. Reading `reported_mode0_dot`
        // (= that exact dot) here armed the m0 IRQ early and broke the
        // m2int_m0irq / m0enable / enable_display / vramw_m3end m0-IRQ clusters.
        // Arm from the m3-length dot instead — the same anchor core-loop used —
        // so the IRQ fires on the calibrated boundary again.
        let mode0_within_line = {
            let m3_len = self.compute_m3_length(mmio, is_cgb);
            let offset = if is_cgb { CGB_MODE0_OFFSET } else { DMG_MODE0_OFFSET };
            self.ticks as i64 + m3_len as i64 + offset as i64
        };
        let mut remaining = mode0_within_line - self.ticks as i64;
        // VBlank (LY 144..153) has no mode 0 on the current line: the hardware
        // xpos-166 advance time lands on the next *rendering* line's mode 0
        // (line 0 of the following frame), far beyond the current VBlank. The
        // `ticks + m3_len + offset` form above computes a bogus within-VBlank-line
        // dot which would fire a spurious m0 STAT IRQ this frame (lycint152_m0irq).
        // Carry the schedule forward to line 0: dots to the end of the current
        // line, plus the full VBlank lines that follow, plus line-0's mode-0 dot
        // offset (reuse `m3_len + offset` from above as the line-0 proxy).
        let ly = self.internal_ly() as i64;
        if ly >= stat_irq::LCD_VRES as i64 {
            let last_line = (stat_irq::LCD_LINES_PER_FRAME - 1) as i64; // 153
            let cpl = stat_irq::LCD_CYCLES_PER_LINE as i64;
            let line0_m0_offset = mode0_within_line - self.ticks as i64; // m3_len + offset
            let dots_to_current_line_end = cpl - self.ticks as i64;
            let full_vblank_lines = (last_line - ly) * cpl;
            remaining = dots_to_current_line_end + full_vblank_lines + line0_m0_offset;
        } else {
            // The mode-0 STAT IRQ fires at the xpos-166 advance time, one xpos
            // before the mode-0 time (xpos 167) the closed-form `m3_len` above tracks.
            // For plain lines those differ by one dot (already folded into
            // `M0IRQ_OFFSET`); when a window starts at WX=166 or a sprite sits at
            // the right edge, the final xpos step carries the whole penalty and
            // the IRQ fires that many dots earlier. Subtract that extra advance.
            remaining -= self.m0irq_xpos166_advance(mmio, is_cgb);
        }
        let ds = mmio.is_double_speed_mode();
        let mut off = M0IRQ_OFFSET;
        if is_cgb && !ds && (mmio.read(SCX) & 0x07) == 2 {
            off += M0IRQ_SCX2_CGB_OFFSET;
        }
        if first_frame && !is_cgb && !ds {
            off += M0IRQ_DMG_FIRST_FRAME_OFFSET;
        }
        let dsf = 1i64 << ds as i32;
        let abs = (self.clk.abs_cc as i64 - dsf + (remaining + off) * dsf).max(0) as u64;
        // The IRQ-dispatch arm keeps the calibrated offset form (the faithful
        // xpos-166-advance-time migration of THIS consumer is deferred — the
        // per-dot dispatch phase is co-tuned with the consume-site `+ds /
        // +cgb_ss_m0_anticip` anticipation). The faithful event cc is consumed
        // independently by the halt-exit `<2` fixup via `m0_irq_event_cc_master`,
        // captured at the m0 IRQ flag site.
        self.clk.sched_m0irq = abs;
        self.stat_sched_touched();
    }
    /// FAITHFUL EVENTCC: the mode-0 STAT IRQ event time
    /// (the xpos-166 advance time = the hardware m0
    /// event time) in MASTER cc — the cc domain `master_cc()` /
    /// `m0_time_master` / STAT-resolve `access_cc` share, so the halt-exit
    /// `cc - event time < 2` halt-exit fixup compares like-for-like.
    ///
    /// Derived from the closed-form `m0_time_master` (= the xpos-167 advance time
    /// in master cc): the m0 IRQ fires one xpos earlier, so subtract the 166->167
    /// step cost `((1 + xpos166_advance) << ds)`. `None` when no closed-form master
    /// exists (window mid-line / first line / VBlank), where no faithful event cc
    /// is available and the halt-exit fixup is skipped.
    pub(crate) fn m0_irq_event_cc_master(&self, mmio: &mmio::Mmio) -> Option<u64> {
        if self.internal_ly() as u32 >= stat_irq::LCD_VRES {
            return None;
        }
        let ds = mmio.is_double_speed_mode() as i64;
        let is_cgb = mmio.is_cgb_features_enabled();
        let adv = self.m0irq_xpos166_advance(mmio, is_cgb);
        // m0_time_master carries the runtime sprite0-at-scx fine-scroll extra
        // (see sprite0_scx_extra); the m0 STAT IRQ fires at the PREDICTOR time,
        // so peel it back out here.
        let spr0 = self.sprite0_scx_extra(mmio, is_cgb) << ds;
        self.m0.m0_time_master
            .map(|m0t| (m0t as i64 - spr0 - ((1 + adv) << ds)).max(0) as u64)
    }
    /// Re-anchor the event-scheduled STAT/mode/LYC clocks to the new CPU speed.
    /// Mirrors the hardware speed-change handling: the renderer's LCD position
    /// (`line_cycle`/`internal_ly`) is in speed-independent dot units and stays
    /// put, but every scheduled event time carried the old `ds` cc-factor, so
    /// recompute them from the live `abs_cc` under the new speed.
    pub(crate) fn speed_change(&mut self, mmio: &mmio::Mmio) {
        if self.disabled || !self.lcdc_has(LCDCFlags::DisplayEnable) {
            return;
        }
        self.reschedule_all_stat_events(mmio);
        if self.clk.sched_m0irq != stat_irq::DISABLED_TIME {
            self.arm_m0irq_for_current_line(mmio, self.clk.first_line_after_enable);
        }
    }
    /// Advance the renderer by `dots` dots during the CGB STOP speed-switch
    /// bridge. The hardware STOP handling advances the LCD to `cc + 8` at the OLD
    /// (single) speed before re-anchoring at the new speed (the LCD speed change).
    /// Our per-dot stepper realizes only `8 >> ds` of those dots through the 8
    /// returned cycles, so this injects the remaining bridge dots so the LCD
    /// lands on the same dot hardware does after the 0x20000-cycle window.
    pub(crate) fn stop_bridge_advance(&mut self, mmio: &mut mmio::Mmio, dots: u32) {
        for _ in 0..dots {
            self.step_scheduled_stat_events(mmio);
            // The bridge injects render dots the CPU's returned cycles did not
            // cover, so the master cc does not advance for them. `step` derives
            // `abs_cc = master_cc - p_now`; pull `p_now` back by one dot first so
            // the derived clock still advances `1<<ds` this bridge step.
            self.clk.p_now = self.clk.p_now.wrapping_sub(1 << mmio.is_double_speed_mode() as u32);
            self.step(mmio);
            self.step_lcdc_events(mmio);
        }
    }
    /// Mark that a DS->SS speed switch just occurred, so the closed-form the LY time
    /// drops its `+1` the LY counter correction (the whole-dot bridge already lands
    /// the counter one master-cc high). See ENGINE_LAZY_PPU.md bug #2.
    pub(crate) fn set_dsss_lytime_adjust(&mut self) {
        self.speed.lytime_no_plus1 = true;
    }
    pub(in crate::ppu) fn dsss_ly_total_par(&self) -> i64 {
        (self.speed.dsss_ly_total_count % 2) as i64
    }
    pub(crate) fn dsss_ly_phase_par(&self) -> i64 {
        (self.speed.dsss_ly_phase_count % 2) as i64
    }
    /// True once any post-STOP DS->SS switch has accumulated a sub-dot phase.
    pub(crate) fn dsss_ly_phase_active(&self) -> bool {
        self.speed.dsss_ly_phase_count > 0
    }
    /// Latch the SS->DS-during-mode3 FF44 (LY) read phase advance. Consumed only
    /// by `get_ly_reg_at_cc` to resolve the LY-register anticipation window against
    /// the hardware re-anchored LY time (the renderer/STAT/m0 phase is unaffected).
    pub(crate) fn set_ssds_mode3_ly_advance(&mut self) {
        self.speed.ssds_mode3_ly_advance = true;
        self.speed.ssds_mode3_frames = 0;
    }
    /// Advance the STAT/LINE-PHASE clock by ONE dot
    /// WITHOUT moving the pixel-fetcher render latch (`self.ticks`/`self.x`/the
    /// FIFO/the render state machine). This is the decoupling primitive:
    /// `line_cycle` (the STAT/LY/ttnl phase clock) is normally welded to the renderer
    /// inside `step` (both `line_cycle += 1` and `self.ticks += 1` per dot). A
    /// faithful sub-dot STOP re-anchor needs to shift the STAT phase by
    /// an ODD dot WITHOUT moving the mode-3 render latch. This
    /// mirrors `step`'s STAT-phase region (the lines between `dispatch_stat_events`
    /// and `update_window_y_latch`) exactly, but skips the `match self.state`
    /// render machine and the `self.ticks += 1`. It is the line-phase HALF of the
    /// lockstep that `step` runs as a whole.
    ///
    /// Caller contract (mirrors `stop_bridge_advance`'s per-dot prelude): pull
    /// `p_now` back by one dot BEFORE calling so the derived `abs_cc` still
    /// advances `1<<ds` for this STAT dot (the carry is a non-master-cc-advancing
    /// bridge dot, same as the rendered bridge dots). `step_scheduled_stat_events`
    /// / `step_lcdc_events` are run by the caller around it, identically to the
    /// rendered-bridge per-dot loop, so the only difference from a bridge `step`
    /// is the absence of render-latch motion.
    fn step_stat_phase_only(&mut self, mmio: &mut mmio::Mmio) {
        if self.disabled || !self.lcdc_has(LCDCFlags::DisplayEnable) {
            return;
        }
        // --- STAT-phase region of `step` (no render match, no `ticks += 1`) ---
        self.dispatch_stat_events(mmio);
        self.clk.abs_cc = mmio.master_cc().wrapping_sub(self.clk.p_now);
        self.clk.line_cycle += 1;
        if self.clk.line_cycle >= stat_irq::LCD_CYCLES_PER_LINE {
            self.clk.line_cycle = 0;
            self.clk.internal_ly_val += 1;
            if self.clk.internal_ly_val as u32 >= stat_irq::LCD_LINES_PER_FRAME {
                self.clk.internal_ly_val = 0;
            }
        }
        self.process_oam_reader_events(mmio);
        let effective_ly = self.effective_ly_for_lyc_compare(mmio);
        if mmio.read(LYC) == effective_ly {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2));
        } else {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2));
        }
        self.update_window_y_latch(mmio);
    }
    /// Register one DS->SS-during-mode3 STOP switch and
    /// return how many STAT-phase carry dots to inject this switch (the increment
    /// in `floor(count/2)`): every 2nd such switch injects ONE extra dot,
    /// reproducing the accumulated reference `now -= 1` half-dot. Stop-count
    /// invariant by construction (the carry depends only on the running count,
    /// not on any single STOP's integer-cc). Returns 0 on the odd switches.
    pub(crate) fn register_dsss_mode3_stop(&mut self) -> u32 {
        let before = self.speed.dsss_mode3_stop_count / 2;
        self.speed.dsss_mode3_stop_count += 1;
        let after = self.speed.dsss_mode3_stop_count / 2;
        after - before
    }
    /// The decoupled STAT-phase carry as a bridge step. Advances the
    /// STAT/line clock by `dots` dots (same per-dot prelude as
    /// `stop_bridge_advance`: `step_scheduled_stat_events`, `p_now` pullback,
    /// then the line-phase step, then `step_lcdc_events`) but the render latch
    /// (`self.ticks`/`self.x`/FIFO/mode-3 fetch) stays PUT. With `dots == 0` this
    /// is a no-op.
    pub(crate) fn stat_phase_carry(&mut self, mmio: &mut mmio::Mmio, dots: u32) {
        for _ in 0..dots {
            self.step_scheduled_stat_events(mmio);
            let dot_cc = 1i64 << mmio.is_double_speed_mode() as u32;
            self.clk.p_now = self.clk.p_now.wrapping_sub(dot_cc as u64);
            self.step_stat_phase_only(mmio);
            self.step_lcdc_events(mmio);
            // The STAT phase (line_cycle/abs_cc) just advanced one dot; the render
            // latch did NOT. Record the divergence so the CPU-access visibility
            // gate (`ppu_blocks` -> `render_carry_skew`) re-aligns a store to the
            // un-carried fetcher position.
            self.speed.render_carry_skew_cc += dot_cc;
        }
    }
    /// Recompute all scheduled IRQ event times from scratch at the current
    /// `abs_cc` (used on LCD enable / LY-counter reset).
    pub(in crate::ppu) fn reschedule_all_stat_events(&mut self, mmio: &mmio::Mmio) {
        let lc = self.ly_counter(mmio);
        let cc = self.clk.abs_cc;
        let stat = self.clk.stat_reg_committed;
        self.clk.lyc_irq.reschedule(&lc, cc);
        self.clk.sched_lycirq = self.clk.lyc_irq.time;
        self.clk.sched_m1irq = stat_irq::mode1_irq_schedule(&lc, cc);
        let m2 = stat_irq::mode2_irq_schedule(stat, &lc, cc);
        self.clk.sched_m2irq = if m2 == stat_irq::DISABLED_TIME { m2 } else { (m2 as i64 + Self::m2_off(mmio.is_double_speed_mode())) as u64 };
        // m0irq is scheduled from the renderer's mode-0 prediction; (re)armed
        // when entering pixel transfer. Leave as-is here.
        self.stat_sched_touched();
    }
    /// Double-speed sub-dot step. At DS the CPU runs two M-cycles per displayed
    /// pixel-dot; the full `step` runs on the even (render) M-cycle and advances
    /// `abs_cc` by 2. This runs on the intervening odd M-cycle so STAT/LYC IRQ
    /// events scheduled at an *odd* `abs_cc` fire at the true half-dot instead of
    /// being rounded up to the next even render dot. It dispatches events at the
    /// intermediate cc (`abs_cc - 1`, i.e. one M-cycle before the next render
    /// dot's post-increment value) without advancing the renderer's clock.
    #[inline]
    pub(crate) fn step_subdot(&mut self, mmio: &mut mmio::Mmio) {
        if self.disabled {
            return;
        }
        // Bail without the abs_cc adjustment dance when no event is due at
        // the odd half-dot: the dispatch fast path tests abs + 2 < sched_min,
        // and the half-dot's abs is abs - 1, so abs + 1 < sched_min is the
        // identical no-op condition.
        if self.clk.abs_cc + 1 < self.clk.sched_min {
            return;
        }
        self.step_subdot_slow(mmio);
    }
    fn step_subdot_slow(&mut self, mmio: &mut mmio::Mmio) {
        // The preceding full `step` dispatched at the even cc N and advanced
        // `abs_cc` to N+2 (the next render dot). The odd half-dot is cc N+1, one
        // machine cycle earlier; dispatch any event due there, then restore.
        self.clk.abs_cc -= 1;
        self.dispatch_stat_events(mmio);
        self.clk.abs_cc += 1;
    }
    /// Fire any STAT IRQ events whose scheduled time has arrived at the current
    /// `abs_cc`. Called once per dot from `step` (and at the DS odd half-dot
    /// from `step_subdot`).
    ///
    /// Fast path: none of the ~10 scheduled events can fire this dot. Every
    /// consumer gates on `sched_cc <= cc + off` with `off <= 2` (the m1/m0
    /// double-speed anticipation), and the wy/scy/scx apply blocks use off 0.
    /// So if the cached earliest scheduled cc (`sched_min`, a lower bound of
    /// the true 9-way min — see the field doc) is still more than 2 dots away,
    /// the whole body is a no-op.
    #[inline]
    pub(in crate::ppu) fn dispatch_stat_events(&mut self, mmio: &mut mmio::Mmio) {
        if self.clk.abs_cc + 2 < self.clk.sched_min {
            return;
        }
        self.dispatch_stat_events_slow(mmio);
    }
    /// Zero the cached scheduled-event lower bound so the next per-dot
    /// dispatch recomputes it. Must be called by every path that can LOWER one
    /// of the 9 slots (`wy2/wy1/scy/scx_apply_cc`, `sched_oneshot_statirq`,
    /// `sched_m1irq/lycirq/m2irq/m0irq`); raise-only writes (to
    /// DISABLED_TIME / later times) may skip it — a stale-LOW bound only costs
    /// a redundant slow call, never a missed event.
    #[inline]
    pub(in crate::ppu) fn stat_sched_touched(&mut self) {
        self.clk.sched_min = 0;
        self.clk.fast_dots_left = 0;
    }
    /// Drop the mode-3 preamble fast budget. Called by the bus on any write
    /// at or above 0xFE00 (OAM/IO — LY/LYC/STAT/WY/LCDC/IE/IF and the OAM
    /// write-pending signal all live there), so the skipped preamble pieces
    /// resume per-dot processing on the very next dot.
    #[inline]
    pub(crate) fn invalidate_fast_span(&mut self) {
        self.clk.fast_dots_left = 0;
        self.clk.fast_hold = 8;
    }
    /// Fresh mode-3 preamble fast budget in render dots: the sched_min slack
    /// (margin 12 covers every dispatch anticipation offset and the DS
    /// sub-dot), gated off entirely near line/frame transients.
    pub(in crate::ppu) fn mode3_fast_budget(&self, mmio: &mmio::Mmio) -> u32 {
        if !(2..=152).contains(&self.clk.internal_ly_val) || self.clk.first_line_after_enable {
            return 0;
        }
        let ds = mmio.is_double_speed_mode() as u32;
        let abs_now = mmio.master_cc().wrapping_sub(self.clk.p_now);
        let slack = self.clk.sched_min.saturating_sub(abs_now.saturating_add(12));
        (slack >> ds).min(512) as u32
    }
    /// Lower bound, in MASTER cc, on the next cc at which the PPU can raise an
    /// IF bit (STAT or VBlank). `sched_min` lower-bounds every scheduled
    /// dispatch slot in abs-cc space (abs_cc = master - p_now); the 8-cc
    /// margin covers the dispatch anticipation offsets (<= 2*ds + the sub-dot
    /// half-step) with room to spare. The ly143->144 render-machine VBlank
    /// fire lands ~3cc AFTER the m1 event, which is itself in the min, so it
    /// is covered too. While the LCD is off the PPU raises nothing. A dirty
    /// bound (sched_min == 0) yields a past cc, i.e. "no batching".
    ///
    /// `sched_m0irq` needs special care: unlike every other slot it is armed
    /// mid-stream (at pixel-transfer entry), so while it is DISARMED with the
    /// m0 STAT source enabled a fire is still possible later this line —
    /// bound by the closed-form current-line mode-0 time. Once that time has
    /// passed (we are in/past this line's HBlank, the slot already fired and
    /// disarmed), the next possible m0 fire is next line's mode-0 entry: at
    /// least (dots to next line start) + mode 2 (80) + minimal mode 3, kept
    /// very conservative at +200 render dots past the line wrap. With no
    /// closed-form anchor (window / first line) batching is refused outright.
    pub(crate) fn next_stat_irq_lower_bound_master(&self, now: u64, ds: bool) -> u64 {
        if self.disabled {
            return u64::MAX;
        }
        let mut bound = self.clk.sched_min.saturating_add(self.clk.p_now);
        if self.clk.sched_m0irq == stat_irq::DISABLED_TIME
            && self.clk.stat_reg_committed & stat_irq::STAT_M0EN != 0
        {
            // The m0 slot is only armed mid-stream at the pixel-transfer
            // transition (ticks 80/82 normal, 84/85 first-line-after-enable;
            // every other arm site is CPU-write-driven, i.e. at a batch
            // boundary). While the slot is disarmed with the m0 STAT source
            // enabled, bound the batch to end 2+ dots before the earliest
            // possible arm; inside the arm zone itself refuse to batch
            // (single-step through it — the traced failure mode was treating
            // t=78/79 as "past the arm" and wrapping a full line across this
            // line's arm AND fire). Past the zone a disarmed slot means this
            // line's fire already happened (or a VBlank line, where the real
            // next arm is even later), so the next arm is next line's.
            const ARM_LO: u64 = 78; // 2 before the earliest arm dot (80)
            const ARM_HI: u64 = 88; // 2 past the latest arm dot (85, first line)
            let t = (self.ticks as u64) % stat_irq::LCD_CYCLES_PER_LINE as u64;
            let to_arm = if t < ARM_LO {
                ARM_LO - t
            } else if t < ARM_HI {
                return 0;
            } else {
                (stat_irq::LCD_CYCLES_PER_LINE as u64 - t) + ARM_LO
            };
            bound = bound.min(now.saturating_add(to_arm << (ds as u32)));
        }
        bound.saturating_sub(8)
    }
    /// One-compare pre-gate for `skip_inert_dots`: only mode 0/1/2 interiors
    /// can be inert, so mode-3 dots skip the full-call attempt entirely.
    #[inline]
    pub(crate) fn maybe_inert_state(&self) -> bool {
        matches!(self.state, State::HBlank | State::VBlank | State::OAMSearch)
    }
    /// Fast-forward through inert HBlank/VBlank interior dots, where the whole
    /// per-dot `step` body is provably bookkeeping: `ticks`/`line_cycle`
    /// advance, the LYC compare rewrites an unchanged flag, the palette latch
    /// re-reads unchanged registers, and the state arm does real work only at
    /// the line edges. Returns RENDER dots consumed (0 = not skippable now).
    ///
    /// Soundness constraints (each maps to per-dot work that would otherwise
    /// run):
    /// - state is HBlank or VBlank with `ticks` in [8, 448): all state-arm
    ///   actions live at ticks 455 (line advance / frame swap) and ticks 6 of
    ///   line 153; the FF41 mode-2 anticipation at 453 and the window-Y latch
    ///   checkpoints (1/450/454) and LYC next-line anticipation (454+) are
    ///   outside the interior.
    /// - internal LY in [2, 152]: excludes the line-153 LY-0 transient and
    ///   the l154 glitch-window disarm checks on lines 0/1.
    /// - no scheduled dispatch event can come due inside the span
    ///   (`sched_min` bound with the same margin the dispatch bail uses), so
    ///   skipping the per-dot dispatch calls skips only no-ops.
    /// - LYC/STAT/palette registers cannot change inside the span (no CPU
    ///   access boundary inside a quiet span) and `bgp_defer_countdown == 0`,
    ///   so the per-dot rewrites are idempotent; the final state equals the
    ///   per-dot outcome.
    /// - the caller (the quiet-span loop) already excludes OAM-DMA, serial,
    ///   the JOYP filter and the HDMA lockstep window, and guarantees no
    ///   pending deferred HDMA writes; within an HBlank interior the HDMA
    ///   period tracker sees no edge and no LY change, so skipped
    ///   `step_hdma` calls are state-identical no-ops (a block fired at the
    ///   mode-0 edge before the interior began).
    /// - `abs_cc` is advanced with the skip: the CPU register-write hooks
    ///   (`write_cc`) and the exact-cc override compares read it at the very
    ///   next access boundary, before any real step would re-derive it.
    pub(crate) fn skip_inert_dots(&mut self, mmio: &mut mmio::Mmio, max_render_dots: u32) -> u32 {
        const INTERIOR_START: u32 = 8;
        const INTERIOR_END: u32 = 448;
        if self.disabled || self.plot.bgp_defer_countdown > 0 || max_render_dots == 0 {
            return 0;
        }
        // A pending delayed LCDC commit must land at its exact dot.
        if !self.lcdc.pending_lcdc_events.is_empty() {
            return 0;
        }
        let mut interior = (INTERIOR_START, INTERIOR_END);
        match self.state {
            State::VBlank => {}
            State::OAMSearch => {
                // Mode-2 interior: the per-dot body is the every-2nd-dot OAM
                // scan slot (batched below with identical per-slot work — the
                // pushes ARE observable at a mid-mode-2 DMA-start boundary,
                // so they must run) plus the same idempotent preamble. The
                // tick-0/1 init and ly0 window checkpoint sit below the
                // interior start; the pixel-transfer arm dot (80/82) and its
                // snapshot rebuild sit past its end. A pending exact-cc
                // OBJ-size override needs its per-dot/per-slot abs_cc
                // resolution, so no batching then.
                if self.clk.first_line_after_enable || self.objs.objsize_apply_cc != wy2_disabled() {
                    return 0;
                }
                // A pending CPU OAM write must be consumed by
                // `process_oam_reader_events` on the very next dot: its
                // `change(cc)` cap anchors the snapshot walk position, which
                // is cc-precise DURING the scan (gambatte late_spXX). A
                // batch would consume it n dots late. (Mode 0/1 skips are
                // immune: the walk is already capped at scan end there.)
                if self.objs.prev_dma_writing || mmio.oam_snoop_event_possible() {
                    return 0;
                }
                let arm = if mmio.is_cgb_features_enabled() {
                    CGB_PIXEL_TRANSFER_ARM_DOT as u32
                } else {
                    DMG_PIXEL_TRANSFER_ARM_DOT as u32
                };
                interior = (4, arm - 2);
            }
            State::HBlank => {
                // With CGB HBlank DMA armed, a block can fire a dot or two
                // INTO HBlank via the per-dot STAT-mode-edge fallback (window
                // lines have no closed-form mode-0 anchor), so HBlank dots
                // are only inert once this line's block has ALREADY fired
                // (the rising edge is consumed; the falling edge and the LY
                // change land past the interior) under a closed-form period
                // anchor. VBlank has no mode-0 edges and stays skippable with
                // HDMA armed.
                if mmio.is_cgb_features_enabled()
                    && (mmio.hdma_is_enabled() || mmio.hdma_req_pending())
                    && (mmio.hdma_req_pending()
                        || !mmio.hdma_block_fired_this_hblank()
                        || self.m0.m0_time_master.is_none())
                {
                    return 0;
                }
            }
            _ => return 0,
        }
        if !(2..=152).contains(&self.clk.internal_ly_val) {
            return 0;
        }
        let t = self.ticks as u32;
        if !(interior.0..interior.1).contains(&t) {
            return 0;
        }
        // Event bound: render dots until the earliest scheduled event, in the
        // same abs-cc space the dispatch compares in. `sched_min == 0`
        // (dirty) yields no skip; the slow dispatch on the next real dot
        // recomputes it.
        let ds = mmio.is_double_speed_mode() as u32;
        let abs_now = mmio.master_cc().wrapping_sub(self.clk.p_now);
        let event_slack = self.clk.sched_min.saturating_sub(abs_now.saturating_add(8));
        let to_event = event_slack >> ds;
        let n = ((interior.1 - t) as u64)
            .min(to_event)
            .min(max_render_dots as u64);
        if n == 0 {
            return 0;
        }
        let n = n as u32;
        // Mode-2: run the scan slots the skipped dots would have run, with
        // the identical per-slot sequence (slot-size latch from the constant
        // LCDC, visibility check + push, next-slot re-latch). One slot per
        // even entry-tick in [t, t+n).
        if matches!(self.state, State::OAMSearch) {
            let slots = ((t + n).div_ceil(2)) - (t.div_ceil(2));
            for _ in 0..slots {
                if self.objs.current_oam_sprite_index >= OAM_SPRITE_COUNT {
                    break;
                }
                let idx = self.objs.current_oam_sprite_index;
                self.objs.scan_slot_large[idx] = self.objs.scan_obj_size_large;
                self.check_single_sprite_for_scanline(mmio, idx);
                self.objs.current_oam_sprite_index += 1;
                self.objs.scan_obj_size_large =
                    self.lcdc_has(LCDCFlags::SpriteSize);
            }
        }
        self.ticks += n as u128;
        self.clk.line_cycle += n;
        // Keep abs_cc exact through the skip: the CPU write hooks anchor
        // their delayed applies on it at the next access boundary.
        self.clk.abs_cc = self.clk.abs_cc.wrapping_add((n as u64) << ds);
        // The palette latch would have re-read the (unchanged) registers each
        // dot; leave it equal to the per-dot outcome.
        self.plot.bgp_delayed = mmio.ppu_io_reg(BGP);
        self.plot.obp0_delayed = mmio.ppu_io_reg(OBP0);
        self.plot.obp1_delayed = mmio.ppu_io_reg(OBP1);
        n
    }
    /// Conservative count of MASTER-cc dots until the PPU's frame wrap (the
    /// ly153->0 frame swap), minus an 8-dot safety margin so the caller's
    /// batch always ends short of the wrap and the swap dot itself resolves
    /// under per-dot stepping (frame-loop return points stay dot-exact).
    /// While the LCD is off there is no wrap (the caller's global cap
    /// bounds the batch instead).
    pub(crate) fn dots_until_frame_wrap_conservative(&self, ds: bool) -> u64 {
        if self.disabled {
            return u64::MAX;
        }
        let pos = self.clk.internal_ly_val as u32 * stat_irq::LCD_CYCLES_PER_LINE + self.clk.line_cycle;
        let total = stat_irq::LCD_LINES_PER_FRAME * stat_irq::LCD_CYCLES_PER_LINE;
        (total.saturating_sub(pos + 8) as u64) << (ds as u32)
    }
    fn dispatch_stat_events_slow(&mut self, mmio: &mut mmio::Mmio) {
        let ds = mmio.is_double_speed_mode();
        let cc = self.clk.abs_cc;

        // Disabled slots hold huge sentinels (u64::MAX / DISABLED_TIME), so the
        // min naturally excludes them.
        let min_sched = self
            .latch.wy2_apply_cc
            .min(self.latch.wy1_apply_cc)
            .min(self.latch.wy_recheck_cc)
            .min(self.latch.scy_apply_cc)
            .min(self.latch.scx_apply_cc)
            .min(self.clk.sched_oneshot_statirq)
            .min(self.clk.sched_m1irq)
            .min(self.clk.sched_lycirq)
            .min(self.clk.sched_m2irq)
            .min(self.clk.sched_m0irq);
        if cc + 2 < min_sched {
            self.clk.sched_min = min_sched;
            return;
        }

        if self.latch.wy2_apply_cc != wy2_disabled() && self.latch.wy2_apply_cc <= cc {
            self.latch.wy2 = self.latch.wy2_pending;
            self.latch.wy2_apply_cc = wy2_disabled();
        }
        if self.latch.wy1_apply_cc != wy2_disabled() && self.latch.wy1_apply_cc <= cc {
            self.latch.wy1 = self.latch.wy1_pending;
            self.latch.wy1_apply_cc = wy2_disabled();
        }
        if self.latch.wy_recheck_cc != wy2_disabled() && self.latch.wy_recheck_cc <= cc {
            self.latch.wy_recheck_cc = wy2_disabled();
            self.run_wy_recheck(mmio.is_cgb(), mmio.is_cgb_features_enabled(), ds);
        }
        if self.latch.scy_apply_cc != wy2_disabled() && self.latch.scy_apply_cc <= cc {
            self.latch.scy_delayed = self.latch.scy_pending;
            self.latch.scy_apply_cc = wy2_disabled();
        }
        if self.latch.scx_apply_cc != wy2_disabled() && self.latch.scx_apply_cc <= cc {
            self.latch.scx_delayed = self.latch.scx_pending;
            self.latch.scx_apply_cc = wy2_disabled();
        }

        if self.clk.sched_oneshot_statirq <= cc {
            mmio.stage_lcd_raise_kind(mmio::LCD_RAISE_LYC);
            mmio.request_interrupt(registers::InterruptFlag::Lcd);
            self.clk.sched_oneshot_statirq = stat_irq::DISABLED_TIME;
        }
        // Order matches the hardware next-memory-event priority for ties.
        // The m1 (VBlank) event (frame_cycle 144*456-2, an even `abs_cc`) is observed
        // two ways at double speed: a CPU FF0F read snapshots IF pre-tick (the snapshot
        // is taken BEFORE this M-cycle's dispatch, so an event at cc == read_cc fires
        // one dispatch too late to be seen — hardware processes events <= cc before
        // read(0xFF0F,cc) returns; needs +2*ds to land at-or-before the read cc), and
        // the VBlank IRQ is *delivered* by the CPU service path (needs the true event
        // cc). The read-snapshot brackets only exist with the m1-STAT source enabled
        // (STAT bit4: lycint143_m1irq `_2`/`_3`, m1irq_disable `_2`); when it is OFF
        // (e.g. the vblankirq retrigger tests, STAT=0x40) the VBlank IRQ-delivery
        // timing dominates and the extra dot delivers the IRQ too early. Anticipate by
        // 2*ds only when m1-STAT is enabled, else by the half-dot +ds the LYC=LY/mode-0
        // events also carry. DS-only (ds=0 leaves the single-speed phase byte-identical).
        let m1en = self.clk.stat_reg_committed & (1 << 4) != 0;
        let m1_anticip = if m1en { 2 * ds as u64 } else { ds as u64 };
        if self.clk.sched_m1irq <= cc + m1_anticip {
            let stat = self.clk.stat_reg_committed;
            if self.clk.mstat_irq.do_m1_event(stat) {
                mmio.stage_lcd_raise_kind(mmio::LCD_RAISE_M1);
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            // The hardware VBlank interrupt (IF bit 0) and the mode-1 STAT IRQ both
            // fire from the SAME LY-counter LY=144 event:
            // bit 0 (VBlank) ALWAYS, bit 1 (STAT) only when the m1 condition holds.
            // The event fires at frame_cycle 144*456-2 (line_cycle 454 of LY=143),
            // ~3cc BEFORE rustyboi's render-machine VBlank (the HBlank ly143->144
            // line transition at line_cycle 455/0). A CPU IF read landing in that
            // gap saw the STAT bit but missed VBlank (the m1irq `_2`/`_3` bracket
            // halves: out0 vs the correct out3, outE2 vs outE3). Flag VBlank here
            // at the faithful m1 event cc so both bits land coincident as on hardware;
            // the render machine's later fire is idempotent (same frame OR).
            if self.clk.internal_ly_val >= 143 {
                mmio.request_interrupt(registers::InterruptFlag::VBlank);
                // Mark so the render-machine ly143->144 transition does not re-flag
                // VBlank after a CPU IF-write cleared it (hardware: single VBlank
                // source). The flag covers the gap between this event (line_cycle
                // 454) and the render transition (line_cycle 455/0).
                self.clk.m1_vblank_fired = true;
            }
            self.clk.sched_m1irq = self.clk.sched_m1irq
                .wrapping_add((stat_irq::LCD_CYCLES_PER_FRAME) << ds as u32);
        }
        if self.clk.sched_lycirq <= cc + ds as u64 {
            let lc = self.ly_counter(mmio);
            if self.clk.lyc_irq.do_event(&lc) {
                mmio.stage_lcd_raise_kind(mmio::LCD_RAISE_LYC);
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            self.clk.sched_lycirq = self.clk.lyc_irq.time;
        }
        if self.clk.sched_m2irq <= cc {
            self.do_mode2_irq_event(mmio, ds);
        }
        // The mode-0 (HBlank) STAT IRQ schedules at an odd `abs_cc` (a half-dot)
        // at double speed; the per-dot dispatch flags it one M-cycle late, which
        // pushes it across a CPU instruction boundary (≈4cc service delay).
        // Anticipating by `ds` dots lands it on the boundary hardware services at
        // — the same half-dot sub-dot fix applied to the LYC=LY IRQ above.
        //
        // On CGB single speed the per-dot dispatch additionally flags the m0 IRQ one
        // dot after the hardware xpos-166 advance time (= mode-0 time-1): the IRQ is
        // delivered at the mode-3->0 transition dot rather than one xpos before it.
        // Measured byte-exact via cctracer (m2int_m0irq_scx3 fires at rel+2 from the
        // IF-clear write M-cycle start vs the hardware rel+1; DMG is already at rel+1).
        // Anticipate by one dot on CGB SS so the m0 IRQ flags at mode-0 time-1, matching
        // the (already exact) m2/LYC phase. Fixes 10sprites/ly0/wxA5 m0irq and the
        // CGB m2int_m0irq_*_ifw IF-clear-vs-m0 ordering.
        let cgb_ss_m0_anticip = (!ds && mmio.is_cgb_features_enabled()) as u64;
        if self.clk.sched_m0irq <= cc + ds as u64 + cgb_ss_m0_anticip {
            let stat = self.clk.stat_reg_committed;
            let ly = self.internal_ly() as u32;
            // FAITHFUL EVENTCC: capture this line's m0 IRQ event cc
            // (the xpos-166 advance time) BEFORE the mutable IF-flag borrow, so
            // the halt-exit `<2` fixup can read the cc the IF bit was raised at
            // (hardware flags the m0 STAT IRQ at its m0 event time).
            let m0_event_cc = self.m0_irq_event_cc_master(mmio);
            let fired = self.clk.mstat_irq.do_m0_event(ly, stat, self.clk.lyc_irq.lyc_reg_src());
            if fired {
                mmio.stage_lcd_raise_kind(mmio::LCD_RAISE_M0);
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
                mmio.set_pending_m0_irq_fire_cc(m0_event_cc);
            }
            // m0irq re-arm happens at next pixel-transfer entry.
            self.clk.sched_m0irq = stat_irq::DISABLED_TIME;
        }

        // Refresh the cached fast-bail bound from the post-fire schedule.
        self.clk.sched_min = self
            .latch.wy2_apply_cc
            .min(self.latch.wy1_apply_cc)
            .min(self.latch.wy_recheck_cc)
            .min(self.latch.scy_apply_cc)
            .min(self.latch.scx_apply_cc)
            .min(self.clk.sched_oneshot_statirq)
            .min(self.clk.sched_m1irq)
            .min(self.clk.sched_lycirq)
            .min(self.clk.sched_m2irq)
            .min(self.clk.sched_m0irq);
    }
    pub(in crate::ppu) fn m2_off(_ds: bool) -> i64 {
        // DS and SS converged on -1 after the double-speed STAT sub-dot step
        // (step_subdot) gave the IRQ model true odd-cc resolution.
        M2IRQ_OFFSET
    }
    fn do_mode2_irq_event(&mut self, mmio: &mut mmio::Mmio, ds: bool) {
        // doMode2IrqEvent: the LY used is the *next* line's LY if the m2 event
        // is within 16 cycles of the ly increment.
        let lc = self.ly_counter(mmio);
        let near_ly_inc = lc.time.saturating_sub(self.clk.sched_m2irq) < 16;
        let ly = if near_ly_inc {
            if lc.ly == stat_irq::LCD_LINES_PER_FRAME - 1 { 0 } else { lc.ly + 1 }
        } else {
            lc.ly
        };
        let stat = self.clk.stat_reg_committed;
        let fired = self.clk.mstat_irq.do_m2_event(ly, stat, self.clk.lyc_irq.lyc_reg_src());
        if fired {
            mmio.stage_lcd_raise_kind(mmio::LCD_RAISE_M2);
            mmio.request_interrupt(registers::InterruptFlag::Lcd);
            // FAITHFUL HALT-EXIT: a halted CPU wakes at this exact cc; the DMG
            // halt-exit fixup (sm83.rs) needs the m2 event time to apply the
            // real +4 (`cc - event time < 2`).
            mmio.set_last_m2_irq_fire_cc(mmio.master_cc());
            // Record the m2-event LY so the CGB halt-exit +4 stall (sm83.rs) can
            // distinguish a rendering-line OAM wake (ly 0..143, intr_2) from the
            // VBlank-entry mode-2 quirk wake (ly 144, vblank_stat_intr).
            mmio.set_last_m2_irq_ly(ly as u8);
        }
        let delta = stat_irq::mode2_reschedule_delta(ly, stat, ds);
        self.clk.sched_m2irq = self.clk.sched_m2irq.wrapping_add(delta);
    }
}

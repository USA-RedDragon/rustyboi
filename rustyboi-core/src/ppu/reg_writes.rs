use crate::cpu::registers;
use crate::memory::mmio;
use crate::memory::Addressable;
use crate::ppu::stat_irq;
use super::controller::{
    wy2_disabled, LCDCFlags, Ppu, State, CGB_PIXEL_TRANSFER_WARMUP, CGB_WY_RAW_LY_INC_DOT,
    DMG_PIXEL_TRANSFER_WARMUP, LCD_STATUS, LY, LYC, SCY_DELAY, WRITE_CC_OFFSET,
    WY1_DELAY, WY2_DELAY_CGB, WY2_DELAY_DMG, WY_RECHECK_LY_VALID_DOT,
};

// Display-column latency between a mid-mode-3 DMG palette-register (BGP/OBP0/OBP1)
// write and the first pixel that sees the new value. `self.x` at the write instant
// is the next column to be popped (the live pipeline plot position); the change
// first reaches the column plotted this many dots later. Same shape as the LCDC
// `self.x + 2` commit in handle_lcdc_write. BGP and OBP carry separate latencies
// (the BG fetcher and the sprite mixer sample at different pipeline stages).
// CGB hardware samples the palette mapping one dot later in the pipeline than DMG
// hardware (the DMG fetcher runs a 4-dot pixel-transfer warmup + the +1 cgb_adj
// phase vs CGB's 2-dot warmup): the same mid-mode-3 write reaches the displayed
// column one column earlier on DMG. Keyed by `is_cgb()` (the hardware, NOT the
// CGB-features mode) so DMG-compat-on-CGB — which renders with the CGB warmup but
// uses the DMG palette regs — takes the CGB latency.
// Pan Docs documents the general observability (a mid-scanline BGP write's effect
// shifts left by any mode-3 delay); the exact per-machine latency is not documented.
// Pan Docs: Rendering — https://gbdev.io/pandocs/Rendering.html
const BGP_LATENCY_CGB: i32 = 2;
const BGP_LATENCY_DMG: i32 = 1;
const OBP_LATENCY_CGB: i32 = 2;
const OBP_LATENCY_DMG: i32 = 1;
// Maximum dot-gap between two consecutive mid-mode-3 palette writes for the DMG
// palette-latch glitch to fire. The glitch is a TWO-WRITE collision: back-to-back
// SET/RESTORE writes ~12 dots apart leave the first write's settling still in-flight
// when the second lands. Single writes, or writes spaced wider than this (~60+ dots
// apart), don't collide and produce no spike.
// Base mid-scanline BGP shift-left is documented (Pan Docs: Rendering, cited above);
// the two-write collision spike itself is not in Pan Docs, TCAGBD, or GBCTR —
// sub-dot render timing from mealybug-tearoom-tests refs.
const BGP_SPIKE_CADENCE_CC: u64 = 12;
fn bgp_latency(cgb: bool) -> i32 {
    if cgb { BGP_LATENCY_CGB } else { BGP_LATENCY_DMG }
}
fn obp_latency(cgb: bool) -> i32 {
    if cgb { OBP_LATENCY_CGB } else { OBP_LATENCY_DMG }
}

impl Ppu {
    /// Re-evaluate the LYC=LY flag and the STAT edge after a CPU write to
    /// FF40 (LCDC), FF41 (STAT), or FF45 (LYC). Called by the host between
    /// CPU instructions when `Mmio::take_stat_register_write_pending`
    /// returns true. The mid-instruction write itself becomes visible to the
    /// PPU on its next dot step; this hook closes the gap where enabling a
    /// STAT source whose underlying condition is already true must produce
    /// an immediate rising edge.
    /// Record the sub-PPU-dot parity of the CPU write about to be resolved, so
    /// the STAT/LYC change hooks can place the event on the correct half-dot at
    /// double speed. `phase` is the persistent CPU T-phase at write resolution.
    pub(crate) fn set_write_subdot(&mut self, phase: u64) {
        self.speed.write_subdot = (phase % 2) as u8;
    }
    /// FF4A (WY) write hook. Hardware applies the write to `wy2` (the value the
    /// window-Y gate reads) delayed by `6 - double_speed` cc after the write.
    /// Schedule that delayed apply against the resolving write's absolute clock.
    pub(crate) fn on_wy_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        if self.disabled {
            self.wy2 = value;
            self.wy2_apply_cc = wy2_disabled();
            self.wy1 = value;
            self.wy1_apply_cc = wy2_disabled();
            return;
        }
        let ds = mmio.is_double_speed_mode();
        let cc = self.write_cc(ds);
        // On a hardware WY change the delayed WY value (the value
        // the window-enable master checkpoints read) applies at cc + 1 + cgb. Schedule that delayed
        // apply so a mid-frame WY write reaches the window-enable master latch with the same
        // phase hardware uses, rather than the live (immediate) mmio value.
        let cgb = mmio.is_cgb_features_enabled() as i64;
        let wy1_delay = WY1_DELAY + cgb;
        self.wy1_pending = value;
        self.wy1_apply_cc = cc + wy1_delay.max(0) as u64;
        // wy2 apply delay (cc) past the write, swept against the late_wy suite:
        // CGB 7, DMG 4 (-ds at double speed). The split reflects the differing
        // M3-start / fine-scroll phase between the two cores.
        let base = if mmio.is_cgb_features_enabled() {
            WY2_DELAY_CGB
        } else {
            WY2_DELAY_DMG
        };
        let delay = (base - ds as i64).max(0) as u64;
        self.wy2_pending = value;
        self.wy2_apply_cc = cc + delay;
        self.arm_wy_recheck(cc, ds);
        self.stat_sched_touched();
    }
    /// Arm the hardware's scheduled window-Y comparison after a WY or LCDC
    /// store. The comparator does not only run at the fixed per-line
    /// checkpoints (see `update_window_y_latch`): a store re-runs it on the
    /// next 4-dot fetch-grid boundary, so a WY value that equals the current
    /// line for only a couple of M-cycles still arms the window for the rest
    /// of the frame. Skipped once the latch is set (the comparison is sticky,
    /// so a re-run can only ever set it again).
    pub(in crate::ppu) fn arm_wy_recheck(&mut self, cc: u64, ds: bool) {
        if self.disabled || self.window_y_triggered {
            return;
        }
        // 4 dots (8 cc at double speed) after the store, quantized onto the
        // grid the comparator runs on.
        let step = 4u64 << (ds as u32);
        self.wy_recheck_cc = cc + step - (cc % step);
    }
    /// Run a scheduled window-Y comparison (see `arm_wy_recheck`).
    pub(in crate::ppu) fn run_wy_recheck(&mut self, cgb_hw: bool, cgb_features: bool, ds: bool) {
        // Double speed re-phases the comparison onto a different sub-dot of
        // the fetch grid (SameBoy quantizes it with `wy_check_modulo + 6`
        // there against `+ 0` in CGB single speed), and rustyboi's CGB
        // double-speed write-vs-LCD phase at the line tail sits several dots
        // off SameBoy's, so the store dot itself is not adjudicable. Left
        // unmodelled rather than approximated (gambatte late_wy_*_ds_*).
        if cgb_hw && ds {
            return;
        }
        if self.disabled
            || self.window_y_triggered
            || !self.lcdc_has(LCDCFlags::WindowDisplayEnable)
        {
            return;
        }
        // The comparison value is the line counter as the comparator sees it,
        // which is not yet valid in the first dots of a line (hardware feeds
        // it an out-of-range value there, so no match is possible).
        if self.ticks < WY_RECHECK_LY_VALID_DOT {
            return;
        }
        // A CGB single-speed store whose scheduled comparison lands AFTER the
        // window's start column has already been fetched does not arm the
        // window at all: on real cgb04c the mid-frame comparator has no later
        // checkpoint this frame, so the window never appears (gambatte
        // late_wy_* `_2`/`_3`, real silicon). SameBoy's model arms it sticky
        // and would show the window on the next line, which cgb04c does not --
        // this suppression tracks the silicon, not SameBoy. The boundary is the
        // window's fetch-start dot: a WX>=7 window draws from x = WX-7 after the
        // first BG tile fill (`warmup + 8 + (WX-7)` past the mode-3 arm); a WX<7
        // window takes the stream over at x == 0 with no BG-fill (`warmup`). The
        // SCX fine-scroll discard is consumed WITHIN the first fetch step, so --
        // unlike the window-ENABLE commit dot in `set_lcdc_visible`, compared
        // against the raw write dot -- it does not shift this grid-quantized
        // boundary for scx&7 <= 4 (the 4-dot-quantized recheck dot already
        // absorbs the sub-tile offset). scx&7 == 5 is the exception: its
        // window-tile fetch runs a dot later in the mode-3 dispatch AND its
        // whole recheck grid lands one 4-dot step later, so the arm/suppress
        // boundary tracks it a full step out. Derived from the cgb04c
        // late_wy_FFto2_ly2_scx{2,3,5} oracles (real silicon): scx2/3 suppress
        // from the same dot as scx0, scx5 one grid step later.
        if cgb_hw && self.state == State::PixelTransfer {
            let wx = self.m0.m3_scheduled_wx as i64;
            let warmup = if cgb_features {
                CGB_PIXEL_TRANSFER_WARMUP as i64
            } else {
                DMG_PIXEL_TRANSFER_WARMUP as i64
            };
            let scx5_step = if wx <= 7 && (self.m3.m3_arm_scx & 7) == 5 { 4 } else { 0 };
            let commit_dot = self.m3.m3_arm_dot as i64
                + warmup
                + if wx >= 7 { 8 + (wx - 7) } else { 0 }
                + scx5_step
                - 1;
            if (self.ticks as i64) >= commit_dot {
                return;
            }
        }
        // The scheduled comparison reads the value the store itself wrote --
        // it is that store that schedules it, and the comparator is fed WY
        // directly. `wy1`'s couple-of-cc apply delay governs only the periodic
        // checkpoints, and a store landing late on the 4-dot grid gets its
        // re-check before that delay has elapsed.
        let wy = if self.wy1_apply_cc != wy2_disabled() { self.wy1_pending } else { self.wy1 };
        if wy == self.wy_comparator_ly(cgb_hw, ds) {
            self.window_y_triggered = true;
        }
    }
    /// Line value the window-Y comparator sees right now.
    ///
    /// A CGB PPU in single speed feeds the comparator the RAW line counter,
    /// which has already advanced across the line tail; every other
    /// configuration (pre-CGB, and a CGB in double speed) feeds it the same
    /// lagging LY-compare value the LYC comparator uses, which still holds the
    /// previous line's number there. SameBoy `Core/display.c` `wy_check`:
    /// `comparison = current_line`, overridden by `ly_for_comparison` only
    /// when `(!GB_is_cgb(gb) || gb->cgb_double_speed)`.
    ///
    /// The two line-tail checkpoints in `update_window_y_latch` bracket the
    /// DMG advance (450 compares `ly`, 454 compares `ly + 1`); the CGB advance
    /// is one M-cycle earlier, at 450. Adjudicated against SameBoy CGB-C/DMG
    /// with the WY store swept through the line tail an M-cycle at a time:
    /// both cores stop arming one step apart, and at the step in between
    /// SameBoy reports `current_line = ly + 1` while `ly_for_comparison` is
    /// still `ly` (gambatte late_wy_* `_2` variants, real cgb04c/dmg08).
    fn wy_comparator_ly(&self, cgb_hw: bool, ds: bool) -> u8 {
        let ly = self.internal_ly();
        if !cgb_hw || ds || self.ticks < CGB_WY_RAW_LY_INC_DOT {
            return ly;
        }
        if ly as u32 + 1 >= stat_irq::LCD_LINES_PER_FRAME { 0 } else { ly + 1 }
    }
    /// FF47 (BGP) write hook. The CPU readback is immediate (handled by mmio), but
    /// the rendered BG palette mapping must change at the exact pixel being drawn
    /// `MID_M3_PAL_LATENCY` columns after the write. Record
    /// the change keyed by the display column it first becomes visible at; the
    /// per-column draw resolves it via `bgp_at`. Only while pixel transfer is active
    /// for this line — a write outside mode 3 just lands in the seed at the next
    /// mode-3 entry. Steady-state (no mid-mode-3 write) is unaffected.
    pub(crate) fn on_bgp_write(&mut self, value: u8, _mmio: &mmio::Mmio) {
        // A BGP write in the OAM scan (mode 2) just before mode 3 is the leading edge
        // of a two-write spike pair when a mode-3 write follows within the cadence
        // window: it settles the glitch palette so the mode-3 partner's transition
        // paints a visible spike (e.g. a $FF write in mode 2 with its restore at
        // col 1 in mode 3). Stash it (survives the mode-3-arm
        // bgp_writes clear); it is injected neighbor-only at mode-3 entry and
        // discarded by the cadence gate if no mode-3 partner lands within
        // BGP_SPIKE_CADENCE_CC.
        if self.state == State::OAMSearch && !_mmio.is_cgb() && !self.disabled {
            self.bgp_mode2_pending = Some((self.abs_cc, value));
        }
        if self.state != State::PixelTransfer || self.disabled {
            return;
        }
        let lat = self.bgp_apply_latency(_mmio);
        // DMG sub-M-cycle phase hold: for a write whose store lands later in the M-cycle
        // (`master_cc % 4` != 0), the DMG per-dot latch (`bgp_delayed`) must keep the old
        // value for `lat - 1` extra dot-refreshes so the new palette first colors the
        // column `self.x + lat` (not `self.x + 1`). Phase-0 writes leave countdown 0 and
        // are unchanged. CGB does not use `bgp_delayed` (it resolves BGP per column from
        // `bgp_history`), so this is DMG-only.
        if !_mmio.is_cgb() {
            let extra = (lat - bgp_latency(false)).max(0) as u8;
            if extra > 0 {
                self.bgp_defer_hold = self.bgp_delayed;
                self.bgp_defer_countdown = extra;
            }
        }
        // DMG mid-mode-3 palette-write glitch (see `bgp_writes`): record this write's
        // apply column, `old | new` glitch value, and cc. Whether it actually spikes a
        // pixel (the TWO-WRITE cadence gate) is resolved at mode-3 end in
        // `resolve_bgp_spikes`, once all of the line's writes are known. Capture the old
        // (settled) value BEFORE recording the new one in the history.
        // A prologue write (SCX-discard warmup) does not paint its own spike, but
        // on hardware it still forms the leading half of a two-write spike cadence:
        // its restore partner (recorded below at a visible column) must find it as a
        // neighbor or the spike vanishes (e.g. a $FF write at x=0 during the SCX
        // discard, restore at x=4 paints the visible black pixel).
        // Record it with a never-painted victim column (>=160) so it is a cadence
        // neighbor only.
        if !_mmio.is_cgb() && self.in_previsible_prologue() {
            self.bgp_writes.push((self.abs_cc, 0xFF, value));
        }
        if !_mmio.is_cgb() && !self.in_previsible_prologue() {
            // The spike's victim is the pixel POPPING at the write's apply dot.
            // When a sprite fetch has the pipeline stalled across that dot no
            // pixel pops — the glitched palette transition collides with
            // nothing and there is no visible spike (a RESTORE landing inside a
            // sprite stall must not paint the first post-stall column). The
            // write is still RECORDED (victim 0xFF, never painted) so its
            // partner keeps its cadence neighbor. On stall-free lines the
            // victim is exactly `self.x + lat` (the old column model).
            let stall = self.sprite_fetch_stall as i32;
            // Pending SCX discard: at x==0 the first display column has not popped
            // while pixels remain to be discarded (m3_pixels_discarded <
            // m3_discard_target). The write's victim pixel is one of those discarded
            // pixels, so no visible spike lands — record it neighbor-only (a restore
            // firing mid-discard).
            let discarding = self.x == 0 && self.m3.m3_pixels_discarded < self.m3.m3_discard_target.max(0) as u8;
            let col = if stall <= lat && !discarding {
                (self.x as i32 + lat - stall).clamp(0, 255) as u8
            } else {
                0xFF
            };
            let old = self.bgp_history.last().map(|&(_, v)| v).unwrap_or(self.bgp_delayed);
            self.bgp_writes.push((self.abs_cc, col, old | value));
        }
        let boundary = self.pal_write_boundary(lat);
        Self::push_pal_history(&mut self.bgp_history, boundary, value);
        // Dot-keyed history for the CGB / DMG-compat BG path: the write applies at
        // its own dot; each pixel samples it at its (stall-delayed) pop dot.
        let apply = self.pal_write_apply_tick(lat);
        Self::push_pal_dot_history(&mut self.bgp_dot_history, apply, value);
    }
    // Display-column latency (dots) for a mid-mode-3 BGP write. This hook fires at the
    // write M-cycle's START, but the DMG store's bus-write lands at a later sub-M-cycle
    // T-cycle, so the change reaches the displayed column a phase-dependent number of
    // dots after `self.x`. The phase is the write's `master_cc % 4`:
    // - phase 0 -> +1 (the baseline). Ordinary one-write-per-line palette streams
    // land here.
    // - later phases add `phase - 1` more dots: a write whose M-cycle starts deeper in
    // the pixel-clock grid latches proportionally later. A tight
    // `LD A,(HL+); LDFF (C),A` gradient write lands at phase 3 (+3 total),
    // 2 columns past the phase-0 baseline.
    // CGB keeps its own 2-dot latency; no phase term (the CGB fetcher samples the
    // palette-RAM pipeline at a fixed stage).
    fn bgp_apply_latency(&self, mmio: &mmio::Mmio) -> i32 {
        if mmio.is_cgb() {
            // CGB-D/E samples the BG palette one dot earlier than CGB-C: CGB-E
            // takes the DMG 1-dot latency while CGB-B/C keep 2.
            //
            // AGB lands on the CGB-B/C side because the bare `is_cgb_de()` is
            // false for it. That placement is INHERITED, not measured: BGP
            // latency is outside the four families `Mmio::set_cgb_de` documents
            // as deliberate (LY-153 window, end-of-vblank STAT, OAM read
            // windows, speed-switch TIMA edge), and NO AGB-graded oracle covers
            // palette latency anywhere (mealybug m3_bgp_change is dmg/cgb only,
            // age m3-bg-bgp tops out at cgbe, and no AGB reference capture
            // exists). Queued for the bench; if AGB turns out to track D/E this
            // becomes `is_agb() || is_cgb_de()`, as the FF41 coincidence
            // tail-hold already spells out.
            let base = if mmio.is_cgb_de() {
                BGP_LATENCY_DMG
            } else {
                bgp_latency(true)
            };
            base + Self::cgb_halt_wake_write_bias(mmio)
        } else {
            let phase = (mmio.master_cc() % 4) as i32;
            bgp_latency(false) + (phase - 1).max(0)
        }
    }
    // CGB halt-woken write-stream bias, in display columns. Hardware charges
    // `cc += 4 * isCgb()` when an IRQ ends HALT — one real
    // M-cycle before the woken stream resumes. rustyboi's halted CPU wakes at
    // the exact IF-set cc and models that M-cycle on the READ side only
    // (STAT-resolve/LY-register biases), so a halt-woken WRITE stream runs 4cc early:
    // every mid-mode-3 palette write it makes lands 4cc (dots, halved in
    // double speed) of display columns short of the hardware column. Re-add
    // the un-charged M-cycle here, gated on the woken stream
    // (`halt_wakeup_skew`, set at wake / cleared at the next HALT): an LYC-woken
    // ISR write stream takes it (its boundaries would otherwise be a uniform 4
    // columns early); a busy-waiting stream (skew=false) keeps the flat latency.
    fn cgb_halt_wake_write_bias(mmio: &mmio::Mmio) -> i32 {
        // A grid-woken CGB stream (quantized DMG-cart path) resumes at the
        // hardware boundary, but its palette writes commit one dot earlier
        // relative to the renderer column clock than the read anchor — the
        // legacy model's read(+5cc)-vs-write(+4col) asymmetry, kept as a -1
        // column write-phase constant (daid ppu_scanline_bgp real-CGB capture).
        if mmio.halt_wake_grid_cgb() {
            return -1;
        }
        // An LYC/m1-woken stream that charged the +4 halt exit as a REAL stall
        // (sm83.rs) already writes at the hardware cc — re-adding the M-cycle
        // here would double it. The m2-woken stall keeps the co-tuned bias.
        if mmio.halt_wakeup_skew() && !mmio.cgb_lcd_stall_charged_no_bias() {
            4 >> mmio.is_double_speed_mode() as i32
        } else {
            0
        }
    }
    // Resolve the DMG mid-mode-3 BGP-write glitch for the just-finished line and paint
    // the spikes into the framebuffer. Called at the mode-3 -> HBlank transition, when
    // every write of the line is known. The glitch is a TWO-WRITE collision: a write
    // spikes its own pixel (looked up through `old | new`) only when it has a
    // neighboring mid-mode-3 write within `BGP_SPIKE_CADENCE_CC` (SET/RESTORE
    // pairs, ~12 dots apart). A single write, or one spaced wider (one write per
    // line, or 60-148 dots apart), has no colliding neighbor and paints no spike —
    // leaving the clean palette transition. Resolving at line end (all writes known) lets a SET
    // write spike on the strength of its FUTURE RESTORE neighbor, which a per-write gate
    // could not see. DMG-only; the CGB path uses true-color palette RAM (no collapse).
    pub(in crate::ppu) fn resolve_bgp_spikes(&mut self, mmio: &mmio::Mmio) {
        if mmio.is_cgb() || self.bgp_writes.len() < 2 {
            return;
        }
        let ly = mmio.read(LY);
        if ly >= 144 {
            return;
        }
        let writes = std::mem::take(&mut self.bgp_writes);
        for i in 0..writes.len() {
            let (cc, col, glitch) = writes[i];
            // Neighboring write within the tight cadence, in either direction.
            let has_neighbor = writes.iter().enumerate().any(|(j, &(occ, _, _))| {
                j != i && cc.abs_diff(occ) <= BGP_SPIKE_CADENCE_CC
            });
            if !has_neighbor || col >= 160 {
                continue;
            }
            // Re-map the BG pixel drawn at `col` through the OR-glitched palette. The
            // per-dot draw stored its BG color index in `line_bg_idx` (-1 = a sprite won
            // this column, or it was BG-disabled; leave those untouched).
            let bg_idx = self.line_bg_idx[col as usize];
            if bg_idx < 0 {
                continue;
            }
            let fb_offset = (ly as u16) * 160 + col as u16;
            self.out.fb_a[fb_offset as usize] = (glitch >> (2 * bg_idx as u8)) & 0x03;
        }
    }
    /// FF48 (OBP0) write hook. See `on_bgp_write`; affects sprite palette 0.
    pub(crate) fn on_obp0_write(&mut self, value: u8, _mmio: &mmio::Mmio) {
        if self.state != State::PixelTransfer || self.disabled {
            return;
        }
        let lat = obp_latency(_mmio.is_cgb())
            + if _mmio.is_cgb() { Self::cgb_halt_wake_write_bias(_mmio) } else { 0 };
        let boundary = self.pal_write_boundary(lat);
        Self::push_pal_history(&mut self.obp0_history, boundary, value);
        let apply = self.pal_write_apply_tick(lat);
        Self::push_pal_dot_history(&mut self.obp0_dot_history, apply, value);
    }
    /// FF49 (OBP1) write hook. See `on_bgp_write`; affects sprite palette 1.
    pub(crate) fn on_obp1_write(&mut self, value: u8, _mmio: &mmio::Mmio) {
        if self.state != State::PixelTransfer || self.disabled {
            return;
        }
        let lat = obp_latency(_mmio.is_cgb())
            + if _mmio.is_cgb() { Self::cgb_halt_wake_write_bias(_mmio) } else { 0 };
        let boundary = self.pal_write_boundary(lat);
        Self::push_pal_history(&mut self.obp1_history, boundary, value);
        let apply = self.pal_write_apply_tick(lat);
        Self::push_pal_dot_history(&mut self.obp1_dot_history, apply, value);
    }
    // Display column at which a mid-mode-3 palette write becomes visible: the next
    // column to be popped (`self.x`) plus the register's pipeline latency in dots.
    // While the pipeline is still warming up (`pixel_transfer_warmup > 0`, before any
    // column has popped) the write lands before column 0 is plotted, so it colors
    // column 0 onward — the `+latency` delay is absorbed by the remaining warmup.
    // Pre-visible phase of a chopped WX<7 window start: the early activation
    // zeroed the warmup, but a write landing before the line's pos-0 dot still
    // colors the whole line (the column-0 pixel pops at/after pos 0), and must
    // not seed a two-write spike either — exactly like a write during the warmup.
    fn in_previsible_prologue(&self) -> bool {
        if self.pixel_transfer_warmup > 0 {
            return true;
        }
        if self.x == 0 && self.m3.m3_discard_target >= 0 && self.win_fetch_anchor.is_some() {
            let base = self.m3.m3_arm_dot + 12 - (self.m3.m3_arm_dot & 1)
                + self.m3.m3_discard_target as u128;
            return self.ticks < base;
        }
        false
    }
    fn pal_write_boundary(&self, latency: i32) -> u64 {
        if self.in_previsible_prologue() {
            return 0;
        }
        (self.x as i32 + latency).clamp(0, 160) as u64
    }
    // Dot at which a mid-mode-3 palette write becomes visible to the pixel
    // pops (the dot-space analog of `pal_write_boundary`; see
    // `obp0_dot_history`). During the previsible prologue the write applies
    // before any visible pop, i.e. tick 0.
    fn pal_write_apply_tick(&self, latency: i32) -> u128 {
        if self.in_previsible_prologue() {
            return 0;
        }
        self.ticks + latency.max(0) as u128
    }
    // Append an (apply_tick, value) dot-keyed palette entry; same last-write-
    // wins collapse as `push_pal_history`.
    fn push_pal_dot_history(hist: &mut Vec<(u128, u8)>, apply: u128, value: u8) {
        if let Some(last) = hist.last_mut()
            && last.0 == apply
        {
            last.1 = value;
            return;
        }
        hist.push((apply, value));
    }
    // Append a (boundary_col, value) palette-history entry. If the last entry shares
    // the same boundary column (two writes resolving to the same display column),
    // overwrite it so only the last write at that column wins.
    fn push_pal_history(hist: &mut Vec<(u64, u8)>, boundary: u64, value: u8) {
        if let Some(last) = hist.last_mut()
            && last.0 == boundary
        {
            last.1 = value;
            return;
        }
        hist.push((boundary, value));
    }
    /// FF42 (SCY) write hook. The CPU readback of FF42 is immediate (handled by
    /// mmio), but the BG fetcher must see the new SCY only ~N dots later, the
    /// write-side analog of the wy1/wy2 delayed latches: rustyboi otherwise
    /// resolves the write pre-tick and the fetcher re-reads it live one M-cycle
    /// too early vs hardware. Schedule the delayed apply against the write cc.
    pub(crate) fn on_scy_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        if self.disabled {
            self.scy_delayed = value;
            self.scy_apply_cc = wy2_disabled();
            return;
        }
        let ds = mmio.is_double_speed_mode();
        let cc = self.write_cc(ds);
        // CGB-only: rustyboi's DMG fetcher already samples SCY at the
        // hardware-correct dot (delay 0); only the CGB core sees the mid-M3 write
        // one M-cycle too early (the `_2/_4/_6` straddle pairs vs the passing
        // `_1/_3/_5`). A DMG delay regresses the DMG scy_during_m3 cases.
        // SCY=2 is the swept optimum (fixes 20 CGB scy_during_m3 straddle cases,
        // zero regression; 1 -> -4, 3 -> -14, 4 -> +8 regresses).
        let delay = if mmio.is_cgb_features_enabled() {
            SCY_DELAY.max(0) as u64
        } else {
            0
        };
        self.scy_pending = value;
        self.scy_apply_cc = cc + delay;
        self.stat_sched_touched();

        // DMG BG bus-glitch SCY journal (see bg_wg_apply): record the exact
        // bus transition time of a mid-mode-3 SCY write; BG fetch reads
        // resolve SCY at their reconstructed hardware dots against it, and
        // the in-flight tile's already-executed reads are re-resolved
        // (bg_retro_repair).
        if !mmio.is_cgb_features_enabled() && self.state == State::PixelTransfer {
            let old = self
                .wg
                .bg_scy_hist
                .last()
                .map(|&(_, _, new)| new)
                .unwrap_or(self.scy_delayed);
            if old != value {
                // Transition placement: the new row/line address bits are
                // effective for reads strictly PAST the write's commit cc —
                // the same phase the live per-substep SCY re-read gives an
                // unshifted read (writes commit pre-tick; the first fetch dot
                // of the write M-cycle already sees the new value). No OR
                // edge: the LCDC pulse captures cannot separate OR from
                // clean-new/clean-old at the transition dots (old side is
                // 0x00 there), and the SCY capture rejects an OR at this
                // phase (whole-row blend pollution).
                self.wg.bg_scy_hist.push((cc, old, value));
                self.bg_retro_repair(mmio);
            }
        }
    }
    /// FF43 (SCX) write hook. See `on_scy_write`.
    pub(crate) fn on_scx_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        if self.disabled {
            self.scx_delayed = value;
            self.scx_apply_cc = wy2_disabled();
            return;
        }
        let ds = mmio.is_double_speed_mode();
        let cc = self.write_cc(ds);
        // SCX has no positive lever in the sweep (delay 1/2 == net-zero vs the
        // live read); the SCX-write straddles need the read-cc convergent root,
        // out of scope. Applied live (delay 0).
        self.scx_pending = value;
        self.scx_apply_cc = cc;
        self.stat_sched_touched();

        // DMG BG grid SCX journal (see bg_wg_apply): record the mid-mode-3 SCX
        // write so the tile-map column resolves it at the tile's reconstructed
        // hardware TileNumber dot instead of the stall-displaced live dot.
        if !mmio.is_cgb_features_enabled() && self.state == State::PixelTransfer {
            let old = self
                .wg
                .bg_scx_hist
                .last()
                .map(|&(_, _, new)| new)
                .unwrap_or(self.scx_delayed);
            if old != value {
                self.wg.bg_scx_hist.push((cc, old, value));
            }
        }

        // Exact-cc f1-discard latch. The "before" value is whatever the f1 loop
        // sees right now (resolving any already-pending latch up to this write's
        // cc); the new value becomes visible at write_cc + 2*cgb (a hardware SCX
        // change becomes visible at cc + 2*cgb). NB: mmio already holds `value` (the
        // store ran before this hook), so `scx_f1_at_cc` must derive the old
        // value from the latch state, never from mmio.read(SCX).
        let cgb = mmio.is_cgb_features_enabled();
        self.scx_prev_f1 = self.scx_f1_pending_at_cc(cc);
        self.scx_f1_new = value;
        // The hardware SCX change (visible at cc + 2*cgb) runs in PPU dot units: the new
        // SCX becomes visible to the f1 fine-scroll loop one PPU dot after the
        // write (CGB). `abs_cc` is the master clock (1 dot = 1<<ds cc), so the
        // dot delay scales with double speed -- otherwise a mid-f1 SCX write
        // lands one f1 iteration too early at DS (scx_0367c0/scx_0761c0 _ds).
        let ds = mmio.is_double_speed_mode() as u32;
        self.scx_f1_apply_cc = cc + if cgb { 2u64 << ds } else { 0 };

        // sub-cc column lever: record the apply boundary on the PLOT clock. The
        // BG fetcher chooses old/new per tile by comparing the tile's plot cc to
        // this. Persists for the line (does not reset on apply).
        self.m3.subcc_scx_old = self.scx_delayed;
        self.m3.subcc_scx_new = value;
        self.m3.subcc_scx_apply_cc = cc + if cgb { 2u64 << ds } else { 0 };
        // Arm the single-tile re-key only when a BG tile is mid-fetch (its
        // column was already committed under the OLD scx and it has not yet
        // pushed). If the fetcher is at TileNumber, the next fetch will read
        // the (about-to-be-NEW) scx itself; no in-flight straddle exists.
        self.m3.subcc_rekey_armed = !self.disabled
            && self.state == State::PixelTransfer
            && self.x > 0
            && !self.fetcher.is_fetching_window()
            && !self.fetcher.fetch_state_is_tile_number()
            && self.fetcher.subcc_last_column_inputs().2 == self.m3.subcc_scx_old;

        // First-tile (f1) prologue straddle (DMG SS): the write lands at x==0
        // (still in the discard prologue) but the first displayed tile is already
        // queued (fifo>=8) and the 2nd tile is mid-fetch (its column was latched
        // under the OLD scx one dot before this write). On hardware that 2nd tile
        // plots after the write, so re-key it to the NEW scx on its next push.
        // Gated on a low-X sprite (OAM x <= 8): the sprite-fetch dot during the
        // discard prologue delays the BG fetcher one tile, so the in-flight 2nd
        // tile latched OLD one dot before the write (vs no in-flight straddle
        // without the sprite). The no-sprite SS straddle (scx_during_m3_4/5) is
        // handled correctly by the steady-state gap==4 rekey and must NOT re-key
        // here, so the sprite gate is required to protect those cases.
        let sprites_enabled = self.lcdc_has(LCDCFlags::SpriteDisplayEnable);
        let low_x_sprite = sprites_enabled
            && self.sprites_on_line.iter().any(|s| s.x <= 8);
        self.m3.prologue_rekey_armed = !self.disabled
            && !cgb
            && ds == 0
            && self.state == State::PixelTransfer
            && self.x == 0
            && low_x_sprite
            && self.fetcher.pixel_fifo.size() >= 8
            && !self.fetcher.is_fetching_window()
            && !self.fetcher.fetch_state_is_tile_number()
            && self.fetcher.subcc_last_column_inputs().2 == self.m3.subcc_scx_old;
    }
    /// SCX value visible to the f1 fine-scroll discard at PPU `cc`, honoring the
    /// CGB `update(cc + 2*cgb)`-before-`the SCX-write handling` write delay. Before the pending
    /// write's apply cc the f1 sees the pre-write value; at/after it sees the
    /// new. Derived purely from the latch state (mmio already holds the latest
    /// write), seeded with the M3-start SCX in `scx_prev_f1`.
    pub(in crate::ppu) fn scx_f1_pending_at_cc(&self, cc: u64) -> u8 {
        if self.scx_f1_apply_cc != wy2_disabled() && cc >= self.scx_f1_apply_cc {
            self.scx_f1_new
        } else {
            self.scx_prev_f1
        }
    }
    /// OBJ-size (large = 8x16) visible to the OAM scan at PPU `cc`, honoring the
    /// CGB `an LCDC write taking effect at cc+2` write delay. Before the pending size write's
    /// apply cc the scan sees the pre-write size; at/after it sees the new. With
    /// no pending change (`apply_cc == disabled`) it falls back to the live LCDC
    /// bit2, so the steady-state per-slot snapshot is unchanged.
    pub(in crate::ppu) fn objsize_large_at_cc(&self, cc: u64) -> bool {
        if self.objsize_apply_cc != wy2_disabled() {
            // Strict `>`: an OAM slot read exactly AT the apply cc still sees the
            // pre-write size (the late_sizechange2_sp01_ds bracket: ds_1's slot
            // cc is strictly past apply -> new size IN; ds_2's slot cc equals
            // apply -> old size OUT, the 1-slot boundary hardware resolves).
            if cc > self.objsize_apply_cc {
                self.objsize_new_large
            } else {
                self.objsize_prev_large
            }
        } else {
            self.lcdc_has(LCDCFlags::SpriteSize)
        }
    }
    pub(crate) fn on_stat_register_write(&mut self, mmio: &mut mmio::Mmio) {
        // The DMG STAT-write bug fires on any FF41 write, even one that leaves
        // the enable bits unchanged. Track whether this was an FF41 write so the
        // unchanged-value case still runs lcdstat_change below.
        let ff41_written = mmio.take_ff41_write_pending();
        // DMG "line 154" STAT-write VBlank-IF glitch (gbmicrotest
        // stat_write_glitch_l154_d). A FF41 write straddling the frame-wrap
        // boundary (LY 153->0 VBlank exit, first dots of the new frame) clears
        // the still-pending VBlank IF bit on real DMG-CPU-08 — the shared
        // VBlank/STAT interrupt-line glitch. `l154_vblank_glitch_window` is armed
        // at the frame wrap and disarmed a few dots into line 0/1, so only a write
        // at that exact boundary is affected. DMG-only (CGB has no STAT-write bug).
        if ff41_written
            && self.l154_vblank_glitch_window
            && !self.disabled
            && !mmio.is_cgb_features_enabled()
        {
            let cur_if = mmio.read(registers::INTERRUPT_FLAG);
            if cur_if & (registers::InterruptFlag::VBlank as u8) != 0 {
                mmio.write(
                    registers::INTERRUPT_FLAG,
                    cur_if & !(registers::InterruptFlag::VBlank as u8),
                );
            }
        }
        // Keep the LYC=LY readback flag (FF41 bit 2) in sync regardless of LCD
        // state; only its IRQ side-effects are gated by enable.
        if self.disabled {
            // STAT-write quirk (the FF41 write path): with the LCD off, an FF41
            // write while the LYC=LY flag is set and LYC IRQ was disabled flags
            // a STAT IRQ. On CGB the written data must also set LYC-IRQ-enable;
            // on DMG it fires regardless of the written value.
            let live_stat = mmio.read(LCD_STATUS);
            let new_stat = live_stat & 0x78;
            let old_stat = self.stat_reg_committed & 0x78;
            let lycflag = live_stat & 0x04 != 0;
            let old_lycen = old_stat & stat_irq::STAT_LYCEN != 0;
            let new_lycen = new_stat & stat_irq::STAT_LYCEN != 0;
            let cgb = mmio.is_cgb_features_enabled();
            let data_ok = if cgb { new_lycen } else { true };
            if ff41_written && lycflag && !old_lycen && data_ok {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            // Keep the IRQ sources' shadow registers current so a later enable
            // sees the right values (hardware runs its LCDSTAT / LYC-register change handling even
            // while off, just skipping event scheduling).
            self.stat_reg_committed = new_stat;
            return;
        }

        let new_stat = mmio.read(LCD_STATUS) & 0x78;
        let new_lyc = mmio.read(LYC);
        let old_stat = self.stat_reg_committed & 0x78;
        let old_lyc = self.lyc_irq.lyc_reg_src();

        // FF41 (STAT) write. Run unconditionally on any FF41 write (even a
        // same-value write) to reproduce the DMG STAT-write IRQ bug; the CGB
        // trigger path self-guards on newly-set bits, so this is a no-op there.
        if ff41_written || new_stat != old_stat {
            self.lcdstat_change(new_stat, mmio);
        }
        // FF45 (LYC) write.
        if new_lyc != old_lyc {
            self.lyc_reg_change(new_lyc, mmio);
        }

        // Re-sync the LYC=LY readback flag after the change.
        self.sync_lyc_flag(mmio);
    }
    fn sync_lyc_flag(&self, mmio: &mut mmio::Mmio) {
        let effective_ly = self.effective_ly_for_lyc_compare(mmio);
        if mmio.read(LYC) == effective_ly {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2));
        } else {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2));
        }
    }
    /// The m0 IRQ time to use in the stat-change immediate-trigger check.
    /// Mirrors hardware: when the scheduled m0 IRQ is disabled but the current
    /// line's mode 0 is still ahead, predict it from the renderer; otherwise use
    /// the scheduled value.
    fn m0_irq_time_for_trigger(&self, mmio: &mmio::Mmio, lc: &stat_irq::LyCounter, cc: u64) -> u64 {
        // The hardware STAT-change-triggers check needs the m0 IRQ time of the *current
        // line*. Our `sched_m0irq` may hold a stale current-line value during
        // HBlank (it is only cleared to DISABLED when the m0 source fires). The
        // DMG/CGB branch logic only cares whether m0IrqTime is before or after
        // `the LY counter.time()` (next-LY): if mode 0 is already active (HBlank) the
        // current line's m0 has passed and the next is on a later line, i.e.
        // `>= lc.time`; during mode 2/3 it is still ahead this line (`< time`).
        // Mode 3 (PixelTransfer): the current line's m0 is ahead, and the
        // closed-form `m0_time_master` is this line's exact mode-0 time — use the exact
        // the hardware mode-0 IRQ event time (the xpos-166 advance time). Mode 2
        // (OAMSearch): `m0_time_master` still holds the PREVIOUS line's value, so
        // keep the per-dot `sched_m0irq` (this line's armed m0). Both clamp below
        // next-LY so the "m0 ahead this line" branch is taken.
        let sched_or_future = if self.sched_m0irq == stat_irq::DISABLED_TIME {
            lc.time.saturating_sub(1)
        } else {
            self.sched_m0irq.min(lc.time.saturating_sub(1))
        };
        match self.state {
            // Mode 0 active: report a time at/after the next LY so the "m0 has
            // occurred" branch is taken.
            State::HBlank => lc.time,
            // VBlank: no m0 this line; far future.
            State::VBlank => stat_irq::DISABLED_TIME,
            State::PixelTransfer => self
                .m0_irq_time_exact(mmio)
                .map(|t| {
                    // Hardware runs pending events before the FF41-write trigger
                    // check: if the write cc has already passed the mode-0 STAT
                    // IRQ time (the xpos-166 advance time), that event fired and
                    // rescheduled the m0 event onto the next line
                    // (> the LY counter.time()). Report a next-LY value so the trigger
                    // takes the "m0 already occurred" branch and the enable
                    // immediately flags the STAT IRQ — the `_2`/`_3`/`_4` bracket
                    // where the window/sprite-deferred m0 xpos lies just before the
                    // enable write.
                    if cc >= t {
                        lc.time
                    } else {
                        t.min(lc.time.saturating_sub(1))
                    }
                })
                .unwrap_or(sched_or_future),
            _ => sched_or_future,
        }
    }
    /// The exact hardware mode-0 STAT-IRQ event time for the current line, used
    /// by the FF41/FF45 latch + immediate-trigger comparisons. The hardware m0 IRQ
    /// fires at the xpos-166 advance time `mode-0 time - (1<<ds)`, one xpos before
    /// the mode-3 -> mode-0 transition (`mode-0 time` = the xpos-167 advance time,
    /// our `m0_time_master`). Returns `None` when no closed-form master exists
    /// (window mid-line / first line after enable), in which case callers fall
    /// back to the per-dot delivery value (`sched_m0irq`).
    fn m0_irq_time_exact(&self, mmio: &mmio::Mmio) -> Option<u64> {
        let ds = mmio.is_double_speed_mode() as i64;
        // `m0_time_master` is the master-cc mode-0 time (= the xpos-167 advance time).
        // The STAT/LYC write-trigger comparisons run in abs-cc units (the same
        // `cc = write_cc()` / `sched_m0irq` clock), so rebase by `p_now`
        // (abs_cc = master_cc - p_now). The mode-0 IRQ fires one xpos earlier:
        // the xpos-166 advance time = mode-0 time - (cost(166->167) << ds), where the
        // 166->167 step costs one dot plus any window-start (WX=166) / right-edge
        // sprite penalty that lands in that final xpos (`m0irq_xpos166_advance`).
        //
        // `m0_time_master` (via `m0_time_exact`) carries a `+1` the LY time correction
        // tuned for the C1 *read* access-cc phase (`access_cc + 2 < mode-0 time`). The
        // *write* cc (write_cc_off = 0) resolves the latch/trigger one cc earlier,
        // so that read-phase `+1` over-counts the write-boundary IRQ time by 1 —
        // subtract it back out to land the write-phase boundary exactly.
        let is_cgb = mmio.is_cgb_features_enabled();
        let adv = self.m0irq_xpos166_advance(mmio, is_cgb);
        self.m0.m0_time_master
            .map(|m0t| (m0t as i64 - ((1 + adv) << ds) - self.p_now as i64 - 1).max(0) as u64)
    }
    /// The current-line mode-0 IRQ time for the FF41/FF45 *latch* comparisons
    /// (the hardware mode-0 IRQ event time). During mode 3 the closed-form
    /// `m0_time_master`-derived exact value (the xpos-166 advance time) is this
    /// line's m0; in HBlank/mode 2/VBlank/window the per-dot `sched_m0irq` already
    /// carries the relevant scheduled (next-line) value, matching the pre-C5 latch
    /// behaviour, so keep it there to avoid disturbing those boundaries.
    fn m0_irq_time_latch(&self, mmio: &mmio::Mmio, lc: &stat_irq::LyCounter) -> u64 {
        match self.state {
            State::PixelTransfer => self
                .m0_irq_time_exact(mmio)
                .map(|t| t.min(lc.time.saturating_sub(1)))
                .unwrap_or(self.sched_m0irq),
            _ => self.sched_m0irq,
        }
    }
    /// Handles an LCD-STAT (FF41) change. `data` is the new FF41 enable bits (& 0x78).
    fn lcdstat_change(&mut self, data: u8, mmio: &mut mmio::Mmio) {
        let cc = self.write_cc(mmio.is_double_speed_mode());
        let lc = self.ly_counter(mmio);
        let old = self.stat_reg_committed & 0x78;
        self.stat_reg_committed = data;
        self.lyc_irq.stat_reg_change(data, &lc, cc);

        // If m0 IRQ just got enabled and isn't scheduled, arm it from the
        // current line's mode-0 prediction.
        if (data & stat_irq::STAT_M0EN != 0) && self.sched_m0irq == stat_irq::DISABLED_TIME {
            self.arm_m0irq_for_current_line(mmio, self.first_line_after_enable);
        }
        let m2 = stat_irq::mode2_irq_schedule(data, &lc, cc);
        self.sched_m2irq = if m2 == stat_irq::DISABLED_TIME { m2 } else { (m2 as i64 + Self::m2_off(mmio.is_double_speed_mode())) as u64 };
        self.sched_lycirq = self.lyc_irq.time;

        // STAT-write IRQ timing follows the CGB LCD controller on CGB hardware
        // (incl. DMG-compat mode), matching the hardware console-is-CGB gate.
        let cgb = mmio.is_cgb();
        let lyc_reg = self.lyc_irq.lyc_reg_src();
        // The hardware STAT-change-triggers-STAT-IRQ (DMG) recomputes the current line's
        // m0 IRQ time when it is unscheduled but mode 0 is still ahead this
        // line. Reproduce that so enabling m0 during mode 2/3 sees a future m0.
        let m0_for_trigger = self.m0_irq_time_for_trigger(mmio, &lc, cc);
        let triggers = if cgb {
            stat_irq::stat_change_triggers_cgb(
                old,
                data,
                &lc,
                cc,
                m0_for_trigger,
                lyc_reg,
                stat_irq::StatWritePhase {
                    agb: mmio.is_agb(),
                    rephased: !mmio.halt_grid_quantized(),
                },
            )
        } else {
            stat_irq::stat_change_triggers_dmg(old, &lc, cc, m0_for_trigger, lyc_reg)
        };
        if triggers {
            mmio.request_interrupt(registers::InterruptFlag::Lcd);
        }

        // Latch the new STAT bits against the exact current-line mode-0 IRQ time
        // (the hardware mode-0 IRQ event time = the xpos-166 advance time)
        // during mode 3, keeping the per-dot `sched_m0irq` next-line value
        // elsewhere (HBlank/mode 2/window) — see `m0_irq_time_latch`.
        let m0_latch = self.m0_irq_time_latch(mmio, &lc);
        self.mstat_irq.stat_reg_change(
            data,
            m0_latch,
            self.sched_m1irq,
            self.sched_m2irq,
            cc,
            cgb,
        );
        self.stat_sched_touched();
    }
    /// Handles an LYC-register change.
    fn lyc_reg_change(&mut self, data: u8, mmio: &mut mmio::Mmio) {
        let old = self.lyc_irq.lyc_reg_src();
        if data == old {
            return;
        }
        let cc = self.write_cc(mmio.is_double_speed_mode());
        let lc = self.ly_counter(mmio);
        let stat = self.stat_reg_committed;
        // LYC-write coincidence/IRQ timing follows the CGB LCD controller on CGB
        // hardware (incl. DMG-compat mode); hardware gates on the console-is-CGB signal.
        let cgb = mmio.is_cgb();
        let ds = mmio.is_double_speed_mode();

        // Trigger/latch against the current-line mode-0 IRQ time: the closed-form
        // `m0_time_master`-derived exact value (the hardware xpos-advance-time
        // (166)) during mode 3, the per-dot `sched_m0irq` (next-line scheduled m0,
        // > lc.time) elsewhere — see `m0_irq_time_latch`.
        let m0_for_trigger = self.m0_irq_time_latch(mmio, &lc);
        self.lyc_irq.lyc_reg_change(data, &lc, cc);
        self.mstat_irq
            .lyc_reg_change(data, m0_for_trigger, self.sched_m2irq, cc, ds, cgb);
        self.sched_lycirq = self.lyc_irq.time;

        // Immediate-trigger m0 time = the hardware m0 event time, which
        // is the *current line's* m0 while it is still ahead (mode 2/3) and the next
        // line's (> lc.time) once mode 0 has passed. `m0_irq_time_latch` is correct
        // in HBlank/mode 3 but reports DISABLED during OAMSearch (the current line's
        // m0 has not yet been armed into `sched_m0irq`); there the current line's m0
        // is still ahead but before next-LY, so substitute `lc.time - 1`. This makes
        // `lyc_change_blocked_by_m0_or_m1` resolve the line-start LYC=LY coincidence
        // (lycwirq_trigger_m0_late_lyc45 `_5`) without disturbing the HBlank
        // line-end LYC writes (lycwirq_trigger_m0_late `_1`/`_2`/`_3`).
        let m0_latch = self.m0_irq_time_latch(mmio, &lc);
        let m0_for_imm = if matches!(self.state, State::OAMSearch)
            && m0_latch == stat_irq::DISABLED_TIME
        {
            lc.time.saturating_sub(1)
        } else {
            m0_latch
        };
        if stat_irq::lyc_change_triggers_stat_irq(old, data, &lc, cc, stat, m0_for_imm, cgb) {
            if cgb && !ds {
                self.sched_oneshot_statirq = cc + 5;
            } else {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
        }
        self.stat_sched_touched();
    }
    /// The absolute clock value attributed to a register write. The write hook
    /// fires after the FF4x store but before this M-cycle's 4 dots tick, so the
    /// renderer's current dot is `abs_cc - 1`.
    ///
    /// At double speed `abs_cc` advances by 2 per PPU step and the PPU only
    /// steps on even CPU T-phases, so `abs_cc` alone can only place a write on
    /// an even half-dot. `write_subdot` carries the true sub-dot parity of the
    /// resolving CPU write (0 on an even T-phase, 1 on an odd one), giving the
    /// STAT model half-PPU-dot precision.
    pub(in crate::ppu) fn write_cc(&self, ds: bool) -> u64 {
        let off = WRITE_CC_OFFSET;
        // `write_subdot` carries the sub-PPU-dot parity of the resolving CPU
        // write. In practice the STAT/render tests align via whole-instruction
        // polling loops, so writes land on M-cycle (even) phases and this term
        // is 0; it remains wired for the rare odd-phase write (post-HALT-1cc).
        let sub = if ds { self.speed.write_subdot as i64 } else { 0 };
        (self.abs_cc as i64 + off + sub).max(0) as u64
    }
    /// LY value used for the LYC=LY comparison. On hardware the compare uses
    /// the next line's LY in the last 2 dots of the current line
    /// (`the LYC-compare-LY calc` `time-to-next-LY <= 2`), so the LYC=LY flag rises one line
    /// early. Line 153's mid-line ly=0 transient is handled separately in
    /// Phase D by writing FF44 directly, so this only anticipates lines
    /// 0..=152 (line 153 -> 0 already came through `write_ly_from_ppu`).
    pub(in crate::ppu) fn effective_ly_for_lyc_compare(&self, mmio: &mmio::Mmio) -> u8 {
        let ly = mmio.ppu_io_reg(LY);
        // STAT LYC compare: the next-line anticipation window is
        // `time-to-next-LY > 2 - (!isDoubleSpeed() && isAgb())`. The renderer's
        // line-cycle equivalent is `ticks >= 456 - thresh`; AGB single-speed
        // lowers the threshold from 2 to 1, extending the window one dot earlier.
        let agb_ss = mmio.is_agb() && !mmio.is_double_speed_mode();
        let anticipate_from = if agb_ss { 455 } else { 454 };
        if self.ticks < anticipate_from {
            return ly;
        }
        match self.state {
            State::HBlank if ly < 143 => ly + 1,
            State::HBlank if ly == 143 => 144,
            State::VBlank if (144..152).contains(&ly) => ly + 1,
            // Line 152 -> 153 transition: still anticipate (next line is 153).
            State::VBlank if ly == 152 => 153,
            _ => ly,
        }
    }
}

use crate::memory::Addressable;
use crate::memory::mmio::Mmio;
use crate::ppu::{self, Ppu};
use std::ops::{Deref, DerefMut};

/// ds-engine STAGE 2: the faithful CPU exact-cc spine gate (RB_FAITHFUL). When
/// OFF (default) the CPU step/service paths are byte-identical to HEAD. When ON
/// the CPU runs the faithful prefetch model (prefetch-at-boundary +
/// execute-no-refetch + service pc-rewind) and event-cc interrupt dispatch (an
/// IRQ is serviceable only once the boundary access cc has reached its recorded
/// fire cc, not merely once its IF bit is set). This stage does NOT touch the
/// timer access anchor — RB_EXACTCC (stage 1) already owns that — so the +106
/// well from the prior ptz-faithful (which welded CC_OFF 5->1 into this gate) is
/// avoided: here RB_FAITHFUL only changes the CPU's boundary/dispatch phasing.
/// Read once, OnceLock-cached.
pub(crate) fn faithful_enabled() -> bool {
    // ds-engine STAGE 7: permanently on.
    true
}

/// ds-engine STAGE 6: the run-to-next-event scheduler gate (RB_EVENTLOOP). When
/// OFF (default) `tick_m` is the per-dot 4×`tick_t` crank, byte-identical to
/// HEAD. When ON the CPU advances `master_cc` by the access duration and a
/// single `run_to(target_cc)` resolves every peripheral up to that cc — the
/// CPU no longer cranks peripherals dot-by-dot, it requests "advance the world
/// to this cc". `run_to` itself, in min-event order, drains each scheduled
/// peripheral whose fire cc <= target. At double speed a CPU access lands on an
/// exact odd/even `master_cc` and `run_to` resolves every peripheral to THAT
/// cc, so the half-dot is exact by construction (no parity gate).
///
/// This stage keeps `run_to`'s per-cc resolution primitive identical to the
/// proven stage-5 per-dot path (it still resolves one dot at a time internally),
/// so flag-on MUST match flag-on stage-5 numbers exactly: it is a control-flow
/// refactor (the CPU drives by duration + one run_to, not by hand-cranked dots),
/// not a behavior change. Any divergence reveals a peripheral still carrying
/// hidden per-instruction (not per-cc) state — that is the diagnostic this
/// stage exists to surface. Read once, OnceLock-cached.
pub(crate) fn eventloop_enabled() -> bool {
    // ds-engine STAGE 7: permanently on.
    true
}

/// A tick-aware view over the system. CPU memory accesses go through `read`/
/// `write`, which advance every peripheral one M-cycle (4 dots) so each access
/// observes/mutates live state at its true intra-instruction cycle. Everything
/// else on `Mmio` is reached transparently via `Deref`.
pub struct Bus<'a> {
    pub mmio: &'a mut Mmio,
    pub ppu: &'a mut Ppu,
    // Dots elapsed since this instruction started; drives the double-speed PPU
    // gate (resets per instruction, matching the old per-`cpu_cycle` loop).
    dot: u32,
    ticked: u32,
}

impl<'a> Bus<'a> {

    pub fn new(mmio: &'a mut Mmio, ppu: &'a mut Ppu) -> Self {
        Bus {
            mmio,
            ppu,
            dot: 0,
            ticked: 0,
        }
    }

    pub fn ticked_dots(&self) -> u32 {
        self.ticked
    }

    /// Advance every peripheral by one dot (per-dot crank). Bookkeeping for the
    /// per-instruction `dot`/`ticked` counters is applied here; the actual
    /// world-advance is in `resolve_one_dot` so the event-loop `run_to` can reuse
    /// the identical resolution without re-touching the per-instruction counters.
    fn tick_t(&mut self) {
        self.resolve_one_dot();
        self.dot = self.dot.wrapping_add(1);
        self.ticked += 1;
    }

    /// STAGE 6: advance the whole world by exactly one dot (one `master_cc`).
    /// This is the per-cc resolution primitive shared by the per-dot crank
    /// (`tick_t`) and the event-loop driver (`run_to`). It steps each peripheral
    /// for this dot in the SAME order the per-dot path always used, so the
    /// event-loop path resolves byte-identically to the per-dot path. The
    /// per-instruction `dot`/`ticked` counters are NOT touched here — callers own
    /// that bookkeeping (only `tick_t` keeps them, matching HEAD semantics; the
    /// event loop drives off `master_cc` directly).
    fn resolve_one_dot(&mut self) {
        self.mmio.step_timer();
        self.mmio.step_serial();
        self.mmio.step_dma();

        let double_speed = self.mmio.is_double_speed_mode();
        // Gate the PPU/audio step on the *persistent* T-phase parity so the
        // PPU's even-dot stepping stays aligned with the true accumulated cc
        // across instruction boundaries (per-instruction `dot` would re-anchor
        // the phase to the instruction start every M-cycle).
        if !double_speed || self.mmio.cpu_t_phase() % 2 == 0 {
            self.ppu.step_scheduled_stat_events(self.mmio);
            self.mmio.step_audio();
            self.ppu.step(self.mmio);
        } else {
            // Double-speed odd half-dot: the renderer steps once per pixel-dot
            // (the even phase above), but the CPU runs a second M-cycle here. Run
            // a STAT/IRQ sub-dot so events scheduled at an odd `abs_cc` fire at
            // the true half-dot instead of being rounded to the next render dot.
            self.ppu.step_subdot(self.mmio);
        }
        // HDMA triggers on the PPU's exact mode-0 (HBlank) entry, so check it
        // AFTER the PPU has stepped this dot. Prefer the renderer's cycle-exact
        // `hdma_period` predicate (Gambatte `isHdmaPeriod`); fall back to the
        // STAT mode-edge when no closed-form mode-0 dot is available.
        let period = self.ppu.hdma_period(double_speed);
        self.mmio.step_hdma(period);
        // Drain any deferred HDMA block writes whose sub-M-cycle delay has
        // elapsed (Gambatte writes byte i at fire + (2 + 2*ds)). Runs after
        // step_hdma so a block flagged this same dot begins its countdown here
        // and only commits to VRAM on a later dot.
        self.mmio.step_hdma_deferred();
        self.ppu.step_lcdc_events(self.mmio);

        self.mmio.advance_cpu_t_phase();
    }

    /// Tick the remaining internal (non-memory) cycles of an instruction.
    pub fn tick_remaining(&mut self, total_cycles: u32) {
        let remaining = total_cycles.saturating_sub(self.ticked);
        if eventloop_enabled() {
            // STAGE 6: resolve the leftover internal dots via the run-to-cc
            // driver too, so the whole instruction advances off cc targets.
            let target = self.mmio.master_cc().wrapping_add(remaining as u64);
            self.run_to(target);
        } else {
            for _ in 0..remaining {
                self.tick_t();
            }
        }
    }

    fn tick_m(&mut self) {
        if eventloop_enabled() {
            // STAGE 6: the CPU advances `master_cc` by the access duration (one
            // M-cycle = 4 dots) and a single `run_to` resolves every peripheral
            // up to that cc. The CPU no longer hand-cranks each dot; it requests
            // "advance the world to target_cc".
            let target = self.mmio.master_cc().wrapping_add(4);
            self.run_to(target);
        } else {
            for _ in 0..4 {
                self.tick_t();
            }
        }
    }

    /// STAGE 6: advance the world to `target_cc`, resolving every peripheral up
    /// to (and including) that cc. This is the run-to-next-event driver: the CPU
    /// hands it a target cc and it drains each scheduled peripheral event whose
    /// fire cc <= target, in min-cc order, until `master_cc == target_cc`.
    ///
    /// In this stage the per-cc resolution primitive (`resolve_one_dot`) is the
    /// proven stage-5 per-dot step, so `run_to` advances one dot at a time and is
    /// byte-identical to the per-dot crank — the structural win is that the CPU
    /// drives off a cc TARGET (and at DS lands on the exact odd/even master_cc),
    /// not off a hand-counted dot loop. The per-instruction `dot`/`ticked`
    /// counters are advanced by the number of dots actually resolved so
    /// `tick_remaining` and the PPU's per-instruction `dot` semantics are
    /// preserved.
    fn run_to(&mut self, target_cc: u64) {
        while self.mmio.master_cc() < target_cc {
            self.resolve_one_dot();
            self.dot = self.dot.wrapping_add(1);
            self.ticked += 1;
        }
    }

    /// Tick one internal (non-memory) M-cycle, for opcodes that need their
    /// internal cycles placed at the right point (e.g. CALL's SP-dec before the
    /// stack pushes) rather than batched at instruction end.
    pub fn internal_cycle(&mut self) {
        self.tick_m();
    }

    pub fn master_cc_dbg(&self) -> u64 { self.mmio.master_cc() }

    /// STAGE 2 (RB_FAITHFUL): the access cc at an instruction boundary — the RAW
    /// master cc captured BEFORE this access M-cycle ticks (Gambatte's `cc` at
    /// which it resolves the access, then `cc += 4`). This is the same raw-cc
    /// anchor stage 1 (RB_EXACTCC) proved for register reads, so the event-cc
    /// dispatch gate compares the boundary access cc against a timer fire cc that
    /// lives in the same space.
    pub fn access_cc(&self) -> u64 {
        self.mmio.master_cc()
    }

    /// STAGE 2: the cc at which the most recent still-undispatched TIMA IRQ fired
    /// (Gambatte's `intevent_interrupts` time for the timer), or `None`.
    pub fn pending_timer_fire_cc(&self) -> Option<u64> {
        self.mmio.pending_timer_fire_cc()
    }

    /// STAGE 2: clear the recorded timer fire cc after the CPU dispatches it.
    pub fn clear_timer_fire_cc(&mut self) {
        self.mmio.clear_timer_fire_cc();
    }

    /// Whether the PPU currently locks CPU access to `addr`: VRAM during Mode 3,
    /// OAM during Mode 2/3, and (CGB) the palette-data ports FF69/FF6B during
    /// Mode 3. Only while the LCD is on. Blocked reads return 0xFF; blocked
    /// writes are dropped.
    fn ppu_locks_access(&self, addr: u16, access_cc: u64) -> bool {
        self.ppu_blocks(addr, true, access_cc)
    }

    /// Whether the PPU locks `addr` from a CPU access of the given direction.
    /// Boundary precision (the exact mode-2->3 and mode-3->0 transition dots)
    /// uses the renderer's cycle-exact predictor (`cpu_access_blocked`), which
    /// mirrors Gambatte's lineCycle thresholds. When no closed-form mode-0 dot
    /// is available (window / first line after enable) it falls back to the
    /// FF41 mode bits.
    fn ppu_blocks(&self, addr: u16, is_read: bool, access_cc: u64) -> bool {
        let is_vram = (0x8000..=0x9FFF).contains(&addr);
        let is_oam = (0xFE00..=0xFE9F).contains(&addr);
        let is_cgb_pal = (addr == 0xFF69 || addr == 0xFF6B) && self.mmio.is_cgb_features_enabled();
        if !(is_vram || is_oam || is_cgb_pal) {
            return false;
        }
        if self.mmio.read(ppu::LCD_CONTROL) & 0x80 == 0 {
            return false;
        }
        let kind: u8 = if is_vram { 0 } else if is_oam { 1 } else { 2 };
        // STAGE 4: derive the fallback mode (used only when no closed-form
        // m0Time anchor exists for this line) from the closed-form getStat at the
        // access cc, not the per-dot renderer's poked FF41 register.
        let mode = if crate::ppu::controller::getstat_enabled() {
            self.ppu
                .get_stat(self.mmio, access_cc)
                .unwrap_or_else(|| self.mmio.read(ppu::LCD_STATUS) & 0x03)
        } else {
            self.mmio.read(ppu::LCD_STATUS) & 0x03
        };
        let mode_locked = if is_oam { mode == 2 || mode == 3 } else { mode == 3 };
        let ds = self.mmio.is_double_speed_mode();
        let is_cgb = self.mmio.is_cgb_features_enabled();
        if let Some(blocked) = self.ppu.cpu_access_blocked(kind, is_read, mode_locked, is_cgb, ds, access_cc) {
            return blocked;
        }
        mode_locked
    }

    pub fn read(&mut self, addr: u16) -> u8 {
        // APU reads (NRxx status, NR52, wave RAM) observe the channels at the
        // read M-cycle START cc (Gambatte resolves the read before advancing).
        // Snapshot the value before ticking; the per-dot step during tick_m would
        // otherwise let a length expiry scheduled within this M-cycle disable a
        // channel 4 dots early — making the cycle-exact `nr52` boundary tests
        // (length expiry at `((cc>>13)+len)<<13` vs the NR52 read cc) read 0 one
        // M-cycle too soon. NR52 status must reflect the pre-tick enabled state.
        let apu_read = if (0xFF10..=0xFF3F).contains(&addr) {
            // Resolve the APU length subsystem at the canonical per-access cc
            // (the SAME cc the timer register access resolves on), so the
            // length-expiry boundary is decided off one uniform clock with no
            // APU-specific phase constant (M7).
            let access_cc = self.mmio.access_cc();
            self.mmio.sync_apu_read_cc(access_cc);
            Some(self.mmio.read(addr))
        } else {
            None
        };
        // Serial registers observe serial state at the read's start cc; snapshot
        // before tick (mirrors the APU read hook).
        let serial_read = if matches!(addr, 0xFF01 | 0xFF02) {
            Some(self.mmio.snapshot_serial_read(addr))
        } else {
            None
        };
        // TIMA/TMA/TAC reads (FF05-07) derive against `abs_cc`; resolve the read
        // at the access START cc (Gambatte `read(addr,cc)`), before the M-cycle
        // ticks `abs_cc` forward 4. Snapshot here so the read anchor equals the
        // scheduled-IRQ delivery anchor (D1/D2). DIV (FF04) stays on the
        // post-tick path with serial/APU (not yet on start-cc).
        let timer_read = if matches!(addr, 0xFF04..=0xFF07) {
            Some(self.mmio.read(addr))
        } else {
            None
        };
        // IF read: the CPU resolves it at cc, but tick_m advances peripherals and
        // would let an IRQ flagged within this read M-cycle leak in 4 dots early.
        // Snapshot the VBlank (0), STAT (1), and serial (3) bits pre-tick so an
        // IRQ raised within this read cycle is observed at the read's start cc
        // (matching Gambatte's read-at-cc); the timer/joypad bits keep the
        // post-tick path, where their flag timing is already tuned to the full
        // M-cycle. Serial completion fires mid-tick_m on the boundary tests, so
        // snapshotting bit 3 makes the read resolve at cc like Gambatte.
        const IF_PRE_MASK: u8 = 0x0B;
        let if_pre = if addr == 0xFF0F {
            Some(self.mmio.snapshot_serial_read(addr) & IF_PRE_MASK)
        } else {
            None
        };
        // FF41 (STAT) read-at-cc: Gambatte resolves the mode bits with
        // `getStat(cc)` at the access START cc, where the mode-3 -> mode-0
        // boundary is `cc + 2 < m0Time`. The per-dot renderer sets the FF41 mode
        // register at a dot boundary that, at double speed, can round the odd-cc
        // sample; resolve it here against the closed-form m0Time so the straddle
        // pairs (m2int_*_m3stat_ds, etc.) sample the same sub-dot boundary
        // Gambatte does. Snapshot pre-tick so the read anchors at access_cc.
        let stat_mode_pre = if addr == ppu::LCD_STATUS {
            // Gambatte resolves FF41 at the raw master cc (== Gambatte `cc`);
            // boundary mode3 iff `master_cc + 2 < m0Time`.
            // C7: the first STAT-mode read after a GDMA/HDMA stall drain starts its
            // M-cycle one dot higher than Gambatte's read cc (the prefetch M-cycle
            // Gambatte absorbs is double-counted by the synchronous-copy + idle-stall
            // model). Resolve it at `master_cc - 1` so the post-DMA mode-3 boundary
            // brackets land on Gambatte's exact sub-dot.
            //
            // Scoped to double speed: at single speed the closed-form `m0_time_master`
            // carries a per-SCX +1 phase error that the `read_off=3` STAT bias already
            // masks at the exact boundary, so the -1 would mis-flag the SS `_2`
            // (mode-0) brackets into mode 3. The DS read uses Gambatte's true `+2`
            // boundary, where the -1 prefetch absorption is exact and regression-free.
            let bias =
                self.mmio.take_dma_prefetch_stat_bias() && self.mmio.is_double_speed_mode();
            let access_cc = self.mmio.master_cc().saturating_sub(if bias { 1 } else { 0 });
            if crate::ppu::controller::getstat_enabled() {
                // STAGE 4: one closed-form getStat off the exact access cc; no
                // reliance on the per-dot renderer FF41 mode register.
                self.ppu.get_stat(self.mmio, access_cc)
            } else {
                self.ppu
                    .get_stat_mode3to0_at_cc(access_cc, self.mmio.is_double_speed_mode())
                    // The mode-3<->0 path only covers in-mode-3 reads; the mode 0/1/2
                    // line-boundary transitions (VBlank entry, line wrap to OAM) are
                    // sampled one M-cycle late by the post-tick register, so resolve
                    // them from the LY phase at the raw read cc too (Gambatte getStat).
                    .or_else(|| self.ppu.get_stat_mode_at_cc(self.mmio, access_cc))
            }
        } else {
            None
        };
        // FF44 (LY) read-at-cc: Gambatte resolves LY with `getLyReg(cc)` at the
        // access cc. In the last few cc of a line the register anticipates the
        // next LY (and reads 0 early on line 153); the per-dot renderer flips the
        // register one dot boundary later, so a read whose M-cycle lands in the
        // anticipation window samples the OLD LY (one M-cycle stale). Resolve it
        // here from the LY-counter phase at the raw read cc (== Gambatte `cc`).
        let ly_reg_pre = if addr == ppu::LY {
            let access_cc = self.mmio.master_cc();
            self.ppu.get_ly_reg_at_cc(self.mmio, access_cc)
        } else {
            None
        };
        // FF41 (STAT) LYC=LY coincidence flag (bit 2) read-at-cc: Gambatte resolves
        // it via getLycCmpLy at the access master cc. The per-dot renderer flips
        // the bit at the dot it changes (e.g. line-153 LY=0 transient at dot 6); a
        // read whose M-cycle straddles that dot reads the post-tick register one
        // M-cycle late. Resolve the flag at access_cc so the lyc0flag/lyc153flag/
        // ly0 boundary probes sample Gambatte's exact sub-dot.
        let stat_lyc_pre = if addr == ppu::LCD_STATUS {
            let access_cc = self.mmio.master_cc();
            self.ppu.get_lyc_flag_at_cc(self.mmio, access_cc)
        } else {
            None
        };
        // Snapshot the access cc at the read's START (Gambatte resolves PPU
        // access gating at `cc` before advancing). The cgbp begin/end boundary
        // is master-cc based and must anchor here, not at the post-tick cc.
        let pre_access_cc = self.mmio.master_cc();
        self.tick_m();
        // VRAM is inaccessible to the CPU during Mode 3, OAM during Mode 2/3;
        // a blocked read returns open-bus 0xFF. Only while the LCD is on.
        if self.ppu_locks_access(addr, pre_access_cc) {
            return 0xFF;
        }
        if let Some(v) = apu_read {
            return v;
        }
        if let Some(v) = serial_read {
            return v;
        }
        if let Some(v) = timer_read {
            return v;
        }
        if let Some(pre) = if_pre {
            return pre | (self.mmio.read(addr) & !IF_PRE_MASK);
        }
        if stat_mode_pre.is_some() || stat_lyc_pre.is_some() {
            let mut v = self.mmio.read(addr);
            if let Some(mode) = stat_mode_pre {
                v = (v & !0x03) | (mode & 0x03);
            }
            if let Some(lyc_flag) = stat_lyc_pre {
                v = (v & !0x04) | ((lyc_flag as u8) << 2);
            }
            return v;
        }
        if let Some(ly) = ly_reg_pre {
            return ly;
        }
        self.mmio.read(addr)
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        // Registers belonging to peripherals we tick inline (timer/serial/DMA)
        // latch at the end of the write M-cycle, so advance first. Everything
        // else (PPU registers, RAM) takes effect as the access is issued.
        //
        // While an OAM DMA transfer is running, the DMA engine advances during
        // this M-cycle *before* the CPU's write is resolved (Gambatte calls
        // `updateOamDma(cc)` at the top of `nontrivial_write`). A write into the
        // DMA's conflict area is then redirected into OAM[oamDmaPos_]. Ticking
        // the M-cycle first reproduces that ordering so `dma_pos` is the value
        // for this cycle when `mmio.write` resolves the conflict.
        // VRAM/OAM/CGB-palette writes are ignored while the PPU owns those
        // resources (see `ppu_locks_access`). Drop the write but still tick.
        // OAM-DMA conflicts are resolved separately in the tick-before path.
        if !self.mmio.dma_active() && self.ppu_blocks(addr, false, self.mmio.master_cc()) {
            self.tick_m();
            return;
        }

        // TIMA/TMA/TAC writes (FF05-07) resolve at the access START cc, then time
        // advances (Gambatte `write(addr,data,cc); cc += 4`). The scheduled-TIMA
        // model derives/IRQ-delivers against `abs_cc`, so the write must land
        // before the M-cycle ticks for its anchor to match the start-cc read
        // anchor (D1). FF04 (DIV) stays on the tick-before path below: its
        // `div_anchor` is shared by serial/APU-FS which are NOT yet on start-cc
        // (the atomic cluster move — serial WRITE_CC_OFFSET drop + APU root 2 —
        // is the follow-up step), so moving DIV alone would mix anchors.
        if matches!(addr, 0xFF04..=0xFF07) {
            self.mmio.write(addr, value);
            self.tick_m();
            return;
        }

        // SC (FF02) ABORT write resolves at the access START cc (M8 serial
        // merge): clearing the transfer-start/internal-clock bits must land
        // BEFORE this M-cycle's per-dot `step_serial` runs — otherwise the
        // in-flight transfer completes during the tick and raises the serial IF
        // the abort is meant to suppress. A START write (bits set) keeps the
        // tick-before path so its scheduled completion cc is unchanged (the
        // nopx/late_div_write completion-timing cases are tuned to it).
        let sc_abort = addr == 0xFF02 && (value & 0x81) != 0x81;
        if sc_abort {
            self.mmio.write(addr, value);
            self.tick_m();
            return;
        }

        let tick_before = matches!(addr, 0xFF01..=0xFF02 | 0xFF46 | 0xFF4A | 0xFF4B)
            || self.mmio.dma_active();
        if tick_before {
            // FF4A (WY): schedule Gambatte's `wy2` at the write's cc (read-at-
            // cc-start phase, like the STAT path) before the M-cycle ticks.
            if addr == ppu::WY {
                self.ppu.set_write_subdot(self.mmio.cpu_t_phase());
                self.ppu.on_wy_write(value, self.mmio);
            }
            self.tick_m();
            self.mmio.write(addr, value);
            // C7-full: an FF55 bit7=1 kick written while OAM-DMA is active routes
            // through this tick-before path; resolve its live-period gate here too
            // so the flag never leaks (see the else branch for the rationale).
            if self.mmio.hdma_kick_eval_pending() {
                let ds = self.mmio.is_double_speed_mode();
                let in_period = self
                    .ppu
                    .hdma_period(ds)
                    .unwrap_or_else(|| self.mmio.hdma_is_in_period_cached());
                self.mmio.resolve_hdma_kick(in_period);
            }
        } else {
            // The write resolves at the current persistent T-phase, before this
            // M-cycle's dots tick. Pass that phase's sub-dot parity so the PPU
            // STAT/LYC hooks place the event on the correct half-dot at DS.
            self.ppu.set_write_subdot(self.mmio.cpu_t_phase());
            self.mmio.write(addr, value);
            // FF42/FF43 (SCY/SCX): the CPU readback above is immediate, but the
            // BG fetcher must see the new value ~N dots later (write-side analog
            // of the wy1/wy2 latches). The mmio write phase is unchanged (kept in
            // this else branch, pre-tick) so steady-state rendering is identical;
            // only the fetcher's mid-M3 view is delayed via on_sc{y,x}_write.
            if addr == ppu::SCY {
                self.ppu.on_scy_write(value, self.mmio);
            }
            if addr == ppu::SCX {
                self.ppu.on_scx_write(value, self.mmio);
            }
            if addr == ppu::LCD_CONTROL {
                self.ppu.handle_lcdc_write(value, self.mmio);
            }
            if self.mmio.take_stat_register_write_pending() {
                self.ppu.on_stat_register_write(self.mmio);
            }
            // C7-full: resolve a pending FF55 bit7=1 kick against the LIVE HDMA
            // period predicate (Gambatte enableHdma -> isHdmaPeriod(cc+4)) rather
            // than the 1-dot-lagged renderer cache. Evaluated here at the write's
            // access cc, before the M-cycle ticks. When `hdma_period` cannot supply
            // a closed-form mode-0 dot (window / first line after enable) fall back
            // to the cached period so those paths still kick.
            if self.mmio.hdma_kick_eval_pending() {
                let ds = self.mmio.is_double_speed_mode();
                let in_period = self
                    .ppu
                    .hdma_period(ds)
                    .unwrap_or_else(|| self.mmio.hdma_is_in_period_cached());
                self.mmio.resolve_hdma_kick(in_period);
            }
            self.tick_m();
        }
    }
}

impl<'a> Deref for Bus<'a> {
    type Target = Mmio;
    fn deref(&self) -> &Mmio {
        self.mmio
    }
}

impl<'a> DerefMut for Bus<'a> {
    fn deref_mut(&mut self) -> &mut Mmio {
        self.mmio
    }
}

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

/// ds-engine STAGE 6/7: the run-to-next-event scheduler is the single CPU world-
/// advance path. `tick_m` advances `master_cc` by the access duration (one
/// M-cycle = 4 dots) and a single `run_to(target_cc)` resolves every peripheral
/// up to that cc — the CPU no longer cranks peripherals dot-by-dot. At double
/// speed a CPU access lands on an exact odd/even `master_cc` and `run_to`
/// resolves every peripheral to THAT cc. (`run_to`'s per-cc primitive still
/// resolves one dot at a time internally — the true min-event jump and the
/// step_subdot/parity-gate removal are deferred until the mid-M3 per-column
/// render is finished.)
///
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
        // STAGE 6/7: resolve the leftover internal dots via the run-to-cc driver,
        // so the whole instruction advances off cc targets (per-dot fallback gone).
        let target = self.mmio.master_cc().wrapping_add(remaining as u64);
        self.run_to(target);
    }

    fn tick_m(&mut self) {
        // STAGE 6/7: the CPU advances `master_cc` by the access duration (one
        // M-cycle = 4 dots) and a single `run_to` resolves every peripheral up to
        // that cc. The CPU no longer hand-cranks each dot; it requests "advance
        // the world to target_cc" (per-dot fallback gone).
        let target = self.mmio.master_cc().wrapping_add(4);
        self.run_to(target);
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
        self.run_to_min_event(target_cc);
    }

    /// per-access STAGE 1: the true min-event-jump driver.
    /// Instead of cranking `resolve_one_dot` once per dot to `target_cc`, advance
    /// `master_cc` directly to `min(target_cc, next_event_cc)` — the next cc at
    /// which any peripheral does something observable — firing exactly that span,
    /// then repeat until `master_cc == target_cc`.
    ///
    /// Stage 1 is a PURE refactor of the advance MECHANISM: it must be
    /// byte-identical to the per-dot crank. The renderer / PPU / OAM-DMA / HDMA /
    /// powered-APU are intrinsically per-dot stateful machines (mode edges,
    /// duty/freq counters, period-edge detection, sub-cycle catch-up), so while ANY
    /// of them is live the loop still resolves dot-by-dot (`resolve_one_dot`) — the
    /// proven byte-identical primitive. The genuine jump win is over IDLE spans
    /// (`Mmio::idle_bulk_skippable`: LCD off, no DMA/HDMA, APU off, serial idle):
    /// there only the timer and serial advance, and both are span-collapsible
    /// (closed-form over the span), so the whole idle span is jumped in one
    /// `bulk_advance_idle` to the next scheduled event cc (timer overflow / next
    /// non-idle boundary), reproducing every fire at its exact cc. The next-event
    /// cc for the idle skip is the next timer-overflow delivery cc (the only event
    /// that can fire while idle); everything else is reached by clamping to
    /// `target_cc`, where the CPU's own access boundary re-evaluates the world.
    fn run_to_min_event(&mut self, target_cc: u64) {
        while self.mmio.master_cc() < target_cc {
            if self.mmio.idle_bulk_skippable() {
                // Jump straight to the next event the timer can raise (an overflow
                // IRQ delivery) or, if none, to the target. The timer overflow is
                // delivered at its absolute fire cc inside `bulk_advance_idle`, so
                // landing exactly there fires it at the identical cc the per-dot
                // path would have. Clamp to target so the CPU access boundary
                // re-checks the world (e.g. a freshly written FF40 turning the LCD
                // on) before we skip past it.
                let next_event = self
                    .mmio
                    .next_timer_overflow_fire_cc()
                    .filter(|&cc| cc > self.mmio.master_cc())
                    .map(|cc| cc.min(target_cc))
                    .unwrap_or(target_cc);
                let span = next_event.wrapping_sub(self.mmio.master_cc());
                self.mmio.bulk_advance_idle(next_event);
                self.dot = self.dot.wrapping_add(span as u32);
                self.ticked += span as u32;
            } else {
                self.resolve_one_dot();
                self.dot = self.dot.wrapping_add(1);
                self.ticked += 1;
            }
        }
    }

    /// Tick one internal (non-memory) M-cycle, for opcodes that need their
    /// internal cycles placed at the right point (e.g. CALL's SP-dec before the
    /// stack pushes) rather than batched at instruction end.
    pub fn internal_cycle(&mut self) {
        self.tick_m();
    }

    pub fn master_cc_dbg(&self) -> u64 { self.mmio.master_cc() }
    pub fn oam_dma_active(&self) -> bool { self.mmio.dma_active() }

    /// Non-ticking instruction-stream peek. Mirrors Gambatte's HALT-bug prefetch
    /// `mem_.read(pc, cc())` (cpu.cpp case 0x76): the byte after HALT is read at
    /// the current cc WITHOUT advancing it; the +4 charge is deferred until the
    /// prefetched opcode is consumed on the next step. Instruction memory is never
    /// PPU-gated, so a direct mmio read is the faithful peek.
    pub fn peek(&self, addr: u16) -> u8 {
        self.mmio.read(addr)
    }

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

    /// Delivery cc of the next scheduled timer overflow (EI-loop fast-dispatch).
    pub fn next_timer_overflow_cc(&self) -> Option<u64> {
        self.mmio.next_timer_overflow_cc()
    }

    /// EARLY (EI-loop) gate cc of the undispatched timer IRQ.
    pub fn pending_timer_fire_cc_ei(&self) -> Option<u64> {
        self.mmio.pending_timer_fire_cc_ei()
    }

    /// EI-loop fast timer delivery (non-halt/non-stop): fire an imminent overflow
    /// at the early anchor and raise its IF bit.
    pub fn force_ei_timer_delivery(&mut self, boundary: u64) {
        self.mmio.force_ei_timer_delivery(boundary);
    }

    /// EARLY (EI-loop) anchor cc of the next scheduled timer overflow.
    pub fn next_timer_overflow_ei_cc(&self) -> Option<u64> {
        self.mmio.next_timer_overflow_ei_cc()
    }

    /// COORDINATED piece #3 (HDMA-halt deferred held-flag): Gambatte's unhalt
    /// re-flag gate (`memory.cpp:224/304`) keys on `isHdmaPeriod` evaluated at the
    /// unhalt cc, NOT a STAT-mode==0 snapshot. The greedy `hdma_in_period_for_unhalt`
    /// (cached period || STAT mode 0) over-fires the m0-edge block at unhalt
    /// (`hdma_late_m0unhalt_*`: FF55 reads 0xFF where Gambatte reads 0x00 because
    /// the block had not yet fired). Resolve the period off the renderer's
    /// cycle-exact `isHdmaPeriod(cc)` predicate at the unhalt access cc instead, so
    /// a Low-at-halt block that is NOT yet in period at unhalt is left to fire on
    /// its natural mode-0 edge after the FF55 read. Fall back to the cached/STAT
    /// gate only when no closed-form mode-0 anchor exists (window / first line).
    pub fn hdma_in_period_for_unhalt(&self) -> bool {
        self.hdma_in_period_for_unhalt_adj(0)
    }

    /// As `hdma_in_period_for_unhalt`, but widens the unhalt-period line-END
    /// bracket by `limit_adj` dots. Used by the EI fast-dispatch path: when the
    /// timer IRQ is delivered at the EARLY anchor (`schedCc + IF_OFF`) instead of
    /// the LATE anchor (`schedCc + CC_OFF`) the timer ISR (which re-enables the
    /// LCD via the FF40 write) runs `CC_OFF - IF_OFF` = 4 cc earlier, so the
    /// closed-form `m0_time_master` for the unhalt line lands 4 cc earlier than
    /// the OFF baseline against which `hdma_period_unhalt`'s limit was calibrated.
    /// The unhalt access cc (driven by the absolute timer-overflow
    /// `intevent_unhalt` schedule) is unchanged, so the period DEPTH (`cc - m0t`)
    /// inflates by 4: a Low-at-halt block near the line END drops its reflag
    /// (`hdma_late_m0unhalt_2`: depth 196->200 across the 198 limit). Widening the
    /// END bracket by +4 on the fast path restores the in-period reflag for that
    /// block WITHOUT disturbing the mode-0 ENTRY (start) bracket — so a block at
    /// depth ~0 (`hdma_ei_m3halt_m0unhalt_ly_*`, Gambatte reflag=1) still reflags.
    /// The non-fast (HALT-late) path passes 0 and is byte-identical.
    pub fn hdma_in_period_for_unhalt_adj(&self, limit_adj: i64) -> bool {
        let lcd_on = self.mmio.read(ppu::LCD_CONTROL) & (ppu::LCDCFlags::DisplayEnable as u8) != 0;
        if !lcd_on {
            return true;
        }
        let ds = self.mmio.is_double_speed_mode();
        let cc = self.mmio.master_cc();
        if let Some(p) = self.ppu.hdma_period_unhalt_adj(cc, ds, limit_adj) {
            return p;
        }
        self.mmio.hdma_in_period_for_unhalt()
    }

    /// Late-hdma-vs-interrupt unhalt precedence: whether a Low-at-halt HDMA block
    /// would fire AT unhalt (before the next interrupt's PC pushes) per Gambatte's
    /// `isHdmaPeriod(cc)` reflag gate at the unhalt cc. When this is false but a
    /// block still fires (its m0-edge falls within the service window) the block
    /// must be deferred past the pushes (the `*_halt_2` content tests). Defaults to
    /// firing before pushes (the synchronous baseline) when no closed-form anchor
    /// exists.
    pub fn hdma_unhalt_fires_before_pushes(&self) -> bool {
        let lcd_on = self.mmio.read(ppu::LCD_CONTROL) & (ppu::LCDCFlags::DisplayEnable as u8) != 0;
        if !lcd_on {
            return true;
        }
        let ds = self.mmio.is_double_speed_mode();
        let cc = self.mmio.master_cc();
        self.ppu
            .hdma_unhalt_fires_before_pushes(cc, ds)
            .unwrap_or(true)
    }

    /// CPU has just entered HALT. Computes Gambatte's `haltHdmaState_` using the
    /// SAME cycle-exact `isHdmaPeriod(cc)` predicate the unhalt re-flag path uses
    /// (`hdma_period_unhalt` anchored on `m0_time_master`), instead of the coarse
    /// per-PPU-step `hdma_is_in_period_cached` (STAT-mode snapshot). This makes the
    /// `haltHdmaState_` latch at HALT entry straddle the line-end `cc + 3 + 3*ds <
    /// lineEnd` boundary precisely: the `hdma_late_m0halt_1` (in-period -> High ->
    /// 1 block) vs `_2` (past-boundary -> Low -> reflag -> 2 blocks) pair differ by
    /// 4cc at the HALT cc and must resolve together. Falls back to the cached/STAT
    /// gate when no closed-form mode-0 anchor exists (window / first line).
    pub fn on_cpu_halt(&mut self) {
        let lcd_on = self.mmio.read(ppu::LCD_CONTROL) & (ppu::LCDCFlags::DisplayEnable as u8) != 0;
        let ds = self.mmio.is_double_speed_mode();
        let cc = self.mmio.master_cc();
        let in_period = if !lcd_on {
            Some(true)
        } else {
            self.ppu.hdma_period_halt(cc, ds)
        };
        // Robust "current period's block already serviced" signal for the
        // HALT-entry High-vs-Requested capture. The live `hdma_block_done_this_period`
        // flag is cleared by the per-dot `hdma_period` falling edge, whose line-END
        // dot sits a hair earlier than `hdma_period_halt`'s end bracket — a HALT in
        // that sliver sees the flag already reset and wrongly captures `Requested`.
        // Derive it instead from the last block-fire cc landing within THIS line's
        // mode-0 period window `[m0t, m0t + lineLen)` (master cc; lineLen scales with
        // double speed). Only supplied when a closed-form m0 anchor exists.
        let block_done_override = match (lcd_on, self.ppu.m0_time_master_cc()) {
            (true, Some(m0t)) => self.mmio.hdma_last_fire_cc().map(|fc| {
                let line_len: u64 = 456u64 << (ds as u64);
                fc >= m0t && fc < m0t + line_len
            }),
            _ => None,
        };
        self.mmio
            .on_cpu_halt_with_period_done(in_period, block_done_override);
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
        // STAGE 4 KEYSTONE: this is a RENDER-visibility gate, so view the CPU
        // access in the carried anchor frame (un-carried fetcher geometry) by
        // adding the accumulated STAT-phase carry skew. The FACET-1 STOP carry
        // advances the STAT/line phase (so getStat / lyTime-anchored boundaries
        // shift) but the fetcher's mode-3 lock window did NOT move; the skew
        // re-aligns the access. 0 (no shift) when no carry is live / flag-OFF.
        let gate_cc = (access_cc as i64 - self.ppu.render_carry_skew()).max(0) as u64;
        // Derive the fallback mode (used only when no closed-form m0Time anchor
        // exists for this line) from the closed-form getStat at the gate cc, not
        // the per-dot renderer's poked FF41 register.
        let mode = self.ppu
                .get_stat(self.mmio, gate_cc)
                .unwrap_or_else(|| self.mmio.read(ppu::LCD_STATUS) & 0x03);
        let mode_locked = if is_oam { mode == 2 || mode == 3 } else { mode == 3 };
        let ds = self.mmio.is_double_speed_mode();
        let is_cgb = self.mmio.is_cgb_features_enabled();
        if let Some(blocked) = self.ppu.cpu_access_blocked(kind, is_read, mode_locked, is_cgb, ds, gate_cc) {
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
        // FAST EI-loop: inside an early-grid ISR the timer overflow raises IF on the
        // early anchor (see timer.rs); sample the timer bit (0x04) at the read's
        // access START cc too — pre-tick — so a read-only early-grid ISR misses an
        // overflow whose early IF-set has NOT been reached at the read cc, matching
        // Gambatte's read-at-cc (tc00_irq_ds_1: read just before the early IF-set =>
        // E0). OFF / late-grid reads keep the timer bit on the post-tick path where
        // its full-M-cycle flag timing is already tuned.
        let if_pre_mask = if self.mmio.timer_isr_on_early_grid() {
            IF_PRE_MASK | 0x04
        } else {
            IF_PRE_MASK
        };
        let if_pre = if addr == 0xFF0F {
            Some(self.mmio.snapshot_serial_read(addr) & if_pre_mask)
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
            // Post-DMA STAT-read prefetch absorption. At double speed the -1
            // applies unconditionally (the read M-cycle starts one dot high after
            // the synchronous-copy + idle-stall drain). At single speed the closed-
            // form m0_time_master carries a per-SCX +1 phase error that read_off=3
            // normally masks; the -1 would then double-correct and flip the SCX>0
            // mode-0 `_2` brackets into mode 3 (gdma_cycles_scx3_2). At SCX&7==0
            // there is no such phase error, so the post-DMA read needs the same -1
            // the DS path uses to land the mode-3 `_1` bracket (hdma_cycles_1).
            let post_dma = self.mmio.take_dma_prefetch_stat_bias();
            let ds = self.mmio.is_double_speed_mode();
            let scx0 = (self.mmio.read(ppu::SCX) & 0x07) == 0;
            let bias_cc: u64 = if post_dma && (ds || scx0) {
                // At DS with SCX&7 != 0 the m0_time_master keeps a 1cc-low per-SCX
                // phase the read_off=2 boundary does not mask, so the post-DMA read
                // ties m0Time exactly (gdma_cycles_2xshort_scx5_ds_1 reads mode 0
                // where Gambatte reads mode 3 at cc+2<m0Time); a second dot of
                // prefetch absorption lands it. SCX&7==0 (DS or SS) keeps the plain
                // -1.
                if ds && !scx0 { 2 } else { 1 }
            } else {
                0
            };
            let access_cc = self.mmio.master_cc().saturating_sub(bias_cc);
            self.ppu.get_stat(self.mmio, access_cc)
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
        // HALT-wakeup VRAM-read phase: a VRAM read on the instruction stream
        // resumed by a HALT wakeup (`halt_wakeup_skew`) resolves its mode-3->0
        // boundary 6cc late vs the engine's `master_cc`. Root: the LYC/STAT IRQ
        // that woke the CPU is flagged into IF one dot early (sched_lycirq <= cc+ds)
        // and serviced in the same step, so the HALT-woken stream's master_cc runs
        // ~6cc ahead of the PPU's m0Time anchor (measured byte-exact via cctracer:
        // engine m0_time_master = gb_m0Time + 6 relative to the woken read, vs +0
        // on the non-halt m0-IRQ-dispatch stream — hdma_start/hdma_late_disable
        // scx3/scx5 `_1` read mode-3 0xFF where Gambatte reads mode-0). The
        // non-halt postread/vramw reads on the same lines are correctly phased, so
        // bias ONLY the halt-woken VRAM read's access cc, not the shared m0Time.
        let vram_read_cc = if (0x8000..=0x9FFF).contains(&addr)
            && self.mmio.halt_wakeup_skew()
            && self.mmio.read(ppu::LCD_CONTROL) & 0x80 != 0
        {
            pre_access_cc + 6
        } else {
            pre_access_cc
        };
        self.tick_m();
        // VRAM is inaccessible to the CPU during Mode 3, OAM during Mode 2/3;
        // a blocked read returns open-bus 0xFF. Only while the LCD is on.
        if self.ppu_locks_access(addr, vram_read_cc) {
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
            return pre | (self.mmio.read(addr) & !if_pre_mask);
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

    /// Interrupt-service low-byte push that ACKs the dispatched IF bit partway
    /// through its M-cycle, faithful to Gambatte's `Interrupter::interrupt`
    /// ordering (`memory.write(low push); memory.ackIrq(n, cc)`). `Memory::ackIrq`
    /// advances each source to a per-source sub-cc offset before clearing only the
    /// dispatched bit `n`:
    ///   `updateSerial(cc + 3 + isCgb())`  (bit 8)
    ///   `updateTimaIrq(cc + 2 + isCgb())` (bit 4)
    ///   `lcd_.update(cc + 2)`             (bit 2)
    /// A source whose completion cc lands at or before its offset is flagged then
    /// immediately cleared (reads back gone); one completing later in the M-cycle
    /// re-flags IF and survives for the ISR to read (the `late_retrigger` /
    /// `start_wait..._read_if` re-fire). The +cgb on serial/timer is the DMG-vs-CGB
    /// discriminator for the `_2` boundary cases. `bit` is the dispatched vector's
    /// IF bit. The stack write targets RAM (SP) and is never PPU-gated (Gambatte
    /// `write<false,false>`).
    pub fn interrupt_low_push_ack(&mut self, sp: u16, value: u8, bit: u8) {
        // Stack byte stores at the access start (the push data is fixed for the
        // whole M-cycle); RAM is never PPU-gated.
        self.mmio.write(sp, value);
        let ds = self.mmio.is_double_speed_mode();
        let cgb = self.mmio.is_cgb() as u64;
        let start = self.mmio.master_cc();
        let target = start.wrapping_add(4);
        // Per-source ack offset (Gambatte Memory::ackIrq), in master cc from the
        // low-push start. Serial/timer carry the +cgb the DMG/CGB `_2` boundaries
        // hinge on; LCD is a flat +2.
        let lcd_bit = crate::cpu::registers::InterruptFlag::Lcd as u8;
        let serial_bit = crate::cpu::registers::InterruptFlag::Serial as u8;
        let timer_bit = crate::cpu::registers::InterruptFlag::Timer as u8;
        // The clear-point trigger. For the LCD vector keep the cctracer-tuned
        // dual rule (DS uses the raw master cc+2; SS uses the PPU abs-cc edge,
        // where STAT events fire at the dot clock). Serial/timer events fire off
        // the master cc (timer.abs_cc / serial phase), so they use Gambatte's exact
        // master `cc + offset` at both speeds.
        // Gambatte `Memory::ackIrq` flags the source up to `cc + N + isCgb()` then
        // clears bit n: serial N=3, timer N=2, lcd N=2 (flat). rustyboi's per-dot
        // crossing compares the source's *fire dot* (abs_cc), not Gambatte's
        // unfolded eventTime; the serial fire dot equals its complete_at (offset
        // 3+cgb maps 1:1), while the timer fire dot is its eventTime + IF_OFF(1),
        // so the equivalent crossing is also `start + 3 + cgb` (= eventTime+1 <=
        // start+2+cgb+1). Both peripheral vectors therefore share the 3+cgb dot
        // threshold; LCD keeps the flat +2.
        let offset = if bit == serial_bit || bit == timer_bit {
            3 + cgb
        } else {
            2
        };
        let ack_abs_threshold = self.ppu.abs_cc().wrapping_add(2);
        let ack_master_threshold = start.wrapping_add(offset);
        let mut acked = false;
        while self.mmio.master_cc() < target {
            self.resolve_one_dot();
            self.dot = self.dot.wrapping_add(1);
            self.ticked += 1;
            let crossed = if bit == lcd_bit && !ds {
                self.ppu.abs_cc() > ack_abs_threshold
            } else {
                self.mmio.master_cc() >= ack_master_threshold
            };
            if !acked && crossed {
                let cur = self.mmio.read(crate::cpu::registers::INTERRUPT_FLAG);
                self.mmio
                    .write(crate::cpu::registers::INTERRUPT_FLAG, cur & !bit);
                acked = true;
            }
        }
        if !acked {
            // Threshold not crossed within this M-cycle: ACK now so the bit is
            // still cleared (the source never re-fired in the trailing dots).
            let cur = self.mmio.read(crate::cpu::registers::INTERRUPT_FLAG);
            self.mmio
                .write(crate::cpu::registers::INTERRUPT_FLAG, cur & !bit);
        }
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

        // SC (FF02) writes resolve at the access START cc (Gambatte
        // `interrupt_request 0x02: updateSerial(cc); ... setEventTime(... cc ...)`
        // — the SC write's `cc` is the access cc, NOT cc+4). An ABORT (bits
        // cleared) must land BEFORE this M-cycle's per-dot `step_serial` so the
        // in-flight transfer cannot complete and raise the serial IF the abort
        // suppresses. A START write must ALSO resolve at the access cc: the
        // completion event is `cc - (cc - divLastUpdate) % align + step*8`, and
        // capturing `cc` (and thus the DIV residue) at the post-tick cc+4 placed
        // both the residue and the completion 4 cc late vs Gambatte (the serial-IF
        // boundary `start83*`/`nopx1* read_if` cases read the IF bit at the exact
        // completion cc). Writing before the tick anchors the SC write at the same
        // start cc the DIV/TIMA writes already use, matching Gambatte's `cc`.
        if addr == 0xFF02 {
            self.mmio.write(addr, value);
            self.tick_m();
            return;
        }

        if addr == 0xFF0F && !self.mmio.dma_active() {
            // IF (0xFF0F) write: split the write M-cycle so the explicit `ifReg`
            // store lands partway through it (Gambatte applies the store at the
            // write cc, after the access M-cycle's leading dots). An IRQ flagged at
            // a cc <= store_cc is already in IF and is overwritten by the write; one
            // flagged later survives. The m0 STAT IRQ at m0Time-1 falls one dot into
            // the IF-clear write's M-cycle, so storing one dot in clears it
            // (m2int_m0irq_scx{3,4}_ifw -> out0/out2) while a later m2/lyc IRQ (next
            // dot, or the read M-cycle at +2 dots) survives. One dot = `1 << ds`
            // master-cc, so the store lands at the same sub-dot at both speeds
            // (cctracer-measured: m0 cleared at write-mcycle +1 dot, kept at +2).
            let ds = self.mmio.is_double_speed_mode();
            let split: u64 = 1u64 << ds as u32;
            let mid = self.mmio.master_cc().wrapping_add(split);
            self.run_to(mid);
            self.mmio.write(addr, value);
            let end = self.mmio.master_cc().wrapping_add(4 - split);
            self.run_to(end);
            return;
        }

        let tick_before = matches!(addr, 0xFF01 | 0xFF46 | 0xFF4A | 0xFF4B)
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
                let cc = self.mmio.master_cc();
                let in_period = self
                    .ppu
                    .hdma_period_kick(cc, ds)
                    .unwrap_or_else(|| self.mmio.hdma_is_in_period_cached());
                self.mmio.resolve_hdma_kick(in_period);
            }
        } else {
            // The write resolves at the current persistent T-phase, before this
            // M-cycle's dots tick. Pass that phase's sub-dot parity so the PPU
            // STAT/LYC hooks place the event on the correct half-dot at DS.
            self.ppu.set_write_subdot(self.mmio.cpu_t_phase());
            // FF55 disable-vs-m0-edge race (Gambatte `disableHdma`): an FF55
            // bit7=0 write to an enabled HDMA only kills the FUTURE m0-edge
            // schedule. A block whose m0 edge has already fired at/before this
            // write's access cc is already latched and still runs. Resolve that at
            // the write's access cc (raw master_cc, the same anchor getStat uses)
            // and stash it so the write handler keeps the request instead of
            // canceling. Evaluated BEFORE the write so the handler sees it.
            if addr == 0xFF55 && (value & 0x80) == 0 && self.mmio.hdma_is_enabled() {
                let ds = self.mmio.is_double_speed_mode();
                let cc = self.mmio.master_cc();
                self.mmio
                    .set_hdma_disable_fires(self.ppu.hdma_disable_fires(cc, ds));
            }
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
                let cc = self.mmio.master_cc();
                // DEFERRED-HDMA-FIRE: use the late-HBlank-aware kick predicate so an
                // FF55 enable written mid-HBlank (after mode-0 entry, same line)
                // still arms its block (Gambatte `isHdmaPeriod` via `lastM0Time`).
                let in_period = self
                    .ppu
                    .hdma_period_kick(cc, ds)
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

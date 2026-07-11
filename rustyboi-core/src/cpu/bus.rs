use crate::memory::Addressable;
use crate::memory::mmio::Mmio;
use crate::ppu::{self, Ppu};
use std::ops::{Deref, DerefMut};

/// DMG OAM-corruption-bug access classification. Each variant selects the OAM
/// mutation applied to the row the PPU is scanning in mode 2: a plain OAM-bus
/// write or a plain OAM-bus read.
/// Pan Docs: OAM Corruption Bug — https://gbdev.io/pandocs/OAM_Corruption_Bug.html
#[derive(Clone, Copy)]
pub enum OamBugKind {
    Write,
    Read,
}

/// A tick-aware view over the system. CPU memory accesses go through `read`/
/// `write`, which advance every peripheral one M-cycle (4 dots) so each access
/// observes/mutates live state at its true intra-instruction cycle. Everything
/// else on `Mmio` is reached transparently via `Deref`. `tick_m` advances
/// `master_cc` by the access duration and a single `run_to(target_cc)` resolves
/// every peripheral up to that cc; at double speed a CPU access lands on an
/// exact odd/even `master_cc`.
pub struct Bus<'a> {
    pub mmio: &'a mut Mmio,
    pub ppu: &'a mut Ppu,
    // Dots elapsed since this instruction started; drives the double-speed PPU
    // gate. Resets per instruction.
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

    /// Advance every peripheral by exactly one dot (one `master_cc`). This is the
    /// per-cc resolution primitive shared by the per-dot crank and the event-loop
    /// driver (`run_to`); it steps each peripheral in a fixed order so both paths
    /// resolve identically. The per-instruction `dot`/`ticked` counters are NOT
    /// touched here — callers own that bookkeeping.
    fn resolve_one_dot(&mut self) {
        self.mmio.step_timer();
        self.mmio.step_serial();
        self.mmio.step_joypad_irq_delay();
        self.mmio.step_dma();

        let double_speed = self.mmio.is_double_speed_mode();
        // Gate the PPU/audio step on the *persistent* T-phase parity so the
        // PPU's even-dot stepping stays aligned with the true accumulated cc
        // across instruction boundaries (per-instruction `dot` would re-anchor
        // the phase to the instruction start every M-cycle).
        if !double_speed || self.mmio.cpu_t_phase().is_multiple_of(2) {
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
        // `hdma_period` predicate; fall back to the STAT mode-edge when no
        // closed-form mode-0 dot is available.
        let period = self.ppu.hdma_period(double_speed);
        self.mmio.step_hdma(period);
        // Drain any deferred HDMA block writes whose sub-M-cycle delay has elapsed
        // (byte i commits at fire + (2 + 2*ds)). Runs after step_hdma so a block
        // flagged this same dot begins its countdown here and only commits to VRAM
        // on a later dot.
        self.mmio.step_hdma_deferred();
        self.ppu.step_lcdc_events(self.mmio);

        // Publish the BG fetcher's current VRAM data-bus address for the next
        // dot's OAM-DMA-source conflict resolution. `step_dma` runs at the START
        // of the following dot (before that dot's `ppu.step`), so it reads the
        // address as latched HERE — one dot earlier — which is the phase the real
        // bus conflict observes.
        self.ppu.update_dma_fetcher_bus(self.mmio);

        // Advance the MBC3 RTC one T-cycle. The RTC crystal runs off the same
        // 4.194304 MHz master (dot) clock this loop cranks, so one dot == one
        // RTC T-cycle. No-op for carts without an RTC.
        self.mmio.tick_rtc(1);

        self.mmio.advance_cpu_t_phase();
    }

    /// Tick the remaining internal (non-memory) cycles of an instruction.
    pub fn tick_remaining(&mut self, total_cycles: u32) {
        let remaining = total_cycles.saturating_sub(self.ticked);
        // Resolve the leftover internal dots via the run-to-cc driver.
        let target = self.mmio.master_cc().wrapping_add(remaining as u64);
        self.run_to(target);
    }

    /// Charge one opcode-fetch M-cycle (4 T-cycles) for a HALT-bug-prefetched
    /// opcode consumed WITHOUT a re-fetch. The advance is absorbed into the
    /// instruction's returned cycle count by the `tick_remaining` reconciliation
    /// at the end of `step`, so it only shifts WHEN the doubled instruction's
    /// operand read resolves — not the instruction length.
    pub fn tick_opcode_fetch_mcycle(&mut self) {
        self.tick_m();
    }

    fn tick_m(&mut self) {
        // Advance `master_cc` by one M-cycle (4 dots); a single `run_to` resolves
        // every peripheral up to that cc.
        let target = self.mmio.master_cc().wrapping_add(4);
        self.run_to(target);
    }

    /// Advance the world to `target_cc`, resolving every peripheral up to (and
    /// including) that cc. The per-instruction `dot`/`ticked` counters are
    /// advanced by the number of dots actually resolved so `tick_remaining` and
    /// the PPU's per-instruction `dot` semantics are preserved.
    fn run_to(&mut self, target_cc: u64) {
        self.run_to_min_event(target_cc);
    }

    /// Min-event-jump driver: advance `master_cc` toward `target_cc`, jumping over
    /// idle spans in one step and resolving dot-by-dot otherwise.
    ///
    /// The renderer / PPU / OAM-DMA / HDMA / powered-APU are per-dot stateful
    /// machines (mode edges, duty/freq counters, period-edge detection), so while
    /// any of them is live the loop resolves dot-by-dot (`resolve_one_dot`). Over
    /// IDLE spans (`Mmio::idle_bulk_skippable`: LCD off, no DMA/HDMA, APU off,
    /// serial idle) only the timer and serial advance, and both are closed-form
    /// over the span, so the whole idle span jumps in one `bulk_advance_idle` to
    /// the next scheduled event cc (the next timer-overflow delivery — the only
    /// event that can fire while idle) or, failing that, clamped to `target_cc`
    /// where the CPU's own access boundary re-evaluates the world.
    fn run_to_min_event(&mut self, target_cc: u64) {
        while self.mmio.master_cc() < target_cc {
            // The PPU's LCD-off transition (set `disabled`, reset the pipeline,
            // LY=0) is applied lazily inside `ppu.step()` on the first dot after
            // the enable bit clears. `idle_bulk_skippable` keys on the LCDC
            // register bit, so a disable followed immediately by an idle span
            // would jump over that first dot and leave the PPU stuck in its
            // pre-disable running state (stale LY, never re-initializing on the
            // next enable). Run one real dot first so the disable is processed at
            // the exact dot the per-dot path would have, then allow the skip.
            if !self.ppu.is_lcd_disabled() && !self.mmio.lcd_display_enabled() {
                self.resolve_one_dot();
                self.dot = self.dot.wrapping_add(1);
                self.ticked += 1;
                continue;
            }
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
                let stall_before = self.mmio.peek_dma_stall();
                self.resolve_one_dot();
                self.dot = self.dot.wrapping_add(1);
                self.ticked += 1;
                // Event-interleaved HDMA transfer. A block that just fired in
                // `step_hdma` queued its transfer cc as `pending_dma_stall` (the
                // CPU pays it at a LATER step, so the PPU otherwise catches up only
                // then and the resume read sees the un-advanced, mode-3-locked
                // line). Hardware advances all peripherals through the transfer cc
                // in lockstep at fire time. Tick the world through the just-queued
                // transfer NOW (consuming the stall) so a same-instruction resume
                // read after the block observes the extended line. Scoped to
                // `hdma_resume_lockstep_window` — armed only at a Requested-context
                // (multi-block) IME-off HALT-bug unhalt, so normal m0-edge blocks
                // keep the deferred-stall path.
                if self.mmio.hdma_resume_lockstep_window() {
                    let stall_after = self.mmio.peek_dma_stall();
                    let delta = stall_after.saturating_sub(stall_before);
                    if delta > 0 {
                        self.mmio.reduce_dma_stall(delta);
                        self.mmio.set_hdma_lockstep_active(true);
                        for _ in 0..delta {
                            self.resolve_one_dot();
                            self.dot = self.dot.wrapping_add(1);
                            self.ticked += 1;
                        }
                        self.mmio.set_hdma_lockstep_active(false);
                    }
                }
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

    /// Non-ticking instruction-stream peek for the HALT-bug prefetch: the byte
    /// after HALT is read at the current cc WITHOUT advancing it; the +4 charge is
    /// deferred until the prefetched opcode is consumed on the next step.
    /// Instruction memory is never PPU-gated, so a direct mmio read is faithful.
    pub fn peek(&self, addr: u16) -> u8 {
        self.mmio.read(addr)
    }

    /// Non-ticking HALT prefetch that RESPECTS the PPU access lockout: a real bus
    /// read at the current cc that resolves VRAM readability, not a debug peek.
    /// The IME-off double-HALT loop re-executes HALT at a frozen pc, re-running
    /// this fetch every M-cycle; when the pc byte lives in VRAM and mode 3 locks
    /// it, the fetch reads open-bus 0xFF (= rst $38), the hardware escape from the
    /// loop. Still no tick: the +4 fetch charge stays deferred to consumption.
    pub fn peek_fetch(&self, addr: u16) -> u8 {
        if self.ppu_locks_access(addr, self.mmio.master_cc()) {
            return 0xFF;
        }
        self.mmio.read(addr)
    }

    /// The access cc at an instruction boundary — the raw master cc captured
    /// BEFORE this access M-cycle ticks (the cc at which the access resolves,
    /// then `cc += 4`). Lets the event-cc dispatch gate compare the boundary
    /// access cc against a timer fire cc in the same space.
    /// Timing model (a memory access resolves at its own M-cycle) is documented in
    /// GBCTR "CPU core timing" (fetch/execute overlap); the event-dispatch anchor
    /// itself is emulator-internal. Not in Pan Docs.
    pub fn access_cc(&self) -> u64 {
        self.mmio.master_cc()
    }

    /// The cc at which the most recent still-undispatched TIMA IRQ fired, or `None`.
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

    /// Whether an HDMA block is in-period at a HALT-exit. The unhalt re-flag gate
    /// keys on `the HDMA-active window` evaluated at the unhalt cc, NOT a STAT-mode==0
    /// snapshot: a cached-period/STAT-mode-0 test over-fires the m0-edge block at
    /// unhalt. Resolve the period off the renderer's cycle-exact `the HDMA-active check at cc`
    /// predicate at the unhalt access cc instead, so a Low-at-halt block not yet in
    /// period at unhalt is left to fire on its natural mode-0 edge after the FF55
    /// read. Fall back to the cached/STAT gate only when no closed-form mode-0
    /// anchor exists (window / first line).
    /// HDMA-pauses-on-HALT is documented in Pan Docs (CGB Registers, FF55) and
    /// TCAGBD §9.6.2; the sub-cycle the HDMA-active window reflag timing is from test-ROM refs.
    pub fn hdma_in_period_for_unhalt(&self) -> bool {
        self.hdma_in_period_for_unhalt_adj(0)
    }

    /// As `hdma_in_period_for_unhalt`, but widens the unhalt-period line-END
    /// bracket by `limit_adj` dots. Used by the EI fast-dispatch path: when the
    /// timer IRQ is delivered at the EARLY anchor instead of the LATE anchor, the
    /// timer ISR (which re-enables the LCD via the FF40 write) runs 4 cc earlier,
    /// so the closed-form `m0_time_master` for the unhalt line lands 4 cc earlier
    /// than the baseline `hdma_period_unhalt`'s limit was calibrated against. The
    /// unhalt access cc is unchanged, so the period DEPTH (`cc - m0t`) inflates by
    /// 4 and a Low-at-halt block near the line END would drop its reflag. Widening
    /// the END bracket by +4 on the fast path restores the reflag WITHOUT
    /// disturbing the mode-0 ENTRY bracket. The non-fast (HALT-late) path passes 0.
    /// HDMA-pauses-on-HALT base-documented (Pan Docs CGB Registers, FF55; TCAGBD
    /// §9.6.2); the fast-path period-bracket adjustment is from test-ROM refs.
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
    /// would fire AT unhalt (before the next interrupt's PC pushes) per the
    /// `the HDMA-active check at cc` reflag gate at the unhalt cc. When this is false but a
    /// block still fires (its m0-edge falls within the service window) the block
    /// must be deferred past the pushes. Defaults to firing before pushes (the
    /// synchronous baseline) when no closed-form anchor exists.
    /// HDMA-resumes-on-unhalt base-documented (Pan Docs CGB Registers, FF55; TCAGBD
    /// §9.6.2); the fires-before-pushes ordering vs interrupt service is from test-ROM refs.
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

    /// CPU has just entered HALT. Computes the HDMA halt-state using the same
    /// cycle-exact `the HDMA-active check at cc` predicate the unhalt re-flag path uses
    /// (anchored on `m0_time_master`), instead of the coarse per-PPU-step
    /// `hdma_is_in_period_cached` (STAT-mode snapshot). This makes the halt-state
    /// latch straddle the line-end `cc + 3 + 3*ds < line-end` boundary precisely
    /// (in-period -> High -> 1 block vs past-boundary -> Low -> reflag -> 2 blocks,
    /// which differ by 4cc at the HALT cc). Falls back to the cached/STAT gate when
    /// no closed-form mode-0 anchor exists (window / first line).
    /// HDMA-halts-with-the-CPU base-documented (Pan Docs CGB Registers, FF55; TCAGBD
    /// §9.6.2); the sub-cycle period-latch straddle boundary is from test-ROM refs.
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
        // mode-0 period window `[m0t, m0t + line length)` (master cc; line length scales with
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

    /// Clear the recorded timer fire cc after the CPU dispatches it.
    pub fn clear_timer_fire_cc(&mut self) {
        self.mmio.clear_timer_fire_cc();
    }

    /// Whether the PPU currently locks CPU access to `addr`: VRAM during Mode 3,
    /// OAM during Mode 2/3, and (CGB) the palette-data ports FF69/FF6B during
    /// Mode 3. Only while the LCD is on. Blocked reads return 0xFF; blocked
    /// writes are dropped.
    /// Pan Docs: Accessing VRAM and OAM — https://gbdev.io/pandocs/Accessing_VRAM_and_OAM.html
    fn ppu_locks_access(&self, addr: u16, access_cc: u64) -> bool {
        self.ppu_blocks(addr, true, access_cc)
    }

    /// Whether the PPU locks `addr` from a CPU access of the given direction.
    /// Boundary precision (the exact mode-2->3 and mode-3->0 transition dots)
    /// uses the renderer's cycle-exact predictor (`cpu_access_blocked`). When no
    /// closed-form mode-0 dot is available (window / first line after enable) it
    /// falls back to the FF41 mode bits.
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
        // This is a render-visibility gate, so view the CPU access in the carried
        // anchor frame by adding the accumulated STAT-phase carry skew. A STOP
        // carry advances the STAT/line phase (so the STAT / the LY time-anchored
        // boundaries shift) but the fetcher's mode-3 lock window did NOT move; the
        // skew re-aligns the access. 0 (no shift) when no carry is live.
        let gate_cc = (access_cc as i64 - self.ppu.render_carry_skew()).max(0) as u64;
        // Derive the fallback mode (used only when no closed-form mode-0 time anchor
        // exists for this line) from the closed-form STAT resolve at the gate cc, not
        // the per-dot renderer's poked FF41 register.
        let mode = self.ppu
                .get_stat(self.mmio, gate_cc)
                .unwrap_or_else(|| self.mmio.read(ppu::LCD_STATUS) & 0x03);
        let mode_locked = if is_oam { mode == 2 || mode == 3 } else { mode == 3 };
        let ds = self.mmio.is_double_speed_mode();
        // The VRAM/OAM access-window boundaries are silicon properties that stay
        // CGB on CGB hardware even in DMG-compat mode (the compat-mode boundaries
        // match cgb-mode, not DMG), so key on `is_cgb`, not
        // `is_cgb_features_enabled` (KEY0 compat off).
        let is_cgb = self.mmio.is_cgb();
        let cgb_de = self.mmio.is_cgb_de();
        if let Some(blocked) = self.ppu.cpu_access_blocked(
            kind,
            is_read,
            mode_locked,
            crate::ppu::controller::AccessEnv { is_cgb, cgb_de, double_speed: ds },
            gate_cc,
        ) {
            return blocked;
        }
        mode_locked
    }

    /// CPU opcode fetch at `pc`. Identical to `read` for every normal fetch, but
    /// applies the PC-in-DMA-dest prefetch absorption: if a synchronous GDMA/HDMA
    /// fired on the previous instruction and `pc` is the block's first destination
    /// byte, the opcode observes the PRE-transfer VRAM byte (the prefetch runs
    /// before the transfer's writes). The prefetch's VRAM-lock decision is taken at
    /// the dma-event (fire) cc, not the post-stall fetch cc: locked at fire ->
    /// open-bus 0xFF, else the pre-transfer byte. Scoped to the opcode fetch (PC
    /// straddling ROM-bank0 -> VRAM); a normal VRAM DATA read after a transfer
    /// keeps the post-transfer byte via `read`.
    /// Not in Pan Docs, TCAGBD, or GBCTR; sub-cycle timing from test-ROM refs. (GBCTR
    /// notes instruction-fetch/OAM-DMA conflicts exist but leaves the details — and
    /// HDMA/GDMA bus conflicts entirely — as TODO.)
    pub fn fetch_opcode(&mut self, pc: u16) -> u8 {
        if let Some((pre, fire_cc)) = self.mmio.take_dma_prefetch_shadow(pc) {
            // Charge the M-cycle exactly as a normal fetch would (the read path
            // ticks before resolving), then resolve against the fire-cc lock.
            self.tick_m();
            // The prefetch resolves at the dma-event cc (the instruction boundary
            // after the FF55 write completes). The synchronous GDMA snapshot is
            // taken at the FF55 write's START cc, one M-cycle (`4`) plus the
            // `ppu_access_cc` phase (`+1`) before that boundary, so advance the
            // fire cc by that fixed offset to land the prefetch's readability on
            // the correct line cycle.
            let prefetch_cc = fire_cc.wrapping_add(5);
            // Silicon boundary, compat-safe — see `ppu_blocks`.
            let is_cgb = self.mmio.is_cgb();
            let ds = self.mmio.is_double_speed_mode();
            // Resolve readability from the LY time-derived line cycle, which honours
            // the mode-2 readable window the renderer-mode lock (`ppu_locks_access`,
            // polluted by the renderer's current FF41 mode) misses. Fall back to the
            // renderer-mode lock when no closed-form mode-0 time exists (window / first
            // line after enable).
            let readable = self
                .ppu
                .vram_readable_at_cc(prefetch_cc, is_cgb, ds)
                .unwrap_or_else(|| !self.ppu_locks_access(pc, fire_cc));
            if !readable {
                return 0xFF;
            }
            return pre;
        }
        // The shadow is valid for exactly the immediately-following opcode fetch;
        // a fetch that did not land on the block's first dest byte invalidates it
        // so it can never leak to a later same-address fetch.
        self.mmio.clear_dma_prefetch_shadow();
        let byte = self.read(pc);
        // VRAM-source GDMA first-word latch: this fetch IS the absorbed
        // next-opcode prefetch, whose byte the word bus duplicated into the
        // transfer's first dest word (see `Mmio::gdma_vram_src_fixup`). Patch it in
        // now that the byte is known.
        if let Some((addr, into_bank1)) = self.mmio.take_gdma_vram_src_fixup() {
            self.mmio.apply_gdma_vram_src_fixup(addr, byte, into_bank1);
        }
        byte
    }

    /// DMG OAM corruption bug. An OAM-bus access while the PPU is in mode 2 (OAM
    /// scan) corrupts the row the PPU is scanning. `kind` selects the pattern.
    /// DMG/MGB/SGB hardware only — CGB/AGB (including CGB-in-DMG-compat) do not
    /// have the bug, so it is gated on `!is_cgb()`. No effect outside mode 2. The
    /// row is sampled from the PPU's current OAM-scan position.
    /// Pan Docs: OAM Corruption Bug — https://gbdev.io/pandocs/OAM_Corruption_Bug.html
    fn oam_bug_corrupt(&mut self, kind: OamBugKind) {
        if self.mmio.is_cgb() {
            return;
        }
        let row = match match kind {
            OamBugKind::Read => self.ppu.oam_bug_mode2_row_read(),
            OamBugKind::Write => self.ppu.oam_bug_mode2_row(),
        } {
            Some(r) => r as usize,
            None => return,
        };
        match kind {
            OamBugKind::Write => self.mmio.oam_bug_write_corrupt(row),
            OamBugKind::Read => self.mmio.oam_bug_read_corrupt(row),
        }
    }

    /// Trigger point for a 16-bit IDU (inc/dec rr) op. On real hardware the IDU is
    /// tied to the address bus, so incrementing/decrementing a 16-bit register
    /// whose value (BEFORE the op) is in 0xFE00-0xFEFF asserts that address and
    /// triggers a write corruption during mode 2. `pre_value` is the register's
    /// value before the inc/dec.
    /// Pan Docs: OAM Corruption Bug — https://gbdev.io/pandocs/OAM_Corruption_Bug.html
    pub fn oam_bug_idu(&mut self, pre_value: u16) {
        if !(0xFE00..=0xFEFF).contains(&pre_value) {
            return;
        }
        self.oam_bug_corrupt(OamBugKind::Write);
    }

    pub fn read(&mut self, addr: u16) -> u8 {
        // APU reads (NRxx status, NR52, wave RAM) observe the channels at the read
        // M-cycle START cc. Snapshot before ticking; the per-dot step during tick_m
        // would otherwise let a length expiry scheduled within this M-cycle disable
        // a channel 4 dots early. NR52 status must reflect the pre-tick enabled
        // state.
        let apu_read = if (0xFF10..=0xFF3F).contains(&addr)
            || matches!(addr, 0xFF76 | 0xFF77)
        {
            // Resolve the APU length subsystem at the canonical per-access cc (the
            // same cc the timer register access resolves on), so the length-expiry
            // boundary is decided off one uniform clock. FF76/FF77 (PCM12/PCM34)
            // read off the same access-cc snapshot so the digital amplitude
            // reflects the channels advanced to the read M-cycle.
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
        // at the access START cc, before the M-cycle ticks `abs_cc` forward 4, so
        // the read anchor equals the scheduled-IRQ delivery anchor. DIV (FF04)
        // stays on the post-tick path with serial/APU.
        let timer_read = if matches!(addr, 0xFF04..=0xFF07) {
            Some(self.mmio.read(addr))
        } else {
            None
        };
        // IF read: the CPU resolves it at cc, but tick_m advances peripherals and
        // would let an IRQ flagged within this read M-cycle leak in 4 dots early.
        // Snapshot the VBlank (0), STAT (1), and serial (3) bits pre-tick so an IRQ
        // raised within this read cycle is observed at the read's start cc; the
        // timer/joypad bits keep the post-tick path, where their flag timing is
        // already tuned to the full M-cycle.
        const IF_PRE_MASK: u8 = 0x0B;
        // Inside an early-grid ISR the timer overflow raises IF on the early anchor
        // (see timer.rs); sample the timer bit (0x04) pre-tick too so a read-only
        // early-grid ISR misses an overflow whose early IF-set has NOT been reached
        // at the read cc. OFF / late-grid reads keep the timer bit on the post-tick
        // path where its full-M-cycle flag timing is already tuned.
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
        // FF41 (STAT) read-at-cc: resolve the mode bits with the closed-form STAT(cc) at the
        // access START cc, where the mode-3 -> mode-0 boundary is `cc + 2 <
        // mode-0 time`. The per-dot renderer sets the FF41 mode register at a dot
        // boundary that, at double speed, can round the odd-cc sample; resolving
        // against the closed-form mode-0 time samples the exact sub-dot boundary.
        // Snapshot pre-tick so the read anchors at access_cc.
        // STAT mode bits and their per-line timing are documented in TCAGBD §8.5/§8.9
        // and Pan Docs (STAT; Rendering mode durations) at dot granularity; the
        // sub-dot mode-3->0 boundary is from test-ROM refs.
        let stat_mode_pre = if addr == ppu::LCD_STATUS {
            // Post-DMA STAT-read prefetch absorption: the first STAT-mode read
            // after a GDMA/HDMA stall drain starts its M-cycle one dot high (the
            // prefetch M-cycle is double-counted by the synchronous-copy +
            // idle-stall model), so bias the anchor back. At DS the -1 applies
            // unconditionally; with SCX&7 != 0 the m0_time_master keeps a 1cc-low
            // per-SCX phase that needs a second dot (-2). At single speed the -1
            // applies only at SCX&7==0, where there is no per-SCX phase error for
            // the read_off boundary to double-correct.
            let post_dma = self.mmio.take_dma_prefetch_stat_bias();
            let ds = self.mmio.is_double_speed_mode();
            let scx0 = (self.mmio.read(ppu::SCX) & 0x07) == 0;
            let bias_cc: u64 = if post_dma && (ds || scx0) {
                if ds && !scx0 { 2 } else { 1 }
            } else {
                0
            };
            let access_cc = self.mmio.master_cc().saturating_sub(bias_cc);
            self.ppu.get_stat(self.mmio, access_cc)
        } else {
            None
        };
        // FF44 (LY) read-at-cc: resolve LY at the access cc. In the last few cc of
        // a line the register anticipates the next LY (and reads 0 early on line
        // 153); the per-dot renderer flips the register one dot boundary later, so
        // a read whose M-cycle lands in the anticipation window samples the OLD LY.
        // Resolve it from the LY-counter phase at the raw read cc.
        // Line-153 LY early-reset is base-documented in TCAGBD §8.9.1 (DMG table) /
        // §8.5; the sub-dot phase is from test-ROM refs. Not in Pan Docs.
        let ly_reg_pre = if addr == ppu::LY {
            let access_cc = self.mmio.master_cc();
            self.ppu.get_ly_reg_at_cc(self.mmio, access_cc)
        } else {
            None
        };
        // FF41 (STAT) LYC=LY coincidence flag (bit 2) read-at-cc: resolve it via
        // the LYC-compare-LY calc at the access master cc. The per-dot renderer flips the bit
        // at the dot it changes (e.g. line-153 LY=0 transient at dot 6); a read
        // whose M-cycle straddles that dot reads the post-tick register one M-cycle
        // late. Resolve the flag at access_cc so the boundary samples the exact
        // sub-dot.
        // The line-153 LY-to-compare-LYC transient is base-documented in TCAGBD
        // §8.9.1; the sub-dot phase is from test-ROM refs. Not in Pan Docs.
        let stat_lyc_pre = if addr == ppu::LCD_STATUS {
            let access_cc = self.mmio.master_cc();
            self.ppu.get_lyc_flag_at_cc(self.mmio, access_cc)
        } else {
            None
        };
        // Snapshot the access cc at the read's START (PPU access gating resolves at
        // `cc` before advancing). The cgbp begin/end boundary is master-cc based
        // and must anchor here, not at the post-tick cc.
        let pre_access_cc = self.mmio.master_cc();
        // HALT-wakeup VRAM-read phase: a VRAM read on the instruction stream
        // resumed by a HALT wakeup (`halt_wakeup_skew`) resolves its mode-3->0
        // boundary 6cc late vs the engine's `master_cc`. The LYC/STAT IRQ that woke
        // the CPU is flagged into IF one dot early and serviced in the same step,
        // so the HALT-woken stream's master_cc runs ~6cc ahead of the PPU's mode-0 time
        // anchor. The non-halt reads on the same lines are correctly phased, so
        // bias ONLY the halt-woken VRAM read's access cc, not the shared mode-0 time.
        // Not in Pan Docs, TCAGBD, or GBCTR; sub-cycle timing from test-ROM refs.
        let vram_read_cc = if (0x8000..=0x9FFF).contains(&addr)
            && self.mmio.halt_wakeup_skew()
            && self.mmio.read(ppu::LCD_CONTROL) & 0x80 != 0
        {
            // A stream that charged the CGB LYC/m1 halt-exit +4 as a REAL stall
            // reads at the true cc already; only the residual (+2 = the one-dot-
            // early IF flag) remains of the +6.
            if self.mmio.cgb_lcd_stall_charged_no_bias() {
                pre_access_cc + 2
            } else {
                pre_access_cc + 6
            }
        } else {
            pre_access_cc
        };
        // DMG OAM-bug: sample the PPU's mode-2 OAM-scan row at the access START
        // (pre-tick), so a CPU OAM read and write sample the row at the same phase
        // of their M-cycle. Captured here; the corruption is applied after the
        // read resolves (DMG-only, mode-2-gated inside `oam_bug_corrupt`).
        let oam_bug_read_row = if (0xFE00..=0xFEFF).contains(&addr)
            && !self.mmio.is_cgb()
            && !self.mmio.oam_dma_window_active()
        {
            self.ppu.oam_bug_mode2_row_read()
        } else {
            None
        };
        self.tick_m();
        // VRAM is inaccessible to the CPU during Mode 3, OAM during Mode 2/3; a
        // blocked read returns open-bus 0xFF. Only while the LCD is on.
        if self.ppu_locks_access(addr, vram_read_cc) {
            // DMG OAM-bug: even though the OAM read returns open-bus 0xFF (the PPU
            // owns OAM in mode 2/3), a CPU OAM read during mode 2 still corrupts
            // the scanned row. `oam_bug_read_row` is Some only on DMG, mode 2,
            // non-DMA; in mode 3 it is None. Apply it before returning 0xFF.
            if let Some(row) = oam_bug_read_row {
                self.mmio.oam_bug_read_corrupt(row as usize);
            }
            return 0xFF;
        }
        // A VRAM read inside the HALT-bug resume window of an in-block dest byte
        // observes the PRE-transfer value — the resume read is ordered before the
        // DMA's dest commits. The mode-readability gate above still applies (mode-3
        // -> 0xFF); a mode-0 readable read returns the old byte the just-fired
        // block has not yet committed.
        if (0x8000..=0x9FFF).contains(&addr)
            && let Some(pre) = self.mmio.hdma_resume_pre_byte(addr) {
                return pre;
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
        // DMG OAM corruption bug: a CPU OAM-bus read during PPU mode 2 returns the
        // byte normally, then corrupts the row the PPU was scanning at the access
        // start. `oam_bug_read_row` is Some only on DMG, in mode 2, outside an
        // OAM-DMA window (the DMA owns the bus; the read returns 0xFF via mmio).
        if let Some(row) = oam_bug_read_row {
            let v = self.mmio.read(addr);
            self.mmio.oam_bug_read_corrupt(row as usize);
            return v;
        }
        self.mmio.read(addr)
    }

    /// Interrupt-service low-byte push that ACKs the dispatched IF bit partway
    /// through its M-cycle (write low push, then ack IRQ). The ack advances each
    /// source to a per-source sub-cc offset before clearing only the dispatched
    /// bit `n`:
    ///   serial (bit 8): `cc + 3 + isCgb()`
    ///   timer  (bit 4): `cc + 2 + isCgb()`
    ///   lcd    (bit 2): `cc + 2`
    /// A source whose completion cc lands at or before its offset is flagged then
    /// immediately cleared; one completing later in the M-cycle re-flags IF and
    /// survives for the ISR to read. The +cgb on serial/timer is the DMG-vs-CGB
    /// discriminator for the boundary cases. `bit` is the dispatched vector's IF
    /// bit. The stack write targets RAM (SP) and is never PPU-gated.
    /// Not in Pan Docs, TCAGBD, or GBCTR; sub-cycle timing from test-ROM refs. (TCAGBD
    /// §4.7 explicitly flags the mode-0 STAT IF-ack case as untested; §4.9 / Pan Docs
    /// base-document only the 5-M-cycle IF-clear, not the per-source sub-cc offsets.)
    pub fn interrupt_low_push_ack(&mut self, sp: u16, value: u8, bit: u8) {
        // Stack byte stores at the access start (the push data is fixed for the
        // whole M-cycle); RAM is never PPU-gated.
        self.mmio.write(sp, value);
        let ds = self.mmio.is_double_speed_mode();
        let cgb = self.mmio.is_cgb() as u64;
        let start = self.mmio.master_cc();
        let target = start.wrapping_add(4);
        // Per-source ack offset, in master cc from the low-push start. Serial/timer
        // carry the +cgb the DMG/CGB boundaries hinge on; LCD is a flat +2.
        let lcd_bit = crate::cpu::registers::InterruptFlag::Lcd as u8;
        let serial_bit = crate::cpu::registers::InterruptFlag::Serial as u8;
        let timer_bit = crate::cpu::registers::InterruptFlag::Timer as u8;
        // The clear-point trigger. For the LCD vector, DS uses the raw master cc+2;
        // SS uses the PPU abs-cc edge (STAT events fire at the dot clock).
        // Serial/timer events fire off the master cc, so they use the exact master
        // `cc + offset` at both speeds. The serial fire dot equals its complete_at
        // (offset 3+cgb maps 1:1); the timer fire dot is its event time + IF_OFF(1),
        // so its equivalent crossing is also `start + 3 + cgb`. Both peripheral
        // vectors share the 3+cgb dot threshold; LCD keeps the flat +2.
        let offset = if bit == serial_bit || bit == timer_bit {
            3 + cgb
        } else {
            2
        };
        let ack_abs_threshold = self.ppu.abs_cc().wrapping_add(2);
        let ack_master_threshold = start.wrapping_add(offset);
        // The LCD is advanced to cc+2 (flagging any STAT IRQ that completes by
        // cc+2) and then the dispatched bit is cleared. A STAT IRQ whose event
        // lands one dot later (the mode-0 HBlank re-trigger at cc+3) is flagged
        // AFTER the clear and survives for the ISR to read. So the clear must land
        // the moment the LCD dot clock REACHES cc+2 (`abs_cc == threshold`), a `>=`
        // crossing, so the next dot's m0 event re-flags after it.
        //
        // This only holds for the mode-0 (HBlank) STAT IRQ. The LYC=LY / mode-1
        // re-trigger events fire one dot EARLIER in the per-dot grid, so a `>=`
        // clear would run in the same iteration that resolved them and wrongly keep
        // the bit; there the original `>` (clear one dot later) reproduces the
        // flagged-then-cleared result. Distinguished by the STAT enable bits: only
        // mode-0 (m0en, bit 3) gets the `>=` clear; LYC/mode-1/mode-2 keep `>`.
        let m0_retrig_window = (self.mmio.read(ppu::LCD_STATUS) & 0x08) != 0;
        let mut acked = false;
        while self.mmio.master_cc() < target {
            self.resolve_one_dot();
            self.dot = self.dot.wrapping_add(1);
            self.ticked += 1;
            let crossed = if bit == lcd_bit && !ds {
                if m0_retrig_window {
                    self.ppu.abs_cc() >= ack_abs_threshold
                } else {
                    self.ppu.abs_cc() > ack_abs_threshold
                }
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
        // Dirty-line probe (scanline-renderer feasibility study). Off by default
        // (probe = None => this whole block is inert). Sample at the write's ISSUE
        // cc, before any M-cycle tick, so `in_pixel_transfer()`/LY reflect the
        // state the write races against. A watched CGB palette (BCPD/OCPD) write
        // that lands during mode 3 is dropped by the PPU (`ppu_blocks`), so it does
        // not affect the current line; record it as `blocked`.
        if let Some(reg) = ppu::WatchedReg::from_addr(addr) {
            let blocked = matches!(reg, ppu::WatchedReg::Bcpd | ppu::WatchedReg::Ocpd)
                && !self.mmio.dma_active()
                && self.ppu_blocks(addr, false, self.mmio.master_cc());
            self.ppu.dirty_probe_register_write(reg, blocked);
        }
        // Registers belonging to peripherals we tick inline (timer/serial/DMA)
        // latch at the end of the write M-cycle, so advance first. Everything else
        // (PPU registers, RAM) takes effect as the access is issued.
        //
        // While an OAM DMA transfer is running, the DMA engine advances during this
        // M-cycle BEFORE the CPU's write is resolved, and a write into the DMA's
        // conflict area is redirected into OAM at the current DMA position. Ticking
        // the M-cycle first reproduces that ordering so `dma_pos` is the value for
        // this cycle when `mmio.write` resolves the conflict.
        // Pan Docs: OAM DMA Transfer — https://gbdev.io/pandocs/OAM_DMA_Transfer.html
        //
        // A deferred post-HALT VRAM write on CGB resolves its PPU mode-block
        // against the post-block1-transfer cc (the PPU advances across block1's
        // transfer before the CPU resumes). Add block1's transfer span to this
        // VRAM write's mode check only; block2's timing is left untouched.
        // One-shot: consumed by this first VRAM write.
        let write_block_cc = {
            let base = self.mmio.master_cc();
            // CGB LYC/m1 halt-exit stall residual (mirror of the read-side VRAM +6
            // -> +2): the PPU access-gate boundaries are co-tuned to the un-stalled
            // halt-woken write cc, so a stream that charged the +4 as a real stall
            // resolves its gate at the legacy anchor. The write's world-visible
            // timing (value landing, timer/IF interactions) keeps the true stalled cc.
            let base = if self.mmio.cgb_lcd_stall_charged_no_bias() {
                base.saturating_sub(4)
            } else {
                base
            };
            if (0x8000..=0x9FFF).contains(&addr) {
                base + self.mmio.take_hdma_dma_due_write_cc_bias()
            } else {
                base
            }
        };
        if !self.mmio.dma_active() && self.ppu_blocks(addr, false, write_block_cc) {
            // DMG OAM-bug: the PPU owns OAM in mode 2/3 so the write is dropped,
            // but a write to OAM during mode 2 still corrupts the scanned row.
            // Sample the row at the access start (pre-tick), matching the read
            // path. DMG-only + mode-2-gated inside `oam_bug_corrupt`.
            if (0xFE00..=0xFEFF).contains(&addr) {
                self.oam_bug_corrupt(OamBugKind::Write);
            }
            // CGB: a mode-3-blocked BCPD/OCPD write drops the palette byte but
            // still performs the BGPI/OBPI auto-increment.
            if addr == 0xFF69 || addr == 0xFF6B {
                self.mmio.palette_blocked_write_increment(addr);
            }
            self.tick_m();
            return;
        }

        // TIMA/TMA/TAC writes (FF05-07) resolve at the access START cc, then time
        // advances. The scheduled-TIMA model derives/IRQ-delivers against `abs_cc`,
        // so the write must land before the M-cycle ticks for its anchor to match
        // the start-cc read anchor. FF04 (DIV) stays on the tick-before path below:
        // its `div_anchor` is shared by serial/APU-FS which are NOT yet on
        // start-cc, so moving DIV alone would mix anchors.
        if matches!(addr, 0xFF04..=0xFF07) {
            self.mmio.write(addr, value);
            self.tick_m();
            return;
        }

        // SC (FF02) writes resolve at the access START cc (the write's `cc` is the
        // access cc, NOT cc+4). An ABORT (bits cleared) must land BEFORE this
        // M-cycle's per-dot `step_serial` so the in-flight transfer cannot complete
        // and raise the serial IF the abort suppresses. A START write must ALSO
        // resolve at the access cc: the completion event is `cc - (cc -
        // DIV last-update) % align + step*8`, so capturing `cc` (and the DIV residue)
        // at the post-tick cc+4 would place the completion 4 cc late. Writing
        // before the tick anchors the SC write at the same start cc DIV/TIMA use.
        if addr == 0xFF02 {
            self.mmio.write(addr, value);
            self.tick_m();
            return;
        }

        if addr == 0xFF0F && !self.mmio.dma_active() {
            // Pump timer overflows at the write cc first (event update before the
            // IF store): an overflow whose schedule has been reached flags IF now
            // and the store below overwrites it.
            self.mmio.flush_timer_overflow_for_ifreg_write();
            // IF (0xFF0F) write: split the write M-cycle so the explicit IF store
            // lands partway through it (at the write cc, after the access M-cycle's
            // leading dots). An IRQ flagged at a cc <= store_cc is already in IF and
            // is overwritten by the write; one flagged later survives. The m0 STAT
            // IRQ at mode-0 time-1 falls one dot into the M-cycle, so storing one dot in
            // clears it while a later m2/lyc IRQ survives. One dot = `1 << ds`
            // master-cc, so the store lands at the same sub-dot at both speeds.
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
            // FF4A (WY): schedule at the write's cc (read-at-cc-start phase, like
            // the STAT path) before the M-cycle ticks.
            if addr == ppu::WY {
                self.ppu.set_write_subdot(self.mmio.cpu_t_phase());
                self.ppu.on_wy_write(value, self.mmio);
            }
            self.tick_m();
            self.mmio.write(addr, value);
            // An FF55 bit7=1 kick written while OAM-DMA is active routes through
            // this tick-before path; resolve its live-period gate here too so the
            // flag never leaks (see the else branch for the rationale).
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
            // FF55 disable-vs-m0-edge race: an FF55 bit7=0 write to an enabled HDMA
            // only kills the FUTURE m0-edge schedule. A block whose m0 edge has
            // already fired at/before this write's access cc is already latched and
            // still runs. Resolve that at the write's access cc (raw master_cc, the
            // same anchor the STAT resolve uses) and stash it so the write handler keeps the
            // request instead of canceling. Evaluated BEFORE the write.
            if addr == 0xFF55 && (value & 0x80) == 0 && self.mmio.hdma_is_enabled() {
                let ds = self.mmio.is_double_speed_mode();
                let cc = self.mmio.master_cc();
                self.mmio
                    .set_hdma_disable_fires(self.ppu.hdma_disable_fires(cc, ds));
            }
            // DMG OAM-bug: a CPU write to the OAM range (0xFE00-0xFEFF) reaching
            // this branch (NOT blocked by the mode-2/3 `ppu_blocks` guard, which
            // only covers 0xFE00-0xFE9F) still corrupts the scanned row in mode 2 —
            // notably writes to the 0xFEA0-0xFEFF tail (PUSH/CALL stack writes into
            // OAM). Sample at the access start (pre-tick); `oam_bug_corrupt` is
            // DMG-only + mode-2-gated, so this is a no-op outside mode 2.
            if (0xFE00..=0xFEFF).contains(&addr) {
                self.oam_bug_corrupt(OamBugKind::Write);
            }
            self.mmio.write(addr, value);
            // FF42/FF43 (SCY/SCX): the CPU readback above is immediate, but the BG
            // fetcher must see the new value a few dots later. The mmio write phase
            // is unchanged (pre-tick) so steady-state rendering is identical; only
            // the fetcher's mid-M3 view is delayed via on_sc{y,x}_write.
            if addr == ppu::SCY {
                self.ppu.on_scy_write(value, self.mmio);
            }
            if addr == ppu::SCX {
                self.ppu.on_scx_write(value, self.mmio);
            }
            if addr == ppu::LCD_CONTROL {
                // Sticky mid-mode-3 LCDC-writer marker: a ROM that races LCDC
                // against the fetcher has its mid-m3 LCDC glitch targets co-tuned to
                // the un-stalled halt-woken write grid, and they cannot be
                // re-anchored post-hoc (they land before the write in engine
                // order), so such ROMs keep the legacy (bias-model) CGB LCD
                // halt-exit timing (see sm83.rs stall scoping).
                if self.ppu.in_pixel_transfer() {
                    self.mmio.set_m3_lcdc_write_seen();
                }
                self.ppu.handle_lcdc_write(value, self.mmio);
            }
            // FF47/FF48/FF49 (BGP/OBP0/OBP1): the CPU readback is immediate (mmio
            // write above), but the rendered palette mapping must change at the
            // exact pixel drawn a fixed latency after the write (mid-mode-3
            // per-pixel palette effect). Record the change keyed by display column;
            // the per-column draw resolves it. No-op outside pixel transfer.
            if addr == ppu::BGP {
                self.ppu.on_bgp_write(value, self.mmio);
            }
            if addr == ppu::OBP0 {
                self.ppu.on_obp0_write(value, self.mmio);
            }
            if addr == ppu::OBP1 {
                self.ppu.on_obp1_write(value, self.mmio);
            }
            if self.mmio.take_stat_register_write_pending() {
                self.ppu.on_stat_register_write(self.mmio);
            }
            // Resolve a pending FF55 bit7=1 kick against the live HDMA period
            // predicate (the HDMA-active check at cc+4) rather than the 1-dot-lagged renderer
            // cache. Evaluated at the write's access cc, before the M-cycle ticks.
            // When `hdma_period` cannot supply a closed-form mode-0 dot (window /
            // first line after enable) fall back to the cached period.
            if self.mmio.hdma_kick_eval_pending() {
                let ds = self.mmio.is_double_speed_mode();
                let cc = self.mmio.master_cc();
                // Late-HBlank-aware kick predicate so an FF55 enable written
                // mid-HBlank (after mode-0 entry, same line) still arms its block.
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

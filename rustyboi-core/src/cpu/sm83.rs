use crate::{cpu::opcodes, cpu::registers, memory, memory::Addressable};
use serde::{Deserialize, Serialize};

/// The five interrupt sources in hardware service priority, highest first.
/// Both dispatch queries walk this one table rather than repeating the order as
/// a branch chain. Pan Docs: Interrupts — https://gbdev.io/pandocs/Interrupts.html
const INTERRUPT_PRIORITY: [registers::InterruptFlag; 5] = [
    registers::InterruptFlag::VBlank,
    registers::InterruptFlag::Lcd,
    registers::InterruptFlag::Timer,
    registers::InterruptFlag::Serial,
    registers::InterruptFlag::Joypad,
];

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct SM83 {
    pub registers: registers::Registers,
    pub(crate) halted: bool,
    pub stopped: bool,
    #[serde(default)]
    pub(crate) ime_enable_delay: u8,
    /// T-cycles remaining in the post-STOP-speed-switch stall.
    /// While non-zero, `step` returns short slices without fetching, so the
    /// surrounding `step_instruction` loop continues to advance peripherals.
    #[serde(default)]
    pub(crate) stop_unhalt_cycles: u32,
    /// Opcode prefetched at the previous instruction's boundary; `prefetched` is
    /// true once that fetch has happened and the opcode awaits execute/discard.
    #[serde(default)]
    pub opcode: u8,
    #[serde(default)]
    pub(crate) prefetched: bool,
    /// HALT-bug prefetch marker. The HALT bug (IME=0 + pending IRQ) peeks the byte
    /// after HALT without charging its fetch M-cycle, unlike the normal
    /// end-of-instruction prefetch. When that peeked opcode is consumed its fetch
    /// M-cycle must still be charged, so the doubled instruction's operand read
    /// resolves one M-cycle later — on the cc a live IO register (TIMA/DIV read as
    /// the immediate) has ticked to. Set only by the HALT-bug peek.
    /// Pan Docs: halt bug — https://gbdev.io/pandocs/halt.html
    #[serde(default)]
    pub(crate) halt_bug_prefetch: bool,
    /// HALT-exit +4: an m2-woken wake charged its extra M-cycle as a real stall
    /// (the `return 4` below), so the woken stream is already at the hardware cc
    /// and the timer-read facet must not re-add the advance.
    /// Base extra-4-clock HALT exit: TCAGBD §4.9. The per-wake-source split
    /// (m2 vs LYC/m1 vs serial) is from test-ROM refs, not in Pan Docs or GBCTR.
    #[serde(default)]
    pub(crate) m2_halt_stall_charged: bool,
    /// CGB LCD-woken HALT-exit +4: the LYC/m1-woken CGB wake charged its extra
    /// M-cycle as a real stall (the `return 4` below). One-shot guard so the
    /// still-halted re-entry does not stall again; taken at unhalt.
    /// Base extra-4-clock HALT exit: TCAGBD §4.9. The CGB LYC/m1 per-wake-source
    /// split is from test-ROM refs, not in Pan Docs or GBCTR.
    #[serde(default)]
    pub(crate) cgb_lcd_halt_stall_charged: bool,
    /// CGB HDMA dma-due deferral: a Requested multi-block HDMA at CGB HALT-exit
    /// with a pending IME=1 interrupt fires its block at unhalt, then executes ONE
    /// post-HALT instruction BEFORE the interrupt is serviced (the block's DMA
    /// event runs first and pushes the interrupt's min-time past that instruction).
    /// Set on the unhalt step, consumed the same step to skip the immediate service
    /// and run the prefetched resume opcode; the interrupt stays pending.
    /// Not in Pan Docs, TCAGBD, or GBCTR; CGB HDMA-vs-interrupt HALT-exit
    /// ordering from test-ROM refs.
    #[serde(default)]
    pub(crate) hdma_dma_due_defer_service: bool,
}

impl Default for SM83 {
    fn default() -> Self {
        Self::new()
    }
}

impl SM83 {
    pub fn new() -> Self {
        SM83 {
            registers: registers::Registers::new(),
            halted: false,
            stopped: false,
            ime_enable_delay: 0,
            stop_unhalt_cycles: 0,
            opcode: 0,
            prefetched: false,
            halt_bug_prefetch: false,
            m2_halt_stall_charged: false,
            cgb_lcd_halt_stall_charged: false,
            hdma_dma_due_defer_service: false,
        }
    }

    /// Power-on state, in place. Owned here rather than field-by-field at the
    /// `GB::reset` call site so a newly added latch (the prefetch/HALT-exit
    /// one-shots above are all `#[serde(default)]` cross-instruction carriers)
    /// cannot survive a reset by being forgotten there.
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn step(&mut self, mmio: &mut crate::cpu::Bus) -> u32 {
        // While stalled after a CGB STOP-speed-switch, advance peripherals in
        // small slices without fetching instructions. CPU fetch is suspended for
        // 0x20000 + 4 T-cycles after STOP completes; the per-cycle peripheral loop
        // in gb.rs still runs.
        if self.stop_unhalt_cycles > 0 {
            // STOP-window early wake: the post-STOP stall window is also a halt, so
            // a pending enabled interrupt (timer/serial) fired during it wakes the
            // CPU before the window drains. Poll each slice boundary; on a pending
            // interrupt, terminate the window and fall through to the halt-exit /
            // dispatch path below (route it through the same `self.halted` path).
            // Speed-switch window itself: TCAGBD §3.8; the interrupt early-wake
            // within it is not in Pan Docs, TCAGBD, or GBCTR (test-ROM refs).
            if self.get_pending_interrupt(mmio).is_some() {
                // Terminate the window here and run the unhalt exit gate (OAM-DMA
                // unfreeze + HDMA reflag) that the window-drained-to-0 path runs.
                self.stop_unhalt_cycles = 0;
                mmio.set_oam_dma_stop_freeze(false);
                if mmio.in_stop_window() {
                    let in_period_unhalt = mmio.hdma_in_period_for_unhalt();
                    mmio.stop_window_exit_reflag(in_period_unhalt);
                }
                // Fall through to dispatch WITHOUT charging an extra wake M-cycle:
                // the HALT-exit advance is already absorbed in the +5 cc at which
                // rustyboi first sees the timer IF, so an added +4 would read
                // DIV/TIMA one tick high. Let the pending interrupt dispatch now.
            } else {
                let slice = self.stop_unhalt_cycles.min(4);
                self.stop_unhalt_cycles -= slice;
                // Stop window over: run the HDMA reflag gate at the unhalt cc, then
                // re-enable the period edge. A block held Low-at-stop reflags only if
                // the unhalt lands back in the HDMA period; one whose unhalt is out
                // of period stays dropped.
                if self.stop_unhalt_cycles == 0 {
                    // Unfreeze OAM DMA (unhalt resumes it).
                    mmio.set_oam_dma_stop_freeze(false);
                    if mmio.in_stop_window() {
                        let in_period_unhalt = mmio.hdma_in_period_for_unhalt();
                        let dsb = mmio.is_double_speed_mode();
                        let unhalt_cc = mmio.master_cc() as i64;
                        let edge = mmio
                            .ppu
                            .hdma_m0_edge(dsb)
                            .map(|e| (e, unhalt_cc));
                        mmio.stop_window_exit_reflag_edge(in_period_unhalt, edge);
                    }
                }
                return slice;
            }
        }

        // Charge the CPU stall owed for HDMA/GDMA blocks: idle (no fetch) while
        // peripherals keep ticking for the transfer duration.
        let dma_stall = mmio.take_dma_stall();
        if dma_stall > 0 {
            return dma_stall;
        }

        let mut cycles = 0;

        // Check for pending interrupts
        let mut pending_interrupt = self.get_pending_interrupt(mmio);

        // If halted, check if we should exit halt state
        let mut just_unhalted = false;
        if self.halted {
            if pending_interrupt.is_some() {
                // M-cycle-grid wake (DMG): with idle batches quantized to whole
                // M-cycles (Bus::halted_idle_dots), this boundary B is the first
                // grid point that SEES the waking IF bit (raised at cc R during
                // the last batch). Hardware samples IF near the END of each
                // M-cycle: an IF that rose fewer than 2 T-cycles before B misses
                // the sample and the exit slips one more whole M-cycle. The
                // per-source event time E is the raise cc for the m0/m2 STAT
                // dispatches (they raise AT their event cc) and raise-1 for
                // LYC/m1/VBlank/Timer (their IF delivery runs one dot after the
                // event; timer delivery = sched+CC_OFF, one past the +4 IF cc).
                // This one rule replaces the DMG per-source +4 stalls and every
                // DMG read-side wake bias: the woken stream now RESUMES at the
                // hardware cc instead of reconstructing it per consumer.
                // Base extra-4-clock HALT exit: TCAGBD §4.9; the 2-T-cycle
                // sampling setup window is a sub-cycle refinement from test-ROM
                // refs (the {E%4 -> +4,+3,+2,+5} advance staircase).
                if mmio.mmio.halt_grid_quantized()
                    && !self.stopped
                    && !self.m2_halt_stall_charged
                    && let Some(f) = pending_interrupt
                {
                    // Minimum-residency clamp: an IF that rises during (or right
                    // after) the HALT M-cycle itself cannot end the halt at the
                    // first post-entry boundary — hardware spends at least one
                    // full HALTED M-cycle before the exit sampling can hit, so
                    // the earliest wake is entry+8. This is the real mechanism
                    // behind the legacy _1b/_2b prefetch-phase split (the woken
                    // stream 'inheriting' an extra M-cycle when the event fell
                    // inside the entry window).
                    if let Some(h) = mmio.mmio.halt_entry_cc()
                        && mmio.master_cc_dbg().wrapping_sub(h) < 8
                    {
                        return 4;
                    }
                    let r = mmio.mmio.if_raise_cc_of(f);
                    if r != u64::MAX {
                        let kind = mmio.mmio.lcd_raise_kind();
                        let e_off = match f {
                            registers::InterruptFlag::Lcd => {
                                match kind {
                                    memory::mmio::LCD_RAISE_M0
                                    | memory::mmio::LCD_RAISE_M2 => 0,
                                    _ => 1,
                                }
                            }
                            registers::InterruptFlag::VBlank => 1,
                            // The timer raise cc (sched + CC_OFF) already
                            // includes the CPU's sampling M-cycle (+4 M-cycle
                            // end, +1 step lag): it IS the hardware wake
                            // boundary, so the setup rule must never add to it.
                            registers::InterruptFlag::Timer => 5,
                            _ => 0,
                        };
                        // CGB console: the exit sampling runs one M-cycle
                        // later than DMG (AntonioND gbc-hw-tests: every CGB
                        // ISR-read grid sits 4cc after the DMG grid for the
                        // same IF-set cc), so the setup window widens by a
                        // whole M-cycle: every LCD/VBlank/serial wake slips
                        // +4 while the timer (raise already ON the boundary,
                        // B-E >= 5) still exits immediately. Exceptions pinned
                        // by real-silicon captures: the LY144 VBlank-entry m2
                        // quirk, a wake right after an m2 dispatch
                        // (vblank_stat_intr), and mid-m3 LCDC racer streams.
                        // The CGB extra exit M-cycle covers LCD wakes in both
                        // cart modes, but the VBlank wake only in DMG-compat
                        // mode: CGB-native VBlank wakes exit on the DMG window
                        // (daid speed_switch first-read $85 vs the dmg_mode
                        // if/ly_timings families' +4-pinned cells, all real
                        // silicon).
                        let cgb = mmio.mmio.is_cgb()
                            && (f == registers::InterruptFlag::Lcd
                                || (f == registers::InterruptFlag::VBlank
                                    && !mmio.mmio.is_cgb_features_enabled()));
                        let is_m2 = f == registers::InterruptFlag::Lcd
                            && kind == memory::mmio::LCD_RAISE_M2;
                        // A proximate m2 dispatch only cancels the exit stall when
                        // the interrupt machinery is live (IME=1) and can actually
                        // dispatch it; an IME=0 HALT just falls through to the next
                        // instruction, so the m2 never contends for the exit and the
                        // plain setup window applies. Same IME-on scoping the legacy
                        // m2 stall path below already uses.
                        // Discriminator pinned by two captures that agree on every
                        // other observable (VBlank wake, kind=m2, m2 4cc back, LY144,
                        // B-E=2, IE.1 clear) and want opposite answers:
                        // mooneye vblank_stat_intr (IME=1, no stall) vs
                        // gbc-hw-tests lcd_frame_timings/mode2 (IME=0, stall). Without
                        // the gate a leftover STATF_MODE10 in FF41 silently cancels
                        // the stall for mode2's banks 1..7 but not bank 0 (which runs
                        // before the probe routine arms mode-2 select), collapsing the
                        // ROM's first 4cc delay-sled increment.
                        let m2_prox = !is_m2
                            && self.registers.ime
                            && mmio.mmio.last_m2_irq_fire_cc().is_some_and(|fire| {
                                mmio.master_cc_dbg().wrapping_sub(fire) < 8
                            });
                        // Excluded CGB shapes take NO exit stall at all (the
                        // quirk wake is pinned at the immediate boundary by the
                        // -C captures), not the DMG window.
                        let window = if !cgb {
                            Some(1)
                        } else if (is_m2 && mmio.mmio.last_m2_irq_ly() >= 144)
                            || m2_prox
                            || mmio.mmio.m3_lcdc_write_seen()
                        {
                            None
                        } else {
                            // 4, not 5: the timer's raise cc is already the
                            // wake boundary (B-E >= 5); every LCD/VBlank/serial
                            // class sits at B-E <= 4 and takes the extra
                            // M-cycle.
                            Some(4)
                        };
                        let e = r.wrapping_sub(e_off);
                        if let Some(w) = window
                            && mmio.master_cc_dbg().wrapping_sub(e) <= w
                        {
                            self.m2_halt_stall_charged = true;
                            return 4;
                        }
                    }
                }
                // HALT-exit +4 (CGB legacy path, m2-woken): hardware spends one
                // extra M-cycle leaving HALT when the wake lands on the waking
                // IRQ's event time. Charged as a REAL stall (still halted; the
                // world advances) so downstream event cc's shift with it. IME-on
                // only. CGB-only now: the DMG m2 wake takes the same +4 through
                // the M-cycle-grid setup rule above; here the +4 applies only for
                // a rendering-line (LY 0..143) m2 wake, not the LY 144
                // VBlank-entry m2 quirk.
                // Base extra-4-clock HALT exit: TCAGBD §4.9. The m2-woken per-source
                // split (and CGB LY<144 scope) is from test-ROM refs, not Pan Docs/GBCTR.
                let m2_stall_ok = if mmio.mmio.is_cgb() {
                    mmio.mmio.last_m2_irq_ly() < 144
                } else {
                    true
                };
                if !mmio.mmio.halt_grid_quantized()
                    && self.registers.ime
                    && m2_stall_ok
                    && pending_interrupt == Some(registers::InterruptFlag::Lcd)
                    && mmio
                        .mmio
                        .last_m2_irq_fire_cc()
                        .is_some_and(|fire| mmio.master_cc_dbg().wrapping_sub(fire) < 2)
                {
                    self.m2_halt_stall_charged = true;
                    if mmio.mmio.is_cgb() {
                        mmio.mmio.set_m2_halt_stall_charged_cgb(true);
                    }
                    return 4;
                }
                // HALT-exit +4 (CGB, LYC/m1 LCD wake): a CGB console spends one
                // extra real M-cycle leaving HALT on a STAT(LYC/m1) wake where DMG
                // resumes immediately (AntonioND gbc-hw-tests: every CGB ISR-read
                // grid sits 4cc after the DMG grid for the same LYC IRQ, while the
                // IF-set cc is identical — so this is per-wake-source, not an
                // unconditional CGB cost). Charged as a REAL stall (writes shift
                // too). IME-independent. Scoped: m0/m2-proximate wakes, HDMA wakeups,
                // and post-STOP streams keep their own paths (below). The mmio flag
                // drops the matching read-side +4 at the consume sites, keeping
                // pure-read consumers byte-identical.
                // Base extra-4-clock HALT exit: TCAGBD §4.9 (which frames it as
                // model-independent); this per-wake-source CGB LYC/m1 delta is from
                // test-ROM refs, not in Pan Docs or GBCTR.
                if !mmio.mmio.halt_grid_quantized()
                    && !self.stopped
                    && pending_interrupt == Some(registers::InterruptFlag::Lcd)
                    && !self.m2_halt_stall_charged
                    && !self.cgb_lcd_halt_stall_charged
                {
                    let mcc = mmio.master_cc_dbg() as i64;
                    let m0_prox = mmio
                        .mmio
                        .pending_m0_irq_fire_cc()
                        .is_some_and(|ev| (-2..=6).contains(&(mcc - ev as i64)));
                    let m2_prox = mmio
                        .mmio
                        .last_m2_irq_fire_cc()
                        .is_some_and(|fire| (mcc as u64).wrapping_sub(fire) < 8);
                    // Sticky FF55 marker: a GDMA / late-armed HDMA that fired only
                    // after the wake is invisible to the wake-time predicates but is
                    // co-tuned to the un-stalled wake, so exclude it too.
                    let hdma_wakeup = mmio.hdma_is_enabled()
                        || mmio.hdma_last_fire_cc().is_some()
                        || mmio.mmio.hdma_machinery_used()
                        || !matches!(
                            mmio.halt_hdma_state(),
                            memory::dma::HaltHdmaState::Low
                        );
                    // Mid-m3 LCDC-race streams keep the legacy timing.
                    let lcdc_racer = mmio.mmio.m3_lcdc_write_seen();
                    if !m0_prox && !m2_prox && !hdma_wakeup && !lcdc_racer {
                        self.cgb_lcd_halt_stall_charged = true;
                        mmio.mmio.set_m2_halt_stall_charged_cgb(true);
                        return 4;
                    }
                }
                // HALT-exit +4 (serial-woken, DMG+CGB): a serial-woken HALT resumes
                // one M-cycle after the completion edge on both consoles (AntonioND
                // serial_int_handle_timing). This is the per-source IF-edge-vs-halt-
                // sampling phase, not a universal exit cost (the timer-woken exit has
                // no such delay). Same one-shot guard; post-STOP streams keep the
                // cc-exact window exit. The CGB read-residual flag is set so a later
                // LCD-read consume site sees the true (+4) stream cc.
                // Base extra-4-clock HALT exit: TCAGBD §4.9. The per-source split is
                // from test-ROM refs, not in Pan Docs or GBCTR.
                // NOTE: TCAGBD §4.9 frames the +4 as universal ("any interrupt");
                // our model makes it per-source (serial delayed, timer not).
                if !mmio.mmio.halt_grid_quantized()
                    && !self.stopped
                    && pending_interrupt == Some(registers::InterruptFlag::Serial)
                    && !self.cgb_lcd_halt_stall_charged
                {
                    self.cgb_lcd_halt_stall_charged = true;
                    if mmio.mmio.is_cgb() {
                        mmio.mmio.set_m2_halt_stall_charged_cgb(true);
                    }
                    return 4;
                }
                self.halted = false;
                just_unhalted = true;
                // Record the waking source class: only a VBLANK-woken CGB-native
                // stream leaves its exit M-cycle uncharged (see the read-phase
                // residue in get_ly_reg_at_cc).
                mmio.mmio.set_halt_wake_vblank(
                    pending_interrupt == Some(registers::InterruptFlag::VBlank),
                );
                // One-shot guard consumed: the woken stream is live, re-arm for
                // the next HALT (the mmio-side flag persists on the stream and
                // is cleared by the next HALT's reset_halt_wakeup).
                self.cgb_lcd_halt_stall_charged = false;
                mmio.clear_cpu_halt();
                // Mark whether this wakeup involved HDMA: those wakes fold the CGB
                // halt-exit +4 into their own transfer/reflag phase, so the
                // get_ly_reg_at_cc halt-exit bias is suppressed for them.
                let hdma_wakeup = mmio.hdma_is_enabled()
                    || mmio.hdma_last_fire_cc().is_some()
                    || !matches!(
                        mmio.halt_hdma_state(),
                        memory::dma::HaltHdmaState::Low
                    );
                mmio.set_halt_wakeup_hdma(hdma_wakeup);
                // The resumed stream carries the unmodeled HALT-prefetch sub-M-cycle
                // skew; flag it so the FF41 STAT line-tail override defers to the
                // renderer register (already correct there). Cleared on the next HALT.
                let legacy_wake = !mmio.mmio.halt_grid_quantized();
                let grid_cgb = mmio.mmio.is_cgb() && !legacy_wake;
                mmio.set_halt_wakeup_skew(legacy_wake);
                mmio.mmio.set_halt_wake_grid_cgb(grid_cgb);
                // The line-tail mode-2 STAT overrides model the m0/m2-wake-exit
                // skew of those streams; mark whether THIS wake is m0/m2-proximate so
                // an LYC/m1-woken stream's line-tail STAT read resolves the true mode
                // instead. The m0 window is the widened [-2,6] grid; the m2 test
                // mirrors the stall check above plus the stall-charged flag.
                {
                    let mcc = mmio.master_cc_dbg() as i64;
                    let m0_prox = mmio
                        .mmio
                        .pending_m0_irq_fire_cc()
                        .is_some_and(|ev| (-2..=6).contains(&(mcc - ev as i64)));
                    let m2_prox = mmio
                        .mmio
                        .last_m2_irq_fire_cc()
                        .is_some_and(|fire| (mcc as u64).wrapping_sub(fire) < 2)
                        || self.m2_halt_stall_charged;
                    let lcd_wake =
                        pending_interrupt == Some(registers::InterruptFlag::Lcd);
                    mmio.set_halt_wake_m0m2(
                        legacy_wake && lcd_wake && (m0_prox || m2_prox),
                    );
                }
                // One-shot M-cycle-grid stall guard consumed: the woken DMG
                // stream is already at the hardware cc (real batch quantization +
                // setup-time stall), so no read-side re-anchors are armed for it.
                let _m2_stall_charged = std::mem::take(&mut self.m2_halt_stall_charged);
                // HALT-exit, CGB m0-woken stream: on CGB the halt-exit +4 is
                // unconditional (not gated on delta < 2), so the wake advance is the
                // ceil-to-M-cycle snap plus a flat +4. Scoped to the DMG-cart case: a
                // CGB-flagged cart instead takes the +5 read bias in get_ly_reg_at_cc,
                // so this must not double-apply there. Consumed read-side by the woken
                // FF44 read.
                // Base extra-4-clock HALT exit: TCAGBD §4.9. The CGB unconditional-+4
                // m0 LY-read re-anchor is a sub-cycle refinement from test-ROM refs,
                // not in Pan Docs or GBCTR.
                if pending_interrupt == Some(registers::InterruptFlag::Lcd)
                    && mmio.mmio.is_cgb()
                    && !mmio.mmio.is_cgb_features_enabled()
                    && let Some(ev) = mmio.mmio.pending_m0_irq_fire_cc()
                {
                    let mcc = mmio.master_cc_dbg() as i64;
                    if mcc - (ev as i64) < 2 {
                        let align = ((4 - (mcc % 4)) % 4) as u32;
                        // Unconditional +4 on CGB, vs the DMG rule's setup-gated +4.
                        let adv = align + 4;
                        mmio.mmio.set_cgb_m0_halt_ly_advance(Some(adv));
                    }
                }
                // HALT-prefetch woken-PC push phase. The interrupt service undoes
                // the boundary prefetch unconditionally (`pc -= 1`), correct when the
                // unhalt did a real pc-advancing fetch. But a Requested-halt wakeup
                // left a NON-advancing prefetch peek at HALT entry (opcodes.rs halt()
                // peeks pc=HALT+1 without advancing pc), so the unconditional undo
                // over-subtracts by one, pinning the pushed resume PC one short. Mark
                // phase 1 for exactly that case so the push consume re-adds the +1.
                // Gated to Timer + CGB, in its own register (timer_push_phase).
                // Not in Pan Docs, TCAGBD, or GBCTR; HALT-wake PC-push prefetch phase
                // from test-ROM refs.
                let req_halt_peek = self.prefetched
                    && matches!(
                        mmio.halt_hdma_state(),
                        memory::dma::HaltHdmaState::Requested
                    );
                if pending_interrupt == Some(registers::InterruptFlag::Timer)
                    && mmio.mmio.is_cgb()
                {
                    let phase = if req_halt_peek { 1u32 } else { 0u32 };
                    mmio.mmio.set_timer_push_phase(phase);
                }
                // Unhalt HDMA re-flag gate:
                //   (HDMA-enabled && the HDMA-active window && halt-HDMA-state == hdma_low)
                //   || halt-HDMA-state == hdma_requested
                // Keys on hdma_low: the block fires on unhalt only when the HDMA
                // period was entered DURING the halt (Low at halt time), not when it
                // was already in-period+armed (High, which already fired). Evaluate
                // the unhalt period off the renderer's cycle-exact the HDMA-active check at cc,
                // so a Low-at-halt block not yet in period at unhalt fires on its
                // natural m0 edge.
                // Timer IRQ is delivered at the early anchor (scheduled cc + IF_OFF). The
                // timer ISR re-enables the LCD (FF40 write), so its m0_time_master
                // lands 4cc earlier while the unhalt access cc is unchanged — the
                // unhalt-period depth inflates by 4, which would drop the reflag of a
                // Low-at-halt block near the line end. Widen the unhalt-period END
                // bracket by +4 so that block still reflags, leaving the mode-0 ENTRY
                // bracket intact (a depth-0 block reflags either way).
                let limit_adj: i64 = 4;
                let in_period_unhalt = mmio.hdma_in_period_for_unhalt_adj(limit_adj);
                let was_requested =
                    matches!(mmio.halt_hdma_state(), memory::dma::HaltHdmaState::Requested);
                match mmio.halt_hdma_state() {
                    memory::dma::HaltHdmaState::Requested => {
                        mmio.set_hdma_req();
                        // A multi-block Requested transfer (hdma_length() != 0) does
                        // NOT inline-fire at unhalt (gated off below); its first block
                        // fires on its m0 edge DURING the resume instruction. Arm the
                        // lockstep window so the bus advances the world through that
                        // block's transfer at fire time, so the same-instruction
                        // resume read sees the extended mode-3 line. Cleared when the
                        // resume instruction completes. IME-off only.
                        if !self.registers.ime && mmio.hdma_length() != 0 {
                            mmio.set_hdma_resume_lockstep_window(true);
                        }
                        // Pre-transfer dest-byte shadow: lets the resume read observe
                        // the old VRAM byte (ordered before dma()'s dest commits).
                        // Armed for both IME states; harmless when the relevant block
                        // fires outside the window.
                        if mmio.hdma_length() != 0 {
                            mmio.set_hdma_resume_shadow_window(true);
                        }
                        // CGB dma-due deferral: when IME is on and the wake services a
                        // CGB interrupt, the block's DMA event fires first, prefetches
                        // the post-HALT opcode and pushes the interrupt's min-time past
                        // the transfer — so the CPU runs exactly ONE post-HALT
                        // instruction before the interrupt is serviced. Defer the
                        // service one instruction for that shape (multi-block Requested).
                        // The post-HALT `ld (nn),a` write now lands before the ISR reads
                        // it, matching hardware.
                        // Not in Pan Docs, TCAGBD, or GBCTR; CGB HDMA-vs-interrupt
                        // HALT-exit ordering from test-ROM refs.
                        if self.registers.ime
                            && mmio.mmio.is_cgb()
                            && mmio.hdma_length() != 0
                        {
                            self.hdma_dma_due_defer_service = true;
                        }
                        // The Requested-held block is about to fire at unhalt. Arm the
                        // sub-block-cc consume so the next-line m0 edge that re-arms the
                        // following block is absorbed iff it lands inside this block's
                        // transfer span, deferring it one line.
                        mmio.arm_hdma_peraccess_consume();
                    }
                    memory::dma::HaltHdmaState::Low
                        if in_period_unhalt && mmio.hdma_is_enabled() =>
                    {
                        mmio.set_hdma_req()
                    }
                    memory::dma::HaltHdmaState::High if mmio.hdma_is_enabled() => {
                        // High-at-halt: the held block was already served and the
                        // unhalt does NOT reflag. Hardware also consumed the following
                        // line's m0 HDMA request during the halt; our unhalt cc lands
                        // ~1 dot before that m0, so without this the post-unhalt STAT
                        // fallback would fire a spurious extra block one line early.
                        mmio.arm_hdma_high_unhalt_consume();
                    }
                    _ => {}
                }
                // Late-hdma-vs-interrupt unhalt precedence: a Low-at-halt block NOT
                // in the HDMA period at unhalt (NOREFLAG) does not fire at unhalt;
                // its m0-edge falls within the following interrupt service, so the
                // block fires AFTER the PC pushes and copies the pushed return
                // address. Flag it so service_interrupt suppresses+reorders that fire
                // past the pushes. An in-period (REFLAG) block fires AT unhalt, before
                // the pushes, and stays synchronous. The straddle deciding REFLAG vs
                // NOREFLAG is ~1cc, and m0_time_master is only reliable on an
                // already-rendered line (LY>=1); on LY=0 after an LCD re-enable it
                // carries a ~6cc phase lag. Scope the defer to the timer IRQ, where
                // the straddle is sound; the mode-0-IRQ block keeps its REFLAG fire.
                let pending_is_timer =
                    pending_interrupt == Some(registers::InterruptFlag::Timer);
                let fires_before_pushes = mmio.hdma_unhalt_fires_before_pushes();
                let noreflag_deferred = pending_is_timer
                    && mmio.hdma_is_enabled()
                    && matches!(mmio.halt_hdma_state(), memory::dma::HaltHdmaState::Low)
                    && !fires_before_pushes;
                mmio.set_hdma_unhalt_noreflag_deferred(noreflag_deferred);
                // Engage the M-cycle fire suppression NOW (before the boundary
                // prefetch read ticks the bus): the deferred block's m0-edge arms
                // during the prefetch's M-cycle and would otherwise fire there,
                // ahead of the interrupt's pushes. Held here, it is fired post-push
                // by `service_interrupt`.
                if noreflag_deferred {
                    mmio.set_hdma_mcycle_fire_suppressed(true);
                }
                mmio.set_halt_hdma_state(memory::dma::HaltHdmaState::Low);
                // Unhalt-cc / LY phase fix. A Requested-held HDMA block (flagged at
                // halt entry) runs its dma() DURING the halt period on hardware
                // (mid-halt, at ~mode-0 time; the unhalt NOP resumes after it). Deferring
                // the whole transfer until after the HALT-bug double-execute resume
                // would land every post-unhalt FF44/PC read one HDMA block (36 SS /
                // 68 DS cc) too early. Fire the held block NOW and tick its transfer
                // stall inline so the prefetched resume byte executes at the
                // post-transfer cc.
                //
                // Gated to:
                //  - hdma_length() == 0 (block COMPLETES the transfer): a multi-block
                //    transfer relies on the per-dot firing path for its second-block
                //    period re-arm and FF55 readback; firing here desyncs that.
                //  - !ime (IME-off double-execute resume): with IME on, the unhalt
                //    instead services an interrupt, which accounts the wakeup cc
                //    through service_interrupt, where this inline shift would
                //    double-count.
                if was_requested
                    && !self.registers.ime
                    && mmio.hdma_length() == 0
                    && mmio.hdma_req_pending()
                    && !mmio.hdma_mcycle_fire_suppressed()
                {
                    mmio.fire_pending_hdma_mcycle();
                    let stall = mmio.take_dma_stall();
                    if stall > 0 {
                        mmio.tick_remaining(stall);
                    }
                }
            } else {
                // CPU is halted and no interrupt is pending: consume a batch of
                // idle cycles bounded so no IF bit can be raised inside the
                // batch (Bus::halted_idle_dots — timer/PPU event lower bounds;
                // serial/joypad activity disables batching). The world still
                // resolves dot-by-dot inside the caller's tick, so peripheral
                // behavior is byte-identical; only the poll cadence of this
                // no-op loop coarsens, and the batch never contains a wake.
                return mmio.halted_idle_dots();
            }
        }

        // Event-cc dispatch: an IRQ is serviceable only once the boundary access
        // cc has reached the cc its IF bit was raised, not merely once the IF bit
        // is flagged. `pending_interrupt` was sampled at this boundary; gate the
        // timer IRQ on the boundary access cc having reached its recorded fire cc,
        // re-resolving a lower-priority armed IRQ if the timer is not yet due.
        //
        // A non-halt, non-stop EI loop services the timer IRQ at the EARLY anchor
        // (pending_timer_fire_cc_ei, scheduled cc + IF_OFF) so the ISR/TAC re-write
        // lands on the exact divider phase. HALT and post-STOP streams keep the
        // LATE anchor (pending_timer_fire_cc, scheduled cc + CC_OFF), where their
        // read-after-overflow tests are byte-exact.
        // Base 1-cycle overflow->IF delay: TCAGBD §4.3. The EI early-anchor vs
        // HALT/STOP late-anchor split is a sub-cycle refinement from test-ROM refs,
        // not in Pan Docs or GBCTR.
        let boundary_access_cc = mmio.access_cc();
        let ei_ctx = !just_unhalted && !self.stopped;

        // EI-loop fast delivery: in a non-halt/non-stop loop with IME on, fire an
        // imminent overflow at the EARLY anchor (scheduled cc + IF_OFF) so the ensuing
        // service runs on the exact divider phase, ahead of the late per-dot
        // delivery. Re-sample the pending interrupt afterward.
        if ei_ctx && self.registers.ime
            && let Some(early) = mmio.next_timer_overflow_ei_cc()
                && boundary_access_cc >= early {
                    mmio.force_ei_timer_delivery(boundary_access_cc);
                    pending_interrupt = self.get_pending_interrupt(mmio);
                }

        let mut serviceable = pending_interrupt;
        if serviceable == Some(registers::InterruptFlag::Timer) {
            // The EI fast-dispatch services on the early anchor; otherwise the
            // gate is the late (+CC_OFF) anchor.
            let gate_cc = if ei_ctx {
                mmio.pending_timer_fire_cc_ei()
            } else {
                mmio.pending_timer_fire_cc()
            };
            if let Some(fire_cc) = gate_cc
                && boundary_access_cc < fire_cc
            {
                serviceable = self.get_pending_interrupt_excluding_timer(mmio);
            }
        }

        // Prefetch model: fetch the opcode at the instruction boundary FIRST
        // (the end-of-previous-instruction prefetch, charged at the boundary cc),
        // THEN decide whether to service. A serviced interrupt UNDOES the prefetch
        // (rewinds pc); otherwise execute consumes it with no re-fetch/re-charge.
        if !self.prefetched {
            self.opcode = mmio.fetch_opcode(self.registers.pc);
            self.registers.pc = self.registers.pc.wrapping_add(1);
            self.prefetched = true;
        }

        // CGB dma-due deferral: run the prefetched post-HALT instruction WITHOUT
        // servicing — the interrupt stays pending and dispatches next boundary.
        // Block1 fires on its own path during this instruction; only the post-HALT
        // `ld (nn),a` write's PPU mode check is biased forward by block1's transfer
        // span so it lands in the post-transfer mode-0 window.
        if just_unhalted && std::mem::take(&mut self.hdma_dma_due_defer_service) {
            let ds = mmio.mmio.is_double_speed_mode();
            let block_span: u64 = 0x10 * (2 + 2 * ds as u64) + 4;
            mmio.mmio.set_hdma_dma_due_write_cc_bias(block_span);
            let op = self.opcode;
            self.prefetched = false;
            if std::mem::take(&mut self.halt_bug_prefetch) {
                mmio.tick_opcode_fetch_mcycle();
            }
            cycles += self.execute(op, mmio);
            mmio.mmio.set_hdma_dma_due_write_cc_bias(0);
            self.apply_ime_delay();
            mmio.set_hdma_resume_lockstep_window(false);
            return cycles;
        }

        if self.registers.ime && serviceable.is_some() {
            return self.service_interrupt(mmio, just_unhalted);
        }

        // No interrupt is serviced this step after the unhalt (IME off, or the
        // timer gate downgraded it): release any unhalt-deferred HDMA
        // suppression and fire the held block now, on the post-prefetch cc
        // (there is no PC push to wait for).
        if mmio.hdma_mcycle_fire_suppressed() && mmio.hdma_unhalt_noreflag_deferred() {
            mmio.set_hdma_mcycle_fire_suppressed(false);
            mmio.fire_pending_hdma_mcycle();
            mmio.set_hdma_unhalt_noreflag_deferred(false);
        }

        let op = self.opcode;
        self.prefetched = false;
        // HALT-bug: the peeked opcode's fetch M-cycle was not ticked at the HALT
        // (unlike the normal fetch_opcode prefetch). Charge it now, BEFORE execute,
        // so the doubled instruction's operand read (which may sample a live IO
        // register — TIMA/DIV read as the `LD B,n` immediate) resolves one M-cycle
        // later, on the cc the register has ticked to.
        // Pan Docs: halt bug — https://gbdev.io/pandocs/halt.html
        if std::mem::take(&mut self.halt_bug_prefetch) {
            mmio.tick_opcode_fetch_mcycle();
        }
        cycles += self.execute(op, mmio);
        self.apply_ime_delay();
        mmio.set_hdma_resume_lockstep_window(false);
        cycles
    }

    fn service_interrupt(&mut self, bus: &mut crate::cpu::Bus, just_unhalted: bool) -> u32 {
        self.registers.ime = false;
        self.ime_enable_delay = 0;

        // Interrupt-vs-dma precedence: an HDMA block whose m0-edge latch is LATER
        // than the interrupt's service cc fires AFTER the PC pushes, so a pushed
        // return address is visible in the HDMA copy of that stack slot. A block
        // already latched before this service began is earlier and keeps firing at
        // its natural dot (before the pushes). So suppress the fire only when no
        // block is pending at service entry; a block latched during the service
        // M-cycles is held and fired explicitly after the pushes.
        //
        // A service resuming from HALT this same step is eligible ONLY when the
        // unhalt did NOT reflag the block (NOREFLAG): that block's m0-edge falls
        // within THIS service window and must fire after the pushes. A REFLAG block
        // fired at unhalt, before the pushes, and stays synchronous. For the
        // unhalt-deferred case the block may already be armed (hdma_req_pending)
        // from the boundary prefetch — its fire was held by the suppression engaged
        // at unhalt, so keep suppressing here; do NOT gate on !hdma_fire_pending.
        // The non-halt path keeps that gate: a block already pending there latched a
        // full instruction earlier and wins the race.
        let unhalt_defer = just_unhalted && bus.hdma_unhalt_noreflag_deferred();
        let suppress = if unhalt_defer {
            true
        } else {
            !just_unhalted && !bus.hdma_fire_pending()
        };
        bus.set_hdma_mcycle_fire_suppressed(suppress);
        // Boundary (pre-push) access cc for the late-hdma-vs-interrupt re-order
        // (see `reorder_late_hdma_after_pushes`): a greedy m0-edge HDMA fire that
        // raced this same M-cycle window read pre-push memory and must be re-run
        // post-push. Only the unhalt-free path is eligible (the halt path's block
        // is governed by the halt-HDMA state machine).
        let service_access_cc = bus.access_cc();

        // The boundary prefetch already read (and charged +4 for) the opcode this
        // interrupt discards. UNDO it: rewind pc and drop the prefetch flag
        // (re-fetched after the ISR returns). With the prefetch supplying one of the
        // 5 interrupt M-cycles, service ticks 2 internal (wait) + 2 pushes = 16cc;
        // prefetch 4 + 16 = the full 20cc / 5 M-cycles.
        // Pan Docs: Interrupt handling — https://gbdev.io/pandocs/Interrupts.html
        self.registers.pc = self.registers.pc.wrapping_sub(1);
        self.prefetched = false;
        // A HALT-bug prefetch that ends up serviced (IME=1 + pending: the bug peeked
        // the byte but the interrupt now undoes it, pc rewound to the HALT) must NOT
        // carry its deferred opcode-fetch M-cycle into the post-ISR resume — the HALT
        // re-runs and re-decides. Drop it here so the charge applies only when the
        // doubled opcode actually executes (IME-off HALT-bug).
        self.halt_bug_prefetch = false;
        // HALT-prefetch woken-PC push consume. The unconditional `pc -= 1` undo
        // above is correct only when the unhalt did a pc-advancing boundary fetch.
        // For the CGB+Timer phase-1 wakeup (HALT left a NON-advancing prefetch peek,
        // pc never advanced) the undo over-subtracts by one. Re-add the +1 here,
        // BEFORE both pushes (so a page-crossing carry propagates to the high byte),
        // reproducing the conditional prefetch undo. The flag is consumed below so it
        // biases exactly one interrupt service.
        if just_unhalted && bus.mmio.timer_push_phase() == 1
        {
            self.registers.pc = self.registers.pc.wrapping_add(1);
        }
        bus.mmio.set_timer_push_phase(0);
        bus.internal_cycle();
        bus.internal_cycle();

        self.registers.sp = self.registers.sp.wrapping_sub(1);
        bus.write(self.registers.sp, (self.registers.pc >> 8) as u8);

        // The vector is latched from IE&IF *after* the high-byte push: if that
        // push wrote over IE (SP near 0xFFFF) the pending set can change, and
        // if nothing is pending the interrupt is cancelled (vector 0x0000).
        // Known behavior (not in Pan Docs/TCAGBD/GBCTR): pinned by Gekkio's mooneye
        // `interrupt-cancellation` test.
        let flag = self.get_pending_interrupt(bus);

        // The low-byte push ACKs the IF bit partway through its M-cycle: each source
        // is advanced to a per-source sub-cc offset (serial +3+cgb, timer +2+cgb,
        // lcd +2), flagging any IRQ that completes by that offset, THEN only the
        // dispatched bit is cleared. A source completing AFTER its offset re-flags IF
        // and survives for the ISR to read; one completing by the offset is
        // flagged-then-cleared and reads back gone. The offset is the DMG-vs-CGB
        // discriminator on the boundary cases. Done inside the push M-cycle so the
        // clear lands at the exact sub-dot. Skip while OAM DMA is active (the
        // split-push bypasses the DMA-conflict redirection in bus.write).
        // Base IF-clear-on-dispatch: TCAGBD §4.9 / Pan Docs. The sub-M-cycle
        // per-source ACK offset is not in Pan Docs, TCAGBD, or GBCTR (test-ROM refs).
        let split_ack = matches!(
            flag,
            Some(registers::InterruptFlag::Lcd)
                | Some(registers::InterruptFlag::Serial)
                | Some(registers::InterruptFlag::Timer)
        ) && !bus.oam_dma_active();
        self.registers.sp = self.registers.sp.wrapping_sub(1);
        if split_ack {
            let bit = flag.map(|f| f as u8).unwrap_or(0);
            bus.interrupt_low_push_ack(self.registers.sp, (self.registers.pc & 0x00FF) as u8, bit);
        } else {
            bus.write(self.registers.sp, (self.registers.pc & 0x00FF) as u8);
        }

        // Pushes complete: re-enable the M-cycle fire and fire any HDMA block
        // latched during the service, so it reads memory as of the post-push cc.
        bus.set_hdma_mcycle_fire_suppressed(false);
        bus.fire_pending_hdma_mcycle();
        bus.set_hdma_unhalt_noreflag_deferred(false);
        // Late-hdma-vs-interrupt: if a greedy m0-edge block fired within this
        // service's M-cycle window (the interrupt won the mode-0 time-vs-the minimum-interrupt-time
        // race) re-run it now so its source reads see the just-pushed PC.
        if !just_unhalted {
            bus.reorder_late_hdma_after_pushes(service_access_cc);
        }

        self.registers.pc = match flag {
            Some(registers::InterruptFlag::VBlank) => 0x40,
            Some(registers::InterruptFlag::Lcd) => 0x48,
            Some(registers::InterruptFlag::Timer) => 0x50,
            Some(registers::InterruptFlag::Serial) => 0x58,
            Some(registers::InterruptFlag::Joypad) => 0x60,
            None => 0x0000,
        };
        if let Some(flag) = flag {
            // The LCD/Serial/Timer vectors were already ACKed mid-push (split_ack);
            // clearing again here would wipe a same-window re-fire that must survive.
            // When the split was skipped (OAM DMA active) or the vector is
            // VBlank/Joypad, clear here as before.
            if !split_ack {
                self.set_interrupt_flag(flag, false, bus);
            }
            // Once the timer IRQ is dispatched, drop its recorded fire cc so the
            // next period's fire is tracked fresh.
            if flag == registers::InterruptFlag::Timer {
                bus.clear_timer_fire_cc();
            }
        }
        20
    }

    fn apply_ime_delay(&mut self) {
        if self.ime_enable_delay == 0 {
            return;
        }

        self.ime_enable_delay -= 1;
        if self.ime_enable_delay == 0 {
            self.registers.ime = true;
        }
    }

    pub(crate) fn set_interrupt_flag(&mut self, flag: registers::InterruptFlag, value: bool, mmio: &mut memory::mmio::Mmio) {
        if value {
            mmio.write(registers::INTERRUPT_FLAG, mmio.read(registers::INTERRUPT_FLAG) | flag as u8);
        } else {
            mmio.write(registers::INTERRUPT_FLAG, mmio.read(registers::INTERRUPT_FLAG) & !(flag as u8));
        }
    }

    /// Highest-priority armed interrupt (IE & IF), skipping any source whose bit
    /// is set in `exclude`. IE and IF are each read once for the whole walk;
    /// `Mmio::read` takes `&self`, so no read in the chain can observe a value a
    /// later one would not.
    fn pending_interrupt(&self, mmio: &memory::mmio::Mmio, exclude: u8) -> Option<registers::InterruptFlag> {
        let armed = mmio.read(registers::INTERRUPT_ENABLE) & mmio.read(registers::INTERRUPT_FLAG) & !exclude;
        INTERRUPT_PRIORITY.into_iter().find(|flag| armed & (*flag as u8) != 0)
    }

    fn get_pending_interrupt(&self, mmio: &memory::mmio::Mmio) -> Option<registers::InterruptFlag> {
        self.pending_interrupt(mmio, 0)
    }

    /// Like `get_pending_interrupt` but skips the Timer source. Used by the
    /// event-cc dispatch gate when the timer IRQ's fire cc has not yet been
    /// reached, so a lower-priority armed interrupt can still dispatch.
    fn get_pending_interrupt_excluding_timer(&self, mmio: &memory::mmio::Mmio) -> Option<registers::InterruptFlag> {
        self.pending_interrupt(mmio, registers::InterruptFlag::Timer as u8)
    }

    fn execute(&mut self, opcode: u8, mmio: &mut crate::cpu::Bus) -> u32 {
        match opcode {
            0x00 => opcodes::nop(self, mmio),
            0x01 => opcodes::ld_bc_imm(self, mmio),
            0x02 => opcodes::ld_memory_bc_a(self, mmio),
            0x03 => opcodes::inc_bc(self, mmio),
            0x04 => opcodes::inc_b(self, mmio),
            0x05 => opcodes::dec_b(self, mmio),
            0x06 => opcodes::ld_b_imm(self, mmio),
            0x07 => opcodes::rlca(self, mmio),
            0x08 => opcodes::ld_memory_imm_16_sp(self, mmio),
            0x09 => opcodes::add_hl_bc(self, mmio),
            0x0A => opcodes::ld_a_memory_bc(self, mmio),
            0x0B => opcodes::dec_bc(self, mmio),
            0x0C => opcodes::inc_c(self, mmio),
            0x0D => opcodes::dec_c(self, mmio),
            0x0E => opcodes::ld_c_imm(self, mmio),
            0x0F => opcodes::rrca(self, mmio),
            0x10 => opcodes::stop(self, mmio),
            0x11 => opcodes::ld_de_imm(self, mmio),
            0x12 => opcodes::ld_memory_de_a(self, mmio),
            0x13 => opcodes::inc_de(self, mmio),
            0x14 => opcodes::inc_d(self, mmio),
            0x15 => opcodes::dec_d(self, mmio),
            0x16 => opcodes::ld_d_imm(self, mmio),
            0x17 => opcodes::rla(self, mmio),
            0x18 => opcodes::jr_imm(self, mmio),
            0x19 => opcodes::add_hl_de(self, mmio),
            0x1A => opcodes::ld_a_memory_de(self, mmio),
            0x1B => opcodes::dec_de(self, mmio),
            0x1C => opcodes::inc_e(self, mmio),
            0x1D => opcodes::dec_e(self, mmio),
            0x1E => opcodes::ld_e_imm(self, mmio),
            0x1F => opcodes::rra(self, mmio),
            0x20 => opcodes::jr_nz_imm(self, mmio),
            0x21 => opcodes::ld_hl_imm(self, mmio),
            0x22 => opcodes::ld_memory_hl_inc_a(self, mmio),
            0x23 => opcodes::inc_hl(self, mmio),
            0x24 => opcodes::inc_h(self, mmio),
            0x25 => opcodes::dec_h(self, mmio),
            0x26 => opcodes::ld_h_imm(self, mmio),
            0x27 => opcodes::daa(self, mmio),
            0x28 => opcodes::jr_z_imm(self, mmio),
            0x29 => opcodes::add_hl_hl(self, mmio),
            0x2A => opcodes::ld_a_memory_hl_inc(self, mmio),
            0x2B => opcodes::dec_hl(self, mmio),
            0x2C => opcodes::inc_l(self, mmio),
            0x2D => opcodes::dec_l(self, mmio),
            0x2E => opcodes::ld_l_imm(self, mmio),
            0x2F => opcodes::cpl(self, mmio),
            0x30 => opcodes::jr_nc_imm(self, mmio),
            0x31 => opcodes::ld_sp_imm(self, mmio),
            0x32 => opcodes::ld_memory_hl_dec_a(self, mmio),
            0x33 => opcodes::inc_sp(self, mmio),
            0x34 => opcodes::inc_memory_hl(self, mmio),
            0x35 => opcodes::dec_memory_hl(self, mmio),
            0x36 => opcodes::ld_memory_hl_imm(self, mmio),
            0x37 => opcodes::scf(self, mmio),
            0x38 => opcodes::jr_c_imm(self, mmio),
            0x39 => opcodes::add_hl_sp(self, mmio),
            0x3A => opcodes::ld_a_memory_hl_dec(self, mmio),
            0x3B => opcodes::dec_sp(self, mmio),
            0x3C => opcodes::inc_a(self, mmio),
            0x3D => opcodes::dec_a(self, mmio),
            0x3E => opcodes::ld_a_imm(self, mmio),
            0x3F => opcodes::ccf(self, mmio),
            0x40 => opcodes::ld_b_b(self, mmio),
            0x41 => opcodes::ld_b_c(self, mmio),
            0x42 => opcodes::ld_b_d(self, mmio),
            0x43 => opcodes::ld_b_e(self, mmio),
            0x44 => opcodes::ld_b_h(self, mmio),
            0x45 => opcodes::ld_b_l(self, mmio),
            0x46 => opcodes::ld_b_memory_hl(self, mmio),
            0x47 => opcodes::ld_b_a(self, mmio),
            0x48 => opcodes::ld_c_b(self, mmio),
            0x49 => opcodes::ld_c_c(self, mmio),
            0x4A => opcodes::ld_c_d(self, mmio),
            0x4B => opcodes::ld_c_e(self, mmio),
            0x4C => opcodes::ld_c_h(self, mmio),
            0x4D => opcodes::ld_c_l(self, mmio),
            0x4E => opcodes::ld_c_memory_hl(self, mmio),
            0x4F => opcodes::ld_c_a(self, mmio),
            0x50 => opcodes::ld_d_b(self, mmio),
            0x51 => opcodes::ld_d_c(self, mmio),
            0x52 => opcodes::ld_d_d(self, mmio),
            0x53 => opcodes::ld_d_e(self, mmio),
            0x54 => opcodes::ld_d_h(self, mmio),
            0x55 => opcodes::ld_d_l(self, mmio),
            0x56 => opcodes::ld_d_memory_hl(self, mmio),
            0x57 => opcodes::ld_d_a(self, mmio),
            0x58 => opcodes::ld_e_b(self, mmio),
            0x59 => opcodes::ld_e_c(self, mmio),
            0x5A => opcodes::ld_e_d(self, mmio),
            0x5B => opcodes::ld_e_e(self, mmio),
            0x5C => opcodes::ld_e_h(self, mmio),
            0x5D => opcodes::ld_e_l(self, mmio),
            0x5E => opcodes::ld_e_memory_hl(self, mmio),
            0x5F => opcodes::ld_e_a(self, mmio),
            0x60 => opcodes::ld_h_b(self, mmio),
            0x61 => opcodes::ld_h_c(self, mmio),
            0x62 => opcodes::ld_h_d(self, mmio),
            0x63 => opcodes::ld_h_e(self, mmio),
            0x64 => opcodes::ld_h_h(self, mmio),
            0x65 => opcodes::ld_h_l(self, mmio),
            0x66 => opcodes::ld_h_memory_hl(self, mmio),
            0x67 => opcodes::ld_h_a(self, mmio),
            0x68 => opcodes::ld_l_b(self, mmio),
            0x69 => opcodes::ld_l_c(self, mmio),
            0x6A => opcodes::ld_l_d(self, mmio),
            0x6B => opcodes::ld_l_e(self, mmio),
            0x6C => opcodes::ld_l_h(self, mmio),
            0x6D => opcodes::ld_l_l(self, mmio),
            0x6E => opcodes::ld_l_memory_hl(self, mmio),
            0x6F => opcodes::ld_l_a(self, mmio),
            0x70 => opcodes::ld_memory_hl_b(self, mmio),
            0x71 => opcodes::ld_memory_hl_c(self, mmio),
            0x72 => opcodes::ld_memory_hl_d(self, mmio),
            0x73 => opcodes::ld_memory_hl_e(self, mmio),
            0x74 => opcodes::ld_memory_hl_h(self, mmio),
            0x75 => opcodes::ld_memory_hl_l(self, mmio),
            0x76 => opcodes::halt(self, mmio),
            0x77 => opcodes::ld_memory_hl_a(self, mmio),
            0x78 => opcodes::ld_a_b(self, mmio),
            0x79 => opcodes::ld_a_c(self, mmio),
            0x7A => opcodes::ld_a_d(self, mmio),
            0x7B => opcodes::ld_a_e(self, mmio),
            0x7C => opcodes::ld_a_h(self, mmio),
            0x7D => opcodes::ld_a_l(self, mmio),
            0x7E => opcodes::ld_a_memory_hl(self, mmio),
            0x7F => opcodes::ld_a_a(self, mmio),
            0x80 => opcodes::add_b(self, mmio),
            0x81 => opcodes::add_c(self, mmio),
            0x82 => opcodes::add_d(self, mmio),
            0x83 => opcodes::add_e(self, mmio),
            0x84 => opcodes::add_h(self, mmio),
            0x85 => opcodes::add_l(self, mmio),
            0x86 => opcodes::add_memory_hl(self, mmio),
            0x87 => opcodes::add_a(self, mmio),
            0x88 => opcodes::adc_b(self, mmio),
            0x89 => opcodes::adc_c(self, mmio),
            0x8A => opcodes::adc_d(self, mmio),
            0x8B => opcodes::adc_e(self, mmio),
            0x8C => opcodes::adc_h(self, mmio),
            0x8D => opcodes::adc_l(self, mmio),
            0x8E => opcodes::adc_a_memory_hl(self, mmio),
            0x8F => opcodes::adc_a(self, mmio),
            0x90 => opcodes::sub_b(self, mmio),
            0x91 => opcodes::sub_c(self, mmio),
            0x92 => opcodes::sub_d(self, mmio),
            0x93 => opcodes::sub_e(self, mmio),
            0x94 => opcodes::sub_h(self, mmio),
            0x95 => opcodes::sub_l(self, mmio),
            0x96 => opcodes::sub_memory_hl(self, mmio),
            0x97 => opcodes::sub_a(self, mmio),
            0x98 => opcodes::sbc_a_b(self, mmio),
            0x99 => opcodes::sbc_a_c(self, mmio),
            0x9A => opcodes::sbc_a_d(self, mmio),
            0x9B => opcodes::sbc_a_e(self, mmio),
            0x9C => opcodes::sbc_a_h(self, mmio),
            0x9D => opcodes::sbc_a_l(self, mmio),
            0x9E => opcodes::sbc_a_memory_hl(self, mmio),
            0x9F => opcodes::sbc_a_a(self, mmio),
            0xA0 => opcodes::and_b(self, mmio),
            0xA1 => opcodes::and_c(self, mmio),
            0xA2 => opcodes::and_d(self, mmio),
            0xA3 => opcodes::and_e(self, mmio),
            0xA4 => opcodes::and_h(self, mmio),
            0xA5 => opcodes::and_l(self, mmio),
            0xA6 => opcodes::and_memory_hl(self, mmio),
            0xA7 => opcodes::and_a(self, mmio),
            0xA8 => opcodes::xor_b(self, mmio),
            0xA9 => opcodes::xor_c(self, mmio),
            0xAA => opcodes::xor_d(self, mmio),
            0xAB => opcodes::xor_e(self, mmio),
            0xAC => opcodes::xor_h(self, mmio),
            0xAD => opcodes::xor_l(self, mmio),
            0xAE => opcodes::xor_memory_hl(self, mmio),
            0xAF => opcodes::xor_a(self, mmio),
            0xB0 => opcodes::or_b(self, mmio),
            0xB1 => opcodes::or_c(self, mmio),
            0xB2 => opcodes::or_d(self, mmio),
            0xB3 => opcodes::or_e(self, mmio),
            0xB4 => opcodes::or_h(self, mmio),
            0xB5 => opcodes::or_l(self, mmio),
            0xB6 => opcodes::or_memory_hl(self, mmio),
            0xB7 => opcodes::or_a(self, mmio),
            0xB8 => opcodes::cp_b(self, mmio),
            0xB9 => opcodes::cp_c(self, mmio),
            0xBA => opcodes::cp_d(self, mmio),
            0xBB => opcodes::cp_e(self, mmio),
            0xBC => opcodes::cp_h(self, mmio),
            0xBD => opcodes::cp_l(self, mmio),
            0xBE => opcodes::cp_memory_hl(self, mmio),
            0xBF => opcodes::cp_a(self, mmio),
            0xC0 => opcodes::ret_nz(self, mmio),
            0xC1 => opcodes::pop_bc(self, mmio),
            0xC2 => opcodes::jp_nz_imm(self, mmio),
            0xC3 => opcodes::jp_imm(self, mmio),
            0xC4 => opcodes::call_nz_imm(self, mmio),
            0xC5 => opcodes::push_bc(self, mmio),
            0xC6 => opcodes::add_imm(self, mmio),
            0xC7 => opcodes::rst_00(self, mmio),
            0xC8 => opcodes::ret_z(self, mmio),
            0xC9 => opcodes::ret(self, mmio),
            0xCA => opcodes::jp_z_imm(self, mmio),
            0xCB => self.execute_cb(mmio),
            0xCC => opcodes::call_z_imm(self, mmio),
            0xCD => opcodes::call_imm(self, mmio),
            0xCE => opcodes::adc_imm(self, mmio),
            0xCF => opcodes::rst_08(self, mmio),
            0xD0 => opcodes::ret_nc(self, mmio),
            0xD1 => opcodes::pop_de(self, mmio),
            0xD2 => opcodes::jp_nc_imm(self, mmio),
            0xD3 => opcodes::undefined(self, mmio),
            0xD4 => opcodes::call_nc_imm(self, mmio),
            0xD5 => opcodes::push_de(self, mmio),
            0xD6 => opcodes::sub_imm(self, mmio),
            0xD7 => opcodes::rst_10(self, mmio),
            0xD8 => opcodes::ret_c(self, mmio),
            0xD9 => opcodes::reti(self, mmio),
            0xDA => opcodes::jp_c_imm(self, mmio),
            0xDB => opcodes::undefined(self, mmio),
            0xDC => opcodes::call_c_imm(self, mmio),
            0xDD => opcodes::undefined(self, mmio),
            0xDE => opcodes::sbc_a_imm(self, mmio),
            0xDF => opcodes::rst_18(self, mmio),
            0xE0 => opcodes::ldh_memory_imm_a(self, mmio),
            0xE1 => opcodes::pop_hl(self, mmio),
            0xE2 => opcodes::ld_memory_c_a(self, mmio),
            0xE3 => opcodes::undefined(self, mmio),
            0xE4 => opcodes::undefined(self, mmio),
            0xE5 => opcodes::push_hl(self, mmio),
            0xE6 => opcodes::and_imm(self, mmio),
            0xE7 => opcodes::rst_20(self, mmio),
            0xE8 => opcodes::add_sp_imm(self, mmio),
            0xE9 => opcodes::jp_hl(self, mmio),
            0xEA => opcodes::ld_memory_imm_a_16(self, mmio),
            0xEB => opcodes::undefined(self, mmio),
            0xEC => opcodes::undefined(self, mmio),
            0xED => opcodes::undefined(self, mmio),
            0xEE => opcodes::xor_imm(self, mmio),
            0xEF => opcodes::rst_28(self, mmio),
            0xF0 => opcodes::ldh_a_memory_imm(self, mmio),
            0xF2 => opcodes::ld_a_memory_c(self, mmio),
            0xF1 => opcodes::pop_af(self, mmio),
            0xF3 => opcodes::di(self, mmio),
            0xF4 => opcodes::undefined(self, mmio),
            0xF5 => opcodes::push_af(self, mmio),
            0xF6 => opcodes::or_imm(self, mmio),
            0xF7 => opcodes::rst_30(self, mmio),
            0xF8 => opcodes::ld_hl_sp_imm(self, mmio),
            0xF9 => opcodes::ld_sp_hl(self, mmio),
            0xFA => opcodes::ld_a_memory_imm_16(self, mmio),
            0xFB => opcodes::ei(self, mmio),
            0xFC => opcodes::undefined(self, mmio),
            0xFD => opcodes::undefined(self, mmio),
            0xFE => opcodes::cp_imm(self, mmio),
            0xFF => opcodes::rst_38(self, mmio),
        }
    }

    fn execute_cb(&mut self, mmio: &mut crate::cpu::Bus) -> u32 {
        let opcode = mmio.read(self.registers.pc);
        self.registers.pc = self.registers.pc.wrapping_add(1);
        match opcode {
            0x00 => opcodes::rlc_b(self, mmio),
            0x01 => opcodes::rlc_c(self, mmio),
            0x02 => opcodes::rlc_d(self, mmio),
            0x03 => opcodes::rlc_e(self, mmio),
            0x04 => opcodes::rlc_h(self, mmio),
            0x05 => opcodes::rlc_l(self, mmio),
            0x06 => opcodes::rlc_hl(self, mmio),
            0x07 => opcodes::rlc_a(self, mmio),
            0x08 => opcodes::rrc_b(self, mmio),
            0x09 => opcodes::rrc_c(self, mmio),
            0x0A => opcodes::rrc_d(self, mmio),
            0x0B => opcodes::rrc_e(self, mmio),
            0x0C => opcodes::rrc_h(self, mmio),
            0x0D => opcodes::rrc_l(self, mmio),
            0x0E => opcodes::rrc_hl(self, mmio),
            0x0F => opcodes::rrc_a(self, mmio),
            0x10 => opcodes::rl_b(self, mmio),
            0x11 => opcodes::rl_c(self, mmio),
            0x12 => opcodes::rl_d(self, mmio),
            0x13 => opcodes::rl_e(self, mmio),
            0x14 => opcodes::rl_h(self, mmio),
            0x15 => opcodes::rl_l(self, mmio),
            0x16 => opcodes::rl_hl(self, mmio),
            0x17 => opcodes::rl_a(self, mmio),
            0x18 => opcodes::rr_b(self, mmio),
            0x19 => opcodes::rr_c(self, mmio),
            0x1A => opcodes::rr_d(self, mmio),
            0x1B => opcodes::rr_e(self, mmio),
            0x1C => opcodes::rr_h(self, mmio),
            0x1D => opcodes::rr_l(self, mmio),
            0x1E => opcodes::rr_hl(self, mmio),
            0x1F => opcodes::rr_a(self, mmio),
            0x20 => opcodes::sla_b(self, mmio),
            0x21 => opcodes::sla_c(self, mmio),
            0x22 => opcodes::sla_d(self, mmio),
            0x23 => opcodes::sla_e(self, mmio),
            0x24 => opcodes::sla_h(self, mmio),
            0x25 => opcodes::sla_l(self, mmio),
            0x26 => opcodes::sla_hl(self, mmio),
            0x27 => opcodes::sla_a(self, mmio),
            0x28 => opcodes::sra_b(self, mmio),
            0x29 => opcodes::sra_c(self, mmio),
            0x2A => opcodes::sra_d(self, mmio),
            0x2B => opcodes::sra_e(self, mmio),
            0x2C => opcodes::sra_h(self, mmio),
            0x2D => opcodes::sra_l(self, mmio),
            0x2E => opcodes::sra_hl(self, mmio),
            0x2F => opcodes::sra_a(self, mmio),
            0x30 => opcodes::swap_b(self, mmio),
            0x31 => opcodes::swap_c(self, mmio),
            0x32 => opcodes::swap_d(self, mmio),
            0x33 => opcodes::swap_e(self, mmio),
            0x34 => opcodes::swap_h(self, mmio),
            0x35 => opcodes::swap_l(self, mmio),
            0x36 => opcodes::swap_hl(self, mmio),
            0x37 => opcodes::swap_a(self, mmio),
            0x38 => opcodes::srl_b(self, mmio),
            0x39 => opcodes::srl_c(self, mmio),
            0x3A => opcodes::srl_d(self, mmio),
            0x3B => opcodes::srl_e(self, mmio),
            0x3C => opcodes::srl_h(self, mmio),
            0x3D => opcodes::srl_l(self, mmio),
            0x3E => opcodes::srl_hl(self, mmio),
            0x3F => opcodes::srl_a(self, mmio),
            0x40 => opcodes::bit_0_b(self, mmio),
            0x41 => opcodes::bit_0_c(self, mmio),
            0x42 => opcodes::bit_0_d(self, mmio),
            0x43 => opcodes::bit_0_e(self, mmio),
            0x44 => opcodes::bit_0_h(self, mmio),
            0x45 => opcodes::bit_0_l(self, mmio),
            0x46 => opcodes::bit_0_hl(self, mmio),
            0x47 => opcodes::bit_0_a(self, mmio),
            0x48 => opcodes::bit_1_b(self, mmio),
            0x49 => opcodes::bit_1_c(self, mmio),
            0x4A => opcodes::bit_1_d(self, mmio),
            0x4B => opcodes::bit_1_e(self, mmio),
            0x4C => opcodes::bit_1_h(self, mmio),
            0x4D => opcodes::bit_1_l(self, mmio),
            0x4E => opcodes::bit_1_hl(self, mmio),
            0x4F => opcodes::bit_1_a(self, mmio),
            0x50 => opcodes::bit_2_b(self, mmio),
            0x51 => opcodes::bit_2_c(self, mmio),
            0x52 => opcodes::bit_2_d(self, mmio),
            0x53 => opcodes::bit_2_e(self, mmio),
            0x54 => opcodes::bit_2_h(self, mmio),
            0x55 => opcodes::bit_2_l(self, mmio),
            0x56 => opcodes::bit_2_hl(self, mmio),
            0x57 => opcodes::bit_2_a(self, mmio),
            0x58 => opcodes::bit_3_b(self, mmio),
            0x59 => opcodes::bit_3_c(self, mmio),
            0x5A => opcodes::bit_3_d(self, mmio),
            0x5B => opcodes::bit_3_e(self, mmio),
            0x5C => opcodes::bit_3_h(self, mmio),
            0x5D => opcodes::bit_3_l(self, mmio),
            0x5E => opcodes::bit_3_hl(self, mmio),
            0x5F => opcodes::bit_3_a(self, mmio),
            0x60 => opcodes::bit_4_b(self, mmio),
            0x61 => opcodes::bit_4_c(self, mmio),
            0x62 => opcodes::bit_4_d(self, mmio),
            0x63 => opcodes::bit_4_e(self, mmio),
            0x64 => opcodes::bit_4_h(self, mmio),
            0x65 => opcodes::bit_4_l(self, mmio),
            0x66 => opcodes::bit_4_hl(self, mmio),
            0x67 => opcodes::bit_4_a(self, mmio),
            0x68 => opcodes::bit_5_b(self, mmio),
            0x69 => opcodes::bit_5_c(self, mmio),
            0x6A => opcodes::bit_5_d(self, mmio),
            0x6B => opcodes::bit_5_e(self, mmio),
            0x6C => opcodes::bit_5_h(self, mmio),
            0x6D => opcodes::bit_5_l(self, mmio),
            0x6E => opcodes::bit_5_hl(self, mmio),
            0x6F => opcodes::bit_5_a(self, mmio),
            0x70 => opcodes::bit_6_b(self, mmio),
            0x71 => opcodes::bit_6_c(self, mmio),
            0x72 => opcodes::bit_6_d(self, mmio),
            0x73 => opcodes::bit_6_e(self, mmio),
            0x74 => opcodes::bit_6_h(self, mmio),
            0x75 => opcodes::bit_6_l(self, mmio),
            0x76 => opcodes::bit_6_hl(self, mmio),
            0x77 => opcodes::bit_6_a(self, mmio),
            0x78 => opcodes::bit_7_b(self, mmio),
            0x79 => opcodes::bit_7_c(self, mmio),
            0x7A => opcodes::bit_7_d(self, mmio),
            0x7B => opcodes::bit_7_e(self, mmio),
            0x7C => opcodes::bit_7_h(self, mmio),
            0x7D => opcodes::bit_7_l(self, mmio),
            0x7E => opcodes::bit_7_hl(self, mmio),
            0x7F => opcodes::bit_7_a(self, mmio),
            0x80 => opcodes::res_0_b(self, mmio),
            0x81 => opcodes::res_0_c(self, mmio),
            0x82 => opcodes::res_0_d(self, mmio),
            0x83 => opcodes::res_0_e(self, mmio),
            0x84 => opcodes::res_0_h(self, mmio),
            0x85 => opcodes::res_0_l(self, mmio),
            0x86 => opcodes::res_0_hl(self, mmio),
            0x87 => opcodes::res_0_a(self, mmio),
            0x88 => opcodes::res_1_b(self, mmio),
            0x89 => opcodes::res_1_c(self, mmio),
            0x8A => opcodes::res_1_d(self, mmio),
            0x8B => opcodes::res_1_e(self, mmio),
            0x8C => opcodes::res_1_h(self, mmio),
            0x8D => opcodes::res_1_l(self, mmio),
            0x8E => opcodes::res_1_hl(self, mmio),
            0x8F => opcodes::res_1_a(self, mmio),
            0x90 => opcodes::res_2_b(self, mmio),
            0x91 => opcodes::res_2_c(self, mmio),
            0x92 => opcodes::res_2_d(self, mmio),
            0x93 => opcodes::res_2_e(self, mmio),
            0x94 => opcodes::res_2_h(self, mmio),
            0x95 => opcodes::res_2_l(self, mmio),
            0x96 => opcodes::res_2_hl(self, mmio),
            0x97 => opcodes::res_2_a(self, mmio),
            0x98 => opcodes::res_3_b(self, mmio),
            0x99 => opcodes::res_3_c(self, mmio),
            0x9A => opcodes::res_3_d(self, mmio),
            0x9B => opcodes::res_3_e(self, mmio),
            0x9C => opcodes::res_3_h(self, mmio),
            0x9D => opcodes::res_3_l(self, mmio),
            0x9E => opcodes::res_3_hl(self, mmio),
            0x9F => opcodes::res_3_a(self, mmio),
            0xA0 => opcodes::res_4_b(self, mmio),
            0xA1 => opcodes::res_4_c(self, mmio),
            0xA2 => opcodes::res_4_d(self, mmio),
            0xA3 => opcodes::res_4_e(self, mmio),
            0xA4 => opcodes::res_4_h(self, mmio),
            0xA5 => opcodes::res_4_l(self, mmio),
            0xA6 => opcodes::res_4_hl(self, mmio),
            0xA7 => opcodes::res_4_a(self, mmio),
            0xA8 => opcodes::res_5_b(self, mmio),
            0xA9 => opcodes::res_5_c(self, mmio),
            0xAA => opcodes::res_5_d(self, mmio),
            0xAB => opcodes::res_5_e(self, mmio),
            0xAC => opcodes::res_5_h(self, mmio),
            0xAD => opcodes::res_5_l(self, mmio),
            0xAE => opcodes::res_5_hl(self, mmio),
            0xAF => opcodes::res_5_a(self, mmio),
            0xB0 => opcodes::res_6_b(self, mmio),
            0xB1 => opcodes::res_6_c(self, mmio),
            0xB2 => opcodes::res_6_d(self, mmio),
            0xB3 => opcodes::res_6_e(self, mmio),
            0xB4 => opcodes::res_6_h(self, mmio),
            0xB5 => opcodes::res_6_l(self, mmio),
            0xB6 => opcodes::res_6_hl(self, mmio),
            0xB7 => opcodes::res_6_a(self, mmio),
            0xB8 => opcodes::res_7_b(self, mmio),
            0xB9 => opcodes::res_7_c(self, mmio),
            0xBA => opcodes::res_7_d(self, mmio),
            0xBB => opcodes::res_7_e(self, mmio),
            0xBC => opcodes::res_7_h(self, mmio),
            0xBD => opcodes::res_7_l(self, mmio),
            0xBE => opcodes::res_7_hl(self, mmio),
            0xBF => opcodes::res_7_a(self, mmio),
            0xC0 => opcodes::set_0_b(self, mmio),
            0xC1 => opcodes::set_0_c(self, mmio),
            0xC2 => opcodes::set_0_d(self, mmio),
            0xC3 => opcodes::set_0_e(self, mmio),
            0xC4 => opcodes::set_0_h(self, mmio),
            0xC5 => opcodes::set_0_l(self, mmio),
            0xC6 => opcodes::set_0_hl(self, mmio),
            0xC7 => opcodes::set_0_a(self, mmio),
            0xC8 => opcodes::set_1_b(self, mmio),
            0xC9 => opcodes::set_1_c(self, mmio),
            0xCA => opcodes::set_1_d(self, mmio),
            0xCB => opcodes::set_1_e(self, mmio),
            0xCC => opcodes::set_1_h(self, mmio),
            0xCD => opcodes::set_1_l(self, mmio),
            0xCE => opcodes::set_1_hl(self, mmio),
            0xCF => opcodes::set_1_a(self, mmio),
            0xD0 => opcodes::set_2_b(self, mmio),
            0xD1 => opcodes::set_2_c(self, mmio),
            0xD2 => opcodes::set_2_d(self, mmio),
            0xD3 => opcodes::set_2_e(self, mmio),
            0xD4 => opcodes::set_2_h(self, mmio),
            0xD5 => opcodes::set_2_l(self, mmio),
            0xD6 => opcodes::set_2_hl(self, mmio),
            0xD7 => opcodes::set_2_a(self, mmio),
            0xD8 => opcodes::set_3_b(self, mmio),
            0xD9 => opcodes::set_3_c(self, mmio),
            0xDA => opcodes::set_3_d(self, mmio),
            0xDB => opcodes::set_3_e(self, mmio),
            0xDC => opcodes::set_3_h(self, mmio),
            0xDD => opcodes::set_3_l(self, mmio),
            0xDE => opcodes::set_3_hl(self, mmio),
            0xDF => opcodes::set_3_a(self, mmio),
            0xE0 => opcodes::set_4_b(self, mmio),
            0xE1 => opcodes::set_4_c(self, mmio),
            0xE2 => opcodes::set_4_d(self, mmio),
            0xE3 => opcodes::set_4_e(self, mmio),
            0xE4 => opcodes::set_4_h(self, mmio),
            0xE5 => opcodes::set_4_l(self, mmio),
            0xE6 => opcodes::set_4_hl(self, mmio),
            0xE7 => opcodes::set_4_a(self, mmio),
            0xE8 => opcodes::set_5_b(self, mmio),
            0xE9 => opcodes::set_5_c(self, mmio),
            0xEA => opcodes::set_5_d(self, mmio),
            0xEB => opcodes::set_5_e(self, mmio),
            0xEC => opcodes::set_5_h(self, mmio),
            0xED => opcodes::set_5_l(self, mmio),
            0xEE => opcodes::set_5_hl(self, mmio),
            0xEF => opcodes::set_5_a(self, mmio),
            0xF0 => opcodes::set_6_b(self, mmio),
            0xF1 => opcodes::set_6_c(self, mmio),
            0xF2 => opcodes::set_6_d(self, mmio),
            0xF3 => opcodes::set_6_e(self, mmio),
            0xF4 => opcodes::set_6_h(self, mmio),
            0xF5 => opcodes::set_6_l(self, mmio),
            0xF6 => opcodes::set_6_hl(self, mmio),
            0xF7 => opcodes::set_6_a(self, mmio),
            0xF8 => opcodes::set_7_b(self, mmio),
            0xF9 => opcodes::set_7_c(self, mmio),
            0xFA => opcodes::set_7_d(self, mmio),
            0xFB => opcodes::set_7_e(self, mmio),
            0xFC => opcodes::set_7_h(self, mmio),
            0xFD => opcodes::set_7_l(self, mmio),
            0xFE => opcodes::set_7_hl(self, mmio),
            0xFF => opcodes::set_7_a(self, mmio),
        }
    }
}

#[cfg(test)]
mod pc_wrap_tests {
    use super::*;

    /// The CB-prefix operand fetch is the one dispatch-side `pc` advance that did
    /// not wrap: a `0xCB` prefix fetched at 0xFFFE leaves `pc` at 0xFFFF, so the
    /// suffix fetch advances straight through the u16 boundary.
    #[test]
    fn cb_prefix_suffix_fetch_advances_pc_across_the_wrap() {
        let mut sm83 = SM83::new();
        let mut mmio = memory::mmio::Mmio::new();
        let mut ppu = crate::ppu::Ppu::new();
        sm83.registers.pc = 0xFFFF;
        {
            let mut bus = crate::cpu::Bus::new(&mut mmio, &mut ppu);
            sm83.execute_cb(&mut bus);
        }
        assert_eq!(sm83.registers.pc, 0x0000);
    }
}

#[cfg(test)]
mod interrupt_priority_tests {
    use super::*;
    use registers::InterruptFlag::{Joypad, Lcd, Serial, Timer, VBlank};

    /// The table-driven lookup replaced two hand-written if/else chains. This
    /// pins it against those chains over every IE/IF combination, for both the
    /// plain query and the timer-excluding one.
    #[test]
    fn table_lookup_matches_the_original_branch_chains() {
        let mut mmio = memory::mmio::Mmio::new();
        let cpu = SM83::new();
        for ie in 0u8..=0x1F {
            for iflag in 0u8..=0x1F {
                mmio.write(registers::INTERRUPT_ENABLE, ie);
                mmio.write(registers::INTERRUPT_FLAG, iflag);
                let e = mmio.read(registers::INTERRUPT_ENABLE);
                let f = mmio.read(registers::INTERRUPT_FLAG);
                let armed = |flag: registers::InterruptFlag| (e & flag as u8) != 0 && (f & flag as u8) != 0;

                // The pre-refactor priority chain, spelled out.
                let want = if armed(VBlank) {
                    Some(VBlank)
                } else if armed(Lcd) {
                    Some(Lcd)
                } else if armed(Timer) {
                    Some(Timer)
                } else if armed(Serial) {
                    Some(Serial)
                } else if armed(Joypad) {
                    Some(Joypad)
                } else {
                    None
                };
                // ...and its timer-skipping twin.
                let want_no_timer = if armed(VBlank) {
                    Some(VBlank)
                } else if armed(Lcd) {
                    Some(Lcd)
                } else if armed(Serial) {
                    Some(Serial)
                } else if armed(Joypad) {
                    Some(Joypad)
                } else {
                    None
                };

                let as_bit = |o: Option<registers::InterruptFlag>| o.map(|flag| flag as u8);
                assert_eq!(
                    as_bit(cpu.get_pending_interrupt(&mmio)),
                    as_bit(want),
                    "IE={ie:#04X} IF={iflag:#04X}"
                );
                assert_eq!(
                    as_bit(cpu.get_pending_interrupt_excluding_timer(&mmio)),
                    as_bit(want_no_timer),
                    "excluding timer, IE={ie:#04X} IF={iflag:#04X}"
                );
            }
        }
    }
}

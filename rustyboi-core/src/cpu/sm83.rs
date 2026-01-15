use crate::{cpu::opcodes, cpu::registers, memory, memory::Addressable};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct SM83 {
    pub registers: registers::Registers,
    pub halted: bool,
    pub stopped: bool,
    #[serde(default)]
    pub ime_enable_delay: u8,
    /// T-cycles remaining in the post-STOP-speed-switch stall.
    /// While non-zero, `step` returns short slices without fetching, so the
    /// surrounding `step_instruction` loop continues to advance peripherals.
    /// Mirrors Gambatte's `intevent_unhalt = cc + 0x20000 + 4` schedule.
    #[serde(default)]
    pub stop_unhalt_cycles: u32,
    /// ds-engine STAGE 2 faithful-prefetch state (RB_FAITHFUL). `opcode` holds the
    /// byte fetched at the previous instruction's boundary; `prefetched` is true
    /// once that fetch has happened and the opcode is awaiting execute/discard.
    /// Unused (and always default) when the gate is OFF.
    #[serde(default)]
    pub opcode: u8,
    #[serde(default)]
    pub prefetched: bool,
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
        }
    }

    pub fn step(&mut self, mmio: &mut crate::cpu::Bus) -> u32 {
        // While stalled after a CGB STOP-speed-switch, advance peripherals in
        // small slices without fetching instructions. Gambatte's `intevent_unhalt`
        // schedule effectively suspends CPU fetch for 0x20000 + 4 T-cycles after
        // STOP completes; the per-cycle peripheral loop in gb.rs still runs.
        if self.stop_unhalt_cycles > 0 {
            let slice = self.stop_unhalt_cycles.min(4);
            self.stop_unhalt_cycles -= slice;
            // Stop window over: run Gambatte's `intevent_unhalt` HDMA reflag gate at
            // the unhalt cc (memory.cpp:224/304), then re-enable the period edge.
            // A block held Low-at-stop reflags only if the unhalt lands back in the
            // HDMA period (`hdma_m3speedchange_late_m0wakeup_*`); one whose unhalt is
            // out of period stays dropped (`hdma_late_m3speedchange_*_1` -> out00).
            if self.stop_unhalt_cycles == 0 {
                // Unfreeze the OAM-DMA (Gambatte's unhalt resumes `updateOamDma`).
                mmio.set_oam_dma_stop_freeze(false);
                if mmio.in_stop_window() {
                    let in_period_unhalt = mmio.hdma_in_period_for_unhalt();
                    mmio.stop_window_exit_reflag(in_period_unhalt);
                }
            }
            return slice;
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
                self.halted = false;
                just_unhalted = true;
                mmio.clear_cpu_halt();
                // Mark whether this wakeup involved HDMA: those families fold the
                // CGB halt-exit +4 into their own block-transfer / unhalt-reflag
                // phase, so the getLyReg halt-exit bias must be suppressed for them
                // (see get_ly_reg_at_cc). A plain (no-HDMA) wakeup gets the bias.
                let hdma_wakeup = mmio.hdma_is_enabled()
                    || mmio.hdma_last_fire_cc().is_some()
                    || !matches!(
                        mmio.halt_hdma_state(),
                        memory::mmio::HaltHdmaState::Low
                    );
                mmio.set_halt_wakeup_hdma(hdma_wakeup);
                // C1: the instruction stream resumed by this wakeup carries the
                // unmodeled HALT-prefetch sub-M-cycle skew; flag it so the FF41
                // getStat-at-cc line-tail override defers to the renderer register
                // (which is already correct there). Cleared on the next HALT.
                mmio.set_halt_wakeup_skew(true);
                // FAITHFUL EVENTCC (R4): Gambatte's HALT-exit fixup (memory.cpp:308)
                // advances `cc += 4 * (isCgb() || cc - eventTime < 2)` before the
                // woken instruction stream resumes. The CGB (`isCgb()`) branch is
                // already modelled by the `+5` getStat read bias (controller.rs);
                // here we add the DMG branch: when the wakeup landed within 2cc of
                // the woken mode-0 STAT IRQ's event time (`pending_m0_irq_fire_cc`,
                // master cc), the +4 also applies on DMG. Flag it so the DMG
                // halt-woken getStat read samples +4cc later (the R4
                // `late_m0*_halt_m0stat_scx3_2b` reads land in the next-line OAM /
                // mode 2 instead of the stale mode 0).
                if crate::ppu::faithful_eventcc_enabled()
                    && pending_interrupt == Some(registers::InterruptFlag::Lcd)
                    && !mmio.mmio.is_cgb()
                {
                    if let Some(ev) = mmio.mmio.pending_m0_irq_fire_cc() {
                        let mcc = mmio.master_cc_dbg() as i64;
                        if mcc - (ev as i64) < 2 {
                            mmio.mmio.set_halt_wake_plus4_dmg(true);
                        }
                    }
                }
                // HALT-PREFETCH (Lever A, RB_PREFETCH_CC). Derive the M-cycle
                // phase bit that separates the byte-identical _1b/_2b streams.
                // The pre-snap HALT-entry cc H (halt_entry_cc) carries the extra
                // M-cycle that Gambatte's ceil_4(eventTime) snap (cpu.cpp:1075
                // `if cc() < nextEventTime()`) would erase: H < S means the snap
                // fired (cc rounded up to S) -> phase 0; H >= S means the snap was
                // SKIPPED (the test failed, cc stayed at the later entry) and the
                // woken read inherits its extra M-cycle -> phase 1. _2b enters
                // HALT one M-cycle (4cc) later than _1b, pushing H across S.
                // Replaces the all-or-nothing +4 with a per-stream 0/1 phase at
                // the FF41 consume site (controller.rs). Stacks on the foundation:
                // halt_wake_plus4_dmg above is left set for the flag-OFF path.
                if crate::ppu::controller::prefetch_cc_enabled()
                    && pending_interrupt == Some(registers::InterruptFlag::Lcd)
                    && !mmio.mmio.is_cgb()
                {
                    let phase = match (mmio.mmio.pending_m0_irq_fire_cc(), mmio.mmio.halt_entry_cc()) {
                        (Some(ev), Some(h)) => {
                            let e = ev as i64;
                            // S = ceil_4(E) = the snap target Gambatte rounds cc up
                            // to (cpu.cpp:1077 `cc += cycles + (-cycles & 3)`).
                            let s = e + ((e.wrapping_neg()) & 3);
                            // Faithful reconstruction of Gambatte's snap-SKIP test
                            // (cpu.cpp:1075 `if cc() < nextEventTime()`): the HALT
                            // M-cycle charges cc forward; when the pre-snap entry H
                            // lands in the M-cycle directly below the snap target,
                            // the post-charge cc reaches the event and the snap is
                            // SKIPPED, so the woken read inherits the extra M-cycle
                            // (phase 1). _2b enters HALT one M-cycle later than _1b
                            // (H = S-4 vs S-8), crossing this boundary. The +4
                            // aligns rustyboi's pre-charge entry cc to Gambatte's
                            // post-HALT-charge cc origin.
                            if (h as i64) + 4 >= s { 1u32 } else { 0u32 }
                        }
                        _ => 0,
                    };
                    mmio.mmio.set_halt_prefetch_phase(phase);
                    if std::env::var("RB_PREFETCH_TRACE").is_ok() {
                        let pc = self.registers.pc;
                        let h = mmio.mmio.halt_entry_cc();
                        let ev = mmio.mmio.pending_m0_irq_fire_cc();
                        let e = ev.map(|x| x as i64).unwrap_or(-1);
                        let s = e + ((e.wrapping_neg()) & 3);
                        let mcc = mmio.master_cc_dbg();
                        eprintln!(
                            "[PREFETCH] pc={:#06x} mcc={} H={:?} E={} S={} phase={}",
                            pc, mcc, h, e, s, phase
                        );
                    }
                }
                // HALT-PREFETCH woken-PC PUSH phase (R-PC, RB_TIMER_PUSH_PHASE).
                // Faithful conditional-prefetch-undo fix. Gambatte's interrupt
                // service undoes the boundary prefetch CONDITIONALLY
                // (interrupter.cpp:42 `if (prefetched_) pc_ -= 1`), where
                // `prefetched_` is the byte fetched by `case 0x76`'s
                // `prefetched_ = mem_.halt(cc()) = hdmaReqFlagged(...)`. rustyboi's
                // service_interrupt instead undoes UNCONDITIONALLY (`pc -= 1`),
                // which is correct for the common path where the unhalt did a real
                // pc-ADVANCING boundary fetch. But a Requested-halt wakeup left a
                // NON-advancing prefetch peek at HALT entry (opcodes.rs halt(): the
                // byte at pc=HALT+1 is peeked, pc NOT advanced), so the unhalt skips
                // its fetch and pc stays at the resume address. There the
                // unconditional `pc -= 1` over-subtracts by one instruction byte,
                // pinning the pushed resume PC one short (pc_scx1 _2: AC instead of
                // AD; _1/_3 took the Low/real-fetch path and net to zero). Mark
                // phase 1 for exactly that case so the push consume re-adds the +1.
                // Gated Timer IRQ + CGB (the failing family's vector + hardware) and
                // stored in a SEPARATE register so the FF41 getStat consumer that
                // reads halt_prefetch_phase is untouched. The blast radius is the
                // whole interrupt-service path, so the gate is the exact wakeup
                // shape: just-unhalted Timer service whose halt left a non-advancing
                // Requested-HDMA prefetch peek.
                let req_halt_peek = self.prefetched
                    && matches!(
                        mmio.halt_hdma_state(),
                        memory::mmio::HaltHdmaState::Requested
                    );
                if crate::ppu::controller::timer_push_phase_enabled()
                    && pending_interrupt == Some(registers::InterruptFlag::Timer)
                    && mmio.mmio.is_cgb()
                {
                    let phase = if req_halt_peek { 1u32 } else { 0u32 };
                    mmio.mmio.set_timer_push_phase(phase);
                    if std::env::var("RB_TIMER_PUSH_TRACE").is_ok() {
                        let pc = self.registers.pc;
                        let h = mmio.mmio.halt_entry_cc();
                        let ev = mmio.mmio.pending_timer_event_cc();
                        let e = ev.map(|x| x as i64).unwrap_or(-1);
                        let mcc = mmio.master_cc_dbg();
                        eprintln!(
                            "[TIMERPUSH] pc={:#06x} mcc={} H={:?} E={} prefetched={} hdma={:?} phase={}",
                            pc, mcc, h, e, self.prefetched, mmio.halt_hdma_state(), phase
                        );
                    }
                }
                // Gambatte unhalt re-flag gate (memory.cpp:224/304):
                //   (hdmaEnabled && isHdmaPeriod && haltHdmaState == hdma_low)
                //   || haltHdmaState == hdma_requested
                // Keys on hdma_low: the block fires on unhalt only when the HDMA
                // period was *entered during* the halt (Low at halt time), not when
                // it was already in-period+armed (High, which already fired).
                // COORDINATED piece #3: evaluate the unhalt period off the
                // renderer's cycle-exact isHdmaPeriod(cc) (bus path), not the loose
                // cached/STAT-mode snapshot, so a Low-at-halt block that is not yet
                // in period at unhalt fires on its natural m0 edge.
                // EI fast-dispatch (RB_EI_FAST) delivers the timer IRQ at the EARLY
                // anchor (schedCc + IF_OFF) instead of the LATE anchor (schedCc +
                // CC_OFF). The timer ISR re-enables the LCD (FF40 write), so its
                // closed-form `m0_time_master` lands `CC_OFF - IF_OFF`(=4) cc
                // earlier under the fast path. The unhalt access cc (absolute
                // `intevent_unhalt` schedule) is unchanged, so the unhalt-period
                // DEPTH (`cc - m0t`) inflates by 4 — dropping the reflag of a
                // Low-at-halt block near the line END. Widen the unhalt-period END
                // bracket by +4 on the fast path so that block still reflags, while
                // leaving the mode-0 ENTRY bracket intact (a depth-0 block reflags
                // either way, matching Gambatte). (Non-fast HALT-late path:
                // limit_adj = 0 => byte-identical.) Fast dispatch is ON by default
                // post co-land; RB_EI_FAST=0 forces it OFF.
                let limit_adj: i64 = 4;
                let in_period_unhalt = mmio.hdma_in_period_for_unhalt_adj(limit_adj);
                let was_requested =
                    matches!(mmio.halt_hdma_state(), memory::mmio::HaltHdmaState::Requested);
                match mmio.halt_hdma_state() {
                    memory::mmio::HaltHdmaState::Requested => {
                        mmio.set_hdma_req();
                        // ENDGAME R2: a multi-block Requested transfer
                        // (hdma_length() != 0) does NOT inline-fire at unhalt (gated
                        // off below); its first block fires on its m0 edge DURING the
                        // resume instruction. Arm the lockstep window so the bus
                        // advances the world through that block's transfer cc at fire
                        // time (event-interleaved dma()), so the same-instruction
                        // resume read sees the extended mode-3 line. Cleared when the
                        // resume instruction completes. IME-off only (the IME-on render
                        // phase breaks under the lockstep advance).
                        if !self.registers.ime && mmio.hdma_length() != 0 {
                            mmio.set_hdma_resume_lockstep_window(true);
                        }
                        // m25: the PRE-transfer dest-byte shadow lets the resume read
                        // observe the old VRAM byte (the read is ordered before dma()'s
                        // dest commits). Armed for both IME states (the IME-on
                        // interrupt-service resume also reads an in-block dest byte);
                        // harmless when the relevant block fires outside the window.
                        if mmio.hdma_length() != 0 {
                            mmio.set_hdma_resume_shadow_window(true);
                        }
                        // per-access STAGE 2 (FACET 3): the Requested-held block is
                        // about to fire at unhalt. Arm the sub-block-cc consume so
                        // the next-line m0 edge that re-arms the following block is
                        // absorbed iff it lands inside this block's transfer span
                        // (Gambatte m0 `memevent_hdma` consumed by the in-flight
                        // `dma()`), deferring it one line.
                        mmio.arm_hdma_peraccess_consume();
                    }
                    memory::mmio::HaltHdmaState::Low
                        if in_period_unhalt && mmio.hdma_is_enabled() =>
                    {
                        mmio.set_hdma_req()
                    }
                    memory::mmio::HaltHdmaState::High if mmio.hdma_is_enabled() => {
                        // High-at-halt: the held block was already served and the
                        // unhalt does NOT reflag. Gambatte also consumed the
                        // immediately-following line's m0 `flagHdmaReq` during the
                        // halt; our unhalt cc lands ~1 dot before that m0, so without
                        // this the post-unhalt STAT fallback would fire a spurious
                        // extra block one line early (hdma_late_m0halt_*lcdoffset*_1).
                        mmio.arm_hdma_high_unhalt_consume();
                    }
                    _ => {}
                }
                // Late-hdma-vs-interrupt unhalt precedence (memory.cpp:329-364): a
                // Low-at-halt block that is NOT in the HDMA period at unhalt
                // (Gambatte's `isHdmaPeriod(cc)` reflag gate false => NOREFLAG) does
                // not fire at unhalt; its m0-edge falls within the immediately-following
                // interrupt service, so the block fires AFTER the PC pushes and
                // copies the pushed return address (`late_hdma_vs_tima_*_halt_2`,
                // 0x11C9). Flag it so `service_interrupt` suppresses+reorders that
                // greedy fire past the pushes. An in-period (REFLAG) block fires AT
                // unhalt, before the pushes (the `_halt_1` dma-wins case), and is
                // left on the synchronous path.
                // The unhalt-cc-vs-m0Time straddle that decides REFLAG (fire at
                // unhalt) vs NOREFLAG (defer past the pushes) is razor-thin (1 cc).
                // rustyboi's `m0_time_master` matches Gambatte to within the +1 dot
                // phase ONLY on an already-rendered line (LY>=1, the TIMA content
                // tests at LY=1). On the first visible line after an LCD re-enable
                // (LY=0) the closed-form m0Time carries the unresolved ~6 cc phase
                // lag, so the straddle is unreliable there — and the LY=0 case here
                // is the mode-0-IRQ `hdma_vs_m0_*_halt` (REFLAG / dma-wins), for which
                // no deferred-content sibling exists. Scope the defer to the timer
                // IRQ (the `late_hdma_vs_tima_*_halt_2` family), where the straddle is
                // sound; the mode-0-IRQ block keeps its synchronous (REFLAG) fire.
                let pending_is_timer =
                    pending_interrupt == Some(registers::InterruptFlag::Timer);
                let fires_before_pushes = mmio.hdma_unhalt_fires_before_pushes();
                let noreflag_deferred = pending_is_timer
                    && mmio.hdma_is_enabled()
                    && matches!(mmio.halt_hdma_state(), memory::mmio::HaltHdmaState::Low)
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
                mmio.set_halt_hdma_state(memory::mmio::HaltHdmaState::Low);
                // Unhalt-cc / LY phase fix. A Requested-held HDMA block (flagged at
                // halt entry) runs its `dma()` event DURING the halt period in
                // Gambatte (intevent_dma fires before intevent_unhalt resumes the CPU;
                // cctracer ground truth: the dma() lands at ~m0Time, mid-halt, the
                // unhalt NOP resumes AFTER it). rustyboi otherwise deferred the whole
                // transfer until AFTER the HALT-bug double-execute resume instruction,
                // so the resume instruction — and every post-unhalt FF44/PC read on the
                // wakeup sled — landed one HDMA-block (36 SS / 68 DS cc) too early in
                // cc relative to the LY/PC Gambatte's stream reads, a +1 LY-dot straddle
                // on the boundary cases (hdma_late_m3halt_m2unhalt_ly_scx1_3/scx2_3).
                // Fire the held block NOW and tick its transfer stall inline (the dma()
                // cc happens during the halt window) so the prefetched resume byte
                // executes at the post-transfer cc == Gambatte's intevent_unhalt.
                //
                // Gated to:
                //  - `hdma_length() == 0` (the block COMPLETES the transfer): a
                //    multi-block transfer (e.g. hdma_transition_*_late_unhalt, ff55=81)
                //    relies on the existing per-dot firing path for its second-block
                //    period re-arm and FF55 readback; firing the first block here
                //    desyncs that sequence.
                //  - `!ime` (the IME-off double-execute resume): when IME is on the
                //    unhalt instead SERVICES an interrupt (the prefetch is rewound and
                //    PC pushed); that path accounts the wakeup cc through
                //    `service_interrupt`, where this inline shift double-counts. The EI
                //    PC/LY readers (hdma_late_ei_m3halt_m2unhalt_pc_scx1_2) carry the
                //    same +4 phase but need the service-path cc fix, not this one.
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
                // CPU is halted and no interrupt is pending, consume 1 cycle and return
                return 1;
            }
        }

        // STAGE 2 event-cc dispatch: an IRQ is serviceable only once the
        // boundary access cc has reached the cc its IF bit was raised
        // (Gambatte's `intevent_interrupts` boundary), not merely once the
        // IF bit is flagged. `pending_interrupt` was sampled at this
        // instruction boundary; gate the timer IRQ on the boundary access cc
        // (raw master_cc, the stage-1 read anchor) having reached its
        // recorded fire cc, re-resolving a lower-priority armed IRQ if the
        // timer is not yet due.
        //
        // PER-ACCESS DELIVERY SPLIT: a non-halt, non-stop EI loop services the
        // timer IRQ at the EARLY anchor (`pending_timer_fire_cc_ei`, schedCc +
        // IF_OFF) so the ISR / TAC re-write lands on Gambatte's exact divider
        // phase (the late_tc01 / irq cluster). HALT and post-STOP streams keep
        // the LATE anchor (`pending_timer_fire_cc`, schedCc + CC_OFF), which is
        // where their read-after-overflow tests are byte-exact.
        let boundary_access_cc = mmio.access_cc();
        let ei_ctx = !just_unhalted && !self.stopped;

        // EI-loop fast delivery: in a non-halt/non-stop loop with IME on, fire
        // an imminent overflow at the EARLY anchor (schedCc + IF_OFF) so the
        // ensuing service runs on Gambatte's exact phase, ahead of the late
        // per-dot delivery. Re-sample the pending interrupt afterward.
        // NOTE: the EI-loop fast timer dispatch is now ON by default (per-access
        // timer fast-dispatch co-land). It dissolves the timer-schedule offset
        // for the pure-timer re-derivation cluster (tima/tc00_late_tc01_*,
        // irq_ds, irq_retrigger: +19 by construction); the +5 IF-delivery grid
        // it bypasses was the shared lever for three coupled subsystems, all
        // re-tuned to the early grid in the same co-land — the IF-edge re-flag
        // tests (tc00_irq_ifw_* / late_retrigger), the CGB STOP sub-dot
        // derivation (speedchange_tima*, STOP_EI_PROMOTE_ADJ split), and the
        // HDMA-vs-IRQ unhalt service-phase race (hdma_*unhalt, END-bracket +4).
        // Net co-land vs the +5-grid baseline is strongly positive; the only
        // residuals are the render-phase-coupled hdma_*_ly_*_1 glyph tests (the
        // EI-fast LCD-re-enable shifts the PPU render phase 4 cc — a lever that
        // belongs to the lazy-PPU render stage, and which simultaneously flips
        // the sibling hdma_*_ly_*_6 tests TO passing). RB_EI_FAST=0 forces the
        // OFF / +5-grid baseline (A/B preserved); unset or =1 leaves it ON.
        if ei_ctx && self.registers.ime {
            if let Some(early) = mmio.next_timer_overflow_ei_cc() {
                if boundary_access_cc >= early {
                    mmio.force_ei_timer_delivery(boundary_access_cc);
                    pending_interrupt = self.get_pending_interrupt(mmio);
                }
            }
        }

        let mut serviceable = pending_interrupt;
        if serviceable == Some(registers::InterruptFlag::Timer) {
            // Only the EI fast-dispatch (when enabled) services on the early
            // anchor; otherwise the gate is the baseline late (+CC_OFF) anchor.
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

        // FAITHFUL prefetch model: fetch the opcode at the instruction
        // boundary FIRST (Gambatte's end-of-previous-instruction prefetch,
        // charged at the boundary cc), THEN decide whether to service. A
        // serviced interrupt UNDOES the prefetch (rewinds pc); otherwise the
        // prefetched opcode is consumed by execute with no re-fetch/re-charge.
        if !self.prefetched {
            self.opcode = mmio.fetch_opcode(self.registers.pc);
            self.registers.pc = self.registers.pc.wrapping_add(1);
            self.prefetched = true;
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
        cycles += self.execute(op, mmio);
        self.apply_ime_delay();
        mmio.set_hdma_resume_lockstep_window(false);
        cycles
    }

    fn service_interrupt(&mut self, bus: &mut crate::cpu::Bus, just_unhalted: bool) -> u32 {
        self.registers.ime = false;
        self.ime_enable_delay = 0;
        bus.clear_delayed_writes();

        // C7-full interrupt-vs-dma precedence (Gambatte memory.cpp:312-320): an
        // HDMA block whose m0-edge latch is LATER than the interrupt's service cc
        // fires AFTER the interrupt's PC pushes, so a pushed return address is
        // visible in the HDMA copy of that stack slot (`late_hdma_vs_ei/ie/tima`
        // content-test root). A block ALREADY latched before this service began is
        // *earlier* than the interrupt and keeps firing at its natural dot (before
        // the pushes — the dma-wins races). So suppress the fire only when no block
        // is pending at service entry; a block latched during the service M-cycles
        // is then held and fired explicitly after the pushes.
        //
        // A service that resumes from HALT this same step is eligible ONLY when the
        // unhalt did NOT reflag the block (Gambatte's `isHdmaPeriod(cc)` reflag gate
        // false at unhalt => NOREFLAG): that block's m0-edge falls within THIS service window
        // and must fire AFTER the pushes (`late_hdma_vs_tima_*_halt_2`, copy 0x11C9).
        // A REFLAG block fired AT unhalt, before the pushes (the `*_halt_1` dma-wins
        // case), and is left on the synchronous path. Non-halt services suppress
        // whenever no block is already pending at entry (a block latched during the
        // service M-cycles is held and fired post-push).
        // For the unhalt-deferred case the block's m0-edge may have already ARMED
        // (`hdma_req_pending`) during the boundary prefetch — its FIRE was held by
        // the suppression engaged at unhalt. Keep suppressing it here (the
        // already-pending block is exactly the one to fire post-push); do NOT gate
        // on `!hdma_fire_pending`. The non-halt path keeps that gate: a block
        // already pending at entry there latched a full instruction earlier and
        // wins the race (fires on its natural dot, not post-push).
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
        // is governed by `haltHdmaState_`).
        let service_access_cc = bus.access_cc();

        // STAGE 2: the boundary prefetch already read (and charged +4 for)
        // the opcode this interrupt discards. UNDO it: rewind pc to that
        // opcode and drop the prefetch flag (re-fetched after the ISR
        // returns). With the real prefetch supplying one of the 5 interrupt
        // M-cycles, service ticks 2 internal (the 2 wait cycles) + 2 pushes =
        // 16; prefetch (4) + 16 = the full 20-cc / 5-M-cycle interrupt cost.
        self.registers.pc = self.registers.pc.wrapping_sub(1);
        self.prefetched = false;
        // HALT-PREFETCH woken-PC PUSH consume (R-PC, RB_TIMER_PUSH_PHASE). The
        // unconditional `pc -= 1` undo above is correct only when the unhalt did a
        // pc-ADVANCING boundary fetch. For the CGB+Timer phase-1 wakeup (the HALT
        // left a NON-advancing Requested-HDMA prefetch peek, so pc was never
        // advanced past the resume byte) the undo over-subtracts by one. Re-add the
        // +1 here, BEFORE both pushes (so a page-crossing carry propagates to the
        // high byte too), reproducing Gambatte's CONDITIONAL prefetch undo
        // (interrupter.cpp:42 `if (prefetched_) pc_ -= 1`, where for this wakeup
        // hdmaReq=false => no undo => pushed resume PC = HALT+1). Phase-conditioned:
        // phase 0 streams (pc_scx1 _1/_3, real-fetch path) are unchanged. The flag
        // is consumed (zeroed) below so it biases exactly one interrupt service.
        if just_unhalted
            && crate::ppu::controller::timer_push_phase_enabled()
            && bus.mmio.timer_push_phase() == 1
        {
            self.registers.pc = self.registers.pc.wrapping_add(1);
        }
        if crate::ppu::controller::timer_push_phase_enabled() {
            bus.mmio.set_timer_push_phase(0);
        }
        bus.internal_cycle();
        bus.internal_cycle();

        self.registers.sp = self.registers.sp.wrapping_sub(1);
        bus.write(self.registers.sp, (self.registers.pc >> 8) as u8);

        // The vector is latched from IE&IF *after* the high-byte push: if that
        // push wrote over IE (SP near 0xFFFF) the pending set can change, and
        // if nothing is pending the interrupt is cancelled (vector 0x0000).
        let flag = self.get_pending_interrupt(bus);

        // The low-byte push ACKs the IF bit partway through its M-cycle (Gambatte
        // `Memory::ackIrq(n, cc)` after the push: it advances each source to a
        // per-source sub-cc offset (`updateSerial(cc+3+cgb)`, `updateTimaIrq(cc+2
        // +cgb)`, `lcd_.update(cc+2)`) — flagging any IRQ that completes by that
        // offset — THEN clears only the dispatched bit `n`. A source completing
        // *after* its offset re-flags IF and survives for the ISR to read (the
        // `late_retrigger` / `start_wait..._read_if` re-fire); one completing by
        // the offset is flagged-then-cleared and reads back gone. The per-source
        // offset (+3+cgb serial, +2+cgb timer, +2 lcd) is the DMG-vs-CGB
        // discriminator for the `_2` boundary cases. Done inside the push M-cycle
        // so the clear lands at the exact sub-dot. Skip while OAM DMA is active
        // (the split-push bypasses the DMA-conflict redirection in `bus.write`).
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
        // service's M-cycle window (the interrupt won the m0Time-vs-minIntTime
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
            // The LCD/Serial/Timer vectors were already ACKed mid-push (split_ack,
            // faithful Gambatte ackIrq ordering); clearing again here would wipe a
            // same-window re-fire that must survive. When the split was skipped
            // (OAM DMA active) or the vector is VBlank/Joypad, clear here as before.
            if !split_ack {
                self.set_interrupt_flag(flag, false, bus);
            }
            // STAGE 2: once the timer IRQ is dispatched, drop its recorded fire
            // cc so the next period's fire is tracked fresh.
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

    pub fn set_interrupt_flag(&mut self, flag: registers::InterruptFlag, value: bool, mmio: &mut memory::mmio::Mmio) {
        if value {
            mmio.write(registers::INTERRUPT_FLAG, mmio.read(registers::INTERRUPT_FLAG) | flag as u8);
        } else {
            mmio.write(registers::INTERRUPT_FLAG, mmio.read(registers::INTERRUPT_FLAG) & !(flag as u8));
        }
    }

    pub fn get_interrupt_flag(&self, flag: registers::InterruptFlag, mmio: &memory::mmio::Mmio) -> bool {
        (mmio.read(registers::INTERRUPT_FLAG) & (flag as u8)) != 0
    }

    pub fn get_interrupt_enable_flag(&self, flag: registers::InterruptFlag, mmio: &memory::mmio::Mmio) -> bool {
        (mmio.read(registers::INTERRUPT_ENABLE) & (flag as u8)) != 0
    }

    fn get_pending_interrupt(&self, mmio: &memory::mmio::Mmio) -> Option<registers::InterruptFlag> {
        // Check interrupts in priority order (highest to lowest)
        if self.get_interrupt_enable_flag(registers::InterruptFlag::VBlank, mmio) && self.get_interrupt_flag(registers::InterruptFlag::VBlank, mmio) {
            Some(registers::InterruptFlag::VBlank)
        } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Lcd, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Lcd, mmio) {
            Some(registers::InterruptFlag::Lcd)
        } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Timer, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Timer, mmio) {
            Some(registers::InterruptFlag::Timer)
        } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Serial, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Serial, mmio) {
            Some(registers::InterruptFlag::Serial)
        } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Joypad, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Joypad, mmio) {
            Some(registers::InterruptFlag::Joypad)
        } else {
            None
        }
    }

    /// Like `get_pending_interrupt` but skips the Timer source. Used by the
    /// STAGE 2 event-cc dispatch gate when the timer IRQ's fire cc has not yet
    /// been reached, so a lower-priority armed interrupt can still dispatch.
    fn get_pending_interrupt_excluding_timer(&self, mmio: &memory::mmio::Mmio) -> Option<registers::InterruptFlag> {
        if self.get_interrupt_enable_flag(registers::InterruptFlag::VBlank, mmio) && self.get_interrupt_flag(registers::InterruptFlag::VBlank, mmio) {
            Some(registers::InterruptFlag::VBlank)
        } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Lcd, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Lcd, mmio) {
            Some(registers::InterruptFlag::Lcd)
        } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Serial, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Serial, mmio) {
            Some(registers::InterruptFlag::Serial)
        } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Joypad, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Joypad, mmio) {
            Some(registers::InterruptFlag::Joypad)
        } else {
            None
        }
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
        self.registers.pc += 1;
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

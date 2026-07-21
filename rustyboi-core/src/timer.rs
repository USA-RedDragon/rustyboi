use crate::cpu;
use crate::memory::Addressable;
use crate::memory::mmio;

use serde::{Deserialize, Serialize};

pub const DIV: u16 = 0xFF04;
pub const TIMA: u16 = 0xFF05;
pub const TMA: u16 = 0xFF06;
pub const TAC: u16 = 0xFF07;

// TAC register bits
const TAC_ENABLE: u8 = 1 << 2; // Bit 2: Timer enable
const TAC_FREQUENCY_MASK: u8 = 0b00000011; // Bits 0-1: Timer frequency

// TAC's 2-bit frequency select routes TIMA off DIV bit 9/3/5/7 (for
// `tac & 3` = 0/1/2/3), i.e. one increment per 256/4/16/64 M-cycles. Storing
// `bit_index + 1` here lets TIMA derive as `(cc - tima_last_update) >> clk`,
// one increment per `2^clk` T-cycles.
// Pan Docs: Timer and Divider Registers — https://gbdev.io/pandocs/Timer_and_Divider_Registers.html
const TIMA_CLOCK: [u32; 4] = [10, 4, 6, 8];

// Sentinel "no event scheduled" marker placed far past any reachable `abs_cc`
// (a dot counter that never approaches u64::MAX within a run). Every use of
// `tmatime`/`next_irq_event_time` is guarded by an explicit disabled check.
const DISABLED_TIME: u64 = u64::MAX;

// Offset from the per-dot `abs_cc` (incremented at the *start* of each dot's
// `step`, so it trails the live access cc by one dot) to the cc at which the
// scheduled IRQ becomes IF-visible. A CPU access occupies a 4-dot M-cycle; the
// effect lands at the M-cycle end (`+4`), plus one dot for the start-of-step
// increment lag (`+1`) = `+5`.
// The three offsets below are sub-cycle model calibrations: not in Pan Docs,
// TCAGBD, or GBCTR (emulator-internal per-dot phase constants). The hardware
// behaviour they encode — the one-CPU-cycle lag between a TIMA overflow and its
// IF bit becoming visible — IS documented (TCAGBD §4.3 "There is a delay of one
// CPU cycle between the overflow and the IF flag being set"; §5.6); only the
// split into read/EI/write sub-quanta is novel, derived from timer test-ROM refs.
const CC_OFF: i64 = 5;
/// EI-loop IF-visibility offset. In a non-halt EI loop the timer IF bit is
/// dispatched at the early anchor `sched_cc + IF_OFF`, where `sched_cc` is the
/// scheduled overflow cc (vs the `CC_OFF`-late gate
/// used by HALT/STOP) so the ISR (and any TAC re-write) runs on the correct
/// divider phase.
const IF_OFF: i64 = 1;
/// Write-side access-cc offset. The write path (APU/serial trigger boundary
/// math) resolves on a different sub-quantum phase than the read `CC_OFF`.
const WRITE_CC_OFF: i64 = 0;

// The TMA reload / IF-set lags the overflow tick. Pan Docs documents this as one
// M-cycle after the overflow (TIMA reads 0 for that M-cycle); TCAGBD §5.6 agrees
// ("Timer interrupt is delayed 1 cycle (4 clocks) from the TIMA overflow ... It
// could be less clocks, but the CPU can't check that"). This model applies a
// 3 T-cycle bias to `tmatime` and the scheduled IRQ cc — a sub-cycle refinement
// inside TCAGBD's "could be less clocks" allowance, not separately documented.
// Pan Docs: Timer obscure behaviour — https://gbdev.io/pandocs/Timer_Obscure_Behaviour.html
const TMA_OFF: u64 = 3;

// The timer register access cc is the raw master cc (`abs_cc`, captured at the
// START of the CPU access M-cycle). The IRQ delivery path in `step()` folds
// CC_OFF back in (`update_irq_delivery`) to keep the absolute fire cc unchanged.
// The APU frame sequencer is driven by a single closed-form (update-to-cc) path.

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Timer {
    tima: u8,
    tma: u8,
    tac: u8,
    // Double-speed state observed at the last `step`; used by `speed_change`
    // (called right after the speed flag toggles, before any further step) to
    // learn the pre-switch speed.
    #[serde(default)]
    last_double_speed: bool,
    // Previous APU-clock bit (DIV bit 12, or 13 in double speed); its falling
    // edge clocks the APU frame sequencer.
    #[serde(default)]
    last_apu_div_bit: bool,
    // Absolute, never-reset master T-cycle counter. This is the single source of
    // time in this module: the DIV divider, the
    // scheduled TIMA, the serial clock and the APU frame sequencer all derive
    // from it.
    #[serde(default)]
    abs_cc: u64,
    // `abs_cc` value of the last DIV write. The DIV divider is
    // `(abs_cc - div_anchor) & 0xFFFF`.
    #[serde(default)]
    div_anchor: u64,
    // Monotonic count of DIV writes (each rebases `div_anchor`). The APU master
    // clock reads this to detect a DIV reset and apply the divider-reset
    // cycle-counter fold to the frame sequencer phase.
    #[serde(default)]
    div_reset_count: u64,
    // Scheduled-TIMA state: `tima_last_update` is the cc the current TIMA value
    // was computed at; TIMA derives as
    // `tima + ((cc - tima_last_update) >> clk)`. `tmatime` is the cc at which a
    // pending overflow's TMA-reload becomes visible. `next_irq_event_time` is the
    // cc at which the timer IRQ fires. All three are absolute `abs_cc` values, so
    // the IRQ is delivered at the same anchor a start-cc CPU read of TIMA resolves
    // on.
    #[serde(default)]
    tima_last_update: u64,
    #[serde(default = "disabled_time")]
    tmatime: u64,
    #[serde(default = "disabled_time")]
    next_irq_event_time: u64,
    // Deferred IRQ flag for the write-path glitches (TAC write / DIV reset) that
    // flag an IRQ inline. The write path has no `mmio` borrow;
    // `step` (and the post-write flush in `mmio`) raise the actual IF bit.
    #[serde(default)]
    pending_irq: bool,
    // APU-visible divider anchor. Currently tracks `div_anchor` for every write
    // (including the CGB STOP speed-switch DIV-write reset); kept as a separate field so
    // the APU fold can diverge from the TIMA/DIV register anchor if needed.
    #[serde(default)]
    div_anchor_apu: u64,
    // The raw-abs_cc cc at which the most recent still-undispatched TIMA IRQ
    // became deliverable (its IF bit was raised). The CPU step gate makes the
    // timer IRQ serviceable only once the boundary access cc reaches this, rather
    // than off the instruction-start IF snapshot. DISABLED_TIME = none pending.
    #[serde(default = "disabled_time")]
    last_fire_cc: u64,
    // The early (EI-loop) gate cc for the same undispatched IRQ: scheduled
    // overflow cc + IF_OFF.
    // The non-halt/non-stop dispatch gate uses this instead of `last_fire_cc`.
    #[serde(default = "disabled_time")]
    last_fire_cc_ei: u64,
    /// Sticky: the current ISR / instruction stream was entered via an EI
    /// fast-dispatch and therefore runs on the early (`IF_OFF`) grid; it
    /// persists through the whole ISR. While set, an
    /// un-serviced overflow re-flags IF on the early anchor (`update_irq_delivery`)
    /// and the FF0F timer-bit read samples at the access cc (`bus.rs`), so the
    /// ISR's IF write / read / re-trigger all resolve on the same grid. Set by
    /// `force_ei_delivery`; cleared when the CPU enters HALT (a HALT-woken ISR is
    /// not on the early grid — its IF-set stays late).
    #[serde(skip, default)]
    isr_on_early_grid: bool,
    /// CGB *silicon* (including CGB-in-DMG-compat) and AGB silicon. Only the
    /// old-TAC-disabled TAC-write glitch reads these: which DIV bit pair can
    /// latch that glitch is a per-family property (see
    /// `disabled_tac_write_glitch`). Reseeded from `Mmio::set_serial_cgb` /
    /// `Mmio::set_agb`, so they ride the machine identity rather than the
    /// savestate.
    #[serde(skip, default)]
    is_cgb: bool,
    #[serde(skip, default)]
    is_agb: bool,
}

fn disabled_time() -> u64 {
    DISABLED_TIME
}

/// Settle a raw `tima + ticks` accumulation that has run past 0x100 back into
/// the post-reload range `(tma, 0x100]`. On the hardware each overflow reloads
/// TIMA to TMA, so exactly `period = 256 - tma` increments elapse between one
/// overflow and the next; the settled value is therefore the accumulation taken
/// modulo that period, biased so that an exact overflow reports as 0x100 (the
/// "just overflowed, in the reload window" state the caller then special-cases).
/// Caller guarantees `tmp > 0x100`, so the subtraction cannot underflow.
fn settle_tima_overflow(tmp: u64, tma: u8) -> u64 {
    let period = 0x100 - tma as u64;
    tma as u64 + (tmp - tma as u64 - 1) % period + 1
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            tima: 0,
            tma: 0,
            tac: 0,
            last_double_speed: false,
            last_apu_div_bit: false,
            abs_cc: 0,
            div_anchor: 0,
            div_reset_count: 0,
            tima_last_update: 0,
            tmatime: DISABLED_TIME,
            next_irq_event_time: DISABLED_TIME,
            pending_irq: false,
            div_anchor_apu: 0,
            last_fire_cc: DISABLED_TIME,
            last_fire_cc_ei: DISABLED_TIME,
            isr_on_early_grid: false,
            is_cgb: false,
            is_agb: false,
        }
    }

    /// CGB silicon flag (true for CGB-in-DMG-compat too — the glitch is a
    /// property of the timer silicon, not of the compat mode).
    pub(crate) fn set_cgb(&mut self, cgb: bool) {
        self.is_cgb = cgb;
    }

    /// AGB silicon flag.
    pub(crate) fn set_agb(&mut self, agb: bool) {
        self.is_agb = agb;
    }

    #[cfg(test)]
    pub(crate) fn rev_flags(&self) -> (bool, bool) {
        (self.is_cgb, self.is_agb)
    }

    /// The cc the most recent still-undispatched TIMA IRQ became deliverable, or
    /// `None`. Cleared at dispatch via `clear_fire_cc`.
    pub(crate) fn pending_fire_cc(&self) -> Option<u64> {
        if self.last_fire_cc != DISABLED_TIME {
            Some(self.last_fire_cc)
        } else {
            None
        }
    }


    /// The EARLY (EI-loop) gate cc for the undispatched timer IRQ, or `None`.
    pub(crate) fn pending_fire_cc_ei(&self) -> Option<u64> {
        if self.last_fire_cc_ei != DISABLED_TIME {
            Some(self.last_fire_cc_ei)
        } else {
            None
        }
    }

    /// Clear the recorded fire cc after the CPU dispatches the IRQ.
    pub(crate) fn clear_fire_cc(&mut self) {
        self.last_fire_cc = DISABLED_TIME;
        self.last_fire_cc_ei = DISABLED_TIME;
    }

    /// The exact cc at which the next scheduled overflow's IF bit will be raised
    /// inside `update_irq_delivery` / `step_to`, accounting for the same `fold`
    /// that path applies (`IF_OFF` on the non-halt early-ISR grid, else `CC_OFF`).
    /// The min-event idle fast path lands precisely on this cc so the overflow
    /// fires at the identical cc the per-dot crank would have. `None` when the
    /// timer is disabled / no overflow scheduled.
    pub(crate) fn next_overflow_fire_cc(&self, cpu_halted: bool) -> Option<u64> {
        if self.tac & TAC_ENABLE == 0 || self.next_irq_event_time == DISABLED_TIME {
            return None;
        }
        let early = !cpu_halted && self.isr_on_early_grid;
        let fold = if early { IF_OFF as u64 } else { CC_OFF as u64 };
        Some(self.next_irq_event_time.wrapping_add(fold))
    }

    /// Early (EI-loop) anchor cc of the next scheduled overflow:
    /// `next_irq_event_time + IF_OFF`.
    /// The non-halt fast dispatch fires the overflow once the boundary reaches this.
    pub(crate) fn next_overflow_ei_cc(&self) -> Option<u64> {
        if self.tac & TAC_ENABLE != 0 && self.next_irq_event_time != DISABLED_TIME {
            Some(self.next_irq_event_time.wrapping_add(IF_OFF as u64))
        } else {
            None
        }
    }

    pub fn abs_cc(&self) -> u64 {
        self.abs_cc
    }

    pub(crate) fn div_reset_count(&self) -> u64 {
        self.div_reset_count
    }

    /// The APU-visible divider anchor of the most recent DIV write. The APU
    /// divider-reset fold runs at this cc, matching the master counter it folds
    /// on. Currently equal to `div_anchor` for every write.
    pub(crate) fn div_anchor_apu(&self) -> u64 {
        self.div_anchor_apu
    }

    /// The 16-bit DIV divider: a pure derivation of the master counter and the
    /// last DIV-write anchor: the low 16 bits of `abs_cc - div_anchor`.
    fn divider(&self) -> u16 {
        (self.abs_cc.wrapping_sub(self.div_anchor) & 0xFFFF) as u16
    }

    fn clk(&self) -> u32 {
        TIMA_CLOCK[(self.tac & TAC_FREQUENCY_MASK) as usize]
    }

    /// T-cycles from a TIMA counter value of `from` until it next wraps past
    /// 0xFF. TIMA advances once every `2^clk` T-cycles, and `0x100 - from`
    /// increments remain before it rolls over, so the elapsed span is the count
    /// of remaining increments scaled by the per-increment T-cycle stride.
    fn cycles_to_overflow(&self, from: u8) -> u64 {
        (0x100 - from as u64) << self.clk()
    }

    /// The post-overflow TMA reload is observable for a 4-T-cycle (one M-cycle)
    /// window that opens at `tmatime`; TIMA reads 0 until then. Report whether `cc`
    /// has entered that window (in which case an observed TIMA now reads back as
    /// TMA), and retire the window once `cc` has passed fully beyond it.
    /// Pan Docs: Timer obscure behaviour — https://gbdev.io/pandocs/Timer_Obscure_Behaviour.html
    fn tma_reload_window_reached(&mut self, cc: u64) -> bool {
        if cc >= self.tmatime {
            if cc >= self.tmatime.wrapping_add(4) {
                self.tmatime = DISABLED_TIME;
            }
            true
        } else {
            false
        }
    }

    /// cc at which a CPU register access resolves: the raw master cc. This is the
    /// per-access cc the timer, serial, and APU all resolve register accesses on.
    pub(crate) fn access_cc(&self) -> u64 {
        // The CPU positions `abs_cc` at the access M-cycle start before any tick,
        // so `abs_cc` is the access cc.
        self.abs_cc
    }

    /// cc at which a CPU register write resolves. The write side is a separate
    /// sub-quantum phase from the read `access_cc()`: the APU/serial trigger
    /// boundary math rounds differently from the read event, so it carries its
    /// own offset (`WRITE_CC_OFF`).
    pub(crate) fn write_access_cc(&self) -> u64 {
        (self.abs_cc as i64 + WRITE_CC_OFF) as u64
    }


    /// Consume the scheduled overflow: mark the IRQ pending and re-arm the
    /// schedule one full reload cycle later. After a reload TIMA restarts at TMA,
    /// so the next overflow lands `cycles_to_overflow(TMA)` T-cycles on.
    fn do_irq_event(&mut self) {
        self.pending_irq = true;
        self.next_irq_event_time = self
            .next_irq_event_time
            .wrapping_add(self.cycles_to_overflow(self.tma));
    }

    /// Flag every scheduled overflow whose fire cc is at or before `cc`. `cc` is
    /// the register access cc (raw `abs_cc`). A glitch IRQ flagged from a register
    /// write compares against the same access cc the schedule was set on, so it is
    /// self-consistent in either anchor space.
    fn update_irq(&mut self, cc: u64) {
        while self.next_irq_event_time != DISABLED_TIME && cc >= self.next_irq_event_time {
            self.do_irq_event();
        }
    }

    /// IRQ delivery path (raw `abs_cc` per-dot). The schedule is anchored in
    /// access-cc space, so the delivery comparison adds the `fold` (`CC_OFF`, or
    /// `IF_OFF` on the early grid) back to keep the absolute fire cc unchanged.
    fn update_irq_delivery(&mut self, abs_cc: u64, cpu_halted: bool) {
        // The IF bit is normally raised at the late anchor (`CC_OFF`) so HALT-wakeup
        // detection and the IF re-flag observation stay on the late grid. The
        // non-halt EI-loop fast dispatch is handled by `force_ei_delivery`.
        //
        // Once the ISR runs on the early grid (`isr_on_early_grid`), an unserviced
        // overflow that only re-flags IF mid-ISR (a second overflow with IME off)
        // must also raise IF on the early anchor — otherwise it sits CC_OFF-IF_OFF
        // cc late vs the ISR's own early IF write/re-trigger. Gated to the non-halt
        // early-grid context; HALTed or OFF keeps the baseline `CC_OFF` grid. The
        // timer-bit read is also sampled at the access cc in this context (see
        // bus.rs) so a read-only ISR still misses an overflow that has not flagged
        // at its read cc.
        let early = !cpu_halted && self.isr_on_early_grid;
        let fold = if early { IF_OFF as u64 } else { CC_OFF as u64 };
        while self.next_irq_event_time != DISABLED_TIME
            && abs_cc >= self.next_irq_event_time.wrapping_add(fold)
        {
            // Record the deliverable (IF-visible) fire cc before do_irq_event
            // advances next_irq_event_time to the following period. The CPU's
            // event-cc gate compares the boundary access cc against this. Only
            // record while none is pending so a back-to-back overflow keeps the
            // earliest undispatched fire.
            if self.last_fire_cc == DISABLED_TIME {
                self.last_fire_cc = self.next_irq_event_time.wrapping_add(CC_OFF as u64);
                self.last_fire_cc_ei = self.next_irq_event_time.wrapping_add(IF_OFF as u64);
            }
            self.do_irq_event();
        }
    }

    /// IF-register (FF0F) store collision: the store first pumps timer events at
    /// the write cc, so an overflow whose schedule cc has been reached
    /// (`next_irq_event_time <= write cc`) flags IF before the store, and the CPU write then
    /// wins the collision on the same M-cycle. Leaves the IF raise to the caller
    /// (`take_pending_irq`); records the dispatch bookkeeping like the per-dot
    /// delivery so a surviving (re-set) bit keeps its fire-cc gate.
    pub(crate) fn flush_overflow_for_ifreg_write(&mut self) {
        if self.tac & TAC_ENABLE == 0 {
            return;
        }
        let cc = self.write_access_cc();
        while self.next_irq_event_time != DISABLED_TIME && cc >= self.next_irq_event_time {
            if self.last_fire_cc == DISABLED_TIME {
                self.last_fire_cc = self.next_irq_event_time.wrapping_add(CC_OFF as u64);
                self.last_fire_cc_ei = self.next_irq_event_time.wrapping_add(IF_OFF as u64);
            }
            self.do_irq_event();
        }
    }

    /// EI-loop fast timer delivery. In a non-halt/non-stop EI loop the CPU calls
    /// this at the early anchor (`boundary >= next_irq_event_time + IF_OFF`) to fire an
    /// imminent overflow before its normal `CC_OFF`-late per-dot delivery, so the
    /// serviced ISR (and any TAC re-write) runs on the correct divider phase.
    /// Mirrors `update_irq_delivery` but keyed on the early anchor. Returns true if
    /// it fired one. Idempotent vs the per-dot path: `do_irq_event` advances
    /// `next_irq_event_time`, so the later `CC_OFF` delivery will not re-fire it.
    pub(crate) fn force_ei_delivery(&mut self, boundary: u64) -> bool {
        if self.tac & TAC_ENABLE == 0 {
            return false;
        }
        let mut fired = false;
        while self.next_irq_event_time != DISABLED_TIME
            && boundary >= self.next_irq_event_time.wrapping_add(IF_OFF as u64)
        {
            if self.last_fire_cc == DISABLED_TIME {
                self.last_fire_cc = self.next_irq_event_time.wrapping_add(CC_OFF as u64);
                self.last_fire_cc_ei = self.next_irq_event_time.wrapping_add(IF_OFF as u64);
            }
            self.do_irq_event();
            fired = true;
        }
        if fired {
            // Sticky: the ISR this dispatch enters runs on the EARLY grid, so a
            // mid-ISR overflow re-flags IF early and FF0F reads/writes resolve on
            // that grid (see `update_irq_delivery` / the FF0F read+write in bus.rs).
            self.isr_on_early_grid = true;
        }
        fired
    }

    /// Is the current ISR running on the early (`IF_OFF`) grid (set by
    /// `force_ei_delivery`, cleared on HALT entry)? When true the unserviced
    /// overflow IF-set uses the early anchor and timer-bit reads sample at the
    /// access cc, so the ISR's IF write/read/re-trigger all resolve on that grid.
    pub(crate) fn isr_on_early_grid(&self) -> bool {
        self.isr_on_early_grid && self.tac & TAC_ENABLE != 0
    }

    /// Materialize the lazily-derived TIMA counter up to time `cc`. TIMA ticks
    /// once per `2^clk` T-cycles; the number of whole ticks since the value was
    /// last committed drives the counter forward, and `tima_last_update` is
    /// snapped to the last tick boundary so the residual sub-tick phase carries
    /// into the next call.
    fn update_tima(&mut self, cc: u64) {
        let clk = self.clk();
        let ticks = (cc - self.tima_last_update) >> clk;
        self.tima_last_update += ticks << clk;

        // A reload from an earlier overflow may already be visible at `cc`.
        if self.tma_reload_window_reached(cc) {
            self.tima = self.tma;
        }

        let mut tmp = self.tima as u64 + ticks;
        if tmp > 0x100 {
            tmp = settle_tima_overflow(tmp, self.tma);
        }

        // `tmp == 0x100` is the exact-overflow instant: the counter reads 0 and a
        // fresh TMA-reload window opens `TMA_OFF` T-cycles after the tick, which
        // itself may already be visible at `cc`.
        if tmp == 0x100 {
            tmp = 0;
            self.tmatime = self.tima_last_update + TMA_OFF;
            if self.tma_reload_window_reached(cc) {
                tmp = self.tma as u64;
            }
        }

        self.tima = tmp as u8;
    }

    /// Store TIMA (FF05). While the timer runs, settle any due overflow and the
    /// derived counter to the access cc first, cancel a reload window that is
    /// about to close (writing during it would otherwise clobber the value), then
    /// re-arm the overflow schedule from the freshly written value.
    fn set_tima(&mut self, data: u8) {
        let cc = self.access_cc();
        if self.tac & TAC_ENABLE != 0 {
            self.update_irq(cc);
            self.update_tima(cc);
            if self.tmatime.wrapping_sub(cc) < 4 {
                self.tmatime = DISABLED_TIME;
            }
            self.next_irq_event_time =
                self.tima_last_update + self.cycles_to_overflow(data) + TMA_OFF;
        }
        self.tima = data;
    }

    /// Store TMA (FF06). A running timer needs its pending overflow and derived
    /// counter brought current before the reload value changes.
    fn set_tma(&mut self, data: u8) {
        let cc = self.access_cc();
        if self.tac & TAC_ENABLE != 0 {
            self.update_irq(cc);
            self.update_tima(cc);
        }
        self.tma = data;
    }

    /// Store TAC (FF07). Changing the enable bit or the frequency selection can
    /// produce a spurious TIMA increment when the DIV bit feeding TIMA sees a
    /// falling edge (documented: Pan Docs Timer Obscure Behaviour; TCAGBD §5.5
    /// TAC-write glitch pseudocode).
    ///
    /// AGB takes the SAME path as DMG/CGB here. An earlier AGB-only enable quirk
    /// (bump TIMA by one when the frequency change moves the feeding DIV bit
    /// high->low) has been REMOVED: it was an undocumented refinement of TCAGBD
    /// §5.5's remark that "AGB and AGS seem to have strange behaviour", which
    /// TCAGBD itself calls a race that "cannot be predicted for every device", and
    /// it had no oracle behind it. Grading AntonioND's real-silicon AGB captures
    /// (gbc-hw-tests, `rev=agb`) contradicts it directly: on timers/timer_reset_test
    /// the real GBA-SP capture is BYTE-IDENTICAL to the real CGB capture, and the
    /// quirk was the sole reason our AGB diverged (TIMA 0x02 vs the captured 0x01).
    /// Dropping it fixes that row and changes nothing else anywhere in the corpus.
    /// Pan Docs: Timer obscure behaviour — https://gbdev.io/pandocs/Timer_Obscure_Behaviour.html
    /// Whether a TAC write that ENABLES the timer over a DISABLED old TAC
    /// glitch-ticks TIMA once. DMG never does: with the old TAC disabled the
    /// edge detector's input was already 0, so there is no falling edge — that
    /// is AntonioND's DMG rule ("if OLD_TAC disabled -> GLITCH = 0",
    /// tac_set_everything/DMG.txt), and his GBC notes mark the same case
    /// `XXXXXXXXX`, i.e. explicitly NOT the DMG rule.
    ///
    /// On CGB/AGB the old mux output is not gated by the old enable bit, so the
    /// detector still sees `SYS[old_bit] -> SYS[new_bit]` and falls when the old
    /// bit is high and the new one is low (the guard below). WHICH bit pair
    /// actually latches that narrow pulse is per-family and NOT derivable from
    /// the guard alone — it is a propagation-delay race, so each pair is taken
    /// only where two physical units agree:
    ///   - CGB, D64 -> E256: the author committed two CGB captures of
    ///     tac_set_everything that differ at 223 bytes, EVERY one of them in the
    ///     old-TAC-disabled region (the old-TAC-enabled half is byte-identical)
    ///     -- yet both agree on this pair. Reproduces real_gbc.sav byte-exactly
    ///     on tac_set_disabled (0 mismatches / 14336 cells).
    ///   - AGB, D16 -> E256: both AGB units (real_gba, real_gba_sp) glitch here,
    ///     and the author annotates the cell `(GBA 1)` in
    ///     tac_set_when_inc_*/info_*.txt.
    ///
    /// The neighbouring pairs are deliberately NOT modelled: the two CGB units
    /// contradict each other on D1024->E16/E256, D64->E16 and D256->E64, and the
    /// two AGB units contradict each other on everything except D16->E256.
    /// Fitting those would be fitting one die's metastability.
    ///
    /// This is NOT the AGB quirk removed in 989e45d0. That one fired on old-TAC-
    /// ENABLED writes and broke timers/timer_reset_test; this path is unreachable
    /// unless the old TAC is disabled, so that row stays fixed.
    ///
    /// The CGB TIMA-parity term is a ripple-carry pulse width effect: a seed
    /// whose low bit is 1 needs the carry to propagate past bit 0, which the
    /// narrow glitch pulse only achieves when SYS bit 3 is also high (widening
    /// it). Derived from tac_set_disabled's 7 TIMA seeds, which split exactly on
    /// bit 0 and on no other property.
    fn disabled_tac_write_glitch(&self, data: u8, cc: u64) -> bool {
        let sys = cc.wrapping_sub(self.div_anchor);
        let old_bit = 1u64 << (TIMA_CLOCK[(self.tac & TAC_FREQUENCY_MASK) as usize] - 1);
        let new_bit = 1u64 << (TIMA_CLOCK[(data & TAC_FREQUENCY_MASK) as usize] - 1);
        // The falling edge itself: old-selected bit high, new-selected bit low.
        if sys & old_bit == 0 || sys & new_bit != 0 {
            return false;
        }
        if self.is_agb {
            old_bit == 8 && new_bit == 128
        } else if self.is_cgb {
            old_bit == 32 && new_bit == 128 && (self.tima & 1 == 0 || sys & 8 != 0)
        } else {
            false
        }
    }

    fn set_tac(&mut self, data: u8) {
        let cc = self.access_cc();
        if (self.tac ^ data) != 0 {
            let mut next = self.next_irq_event_time;

            if self.tac & TAC_ENABLE != 0 {
                let old_clk = self.clk();
                // The stale edge produces a half-period back-shift (one extra
                // tick) unless the new setting keeps the feeding bit high: the
                // timer must remain enabled AND the newly-selected DIV bit must
                // currently read 1. TMA_OFF is the constant schedule bias.
                let new_enabled = data & TAC_ENABLE != 0;
                let new_bit_high = ((cc - self.div_anchor)
                    >> (TIMA_CLOCK[(data & 3) as usize] - 1))
                    & 1
                    != 0;
                let shift = if new_enabled && new_bit_high {
                    TMA_OFF
                } else {
                    (1u64 << (old_clk - 1)) + TMA_OFF
                };
                self.tima_last_update = self.tima_last_update.wrapping_sub(shift);
                next = next.wrapping_sub(shift);
                if next != DISABLED_TIME && cc >= next {
                    self.pending_irq = true;
                }
                self.update_tima(cc);
                self.tmatime = DISABLED_TIME;
                next = DISABLED_TIME;
            }

            // Enabling the timer over a DISABLED old TAC: on CGB/AGB silicon the
            // stale mux output can still glitch a tick through. Applied before
            // the re-anchor below so the fresh overflow schedule starts from the
            // bumped TIMA.
            if self.tac & TAC_ENABLE == 0
                && data & TAC_ENABLE != 0
                && self.disabled_tac_write_glitch(data, cc)
            {
                // A real tick, not a bare +1: it carries into an overflow, which
                // reloads TMA and raises the timer IRQ. AntonioND's CGB capture
                // pins this — the TIMA=0xFF glitch cells read back TIMA=TMA(0x00)
                // WITH IF bit 2 set (tac_set_disabled, old TAC D64 -> E256).
                if self.tima == 0xFF {
                    self.tima = self.tma;
                    self.pending_irq = true;
                } else {
                    self.tima += 1;
                }
            }

            if data & TAC_ENABLE != 0 {
                // Re-anchor the tick grid to the current DIV phase for the new
                // frequency (drop the residual sub-tick bits of the divider), then
                // schedule the next overflow from the present TIMA value.
                let new_clk = TIMA_CLOCK[(data & 3) as usize];
                self.tima_last_update =
                    cc - ((cc - self.div_anchor) & ((1u64 << new_clk) - 1));
                next = self.tima_last_update + ((0x100 - self.tima as u64) << new_clk) + TMA_OFF;
            }

            self.next_irq_event_time = next;
        }
        self.tac = data;
    }

    /// DIV-write (FF04) reset. Resetting the divider can drop the DIV bit feeding
    /// TIMA, glitch-ticking TIMA once (schedule back-shift of `(1 << (clk-1)) + 3`).
    /// Documented: Pan Docs Timer Obscure Behaviour; TCAGBD §5.5 ("When writing to
    /// DIV register the TIMA register can be increased if the counter has reached
    /// half the clocks it needs to increase").
    /// `cc` is the resolution cc: `access_cc()` for a CPU FF04 write, raw `abs_cc`
    /// for the STOP-internal reset.
    /// Pan Docs: Timer obscure behaviour — https://gbdev.io/pandocs/Timer_Obscure_Behaviour.html
    fn div_reset_at(&mut self, cc: u64) {
        self.div_reset_split(cc, cc);
    }

    /// Divider reset allowing the TIMA-glitch/reset value to be resolved at
    /// `tima_cc` while the divider/derivation anchor lands at `anchor_cc`. For a
    /// normal FF04 write these are equal (`div_reset_at`); the CGB STOP speed
    /// switch passes the true switch cc as `tima_cc` (so the reset TIMA matches the
    /// hardware grid) and the read-grid anchor as `anchor_cc` (so post-switch
    /// `read_cc - div_anchor` resolves the divider at the exact read cc). The TIMA
    /// tick grid (`tima_last_update`) and IRQ schedule are based on `anchor_cc`
    /// since post-switch reads/IRQs all arrive on that same read grid.
    fn div_reset_split(&mut self, tima_cc: u64, anchor_cc: u64) {
        self.div_reset_split_hold(tima_cc, anchor_cc, 0);
    }

    /// `div_reset_split` with a CGB-D/E hold: on CGB-D/E the STOP speed-switch DIV
    /// reset's immediate TIMA increment lands one M-cycle later for the 65KHz/16KHz
    /// clocks. `de_hold` (0 or 4) shrinks the divider-phase back-shift so the
    /// glitch tick crosses 4 cc later. Not in Pan Docs, TCAGBD, or GBCTR; TCAGBD
    /// §5.5 only notes generally that "different revisions of the GBC have a
    /// different behaviour", not this STOP-speed-switch per-revision TIMA timing.
    /// Derived from age spsw-tima.
    fn div_reset_split_hold(&mut self, tima_cc: u64, anchor_cc: u64, de_hold: u64) {
        if self.tac & TAC_ENABLE != 0 {
            let clk = self.clk();
            // Resetting the divider drops the feeding DIV bit, so TIMA glitch-ticks
            // once: a half-period back-shift of `tima_last_update` and the schedule,
            // biased by the constant TMA_OFF and shortened by the CGB-D/E hold.
            let shift = (1u64 << (clk - 1)) + TMA_OFF - de_hold;
            self.tima_last_update = self.tima_last_update.wrapping_sub(shift);
            if self.next_irq_event_time != DISABLED_TIME {
                self.next_irq_event_time = self.next_irq_event_time.wrapping_sub(shift);
                if tima_cc >= self.next_irq_event_time {
                    self.pending_irq = true;
                }
            }
            // Settle the derived TIMA up to the true switch cc to capture the
            // post-reset value, then re-anchor the tick grid and overflow schedule
            // to the read-grid `anchor_cc` so subsequent `read_cc - tima_last_update`
            // resolves on the same grid reads arrive on.
            self.update_tima(tima_cc);
            self.tima_last_update = anchor_cc;
            self.next_irq_event_time =
                self.tima_last_update + self.cycles_to_overflow(self.tima) + TMA_OFF;
        }
        self.div_anchor = anchor_cc;
        // Normal FF04 writes share one cc for the DIV register and the APU fold;
        // the STOP path overrides `div_anchor_apu` afterward with its own offset.
        self.div_anchor_apu = anchor_cc;
        self.div_reset_count = self.div_reset_count.wrapping_add(1);
    }

    /// CGB STOP speed switch divider/TIMA re-derivation. The divider continues
    /// ticking from the switch cc at the new speed; the DIV-write reset / tick-grid / APU
    /// fold all anchor at the bare `abs_cc`. CGB-D/E (`cgb_de`) delays the
    /// speed-switch DIV-reset immediate TIMA increment by one M-cycle for the
    /// 65KHz/16KHz clocks (TAC&3 >= 2); the 4KHz/262KHz clocks are revision-common.
    /// See `div_reset_split_hold`. STOP DIV reset — Pan Docs: Timer and Divider
    /// Registers — https://gbdev.io/pandocs/Timer_and_Divider_Registers.html
    pub(crate) fn stop_div_reset(&mut self, cgb_de: bool) {
        let anchor_cc = self.abs_cc;
        let de_hold = if cgb_de && (self.tac & TAC_FREQUENCY_MASK) >= 2 { 4 } else { 0 };
        self.div_reset_split_hold(anchor_cc, anchor_cc, de_hold);
        self.div_anchor_apu = anchor_cc;
    }

    /// Initialize the timer's internal 16-bit counter (used at boot to seed the
    /// divider: the low 16 bits of `abs_cc - div_anchor`).
    pub(crate) fn set_internal_counter(&mut self, value: u16) {
        self.abs_cc = value as u64;
        self.div_anchor = 0;
        self.div_anchor_apu = 0;
        self.tima_last_update = self.abs_cc;
    }

    pub(crate) fn internal_counter(&self) -> u16 {
        self.divider()
    }

    /// CGB STOP speed switch. Pulls the timer's tick anchor (and the scheduled IRQ
    /// time) back by 4 T-cycles for an enabled fast-frequency timer
    /// (`tac & 0x07 >= 0x05`), in either speed direction.
    pub(crate) fn speed_change(&mut self) {
        let fast = (self.tac & 0x07) >= 0x05;
        if fast {
            self.tima_last_update = self.tima_last_update.wrapping_sub(4);
            if self.next_irq_event_time != DISABLED_TIME {
                self.next_irq_event_time = self.next_irq_event_time.wrapping_sub(4);
            }
        }
    }

    /// Raise the pending TIMA IRQ (if any) into `mmio`. Called from `step` and
    /// immediately after a write that may have flagged a glitch IRQ.
    pub(crate) fn flush_pending_irq(&mut self, mmio: &mut mmio::Mmio) {
        if self.pending_irq {
            self.pending_irq = false;
            mmio.request_interrupt(cpu::registers::InterruptFlag::Timer);
        }
    }

    pub(crate) fn take_pending_irq(&mut self) -> bool {
        let p = self.pending_irq;
        self.pending_irq = false;
        p
    }

    /// A HALT (entry or wakeup) ends any prior EI fast-dispatch stream — the next
    /// ISR is HALT-driven and observes the IF re-flag on the late grid. Clears the
    /// early-grid stick.
    pub(crate) fn clear_isr_early_grid(&mut self) {
        self.isr_on_early_grid = false;
    }

    /// Advance the timer one dot. Takes the two hardware flags it needs by value
    /// (instead of borrowing mmio) and returns whether a TIMA overflow IRQ
    /// should be raised. The caller (`Mmio::step_timer`) applies it to mmio.
    /// Keeping mmio out of here lets the timer step in place with no per-dot
    /// clone (it never touches its own copy inside mmio anyway).
    pub fn step(&mut self, ds: bool, cpu_halted: bool) -> bool {
        self.abs_cc = self.abs_cc.wrapping_add(1);

        // Scheduled TIMA IRQ: fire any event whose absolute cc has now passed. The
        // IRQ is a per-dot event keyed on the raw `abs_cc` (the cc the IF bit
        // becomes visible to the CPU), whereas register read/write effects resolve
        // at `access_cc()`. These are deliberately different anchors: the IF-visible
        // cc trails the scheduled `next_irq_event_time` (in access-cc space) by
        // `CC_OFF` dots, matching the hardware's late IRQ sampling.
        if self.tac & TAC_ENABLE != 0 {
            self.update_irq_delivery(self.abs_cc, cpu_halted);
        }
        let timer_irq = if self.pending_irq {
            self.pending_irq = false;
            true
        } else {
            false
        };

        // Recorded for `quiet_until`, which bounds the idle fast path at the
        // next DIV-bit-12 (bit-13 in double speed) falling edge.
        self.last_double_speed = ds;
        timer_irq
    }

    /// Raw one-dot master-clock bump for the quiet-span fast loop: byte-
    /// identical to `step` for any dot proven to cross no scheduled overflow
    /// delivery and no APU FS edge (see `quiet_until`) — `update_irq_delivery`
    /// is then a no-op (its while-loop condition is keyed on absolute ccs and
    /// a later call drains at the identical ccs) and `pending_irq` stays false.
    #[inline]
    pub(crate) fn bump_cc_one(&mut self) {
        self.abs_cc = self.abs_cc.wrapping_add(1);
    }

    /// n-dot variant of `bump_cc_one`; every bumped dot must lie strictly
    /// below `quiet_until`.
    #[inline]
    pub(crate) fn bump_cc_by(&mut self, n: u64) {
        self.abs_cc = self.abs_cc.wrapping_add(n);
    }

    /// Exclusive upper bound up to which per-dot `step` is a pure `abs_cc`
    /// increment: the earlier of the next scheduled overflow delivery cc and
    /// the next APU frame-sequencer edge cc. A pending undelivered IRQ or a
    /// due event yields `abs_cc` (no quiet span).
    pub(crate) fn quiet_until(&self, cpu_halted: bool) -> u64 {
        if self.pending_irq {
            return self.abs_cc;
        }
        let mut bound = u64::MAX;
        if let Some(cc) = self.next_overflow_fire_cc(cpu_halted) {
            bound = bound.min(cc);
        }
        let shift = if self.last_double_speed { 14 } else { 13 };
        let cnt = self.abs_cc.wrapping_sub(self.div_anchor);
        let next_edge = self
            .div_anchor
            .wrapping_add(((cnt >> shift) + 1) << shift);
        bound.min(next_edge)
    }

    /// Bulk-advance the timer directly to `target_abs_cc` (>= current abs_cc),
    /// producing the byte-identical net effect of calling `step` once per
    /// intervening dot. Every part of `step` is span-based:
    /// `update_irq_delivery` is a `while` loop keyed on the absolute cc (it drains
    /// all overflows due <= abs_cc, so a single call at the final cc fires the same
    /// set as the per-dot calls). The only per-dot bookkeeping is the
    /// `abs_cc += 1`, which is collapsed to a single assignment here. This is the
    /// timer half of the min-event-jump idle fast path (`Bus::run_to`); it is only
    /// invoked when the world is provably idle except for the timer (LCD off, no
    /// DMA/HDMA, audio off, serial idle), so no other peripheral's per-dot state is
    /// skipped.
    pub(crate) fn step_to(&mut self, target_abs_cc: u64, mmio: &mut mmio::Mmio) {
        if target_abs_cc <= self.abs_cc {
            return;
        }
        self.abs_cc = target_abs_cc;
        if self.tac & TAC_ENABLE != 0 {
            self.update_irq_delivery(self.abs_cc, mmio.cpu_is_halted());
        }
        self.flush_pending_irq(mmio);
        self.last_double_speed = mmio.is_double_speed_mode();
    }
}

impl Addressable for Timer {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            DIV => {
                let cc = self.access_cc();
                let div = (cc.wrapping_sub(self.div_anchor) & 0xFFFF) as u16;
                (div >> 8) as u8
            }
            // TIMA derives lazily; `read` takes `&self`, so reproduce
            // `update_tima` arithmetically without mutating.
            TIMA => {
                if self.tac & TAC_ENABLE == 0 {
                    self.tima
                } else {
                    let cc = self.access_cc();
                    let clk = self.clk();
                    let ticks = (cc - self.tima_last_update) >> clk;
                    let mut tima = self.tima;
                    if cc >= self.tmatime {
                        tima = self.tma;
                    }
                    let mut tmp = tima as u64 + ticks;
                    if tmp > 0x100 {
                        tmp = settle_tima_overflow(tmp, self.tma);
                    }
                    if tmp == 0x100 {
                        let tmatime = self.tima_last_update + (ticks << clk) + TMA_OFF;
                        tmp = if cc >= tmatime { self.tma as u64 } else { 0 };
                    }
                    tmp as u8
                }
            }
            TMA => self.tma,
            TAC => self.tac,
            _ => panic!("Timer: Invalid read address {:04X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            DIV => {
                let cc = self.access_cc();
                self.div_reset_at(cc);
            }
            TIMA => self.set_tima(value),
            TMA => self.set_tma(value),
            TAC => self.set_tac(value & 0b00000111),
            _ => panic!("Timer: Invalid write address {:04X}", addr),
        }
    }
}

#[cfg(test)]
mod disabled_tac_write_glitch_tests {
    //! The old-TAC-DISABLED TAC-write glitch. DMG has none; CGB latches it for
    //! D64 -> E256 and AGB for D16 -> E256. Both pairs are the ones two physical
    //! units agree on in AntonioND's gbc-hw-tests captures -- see
    //! `Timer::disabled_tac_write_glitch` for the provenance and for why the
    //! neighbouring pairs are deliberately unmodelled.
    use super::*;

    /// TAC encodings used here: `Dn` disabled / `En` enabled, n = clocks per tick.
    const D16: u8 = 0b001;
    const D64: u8 = 0b010;
    const E256: u8 = 0b111;
    const E64: u8 = 0b110;

    /// Drive one `tac_set_disabled`-shaped probe: seed TIMA/TMA, reset DIV, let
    /// the divider reach `sys`, then write the new TAC. Returns (TIMA, IRQ).
    fn probe(cgb: bool, agb: bool, old_tac: u8, new_tac: u8, sys: u64, tima: u8) -> (u8, bool) {
        let mut t = Timer::new();
        t.set_cgb(cgb);
        t.set_agb(agb);
        t.bump_cc_by(0x1000);
        t.write(TAC, old_tac);
        t.write(TMA, 0);
        t.write(DIV, 0);
        t.write(TIMA, tima);
        t.bump_cc_by(sys);
        t.write(TAC, new_tac);
        (t.read(TIMA), t.take_pending_irq())
    }

    /// The falling edge the glitch rides: old-selected DIV bit high (D64 -> bit
    /// 5), new-selected bit low (E256 -> bit 7). SYS=32 satisfies both.
    #[test]
    fn cgb_latches_d64_to_e256_and_dmg_never_does() {
        assert_eq!(probe(true, false, D64, E256, 32, 0x10).0, 0x11, "CGB must glitch D64 -> E256");
        assert_eq!(probe(false, false, D64, E256, 32, 0x10).0, 0x10, "DMG must never glitch");
    }

    /// The CGB parity term: an odd TIMA needs the carry to propagate past bit 0,
    /// which the narrow pulse only manages when SYS bit 3 is also high. SYS=32
    /// has bit 3 clear, SYS=40 has it set.
    #[test]
    fn cgb_odd_tima_needs_sys_bit3() {
        assert_eq!(probe(true, false, D64, E256, 32, 0x11).0, 0x11, "odd TIMA, SYS bit3 clear");
        assert_eq!(probe(true, false, D64, E256, 40, 0x11).0, 0x12, "odd TIMA, SYS bit3 set");
        // An even TIMA latches either way.
        assert_eq!(probe(true, false, D64, E256, 32, 0x10).0, 0x11);
        assert_eq!(probe(true, false, D64, E256, 40, 0x10).0, 0x11);
    }

    /// AGB takes D16 -> E256 instead, and does NOT take the CGB pair.
    #[test]
    fn agb_latches_d16_to_e256_only() {
        assert_eq!(probe(true, true, D16, E256, 40, 0x10).0, 0x11, "AGB must glitch D16 -> E256");
        assert_eq!(probe(true, true, D64, E256, 32, 0x10).0, 0x10, "AGB must not take the CGB pair");
        assert_eq!(probe(true, false, D16, E256, 40, 0x10).0, 0x10, "CGB must not take the AGB pair");
    }

    /// Pairs neither family latches stay clean even when the falling edge is
    /// present (D64 -> E64 shares a bit, D16 -> E64 is an unmodelled pair).
    #[test]
    fn unmodelled_pairs_stay_clean() {
        for (cgb, agb) in [(true, false), (true, true)] {
            assert_eq!(probe(cgb, agb, D64, E64, 32, 0x10).0, 0x10);
            assert_eq!(probe(cgb, agb, D16, E64, 40, 0x10).0, 0x10);
        }
    }

    /// The glitch is a real tick: from 0xFF it overflows, reloading TMA and
    /// raising the timer IRQ. Both cells are read straight off the CGB capture
    /// (tac_set_disabled, old TAC D64 -> E256, single speed):
    ///   seed 0xFF (odd, so SYS bit 3 required) at SYS=40 -> TIMA 0x00, IF&4=4
    ///   seed 0xFE (even) at SYS=32                       -> TIMA 0xFF, IF&4=0
    #[test]
    fn glitch_from_ff_overflows_and_raises_irq() {
        let (tima, irq) = probe(true, false, D64, E256, 40, 0xFF);
        assert_eq!(tima, 0x00, "overflow must reload TMA (0)");
        assert!(irq, "overflow must raise the timer IRQ");
        let (tima, irq) = probe(true, false, D64, E256, 32, 0xFE);
        assert_eq!(tima, 0xFF);
        assert!(!irq, "a non-overflowing glitch must not raise the IRQ");
    }

    /// An odd seed one short of overflow does NOT tick when SYS bit 3 is clear,
    /// so it must not overflow either -- the parity term gates the IRQ too.
    #[test]
    fn odd_ff_without_sys_bit3_neither_ticks_nor_fires() {
        let (tima, irq) = probe(true, false, D64, E256, 32, 0xFF);
        assert_eq!(tima, 0xFF, "no tick, so no overflow");
        assert!(!irq);
    }
}

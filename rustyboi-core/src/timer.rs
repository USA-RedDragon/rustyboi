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

// Gambatte `timaClock[]`: the DIV bit feeding TIMA for `tac & 3` is at
// `timaClock[tac&3] - 1` (i.e. 9/3/5/7). TIMA derives as `(cc-lastUpdate) >> clk`.
const TIMA_CLOCK: [u32; 4] = [10, 4, 6, 8];

// Gambatte `disabled_time`: a sentinel far in the future. The real `abs_cc` is a
// dot counter that never approaches this within any test, and all arithmetic on
// `tmatime`/`next_irq_event_time` is guarded by an explicit disabled check.
const DISABLED_TIME: u64 = u64::MAX;

// Offset mapping the per-dot `abs_cc` (incremented at the *start* of each dot's
// `step`, so it trails the live access cc by one dot) to the cc at which a CPU
// timer-register access resolves. A CPU access occupies a 4-dot M-cycle; its
// effect lands at the M-cycle end (`+4`), plus one dot for the start-of-step
// increment lag (`+1`) = `+5`. Empirically the sharp minimum of the tima suite
// (13 failures, below the 17 baseline) sits exactly at +5, confirming the
// scheduled-TIMA arithmetic is exact at this anchor.
const CC_OFF: i64 = 5;
/// EI-loop IF-visibility offset. The timer IF bit becomes visible at
/// `schedCc + IF_OFF` (vs the `CC_OFF`-late gate cc used by HALT/STOP). A non-halt
/// EI loop dispatches the IRQ at this early anchor so the ISR (and any TAC
/// re-write) runs on Gambatte's exact divider phase. HALT/STOP keep `CC_OFF`.
const IF_OFF: i64 = 1;
/// Write-side canonical access-cc offset (M8). Swept against the ch2
/// `late_reset_nr52` a/b pairs; the trigger's length boundary lands at this
/// phase rather than the read's `CC_OFF`.
const WRITE_CC_OFF: i64 = 0;
/// STOP speed-switch DIV/TIMA *derivation anchor* offset, relative to the per-dot
/// `abs_cc` (the STOP instruction's start cc). This is the cc the post-switch
/// divider derivation (`read_cc - div_anchor`) and the TIMA tick grid resolve
/// against. The engine's post-switch TIMA register READ resolves at a cc that
/// trails Gambatte's read cc by a fixed amount K (the prefetch access-cc skew:
/// K=8 single-speed entry, K=4 double-speed entry), while Gambatte's switch cc is
/// `instr_start + (ds ? 0 : 4)`. The boundary-exact derivation anchor is therefore
/// `switch_cc - K = abs_cc - 4` in BOTH speed directions (traced byte-exact on the
/// full speedchange `_1a/_1b/_2a/_2b` bracket families against the Gambatte
/// cctracer: this single value passes BOTH sub-dot probe sides simultaneously,
/// confirming it is the exact divider-phase derivation and not a swap).
const STOP_DERIV_OFF: i64 = -4;
/// STOP derivation-anchor adjustment when the enclosing EI-loop ISR was force-
/// delivered the timer IRQ early (`Timer::ei_promoted`). The early service shifts
/// the STOP's `abs_cc`; +1 recovers the speedchange_tima post-switch divider phase
/// (swept against the speedchange `tima00..03` + `tima01_nop` families with the
/// EI fast-dispatch active). Env-overridable via RB_STOP_PROMOTE for calibration.
const STOP_EI_PROMOTE_ADJ: i64 = 0;
/// Extra master-cc added (on top of `STOP_DERIV_OFF`) to the cc the post-switch
/// *reset TIMA value* is computed at, so it lands on Gambatte's true switch cc.
/// Gambatte derives the reset TIMA at `instr_start + (ds ? 0 : 4)`, i.e. 4 cc
/// later than the derivation anchor in the single-speed (SS->DS) direction and
/// 0 cc later in the double-speed (DS->SS) direction (where `abs_cc` already maps
/// to Gambatte's switch cc). Decoupling the reset-TIMA cc from the derivation
/// anchor is what makes both bracket sides of the DS->SS families resolve at once
/// (a single shared cc gives the reset TIMA one short OR the read derivation one
/// long — they straddle a sub-cc boundary; see the trace in this module's notes).
const STOP_TIMA_SS_EXTRA: i64 = 0;
const STOP_TIMA_DS_EXTRA: i64 = 1;
/// APU-specific STOP speed-switch divReset anchor offset (relative to the
/// derivation anchor). The APU divReset fold (square duty + length `cc>>13`
/// boundary) re-anchors at a different sub-cycle phase than the TIMA/DIV
/// high-byte boundary, so it carries its own offset.
const STOP_APU_SS_EXTRA: i64 = 0;
const STOP_APU_DS_EXTRA: i64 = 2;

// Gambatte's `+3` constant in `tmatime`/`nextIrqEventTime` (mem/tima.cpp).
const TMA_OFF: u64 = 3;

// ds-engine STAGE 1/7: the timer register access cc is the RAW master cc
// (`abs_cc`, captured at the START of the CPU access M-cycle — proven by the
// cctracer LP0 oracle to be Gambatte's read/write cc); the old
// `access_cc()` = abs_cc + CC_OFF (=5) anchor and its RB_CC_OFF env sweep are
// gone. The IRQ DELIVERY path in `step()` still folds CC_OFF back in
// (`update_irq_delivery`) to keep the absolute fire cc unchanged.
//
// ds-engine STAGE 3/7: the closed-form (update-to-cc) APU frame sequencer is the
// single FS path (the per-dot edge-detect fallback is deleted).

#[derive(Serialize, Deserialize, Clone)]
pub struct Timer {
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
    // Absolute, never-reset T-cycle counter mirroring Gambatte's `cycleCounter_`.
    // This is the single source of time in this module: the DIV divider, the
    // scheduled TIMA, the serial clock and the APU frame sequencer all derive
    // from it.
    #[serde(default)]
    abs_cc: u64,
    // `abs_cc` value of the last DIV write (Gambatte `divLastUpdate_`). The DIV
    // divider is `(abs_cc - div_anchor) & 0xFFFF`.
    #[serde(default)]
    div_anchor: u64,
    // Monotonic count of DIV writes (each rebases `div_anchor`). The APU master
    // clock reads this to detect a DIV reset and apply Gambatte's
    // `PSG::divReset` cycle-counter fold.
    #[serde(default)]
    div_reset_count: u64,
    // Scheduled-TIMA state (Gambatte `Tima`): `tima_last_update` is the cc the
    // current TIMA value was computed at; TIMA derives as
    // `tima + ((cc - tima_last_update) >> clk)`. `tmatime` is the cc at which a
    // pending overflow's TMA-reload becomes visible. `next_irq_event_time` is the
    // cc at which the timer IRQ fires (Gambatte's scheduler slot for
    // `intevent_tima`). All three are absolute `abs_cc` values, so the IRQ is
    // delivered at the same anchor a start-cc CPU read of TIMA resolves on.
    #[serde(default)]
    tima_last_update: u64,
    #[serde(default = "disabled_time")]
    tmatime: u64,
    #[serde(default = "disabled_time")]
    next_irq_event_time: u64,
    // Deferred IRQ flag for the write-path glitches (setTac/divReset) that
    // Gambatte flags inline via `flagIrq`. The write path has no `mmio` borrow;
    // `step` (and the post-write flush in `mmio`) raise the actual IF bit.
    #[serde(default)]
    pending_irq: bool,
    // APU-visible divider anchor. Equals `div_anchor` for normal FF04 writes, but
    // for the CGB STOP speed-switch divReset it carries the APU's own switch-cc
    // offset (`STOP_APU_DS_OFF`), which is calibrated independently of the
    // TIMA/DIV-register `STOP_DS_OFF` (the square-duty sub-cycle phase rounds
    // differently from the TIMA/DIV high-byte boundary).
    #[serde(default)]
    div_anchor_apu: u64,
    // ds-engine STAGE 2 (RB_FAITHFUL event-cc dispatch): the raw-abs_cc cc at
    // which the most recent still-undispatched TIMA IRQ became deliverable (its
    // IF bit was raised). The CPU's faithful step gate makes the timer IRQ
    // serviceable only once the boundary access cc has reached this cc, instead
    // of off the instruction-start IF snapshot. DISABLED_TIME = none pending.
    #[serde(default = "disabled_time")]
    last_fire_cc: u64,
    // The EARLY (EI-loop) gate cc for the same undispatched IRQ: `schedCc + IF_OFF`.
    // The non-halt/non-stop dispatch gate uses this instead of `last_fire_cc`.
    #[serde(default = "disabled_time")]
    last_fire_cc_ei: u64,
    // ds-engine STAGE 3 (RB_LAZYPERIPH): the `abs_cc` up to and including which
    // the APU frame sequencer has been clocked. The closed-form FS counts
    // DIV-bit-12/13 falling edges in `(last_apu_cc, abs_cc]` instead of per-dot
    // edge detection. A DIV reset rebases this to the reset cc (the divider — and
    // thus the FS phase — restarts from the new anchor).
    #[serde(default)]
    last_apu_cc: u64,
    // Set when a timer IRQ was force-delivered to the EI loop at the early anchor
    // (`force_ei_delivery`); the ensuing ISR runs ~4cc earlier than the normal
    // +5-late service, so a STOP speed-switch issued inside that ISR enters its
    // divider-derivation `abs_cc` shifted, and `stop_div_reset` must compensate the
    // derivation anchor by `+STOP_EI_PROMOTE_ADJ`. Cleared by the STOP (consumed)
    // and never persists past one STOP.
    #[serde(skip, default)]
    ei_promoted: bool,
}

fn disabled_time() -> u64 {
    DISABLED_TIME
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
            last_apu_cc: 0,
            ei_promoted: false,
        }
    }

    /// STAGE 2 (RB_FAITHFUL): the cc the most recent still-undispatched TIMA IRQ
    /// became deliverable, or `None`. Cleared at dispatch via `clear_fire_cc`.
    pub fn pending_fire_cc(&self) -> Option<u64> {
        if self.last_fire_cc != DISABLED_TIME {
            Some(self.last_fire_cc)
        } else {
            None
        }
    }

    /// The EARLY (EI-loop) gate cc for the undispatched timer IRQ, or `None`.
    pub fn pending_fire_cc_ei(&self) -> Option<u64> {
        if self.last_fire_cc_ei != DISABLED_TIME {
            Some(self.last_fire_cc_ei)
        } else {
            None
        }
    }

    /// STAGE 2: clear the recorded fire cc after the CPU dispatches the IRQ.
    pub fn clear_fire_cc(&mut self) {
        self.last_fire_cc = DISABLED_TIME;
        self.last_fire_cc_ei = DISABLED_TIME;
    }

    /// The DELIVERY cc of the NEXT scheduled timer overflow (the cc at which its IF
    /// bit will be raised: `next_irq_event_time + CC_OFF`), or `None` if disabled.
    /// Used by the EI-loop fast-dispatch to promote an imminent overflow so the
    /// non-halt service runs on Gambatte's exact phase rather than +5 late.
    pub fn next_overflow_deliver_cc(&self) -> Option<u64> {
        if self.tac & TAC_ENABLE != 0 && self.next_irq_event_time != DISABLED_TIME {
            Some(self.next_irq_event_time.wrapping_add(CC_OFF as u64))
        } else {
            None
        }
    }

    /// EARLY (EI-loop) anchor cc of the next scheduled overflow: `schedCc + IF_OFF`.
    /// The non-halt fast dispatch fires the overflow once the boundary reaches this.
    pub fn next_overflow_ei_cc(&self) -> Option<u64> {
        if self.tac & TAC_ENABLE != 0 && self.next_irq_event_time != DISABLED_TIME {
            let if_off = std::env::var("RB_IF_OFF")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(IF_OFF);
            Some(self.next_irq_event_time.wrapping_add(if_off as u64))
        } else {
            None
        }
    }

    pub fn abs_cc(&self) -> u64 {
        self.abs_cc
    }

    pub fn div_reset_count(&self) -> u64 {
        self.div_reset_count
    }

    /// The access cc of the most recent DIV write (Gambatte `divLastUpdate_`).
    /// The APU divReset fold must run at this cc, matching the single
    /// `cycleCounter_` Gambatte folds on. Returns the APU-visible anchor, which
    /// for a STOP speed-switch divReset carries the APU-specific switch-cc offset
    /// (`div_anchor_apu` tracks `div_anchor` for every normal FF04 write and only
    /// diverges across a STOP speed switch).
    pub fn div_anchor(&self) -> u64 {
        self.div_anchor_apu
    }

    /// The 16-bit DIV divider: a pure derivation of the master counter and the
    /// last DIV-write anchor (Gambatte `(cc - divLastUpdate)`'s low 16 bits).
    fn divider(&self) -> u16 {
        (self.abs_cc.wrapping_sub(self.div_anchor) & 0xFFFF) as u16
    }

    fn clk(&self) -> u32 {
        TIMA_CLOCK[(self.tac & TAC_FREQUENCY_MASK) as usize]
    }

    /// cc at which a CPU register access resolves, relative to the per-dot
    /// `abs_cc` (tuning lever `CC_OFF`). This is the canonical per-access cc the
    /// timer, serial, and APU all resolve register accesses on (M7).
    pub fn access_cc(&self) -> u64 {
        // STAGE 1/7: the access resolves at the raw master cc (Gambatte read/write
        // cc), with no CC_OFF. The CPU positions `abs_cc` at the access M-cycle
        // start before any tick, so `abs_cc` IS the access cc. (RB_CC_OFF env
        // sweep deleted in stage 7.)
        self.abs_cc
    }

    /// cc at which a CPU register WRITE resolves. The write side is a separate
    /// sub-quantum phase term from the read `access_cc()` (M8): the APU/serial
    /// trigger boundary math (`nr4Change`, serial completion/abort) rounds
    /// differently from the read `event`, so its canonical write cc carries its
    /// own offset (`WRITE_CC_OFF`) rather than reusing the read's `CC_OFF`.
    pub fn write_access_cc(&self) -> u64 {
        (self.abs_cc as i64 + WRITE_CC_OFF) as u64
    }


    /// Gambatte `Tima::doIrqEvent`: flag the IRQ and advance the scheduled time
    /// by a full TIMA period. Returns `true` so the caller can raise the IF bit
    /// at the actual fire cc.
    fn do_irq_event(&mut self) {
        self.pending_irq = true;
        self.next_irq_event_time = self
            .next_irq_event_time
            .wrapping_add((256u64 - self.tma as u64) << self.clk());
    }

    /// Gambatte `updateIrq`: fire all IRQ events whose scheduled cc has passed.
    /// `cc` is the access cc (now raw `abs_cc` under RB_EXACTCC). The glitch-IRQ
    /// flagging from a register write compares against the same access cc the
    /// schedule was set on, so it is self-consistent in either anchor space.
    fn update_irq(&mut self, cc: u64) {
        while self.next_irq_event_time != DISABLED_TIME && cc >= self.next_irq_event_time {
            self.do_irq_event();
        }
    }

    /// IRQ DELIVERY path (raw `abs_cc` per-dot). Under RB_EXACTCC the schedule is
    /// anchored CC_OFF lower than the legacy access-cc anchor (writes now resolve
    /// at the raw start cc, not abs_cc+CC_OFF), so the delivery comparison adds
    /// CC_OFF back to keep the absolute fire cc — and thus steady state —
    /// unchanged. Flag-off this is identical to `update_irq(abs_cc)`.
    fn update_irq_delivery(&mut self, abs_cc: u64) {
        // The IF bit is raised at the LATE anchor (`CC_OFF`) — UNCHANGED from
        // baseline so the HALT-wakeup detection AND the IF re-flag observation
        // (irq_1-style tests) stay on the late grid. The non-halt EI-loop fast
        // dispatch is handled separately by `force_ei_delivery` (the CPU calls it
        // in a non-halt/non-stop EI loop): it does the same do_irq_event early so
        // the ISR / TAC re-write runs on Gambatte's exact phase, but ONLY when the
        // CPU is about to service it — it never raises IF early on the bus that the
        // HALT/re-flag paths observe.
        let fold = CC_OFF as u64;
        while self.next_irq_event_time != DISABLED_TIME
            && abs_cc >= self.next_irq_event_time.wrapping_add(fold)
        {
            // STAGE 2: record the deliverable cc (the IF-visible fire cc) before
            // do_irq_event advances next_irq_event_time to the following period.
            // The CPU's faithful event-cc gate compares the boundary access cc
            // (raw master_cc) against this. Only record while none is pending so
            // a back-to-back overflow keeps the earliest undispatched fire.
            if crate::cpu::bus::faithful_enabled() && self.last_fire_cc == DISABLED_TIME {
                self.last_fire_cc = self.next_irq_event_time.wrapping_add(CC_OFF as u64);
                self.last_fire_cc_ei = self.next_irq_event_time.wrapping_add(IF_OFF as u64);
                // A normally-delivered (not force-promoted) IRQ resets the one-shot
                // EI-promotion flag so a stale promotion from an earlier ISR cannot
                // mis-bias a later, normally-entered STOP.
                self.ei_promoted = false;
            }
            if std::env::var("RB_TIMATRACE").is_ok() {
                eprintln!("[RB IRQ-fire] schedCc={} deliverCc={} ifCc={} (abs_cc={})",
                    self.next_irq_event_time, self.next_irq_event_time.wrapping_add(CC_OFF as u64),
                    self.next_irq_event_time.wrapping_add(fold), abs_cc);
            }
            self.do_irq_event();
        }
    }

    /// EI-loop fast timer delivery. In a non-halt/non-stop EI loop the CPU calls
    /// this at the EARLY anchor (`boundary >= schedCc + IF_OFF`) to fire an
    /// imminent overflow BEFORE its normal `CC_OFF`-late per-dot delivery, so the
    /// serviced ISR (and any TAC re-write) runs on Gambatte's exact divider phase.
    /// Mirrors `update_irq_delivery` but keyed on the early anchor and only
    /// reachable from the non-halt dispatch path. Returns true if it fired one (so
    /// the CPU can raise the IF bit and service). Idempotent vs the per-dot path:
    /// `do_irq_event` advances `next_irq_event_time`, so the later +5 delivery
    /// will not re-fire the same overflow.
    pub fn force_ei_delivery(&mut self, boundary: u64) -> bool {
        if self.tac & TAC_ENABLE == 0 {
            return false;
        }
        let if_off = std::env::var("RB_IF_OFF")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(IF_OFF);
        let mut fired = false;
        while self.next_irq_event_time != DISABLED_TIME
            && boundary >= self.next_irq_event_time.wrapping_add(if_off as u64)
        {
            if self.last_fire_cc == DISABLED_TIME {
                self.last_fire_cc = self.next_irq_event_time.wrapping_add(CC_OFF as u64);
                self.last_fire_cc_ei = self.next_irq_event_time.wrapping_add(IF_OFF as u64);
            }
            self.do_irq_event();
            fired = true;
        }
        if fired {
            self.ei_promoted = true;
        }
        fired
    }

    /// Gambatte `Tima::updateTima`: advance the derived TIMA value to `cc`.
    fn update_tima(&mut self, cc: u64) {
        let clk = self.clk();
        let ticks = (cc - self.tima_last_update) >> clk;
        self.tima_last_update += ticks << clk;

        if cc >= self.tmatime {
            if cc >= self.tmatime.wrapping_add(4) {
                self.tmatime = DISABLED_TIME;
            }
            self.tima = self.tma;
        }

        let mut tmp = self.tima as u64 + ticks;
        if tmp > 0x100 {
            let diff = 0x100 - self.tma as u64;
            tmp -= diff * (tmp / diff - 0x100 / diff);
            if tmp > 0x100 {
                tmp -= diff;
            }
        }

        if tmp == 0x100 {
            tmp = 0;
            self.tmatime = self.tima_last_update + TMA_OFF;
            if cc >= self.tmatime {
                if cc >= self.tmatime.wrapping_add(4) {
                    self.tmatime = DISABLED_TIME;
                }
                tmp = self.tma as u64;
            }
        }

        self.tima = tmp as u8;
    }

    /// Gambatte `Tima::setTima`.
    fn set_tima(&mut self, data: u8) {
        let cc = self.access_cc();
        if self.tac & TAC_ENABLE != 0 {
            self.update_irq(cc);
            self.update_tima(cc);
            if self.tmatime.wrapping_sub(cc) < 4 {
                self.tmatime = DISABLED_TIME;
            }
            self.next_irq_event_time =
                self.tima_last_update + ((256u64 - data as u64) << self.clk()) + TMA_OFF;
        }
        self.tima = data;
    }

    /// Gambatte `Tima::setTma`.
    fn set_tma(&mut self, data: u8) {
        let cc = self.access_cc();
        if self.tac & TAC_ENABLE != 0 {
            self.update_irq(cc);
            self.update_tima(cc);
        }
        self.tma = data;
    }

    /// Gambatte `Tima::setTac` (DMG / CGB; `agbFlag` is false for both targets).
    fn set_tac(&mut self, data: u8) {
        let cc = self.access_cc();
        if std::env::var("RB_TIMATRACE").is_ok() {
            eprintln!("[RB setTac] cc={} data={:02X} oldtac={:02X} tima={:02X} tma={:02X} lastUpd={} nextIrq={} tmatime={} divAnchor={}",
                cc, data, self.tac, self.tima, self.tma, self.tima_last_update,
                if self.next_irq_event_time==DISABLED_TIME {0} else {self.next_irq_event_time},
                if self.tmatime==DISABLED_TIME {0} else {self.tmatime}, self.div_anchor);
        }
        if (self.tac ^ data) != 0 {
            let mut next = self.next_irq_event_time;

            if self.tac & TAC_ENABLE != 0 {
                let old_clk = self.clk();
                let inc = (!((data as u64 >> 2)
                    & ((cc - self.div_anchor) >> (TIMA_CLOCK[(data & 3) as usize] - 1)))
                    & 1) as u64;
                let shift = (inc << (old_clk - 1)) + 3;
                self.tima_last_update = self.tima_last_update.wrapping_sub(shift);
                next = next.wrapping_sub(shift);
                if next != DISABLED_TIME && cc >= next {
                    self.pending_irq = true;
                }
                self.update_tima(cc);
                self.tmatime = DISABLED_TIME;
                next = DISABLED_TIME;
            }

            if data & TAC_ENABLE != 0 {
                let new_clk = TIMA_CLOCK[(data & 3) as usize];
                self.tima_last_update =
                    cc - ((cc - self.div_anchor) & ((1u64 << new_clk) - 1));
                next = self.tima_last_update + ((256u64 - self.tima as u64) << new_clk) + TMA_OFF;
            }

            self.next_irq_event_time = next;
        }
        self.tac = data;
    }

    /// Gambatte `Tima::divReset`: applied on a DIV write (FF04). The divider phase
    /// back-shift glitches TIMA by `(1 << (clk-1)) + 3`. `cc` is the resolution
    /// cc: `access_cc()` for a CPU FF04 write, raw `abs_cc` for the STOP-internal
    /// reset (the speed-switch DIV reset happens at the STOP cc, not a CPU
    /// register-access M-cycle end).
    fn div_reset_at(&mut self, cc: u64) {
        self.div_reset_split(cc, cc);
    }

    /// Generalized `Tima::divReset` allowing the TIMA-glitch/reset value to be
    /// resolved at `tima_cc` while the divider/derivation anchor lands at
    /// `anchor_cc`. For a normal FF04 write these are equal (`div_reset_at`); the
    /// CGB STOP speed switch passes the true switch cc as `tima_cc` (so the reset
    /// TIMA matches Gambatte's grid) and the read-grid anchor as `anchor_cc` (so
    /// post-switch `read_cc - div_anchor` resolves the divider at the exact read
    /// cc). The TIMA tick grid (`tima_last_update`) and IRQ schedule are based on
    /// `anchor_cc` since post-switch reads/IRQs all arrive on that same read grid.
    fn div_reset_split(&mut self, tima_cc: u64, anchor_cc: u64) {
        if self.tac & TAC_ENABLE != 0 {
            let clk = self.clk();
            let shift = (1u64 << (clk - 1)) + 3;
            self.tima_last_update = self.tima_last_update.wrapping_sub(shift);
            if self.next_irq_event_time != DISABLED_TIME {
                self.next_irq_event_time = self.next_irq_event_time.wrapping_sub(shift);
                if tima_cc >= self.next_irq_event_time {
                    self.pending_irq = true;
                }
            }
            // Advance the derived TIMA up to the true switch cc (Gambatte's grid),
            // capturing the post-switch reset TIMA value, then re-anchor the tick
            // grid and IRQ schedule to the read-grid `anchor_cc` so subsequent
            // `read_cc - tima_last_update` resolves on the same grid reads arrive.
            self.update_tima(tima_cc);
            self.tima_last_update = anchor_cc;
            self.next_irq_event_time =
                self.tima_last_update + ((256u64 - self.tima as u64) << clk) + TMA_OFF;
        }
        self.div_anchor = anchor_cc;
        // Normal FF04 writes share one cc for the DIV register and the APU fold;
        // the STOP path overrides `div_anchor_apu` afterward with its own offset.
        self.div_anchor_apu = anchor_cc;
        self.div_reset_count = self.div_reset_count.wrapping_add(1);
        // Closed-form FS (RB_LAZYPERIPH): the divider restarts from this cc, so
        // re-anchor the FS sync point to the reset cc — the divider counter (and
        // thus the bit-12/13 falling-edge grid) is now relative to the new anchor.
        self.last_apu_cc = anchor_cc;
    }

    /// CGB STOP speed switch divider/TIMA re-derivation. The divider continues
    /// ticking from the exact switch cc at the new speed: the derivation anchor
    /// (`div_anchor`, the post-switch divider/TIMA tick base) lands at
    /// `abs_cc + STOP_DERIV_OFF`, while the *reset TIMA value* is resolved at the
    /// true Gambatte switch cc (`anchor + STOP_TIMA_{SS,DS}_EXTRA`). Decoupling the
    /// two makes both sub-dot bracket sides of every speedchange family resolve at
    /// once (no frozen window, no single shared offset that swaps one side for the
    /// other). The APU divReset fold keeps its own anchor offset
    /// (`STOP_APU_{SS,DS}_EXTRA`), set after the split (which would otherwise leave
    /// `div_anchor_apu` at the derivation anchor).
    pub fn stop_div_reset(&mut self, old_ds: bool) {
        // A STOP issued inside an EI-loop ISR that was force-delivered the timer
        // IRQ at the early anchor enters with `abs_cc` shifted vs the normal +5-late
        // service; compensate the derivation anchor by `STOP_EI_PROMOTE_ADJ` so the
        // post-switch divider phase (and the much-later TIMA read it feeds) matches
        // Gambatte. Consumes the one-shot promotion flag.
        let promote_adj_const = std::env::var("RB_STOP_PROMOTE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(STOP_EI_PROMOTE_ADJ);
        let promote_adj = if self.ei_promoted { promote_adj_const } else { 0 };
        self.ei_promoted = false;
        let anchor_cc = (self.abs_cc as i64 + STOP_DERIV_OFF + promote_adj) as u64;
        let tima_cc = (anchor_cc as i64
            + if old_ds { STOP_TIMA_DS_EXTRA } else { STOP_TIMA_SS_EXTRA }) as u64;
        // APU divReset fold anchor: independently offset from the derivation anchor.
        let apu_cc = (anchor_cc as i64
            + if old_ds { STOP_APU_DS_EXTRA } else { STOP_APU_SS_EXTRA }) as u64;
        self.div_reset_split(tima_cc, anchor_cc);
        // div_reset_split set div_anchor_apu = anchor_cc; override with APU cc.
        self.div_anchor_apu = apu_cc;
    }

    /// Initialize the timer's internal 16-bit counter (used at boot to mirror
    /// Gambatte's post-boot `cycleCounter - divLastUpdate` low 16 bits).
    pub fn set_internal_counter(&mut self, value: u16) {
        self.abs_cc = value as u64;
        self.div_anchor = 0;
        self.div_anchor_apu = 0;
        self.tima_last_update = self.abs_cc;
        self.last_apu_cc = self.abs_cc;
    }

    pub fn internal_counter(&self) -> u16 {
        self.divider()
    }

    /// CGB STOP speed switch. Gambatte's `Tima::speedChange` pulls the timer's
    /// `lastUpdate_` (and the scheduled IRQ time) back by 4 T-cycles for an
    /// enabled fast-frequency timer (`tac & 0x07 >= 0x05`). We additionally cover
    /// the double->single direction the same way (the original per-dot model did
    /// the catch-up for any enabled timer switching back to single speed).
    pub fn speed_change(&mut self) {
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
    pub fn flush_pending_irq(&mut self, mmio: &mut mmio::Mmio) {
        if self.pending_irq {
            self.pending_irq = false;
            mmio.request_interrupt(cpu::registers::InterruptFlag::Timer);
        }
    }

    pub fn take_pending_irq(&mut self) -> bool {
        let p = self.pending_irq;
        self.pending_irq = false;
        p
    }

    /// Count DIV-bit-12 (single speed) / bit-13 (double speed) falling edges in
    /// the cc interval `(a, b]`. The divider counter is `cc - div_anchor`; bit N
    /// falls each time that counter passes a multiple of `2^(N+1)`. Used by the
    /// closed-form APU frame-sequencer clock (RB_LAZYPERIPH).
    fn apu_fs_edges(&self, a: u64, b: u64) -> u64 {
        if b <= a {
            return 0;
        }
        let shift = if self.last_double_speed { 14 } else { 13 }; // N+1
        let ca = a.wrapping_sub(self.div_anchor);
        let cb = b.wrapping_sub(self.div_anchor);
        (cb >> shift).wrapping_sub(ca >> shift)
    }

    pub fn step(&mut self, mmio: &mut mmio::Mmio) {
        self.abs_cc = self.abs_cc.wrapping_add(1);

        // Scheduled TIMA IRQ: fire any event whose absolute cc has now passed.
        // This delivers the IRQ at the same `abs_cc` anchor a CPU read resolves
        // TIMA on (Gambatte `intevent_tima`).
        // The IRQ is delivered as a real-time per-dot event keyed on the raw
        // `abs_cc` (the cc the IF bit physically becomes visible to the CPU),
        // whereas register read/write effects resolve at `access_cc()` (the CPU
        // M-cycle's start cc). These are deliberately different anchors: the
        // IF-visible cc trails the scheduled `next_irq_event_time` (set in
        // access-cc space) by `CC_OFF` dots, which matches Gambatte's late IRQ
        // sampling relative to the access that scheduled it.
        if self.tac & TAC_ENABLE != 0 {
            self.update_irq_delivery(self.abs_cc);
        }
        self.flush_pending_irq(mmio);

        // The APU frame sequencer (sweep + noise-envelope legs that remain
        // FS-clocked; length is now cc-driven in the controller) is clocked by
        // the falling edge of DIV bit 12 (bit 13 in double speed), derived from
        // the SAME master `abs_cc`/`div_anchor` the timer/DIV use.
        self.last_double_speed = mmio.is_double_speed_mode();
        // Closed-form FS (stage 3/7, permanent): clock once per DIV-bit-12 (bit-13
        // at DS) falling edge in (last_apu_cc, abs_cc]. The divider counter is
        // (cc - div_anchor); bit N falls each time that counter crosses a
        // multiple of 2^(N+1). Count = floor(c_now/P) - floor(c_prev/P).
        let edges = self.apu_fs_edges(self.last_apu_cc, self.abs_cc);
        for _ in 0..edges {
            mmio.clock_apu_frame_sequencer();
        }
        self.last_apu_cc = self.abs_cc;
    }
}

impl Addressable for Timer {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            DIV => {
                let div = (self.access_cc().wrapping_sub(self.div_anchor) & 0xFFFF) as u16;
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
                        let diff = 0x100 - self.tma as u64;
                        tmp -= diff * (tmp / diff - 0x100 / diff);
                        if tmp > 0x100 {
                            tmp -= diff;
                        }
                    }
                    if tmp == 0x100 {
                        let tmatime = self.tima_last_update + (ticks << clk) + TMA_OFF;
                        tmp = if cc >= tmatime { self.tma as u64 } else { 0 };
                    }
                    if std::env::var("RB_TIMATRACE").is_ok() {
                        eprintln!("[RB TIMA read] cc={} -> {:02X} (lastUpd={} tmatime={} tma={:02X})",
                            cc, tmp as u8, self.tima_last_update,
                            if self.tmatime==DISABLED_TIME {0} else {self.tmatime}, self.tma);
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

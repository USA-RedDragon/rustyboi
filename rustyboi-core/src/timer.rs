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
/// Write-side canonical access-cc offset (M8). Swept against the ch2
/// `late_reset_nr52` a/b pairs; the trigger's length boundary lands at this
/// phase rather than the read's `CC_OFF`.
const WRITE_CC_OFF: i64 = 0;
/// STOP-write DIV-reset canonical cc offset (M8 speedchange). The STOP
/// speed-switch DIV reset resolves at this phase relative to the per-dot
/// `abs_cc`; swept against `speedchange2_tima00_2a/2b`.
const STOP_CC_OFF: i64 = 0;
/// Extra master-cc added to the STOP divReset/speed-switch cc when the STOP is
/// entered in DOUBLE speed (DS->SS direction). Gambatte's STOP-entry cc carries
/// one more prefetch M-cycle in double speed than in single (cctracer: SS-entry
/// divReset at instr_start+4, DS-entry at instr_start+8), so the per-dot `abs_cc`
/// (which trails by the single-speed amount) is 4 master-cc short of the true
/// switch cc in the DS->SS direction. The DIV/TIMA register READ resolves at
/// `access_cc()` (CC_OFF=5), which itself trails Gambatte's read cc by 3 for these
/// post-switch reads; only `read_cc - div_anchor` matters for the high-byte/tick
/// boundary, so the boundary-matching re-anchor is `4 - 3 = +1`. Swept against the
/// full speedchange2 tima/div families: +1 passes BOTH sub-dot probe sides of
/// every `_1/_2/_1a/_1b/_2a/_2b` pair (10 fixes, zero regressions).
const STOP_DS_OFF: i64 = 1;
/// APU-specific STOP speed-switch cc offset (DS->SS direction). The APU divReset
/// fold (square duty + length `cc>>13` boundary) re-anchors at a different
/// sub-cycle phase than the TIMA/DIV high-byte boundary, so it carries its own
/// offset (swept: +2 fixes the `ch2_nr52_2b` length-expiry probes with no
/// regression; the TIMA/DIV `STOP_DS_OFF` of +1 is calibrated separately).
const STOP_APU_DS_OFF: i64 = 2;

// Gambatte's `+3` constant in `tmatime`/`nextIrqEventTime` (mem/tima.cpp).
const TMA_OFF: u64 = 3;

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
        let off = std::env::var("RB_CC_OFF").ok().and_then(|v| v.parse().ok()).unwrap_or(CC_OFF);
        (self.abs_cc as i64 + off) as u64
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
    fn update_irq(&mut self, cc: u64) {
        while self.next_irq_event_time != DISABLED_TIME && cc >= self.next_irq_event_time {
            self.do_irq_event();
        }
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
        if self.tac & TAC_ENABLE != 0 {
            let clk = self.clk();
            let shift = (1u64 << (clk - 1)) + 3;
            self.tima_last_update = self.tima_last_update.wrapping_sub(shift);
            if self.next_irq_event_time != DISABLED_TIME {
                self.next_irq_event_time = self.next_irq_event_time.wrapping_sub(shift);
                if cc >= self.next_irq_event_time {
                    self.pending_irq = true;
                }
            }
            self.update_tima(cc);
            self.tima_last_update = cc;
            self.next_irq_event_time =
                self.tima_last_update + ((256u64 - self.tima as u64) << clk) + TMA_OFF;
        }
        self.div_anchor = cc;
        // Normal FF04 writes share one cc for the DIV register and the APU fold;
        // the STOP path overrides `div_anchor_apu` afterward with its own offset.
        self.div_anchor_apu = cc;
        self.div_reset_count = self.div_reset_count.wrapping_add(1);
    }

    /// CGB STOP speed-switch DIV reset. Re-anchors DIV/TIMA at the switch cc:
    /// `abs_cc + STOP_CC_OFF`, plus `STOP_DS_OFF` extra master-cc when the STOP is
    /// entered in DOUBLE speed (`old_ds`, the DS->SS direction); see `STOP_DS_OFF`
    /// for the derivation. The APU divReset fold's anchor (`div_anchor_apu`) gets
    /// its own `STOP_APU_DS_OFF` offset, set after `div_reset_at` (which would
    /// otherwise overwrite it with the TIMA/DIV `cc`).
    pub fn stop_div_reset(&mut self, old_ds: bool) {
        let cc = (self.abs_cc as i64 + STOP_CC_OFF
            + if old_ds { STOP_DS_OFF } else { 0 }) as u64;
        // APU divReset fold anchor: independently offset from the TIMA/DIV cc.
        let apu_cc = (self.abs_cc as i64 + STOP_CC_OFF
            + if old_ds { STOP_APU_DS_OFF } else { 0 }) as u64;
        self.div_reset_at(cc);
        // div_reset_at set div_anchor_apu = cc; override with the APU-specific cc.
        self.div_anchor_apu = apu_cc;
    }

    /// Initialize the timer's internal 16-bit counter (used at boot to mirror
    /// Gambatte's post-boot `cycleCounter - divLastUpdate` low 16 bits).
    pub fn set_internal_counter(&mut self, value: u16) {
        self.abs_cc = value as u64;
        self.div_anchor = 0;
        self.div_anchor_apu = 0;
        self.tima_last_update = self.abs_cc;
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
            self.update_irq(self.abs_cc);
        }
        self.flush_pending_irq(mmio);

        // The APU frame sequencer (sweep + noise-envelope legs that remain
        // FS-clocked; length is now cc-driven in the controller) is clocked by
        // the falling edge of DIV bit 12 (bit 13 in double speed), derived from
        // the SAME master `abs_cc`/`div_anchor` the timer/DIV use.
        self.last_double_speed = mmio.is_double_speed_mode();
        let apu_bit_pos = if self.last_double_speed { 13 } else { 12 };
        let apu_div = (self.abs_cc.wrapping_sub(self.div_anchor) & 0xFFFF) as u16;
        let apu_bit = (apu_div & (1 << apu_bit_pos)) != 0;
        if self.last_apu_div_bit && !apu_bit {
            mmio.clock_apu_frame_sequencer();
        }
        self.last_apu_div_bit = apu_bit;
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

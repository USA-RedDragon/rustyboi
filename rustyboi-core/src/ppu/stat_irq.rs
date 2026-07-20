// Event-scheduled STAT/mode/LYC interrupt model.
//
// Each STAT interrupt source (LYC=LY compare, mode 0/1/2 entry) fires at a
// fixed offset within the LCD frame. We predict each source's next fire as an
// absolute dot time and, ticking the PPU per-dot on an absolute counter (`cc`),
// raise an IRQ on the dot whose scheduled time equals `cc`. Register writes to
// FF41/FF45 recompute those times and may immediately flag an IRQ. Times are in
// dots (single-speed cycles); `ds` (double speed) scales durations via `<< ds`.
//
// Sub-dot offset constants (the `2 + 2*ds`, `6 + 4*ds`, `-2`, `+6` forms) and
// the LCD geometry are hardware timing facts, annotated inline with the silicon
// timing they encode. The predicates below are decomposed by result, not by any
// external source's control flow; the constants are the load-bearing part.
use serde::{Deserialize, Serialize};

pub(super) const LCD_CYCLES_PER_LINE: u32 = 456;
pub(super) const LCD_LINES_PER_FRAME: u32 = 154;
pub(super) const LCD_VRES: u32 = 144;
pub(super) const LCD_CYCLES_PER_FRAME: u64 = LCD_LINES_PER_FRAME as u64 * LCD_CYCLES_PER_LINE as u64;

pub(super) const STAT_M0EN: u8 = 0x08;
pub(super) const STAT_M1EN: u8 = 0x10;
pub(super) const STAT_M2EN: u8 = 0x20;
pub(super) const STAT_LYCEN: u8 = 0x40;

pub(super) const DISABLED_TIME: u64 = u64::MAX / 4;

const MODE1_IRQ_FRAME_CYCLE: i64 = LCD_VRES as i64 * LCD_CYCLES_PER_LINE as i64 - 2;
const MODE2_IRQ_LINE_CYCLE: i64 = LCD_CYCLES_PER_LINE as i64 - 4;
const MODE2_IRQ_LINE_CYCLE_LY0: i64 = LCD_CYCLES_PER_LINE as i64 - 2;

/// A snapshot of the LY counter used to place frame/line events on the absolute
/// dot clock. `time` is the absolute `cc` (dots) at which LY next increments.
#[derive(Clone, Copy)]
pub(super) struct LyCounter {
    pub ly: u32,
    pub time: u64,
    pub(crate) ds: bool,
}

impl LyCounter {
    pub(super) fn line_time(&self) -> u64 {
        (LCD_CYCLES_PER_LINE as u64) << self.ds as u32
    }

    pub(super) fn frame_cycles(&self, cc: u64) -> u64 {
        self.ly as u64 * LCD_CYCLES_PER_LINE as u64 + self.line_cycles(cc)
    }

    pub(super) fn line_cycles(&self, cc: u64) -> u64 {
        LCD_CYCLES_PER_LINE as u64 - ((self.time - cc) >> self.ds as u32)
    }

    pub(super) fn next_line_cycle(&self, line_cycle: i64, cc: u64) -> u64 {
        // The candidate dot may land more than one line ahead of `cc` (the target
        // line-cycle already passed this line); if so, pull it back by exactly one
        // line so it names the current line's occurrence. The distance is measured
        // in wrapping u64 arithmetic to tolerate the case where the candidate sits
        // just behind `cc`. Selected as an expression rather than mutated in place.
        let line = self.line_time();
        let tmp = (self.time as i64 + (line_cycle << self.ds as u32)) as u64;
        match tmp.wrapping_sub(cc) > line {
            true => tmp.wrapping_sub(line),
            false => tmp,
        }
    }

    pub(super) fn next_frame_cycle(&self, frame_cycle: i64, cc: u64) -> u64 {
        let span = ((LCD_LINES_PER_FRAME as i64 - 1 - self.ly as i64) * LCD_CYCLES_PER_LINE as i64
            + frame_cycle)
            << self.ds as u32;
        let frame = LCD_CYCLES_PER_FRAME << self.ds as u32;
        let tmp = (self.time as i64 + span) as u64;
        // Same one-period-back correction as `next_line_cycle`, but over a whole
        // frame instead of a line.
        match tmp.wrapping_sub(cc) > frame {
            true => tmp.wrapping_sub(frame),
            false => tmp,
        }
    }
}

pub(super) struct LyCmp {
    pub ly: u32,
    pub(crate) time_to_next_ly: i64,
}

/// The LY value the LYC=LY comparator uses. In the final few dots of a line the
/// comparator already anticipates the upcoming line, so this returns that
/// upcoming LY together with the remaining dots until it takes effect.
pub(super) fn get_lyc_cmp_ly(lc: &LyCounter, cc: u64) -> LyCmp {
    let ds = lc.ds as i64;
    let line_time = lc.line_time() as i64;
    let mut ttnl = lc.time as i64 - cc as i64;
    // Dots by which the comparator anticipates the upcoming LY before it latches.
    // Mid-frame the switch happens `2 + 2*ds` dots early: 2 dots single-speed,
    // 4 dots (2 << 1) double-speed — the LYC=LY compare already reads the next
    // line's value slightly before LY increments (age ly_timings_* sub-dot edge).
    // On the wrap line (LY=153) the LY=0 anticipation instead trails the nominal
    // increment: `line_time - (6 + 6*ds)` places it 6 dots (SS) / 12 dots (DS)
    // past the line start, matching the extended line-153->0 wrap window.
    let advance_offset = if lc.ly == LCD_LINES_PER_FRAME - 1 {
        line_time - 6 - 6 * ds
    } else {
        2 + 2 * ds
    };
    ttnl -= advance_offset;
    // Both cases advance by one line on crossing; inc_ly folds the 153->0 wrap and
    // the k->k+1 step into a single step, so the two branches collapse into one.
    let mut ly = lc.ly;
    if ttnl <= 0 {
        ly = inc_ly(ly);
        ttnl += line_time;
    }
    LyCmp { ly, time_to_next_ly: ttnl }
}

/// The LY value one line later. LY counts 0..=153 and wraps to 0 after the
/// final line, so advancing modulo the frame height gives the next line.
pub(super) fn inc_ly(ly: u32) -> u32 {
    (ly + 1) % LCD_LINES_PER_FRAME
}

pub(super) fn mode1_irq_schedule(lc: &LyCounter, cc: u64) -> u64 {
    lc.next_frame_cycle(MODE1_IRQ_FRAME_CYCLE, cc)
}

pub(super) fn mode2_irq_schedule(stat: u8, lc: &LyCounter, cc: u64) -> u64 {
    if stat & STAT_M2EN == 0 {
        return DISABLED_TIME;
    }
    // Two frame-relative mode-2 fire points: the last visible line (LY=143) and
    // the post-frame LY=0 slot on line 153. When we are already past the last
    // visible line's mode-2 point but not yet at the LY=0 point, the next mode-2
    // is the LY=0 one; mode-0 enable forces that same frame-relative schedule.
    // Otherwise the next mode-2 is one line ahead. (last_m2_fc < ly0_m2_fc, so a
    // plain half-open range test replaces the wrap-on-subtract idiom.)
    let last_m2_fc =
        ((LCD_VRES - 1) * LCD_CYCLES_PER_LINE) as u64 + MODE2_IRQ_LINE_CYCLE as u64;
    let ly0_m2_fc =
        ((LCD_LINES_PER_FRAME - 1) * LCD_CYCLES_PER_LINE) as u64 + MODE2_IRQ_LINE_CYCLE_LY0 as u64;
    if (last_m2_fc..ly0_m2_fc).contains(&lc.frame_cycles(cc)) || (stat & STAT_M0EN != 0) {
        lc.next_frame_cycle(ly0_m2_fc as i64, cc)
    } else {
        lc.next_line_cycle(MODE2_IRQ_LINE_CYCLE, cc)
    }
}

// ---- LYC=LY compare interrupt ----
// Predicts the absolute dot at which the next LY==LYC match fires and re-arms
// itself each frame. Keeps both the live "source" register values (as written)
// and a delayed "committed" copy, because FF41/FF45 writes take effect a couple
// of dots after the store on real hardware.
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
pub(super) struct LycIrq {
    pub time: u64,
    lyc_reg_src: u8,
    stat_reg_src: u8,
    lyc_reg: u8,
    stat_reg: u8,
    cgb: bool,
}

fn lyc_schedule(stat: u8, lyc: u8, lc: &LyCounter, cc: u64) -> u64 {
    // No LYC IRQ when the source is off or LYC selects a nonexistent line.
    if (stat & STAT_LYCEN == 0) || (lyc as u32) >= LCD_LINES_PER_FRAME {
        return DISABLED_TIME;
    }
    // Frame-relative dot of the LY==LYC match. For LYC>0 the match fires `-2`
    // dots before that line's start (the compare leads LY by 2 dots, single
    // speed; age lcd_frame_timings). LYC==0 is special: the LY=0 match is placed
    // on the wrap line (153) at `+6` dots, reflecting the delayed LY=0 latch.
    let fc = if lyc != 0 {
        lyc as i64 * LCD_CYCLES_PER_LINE as i64 - 2
    } else {
        (LCD_LINES_PER_FRAME as i64 - 1) * LCD_CYCLES_PER_LINE as i64 + 6
    };
    lc.next_frame_cycle(fc, cc)
}

// An LYC match on a line that is also a mode-2 (visible lines 1..=144) or
// mode-1 (line 0 / v-blank) entry is suppressed when that mode's own STAT
// source is enabled, since the coincident mode IRQ takes precedence.
fn lyc_blocked_by_m2_or_m1(ly: u32, stat: u8) -> bool {
    let mode_bit = if (1..=LCD_VRES).contains(&ly) { STAT_M2EN } else { STAT_M1EN };
    stat & mode_bit != 0
}

impl LycIrq {
    pub(super) fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    pub(super) fn lyc_reg_src(&self) -> u8 {
        self.lyc_reg_src
    }

    pub(super) fn lcd_reset(&mut self) {
        self.stat_reg = self.stat_reg_src;
        self.lyc_reg = self.lyc_reg_src;
    }

    fn reg_change(&mut self, stat: u8, lyc: u8, lc: &LyCounter, cc: u64) {
        let time_src = lyc_schedule(stat, lyc, lc, cc);
        self.stat_reg_src = stat;
        self.lyc_reg_src = lyc;
        self.time = self.time.min(time_src);

        if self.cgb {
            let ds = lc.ds as u64;
            if self.time.saturating_sub(cc) > 6 + 4 * ds
                || (time_src != self.time && self.time.saturating_sub(cc) > 2)
            {
                self.lyc_reg = lyc;
            }
            if self.time.saturating_sub(cc) > 2 {
                self.stat_reg = stat;
            }
        } else {
            if self.time.saturating_sub(cc) > 4 || time_src != self.time {
                self.lyc_reg = lyc;
            }
            self.stat_reg = stat;
        }
    }

    pub(super) fn stat_reg_change(&mut self, stat: u8, lc: &LyCounter, cc: u64) {
        let lyc = self.lyc_reg_src;
        self.reg_change(stat, lyc, lc, cc);
    }

    pub(super) fn lyc_reg_change(&mut self, lyc: u8, lc: &LyCounter, cc: u64) {
        let stat = self.stat_reg_src;
        self.reg_change(stat, lyc, lc, cc);
    }

    /// Returns true if an LYC IRQ should be flagged.
    pub(super) fn do_event(&mut self, lc: &LyCounter) -> bool {
        let mut flag = false;
        if (self.stat_reg | self.stat_reg_src) & STAT_LYCEN != 0 {
            // The event fires just before the line boundary, so it compares
            // against the LY that is about to latch.
            let cmp_ly = inc_ly(lc.ly);
            flag = self.lyc_reg as u32 == cmp_ly
                && !lyc_blocked_by_m2_or_m1(self.lyc_reg as u32, self.stat_reg);
        }
        self.lyc_reg = self.lyc_reg_src;
        self.stat_reg = self.stat_reg_src;
        self.time = lyc_schedule(self.stat_reg, self.lyc_reg, lc, self.time);
        flag
    }

    pub(super) fn reschedule(&mut self, lc: &LyCounter, cc: u64) {
        self.time = lyc_schedule(self.stat_reg, self.lyc_reg, lc, cc)
            .min(lyc_schedule(self.stat_reg_src, self.lyc_reg_src, lc, cc));
    }

    /// Seed all source/committed registers from the live FF41/FF45 values,
    /// e.g. on LCD enable. Does not schedule; call `reschedule` afterwards.
    pub(super) fn seed(&mut self, stat: u8, lyc: u8) {
        self.stat_reg_src = stat;
        self.lyc_reg_src = lyc;
        self.stat_reg = stat;
        self.lyc_reg = lyc;
        self.time = DISABLED_TIME;
    }
}

// ---- Mode 0/1/2 STAT interrupt events ----
// Fires the h-blank (mode 0), v-blank (mode 1) and OAM-scan (mode 2) STAT
// sources, applying the mutual-exclusion rules between a mode IRQ and a
// coincident LYC match on the same line. Holds a delayed copy of STAT/LYC for
// the same write-latency reason as the LYC event above.
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
pub(super) struct MStatIrq {
    lyc_reg: u8,
    stat_reg: u8,
}

impl MStatIrq {
    pub(super) fn lcd_reset(&mut self, lyc_reg: u8) {
        self.lyc_reg = lyc_reg;
    }

    pub(super) fn seed(&mut self, stat: u8, lyc: u8) {
        self.stat_reg = stat;
        self.lyc_reg = lyc;
    }

    pub(super) fn lyc_reg_change(
        &mut self,
        lyc: u8,
        next_m0: u64,
        next_m2: u64,
        cc: u64,
        ds: bool,
        cgb: bool,
    ) {
        if (cc + 5 * cgb as u64 + 1 - ds as u64) < next_m0.min(next_m2) {
            self.lyc_reg = lyc;
        }
    }

    pub(super) fn stat_reg_change(
        &mut self,
        stat: u8,
        next_m0: u64,
        next_m1: u64,
        next_m2: u64,
        cc: u64,
        cgb: bool,
    ) {
        if (cc + 2 * cgb as u64) < next_m0.min(next_m1).min(next_m2) {
            self.stat_reg = stat;
        }
    }

    pub(super) fn do_m0_event(&mut self, ly: u32, stat: u8, lyc: u8) -> bool {
        let flag = ((stat | self.stat_reg) & STAT_M0EN != 0)
            && ((self.stat_reg & STAT_LYCEN == 0) || ly != self.lyc_reg as u32);
        self.lyc_reg = lyc;
        self.stat_reg = stat;
        flag
    }

    pub(super) fn do_m1_event(&mut self, stat: u8) -> bool {
        let flag =
            (stat & STAT_M1EN != 0) && (self.stat_reg & (STAT_M2EN | STAT_M0EN) == 0);
        self.stat_reg = stat;
        flag
    }

    pub(super) fn do_m2_event(&mut self, ly: u32, stat: u8, lyc: u8) -> bool {
        let blocked_by_m1 = ly == 0 && (self.stat_reg & STAT_M1EN != 0);
        // Mode 2 begins a line, so its coincident-LYC check uses the previous
        // line's number (line 0 has no predecessor, so it clamps to 0).
        let cmp = ly.saturating_sub(1);
        let blocked_by_lyc = (self.stat_reg & STAT_LYCEN != 0) && cmp == self.lyc_reg as u32;
        let flag = !blocked_by_m1 && !blocked_by_lyc;
        self.lyc_reg = lyc;
        self.stat_reg = stat;
        flag
    }
}

// ---- Immediate STAT IRQ on an FF41 write ----
// Writing STAT can raise an interrupt on the same dot if the newly enabled
// source already matches the current LCD position. These predicates decide that
// for the two hardware variants, decomposed by the v-blank / mode-0 / LYC regions
// they distinguish. The sub-dot boundary constants are validated against the
// timing test ROMs and carried verbatim.

/// DMG variant. `m0_irq_time` is the absolute dot of the current line's mode-0
/// IRQ (DISABLED_TIME if none scheduled).
pub(super) fn stat_change_triggers_dmg(
    old: u8,
    lc: &LyCounter,
    cc: u64,
    m0_irq_time: u64,
    lyc_reg: u8,
) -> bool {
    let lyc_cmp = get_lyc_cmp_ly(lc, cc);
    let lyc_match = lyc_cmp.ly == lyc_reg as u32;
    // The coincident LYC match is "already armed" (so a STAT write cannot re-fire
    // it) exactly when it matches AND the LYC source was already enabled.
    let lyc_already_armed = lyc_match && (old & STAT_LYCEN != 0);

    // V-blank lines (LY>=144): only a newly-enabled mode-1 source, or an LYC
    // match that was not already armed, can raise the immediate IRQ.
    if lc.ly >= LCD_VRES {
        return (old & STAT_M1EN == 0) && !lyc_already_armed;
    }
    // Visible line with this line's mode-0 IRQ still ahead (a real pending m0).
    let m0_pending = m0_irq_time != DISABLED_TIME && m0_irq_time >= lc.time;
    if !m0_pending {
        // Mode 0 already passed / none: only a fresh LYC match can fire.
        return lyc_match && (old & STAT_LYCEN == 0);
    }
    (old & STAT_M0EN == 0) && !lyc_already_armed
}

fn stat_change_triggers_m2_cgb(
    old: u8,
    data: u8,
    ly: i64,
    time_to_next_ly: i64,
    ds: bool,
) -> bool {
    if (old & STAT_M2EN != 0)
        || (data & (STAT_M2EN | STAT_M0EN)) != STAT_M2EN
    {
        return false;
    }
    let ds = ds as i64;
    let line = LCD_CYCLES_PER_LINE as i64;
    // `line - MODE2_IRQ_LINE_CYCLE` = dots the mode-2 fire point sits before the
    // LY increment (4 dots; `* (1 + ds)` doubles it under double speed). The lower
    // bound excludes the last few dots where the mode-2 write no longer catches
    // this line: `> 2` on interior lines, `> 4 + 2*ds` on the last visible line
    // (LY=143, where the coming v-blank shifts the boundary). LY=153 uses the LY=0
    // fire point (`MODE2_IRQ_LINE_CYCLE_LY0`). Any other LY has no mode-2 fire.
    let bounds = if ly < LCD_VRES as i64 - 1 {
        Some(((line - MODE2_IRQ_LINE_CYCLE) * (1 + ds), 2))
    } else if ly == LCD_VRES as i64 - 1 {
        Some(((line - MODE2_IRQ_LINE_CYCLE) * (1 + ds), 4 + 2 * ds))
    } else if ly == LCD_LINES_PER_FRAME as i64 - 1 {
        Some(((line - MODE2_IRQ_LINE_CYCLE_LY0) * (1 + ds), 2))
    } else {
        None
    };
    match bounds {
        Some((upper, lower)) => time_to_next_ly <= upper && time_to_next_ly > lower,
        None => false,
    }
}

fn stat_change_triggers_m0lyc_or_m1_cgb(
    old: u8,
    data: u8,
    lycperiod: bool,
    lc: &LyCounter,
    cc: u64,
    m0_irq_time: u64,
) -> bool {
    let ly = lc.ly as i64;
    let time_to_next_ly = lc.time as i64 - cc as i64;
    let ds = lc.ds as i64;
    let m1_irq_lc_inv =
        LCD_CYCLES_PER_LINE as i64 - (MODE1_IRQ_FRAME_CYCLE % LCD_CYCLES_PER_LINE as i64);

    // Shared term: a fresh LYC match can fire whenever we are in the LYC period
    // and the write enables the LYC source. Factored out of all three uses below.
    let lyc_fire = lycperiod && (data & STAT_LYCEN != 0);

    // The mode-0 region: every visible line, plus the top of line 143 before the
    // v-blank window opens (`m1_irq_lc_inv * (1 + ds)` = dots from the mode-1 fire
    // point back to the LY inc; the mode-1 fire lands `-2` dots before line 144).
    let in_m0_region = ly < LCD_VRES as i64 - 1
        || (ly == LCD_VRES as i64 - 1 && time_to_next_ly > m1_irq_lc_inv * (1 + ds));
    if in_m0_region {
        // Within `4 + 4*ds` dots (interior) / `4 + 2*ds` (LY=143) of the LY inc,
        // or once this line's mode-0 IRQ has passed, only the LYC match survives.
        let m0_guard = if ly < LCD_VRES as i64 - 1 { 4 + 4 * ds } else { 4 + 2 * ds };
        if m0_irq_time < lc.time || time_to_next_ly <= m0_guard {
            return lyc_fire;
        }
        // Otherwise a newly-enabled mode-0 (unless it was already on) or the LYC
        // match fires; the guard and the disjunction collapse into one expression.
        return (old & STAT_M0EN == 0) && ((data & STAT_M0EN != 0) || lyc_fire);
    }

    // V-blank region: the mode-1 source is blocked from re-firing if it was
    // already enabled and we are not in the final-line grace window (`> 3 + 3*ds`
    // dots before the LY=0 latch). Otherwise a newly/still-armed mode-1 (with its
    // own `> 4 + 2*ds` last-line window) or an LYC match fires.
    let not_last_line = ly < LCD_LINES_PER_FRAME as i64 - 1;
    let m1_blocked = (old & STAT_M1EN != 0) && (not_last_line || time_to_next_ly > 3 + 3 * ds);
    let m1_fire = (data & STAT_M1EN != 0) && (not_last_line || time_to_next_ly > 4 + 2 * ds);
    !m1_blocked && (m1_fire || lyc_fire)
}

pub(super) fn stat_change_triggers_cgb(
    old: u8,
    data: u8,
    lc: &LyCounter,
    cc: u64,
    m0_irq_time: u64,
    lyc_reg: u8,
) -> bool {
    let newly_enabled = data & !old & (STAT_LYCEN | STAT_M2EN | STAT_M1EN | STAT_M0EN);
    let lyc_cmp = get_lyc_cmp_ly(lc, cc);
    // The compare only counts as "in the LYC period" while it still has >2 dots to
    // run, so a same-dot write does not spuriously match against the stale value.
    let lycperiod = lyc_cmp.ly == lyc_reg as u32 && lyc_cmp.time_to_next_ly > 2;
    // Combined fast reject: nothing was newly enabled, or a coincident LYC match
    // was already armed (STAT write cannot re-fire it).
    if newly_enabled == 0 || (lycperiod && (old & STAT_LYCEN != 0)) {
        return false;
    }
    let ly = lc.ly as i64;
    let time_to_next_ly = lc.time as i64 - cc as i64;
    stat_change_triggers_m0lyc_or_m1_cgb(old, data, lycperiod, lc, cc, m0_irq_time)
        || stat_change_triggers_m2_cgb(old, data, ly, time_to_next_ly, lc.ds)
}

// ---- Immediate STAT IRQ on an FF45 (LYC) write ----
// Writing LYC can raise an LYC=LY interrupt on the same dot if the new value
// matches the LY currently being compared. Decomposed into a mode-0/mode-1 block
// test plus a boundary-anticipation compare; sub-dot boundary constants validated
// against the timing test ROMs and carried verbatim.

fn lyc_change_blocked_by_m0_or_m1(
    data: u8,
    lc: &LyCounter,
    cc: u64,
    stat: u8,
    m0_irq_time: u64,
    cgb: bool,
) -> bool {
    // V-blank lines: a mode-1 STAT source blocks the LYC write, except in the
    // last-line grace window where the LY=0 latch is already imminent — `2 + 2*ds
    // + 2*cgb` dots before the increment (the +2 CGB term is the extra CGB compare
    // latency). Handled first so the visible-line case is the fallthrough.
    if lc.ly >= LCD_VRES {
        let time_to_next_ly = lc.time as i64 - cc as i64;
        let last_line_grace = lc.ly == LCD_LINES_PER_FRAME - 1
            && time_to_next_ly <= 2 + 2 * lc.ds as i64 + 2 * cgb as i64;
        return (stat & STAT_M1EN != 0) && !last_line_grace;
    }
    // Visible lines: a scheduled mode-0 STAT IRQ still ahead blocks an LYC write
    // that targets the current line.
    (stat & STAT_M0EN != 0) && m0_irq_time > lc.time && data as u32 == lc.ly
}

pub(super) fn lyc_change_triggers_stat_irq(
    old: u8,
    data: u8,
    lc: &LyCounter,
    cc: u64,
    stat: u8,
    m0_irq_time: u64,
    cgb: bool,
) -> bool {
    if (stat & STAT_LYCEN == 0)
        || (data as u32) >= LCD_LINES_PER_FRAME
        || lyc_change_blocked_by_m0_or_m1(data, lc, cc, stat, m0_irq_time, cgb)
    {
        return false;
    }
    let lyc_cmp = get_lyc_cmp_ly(lc, cc);
    // Within `4 + 4*ds + 2*cgb` dots of the LY increment the comparator is on the
    // boundary: the write races the latch (the +2 CGB term is CGB's extra compare
    // latency; the `> 2*cgb` inner window is where the old LYC already matched, so
    // re-writing it cannot re-fire).
    let near_boundary = lyc_cmp.time_to_next_ly <= 4 + 4 * lc.ds as i64 + 2 * cgb as i64;
    if near_boundary && old as u32 == lyc_cmp.ly && lyc_cmp.time_to_next_ly > 2 * cgb as i64 {
        return false;
    }
    // On the boundary the compare has already anticipated the next line, so match
    // the new LYC against that advanced value instead of mutating the snapshot.
    let effective_ly = if near_boundary { inc_ly(lyc_cmp.ly) } else { lyc_cmp.ly };
    data as u32 == effective_ly
}

/// After a mode-2 IRQ fires, the dot delta to the next one. With mode-0 also
/// enabled the sources coincide once per frame; otherwise mode 2 recurs each
/// line, with LY=0 (line 153) and the last visible line shifting the delta by
/// the LY=0 slot's offset.
pub(super) fn mode2_reschedule_delta(ly: u32, stat: u8, ds: bool) -> u64 {
    let mut next = LCD_CYCLES_PER_FRAME;
    if stat & STAT_M0EN == 0 {
        next = LCD_CYCLES_PER_LINE as u64;
        if ly == 0 {
            next -= (MODE2_IRQ_LINE_CYCLE_LY0 - MODE2_IRQ_LINE_CYCLE) as u64;
        } else if ly == LCD_VRES {
            next += LCD_CYCLES_PER_LINE as u64
                * (LCD_LINES_PER_FRAME - LCD_VRES - 1) as u64
                + (MODE2_IRQ_LINE_CYCLE_LY0 - MODE2_IRQ_LINE_CYCLE) as u64;
        }
    }
    next << ds as u32
}

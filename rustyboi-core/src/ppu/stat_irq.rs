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
// the LCD geometry are hardware timing facts. Predicate/timing functions below
// mirror the observed silicon edge cases; the algebraic time helpers are kept
// verbatim where any restructuring risks the sub-dot STAT tests.
use serde::{Deserialize, Serialize};

pub const LCD_CYCLES_PER_LINE: u32 = 456;
pub const LCD_LINES_PER_FRAME: u32 = 154;
pub const LCD_VRES: u32 = 144;
pub const LCD_CYCLES_PER_FRAME: u64 = LCD_LINES_PER_FRAME as u64 * LCD_CYCLES_PER_LINE as u64;

pub const STAT_M0EN: u8 = 0x08;
pub const STAT_M1EN: u8 = 0x10;
pub const STAT_M2EN: u8 = 0x20;
pub const STAT_LYCEN: u8 = 0x40;

pub const DISABLED_TIME: u64 = u64::MAX / 4;

const MODE1_IRQ_FRAME_CYCLE: i64 = LCD_VRES as i64 * LCD_CYCLES_PER_LINE as i64 - 2;
const MODE2_IRQ_LINE_CYCLE: i64 = LCD_CYCLES_PER_LINE as i64 - 4;
const MODE2_IRQ_LINE_CYCLE_LY0: i64 = LCD_CYCLES_PER_LINE as i64 - 2;

/// A snapshot of the LY counter used to place frame/line events on the absolute
/// dot clock. `time` is the absolute `cc` (dots) at which LY next increments.
#[derive(Clone, Copy)]
pub struct LyCounter {
    pub ly: u32,
    pub time: u64,
    pub ds: bool,
}

impl LyCounter {
    pub fn line_time(&self) -> u64 {
        (LCD_CYCLES_PER_LINE as u64) << self.ds as u32
    }

    pub fn frame_cycles(&self, cc: u64) -> u64 {
        self.ly as u64 * LCD_CYCLES_PER_LINE as u64 + self.line_cycles(cc)
    }

    pub fn line_cycles(&self, cc: u64) -> u64 {
        LCD_CYCLES_PER_LINE as u64 - ((self.time - cc) >> self.ds as u32)
    }

    pub fn next_line_cycle(&self, line_cycle: i64, cc: u64) -> u64 {
        // The result may land just before `cc`; a single line-length wrap pulls
        // it forward, done in wrapping u64 arithmetic to tolerate the underflow.
        let mut tmp = (self.time as i64 + (line_cycle << self.ds as u32)) as u64;
        if tmp.wrapping_sub(cc) > self.line_time() {
            tmp = tmp.wrapping_sub(self.line_time());
        }
        tmp
    }

    pub fn next_frame_cycle(&self, frame_cycle: i64, cc: u64) -> u64 {
        let span = ((LCD_LINES_PER_FRAME as i64 - 1 - self.ly as i64) * LCD_CYCLES_PER_LINE as i64
            + frame_cycle)
            << self.ds as u32;
        let mut tmp = (self.time as i64 + span) as u64;
        let frame = LCD_CYCLES_PER_FRAME << self.ds as u32;
        if tmp.wrapping_sub(cc) > frame {
            tmp = tmp.wrapping_sub(frame);
        }
        tmp
    }
}

pub struct LyCmp {
    pub ly: u32,
    pub time_to_next_ly: i64,
}

/// The LY value the LYC=LY comparator uses. In the final few dots of a line the
/// comparator already anticipates the upcoming line, so this returns that
/// upcoming LY together with the remaining dots until it takes effect.
pub fn get_lyc_cmp_ly(lc: &LyCounter, cc: u64) -> LyCmp {
    let mut ly = lc.ly;
    let mut ttnl = lc.time as i64 - cc as i64;
    let ds = lc.ds as i64;
    if ly == LCD_LINES_PER_FRAME - 1 {
        let line_time = lc.line_time() as i64;
        ttnl -= line_time - 6 - 6 * ds;
        if ttnl <= 0 {
            ly = 0;
            ttnl += line_time;
        }
    } else {
        ttnl -= 2 + 2 * ds;
        if ttnl <= 0 {
            ly += 1;
            ttnl += lc.line_time() as i64;
        }
    }
    LyCmp { ly, time_to_next_ly: ttnl }
}

/// The LY value one line later. LY counts 0..=153 and wraps to 0 after the
/// final line, so advancing modulo the frame height gives the next line.
pub fn inc_ly(ly: u32) -> u32 {
    (ly + 1) % LCD_LINES_PER_FRAME
}

pub fn mode1_irq_schedule(lc: &LyCounter, cc: u64) -> u64 {
    lc.next_frame_cycle(MODE1_IRQ_FRAME_CYCLE, cc)
}

pub fn mode2_irq_schedule(stat: u8, lc: &LyCounter, cc: u64) -> u64 {
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
pub struct LycIrq {
    pub time: u64,
    lyc_reg_src: u8,
    stat_reg_src: u8,
    lyc_reg: u8,
    stat_reg: u8,
    cgb: bool,
}

fn lyc_schedule(stat: u8, lyc: u8, lc: &LyCounter, cc: u64) -> u64 {
    if (stat & STAT_LYCEN != 0) && (lyc as u32) < LCD_LINES_PER_FRAME {
        let fc = if lyc != 0 {
            lyc as i64 * LCD_CYCLES_PER_LINE as i64 - 2
        } else {
            (LCD_LINES_PER_FRAME as i64 - 1) * LCD_CYCLES_PER_LINE as i64 + 6
        };
        lc.next_frame_cycle(fc, cc)
    } else {
        DISABLED_TIME
    }
}

// An LYC match on a line that is also a mode-2 (visible lines 1..=144) or
// mode-1 (line 0 / v-blank) entry is suppressed when that mode's own STAT
// source is enabled, since the coincident mode IRQ takes precedence.
fn lyc_blocked_by_m2_or_m1(ly: u32, stat: u8) -> bool {
    let mode_bit = if (1..=LCD_VRES).contains(&ly) { STAT_M2EN } else { STAT_M1EN };
    stat & mode_bit != 0
}

impl LycIrq {
    pub fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    pub fn lyc_reg_src(&self) -> u8 {
        self.lyc_reg_src
    }

    pub fn lcd_reset(&mut self) {
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

    pub fn stat_reg_change(&mut self, stat: u8, lc: &LyCounter, cc: u64) {
        let lyc = self.lyc_reg_src;
        self.reg_change(stat, lyc, lc, cc);
    }

    pub fn lyc_reg_change(&mut self, lyc: u8, lc: &LyCounter, cc: u64) {
        let stat = self.stat_reg_src;
        self.reg_change(stat, lyc, lc, cc);
    }

    /// Returns true if an LYC IRQ should be flagged.
    pub fn do_event(&mut self, lc: &LyCounter) -> bool {
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

    pub fn reschedule(&mut self, lc: &LyCounter, cc: u64) {
        self.time = lyc_schedule(self.stat_reg, self.lyc_reg, lc, cc)
            .min(lyc_schedule(self.stat_reg_src, self.lyc_reg_src, lc, cc));
    }

    /// Seed all source/committed registers from the live FF41/FF45 values,
    /// e.g. on LCD enable. Does not schedule; call `reschedule` afterwards.
    pub fn seed(&mut self, stat: u8, lyc: u8) {
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
pub struct MStatIrq {
    lyc_reg: u8,
    stat_reg: u8,
}

impl MStatIrq {
    pub fn lcd_reset(&mut self, lyc_reg: u8) {
        self.lyc_reg = lyc_reg;
    }

    pub fn seed(&mut self, stat: u8, lyc: u8) {
        self.stat_reg = stat;
        self.lyc_reg = lyc;
    }

    pub fn lyc_reg_change(
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

    pub fn stat_reg_change(
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

    pub fn do_m0_event(&mut self, ly: u32, stat: u8, lyc: u8) -> bool {
        let flag = ((stat | self.stat_reg) & STAT_M0EN != 0)
            && ((self.stat_reg & STAT_LYCEN == 0) || ly != self.lyc_reg as u32);
        self.lyc_reg = lyc;
        self.stat_reg = stat;
        flag
    }

    pub fn do_m1_event(&mut self, stat: u8) -> bool {
        let flag =
            (stat & STAT_M1EN != 0) && (self.stat_reg & (STAT_M2EN | STAT_M0EN) == 0);
        self.stat_reg = stat;
        flag
    }

    pub fn do_m2_event(&mut self, ly: u32, stat: u8, lyc: u8) -> bool {
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
// for the two hardware variants. (Left in their original condition-by-condition
// form: the sub-dot boundary tests are validated only against the timing test
// ROMs, so the structure is not re-expressed here.)

/// DMG variant. `m0_irq_time` is the absolute dot of the current line's mode-0
/// IRQ (DISABLED_TIME if none scheduled).
pub fn stat_change_triggers_dmg(
    old: u8,
    lc: &LyCounter,
    cc: u64,
    m0_irq_time: u64,
    lyc_reg: u8,
) -> bool {
    let lyc_cmp = get_lyc_cmp_ly(lc, cc);
    if lc.ly < LCD_VRES {
        if m0_irq_time == DISABLED_TIME || m0_irq_time < lc.time {
            return lyc_cmp.ly == lyc_reg as u32 && (old & STAT_LYCEN == 0);
        }
        return (old & STAT_M0EN == 0)
            && !(lyc_cmp.ly == lyc_reg as u32 && (old & STAT_LYCEN != 0));
    }
    (old & STAT_M1EN == 0) && !(lyc_cmp.ly == lyc_reg as u32 && (old & STAT_LYCEN != 0))
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
    if ly < LCD_VRES as i64 - 1 {
        return time_to_next_ly <= (line - MODE2_IRQ_LINE_CYCLE) * (1 + ds) && time_to_next_ly > 2;
    }
    if ly == LCD_VRES as i64 - 1 {
        return time_to_next_ly <= (line - MODE2_IRQ_LINE_CYCLE) * (1 + ds)
            && time_to_next_ly > 4 + 2 * ds;
    }
    if ly == LCD_LINES_PER_FRAME as i64 - 1 {
        return time_to_next_ly <= (line - MODE2_IRQ_LINE_CYCLE_LY0) * (1 + ds)
            && time_to_next_ly > 2;
    }
    false
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

    if ly < LCD_VRES as i64 - 1
        || (ly == LCD_VRES as i64 - 1 && time_to_next_ly > m1_irq_lc_inv * (1 + ds))
    {
        if m0_irq_time < lc.time
            || time_to_next_ly <= (if ly < LCD_VRES as i64 - 1 { 4 + 4 * ds } else { 4 + 2 * ds })
        {
            return lycperiod && (data & STAT_LYCEN != 0);
        }
        if old & STAT_M0EN != 0 {
            return false;
        }
        return (data & STAT_M0EN != 0) || (lycperiod && (data & STAT_LYCEN != 0));
    }

    if (old & STAT_M1EN != 0)
        && (ly < LCD_LINES_PER_FRAME as i64 - 1 || time_to_next_ly > 3 + 3 * ds)
    {
        return false;
    }
    ((data & STAT_M1EN != 0)
        && (ly < LCD_LINES_PER_FRAME as i64 - 1 || time_to_next_ly > 4 + 2 * ds))
        || (lycperiod && (data & STAT_LYCEN != 0))
}

pub fn stat_change_triggers_cgb(
    old: u8,
    data: u8,
    lc: &LyCounter,
    cc: u64,
    m0_irq_time: u64,
    lyc_reg: u8,
) -> bool {
    if (data & !old & (STAT_LYCEN | STAT_M2EN | STAT_M1EN | STAT_M0EN)) == 0 {
        return false;
    }
    let ly = lc.ly as i64;
    let time_to_next_ly = lc.time as i64 - cc as i64;
    let lyc_cmp = get_lyc_cmp_ly(lc, cc);
    let lycperiod = lyc_cmp.ly == lyc_reg as u32 && lyc_cmp.time_to_next_ly > 2;
    if lycperiod && (old & STAT_LYCEN != 0) {
        return false;
    }
    stat_change_triggers_m0lyc_or_m1_cgb(old, data, lycperiod, lc, cc, m0_irq_time)
        || stat_change_triggers_m2_cgb(old, data, ly, time_to_next_ly, lc.ds)
}

// ---- Immediate STAT IRQ on an FF45 (LYC) write ----
// Writing LYC can raise an LYC=LY interrupt on the same dot if the new value
// matches the LY currently being compared. Structure left as-is (sub-dot
// boundary handling validated only against the timing test ROMs).

fn lyc_change_blocked_by_m0_or_m1(
    data: u8,
    lc: &LyCounter,
    cc: u64,
    stat: u8,
    m0_irq_time: u64,
    cgb: bool,
) -> bool {
    let time_to_next_ly = lc.time as i64 - cc as i64;
    if lc.ly < LCD_VRES {
        return (stat & STAT_M0EN != 0)
            && m0_irq_time > lc.time
            && data as u32 == lc.ly;
    }
    (stat & STAT_M1EN != 0)
        && !(lc.ly == LCD_LINES_PER_FRAME - 1
            && time_to_next_ly <= 2 + 2 * lc.ds as i64 + 2 * cgb as i64)
}

pub fn lyc_change_triggers_stat_irq(
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
    let mut lyc_cmp = get_lyc_cmp_ly(lc, cc);
    if lyc_cmp.time_to_next_ly <= 4 + 4 * lc.ds as i64 + 2 * cgb as i64 {
        if old as u32 == lyc_cmp.ly && lyc_cmp.time_to_next_ly > 2 * cgb as i64 {
            return false;
        }
        lyc_cmp.ly = inc_ly(lyc_cmp.ly);
    }
    data as u32 == lyc_cmp.ly
}

/// After a mode-2 IRQ fires, the dot delta to the next one. With mode-0 also
/// enabled the sources coincide once per frame; otherwise mode 2 recurs each
/// line, with LY=0 (line 153) and the last visible line shifting the delta by
/// the LY=0 slot's offset.
pub fn mode2_reschedule_delta(ly: u32, stat: u8, ds: bool) -> u64 {
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

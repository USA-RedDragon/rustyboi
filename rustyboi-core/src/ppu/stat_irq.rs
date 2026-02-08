// Event-scheduled STAT/mode/LYC interrupt model, ported from Gambatte
// (libgambatte/src/video.cpp + video/lyc_irq.*, video/mstat_irq.h).
//
// Gambatte predicts each STAT interrupt source as an absolute cycle time and
// fires whichever event is earliest; register writes recompute those times and
// may immediately flag an IRQ. rustyboi ticks the PPU per-dot, so we keep an
// absolute dot counter (`cc`) and, each dot, fire any event whose scheduled
// time equals the current `cc`. Times are in dots (single-speed cycles); the
// caller passes `ds` (double speed) so the `<< ds` scaling matches Gambatte.
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

/// A snapshot of the LY counter that lets us evaluate Gambatte's
/// `lyCounter` helpers against an absolute dot clock. `time` is the absolute
/// `cc` (dots) at which LY next increments.
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
        // Unsigned wrapping arithmetic, matching Gambatte.
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

/// getLycCmpLy: the LY value used for the LYC=LY compare, which anticipates the
/// next line in the last few dots of the current one.
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

pub fn inc_ly(ly: u32) -> u32 {
    if ly == LCD_LINES_PER_FRAME - 1 { 0 } else { ly + 1 }
}

pub fn mode1_irq_schedule(lc: &LyCounter, cc: u64) -> u64 {
    lc.next_frame_cycle(MODE1_IRQ_FRAME_CYCLE, cc)
}

pub fn mode2_irq_schedule(stat: u8, lc: &LyCounter, cc: u64) -> u64 {
    if stat & STAT_M2EN == 0 {
        return DISABLED_TIME;
    }
    // Gambatte does this in unsigned arithmetic; the subtraction wraps when
    // frameCycles < lastM2Fc, which is intentional (selects the regular branch
    // for early-frame schedules).
    let last_m2_fc =
        ((LCD_VRES - 1) * LCD_CYCLES_PER_LINE) as u64 + MODE2_IRQ_LINE_CYCLE as u64;
    let ly0_m2_fc =
        ((LCD_LINES_PER_FRAME - 1) * LCD_CYCLES_PER_LINE) as u64 + MODE2_IRQ_LINE_CYCLE_LY0 as u64;
    if lc.frame_cycles(cc).wrapping_sub(last_m2_fc) < ly0_m2_fc.wrapping_sub(last_m2_fc)
        || (stat & STAT_M0EN != 0)
    {
        lc.next_frame_cycle(ly0_m2_fc as i64, cc)
    } else {
        lc.next_line_cycle(MODE2_IRQ_LINE_CYCLE, cc)
    }
}

// ---- LycIrq (video/lyc_irq.*) ----
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

fn lyc_blocked_by_m2_or_m1(ly: u32, stat: u8) -> bool {
    if ly <= LCD_VRES && ly > 0 {
        stat & STAT_M2EN != 0
    } else {
        stat & STAT_M1EN != 0
    }
}

impl LycIrq {
    #[allow(dead_code)]
    pub fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    pub fn lyc_reg(&self) -> u8 {
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
            let cmp_ly = if lc.ly == LCD_LINES_PER_FRAME - 1 { 0 } else { lc.ly + 1 };
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

// ---- MStatIrqEvent (video/mstat_irq.h) ----
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
        let cmp = if ly == 0 { 0 } else { ly - 1 };
        let blocked_by_lyc = (self.stat_reg & STAT_LYCEN != 0) && cmp == self.lyc_reg as u32;
        let flag = !blocked_by_m1 && !blocked_by_lyc;
        self.lyc_reg = lyc;
        self.stat_reg = stat;
        flag
    }
}

// ---- statChangeTriggers* (immediate-fire on FF41 write) ----

/// DMG: statChangeTriggersStatIrqDmg. `m0_irq_time` is the absolute event time
/// for the current line's mode-0 IRQ (DISABLED_TIME if none scheduled),
/// `next_ly_time` = lyCounter.time().
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

#[allow(clippy::too_many_arguments)]
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

// ---- lycRegChangeTriggersStatIrq ----

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

/// doMode2IrqEvent's reschedule: compute the next m2 event time given the LY
/// that was used for the firing and whether m0 is enabled.
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

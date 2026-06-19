use crate::cpu::registers;
use crate::memory::mmio;
use crate::memory::Addressable;
use crate::ppu::fetcher;
use crate::ppu::stat_irq;
use serde::{Deserialize, Serialize};

pub const LCD_CONTROL: u16 = 0xFF40;
pub const LCD_STATUS: u16 = 0xFF41;
pub const LY: u16 = 0xFF44;
pub const SCY: u16 = 0xFF42;
pub const SCX: u16 = 0xFF43;
pub const LYC: u16 = 0xFF45;
pub const BGP: u16 = 0xFF47;
pub const OBP0: u16 = 0xFF48; // Object Palette 0 Data
pub const OBP1: u16 = 0xFF49; // Object Palette 1 Data
pub const WY: u16 = 0xFF4A;  // Window Y Position
pub const WX: u16 = 0xFF4B;  // Window X Position

pub const FRAMEBUFFER_SIZE: usize = 160 * 144;

// OAM constants
pub const OAM_SPRITE_COUNT: usize = 40; // 40 sprites total in OAM
pub const OAM_BYTES_PER_SPRITE: usize = 4; // 4 bytes per sprite
pub const MAX_SPRITES_PER_LINE: usize = 10; // Maximum 10 sprites per scanline

const DMG_PIXEL_TRANSFER_ARM_DOT: u128 = 80;
const CGB_PIXEL_TRANSFER_ARM_DOT: u128 = 82;
const DMG_PIXEL_TRANSFER_WARMUP: u8 = 4;
const CGB_PIXEL_TRANSFER_WARMUP: u8 = 2;
// First line after LCDC.7 0->1: Gambatte sets the PPU's internal cycle
// counter to -(m3StartLineCycle + 2), so the first M3 begins
// (m3StartLineCycle + 2) dots after enable. m3StartLineCycle = 83 + cgb,
// giving 85 (DMG) / 86 (CGB) dots from enable to first M3.
const DMG_FIRST_FRAME_ARM_DOT: u128 = 85;
// Gambatte's documented first-M3 start is m3StartLineCycle+2 = 86 (CGB), but the
// emulated first-line pixel pipeline (warmup + arm) lands the mode-0 transition
// two dots late versus hardware at this point. Arming two dots earlier aligns the
// first-line mode-0 IRQ. Calibrated against enable_display m0irq cases (3 fixed,
// 1 regressed -> net -2; only the enable_display cluster moves).
const CGB_FIRST_FRAME_ARM_DOT: u128 = 84;
// On the first line after enable, VRAM/OAM lock (PPU reports mode 3) at the
// same line-cycle as a normal line (Gambatte: lineCycles >= ~79), even though
// the actual pixel fetch (M3Start) begins later at FIRST_FRAME_ARM_DOT.
const DMG_FIRST_FRAME_LOCK_DOT: u128 = 80;
const CGB_FIRST_FRAME_LOCK_DOT: u128 = 82;
// At double speed the CGB first-frame VRAM/OAM lock engages one dot earlier than
// the single-speed boundary. Calibrated against enable_display _ds CGB cases
// (oambusy_read_ds, cgbpw_ds, vramr_ds: 3 fixed, 1 regressed -> net -2; only the
// enable_display cluster moves).
const CGB_FIRST_FRAME_LOCK_DOT_DS: u128 = 81;
fn dmg_first_frame_lock_dot() -> u128 { env_off("RB_DMG_FF_LOCK", DMG_FIRST_FRAME_LOCK_DOT as i64).max(0) as u128 }
fn cgb_first_frame_lock_dot(double_speed: bool) -> u128 {
    let default = if double_speed { CGB_FIRST_FRAME_LOCK_DOT_DS } else { CGB_FIRST_FRAME_LOCK_DOT };
    env_off("RB_CGB_FF_LOCK", default as i64).max(0) as u128
}
// Offset between rustyboi's `ticks` at M3 arm and Gambatte's lineCycle frame
// for the scheduled Mode 3 -> Mode 0 transition. Swept against the full suite.
const DMG_MODE0_OFFSET: i32 = 4;
const CGB_MODE0_OFFSET: i32 = 4;
// Mode-3 dot penalty for a window starting on this line (Gambatte StartWindowDraw).
const WIN_M3_PENALTY: i32 = 6;
// Offset (dots) between the renderer's scheduled mode-0 transition and the
// event-model mode-0 STAT IRQ fire time. Tuned against the suite.
const M0IRQ_OFFSET: i64 = -3;
// Mode-2 STAT IRQ fires this many dots relative to the schedule formula; the
// renderer-timed render tests need it earlier. Swept against the suite.
const M2IRQ_OFFSET: i64 = -1;
// Absolute-clock offset attributed to an FF41/FF45 register write. The write
// hook fires after the store but before this M-cycle's dots tick, so the
// renderer's current dot is already `abs_cc` (the M-cycle start), matching
// Gambatte's `write(addr, data, cc)` resolving at `cc` before `cc += 4`. No
// extra bias is needed at single speed. Swept against the full suite (0 beats
// the former -1 by 32 net).
const WRITE_CC_OFFSET: i64 = 0;

// Sentinel for "no pending wy2 update".
fn wy2_disabled() -> u64 { u64::MAX }
fn pnow_disabled() -> u64 { u64::MAX }
fn win_y_pos_init() -> u8 { 0xFF }

// Env-tunable override of an i64 offset (for sweeping during development). When
// the named env var is unset, the compiled-in default is used.
#[inline]
fn env_off(name: &str, default: i64) -> i64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
// DS offsets re-derived after the double-speed STAT sub-dot step (step_subdot)
// gave the IRQ model true odd-cc resolution: m2 relaxes -2 -> -1 (the odd-cc
// fire is now caught by the sub-dot rather than rounded down), and the write cc
// tightens -3 -> -4.
fn write_cc_off_ds() -> i64 { env_off("RB_WRITE_CC_OFF_DS", -4) }
fn m0irq_off_ds() -> i64 { env_off("RB_M0IRQ_OFF_DS", M0IRQ_OFFSET) }
fn m2irq_off_ds() -> i64 { env_off("RB_M2IRQ_OFF_DS", -1) }
// Sweep-tunable single-speed offsets (default to the compiled-in constants).
fn dmg_mode0_offset() -> i32 { env_off("RB_DMG_MODE0_OFF", DMG_MODE0_OFFSET as i64) as i32 }
fn cgb_mode0_offset() -> i32 { env_off("RB_CGB_MODE0_OFF", CGB_MODE0_OFFSET as i64) as i32 }
fn m0irq_off_ss() -> i64 { env_off("RB_M0IRQ_OFF", M0IRQ_OFFSET) }
fn m2irq_off_ss() -> i64 { env_off("RB_M2IRQ_OFF", M2IRQ_OFFSET) }
fn write_cc_off_ss() -> i64 { env_off("RB_WRITE_CC_OFF", WRITE_CC_OFFSET) }
const MODE2_STAT_PRETRIGGER_DOT: u128 = 452;
// Within line 153 (the last VBlank line) the LY register is held at 153 only
// briefly; after this many dots it reads 0, even though the line itself
// continues until dot 455. This matches Gambatte's getLycCmpLy threshold
// (lineTime - 6 in single speed) and makes the LYC=LY interrupt for LY=0
// fire one line earlier than a naive end-of-line transition would suggest.
const LINE_153_LY_ZERO_DOT: u128 = 6;

// Sprite attribute flags (from byte 3 of sprite data)
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct SpriteAttributes {
    pub priority: bool,    // 0 = above BG, 1 = behind BG colors 1-3
    pub y_flip: bool,      // 0 = normal, 1 = vertically mirrored
    pub x_flip: bool,      // 0 = normal, 1 = horizontally mirrored
    pub palette: bool,     // 0 = OBP0, 1 = OBP1 (DMG compatibility)
    pub raw: u8,           // Raw attribute byte for CGB palette access
}

impl SpriteAttributes {
    pub fn from_byte(byte: u8) -> Self {
        SpriteAttributes {
            priority: (byte & 0x80) != 0,
            y_flip: (byte & 0x40) != 0,
            x_flip: (byte & 0x20) != 0,
            palette: (byte & 0x10) != 0,
            raw: byte,
        }
    }
}

// Sprite data structure
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Sprite {
    pub y: u8,
    pub x: u8,
    pub tile_index: u8,
    pub attributes: SpriteAttributes,
    pub oam_index: u8, // For priority resolution
}

pub enum LCDCFlags {
    BGDisplay = 1<<0,
    SpriteDisplayEnable = 1<<1,
    SpriteSize = 1<<2,
    BGTileMapDisplaySelect = 1<<3,
    BGWindowTileDataSelect = 1<<4,
    WindowDisplayEnable = 1<<5,
    WindowTileMapDisplaySelect = 1<<6,
    DisplayEnable = 1<<7,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum State {
    OAMSearch,
    PixelTransfer,
    HBlank,
    VBlank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchDebugEventKind {
    TileNumber,
    TileDataLow,
    TileDataHigh,
    PushToFifo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FetchDebugEvent {
    pub kind: FetchDebugEventKind,
    pub ppu_ticks: u128,
    pub x: u8,
    pub ly: u8,
    pub fifo_size: usize,
    pub tile_index: u8,
    pub tile_num: u8,
    pub tile_attributes: u8,
    pub tile_line: u8,
    pub addr: Option<u16>,
    pub value: Option<u8>,
    pub lcdc: u8,
    pub tile_index_is_tile_data: bool,
    pub fetching_window: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelDebugEvent {
    pub ppu_ticks: u128,
    pub x: u8,
    pub ly: u8,
    pub bg_pixel_idx: u8,
    pub rgb: [u8; 3],
    pub lcdc: u8,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
enum PendingLcdcEventKind {
    TileDataSelectOnly,
    Full,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
struct PendingLcdcEvent {
    cycles_remaining: u32,
    base_value: u8,
    value: u8,
    kind: PendingLcdcEventKind,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CgbColorConversion {
    #[default]
    Linear,
    Gambatte,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Ppu {
    fetcher: fetcher::Fetcher,
    disabled: bool,
    state: State,
    ticks: u128,
    x: u8,

    // Sprite data for current scanline
    sprites_on_line: Vec<Sprite>,
    current_oam_sprite_index: usize, // Current sprite being checked during OAM search
    #[serde(default)]
    next_sprite_fetch_index: usize,
    #[serde(default)]
    sprite_fetch_stall: u8,
    #[serde(default)]
    pixel_transfer_warmup: u8,
    // Fetcher cadence counter, decoupled from absolute self.ticks so that
    // sprite-fetch stall dots do not flip the fetcher's even/odd phase.
    // Reset to 0 on every OAMSearch -> PixelTransfer transition.
    #[serde(default)]
    fetcher_cadence_tick: u8,
    
    // Window state tracking
    window_line_counter: u8,    // Internal counter for window Y position
    // Gambatte's `winYPos`: the window's internal Y line, incremented by 1 ONLY
    // at the moment the window actually begins drawing on a line (M3Start::f0 /
    // plotPixel draw-start), NOT per-line whenever ly > wy. Initialized to 0xFF
    // at frame start so the first window-draw line yields winYPos == 0. The
    // fetcher uses this (masked) for the window tile row / tile line.
    #[serde(default = "win_y_pos_init")]
    win_y_pos: u8,
    // Gambatte's `win_draw_start` bit of winDrawState. On DMG, when WX matches
    // at xpos == 166 (lcd_hres+6) the window cannot draw this line (the line
    // ends first) but ARMS: win_draw_start is set and survives into the next
    // line, where M3Start::f0 activates the window from x==0 (++winYPos) even
    // though WX is unchanged. Set during a line, consumed at the next line's
    // M3 start. CGB never arms this way (handled by plotPixel's !cgb guard).
    #[serde(default)]
    win_draw_start: bool,
    // Set at this line's M3 start (M3Start::f0) when win_draw_start was armed
    // from the previous line and the window is enabled: the window draws from
    // x==0 this line regardless of WX. Consumed by the PixelTransfer window
    // start at x==0.
    #[serde(default)]
    win_draw_started_at_x0: bool,
    window_y_triggered: bool,   // Whether WY condition was met this frame
    window_started_this_line: bool, // Whether window started rendering on current scanline
    // Dot (within-line `ticks`) at which the window began drawing this line.
    // The StartWindowDraw mode-3 penalty becomes non-refundable once the
    // pipeline advances WIN_M3_PENALTY dots past this; used by the late_disable
    // read-at-cc recompute to decide whether a mid-M3 window-disable keeps the
    // window-inclusive m0Time or reverts to the no-window length.
    win_start_dot: Option<u128>,
    // Set once a late-WX mid-window refund has been applied this line, so a
    // second WX write does not refund twice.
    win_wx_penalty_resolved: bool,
    
    // STAT interrupt state tracking
    previous_stat_interrupt_line: bool, // Previous state of STAT interrupt line for edge detection
    #[serde(default)]
    mode2_irq_pretriggered_for_next_line: bool,
    // True for the first scanline after LCDC.7 transitions 0 -> 1. On real
    // hardware this line has no Mode 2 phase: STAT reports mode 0 until M3
    // begins, no Mode 2 STAT IRQ fires, and M3 starts later than usual
    // (dot 85 on DMG / 86 on CGB instead of 80 / 82).
    #[serde(default)]
    first_line_after_enable: bool,
    // True once we've zeroed FF44 partway through line 153 and before the
    // line itself ends. Used to gate the end-of-frame transition and the
    // LY=0 Mode 2 pretrigger (both of which originally checked LY==153).
    #[serde(default)]
    line_153_ly_zeroed: bool,
    // True once the current line's Mode 0 (HBlank) FF41 mode bits and
    // STAT IRQ have been pretriggered. Gambatte's `getStat` reports mode
    // 0 starting two cycles before the actual Mode 3 -> Mode 0 transition
    // (`cc + 2 < m0TimeOfCurrentLine`); pretrigger Mode 0 from the pixel
    // push at x == 158 so the FF41 read-back and the wired-OR mode-0 IRQ
    // fire at the right cycle. Reset when entering PixelTransfer.
    #[serde(default)]
    mode0_pretriggered_this_line: bool,
    // Number of BG pixels discarded so far for SCX fine-scroll alignment at
    // the start of Mode 3 (while x == 0). Faithful to Gambatte's M3Start::f1
    // per-dot loop: each dot, the LIVE `scx % 8` is re-read; if we have not
    // yet discarded that many pixels we pop one and consume the dot, else we
    // begin output. A mid-M3 SCX write therefore changes both the discard
    // count and (because the BG tile column re-reads SCX live) the fetched
    // tile-map column. Reset to 0 at every M3 arm.
    #[serde(default)]
    m3_pixels_discarded: u8,
    // Fine-scroll discard target latched at M3 start (Gambatte M3Start::f1
    // samples `scx % 8` when the loop first runs, at M3 start, before the
    // mode-2 STAT handler's mid-M3 SCX write lands). Reading SCX live in the
    // pop loop samples it too late (after FIFO latency), capturing the
    // already-written value and over-discarding. -1 = not yet latched.
    #[serde(default)]
    m3_discard_target: i8,
    // Dot at which the current line's M3 (PixelTransfer) was armed. xpos in
    // Gambatte's M3Start::f1 loop == ticks - this. Used to re-read SCX at the
    // same early M3 dots Gambatte samples, so a mid-discard SCX write moves the
    // break target without the FIFO-warmup latency over-reading later writes.
    #[serde(default)]
    m3_arm_dot: u128,
    // scx%8 sampled at M3 arm, used by the closed-form mode-0 schedule's
    // discard prefix. If the live f1 break resolves to a different count, the
    // schedule is nudged by the difference so M3 ends at the right dot.
    #[serde(default)]
    m3_arm_scx: u8,
    // WX snapshot taken when the closed-form mode-0 schedule was computed; a
    // mid-mode-3 WX change before the window starts invalidates the schedule.
    m3_scheduled_wx: u8,
    // window_will_start() result at schedule time; a mid-mode-3 WY write that
    // flips it (late WY==ly) invalidates the schedule.
    #[serde(default)]
    m3_scheduled_win: bool,
    // OBJ-size (LCDC bit2) value used by the mode-2 OAM scan, latched one scan
    // slot behind the live LCDC. Gambatte's SpriteMapper latches the per-OAM
    // entry size (`lsbuf_[pos/2]`) when that entry's OAM slot is read; a mid-mode-2
    // size write only affects entries scanned strictly AFTER the write commits.
    // Refreshed from the live LCDC after each scan slot so a write landing within
    // a slot's window applies to the next slot (the late_sizechange 1-cc boundary).
    #[serde(default)]
    scan_obj_size_large: bool,
    // Absolute `ticks` dot at which Mode 3 -> Mode 0 (HBlank) fires. Computed
    // at M3 arm from a cycle-exact mode-3 length formula (Gambatte oracle) and
    // drives the FF41 mode bits + mode-0 STAT IRQ, replacing the x==160 trigger.
    #[serde(default)]
    scheduled_mode0_dot: Option<u128>,
    // Gambatte's `m0TimeOfCurrentLine` in MASTER-cc units: the absolute clock at
    // which the predicted mode-3 -> mode-0 transition occurs, equal to
    // `predictedNextXposTime(167) = now_at_arm + (m3_len << ds)`. Captured at M3
    // arm (master_cc + m3_len<<ds). The CPU's FF41 read resolves mode 3 iff
    // `access_cc + 2 < m0_time_master` (Gambatte `getStat`); the mode-0 STAT IRQ
    // fires one xpos earlier (`predictedNextXposTime(166) = m0Time - (1<<ds)`).
    // None when no closed-form dot is available (window / first line).
    #[serde(default)]
    m0_time_master: Option<u64>,
    // Master-cc anchor at which CGB palette RAM (FF69/FF6B) becomes INACCESSIBLE
    // for the current line (Gambatte `cgbpAccessible`: blocked once
    // `lineCycles(cc) + ds >= 80`). Captured at M3 arm from the same master_cc /
    // m3_arm_dot the m0_time_master uses, so the cgbp begin boundary resolves at
    // the CPU's access cc rather than the renderer dot (whose pre/post-tick phase
    // differs between the read and write paths). None when no closed-form M3 arm
    // exists (first line after enable). Paired with `m0_time_master` for the end.
    #[serde(default)]
    cgbp_block_start_cc: Option<u64>,
    // The CPU-visible mode-0 (HBlank) start dot is computed on demand by
    // `reported_mode0_dot_value` from the closed-form `scheduled_mode0_dot` plus
    // a per-phase early-report nudge. It is decoupled from the live pixel
    // pipeline's actual M3 termination, driving ONLY the FF41 mode bits read back
    // by the CPU and the mode-0 STAT IRQ arm, so it can report mode 0 a few dots
    // EARLIER than the renderer drains its FIFO (Gambatte computes the reported
    // mode from the closed-form mode-3 length, not from the pixel-pump
    // termination) without ever hanging M3. This flag latches once that report
    // has fired for the current line, so the later live termination does not
    // re-drive the mode bits or re-fire the STAT check.
    #[serde(default)]
    mode0_reported_this_line: bool,

    // Event-scheduled STAT/mode/LYC IRQ model (Gambatte port). `abs_cc` is a
    // monotonic absolute dot clock; `line_cycle` (0..455) tracks position
    // within the current 456-dot line. Together they reproduce Gambatte's
    // `lyCounter` (`time` = abs_cc when LY next increments).
    #[serde(default)]
    abs_cc: u64,
    // LCD-enable anchor (Gambatte `p_.now()` base): the master cc value at which
    // the PPU dot-clock `abs_cc` was last re-based. The PPU's machine-cycle clock
    // is `master_cc - p_now` (both advance 1/T-cycle), so `p_now` folds the PPU
    // onto the single master cc. Re-anchored on LCD enable / LY-write reset, and
    // on every speed change / STOP bridge where the master cc and the PPU's
    // render-dot accumulation diverge in count. DISABLED sentinel until first
    // enable, where it is seeded so the derived value equals the accumulator.
    #[serde(default = "pnow_disabled")]
    p_now: u64,
    // After a DS->SS speed switch the 3-dot stop bridge lands the LyCounter one
    // master-cc higher than Gambatte (the DS half-dot the whole-dot bridge can't
    // express), so the closed-form `+1` LyCounter correction in `m0_time_exact`
    // over-corrects by 1. Set on the DS->SS switch, cleared at the next LCD
    // enable / LY reset. See ENGINE_LAZY_PPU.md bug #2.
    #[serde(default)]
    lytime_no_plus1: bool,
    // Sub-PPU-dot parity (0/1) of the currently-resolving CPU register write at
    // double speed. Set by the bus just before the FF4x write hooks run.
    #[serde(skip, default)]
    write_subdot: u8,
    // Gambatte's `wy2`: WY delayed by `6 - isDoubleSpeed()` cc after a write.
    // Event-scheduled against the write cc; consumed by the window-Y gate so
    // the M3-length predictor / window-start see the delayed value.
    #[serde(default)]
    wy2: u8,
    // Absolute clock at which a pending wy2 update applies; DISABLED when none.
    #[serde(default = "wy2_disabled")]
    wy2_apply_cc: u64,
    // The WY value to latch into wy2 when wy2_apply_cc arrives.
    #[serde(default)]
    wy2_pending: u8,
    // Gambatte's `p.wy` (the value the weMaster checkpoints read): updated at
    // `cc + 1 + cgb` after a write (`update(cc + 1 + cgb)` in `wyChange`).
    // Distinct from `wy2` (the per-line gate value), which is delayed further.
    #[serde(default = "win_y_pos_init")]
    wy1: u8,
    #[serde(default = "wy2_disabled")]
    wy1_apply_cc: u64,
    #[serde(default)]
    wy1_pending: u8,
    // Delayed SCY/SCX visible to the BG fetcher during mode 3. A mid-M3 write to
    // FF42/FF43 resolves in mmio immediately (CPU readback is live), but the
    // fetcher sees the new value only after `scy/scx_apply_cc` (write-side analog
    // of the wy1/wy2 delayed-apply latches). Steady-state these equal the live
    // register, so non-write rendering is unaffected.
    #[serde(default)]
    scy_delayed: u8,
    #[serde(default = "wy2_disabled")]
    scy_apply_cc: u64,
    #[serde(default)]
    scy_pending: u8,
    #[serde(default)]
    scx_delayed: u8,
    #[serde(default = "wy2_disabled")]
    scx_apply_cc: u64,
    #[serde(default)]
    scx_pending: u8,
    #[serde(default)]
    line_cycle: u32,
    #[serde(default)]
    internal_ly_val: u8,
    #[serde(default)]
    sched_lycirq: u64,
    #[serde(default)]
    sched_m1irq: u64,
    #[serde(default)]
    sched_m2irq: u64,
    #[serde(default)]
    sched_m0irq: u64,
    #[serde(default)]
    sched_oneshot_statirq: u64,
    #[serde(default)]
    lyc_irq: stat_irq::LycIrq,
    #[serde(default)]
    mstat_irq: stat_irq::MStatIrq,
    #[serde(default)]
    stat_reg_committed: u8,

    // DMG palette registers delayed by one dot. A BGP/OBP write during mode 3
    // is resolved by the CPU before the four PPU dots of the write M-cycle are
    // stepped, but on hardware the new palette only affects the pixel one dot
    // after the write lands. The renderer resolves palettes at pixel shift-out
    // from these delayed copies; each are refreshed to the live register at the
    // end of every dot, yielding the one-dot apply latency.
    #[serde(default)]
    bgp_delayed: u8,
    #[serde(default)]
    obp0_delayed: u8,
    #[serde(default)]
    obp1_delayed: u8,

    #[serde(with = "serde_bytes")]
    fb_a: [u8; FRAMEBUFFER_SIZE],
    #[serde(with = "serde_bytes")]
    fb_b: [u8; FRAMEBUFFER_SIZE],
    #[serde(with = "serde_bytes")]
    color_fb_a: [u8; FRAMEBUFFER_SIZE * 3], // RGB color framebuffer
    #[serde(with = "serde_bytes")]
    color_fb_b: [u8; FRAMEBUFFER_SIZE * 3], // RGB color framebuffer
    have_frame: bool,
    #[serde(default)]
    lcdc: u8,
    #[serde(default)]
    cgb_tile_index_is_tile_data: bool,
    #[serde(default)]
    pending_lcdc_events: Vec<PendingLcdcEvent>,
    #[serde(default)]
    cgb_color_conversion: CgbColorConversion,
    #[serde(skip, default)]
    fetch_debug_events_enabled: bool,
    #[serde(skip, default)]
    fetch_debug_events: Vec<FetchDebugEvent>,
    #[serde(skip, default)]
    pixel_debug_events: Vec<PixelDebugEvent>,
}

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            fetcher: fetcher::Fetcher::new(),
            disabled: true,
            state: State::OAMSearch,
            ticks: 0,
            x: 0,
            sprites_on_line: Vec::new(),
            current_oam_sprite_index: 0,
            next_sprite_fetch_index: 0,
            sprite_fetch_stall: 0,
            pixel_transfer_warmup: 0,
            fetcher_cadence_tick: 0,
            window_line_counter: 0,
            win_y_pos: 0xFF,
            win_draw_start: false,
            win_draw_started_at_x0: false,
            window_y_triggered: false,
            win_start_dot: None,
            win_wx_penalty_resolved: false,
            window_started_this_line: false,
            previous_stat_interrupt_line: false,
            mode2_irq_pretriggered_for_next_line: false,
            first_line_after_enable: false,
            line_153_ly_zeroed: false,
            mode0_pretriggered_this_line: false,
            m3_pixels_discarded: 0,
            m3_discard_target: -1,
            m3_arm_dot: 0,
            m3_arm_scx: 0,
            m3_scheduled_wx: 0,
            m3_scheduled_win: false,
            scan_obj_size_large: false,
            scheduled_mode0_dot: None,
            m0_time_master: None,
            lytime_no_plus1: false,
            cgbp_block_start_cc: None,
            mode0_reported_this_line: false,
            abs_cc: 0,
            p_now: pnow_disabled(),
            write_subdot: 0,
            wy2: 0,
            wy2_apply_cc: wy2_disabled(),
            wy2_pending: 0,
            wy1: 0xFF,
            wy1_apply_cc: wy2_disabled(),
            wy1_pending: 0,
            scy_delayed: 0,
            scy_apply_cc: wy2_disabled(),
            scy_pending: 0,
            scx_delayed: 0,
            scx_apply_cc: wy2_disabled(),
            scx_pending: 0,
            line_cycle: 0,
            internal_ly_val: 0,
            sched_lycirq: stat_irq::DISABLED_TIME,
            sched_m1irq: stat_irq::DISABLED_TIME,
            sched_m2irq: stat_irq::DISABLED_TIME,
            sched_m0irq: stat_irq::DISABLED_TIME,
            sched_oneshot_statirq: stat_irq::DISABLED_TIME,
            lyc_irq: stat_irq::LycIrq::default(),
            mstat_irq: stat_irq::MStatIrq::default(),
            stat_reg_committed: 0,
            bgp_delayed: 0,
            obp0_delayed: 0,
            obp1_delayed: 0,
            fb_a: [0; FRAMEBUFFER_SIZE],
            fb_b: [0; FRAMEBUFFER_SIZE],
            color_fb_a: [0; FRAMEBUFFER_SIZE * 3],
            color_fb_b: [0; FRAMEBUFFER_SIZE * 3],
            have_frame: false,
            lcdc: 0,
            cgb_tile_index_is_tile_data: false,
            pending_lcdc_events: Vec::new(),
            cgb_color_conversion: CgbColorConversion::Linear,
            fetch_debug_events_enabled: false,
            fetch_debug_events: Vec::new(),
            pixel_debug_events: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn set_cgb_color_conversion(&mut self, conversion: CgbColorConversion) {
        self.cgb_color_conversion = conversion;
    }

    pub fn sync_lcdc_from_mmio(&mut self, mmio: &mmio::Mmio) {
        self.set_lcdc_visible(mmio.read(LCD_CONTROL), mmio.is_cgb_features_enabled(), mmio.is_double_speed_mode());
        self.pending_lcdc_events.clear();
    }

    pub fn handle_lcdc_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        let display_enable = LCDCFlags::DisplayEnable as u8;
        let old_lcdc = self.lcdc;
        let display_stays_enabled = (old_lcdc & display_enable) != 0 && (value & display_enable) != 0;

        if mmio.is_cgb_features_enabled() && display_stays_enabled {
            self.pending_lcdc_events.push(PendingLcdcEvent {
                cycles_remaining: 1,
                base_value: old_lcdc,
                value,
                kind: PendingLcdcEventKind::TileDataSelectOnly,
            });
            // Full lands 2 PPU dots after the write commits, matching Gambatte's
            // `update(cc + 2); setLcdc(data, cc + 2)`.
            self.pending_lcdc_events.push(PendingLcdcEvent {
                cycles_remaining: 2,
                base_value: old_lcdc,
                value,
                kind: PendingLcdcEventKind::Full,
            });
        } else {
            self.pending_lcdc_events.clear();
            self.set_lcdc_visible(value, mmio.is_cgb_features_enabled(), mmio.is_double_speed_mode());
        }
    }

    pub fn step_lcdc_events(&mut self, mmio: &mmio::Mmio) {
        let mut index = 0;
        while index < self.pending_lcdc_events.len() {
            if self.pending_lcdc_events[index].cycles_remaining > 0 {
                self.pending_lcdc_events[index].cycles_remaining -= 1;
            }

            if self.pending_lcdc_events[index].cycles_remaining == 0 {
                let event = self.pending_lcdc_events.remove(index);
                match event.kind {
                    PendingLcdcEventKind::TileDataSelectOnly => {
                        let tile_data_select = LCDCFlags::BGWindowTileDataSelect as u8;
                        let value = (event.base_value & !tile_data_select) | (event.value & tile_data_select);
                        self.set_lcdc_visible(value, mmio.is_cgb_features_enabled(), mmio.is_double_speed_mode());
                    }
                    PendingLcdcEventKind::Full => {
                        self.set_lcdc_visible(event.value, mmio.is_cgb_features_enabled(), mmio.is_double_speed_mode());
                    }
                }
            } else {
                index += 1;
            }
        }
    }

    fn fetcher_lcdc_state(&self) -> fetcher::FetcherLcdcState {
        fetcher::FetcherLcdcState {
            lcdc: self.lcdc,
            cgb_tile_index_is_tile_data: self.cgb_tile_index_is_tile_data,
        }
    }

    fn set_lcdc_visible(&mut self, value: u8, cgb_features_enabled: bool, ds: bool) {
        let old_lcdc = self.lcdc;
        let tile_data_select = LCDCFlags::BGWindowTileDataSelect as u8;
        let display_enable = LCDCFlags::DisplayEnable as u8;
        self.cgb_tile_index_is_tile_data = cgb_features_enabled
            && (old_lcdc & tile_data_select) != 0
            && (value & tile_data_select) == 0
            && (old_lcdc & display_enable) != 0
            && (value & display_enable) != 0;
        // A mid-mode-3 window-enable toggle invalidates the closed-form mode-0
        // schedule (computed at M3 start from the initial WX/LCDC). Fall back to
        // the live emergent x==160 transition, which tracks the change.
        let win_bit = LCDCFlags::WindowDisplayEnable as u8;
        // A mid-mode-3 sprite-enable (bit 1) or sprite-size (bit 2) toggle also
        // changes the closed-form sprite-fetch penalty; invalidate and fall back
        // to the live emergent transition.
        let spr_bits = (LCDCFlags::SpriteDisplayEnable as u8) | (LCDCFlags::SpriteSize as u8);
        if self.state == State::PixelTransfer
            && ((old_lcdc & win_bit) != (value & win_bit)
                || (old_lcdc & spr_bits) != (value & spr_bits))
        {
            self.scheduled_mode0_dot = None;
            // A mid-mode-3 window-DISABLE toggle (not sprite) interacts with the
            // StartWindowDraw mode-3 penalty captured at M3 arm. Gambatte locks
            // the penalty once the window has drawn for WIN_M3_PENALTY dots
            // (StartWindowDraw::inc spans those dots); a disable BEFORE that lock
            // refunds the whole window penalty, a disable after keeps it. The
            // read-at-cc m0Time captured at arm already includes the penalty, so:
            //   - disable >= win_start_dot + WIN_M3_PENALTY: keep m0Time as-is.
            //   - disable <  win_start_dot + WIN_M3_PENALTY: subtract the penalty
            //     (refund) so the FF41 read resolves the no-window boundary.
            //   - window never started: null (fall back; live no-window path).
            // The live pipeline (scheduled_mode0_dot) is invalidated above either
            // way; only the read-at-cc m0Time is adjusted. Sprite-bit toggles
            // null m0Time (the sprite-fetch penalty genuinely changes).
            let enable_off = std::env::var("RB_WIN_KEEP_M0T").map(|v| v == "0").unwrap_or(false);
            let only_win_toggle = (old_lcdc & spr_bits) == (value & spr_bits)
                && (old_lcdc & win_bit) != (value & win_bit)
                && (value & win_bit) == 0; // disable
            // GRADUATED StartWindowDraw refund: the window mode-3 penalty accrues
            // one dot per drawn window dot, capped at WIN_M3_PENALTY. A mid-M3
            // window-disable at dot `ticks` has accrued
            //   accrued = min(WIN_M3_PENALTY, ticks - win_start)
            // dots; the unaccrued remainder is refunded from the read-at-cc
            // m0Time captured (full-penalty) at arm. This generalises the
            // refund/keep across SCX phase and WX (each phase shifts win_start
            // and m0Time together). Scoped CGB / no sprites / single speed; DS
            // keeps the calibrated binary lock below. The live pipeline
            // (scheduled_mode0_dot) is invalidated above regardless.
            let clean_ss = cgb_features_enabled
                && !ds
                && self.sprites_on_line.is_empty();
            let clean_ds = cgb_features_enabled
                && ds
                && self.m3_arm_scx & 7 == 0
                && self.sprites_on_line.is_empty();
            if enable_off || !only_win_toggle || !self.window_started_this_line {
                self.m0_time_master = None;
            } else if clean_ss {
                if let (Some(m0t), Some(ws)) = (self.m0_time_master, self.win_start_dot) {
                    let drawn = (self.ticks as i64) - ws as i64;
                    let accrued = drawn.clamp(0, WIN_M3_PENALTY as i64);
                    let refund = WIN_M3_PENALTY as i64 - accrued;
                    self.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                } else {
                    self.m0_time_master = None;
                }
            } else if clean_ds {
                if let (Some(m0t), Some(ws)) = (self.m0_time_master, self.win_start_dot) {
                    let lock = ws + (WIN_M3_PENALTY as u128 - 2);
                    if (self.ticks as u128) < lock {
                        self.m0_time_master =
                            Some((m0t as i64 - ((WIN_M3_PENALTY as i64) << 1)).max(0) as u64);
                    }
                } else {
                    self.m0_time_master = None;
                }
            } else {
                self.m0_time_master = None;
            }
        }
        self.lcdc = value;
    }

    pub fn set_fetch_debug_events_enabled(&mut self, enabled: bool) {
        self.fetch_debug_events_enabled = enabled;
        if !enabled {
            self.fetch_debug_events.clear();
            self.pixel_debug_events.clear();
        }
    }

    pub fn take_fetch_debug_events(&mut self) -> Vec<FetchDebugEvent> {
        std::mem::take(&mut self.fetch_debug_events)
    }

    pub fn take_pixel_debug_events(&mut self) -> Vec<PixelDebugEvent> {
        std::mem::take(&mut self.pixel_debug_events)
    }

    fn record_fetch_debug_event(&mut self, event: fetcher::FetcherDebugEvent, mmio: &mmio::Mmio) {
        if !self.fetch_debug_events_enabled {
            return;
        }

        let kind = match event.kind {
            fetcher::FetcherDebugEventKind::TileNumber => FetchDebugEventKind::TileNumber,
            fetcher::FetcherDebugEventKind::TileDataLow => FetchDebugEventKind::TileDataLow,
            fetcher::FetcherDebugEventKind::TileDataHigh => FetchDebugEventKind::TileDataHigh,
            fetcher::FetcherDebugEventKind::PushToFifo => FetchDebugEventKind::PushToFifo,
        };

        self.fetch_debug_events.push(FetchDebugEvent {
            kind,
            ppu_ticks: self.ticks,
            x: self.x,
            ly: mmio.read(LY),
            fifo_size: event.fifo_size,
            tile_index: event.tile_index,
            tile_num: event.tile_num,
            tile_attributes: event.tile_attributes,
            tile_line: event.tile_line,
            addr: event.addr,
            value: event.value,
            lcdc: event.lcdc,
            tile_index_is_tile_data: event.tile_index_is_tile_data,
            fetching_window: event.fetching_window,
        });
    }

    fn record_pixel_debug_event(&mut self, ly: u8, bg_pixel_idx: u8, rgb: [u8; 3]) {
        if !self.fetch_debug_events_enabled {
            return;
        }

        self.pixel_debug_events.push(PixelDebugEvent {
            ppu_ticks: self.ticks,
            x: self.x,
            ly,
            bg_pixel_idx,
            rgb,
            lcdc: self.lcdc,
        });
    }

    pub fn get_palette_color(&self, _mmio: &mmio::Mmio, idx: u8) -> u8 {
        let bgp = self.bgp_delayed;
        match idx {
            0 => bgp&0x03,        // White
            1 => (bgp>>2)&0x03, // Light Gray
            2 => (bgp>>4)&0x03, // Dark Gray
            3 => (bgp>>6)&0x03, // Black
            _ => 0x00, // Default to black for invalid indices
        }
    }

    pub fn get_sprite_palette_color(&self, _mmio: &mmio::Mmio, idx: u8, palette: bool) -> u8 {
        if idx == 0 {
            return 0; // Transparent for sprites
        }

        let obp = if palette { self.obp1_delayed } else { self.obp0_delayed };
        match idx {
            1 => (obp>>2)&0x03, // Light Gray
            2 => (obp>>4)&0x03, // Dark Gray
            3 => (obp>>6)&0x03, // Black
            _ => 0x00, // Default to transparent for invalid indices
        }
    }

    // ---- Event-scheduled STAT IRQ model (Gambatte port) ----

    fn ly_counter(&self, mmio: &mmio::Mmio) -> stat_irq::LyCounter {
        let ds = mmio.is_double_speed_mode();
        // `abs_cc` is in machine cycles (advances by 1<<ds per dot). `time` is
        // the machine-cycle clock at the next LY increment.
        let dots_to_next_line = (stat_irq::LCD_CYCLES_PER_LINE - self.line_cycle) as u64;
        stat_irq::LyCounter {
            ly: self.internal_ly() as u32,
            time: self.abs_cc + (dots_to_next_line << ds as u32),
            ds,
        }
    }

    // The internal (clean) LY derived from the line clock, independent of the
    // LY register's mid-line transients (line 153 ly=0, etc.).
    fn internal_ly(&self) -> u8 {
        self.internal_ly_val
    }

    /// Byte-exact Gambatte `m0Time` (master-cc) for the current line, given the
    /// closed-form mode-3 length `m3_len` (= `predictCyclesUntilXpos(167)` dots).
    ///   m0Time = (p_now + ly_counter().time + 1) − ((456 − (m3_len + BASE)) << ds)
    /// BASE = 84 (CGB SS+DS), 83 (DMG). `p_now + ly_counter().time` is the next-LY
    /// master cc; the +1 corrects rustyboi's LyCounter.time running one master-cc
    /// below Gambatte's lyTime. getStat boundary: mode3 iff `master_cc + 2 < m0Time`.
    fn m0_time_exact(&self, mmio: &mmio::Mmio, m3_len: u128, is_cgb: bool) -> u64 {
        let ds = mmio.is_double_speed_mode() as u32;
        let base: i64 = if is_cgb { 84 } else { 83 };
        let plus1 = if self.lytime_no_plus1 { 0 } else { 1 };
        let ly_time = self.p_now as i64 + self.ly_counter(mmio).time as i64 + plus1;
        let m0_line_cycle = m3_len as i64 + base;
        (ly_time - ((456 - m0_line_cycle) << ds)).max(0) as u64
    }

    /// The CPU-visible mode-0 (HBlank) start dot, decoupled from the live pixel
    /// pipeline's actual M3 termination. Derived from the closed-form
    /// `scheduled_mode0_dot` plus a per-phase early-report nudge (<= 0): in
    /// Gambatte the FF41 mode and the mode-0 STAT IRQ are computed from the
    /// predicted mode-3 length and report mode 0 a few dots before the renderer
    /// finishes draining the FIFO. Moving this value earlier is safe because it
    /// drives ONLY the FF41 mode bits and the STAT mode-0 arm, never the
    /// pipeline's own `x==160`/FIFO-drain termination. Returns None when no
    /// closed-form dot is available (window / first line after enable) so the
    /// caller falls back to the live x==160 transition for the report too.
    fn reported_mode0_dot_value(&self, mmio: &mmio::Mmio) -> Option<u128> {
        let sched = self.scheduled_mode0_dot? as i64;
        let nudge = self.reported_mode0_early_nudge(mmio);
        Some((sched + nudge).max(0) as u128)
    }

    /// Per-phase early-report nudge (<= 0 dots) applied to the reported mode-0
    /// dot. The live pipeline (rendering / VRAM-unlock) is untouched; only the
    /// FF41 mode read-back and the mode-0 STAT IRQ arm see this. Bucketed by
    /// SCX&7 / sprite-count / speed / CGB-DMG, env-overridable, default 0 so the
    /// pure decouple is net-zero. Each non-zero default below is a measured,
    /// zero-regression net-positive on the m3stat / m0irq / scx_during_m3
    /// clusters.
    fn reported_mode0_early_nudge(&self, mmio: &mmio::Mmio) -> i64 {
        let _ = mmio;
        0
    }

    /// Arm `sched_m0irq` for the current line from the renderer's predicted
    /// mode-0 start (`scheduled_mode0_dot`, a within-line dot). Converted to the
    /// absolute clock. If no closed-form mode-0 dot is available (window/first
    /// line), fall back to the m0 prediction from the m3 length.
    fn arm_m0irq_for_current_line(&mut self, mmio: &mmio::Mmio) {
        let is_cgb = mmio.is_cgb_features_enabled();
        // The mode-0 STAT IRQ is armed from the CPU-visible reported mode-0 dot
        // (decoupled from the live pipeline termination), so the IRQ fires in
        // lockstep with the FF41 mode-0 read-back.
        let mode0_within_line = match self.reported_mode0_dot_value(mmio) {
            Some(d) => d as i64,
            None => {
                let m3_len = self.compute_m3_length(mmio, is_cgb);
                let offset = if is_cgb { cgb_mode0_offset() } else { dmg_mode0_offset() };
                self.ticks as i64 + m3_len as i64 + offset as i64
            }
        };
        let remaining = mode0_within_line - self.ticks as i64;
        let ds = mmio.is_double_speed_mode();
        let mut off = if ds { m0irq_off_ds() } else { m0irq_off_ss() };
        if is_cgb && !ds && (mmio.read(SCX) & 0x07) == 2 {
            off += env_off("RB_M0IRQ_SCX2_CGB", -1);
        }
        let dsf = 1i64 << ds as i32;
        let abs = (self.abs_cc as i64 - dsf + (remaining + off) * dsf).max(0) as u64;
        self.sched_m0irq = abs;
    }

    /// Re-anchor the event-scheduled STAT/mode/LYC clocks to the new CPU speed.
    /// Mirrors Gambatte's `LCD::speedChange`: the renderer's LCD position
    /// (`line_cycle`/`internal_ly`) is in speed-independent dot units and stays
    /// put, but every scheduled event time carried the old `ds` cc-factor, so
    /// recompute them from the live `abs_cc` under the new speed.
    pub fn speed_change(&mut self, mmio: &mmio::Mmio) {
        if self.disabled || self.lcdc & (LCDCFlags::DisplayEnable as u8) == 0 {
            return;
        }
        self.reschedule_all_stat_events(mmio);
        if self.sched_m0irq != stat_irq::DISABLED_TIME {
            self.arm_m0irq_for_current_line(mmio);
        }
    }

    /// Advance the renderer by `dots` dots during the CGB STOP speed-switch
    /// bridge. Gambatte's `Memory::stop` advances the LCD to `cc + 8` at the OLD
    /// (single) speed before re-anchoring at the new speed (`lcd_.speedChange`).
    /// Our per-dot stepper realizes only `8 >> ds` of those dots through the 8
    /// returned cycles, so this injects the remaining bridge dots so the LCD
    /// lands on the same dot Gambatte does after the 0x20000-cycle window.
    pub fn stop_bridge_advance(&mut self, mmio: &mut mmio::Mmio, dots: u32) {
        for _ in 0..dots {
            self.step_scheduled_stat_events(mmio);
            // The bridge injects render dots the CPU's returned cycles did not
            // cover, so the master cc does not advance for them. `step` derives
            // `abs_cc = master_cc - p_now`; pull `p_now` back by one dot first so
            // the derived clock still advances `1<<ds` this bridge step.
            self.p_now = self.p_now.wrapping_sub(1 << mmio.is_double_speed_mode() as u32);
            self.step(mmio);
            self.step_lcdc_events(mmio);
        }
    }

    /// Mark that a DS->SS speed switch just occurred, so the closed-form lyTime
    /// drops its `+1` LyCounter correction (the whole-dot bridge already lands
    /// the counter one master-cc high). See ENGINE_LAZY_PPU.md bug #2.
    pub fn set_dsss_lytime_adjust(&mut self) {
        self.lytime_no_plus1 = true;
    }

    /// Recompute all scheduled IRQ event times from scratch at the current
    /// `abs_cc` (used on LCD enable / LY-counter reset).
    fn reschedule_all_stat_events(&mut self, mmio: &mmio::Mmio) {
        let lc = self.ly_counter(mmio);
        let cc = self.abs_cc;
        let stat = self.stat_reg_committed;
        self.lyc_irq.reschedule(&lc, cc);
        self.sched_lycirq = self.lyc_irq.time;
        self.sched_m1irq = stat_irq::mode1_irq_schedule(&lc, cc);
        let m2 = stat_irq::mode2_irq_schedule(stat, &lc, cc);
        self.sched_m2irq = if m2 == stat_irq::DISABLED_TIME { m2 } else { (m2 as i64 + Self::m2_off(mmio.is_double_speed_mode())) as u64 };
        // m0irq is scheduled from the renderer's mode-0 prediction; (re)armed
        // when entering pixel transfer. Leave as-is here.
    }

    /// Double-speed sub-dot step. At DS the CPU runs two M-cycles per displayed
    /// pixel-dot; the full `step` runs on the even (render) M-cycle and advances
    /// `abs_cc` by 2. This runs on the intervening odd M-cycle so STAT/LYC IRQ
    /// events scheduled at an *odd* `abs_cc` fire at the true half-dot instead of
    /// being rounded up to the next even render dot. It dispatches events at the
    /// intermediate cc (`abs_cc - 1`, i.e. one M-cycle before the next render
    /// dot's post-increment value) without advancing the renderer's clock.
    pub fn step_subdot(&mut self, mmio: &mut mmio::Mmio) {
        if self.disabled {
            return;
        }
        // The preceding full `step` dispatched at the even cc N and advanced
        // `abs_cc` to N+2 (the next render dot). The odd half-dot is cc N+1, one
        // machine cycle earlier; dispatch any event due there, then restore.
        self.abs_cc -= 1;
        self.dispatch_stat_events(mmio);
        self.abs_cc += 1;
    }

    /// Fire any STAT IRQ events whose scheduled time has arrived at the current
    /// `abs_cc`. Called once per dot from `step`.
    fn dispatch_stat_events(&mut self, mmio: &mut mmio::Mmio) {
        let ds = mmio.is_double_speed_mode();
        let cc = self.abs_cc;

        if self.wy2_apply_cc != wy2_disabled() && self.wy2_apply_cc <= cc {
            self.wy2 = self.wy2_pending;
            self.wy2_apply_cc = wy2_disabled();
        }
        if self.wy1_apply_cc != wy2_disabled() && self.wy1_apply_cc <= cc {
            self.wy1 = self.wy1_pending;
            self.wy1_apply_cc = wy2_disabled();
        }
        if self.scy_apply_cc != wy2_disabled() && self.scy_apply_cc <= cc {
            self.scy_delayed = self.scy_pending;
            self.scy_apply_cc = wy2_disabled();
        }
        if self.scx_apply_cc != wy2_disabled() && self.scx_apply_cc <= cc {
            self.scx_delayed = self.scx_pending;
            self.scx_apply_cc = wy2_disabled();
        }

        if self.sched_oneshot_statirq <= cc {
            mmio.request_interrupt(registers::InterruptFlag::Lcd);
            self.sched_oneshot_statirq = stat_irq::DISABLED_TIME;
        }
        // Order matches Gambatte's nextMemEvent priority for ties.
        if self.sched_m1irq <= cc {
            let stat = self.stat_reg_committed;
            if self.mstat_irq.do_m1_event(stat) {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            self.sched_m1irq = self.sched_m1irq
                .wrapping_add((stat_irq::LCD_CYCLES_PER_FRAME) << ds as u32);
        }
        if self.sched_lycirq <= cc + ds as u64 {
            let lc = self.ly_counter(mmio);
            if self.lyc_irq.do_event(&lc) {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            self.sched_lycirq = self.lyc_irq.time;
        }
        if self.sched_m2irq <= cc {
            self.do_mode2_irq_event(mmio, ds);
        }
        if self.sched_m0irq <= cc {
            let stat = self.stat_reg_committed;
            let ly = self.internal_ly() as u32;
            if self.mstat_irq.do_m0_event(ly, stat, self.lyc_irq.lyc_reg()) {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            // m0irq re-arm happens at next pixel-transfer entry.
            self.sched_m0irq = stat_irq::DISABLED_TIME;
        }
    }

    fn m2_off(ds: bool) -> i64 {
        if ds { m2irq_off_ds() } else { m2irq_off_ss() }
    }

    fn do_mode2_irq_event(&mut self, mmio: &mut mmio::Mmio, ds: bool) {
        // doMode2IrqEvent: the LY used is the *next* line's LY if the m2 event
        // is within 16 cycles of the ly increment.
        let lc = self.ly_counter(mmio);
        let near_ly_inc = lc.time.saturating_sub(self.sched_m2irq) < 16;
        let ly = if near_ly_inc {
            if lc.ly == stat_irq::LCD_LINES_PER_FRAME - 1 { 0 } else { lc.ly + 1 }
        } else {
            lc.ly
        };
        let stat = self.stat_reg_committed;
        let fired = self.mstat_irq.do_m2_event(ly, stat, self.lyc_irq.lyc_reg());
        if fired {
            mmio.request_interrupt(registers::InterruptFlag::Lcd);
        }
        let delta = stat_irq::mode2_reschedule_delta(ly, stat, ds);
        self.sched_m2irq = self.sched_m2irq.wrapping_add(delta);
    }

    /// Calculate the current state of the STAT interrupt line based on all interrupt sources
    fn calculate_stat_interrupt_line(&self, mmio: &mmio::Mmio) -> bool {
        let stat_register = mmio.read(LCD_STATUS);
        
        // Extract enable bits for each interrupt source
        let mode_0_int_enable = (stat_register & (1 << 3)) != 0; // HBlank
        let mode_1_int_enable = (stat_register & (1 << 4)) != 0; // VBlank
        let mode_2_int_enable = (stat_register & (1 << 5)) != 0; // OAM Search
        let lyc_int_enable = (stat_register & (1 << 6)) != 0;    // LYC=LY
        
        // Extract current state flags
        let current_mode = stat_register & 0x03; // Bits 1-0: PPU mode
        let lyc_equals_ly = (stat_register & (1 << 2)) != 0;     // Bit 2: LYC=LY flag
        
        // Check each interrupt source and OR them together
        let mut interrupt_line = false;
        
        // Mode interrupts
        match current_mode {
            0 if mode_0_int_enable => interrupt_line = true, // HBlank
            1 if mode_1_int_enable => interrupt_line = true, // VBlank
            2 if mode_2_int_enable => interrupt_line = true, // OAM Search
            _ => {}
        }
        
        // LYC=LY interrupt
        if lyc_int_enable && lyc_equals_ly {
            interrupt_line = true;
        }
        
        interrupt_line
    }


    // Cycle-exact Mode 3 length (dots from M3 start to xpos=167), ported from
    // Gambatte's predictCyclesUntilXpos_fn / addSpriteCycles. Sprites must be
    // pre-sorted by raw OAM X ascending. Returns dots to add past the 167 base.
    // Whether the window starts drawing on this line (Gambatte's win-draw-start
    // gate). DMG ignores WX==166.
    // Gambatte weMaster latch (M2_Ly0::f0 + M2_LyNon0::f0/f1). Sets the sticky
    // `window_y_triggered` flag at the same three line-cycle checkpoints
    // Gambatte uses, reading WY live so late writes are caught precisely.
    fn update_window_y_latch(&mut self, mmio: &mmio::Mmio) {
        if self.disabled {
            return;
        }
        let is_cgb = mmio.is_cgb_features_enabled();
        let win_en = (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
        if !win_en {
            return;
        }
        let ly = mmio.read(LY) as i32;
        // Gambatte's weMaster checkpoints read `p.wy`, the WY value applied
        // `1 + cgb` cc after the write (not the live mmio value). Using the
        // delayed `wy1` makes a mid-frame WY write reach these checkpoints with
        // Gambatte's exact phase.
        let wy = self.wy1 as i32;

        // ly0 check (only valid during the active frame's line 0 mode-2).
        // weMasterCheckLy0LineCycle = 1 + cgb. Also runs on the first line
        // after enable (where ly is held at 0 and there is no mode-2 phase).
        if ly == 0
            && self.state == State::OAMSearch
            && self.ticks == (1 + is_cgb as u128)
        {
            if wy == 0 {
                self.window_y_triggered = true;
            }
            return;
        }

        // The remaining checks ride the previous line's HBlank; on the first
        // line after enable there is no such prior line.
        if self.first_line_after_enable {
            return;
        }

        // Prior-to-LY-inc check at line cycle 450: weMaster |= (ly == wy).
        if self.ticks == 450 {
            if ly == wy {
                self.window_y_triggered = true;
            }
            return;
        }
        // After-LY-inc check at line cycle 454: weMaster |= (ly + 1 == wy).
        if self.ticks == 454 && ly + 1 == wy {
            self.window_y_triggered = true;
        }
    }

    // Pop one pixel from the BG/window FIFO, mix sprites, write it to the
    // framebuffer at the current x and advance x. Returns true if a pixel was
    // drawn (FIFO non-empty).
    fn draw_fifo_pixel(&mut self, mmio: &mmio::Mmio) -> bool {
        let Ok(bg_pixel) = self.fetcher.pixel_fifo.pop() else {
            return false;
        };
        let bg_pixel_idx = bg_pixel.color;
        let bg_attrs = bg_pixel.attrs;
        let ly = mmio.read(LY) as u16;
        let fb_offset = (ly * 160) + self.x as u16;

        if mmio.is_cgb_features_enabled() {
            let final_color_rgb =
                self.mix_background_and_sprites_color(mmio, bg_pixel_idx, bg_attrs, self.x, ly as u8);
            self.record_pixel_debug_event(
                ly as u8,
                bg_pixel_idx,
                [final_color_rgb.0, final_color_rgb.1, final_color_rgb.2],
            );
            let color_offset = fb_offset as usize * 3;
            self.color_fb_a[color_offset] = final_color_rgb.0;
            self.color_fb_a[color_offset + 1] = final_color_rgb.1;
            self.color_fb_a[color_offset + 2] = final_color_rgb.2;
        } else {
            let final_color = self.mix_background_and_sprites(mmio, bg_pixel_idx, self.x, ly as u8);
            let intensity = match final_color {
                0 => 255,
                1 => 170,
                2 => 85,
                _ => 0,
            };
            self.record_pixel_debug_event(ly as u8, bg_pixel_idx, [intensity, intensity, intensity]);
            self.fb_a[fb_offset as usize] = final_color;
        }
        self.x += 1;
        true
    }

    // Gambatte's plotPixel/predictor window-Y gate: `weMaster || (wy2 == ly &&
    // winEn)`. `wy2` is WY delayed ~2 dots after a write; we read WY live, which
    // matches by the time the fetcher reaches WX. This `wy2 == ly` fallback
    // catches late-frame WY writes that land after the three weMaster
    // checkpoints (e.g. WY=ly written during the same line's mode 3).
    fn window_y_active(&self, mmio: &mmio::Mmio) -> bool {
        if (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) == 0 {
            return false;
        }
        if self.window_y_triggered {
            return true;
        }
        self.wy2 == mmio.read(LY)
    }

    fn window_will_start(&self, mmio: &mmio::Mmio, is_cgb: bool) -> bool {
        if !self.window_y_active(mmio) {
            return false;
        }
        let wx = mmio.read(WX) as i32;
        (0..=166).contains(&wx) && (is_cgb || wx != 166)
    }

    fn compute_m3_length(&self, mmio: &mmio::Mmio, is_cgb: bool) -> u128 {
        let (len, _win) = self.compute_m3_length_win(mmio, is_cgb);
        len
    }

    // Returns (mode-3 length in dots past base, whether the window contributed).
    fn compute_m3_length_win(&self, mmio: &mmio::Mmio, is_cgb: bool) -> (u128, bool) {
        let scx = (mmio.read(SCX) & 0x07) as i32;
        // Fine-scroll discard prefix: M3Start::f1 consumes scx%8 dots, then
        // nextCall(1-cgb) before the tile loop (167-base) begins.
        let mut cycles: i32 = scx + (1 - is_cgb as i32);
        cycles += 167; // targetx - xpos, xpos=0 at tile-loop start

        // Window: if it will start on this line in range. Gambatte sets
        // `nwx = wx` and adds 6; sprites then split into a `spx <= nwx` group
        // (firstTileXpos = endx%8) and a `spx > nwx` group (firstTileXpos =
        // nwx+1, prevTileNo reset). nwx stays 0xFF when no window starts.
        let mut nwx: i32 = 0xFF;
        let mut win = false;
        if self.window_will_start(mmio, is_cgb) {
            nwx = mmio.read(WX) as i32;
            cycles += WIN_M3_PENALTY + env_off("RB_WIN_M3_PEN", 0) as i32;
            // CGB window lines at SCX%8 == 5: the closed-form mode-3 window
            // penalty runs one dot long versus Gambatte's M3Start fine-scroll
            // dispatch at this phase, flipping the sampled STAT mode on the
            // m2int_*_scx5 window probes. Scoped to this SCX phase / speed-
            // agnostic; other phases (scx2 DMG, scx7) regress and stay put.
            if is_cgb && scx == 5 && self.sprites_on_line.is_empty() {
                cycles += env_off("RB_WIN_M3_SCX5_CGB", -1) as i32;
            }
            win = true;
        }

        // Sprites. Only count if OBJ enabled (or CGB always evaluates them).
        let obj_enabled = (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0;
        if obj_enabled || is_cgb {
            let first_tile_xpos = (8 - scx) % 8; // = endx % 8
            let target_x = 167;
            let mut sprite_xs: Vec<i32> = self.sprites_on_line.iter().map(|s| s.x as i32).collect();
            sprite_xs.sort_unstable();
            let mut idx = 0usize;

            // addSpriteCycles helper: accumulates for sprites with spx <= max_spx.
            let add_sprite_cycles = |xs: &[i32], idx: &mut usize, max_spx: i32,
                                     first_tile_xpos: i32, mut prev_tile_no: i32,
                                     cycles: &mut i32| {
                while *idx < xs.len() && xs[*idx] <= max_spx {
                    let spx = xs[*idx];
                    let dist = (spx - first_tile_xpos).rem_euclid(8);
                    let tile_no = (spx - first_tile_xpos) & !7;
                    let mut c = 6;
                    if dist < 5 && tile_no != prev_tile_no {
                        c = 11 - dist;
                    }
                    prev_tile_no = tile_no;
                    *cycles += c;
                    *idx += 1;
                }
            };

            if idx < sprite_xs.len() {
                // First-sprite special case (Tile::predictCyclesUntilXpos_fn, the
                // `fno + spx - xpos` first-sprite branch). The `fno` Gambatte
                // passes from M3Start::f1 is the fine-scroll discard
                // `min(scx % 8, 5)`, NOT a constant 1; xpos is 0 here.
                let spx0 = sprite_xs[0];
                let fno = scx.min(5);
                let prev_tile_no = (0 - first_tile_xpos) & !7; // (xpos - firstTileXpos) & -8
                if fno + spx0 < 5 && spx0 <= nwx && spx0 <= target_x {
                    cycles += 11 - (fno + spx0);
                    idx += 1;
                }
                if nwx < target_x {
                    add_sprite_cycles(&sprite_xs, &mut idx, nwx, first_tile_xpos, prev_tile_no, &mut cycles);
                    add_sprite_cycles(&sprite_xs, &mut idx, target_x, nwx + 1, 1, &mut cycles);
                } else {
                    add_sprite_cycles(&sprite_xs, &mut idx, target_x, first_tile_xpos, prev_tile_no, &mut cycles);
                }
            }
        }

        (cycles.max(0) as u128, win)
    }

    fn set_lcd_status_mode(mmio: &mut mmio::Mmio, mode: u8) {
        mmio.write_lcd_status_from_ppu((mmio.read(LCD_STATUS) & !0x03) | (mode & 0x03));
    }

    fn reset_lcd_pipeline(&mut self) {
        self.fetcher.reset();
        self.ticks = 0;
        self.x = 0;
        self.sprites_on_line.clear();
        self.current_oam_sprite_index = 0;
        self.next_sprite_fetch_index = 0;
        self.sprite_fetch_stall = 0;
        self.pixel_transfer_warmup = 0;
        self.window_line_counter = 0;
        self.win_y_pos = 0xFF;
        self.win_draw_start = false;
        self.window_y_triggered = false;
        self.window_started_this_line = false;
        self.mode2_irq_pretriggered_for_next_line = false;
        self.first_line_after_enable = false;
        self.line_153_ly_zeroed = false;
        self.mode0_pretriggered_this_line = false;
        self.m3_pixels_discarded = 0;
        self.scheduled_mode0_dot = None;
        self.m0_time_master = None;
        self.cgbp_block_start_cc = None;
    }

    /// Latch the current wired-OR STAT line state for edge bookkeeping. IRQ
    /// delivery is now handled exclusively by the event-scheduled model
    /// (`dispatch_stat_events` + the FF41/FF45 write hooks), so this no longer
    /// fires interrupts.
    fn check_and_trigger_stat_interrupt(&mut self, mmio: &mut mmio::Mmio) {
        self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
    }

    /// Re-evaluate the LYC=LY flag and the STAT edge after a CPU write to
    /// FF40 (LCDC), FF41 (STAT), or FF45 (LYC). Called by the host between
    /// CPU instructions when `Mmio::take_stat_register_write_pending`
    /// returns true. The mid-instruction write itself becomes visible to the
    /// PPU on its next dot step; this hook closes the gap where enabling a
    /// STAT source whose underlying condition is already true must produce
    /// an immediate rising edge.
    /// Record the sub-PPU-dot parity of the CPU write about to be resolved, so
    /// the STAT/LYC change hooks can place the event on the correct half-dot at
    /// double speed. `phase` is the persistent CPU T-phase at write resolution.
    pub fn set_write_subdot(&mut self, phase: u64) {
        self.write_subdot = (phase % 2) as u8;
    }

    /// FF4A (WY) write hook. Gambatte applies the write to `wy2` (the value the
    /// window-Y gate reads) delayed by `6 - isDoubleSpeed()` cc after the write.
    /// Schedule that delayed apply against the resolving write's absolute clock.
    pub fn on_wy_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        if self.disabled {
            self.wy2 = value;
            self.wy2_apply_cc = wy2_disabled();
            self.wy1 = value;
            self.wy1_apply_cc = wy2_disabled();
            return;
        }
        let ds = mmio.is_double_speed_mode();
        let cc = self.write_cc(ds);
        // Gambatte `wyChange`: `update(cc + 1 + cgb)` applies `p.wy` (the value
        // the weMaster checkpoints read) at cc + 1 + cgb. Schedule that delayed
        // apply so a mid-frame WY write reaches the weMaster latch with the same
        // phase Gambatte uses, rather than the live (immediate) mmio value.
        let cgb = mmio.is_cgb_features_enabled() as i64;
        let wy1_delay = env_off("RB_WY1_DELAY", 2) + cgb;
        self.wy1_pending = value;
        self.wy1_apply_cc = cc + wy1_delay.max(0) as u64;
        // wy2 apply delay (cc) past the write, swept against the late_wy suite:
        // CGB 7, DMG 4 (-ds at double speed). The split reflects the differing
        // M3-start / fine-scroll phase between the two cores.
        let base = if mmio.is_cgb_features_enabled() {
            env_off("RB_WY2_DELAY_CGB", 7)
        } else {
            env_off("RB_WY2_DELAY_DMG", 4)
        };
        let delay = (base - ds as i64).max(0) as u64;
        self.wy2_pending = value;
        self.wy2_apply_cc = cc + delay;
    }

    /// FF42 (SCY) write hook. The CPU readback of FF42 is immediate (handled by
    /// mmio), but the BG fetcher must see the new SCY only ~N dots later, the
    /// write-side analog of the wy1/wy2 delayed latches: rustyboi otherwise
    /// resolves the write pre-tick and the fetcher re-reads it live one M-cycle
    /// too early vs Gambatte. Schedule the delayed apply against the write cc.
    pub fn on_scy_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        if self.disabled {
            self.scy_delayed = value;
            self.scy_apply_cc = wy2_disabled();
            return;
        }
        let ds = mmio.is_double_speed_mode();
        let cc = self.write_cc(ds);
        // CGB-only: rustyboi's DMG fetcher already samples SCY at the
        // Gambatte-correct dot (delay 0); only the CGB core sees the mid-M3 write
        // one M-cycle too early (the `_2/_4/_6` straddle pairs vs the passing
        // `_1/_3/_5`). A DMG delay regresses the DMG scy_during_m3 cases.
        // SCY=2 is the swept optimum (fixes 20 CGB scy_during_m3 straddle cases,
        // zero regression; 1 -> -4, 3 -> -14, 4 -> +8 regresses).
        let delay = if mmio.is_cgb_features_enabled() {
            env_off("RB_SCY_DELAY", 2).max(0) as u64
        } else {
            0
        };
        self.scy_pending = value;
        self.scy_apply_cc = cc + delay;
    }

    /// FF43 (SCX) write hook. See `on_scy_write`.
    pub fn on_scx_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        if self.disabled {
            self.scx_delayed = value;
            self.scx_apply_cc = wy2_disabled();
            return;
        }
        let ds = mmio.is_double_speed_mode();
        let cc = self.write_cc(ds);
        // SCX has no positive lever in the sweep (delay 1/2 == net-zero vs the
        // live read); the SCX-write straddles need the read-cc convergent root,
        // out of scope. Default 0 (live), env-overridable for future work.
        let delay = if mmio.is_cgb_features_enabled() {
            env_off("RB_SCX_DELAY", 0).max(0) as u64
        } else {
            0
        };
        self.scx_pending = value;
        self.scx_apply_cc = cc + delay;
    }

    pub fn on_stat_register_write(&mut self, mmio: &mut mmio::Mmio) {
        // The DMG STAT-write bug fires on any FF41 write, even one that leaves
        // the enable bits unchanged. Track whether this was an FF41 write so the
        // unchanged-value case still runs lcdstat_change below.
        let ff41_written = mmio.take_ff41_write_pending();
        // Keep the LYC=LY readback flag (FF41 bit 2) in sync regardless of LCD
        // state; only its IRQ side-effects are gated by enable.
        if self.disabled {
            self.previous_stat_interrupt_line = false;
            // STAT-write quirk (memory.cpp case 0x41): with the LCD off, an FF41
            // write while the LYC=LY flag is set and LYC IRQ was disabled flags
            // a STAT IRQ. On CGB the written data must also set LYC-IRQ-enable;
            // on DMG it fires regardless of the written value.
            let live_stat = mmio.read(LCD_STATUS);
            let new_stat = live_stat & 0x78;
            let old_stat = self.stat_reg_committed & 0x78;
            let lycflag = live_stat & 0x04 != 0;
            let old_lycen = old_stat & stat_irq::STAT_LYCEN != 0;
            let new_lycen = new_stat & stat_irq::STAT_LYCEN != 0;
            let cgb = mmio.is_cgb_features_enabled();
            let data_ok = if cgb { new_lycen } else { true };
            if ff41_written && lycflag && !old_lycen && data_ok {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            // Keep the IRQ sources' shadow registers current so a later enable
            // sees the right values (Gambatte calls lcdstat/lycRegChange even
            // while off, just skipping event scheduling).
            self.stat_reg_committed = new_stat;
            return;
        }

        let new_stat = mmio.read(LCD_STATUS) & 0x78;
        let new_lyc = mmio.read(LYC);
        let old_stat = self.stat_reg_committed & 0x78;
        let old_lyc = self.lyc_irq.lyc_reg();

        // FF41 (STAT) write. Run unconditionally on any FF41 write (even a
        // same-value write) to reproduce the DMG STAT-write IRQ bug; the CGB
        // trigger path self-guards on newly-set bits, so this is a no-op there.
        if ff41_written || new_stat != old_stat {
            self.lcdstat_change(new_stat, mmio);
        }
        // FF45 (LYC) write.
        if new_lyc != old_lyc {
            self.lyc_reg_change(new_lyc, mmio);
        }

        // Re-sync the LYC=LY readback flag after the change.
        self.sync_lyc_flag(mmio);
        self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
    }

    fn sync_lyc_flag(&self, mmio: &mut mmio::Mmio) {
        let effective_ly = self.effective_ly_for_lyc_compare(mmio);
        if mmio.read(LYC) == effective_ly {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2));
        } else {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2));
        }
    }

    /// The m0 IRQ time to use in the stat-change immediate-trigger check.
    /// Mirrors Gambatte: when the scheduled m0 IRQ is disabled but the current
    /// line's mode 0 is still ahead, predict it from the renderer; otherwise use
    /// the scheduled value.
    fn m0_irq_time_for_trigger(&self, _mmio: &mmio::Mmio, lc: &stat_irq::LyCounter, _cc: u64) -> u64 {
        // Gambatte's statChangeTriggers* needs the m0 IRQ time of the *current
        // line*. Our `sched_m0irq` may hold a stale current-line value during
        // HBlank (it is only cleared to DISABLED when the m0 source fires). The
        // DMG/CGB branch logic only cares whether m0IrqTime is before or after
        // `lyCounter.time()` (next-LY): if mode 0 is already active (HBlank) the
        // current line's m0 has passed and the next is on a later line, i.e.
        // `>= lc.time`; during mode 2/3 it is still ahead this line (`< time`).
        match self.state {
            // Mode 0 active: report a time at/after the next LY so the "m0 has
            // occurred" branch is taken.
            State::HBlank => lc.time,
            // VBlank: no m0 this line; far future.
            State::VBlank => stat_irq::DISABLED_TIME,
            // Mode 2/3: current line's m0 is ahead but before next LY.
            _ => {
                if self.sched_m0irq == stat_irq::DISABLED_TIME {
                    lc.time.saturating_sub(1)
                } else {
                    self.sched_m0irq.min(lc.time.saturating_sub(1))
                }
            }
        }
    }

    /// Port of LCD::lcdstatChange. `data` is the new FF41 enable bits (& 0x78).
    fn lcdstat_change(&mut self, data: u8, mmio: &mut mmio::Mmio) {
        let cc = self.write_cc(mmio.is_double_speed_mode());
        let lc = self.ly_counter(mmio);
        let old = self.stat_reg_committed & 0x78;
        self.stat_reg_committed = data;
        self.lyc_irq.stat_reg_change(data, &lc, cc);

        // If m0 IRQ just got enabled and isn't scheduled, arm it from the
        // current line's mode-0 prediction.
        if (data & stat_irq::STAT_M0EN != 0) && self.sched_m0irq == stat_irq::DISABLED_TIME {
            self.arm_m0irq_for_current_line(mmio);
        }
        let m2 = stat_irq::mode2_irq_schedule(data, &lc, cc);
        self.sched_m2irq = if m2 == stat_irq::DISABLED_TIME { m2 } else { (m2 as i64 + Self::m2_off(mmio.is_double_speed_mode())) as u64 };
        self.sched_lycirq = self.lyc_irq.time;

        let cgb = mmio.is_cgb_features_enabled();
        let lyc_reg = self.lyc_irq.lyc_reg();
        // Gambatte's statChangeTriggersStatIrqDmg recomputes the current line's
        // m0 IRQ time when it is unscheduled but mode 0 is still ahead this
        // line. Reproduce that so enabling m0 during mode 2/3 sees a future m0.
        let m0_for_trigger = self.m0_irq_time_for_trigger(mmio, &lc, cc);
        let triggers = if cgb {
            stat_irq::stat_change_triggers_cgb(old, data, &lc, cc, m0_for_trigger, lyc_reg)
        } else {
            stat_irq::stat_change_triggers_dmg(old, &lc, cc, m0_for_trigger, lyc_reg)
        };
        if triggers {
            mmio.request_interrupt(registers::InterruptFlag::Lcd);
        }

        self.mstat_irq.stat_reg_change(
            data,
            self.sched_m0irq,
            self.sched_m1irq,
            self.sched_m2irq,
            cc,
            cgb,
        );
    }

    /// Port of LCD::lycRegChange.
    fn lyc_reg_change(&mut self, data: u8, mmio: &mut mmio::Mmio) {
        let old = self.lyc_irq.lyc_reg();
        if data == old {
            return;
        }
        let cc = self.write_cc(mmio.is_double_speed_mode());
        let lc = self.ly_counter(mmio);
        let stat = self.stat_reg_committed;
        let cgb = mmio.is_cgb_features_enabled();
        let ds = mmio.is_double_speed_mode();

        self.lyc_irq.lyc_reg_change(data, &lc, cc);
        self.mstat_irq
            .lyc_reg_change(data, self.sched_m0irq, self.sched_m2irq, cc, ds, cgb);
        self.sched_lycirq = self.lyc_irq.time;

        if stat_irq::lyc_change_triggers_stat_irq(old, data, &lc, cc, stat, self.sched_m0irq, cgb) {
            if cgb && !ds {
                self.sched_oneshot_statirq = cc + 5;
            } else {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
        }
    }

    /// The absolute clock value attributed to a register write. The write hook
    /// fires after the FF4x store but before this M-cycle's 4 dots tick, so the
    /// renderer's current dot is `abs_cc - 1`.
    ///
    /// At double speed `abs_cc` advances by 2 per PPU step and the PPU only
    /// steps on even CPU T-phases, so `abs_cc` alone can only place a write on
    /// an even half-dot. `write_subdot` carries the true sub-dot parity of the
    /// resolving CPU write (0 on an even T-phase, 1 on an odd one), giving the
    /// STAT model half-PPU-dot precision.
    fn write_cc(&self, ds: bool) -> u64 {
        let off = if ds { write_cc_off_ds() } else { write_cc_off_ss() };
        // `write_subdot` carries the sub-PPU-dot parity of the resolving CPU
        // write. In practice the STAT/render tests align via whole-instruction
        // polling loops, so writes land on M-cycle (even) phases and this term
        // is 0; it remains wired for the rare odd-phase write (post-HALT-1cc).
        let sub = if ds { self.write_subdot as i64 } else { 0 };
        (self.abs_cc as i64 + off + sub).max(0) as u64
    }

    /// LY value used for the LYC=LY comparison. In Gambatte the compare uses
    /// the next line's LY in the last 2 dots of the current line
    /// (`getLycCmpLy` `timeToNextLy <= 2`), so the LYC=LY flag rises one line
    /// early. Line 153's mid-line ly=0 transient is handled separately in
    /// Phase D by writing FF44 directly, so this only anticipates lines
    /// 0..=152 (line 153 -> 0 already came through `write_ly_from_ppu`).
    fn effective_ly_for_lyc_compare(&self, mmio: &mmio::Mmio) -> u8 {
        let ly = mmio.read(LY);
        if self.ticks < 454 {
            return ly;
        }
        match self.state {
            State::HBlank if ly < 143 => ly + 1,
            State::HBlank if ly == 143 => 144,
            State::VBlank if (144..152).contains(&ly) => ly + 1,
            // Line 152 -> 153 transition: still anticipate (next line is 153).
            State::VBlank if ly == 152 => 153,
            _ => ly,
        }
    }

    fn enter_scheduled_mode2(&mut self, mmio: &mut mmio::Mmio) {
        // Seed the per-line OBJ-size scan latch from the LCDC as of the mode-2
        // entry boundary. A size write in the prior line's HBlank/VBlank is
        // captured here (affects this line); a write after this boundary (this
        // line's mode2) is applied per-slot after the scan, so sprite-0 keeps
        // the pre-boundary size. This is the late_sizechange 1-cc M2-boundary
        // discriminator (Gambatte SpriteMapper lsbuf per-entry latch).
        self.scan_obj_size_large = (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;
        Self::set_lcd_status_mode(mmio, 2);
        // IRQ delivery is handled by the event model; just latch the line.
        self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
        self.mode2_irq_pretriggered_for_next_line = false;
        // Arm the cgbp begin boundary (Gambatte cgbpAccessible: blocked once
        // `lineCycles(cc) + ds >= 80`) as soon as the line's mode 2 begins, so a
        // BCPD/OCPD write landing in late mode 2 (before M3 is armed) sees it.
        // lineCycles = ticks - (4 - cgb); the block begins at the renderer tick
        // `tick_block = 80 - ds + (4 - cgb)`, converted to master cc from the
        // current tick (each dot = 1<<ds cc). Refined (kept) at M3 arm.
        let ds = mmio.is_double_speed_mode() as i64;
        let cgb_i = mmio.is_cgb_features_enabled() as i64;
        let tick_block = 80 - ds + (4 - cgb_i);
        self.cgbp_block_start_cc = Some(
            (mmio.master_cc() as i64 + ((tick_block - self.ticks as i64) << ds)).max(0) as u64,
        );
    }

    pub fn step_scheduled_stat_events(&mut self, mmio: &mut mmio::Mmio) {
        if self.disabled {
            return;
        }

        // FF41 mode-bit read-back anticipation: in the last 3 dots of an
        // HBlank line (or of line 153) FF41 reports mode 2 (the next line's
        // mode). Match Gambatte's `getStat` `lineCycles >= 453` threshold by
        // writing the anticipated mode at dot 453 and re-syncing the STAT
        // edge latch so the bit change does not produce a duplicate IRQ
        // rising edge — the actual mode-2 IRQ has already been delivered by
        // the pretrigger above when its conditions were met.
        let mode2_anticipate_dot = MODE2_STAT_PRETRIGGER_DOT + 1; // 453
        let should_anticipate_mode2 = match self.state {
            State::HBlank => self.ticks == mode2_anticipate_dot && mmio.read(LY) < 143,
            State::VBlank => self.ticks == mode2_anticipate_dot
                && (mmio.read(LY) == 153 || self.line_153_ly_zeroed),
            _ => false,
        };
        if should_anticipate_mode2 && (mmio.read(LCD_STATUS) & 0x03) != 2 {
            Self::set_lcd_status_mode(mmio, 2);
            self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
        }
    }

    pub fn step(&mut self, mmio: &mut mmio::Mmio) {
        if self.disabled {
            // While the LCD is off the LY counter is held at 0; consume any
            // pending CPU write so it doesn't affect the next enable.
            let _ = mmio.take_ly_write_pending();
            if mmio.read(LCD_CONTROL)&(LCDCFlags::DisplayEnable as u8) != 0 {
                self.sync_lcdc_from_mmio(mmio);
                self.disabled = false;
                mmio.write_ly_from_ppu(0);
                self.reset_lcd_pipeline();
                self.state = State::OAMSearch;
                // First line after enable: STAT reports mode 0 (not 2), no
                // Mode 2 STAT IRQ fires, and M3 starts later than usual.
                self.first_line_after_enable = true;
                // Carried-edge LYC=0 IRQ on enable (memory.cpp case 0x40): when
                // the LYC IRQ source is enabled, LYC==0 and the pre-enable STAT
                // did NOT already hold the LYC=LY coincidence flag, enabling the
                // LCD flags a STAT IRQ immediately. The pre-enable lycflag is
                // bit 2 of the stored FF41 (untouched by the mode write below).
                let pre_enable_stat = mmio.read(LCD_STATUS);
                if pre_enable_stat & (1 << 6) != 0
                    && mmio.read(LYC) == 0
                    && pre_enable_stat & (1 << 2) == 0
                {
                    mmio.request_interrupt(registers::InterruptFlag::Lcd);
                }
                Self::set_lcd_status_mode(mmio, 0);
                self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
                self.check_and_trigger_stat_interrupt(mmio);
                // Initialize the event-scheduled IRQ clock at enable: LY=0,
                // line_cycle=0. Mirror Gambatte's lcdcChange enable branch.
                self.line_cycle = 0;
                self.internal_ly_val = 0;
                // Anchor the PPU dot-clock onto the master cc at LCD enable
                // (Gambatte seeds `p_.now()` here). `abs_cc` keeps its accumulated
                // value across an off/on cycle. The derive at the end of THIS step
                // must reproduce the old post-increment value (pre + 1<<ds), so the
                // anchor subtracts that one dot the old accumulator added below.
                let ds_inc = 1u64 << mmio.is_double_speed_mode() as u32;
                self.p_now = mmio.master_cc().wrapping_sub(self.abs_cc + ds_inc);
                self.lytime_no_plus1 = false;
                self.wy2 = mmio.read(WY);
                self.wy2_apply_cc = wy2_disabled();
                self.wy1 = mmio.read(WY);
                self.wy1_apply_cc = wy2_disabled();
                self.scy_delayed = mmio.read(SCY);
                self.scy_apply_cc = wy2_disabled();
                self.scx_delayed = mmio.read(SCX);
                self.scx_apply_cc = wy2_disabled();
                self.stat_reg_committed = mmio.read(LCD_STATUS);
                self.lyc_irq.set_cgb(mmio.is_cgb_features_enabled());
                self.lyc_irq.seed(mmio.read(LCD_STATUS), mmio.read(LYC));
                self.mstat_irq.seed(mmio.read(LCD_STATUS), mmio.read(LYC));
                self.lyc_irq.lcd_reset();
                self.mstat_irq.lcd_reset(self.lyc_irq.lyc_reg());
                self.reschedule_all_stat_events(mmio);
                self.sched_m0irq = stat_irq::DISABLED_TIME;
                self.sched_oneshot_statirq = stat_irq::DISABLED_TIME;
            } else {
                return;
            }
        } else if self.lcdc&(LCDCFlags::DisplayEnable as u8) == 0 {
            mmio.write_ly_from_ppu(0);
            self.reset_lcd_pipeline();
            Self::set_lcd_status_mode(mmio, 0);
            self.disabled = true;
            self.previous_stat_interrupt_line = false;
            // The LCD just turned off; drop any pending LY write.
            let _ = mmio.take_ly_write_pending();
            return;
        }

        // Fire any scheduled STAT IRQ events that have come due at this dot,
        // then advance the clean event clock by one dot (phase-locked with the
        // renderer's 456-dot line).
        self.dispatch_stat_events(mmio);
        // Fold the PPU dot-clock onto the master cc. `p_now` is the LCD-enable
        // anchor such that the PPU machine-cycle clock is `master_cc - p_now`
        // (Gambatte `p_.now()`); the master cc advances `1<<ds` per render dot
        // within a speed epoch, so the derived clock advances exactly as the old
        // accumulator did. `p_now` is seeded at enable and re-based on the speed
        // change / STOP bridge (where the master cc and render-dot counts diverge).
        self.abs_cc = mmio.master_cc().wrapping_sub(self.p_now);
        self.line_cycle += 1;
        if self.line_cycle >= stat_irq::LCD_CYCLES_PER_LINE {
            self.line_cycle = 0;
            self.internal_ly_val += 1;
            if self.internal_ly_val as u32 >= stat_irq::LCD_LINES_PER_FRAME {
                self.internal_ly_val = 0;
            }
        }

        // CPU writes to FF44 (LY) reset the line counter to 0 and re-arm the
        // PPU at the start of an OAM search.
        if mmio.take_ly_write_pending() {
            self.reset_lcd_pipeline();
            mmio.write_ly_from_ppu(0);
            self.state = State::OAMSearch;
            self.enter_scheduled_mode2(mmio);
            self.line_cycle = 0;
            self.internal_ly_val = 0;
            self.stat_reg_committed = mmio.read(LCD_STATUS);
            self.lyc_irq.lcd_reset();
            self.mstat_irq.lcd_reset(self.lyc_irq.lyc_reg());
            self.reschedule_all_stat_events(mmio);
            self.sched_m0irq = stat_irq::DISABLED_TIME;
        }

        // LYC=LY compare uses an "effective LY" that anticipates the
        // next-line value in the last 2 dots of any line (matches Gambatte's
        // `getLycCmpLy` `timeToNextLy <= 2` threshold). Line 153's earlier
        // ly=0 transient is handled separately in Phase D by writing FF44
        // directly, so this anticipation only fires on lines 0..=152.
        let effective_ly = self.effective_ly_for_lyc_compare(mmio);
        if mmio.read(LYC) == effective_ly {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2)); // Set the LYC=LY flag
        } else {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2)); // Clear the LYC=LY flag
        }
        
        // Check for STAT interrupt after LYC=LY update
        self.check_and_trigger_stat_interrupt(mmio);

        // Gambatte-style window-Y (weMaster) latch. The trigger is sticky for
        // the frame and is evaluated at three points: ly0 mode-2 start
        // (wy==0), and near each line's end at the prior-to-LY-inc (ly==wy)
        // and after-LY-inc (ly+1==wy) cycles. This catches late WY writes that
        // land in the small window between these checks.
        self.update_window_y_latch(mmio);

        match self.state {
            State::OAMSearch => {
                // Window line-counter bookkeeping at the start of Mode 2. The WY
                // trigger latch (`window_y_triggered`/weMaster) is handled by the
                // Gambatte-style three-point check in `update_window_y_latch`,
                // which runs near the previous line's end.
                if self.ticks == 0 {
                    // winYPos is now incremented at window draw-start (see the
                    // PixelTransfer start_window site), matching Gambatte's
                    // M3Start::f0 semantics. The old per-line `window_line_counter`
                    // increment here (every line with ly > wy) is removed; the
                    // counter is no longer consumed by the fetcher.
                    // Reset window line flag for new scanline
                    self.window_started_this_line = false;
                    self.win_start_dot = None;
                    self.win_wx_penalty_resolved = false;

                    // Initialize OAM search state
                    self.sprites_on_line.clear();
                    self.current_oam_sprite_index = 0;
                    self.next_sprite_fetch_index = 0;
                    self.sprite_fetch_stall = 0;
                    self.pixel_transfer_warmup = 0;
                }
                
                // First line after enable: VRAM/OAM lock (PPU reports mode 3)
                // at the normal mode-2->3 boundary, even though the real pixel
                // fetch starts later at FIRST_FRAME_ARM_DOT. Matches Gambatte's
                // vramWritable/oamReadable (lineCycles-based, not M3Start).
                if self.first_line_after_enable {
                    let is_cgb = mmio.is_cgb_features_enabled();
                    let lock_dot = if is_cgb { cgb_first_frame_lock_dot(mmio.is_double_speed_mode()) } else { dmg_first_frame_lock_dot() };
                    if self.ticks == lock_dot && (mmio.read(LCD_STATUS) & 0x03) != 3 {
                        Self::set_lcd_status_mode(mmio, 3);
                        self.check_and_trigger_stat_interrupt(mmio);
                    }
                }

                // Perform sprite search distributed across 80 ticks
                // Check one sprite every 2 ticks (40 sprites × 2 ticks = 80 ticks)
                // Skipped on the first scanline after LCD enable (no Mode 2 phase).
                if !self.first_line_after_enable
                    && self.ticks.is_multiple_of(2)
                    && self.current_oam_sprite_index < OAM_SPRITE_COUNT
                {
                    self.check_single_sprite_for_scanline(mmio, self.current_oam_sprite_index);
                    self.current_oam_sprite_index += 1;
                    // Latch the OBJ-size for the NEXT scan slot from the live LCDC
                    // (DMG: write applies to entries scanned after it commits, not
                    // the one just read; Gambatte lsbuf per-slot latch).
                    self.scan_obj_size_large = (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;
                }
                
                let is_cgb = mmio.is_cgb_features_enabled();
                let pixel_transfer_arm_dot = if self.first_line_after_enable {
                    if is_cgb {
                        CGB_FIRST_FRAME_ARM_DOT
                    } else {
                        DMG_FIRST_FRAME_ARM_DOT
                    }
                } else if is_cgb {
                    CGB_PIXEL_TRANSFER_ARM_DOT
                } else {
                    DMG_PIXEL_TRANSFER_ARM_DOT
                };

                if self.ticks == pixel_transfer_arm_dot {
                    // Sort sprites by priority after OAM search is complete
                    if is_cgb {
                        // CGB mode: Sort by OAM index only (already in order, but ensure it)
                        self.sprites_on_line.sort_by_key(|sprite| sprite.oam_index);
                    } else {
                        // DMG mode: Sort by X coordinate first, then OAM index
                        self.sprites_on_line.sort_by(|a, b| {
                            a.x.cmp(&b.x).then(a.oam_index.cmp(&b.oam_index))
                        });
                    }
                    
                    self.x = 0;
                    self.fetcher.reset();
                    self.next_sprite_fetch_index = 0;
                    self.sprite_fetch_stall = 0;
                    self.fetcher_cadence_tick = 0;
                    // CGB arms two dots later, so use a shorter warmup to keep the first visible pixel aligned.
                    self.pixel_transfer_warmup = if is_cgb {
                        CGB_PIXEL_TRANSFER_WARMUP
                    } else {
                        DMG_PIXEL_TRANSFER_WARMUP
                    };
                    Self::set_lcd_status_mode(mmio, 3);
                    self.state = State::PixelTransfer;
                    // Gambatte M3Start::f0: if win_draw_start was armed from the
                    // previous line (DMG wx==166 case) and the window is enabled,
                    // the window draws from xpos 0 this line (++winYPos), even
                    // though WX is unchanged. Otherwise winDrawState clears to 0.
                    {
                        let win_en = (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
                        if self.win_draw_start && win_en && !self.first_line_after_enable {
                            self.win_y_pos = self.win_y_pos.wrapping_add(1);
                            self.win_draw_started_at_x0 = true;
                            // The window is `started` from line begin: fetch
                            // window tiles from xpos 0 (after the SCX discard
                            // prefix), not BG. Gambatte M3Start::f0 seeds
                            // wscx = tile_len + scx%8, so the first window tile
                            // column is wscx/8 == 1 (for scx<8).
                            let scx = (mmio.read(SCX) & 0x07) as u32;
                            let start_tile = ((8 + scx) / 8) as u8;
                            self.fetcher.start_window_at_tile(0, start_tile);
                            self.window_started_this_line = true;
                            self.win_start_dot = Some(self.ticks);
                        } else {
                            self.win_draw_started_at_x0 = false;
                        }
                        self.win_draw_start = false;
                    }
                    // DMG wx==166 (lcd_hres+6): the window cannot draw this line
                    // (the line ends before xpos reaches it) but ARMS for the next
                    // line. Gambatte plotPixel sets win_draw_start at xpos==166
                    // on DMG whenever the window-Y condition holds, regardless of
                    // whether the window is already drawing. Evaluated here at M3
                    // start (M3 always reaches xpos 166); win_draw_start is then
                    // consumed by the next line's M3Start::f0 above.
                    if !is_cgb
                        && !self.first_line_after_enable
                        && mmio.read(WX) == 166
                        && self.window_y_active(mmio)
                    {
                        self.win_draw_start = true;
                    }
                    // First scanline after enable is now armed; subsequent
                    // lines use normal Mode 2 timing.
                    let was_first_line = self.first_line_after_enable;
                    self.first_line_after_enable = false;
                    self.mode0_pretriggered_this_line = false;
                    self.mode0_reported_this_line = false;
                    // SCX fine-scroll discard target (Gambatte M3Start::f1): the
                    // break xpos is resolved over the first M3 dots by re-reading
                    // SCX live (see the early-window loop in PixelTransfer). Seed
                    // it unlatched (-1) and record the arm dot for xpos tracking.
                    self.m3_pixels_discarded = 0;
                    self.m3_arm_dot = self.ticks;
                    self.m3_arm_scx = (mmio.read(SCX) & 0x07) as u8;
                    // The first line after display enable has bespoke warmup/arm
                    // timing; the live f1 xpos mapping does not align there, so
                    // latch the discard immediately (pre-write SCX), as before.
                    self.m3_discard_target = if was_first_line { self.m3_arm_scx as i8 } else { -1 };
                    self.check_and_trigger_stat_interrupt(mmio);

                    if was_first_line {
                        self.scheduled_mode0_dot = None;
                        self.m0_time_master = None;
                        self.cgbp_block_start_cc = None;
                    } else {
                        // Closed-form mode-0 schedule, including window-start lines
                        // (compute_m3_length applies the window penalty). Mid-mode-3
                        // window-enable toggles (set_lcdc_visible) and WX changes
                        // (PixelTransfer) invalidate it, falling back to the live
                        // emergent x==160 transition.
                        let m3_len = self.compute_m3_length(mmio, is_cgb);
                        let ds = mmio.is_double_speed_mode() as u32;
                        // Byte-exact m0Time, lyTime-anchored (ENGINE_LAZY_PPU.md):
                        //   m0Time = (p_now + ly_counter().time + 1)
                        //            − ((456 − (m3_len + BASE)) << ds)
                        // BASE = 84 (CGB SS+DS), 83 (DMG — the `1−cgb` term already
                        // lives in m3_len). `p_now + ly_counter().time` is the
                        // next-LY master cc; +1 corrects rustyboi's LyCounter.time
                        // running 1 master-cc below Gambatte's lyTime.
                        let m0t = self.m0_time_exact(mmio, m3_len, is_cgb);
                        self.m0_time_master = Some(m0t);
                        // The within-line mode-0 dot is DERIVED from the same exact
                        // m0Time (master cc) so the eager-grid consumers (reported
                        // FF41 mode poke, m0 IRQ arm, cgbp tick fallback) ride the
                        // identical boundary: dot = arm_ticks + (m0t − arm_cc) >> ds.
                        let arm_cc = mmio.master_cc() as i64;
                        let dot = self.ticks as i64 + (((m0t as i64) - arm_cc) >> ds);
                        self.scheduled_mode0_dot = Some(dot.max(0) as u128);
                        self.m3_scheduled_wx = mmio.read(WX);
                        self.m3_scheduled_win = self.window_will_start(mmio, is_cgb);
                        // cgbp begin boundary (Gambatte cgbpAccessible: blocked once
                        // `lineCycles(cc) + ds >= 80`). lineCycles = ticks - (4 - cgb),
                        // so the block begins at the renderer tick
                        // `tick_block = 80 - ds + (4 - cgb)`; convert to master cc using
                        // the same arm anchor m0_time_master uses (each dot = 1<<ds cc).
                        let cgb_i = is_cgb as i64;
                        let tick_block = 80 - ds as i64 + (4 - cgb_i);
                        self.cgbp_block_start_cc = Some(
                            (mmio.master_cc() as i64
                                + ((tick_block - self.m3_arm_dot as i64) << ds))
                                .max(0) as u64,
                        );
                    }
                    // Arm the mode-0 (HBlank) STAT IRQ event at the predicted
                    // mode-0 start, in absolute clock terms. Gambatte schedules
                    // memevent_m0irq only when m0 is enabled, but keeps the time
                    // current for FF41/FF45 immediate-trigger checks; we always
                    // arm it (dispatch gates on the enable in mstat_irq).
                    self.arm_m0irq_for_current_line(mmio);
                }
            },
            State::PixelTransfer => 'label: {
                // A mid-mode-3 WX change before the window starts invalidates the
                // closed-form schedule; fall back to the live emergent transition.
                if self.scheduled_mode0_dot.is_some()
                    && !self.window_started_this_line
                    && (mmio.read(WX) != self.m3_scheduled_wx
                        || self.window_will_start(mmio, mmio.is_cgb_features_enabled())
                            != self.m3_scheduled_win)
                {
                    self.scheduled_mode0_dot = None;
                    self.m0_time_master = None;
                }
                // late_wx: a mid-mode-3 WX write AFTER the window has started,
                // moving WX out of range, cancels the remaining window draw and
                // refunds the unaccrued StartWindowDraw penalty from the
                // read-at-cc m0Time. Graduated like late_disable (one accrued dot
                // per drawn window dot, capped at WIN_M3_PENALTY); a nonzero SCX
                // fine-scroll prefix advances the accrual one dot. WX<7 windows
                // (immediate x==0 start) lock at win_start (no refund once
                // started). CGB single-speed / no sprites; live pipeline
                // untouched; applied once per line.
                if self.m0_time_master.is_some()
                    && self.window_started_this_line
                    && mmio.is_cgb_features_enabled()
                    && !mmio.is_double_speed_mode()
                    && self.sprites_on_line.is_empty()
                    && mmio.read(WX) != self.m3_scheduled_wx
                    && !self.win_wx_penalty_resolved
                    && std::env::var("RB_WIN_LATE_WX").map(|v| v != "0").unwrap_or(true)
                {
                    let wx_now = mmio.read(WX) as i32;
                    let wx_in_range = (0..=166).contains(&wx_now);
                    if let (Some(ws), Some(m0t)) = (self.win_start_dot, self.m0_time_master)
                        && !wx_in_range
                    {
                        if (self.m3_scheduled_wx as i32) < 7 {
                            // Immediate-start window: penalty already locked.
                            self.win_wx_penalty_resolved = true;
                        } else {
                            let scx_bias = if (self.m3_arm_scx & 7) != 0 { 1 } else { 0 };
                            let drawn = (self.ticks as i64) - ws as i64 + scx_bias;
                            let accrued = drawn.clamp(0, WIN_M3_PENALTY as i64);
                            let refund = WIN_M3_PENALTY as i64 - accrued;
                            self.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                            self.win_wx_penalty_resolved = true;
                        }
                    }
                }
                // ATOMIC mode-3 END: mode 3 ends at the exact closed-form m0Time
                // (master cc), and EVERYTHING (eager FF41 mode register, mode-0
                // STAT check, VRAM/OAM/cgbp unblock, m0 IRQ) is driven off this one
                // boundary. The pixel pipeline is now image-only: at the transition
                // we flush any remaining FIFO pixels to x==160 so the visible line
                // is complete, and the pipeline's own x==160 push no longer drives
                // timing. When no closed-form m0Time exists (first line after
                // enable / mid-M3 invalidation), fall back to the live x==160 push.
                if let Some(m0t) = self.m0_time_master {
                    if mmio.master_cc() >= m0t {
                        self.scheduled_mode0_dot = None;
                        // Flush remaining FIFO pixels to fill all 160 columns; the
                        // pipeline may lag the closed-form boundary by a few dots.
                        while self.x < 160 && self.draw_fifo_pixel(mmio) {}
                        self.state = State::HBlank;
                        if !self.mode0_reported_this_line {
                            self.mode0_reported_this_line = true;
                            Self::set_lcd_status_mode(mmio, 0);
                            self.check_and_trigger_stat_interrupt(mmio);
                        }
                        break 'label;
                    }
                }

                // Gambatte M3Start::f1 fine-scroll break resolution. The f1 loop
                // runs xpos = 0,1,2,... one per M3 dot, re-reading p.scx each
                // step, and breaks (fixing the discard count) at the first xpos
                // with xpos%8 == scx%8. xpos == ticks - arm dot, so reading SCX
                // here samples it at the same early M3 dots Gambatte does -
                // independent of the FIFO/warmup latency that delays the pops.
                // Once resolved the target is frozen, so a later SCX write past
                // the break has no effect (matching the single-write tests).
                if self.x == 0 && self.m3_discard_target < 0 {
                    const F1_OFFSET: i64 = -1;
                    let xpos = ((self.ticks as i64 - self.m3_arm_dot as i64 + F1_OFFSET).max(0)) as u32;
                    let scx_live = (mmio.read(SCX) & 0x07) as u32;
                    if xpos % 8 == scx_live || xpos >= 80 {
                        // Discard the full xpos count: a mid-discard SCX change can
                        // push the break past tile_len (Gambatte loops on to the
                        // next matching xpos), discarding more than 7 pixels.
                        self.m3_discard_target = xpos as i8;
                        // The closed-form mode-0 schedule assumed m3_arm_scx dots
                        // of discard; nudge it by the actual difference so M3 ends
                        // at the right dot (the extra discards lengthen M3).
                        if let Some(dot) = self.scheduled_mode0_dot {
                            let delta = xpos as i64 - self.m3_arm_scx as i64;
                            self.scheduled_mode0_dot = Some((dot as i64 + delta).max(0) as u128);
                            if let Some(m0t) = self.m0_time_master {
                                let ds = mmio.is_double_speed_mode() as u32;
                                self.m0_time_master =
                                    Some((m0t as i64 + (delta << ds)).max(0) as u64);
                            }
                        }
                    }
                }

                if self.sprite_fetch_stall > 0 {
                    self.sprite_fetch_stall -= 1;
                    break 'label;
                }

                if self.fetcher.pixel_fifo.size() != 0 && self.pixel_transfer_warmup == 0 {
                    self.sprite_fetch_stall = self.sprite_fetch_penalty_for_current_x(mmio).unwrap_or(0);
                    if self.sprite_fetch_stall > 0 {
                        self.sprite_fetch_stall -= 1;
                        break 'label;
                    }
                }

                // Fetcher cadence: on CGB, decouple from absolute self.ticks so that
                // sprite-fetch stall dots don't flip the fetcher's even/odd phase
                // (matches Gambatte). On DMG, keep the original self.ticks gate.
                let cadence_even = if mmio.is_cgb_features_enabled() {
                    let even = self.fetcher_cadence_tick % 2 == 0;
                    self.fetcher_cadence_tick = self.fetcher_cadence_tick.wrapping_add(1);
                    even
                } else {
                    self.ticks.is_multiple_of(2)
                };

                let fetcher_lcdc_state = self.fetcher_lcdc_state();
                // Pixels still to be discarded for SCX fine-scroll: they sit in
                // the FIFO but won't be displayed, so the BG tile column (derived
                // from display_x + FIFO depth) must not count them.
                let pending_discard = if self.x == 0 {
                    (self.m3_discard_target.max(0) as u8).saturating_sub(self.m3_pixels_discarded)
                } else {
                    0
                };
                if cadence_even
                    && let Some(event) = self.fetcher.step(mmio, self.win_y_pos, fetcher_lcdc_state, self.x, pending_discard, self.scy_delayed, self.scx_delayed) {
                        self.record_fetch_debug_event(event, mmio);
                }

                if self.fetcher.pixel_fifo.size() == 0 {
                    break 'label;
                }

                if self.pixel_transfer_warmup > 0 {
                    self.pixel_transfer_warmup -= 1;
                    break 'label;
                }

                // Check if we should start window rendering
                if self.window_y_active(mmio) && !self.fetcher.is_fetching_window() {
                    let wx = mmio.read(WX);
                    let is_cgb = mmio.is_cgb_features_enabled();
                    // DMG never starts the window drawing at WX==166; CGB does.
                    let wx_allowed = wx <= 166 && (is_cgb || wx != 166);
                    // WX=0-6 can trigger immediately, WX=7+ needs exact match with X+7
                    let should_start_window = wx_allowed
                        && if wx < 7 {
                            self.x == 0 // Start immediately if WX is 0-6
                        } else {
                            self.x + 7 == wx
                        };

                    if should_start_window {
                        // Window draw-start: Gambatte increments winYPos here
                        // (M3Start::f0 / plotPixel win_draw_start), once per line
                        // the window actually begins drawing, not per-line in M2.
                        self.win_y_pos = self.win_y_pos.wrapping_add(1);
                        // Start window rendering
                        self.fetcher.start_window(self.x);
                        self.window_started_this_line = true;
                        if self.win_start_dot.is_none() {
                            self.win_start_dot = Some(self.ticks);
                        }
                        break 'label; // Skip this cycle to let window fetching start
                    }
                }

                // SCX fine-scroll discard (Gambatte M3Start::f1 per-dot loop):
                // while x == 0, re-read the LIVE SCX each dot. If we have not
                // yet discarded `scx % 8` BG pixels, pop one and consume the
                // dot. A mid-M3 SCX write changes this count (and the fetched
                // tile column, since TileNumber re-reads SCX live).
                if self.x == 0 {
                    // Hold output until the f1 break is resolved (target latched).
                    if self.m3_discard_target < 0 {
                        break 'label;
                    }
                    let target = self.m3_discard_target as u8;
                    if self.m3_pixels_discarded < target
                        && let Ok(_) = self.fetcher.pixel_fifo.pop() {
                            self.m3_pixels_discarded += 1;
                            break 'label;
                    }
                }

                // Put a pixel from the FIFO on screen with sprite mixing.
                // Stop visible output at x==160; the scheduled dot ends Mode 3.
                if self.x >= 160 {
                    break 'label;
                }
                if self.draw_fifo_pixel(mmio) {
                    // Fallback (no closed-form m0Time: first line after enable /
                    // mid-M3 invalidation): end Mode 3 at the x==160 pixel push.
                    // When m0Time IS known the transition is driven off master_cc
                    // above; the pipeline caps at 160 and idles until then.
                    if self.m0_time_master.is_none() && self.x == 160 {
                        self.state = State::HBlank;
                        self.mode0_reported_this_line = true;
                        Self::set_lcd_status_mode(mmio, 0);
                        self.check_and_trigger_stat_interrupt(mmio);
                    }
                }
            },
            State::HBlank => {
                if self.ticks == 455 {
                    self.ticks = 0;
                    let current_ly = mmio.read(LY);
                    
                    if current_ly >= 143 {
                        mmio.write_ly_from_ppu(144);
                        self.state = State::VBlank;
                        Self::set_lcd_status_mode(mmio, 1);
                        mmio.request_interrupt(registers::InterruptFlag::VBlank);
                        self.check_and_trigger_stat_interrupt(mmio);
                    } else {
                        // Continue to next visible scanline
                        let next_ly = current_ly.saturating_add(1);
                        mmio.write_ly_from_ppu(next_ly);
                        self.state = State::OAMSearch;
                        self.enter_scheduled_mode2(mmio);
                        self.next_sprite_fetch_index = 0;
                        self.sprite_fetch_stall = 0;
                        self.pixel_transfer_warmup = 0;
                    }
                    return;
                }
            },
            State::VBlank => {
                // Partway through line 153, FF44 reads as 0 even though the
                // line itself has not ended. Update LYC=LY immediately so the
                // STAT line for LYC==0 fires one line earlier than the
                // visible LY=0 scanline.
                // At double speed the line-153 LY-register zero transition lands
                // a couple of dots later (in speed-independent dot units) than the
                // single-speed dot-6 threshold: the FF44/LYC=0 read tests
                // (ly0 lyc0flag_ds / lyc153flag_ds) only see LY==0 once the renderer
                // has advanced past dot 8. Gambatte's getLycCmpLy uses lineTime-6-6*ds
                // for the compare anticipation; the register-visibility transition
                // measured by these DS probes sits at dot 8. Single speed stays at 6.
                let line_153_zero_dot = if mmio.is_double_speed_mode() {
                    env_off("RB_LINE153_LY0_DOT_DS", 8).max(0) as u128
                } else {
                    LINE_153_LY_ZERO_DOT
                };
                if !self.line_153_ly_zeroed
                    && self.ticks == line_153_zero_dot
                    && mmio.read(LY) == 153
                {
                    mmio.write_ly_from_ppu(0);
                    self.line_153_ly_zeroed = true;
                    if mmio.read(LYC) == 0 {
                        mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2));
                    } else {
                        mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2));
                    }
                    self.check_and_trigger_stat_interrupt(mmio);
                }

                if self.ticks == 455 {
                    self.ticks = 0;
                    let current_ly = mmio.read(LY);
                    let end_of_frame = current_ly >= 153 || self.line_153_ly_zeroed;

                    if end_of_frame {
                        mmio.write_ly_from_ppu(0);
                        self.line_153_ly_zeroed = false;
                        self.state = State::OAMSearch;
                        self.enter_scheduled_mode2(mmio);
                        self.next_sprite_fetch_index = 0;
                        self.sprite_fetch_stall = 0;
                        self.pixel_transfer_warmup = 0;
                        self.window_line_counter = 0;
                        self.win_y_pos = 0xFF;
                        // NOTE: win_draw_start is intentionally NOT reset here.
                        // Gambatte resets winYPos at M2_Ly0::f0 but leaves
                        // winDrawState untouched across the frame boundary, so a
                        // window armed on the last visible line (e.g. DMG wx==166
                        // on line 143) carries win_draw_start through vblank and
                        // activates the window on the next frame's line 0.
                        self.window_y_triggered = false;
                        self.window_started_this_line = false;
                        
                        if mmio.is_cgb_features_enabled() {
                            // CGB mode: swap color framebuffers
                            self.color_fb_b = self.color_fb_a;
                            self.color_fb_a = [0; FRAMEBUFFER_SIZE * 3];
                        } else {
                            // DMG mode: swap monochrome framebuffers
                            self.fb_b = self.fb_a;
                            self.fb_a = [0; FRAMEBUFFER_SIZE];
                        }
                        
                        self.have_frame = true;
                    } else if (144..153).contains(&current_ly) {
                        let next_ly = current_ly.saturating_add(1);
                        mmio.write_ly_from_ppu(next_ly);
                    }
                    return;
                }
            },
        }
        // Latch the live DMG palette registers for use one dot from now. A
        // mid-mode-3 write lands before this dot's pixel push (the CPU resolves
        // the write before stepping the M-cycle's four dots), so resolving from
        // last dot's snapshot gives the one-dot apply latency hardware shows.
        self.bgp_delayed = mmio.read(BGP);
        self.obp0_delayed = mmio.read(OBP0);
        self.obp1_delayed = mmio.read(OBP1);
        self.ticks += 1;
    }

    pub fn frame_ready(&self) -> bool {
        self.have_frame
    }

    pub fn get_frame(&mut self, mmio: &mmio::Mmio) -> crate::gb::Frame {
        self.have_frame = false;
        if mmio.is_cgb_features_enabled() {
            crate::gb::Frame::Color(self.color_fb_b)
        } else {
            crate::gb::Frame::Monochrome(self.fb_b)
        }
    }

    // Debug methods
    pub fn get_fetcher_pixel_buffer(&self) -> [u8; 8] {
        self.fetcher.get_pixel_buffer()
    }

    pub fn get_fetcher_fifo_size(&self) -> usize {
        self.fetcher.get_fifo_size()
    }

    pub fn get_fetcher_tile_index(&self) -> u8 {
        self.fetcher.get_tile_index()
    }

    pub fn get_sprite_fetch_stall(&self) -> u8 {
        self.sprite_fetch_stall
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    pub fn get_state(&self) -> &State {
        &self.state
    }

    pub fn get_ticks(&self) -> u128 {
        self.ticks
    }

    /// Cycle-exact HDMA-eligibility predicate, mirroring Gambatte's
    /// `isHdmaPeriod` (video.cpp): a visible line, the within-line dot is at or
    /// past the predicted mode-0 (HBlank) start, and there is still room before
    /// line end to run a block (`dot + 3 + 3*ds < lineEnd`). Returns None when
    /// no closed-form mode-0 dot is available (window/first line after enable),
    /// so callers can fall back to the STAT mode-edge model. Read-only.
    pub fn hdma_period(&self, double_speed: bool) -> Option<bool> {
        if self.disabled {
            return None;
        }
        let m0 = self.scheduled_mode0_dot? as i128;
        let ly = self.internal_ly_val;
        if ly >= 144 {
            return Some(false);
        }
        let ds = double_speed as i128;
        let dot = self.ticks as i128;
        // Gambatte gates HDMA on `cc >= m0Time` but its eligibility call site
        // (video.cpp:357) passes `cc + 4`; the +1 dot here aligns the renderer
        // tick with that access cc. Net +1 on the dma suite, no regressions.
        let m0n = m0 + self.dma_scx_m0_nudge(double_speed, false) as i128;
        Some(dot >= m0n + 1 && dot + 3 + 3 * ds < 456)
    }

    /// SCX-phase-conditioned nudge to the mode-0 boundary dot used by the
    /// HDMA/VRAM-lock predictors (NOT the m0 STAT IRQ, which is calibrated
    /// separately). The closed-form `compute_m3_length` prefix `scx + (1-cgb)`
    /// is a dot-count model; at some SCX phases Gambatte's M3Start fine-scroll
    /// dispatch lands the actual HBlank one renderer dot off from that linear
    /// model, and that boundary feeds the HDMA trigger / VRAM-unlock the dma
    /// suite measures. Env-overridable, gated per SCX&7 phase and per speed so
    /// it cannot touch co-calibrated clusters at other phases.
    fn dma_scx_m0_nudge(&self, double_speed: bool, vram: bool) -> i64 {
        let scx = self.m3_arm_scx & 0x07;
        let suffix = if double_speed { "_DS" } else { "" };
        // Two surgical, phase-scoped boundary nudges, each a clean -1 on the dma
        // cluster with zero regressions across the co-calibrated clusters
        // (window / scx_during_m3 / cgbpal_m3 / enable_display / scy / oamdma):
        //
        // * HDMA-trigger boundary, SCX&7==1 (vram=false): Gambatte's M3Start
        //   fine-scroll dispatch lands the actual HBlank one renderer dot before
        //   the linear `scx + (1-cgb)` prefix model implies, so the HDMA block at
        //   this phase arms one dot early in our model; -1 realigns it. Only the
        //   HDMA consumer (dma cluster) sees this; VRAM-lock is untouched here.
        //
        // * VRAM-lock end boundary, SCX&7==3 (vram=true): at this phase the
        //   cycle-exact mode-3->0 unblock the dma reads probe sits one dot late
        //   vs hardware; -1 realigns it. Verified to fix 1 dma with no regression
        //   in any co-calibrated VRAM/OAM/cgbpal-access test.
        //
        // SCX&7==0 was -2 on dma-only but regresses two window m2int_wxA6
        // busyread tests, so it is deliberately left unbiased (default 0).
        let default = match (vram, scx) {
            (false, 1) => -1,
            (true, 3) => -1,
            _ => 0,
        };
        let pfx = if vram { "RB_VRAM_M0_SCX" } else { "RB_DMA_M0_SCX" };
        match scx {
            0 | 1 | 2 | 3 | 5 => env_off(&format!("{pfx}{scx}{suffix}"), default),
            _ => 0,
        }
    }

    /// Whether the CPU may currently access VRAM/OAM/CGB-palette, mirroring
    /// Gambatte's `vramReadable`/`vramWritable`/`oamReadable`/`oamWritable`/
    /// `cgbpAccessible` lineCycle thresholds rather than the rounded FF41 mode.
    /// `ticks` is the renderer's within-line dot (mode-3 starts at dot 80 DMG /
    /// 82 CGB); Gambatte's `lineCycles` frame is `ticks - (4 - cgb)`. The mode-0
    /// end is the scheduled mode-0 dot. Returns None when no closed-form mode-0
    /// dot is available (window / first line after enable) so the caller falls
    /// back to the FF41-mode gate. `is_read` selects the read vs write
    /// threshold; `kind`: 0=vram, 1=oam, 2=cgbpal. Read-only.
    /// `mode3_locked` is the caller's FF41-mode start gate (mode 3 for vram/cgbp,
    /// mode 2|3 for oam). The cycle-exact predictor only refines the mode-3->0
    /// END boundary against `scheduled_mode0_dot` (Gambatte's `m0TimeOfCurrentLine`);
    /// the start stays on the renderer's mode set, which is window-independent.
    pub fn cpu_access_blocked(&self, kind: u8, is_read: bool, mode3_locked: bool, is_cgb: bool, double_speed: bool, access_cc: u64) -> Option<bool> {
        let _ = is_read;
        if self.disabled || self.internal_ly_val >= 144 {
            return Some(false);
        }
        let cc = access_cc as i64;
        let ds = double_speed as i64;
        // CGB palette RAM (FF69/FF6B): Gambatte `cgbpAccessible(cc)` — accessible
        // iff `lineCycles(cc) + ds < 80` OR `cc >= m0Time + 2`. Both boundaries are
        // resolved at the raw access cc against master-cc anchors (begin =
        // cgbp_block_start_cc, end = exact m0_time_master).
        if kind == 2 {
            if let Some(start) = self.cgbp_block_start_cc {
                let begun = cc >= start as i64;
                let ended = match self.m0_time_master {
                    Some(m0t) => cc >= m0t as i64 + 2,
                    None => false,
                };
                return Some(begun && !ended);
            }
            // No begin anchor (first line after enable / window fallback): use the
            // renderer-tick boundary below.
            let m0t = self.m0_time_master;
            let begun = self.ticks as i64 + ds - (4 - is_cgb as i64) >= 80;
            let ended = match m0t {
                Some(m0t) => cc >= m0t as i64 + 2,
                None => return Some(begun && mode3_locked),
            };
            return Some(begun && !ended);
        }
        // VRAM/OAM: blocked during mode 3 (start gated on the FF41 mode register,
        // window-safe); END unblocks at Gambatte's `cc + 2 >= m0Time` (exact).
        // The m0Time end-boundary only applies once mode 3 has begun: during mode 2
        // (OAMSearch) `m0_time_master` still holds the PREVIOUS line's (now-past)
        // value, so the `cc+2 >= m0t` test would spuriously report "ended" and
        // unblock OAM mid-OAM-scan. In mode 2 the access is simply blocked.
        let m0t = self.m0_time_master? as i64;
        let ended = self.state != State::OAMSearch && cc + 2 >= m0t;
        Some(mode3_locked && !ended)
    }

    /// Gambatte `getStat` mode-3 <-> mode-0 resolution at the CPU's access cc.
    /// Returns the FF41 lower two mode bits the CPU observes when reading FF41 at
    /// `access_cc` (master-cc units), or None when no closed-form m0Time is
    /// available (window / first line / not in mode 3) so the bus falls back to
    /// the renderer-set FF41 register.
    ///
    /// Gambatte resolves mode 3 iff `cc + 2 < m0TimeOfCurrentLine(cc)`; the first
    /// mode-0 read therefore lands at `cc = m0Time - 2`. This reproduces the
    /// (now Gambatte-exact) persisted boundary at single speed and adds correct
    /// sub-dot resolution at double speed, where the CPU samples FF41 at an odd
    /// master cc that the per-dot renderer would otherwise round.
    pub fn get_stat_mode3to0_at_cc(&self, access_cc: u64) -> Option<u8> {
        if self.disabled || self.internal_ly_val >= 144 {
            return None;
        }
        // Only refine when the renderer currently reports mode 3 (we are in the
        // mode-3 window for this line) and a closed-form m0Time exists. Outside
        // mode 3 the register is already correct (mode 0/2 boundaries handled
        // elsewhere).
        if self.state != State::PixelTransfer {
            return None;
        }
        let m0t = self.m0_time_master? as i64;
        // Gambatte getStat: mode 3 iff `cc + 2 < m0Time` (raw master cc).
        if (access_cc as i64) + 2 < m0t {
            Some(3)
        } else {
            Some(0)
        }
    }


    pub fn get_x(&self) -> u8 {
        self.x
    }

    pub fn has_frame(&self) -> bool {
        self.have_frame
    }

    pub fn get_sprites_on_line_count(&self) -> usize {
        self.sprites_on_line.len()
    }
    
    // CGB color conversion functions
    fn cgb_color_to_rgb(&self, low_byte: u8, high_byte: u8) -> (u8, u8, u8) {
        // CGB color format: GGGRRRRR BBBBBGGG (little endian)
        let color_word = (high_byte as u16) << 8 | low_byte as u16;
        
        // Extract 5-bit RGB components
        let r = (color_word & 0x1F) as u16;
        let g = ((color_word >> 5) & 0x1F) as u16;
        let b = ((color_word >> 10) & 0x1F) as u16;
        
        match self.cgb_color_conversion {
            CgbColorConversion::Linear => {
                let r8 = ((r * 255) / 31) as u8;
                let g8 = ((g * 255) / 31) as u8;
                let b8 = ((b * 255) / 31) as u8;
                (r8, g8, b8)
            }
            CgbColorConversion::Gambatte => {
                let r8 = ((r * 13 + g * 2 + b) / 2) as u8;
                let g8 = ((g * 3 + b) * 2) as u8;
                let b8 = ((r * 3 + g * 2 + b * 11) / 2) as u8;
                (r8, g8, b8)
            }
        }
    }
    
    fn get_cgb_bg_color(&self, mmio: &mmio::Mmio, palette_idx: u8, color_idx: u8) -> (u8, u8, u8) {
        if !mmio.is_cgb_features_enabled() {
            // Fallback to monochrome conversion
            let mono_color = self.get_palette_color(mmio, color_idx);
            let intensity = match mono_color {
                0 => 255, // White
                1 => 170, // Light gray
                2 => 85,  // Dark gray
                _ => 0,   // Black
            };
            return (intensity, intensity, intensity);
        }
        
        // Read CGB palette data from palette RAM
        let (low_byte, high_byte) = mmio.read_bg_palette_data(palette_idx, color_idx);
        self.cgb_color_to_rgb(low_byte, high_byte)
    }
    
    fn get_cgb_obj_color(&self, mmio: &mmio::Mmio, palette_idx: u8, color_idx: u8) -> (u8, u8, u8) {
        if color_idx == 0 {
            return (0, 0, 0); // Transparent - will be handled by caller
        }
        
        if !mmio.is_cgb_features_enabled() {
            // Fallback to monochrome conversion
            let mono_color = self.get_sprite_palette_color(mmio, color_idx, palette_idx != 0);
            let intensity = match mono_color {
                0 => 0,   // Transparent
                1 => 170, // Light gray
                2 => 85,  // Dark gray
                _ => 0,   // Black
            };
            return (intensity, intensity, intensity);
        }
        
        // Read CGB palette data from palette RAM
        let (low_byte, high_byte) = mmio.read_obj_palette_data(palette_idx, color_idx);
        self.cgb_color_to_rgb(low_byte, high_byte)
    }

    // Check a single sprite during distributed OAM search
    fn check_single_sprite_for_scanline(&mut self, mmio: &mut mmio::Mmio, sprite_index: usize) {
        // Skip if we already have the maximum sprites for this line
        if self.sprites_on_line.len() >= MAX_SPRITES_PER_LINE {
            return;
        }
        
        let ly = mmio.read(LY);
        let lcdc = self.lcdc;

        // OAM scan (Gambatte's SpriteMapper::mapSprites) builds the per-line
        // sprite list regardless of the OBJ-enable bit (LCDC.1). The enable bit
        // only gates the M3 sprite fetch and the final pixel mix, so a sprite
        // enabled mid-mode-3 still incurs its fetch penalty. Do not early-out
        // here on OBJ-disable.

        // Determine sprite height (8x8 or 8x16). Use the per-line scan latch
        // (lags the live LCDC by one OAM slot) so a mid-mode-2 OBJ-size write
        // affects only entries scanned strictly after it commits, matching
        // Gambatte's per-entry lsbuf latch. Gated for safety.
        let use_latch = std::env::var("RB_OBJSIZE_SCAN")
            .map(|v| v != "0")
            .unwrap_or(true);
        let large = if use_latch {
            self.scan_obj_size_large
        } else {
            (lcdc & (LCDCFlags::SpriteSize as u8)) != 0
        };
        let sprite_height = if large { 16 } else { 8 };

        let oam_offset = sprite_index * OAM_BYTES_PER_SPRITE;
        let sprite_y = mmio.read(0xFE00 + oam_offset as u16);
        let sprite_x = mmio.read(0xFE00 + oam_offset as u16 + 1);
        let tile_index = mmio.read(0xFE00 + oam_offset as u16 + 2);
        let attributes_byte = mmio.read(0xFE00 + oam_offset as u16 + 3);

        // Sprites use offset coordinates: Y=0 is at line -16, X=0 is at column -8
        let sprite_screen_y = sprite_y.wrapping_sub(16);
        
        // Check if sprite is visible on current scanline
        if ly >= sprite_screen_y && ly < sprite_screen_y + sprite_height {
            let sprite = Sprite {
                y: sprite_y,
                x: sprite_x,
                tile_index,
                attributes: SpriteAttributes::from_byte(attributes_byte),
                oam_index: sprite_index as u8,
            };
            
            self.sprites_on_line.push(sprite);
        }
    }

    fn sprite_fetch_penalty_for_current_x(&mut self, mmio: &mmio::Mmio) -> Option<u8> {
        let lcdc = self.lcdc;
        if (lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) == 0 && !mmio.is_cgb_features_enabled() {
            return None;
        }

        while self.next_sprite_fetch_index < self.sprites_on_line.len() {
            let sprite_x = self.sprites_on_line[self.next_sprite_fetch_index].x;
            let trigger_x = sprite_x.saturating_sub(8);

            if trigger_x < self.x {
                self.next_sprite_fetch_index += 1;
                continue;
            }

            if trigger_x > self.x {
                return None;
            }

            self.next_sprite_fetch_index += 1;

            if sprite_x == 0 {
                return Some(11);
            }

            // Match Gambatte's addSpriteCycles: first sprite per BG tile contributes
            // (11 - distanceFromTileStart) dots, where distance < 5; otherwise 6.
            // distance = pixel_in_tile = (x + scx) & 7. (7-x).saturating_sub(2) + 6 yields
            // 11,10,9,8,7,6,6,6 for pixel_in_tile = 0..7, matching Gambatte exactly.
            let pixel_in_tile = self.x.wrapping_add(mmio.read(SCX)) & 0x07;
            let wait_for_bg_fetch = (7u8 - pixel_in_tile).saturating_sub(2);
            let base_penalty = wait_for_bg_fetch + 6;
            return Some(base_penalty);
        }

        None
    }

    // Mix background pixel with sprites at the given screen coordinates (CGB color version)
    fn mix_background_and_sprites_color(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, bg_attrs: u8, screen_x: u8, screen_y: u8) -> (u8, u8, u8) {
        let lcdc = self.lcdc;
        let bg_priority_master = (lcdc & (LCDCFlags::BGDisplay as u8)) != 0;

        // Background attributes captured at fetch time travel with the pixel.
        let tile_attributes = bg_attrs;
        let palette_idx = tile_attributes & 0x07; // Bits 0-2 = palette index
        let bg_color_rgb = self.get_cgb_bg_color(mmio, palette_idx, bg_pixel_idx);
        
        // Check if sprites are enabled
        if (lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) == 0 {
            return bg_color_rgb;
        }
        
        // First, resolve object-to-object priority to find the highest priority opaque sprite pixel
        let mut selected_sprite: Option<(&Sprite, u8, (u8, u8, u8))> = None; // (sprite, pixel_idx, color)
        
        for sprite in &self.sprites_on_line {
            // Sprite X coordinate is offset by 8, Y coordinate is offset by 16
            let sprite_actual_x = sprite.x as i16 - 8;
            let sprite_actual_y = sprite.y as i16 - 16;
            
            // Check if this screen pixel is within the sprite bounds
            let relative_x = screen_x as i16 - sprite_actual_x;
            let relative_y = screen_y as i16 - sprite_actual_y;
            
            // Sprite is 8 pixels wide
            if (0..8).contains(&relative_x) {
                let sprite_height = if (lcdc & (LCDCFlags::SpriteSize as u8)) != 0 { 16 } else { 8 };
                if relative_y >= 0 && relative_y < sprite_height as i16 {
                    // Get sprite pixel data
                    if let Some(sprite_pixel_idx) = self.get_sprite_pixel(mmio, sprite, relative_x as u8, relative_y as u8)
                        && sprite_pixel_idx != 0 { // Sprite pixel is not transparent
                            
                            // Get sprite palette (in CGB mode, sprite attributes can specify palette)
                            let sprite_palette_idx = if mmio.is_cgb_features_enabled() {
                                // CGB mode: Use bits 2-0 for palette selection (0-7)
                                sprite.attributes.raw & 0x07
                            } else {
                                // DMG mode: Use bit 4 for palette selection (0-1)
                                if sprite.attributes.palette { 1 } else { 0 }
                            };
                            
                            let sprite_color_rgb = self.get_cgb_obj_color(mmio, sprite_palette_idx, sprite_pixel_idx);
                            
                            // Check if this sprite has higher priority than the currently selected one
                            let is_higher_priority = if let Some((current_sprite, _, _)) = selected_sprite {
                                if mmio.is_cgb_features_enabled() {
                                    // CGB mode: Only OAM position matters (lower index = higher priority)
                                    sprite.oam_index < current_sprite.oam_index
                                } else {
                                    // DMG mode: X coordinate first, then OAM position
                                    sprite.x < current_sprite.x || 
                                    (sprite.x == current_sprite.x && sprite.oam_index < current_sprite.oam_index)
                                }
                            } else {
                                true // First opaque sprite found
                            };
                            
                            if is_higher_priority {
                                selected_sprite = Some((sprite, sprite_pixel_idx, sprite_color_rgb));
                            }
                        }
                }
            }
        }
        
        // Now resolve BG vs OBJ priority using the selected sprite (if any)
        if let Some((sprite, _, sprite_color_rgb)) = selected_sprite {
            if mmio.is_cgb_features_enabled() {
                // CGB priority rules
                // If BG color index is 0, OBJ always has priority
                if bg_pixel_idx == 0 {
                    return sprite_color_rgb;
                }
                
                // In CGB mode LCDC bit 0 keeps BG/window visible, but disables BG priority over OBJ.
                if !bg_priority_master {
                    return sprite_color_rgb;
                }
                
                // Check BG attributes bit 7 and OAM attributes bit 7
                let bg_priority = (tile_attributes & 0x80) != 0; // BG attr bit 7
                let obj_priority = sprite.attributes.priority;   // OAM attr bit 7 (note: priority=true means "behind BG")
                
                // If both BG and OAM attributes have bit 7 clear, OBJ has priority
                // Otherwise, BG has priority (when BG color is 1-3)
                if !bg_priority && !obj_priority {
                    return sprite_color_rgb; // OBJ priority
                } else {
                    return bg_color_rgb; // BG priority for colors 1-3
                }
            } else {
                // DMG mode: Simple priority check
                if !sprite.attributes.priority || bg_pixel_idx == 0 {
                    return sprite_color_rgb;
                }
            }
        }
        
        bg_color_rgb
    }

    // Mix background pixel with sprites at the given screen coordinates
    fn mix_background_and_sprites(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, screen_x: u8, screen_y: u8) -> u8 {
        let lcdc = self.lcdc;
        
        // Check if BG/Window display is enabled (LCDC bit 0)
        let bg_enabled = (lcdc & (LCDCFlags::BGDisplay as u8)) != 0;
        
        // Get background color - if BG display is disabled, force to white (color 0)
        let bg_color = if bg_enabled {
            self.get_palette_color(mmio, bg_pixel_idx)
        } else {
            // When BG display is disabled, background becomes white (palette color 0)
            self.get_palette_color(mmio, 0)
        };
        
        // For sprite priority calculation, we need the original bg_pixel_idx
        let effective_bg_pixel_idx = if bg_enabled { bg_pixel_idx } else { 0 };
        
        // Check if sprites are enabled
        if (lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) == 0 {
            return bg_color;
        }
        
        // Find the highest priority sprite at this position
        for sprite in &self.sprites_on_line {
            // Sprite X coordinate is offset by 8, Y coordinate is offset by 16
            let sprite_actual_x = sprite.x as i16 - 8;
            let sprite_actual_y = sprite.y as i16 - 16;
            
            // Check if this screen pixel is within the sprite bounds
            let relative_x = screen_x as i16 - sprite_actual_x;
            let relative_y = screen_y as i16 - sprite_actual_y;
            
            // Sprite is 8 pixels wide
            if (0..8).contains(&relative_x) {
                let sprite_height = if (lcdc & (LCDCFlags::SpriteSize as u8)) != 0 { 16 } else { 8 };
                if relative_y >= 0 && relative_y < sprite_height as i16 {
                    // Get sprite pixel data
                    if let Some(sprite_pixel_idx) = self.get_sprite_pixel(mmio, sprite, relative_x as u8, relative_y as u8)
                        && sprite_pixel_idx != 0 { // Sprite pixel is not transparent
                            let sprite_color = self.get_sprite_palette_color(mmio, sprite_pixel_idx, sprite.attributes.palette);
                            
                            // Handle sprite priority
                            if !sprite.attributes.priority || effective_bg_pixel_idx == 0 {
                                // Sprite appears above background or background is transparent
                                return sprite_color;
                            }
                            // If sprite has priority=1 and background is not color 0, background wins
                        }
                }
            }
        }
        
        bg_color
    }

    // Get a specific pixel from a sprite's tile data
    fn get_sprite_pixel(&self, mmio: &mmio::Mmio, sprite: &Sprite, sprite_x: u8, sprite_y: u8) -> Option<u8> {
        let lcdc = self.lcdc;
        let sprite_height = if (lcdc & (LCDCFlags::SpriteSize as u8)) != 0 { 16 } else { 8 };
        
        if sprite_x >= 8 || sprite_y >= sprite_height {
            return None;
        }
        
        // Handle Y flipping
        let actual_y = if sprite.attributes.y_flip {
            sprite_height - 1 - sprite_y
        } else {
            sprite_y
        };
        
        // For 8x16 sprites, the tile index is different
        let tile_index = if sprite_height == 16 {
            if actual_y < 8 {
                sprite.tile_index & 0xFE // Top tile (even)
            } else {
                sprite.tile_index | 0x01  // Bottom tile (odd)
            }
        } else {
            sprite.tile_index
        };
        
        let tile_line = actual_y % 8;
        
        // Sprite tiles always use the $8000 addressing method
        let tile_addr = 0x8000 + (tile_index as u16) * 16 + (tile_line as u16) * 2;
        
        // In CGB mode, sprites can use VRAM bank 1 if bit 3 is set
        let (low_byte, high_byte) = if mmio.is_cgb_features_enabled() && (sprite.attributes.raw & 0x08) != 0 {
            // Read from VRAM bank 1
            (mmio.read_vram_bank1(tile_addr), mmio.read_vram_bank1(tile_addr + 1))
        } else {
            // Read from VRAM bank 0 (or current bank on DMG)
            (mmio.read(tile_addr), mmio.read(tile_addr + 1))
        };
        
        // Handle X flipping
        let bit_index = if sprite.attributes.x_flip {
            sprite_x
        } else {
            7 - sprite_x
        };
        
        let low_bit = (low_byte >> bit_index) & 1;
        let high_bit = (high_byte >> bit_index) & 1;
        
        Some((high_bit << 1) | low_bit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::SM83;
    use crate::memory::Addressable;

    // The previous mode-2 STAT pretrigger unit tests were removed: the Mode-2
    // STAT IRQ is now delivered by the event-scheduled model (see `stat_irq` and
    // `dispatch_stat_events`), validated end-to-end by the Gambatte hwtest suite
    // (m2int/m2enable/miscmstatirq clusters), not the old per-dot pretrigger.

    #[test]
    fn cgb_lcdc_enabled_write_applies_tile_data_before_full_lcdc() {
        let mut mmio = mmio::Mmio::new();
        mmio.set_cgb_features_enabled(true);

        let old_lcdc = LCDCFlags::DisplayEnable as u8
            | LCDCFlags::SpriteDisplayEnable as u8
            | LCDCFlags::SpriteSize as u8
            | LCDCFlags::BGWindowTileDataSelect as u8;
        let new_lcdc = LCDCFlags::DisplayEnable as u8
            | LCDCFlags::BGDisplay as u8
            | LCDCFlags::SpriteDisplayEnable as u8
            | LCDCFlags::SpriteSize as u8
            | LCDCFlags::BGTileMapDisplaySelect as u8;

        mmio.write(LCD_CONTROL, old_lcdc);
        let mut ppu = Ppu::new();
        ppu.sync_lcdc_from_mmio(&mmio);
        ppu.handle_lcdc_write(new_lcdc, &mmio);

        ppu.step_lcdc_events(&mmio);
        assert_eq!(ppu.lcdc & (LCDCFlags::BGWindowTileDataSelect as u8), 0);
        assert_eq!(ppu.lcdc & (LCDCFlags::BGDisplay as u8), 0);
        assert_eq!(ppu.lcdc & (LCDCFlags::BGTileMapDisplaySelect as u8), 0);
        assert!(ppu.cgb_tile_index_is_tile_data);

        ppu.step_lcdc_events(&mmio);
        assert_eq!(ppu.lcdc, new_lcdc);
        assert_ne!(ppu.lcdc & (LCDCFlags::BGDisplay as u8), 0);
        assert!(!ppu.cgb_tile_index_is_tile_data);
    }
}

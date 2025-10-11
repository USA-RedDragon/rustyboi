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
const CGB_FIRST_FRAME_ARM_DOT: u128 = 86;
// On the first line after enable, VRAM/OAM lock (PPU reports mode 3) at the
// same line-cycle as a normal line (Gambatte: lineCycles >= ~79), even though
// the actual pixel fetch (M3Start) begins later at FIRST_FRAME_ARM_DOT.
const DMG_FIRST_FRAME_LOCK_DOT: u128 = 80;
const CGB_FIRST_FRAME_LOCK_DOT: u128 = 82;
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
// hook fires after the store but before this M-cycle's dots tick; `abs_cc`
// advances by 1<<ds per dot, so at double speed the write's true cycle is a
// full M-cycle (4 machine cycles) behind. Swept against the suite.
const WRITE_CC_OFFSET: i64 = -1;
const WRITE_CC_OFFSET_DS: i64 = -4;
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
    window_y_triggered: bool,   // Whether WY condition was met this frame
    window_started_this_line: bool, // Whether window started rendering on current scanline
    
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
    // WX snapshot taken when the closed-form mode-0 schedule was computed; a
    // mid-mode-3 WX change before the window starts invalidates the schedule.
    m3_scheduled_wx: u8,
    // Absolute `ticks` dot at which Mode 3 -> Mode 0 (HBlank) fires. Computed
    // at M3 arm from a cycle-exact mode-3 length formula (Gambatte oracle) and
    // drives the FF41 mode bits + mode-0 STAT IRQ, replacing the x==160 trigger.
    #[serde(default)]
    scheduled_mode0_dot: Option<u128>,

    // Event-scheduled STAT/mode/LYC IRQ model (Gambatte port). `abs_cc` is a
    // monotonic absolute dot clock; `line_cycle` (0..455) tracks position
    // within the current 456-dot line. Together they reproduce Gambatte's
    // `lyCounter` (`time` = abs_cc when LY next increments).
    #[serde(default)]
    abs_cc: u64,
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
            window_y_triggered: false,
            window_started_this_line: false,
            previous_stat_interrupt_line: false,
            mode2_irq_pretriggered_for_next_line: false,
            first_line_after_enable: false,
            line_153_ly_zeroed: false,
            mode0_pretriggered_this_line: false,
            m3_pixels_discarded: 0,
            m3_scheduled_wx: 0,
            scheduled_mode0_dot: None,
            abs_cc: 0,
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
        self.set_lcdc_visible(mmio.read(LCD_CONTROL), mmio.is_cgb_features_enabled());
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
            self.set_lcdc_visible(value, mmio.is_cgb_features_enabled());
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
                        self.set_lcdc_visible(value, mmio.is_cgb_features_enabled());
                    }
                    PendingLcdcEventKind::Full => {
                        self.set_lcdc_visible(event.value, mmio.is_cgb_features_enabled());
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

    fn set_lcdc_visible(&mut self, value: u8, cgb_features_enabled: bool) {
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
        if self.state == State::PixelTransfer && (old_lcdc & win_bit) != (value & win_bit) {
            self.scheduled_mode0_dot = None;
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

    pub fn get_palette_color(&self, mmio: &mmio::Mmio, idx: u8) -> u8 {
        match idx {
            0 => mmio.read(BGP)&0x03,        // White
            1 => (mmio.read(BGP)>>2)&0x03, // Light Gray
            2 => (mmio.read(BGP)>>4)&0x03, // Dark Gray
            3 => (mmio.read(BGP)>>6)&0x03, // Black
            _ => 0x00, // Default to black for invalid indices
        }
    }

    pub fn get_sprite_palette_color(&self, mmio: &mmio::Mmio, idx: u8, palette: bool) -> u8 {
        if idx == 0 {
            return 0; // Transparent for sprites
        }
        
        let palette_reg = if palette { OBP1 } else { OBP0 };
        match idx {
            1 => (mmio.read(palette_reg)>>2)&0x03, // Light Gray
            2 => (mmio.read(palette_reg)>>4)&0x03, // Dark Gray
            3 => (mmio.read(palette_reg)>>6)&0x03, // Black
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

    /// Arm `sched_m0irq` for the current line from the renderer's predicted
    /// mode-0 start (`scheduled_mode0_dot`, a within-line dot). Converted to the
    /// absolute clock. If no closed-form mode-0 dot is available (window/first
    /// line), fall back to the m0 prediction from the m3 length.
    fn arm_m0irq_for_current_line(&mut self, mmio: &mmio::Mmio) {
        let is_cgb = mmio.is_cgb_features_enabled();
        let mode0_within_line = match self.scheduled_mode0_dot {
            Some(d) => d as i64,
            None => {
                let m3_len = self.compute_m3_length(mmio, is_cgb);
                let offset = if is_cgb { CGB_MODE0_OFFSET } else { DMG_MODE0_OFFSET };
                self.ticks as i64 + m3_len as i64 + offset as i64
            }
        };
        // The renderer's "current dot" abs value is abs_cc-1 (advanced at top of
        // step). Dots remaining until mode 0 = mode0_within_line - ticks.
        let remaining = mode0_within_line - self.ticks as i64;
        let off = M0IRQ_OFFSET;
        let ds = mmio.is_double_speed_mode();
        let dsf = 1i64 << ds as i32;
        let abs = (self.abs_cc as i64 - dsf + (remaining + off) * dsf).max(0) as u64;
        self.sched_m0irq = abs;
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
        self.sched_m2irq = if m2 == stat_irq::DISABLED_TIME { m2 } else { (m2 as i64 + Self::m2_off()) as u64 };
        // m0irq is scheduled from the renderer's mode-0 prediction; (re)armed
        // when entering pixel transfer. Leave as-is here.
    }

    /// Fire any STAT IRQ events whose scheduled time has arrived at the current
    /// `abs_cc`. Called once per dot from `step`.
    fn dispatch_stat_events(&mut self, mmio: &mut mmio::Mmio) {
        let ds = mmio.is_double_speed_mode();
        let cc = self.abs_cc;

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
        if self.sched_lycirq <= cc {
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

    fn m2_off() -> i64 {
        M2IRQ_OFFSET
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
    fn window_will_start(&self, mmio: &mmio::Mmio, is_cgb: bool) -> bool {
        let window_enabled = (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
        if !window_enabled || !self.window_y_triggered {
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
            cycles += WIN_M3_PENALTY;
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
                // First-sprite special case: fno=1, xpos=0.
                let spx0 = sprite_xs[0];
                let prev_tile_no = (0 - first_tile_xpos) & !7; // (xpos - firstTileXpos) & -8
                if 1 + spx0 < 5 && spx0 <= nwx && spx0 <= target_x {
                    cycles += 11 - (1 + spx0);
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
        self.window_y_triggered = false;
        self.window_started_this_line = false;
        self.mode2_irq_pretriggered_for_next_line = false;
        self.first_line_after_enable = false;
        self.line_153_ly_zeroed = false;
        self.mode0_pretriggered_this_line = false;
        self.m3_pixels_discarded = 0;
        self.scheduled_mode0_dot = None;
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
    pub fn on_stat_register_write(&mut self, mmio: &mut mmio::Mmio) {
        // Keep the LYC=LY readback flag (FF41 bit 2) in sync regardless of LCD
        // state; only its IRQ side-effects are gated by enable.
        if self.disabled {
            self.previous_stat_interrupt_line = false;
            // STAT-write quirk (memory.cpp case 0x41): with the LCD off, an FF41
            // write that newly enables LYC IRQ (0->1) while the LYC=LY flag is
            // set flags a STAT IRQ.
            let live_stat = mmio.read(LCD_STATUS);
            let new_stat = live_stat & 0x78;
            let old_stat = self.stat_reg_committed & 0x78;
            let lycflag = live_stat & 0x04 != 0;
            let old_lycen = old_stat & stat_irq::STAT_LYCEN != 0;
            let new_lycen = new_stat & stat_irq::STAT_LYCEN != 0;
            if lycflag && !old_lycen && new_lycen {
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

        // FF41 (STAT) write.
        if new_stat != old_stat {
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
        self.sched_m2irq = if m2 == stat_irq::DISABLED_TIME { m2 } else { (m2 as i64 + Self::m2_off()) as u64 };
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
    fn write_cc(&self, ds: bool) -> u64 {
        let off = if ds { WRITE_CC_OFFSET_DS } else { WRITE_CC_OFFSET };
        (self.abs_cc as i64 + off).max(0) as u64
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
        Self::set_lcd_status_mode(mmio, 2);
        // IRQ delivery is handled by the event model; just latch the line.
        self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
        self.mode2_irq_pretriggered_for_next_line = false;
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
                Self::set_lcd_status_mode(mmio, 0);
                self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
                self.check_and_trigger_stat_interrupt(mmio);
                // Initialize the event-scheduled IRQ clock at enable: LY=0,
                // line_cycle=0. Mirror Gambatte's lcdcChange enable branch.
                self.line_cycle = 0;
                self.internal_ly_val = 0;
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
        self.abs_cc += 1 << mmio.is_double_speed_mode() as u32;
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
        match self.state {
            State::OAMSearch => {
                // Check WY condition at the start of Mode 2 (OAMSearch)
                if self.ticks == 0 {
                    let ly = mmio.read(LY);
                    let wy = mmio.read(WY);
                    if ly == wy {
                        self.window_y_triggered = true;
                        // Reset window line counter when window first becomes active
                        self.window_line_counter = 0;
                    }
                    
                    // If window is already active and enabled, increment the window line counter
                    let lcdc = self.lcdc;
                    let window_enabled = (lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
                    if window_enabled && self.window_y_triggered && ly > wy {
                        self.window_line_counter = self.window_line_counter.wrapping_add(1);
                    }
                    
                    // Reset window line flag for new scanline
                    self.window_started_this_line = false;
                    
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
                    let lock_dot = if is_cgb { CGB_FIRST_FRAME_LOCK_DOT } else { DMG_FIRST_FRAME_LOCK_DOT };
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
                    // First scanline after enable is now armed; subsequent
                    // lines use normal Mode 2 timing.
                    let was_first_line = self.first_line_after_enable;
                    self.first_line_after_enable = false;
                    self.mode0_pretriggered_this_line = false;
                    // SCX fine-scroll discard is done per-dot at the start of
                    // Mode 3 (see `m3_pixels_discarded`), re-reading SCX live.
                    self.m3_pixels_discarded = 0;
                    self.check_and_trigger_stat_interrupt(mmio);

                    if was_first_line {
                        self.scheduled_mode0_dot = None;
                    } else {
                        // Closed-form mode-0 schedule, including window-start lines
                        // (compute_m3_length applies the window penalty). Mid-mode-3
                        // window-enable toggles (set_lcdc_visible) and WX changes
                        // (PixelTransfer) invalidate it, falling back to the live
                        // emergent x==160 transition.
                        let m3_len = self.compute_m3_length(mmio, is_cgb);
                        let offset = if is_cgb { CGB_MODE0_OFFSET } else { DMG_MODE0_OFFSET };
                        let dot = self.ticks as i64 + m3_len as i64 + offset as i64;
                        self.scheduled_mode0_dot = Some(dot.max(0) as u128);
                        self.m3_scheduled_wx = mmio.read(WX);
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
                    && mmio.read(WX) != self.m3_scheduled_wx
                {
                    self.scheduled_mode0_dot = None;
                }
                // Scheduled cycle-exact Mode 3 -> Mode 0 transition.
                if self.scheduled_mode0_dot == Some(self.ticks) {
                    self.scheduled_mode0_dot = None;
                    self.state = State::HBlank;
                    Self::set_lcd_status_mode(mmio, 0);
                    self.check_and_trigger_stat_interrupt(mmio);
                    break 'label;
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
                if cadence_even
                    && let Some(event) = self.fetcher.step(mmio, self.window_line_counter, fetcher_lcdc_state) {
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
                let lcdc = self.lcdc;
                let window_enabled = (lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
                if window_enabled && self.window_y_triggered && !self.fetcher.is_fetching_window() {
                    let wx = mmio.read(WX);
                    // WX=0-6 can trigger immediately, WX=7+ needs exact match with X+7
                    let should_start_window = if wx < 7 {
                        self.x == 0  // Start immediately if WX is 0-6
                    } else {
                        self.x + 7 == wx
                    };
                    
                    if should_start_window {
                        // Start window rendering
                        self.fetcher.start_window(self.x);
                        self.window_started_this_line = true;
                        break 'label; // Skip this cycle to let window fetching start
                    }
                }

                // SCX fine-scroll discard (Gambatte M3Start::f1 per-dot loop):
                // while x == 0, re-read the LIVE SCX each dot. If we have not
                // yet discarded `scx % 8` BG pixels, pop one and consume the
                // dot. A mid-M3 SCX write changes this count (and the fetched
                // tile column, since TileNumber re-reads SCX live).
                if self.x == 0 {
                    let scx_low3 = mmio.read(SCX) & 0x07;
                    if self.m3_pixels_discarded < scx_low3
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
                if let Ok(bg_pixel) = self.fetcher.pixel_fifo.pop() {
                    let bg_pixel_idx = bg_pixel.color;
                    let bg_attrs = bg_pixel.attrs;
                    let ly = mmio.read(LY) as u16;
                    let fb_offset = (ly * 160) + self.x as u16;

                    if mmio.is_cgb_features_enabled() {
                        // CGB mode: write to color framebuffer with proper sprite mixing
                        let final_color_rgb = self.mix_background_and_sprites_color(mmio, bg_pixel_idx, bg_attrs, self.x, ly as u8);
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
                        // DMG mode: write to monochrome framebuffer
                        let final_color = self.mix_background_and_sprites(mmio, bg_pixel_idx, self.x, ly as u8);
                        let intensity = match final_color {
                            0 => 255,
                            1 => 170,
                            2 => 85,
                            _ => 0,
                        };
                        self.record_pixel_debug_event(
                            ly as u8,
                            bg_pixel_idx,
                            [intensity, intensity, intensity],
                        );
                        self.fb_a[fb_offset as usize] = final_color;
                    }

                    self.x += 1;
                    // When no cycle-exact dot is scheduled (window-start lines),
                    // fall back to ending Mode 3 at the x==160 pixel push.
                    if self.scheduled_mode0_dot.is_none() && self.x == 160 {
                        self.state = State::HBlank;
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
                if !self.line_153_ly_zeroed
                    && self.ticks == LINE_153_LY_ZERO_DOT
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
        Some(dot >= m0 && dot + 3 + 3 * ds < 456)
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
        
        // Check if sprites are enabled
        if (lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) == 0 {
            return;
        }
        
        // Determine sprite height (8x8 or 8x16)
        let sprite_height = if (lcdc & (LCDCFlags::SpriteSize as u8)) != 0 { 16 } else { 8 };
        
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

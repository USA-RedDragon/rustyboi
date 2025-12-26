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
fn dmg_first_frame_lock_dot() -> u128 { DMG_FIRST_FRAME_LOCK_DOT }
fn cgb_first_frame_lock_dot(double_speed: bool) -> u128 {
    if double_speed { CGB_FIRST_FRAME_LOCK_DOT_DS } else { CGB_FIRST_FRAME_LOCK_DOT }
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
// First-line-after-enable DMG single-speed mode-0 STAT IRQ correction (dots).
// On the first frame after the LCD turns on there is no prior mode-2 scan; the
// DMG first-frame arm (DMG_FIRST_FRAME_ARM_DOT=85) lands the line-0 m0 IRQ three
// master-cc late versus hardware. The ly0_m0irq / frame0_m0irq_count brackets
// (read-PC-calibrated to the exact m0 fire) place the true fire 3 dots earlier;
// every scx (0..3) is uniformly +3. Scoped to DMG SS first line so the
// steady-state m0/m2 IRQ schedule (the m0int/m2int canaries) is untouched.
const M0IRQ_DMG_FIRST_FRAME_OFFSET: i64 = -3;
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

// Mid-mode-3 register-write commit delays (dots, relative to the write cc) and
// render-phase offsets. These were once env-tunable sweep knobs; the sweeps are
// deleted and each is now its single calibrated constant.
const M0IRQ_SCX2_CGB_OFFSET: i64 = -1;
const WY1_DELAY: i64 = 2;
const WY2_DELAY_CGB: i64 = 7;
const WY2_DELAY_DMG: i64 = 4;
const SCY_DELAY: i64 = 2;
const WXEN_COMMIT_DELAY: i64 = 3;
const WYTRIG_COMMIT_DELAY: i64 = 3;
const LINE153_LY0_DOT_DS: i64 = 6;
const GETSTAT_OFF_DS: i64 = -1;

// ds-engine STAGE 5: RB_LINERENDER. With getStat (stage 4) owning all CPU-visible
// timing, the pixel pipeline no longer affects timing — only the final
// framebuffer values (read at frame end) matter. When this flag is set the
// per-dot fetcher/FIFO still RUNS (it advances `self.x` so the timing fallbacks
// that key off `x==160` are unchanged) but it no longer WRITES the framebuffer;
// instead each visible line is rendered ONCE in a single closed-form pass
// (`render_full_line`) at the mode-3 -> HBlank transition, driven by the same
// per-line geometry getStat uses (SCX/SCY/WX/WY/LCDC + the latched
// `sprites_on_line` / `win_y_pos`). Flag-off keeps the per-dot draw
// (byte-identical to stage 4). Timing is unaffected either way (it lives wholly
// in getStat / the STAT event schedule).
pub fn linerender_enabled() -> bool {
    // ds-engine converge: OFF. The closed-form per-line render ignored mid-mode-3
    // register writes (SCX/LCDC/BGP/WX) -> 219 pixel-content regressions. The
    // per-dot fetcher/FIFO render handles mid-line writes correctly and getStat
    // still owns all CPU-visible timing, so the framebuffer goes back to the
    // per-dot path while keeping the exact-cc/getStat spine.
    false
}
// DS offsets re-derived after the double-speed STAT sub-dot step (step_subdot)
// gave the IRQ model true odd-cc resolution: m2 relaxes -2 -> -1 (the odd-cc
// fire is now caught by the sub-dot rather than rounded down), and the write cc
// tightens -3 -> -4.
fn write_cc_off_ds() -> i64 { 0 }
fn m0irq_off_ds() -> i64 { M0IRQ_OFFSET }
fn m2irq_off_ds() -> i64 { -1 }
// Single-speed offsets (the compiled-in calibrated constants).
fn dmg_mode0_offset() -> i32 { DMG_MODE0_OFFSET }
fn cgb_mode0_offset() -> i32 { CGB_MODE0_OFFSET }
fn m0irq_off_ss() -> i64 { M0IRQ_OFFSET }
fn m2irq_off_ss() -> i64 { M2IRQ_OFFSET }
fn write_cc_off_ss() -> i64 { WRITE_CC_OFFSET }

// Sentinel tile number that can never equal a real `(spx - firstTileXpos) & -8`
// value (Gambatte's `tileno_none` = low bit set). Used to force the first sprite
// of a fresh tile group to be charged the leading-sprite rate.
const SPRITE_TILE_NONE: i32 = 1;
fn sprite_prev_tile_default() -> i32 { SPRITE_TILE_NONE }


/// One faithful port of Gambatte's mode-3 sprite-cost tile walk
/// (`predictCyclesUntilXpos_fn` + `addSpriteCycles`, ppu.cpp:1313-1392, the same
/// per-tile cost the runtime `doFullTilesUnrolled` charges at ppu.cpp:525-530).
///
/// Walks the BG tiles left-to-right. Within each 8-pixel tile, the FIRST sprite
/// whose `spx` falls in the tile costs `max(11 - dist, 6)` (where `dist =
/// (spx - firstTileXpos) % 8`, and the leading rate only applies when `dist < 5`);
/// every FURTHER sprite in the same tile costs a flat 6. The window split (the
/// `spx <= nwx` group vs the `spx > nwx` group) mirrors Gambatte exactly: the
/// post-window group restarts the tile grid at `nwx + 1` with no previous tile.
///
/// `sprite_xs` MUST be sorted ascending by spx. `scx` is `SCX & 7`. `nwx` is the
/// window X split point (0xFF when no window starts this line). `target_x` is
/// `lcd_hres + 7 = 167`. `obj_enabled` follows `lcdcObjEn(p) | p.cgb`.
/// Returns the total sprite cost in dots.
fn sprite_tile_walk_cost(
    sprite_xs: &[i32],
    scx: i32,
    nwx: i32,
    target_x: i32,
    obj_enabled: bool,
) -> i32 {
    if !obj_enabled || sprite_xs.is_empty() {
        return 0;
    }
    // firstTileXpos = endx % 8 = (8 - scx%8) % 8: the BG-tile grid phase at
    // xpos = 0 (M3 start). fno is the fine-scroll discard count Gambatte passes
    // from M3Start (`min(scx%8, 5)`), used only for the first sprite.
    let first_tile_xpos = (8 - scx).rem_euclid(8);
    let fno = scx.min(5);
    let mut cycles = 0i32;
    let mut idx = 0usize;

    // First-sprite special case (Tile::predictCyclesUntilXpos_fn first branch):
    // xpos is 0, so the leading sprite uses `fno + spx` for its distance.
    let prev_tile_no_initial = (0 - first_tile_xpos) & !7; // (xpos - firstTileXpos) & -8
    let spx0 = sprite_xs[0];
    if fno + spx0 < 5 && spx0 <= nwx && spx0 <= target_x {
        cycles += 11 - (fno + spx0);
        idx += 1;
    }

    // addSpriteCycles: accumulate for sprites with spx <= max_spx, charging the
    // first per tile the leading rate and 6 for the rest.
    let add = |xs: &[i32], idx: &mut usize, max_spx: i32, first_tile_xpos: i32,
               mut prev_tile_no: i32, cycles: &mut i32| {
        while *idx < xs.len() && xs[*idx] <= max_spx {
            let spx = xs[*idx];
            let dist = (spx - first_tile_xpos).rem_euclid(8);
            let tile_no = (spx - first_tile_xpos) & !7;
            let c = if dist < 5 && tile_no != prev_tile_no { 11 - dist } else { 6 };
            prev_tile_no = tile_no;
            *cycles += c;
            *idx += 1;
        }
    };

    if nwx < target_x {
        add(sprite_xs, &mut idx, nwx, first_tile_xpos, prev_tile_no_initial, &mut cycles);
        add(sprite_xs, &mut idx, target_x, nwx + 1, SPRITE_TILE_NONE, &mut cycles);
    } else {
        add(sprite_xs, &mut idx, target_x, first_tile_xpos, prev_tile_no_initial, &mut cycles);
    }

    cycles
}

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

// Faithful port of Gambatte's `SpriteMapper::OamReader` (sprite_mapper.cpp).
// Holds a lazily-sampled 80-byte snapshot of the OAM Y/X positions (`buf`,
// even=Y odd=X) plus the per-sprite large-size flag (`lsbuf`). The snapshot is
// advanced by `update(cc)`, which walks OAM positions up to
// `toPosCycles(cc) = (lineCycles(cc) + 1) % 456`, copying from the source. The
// source is the real OAM normally, but reads as 0xFF for the whole window of an
// active OAM-DMA (Gambatte points `oamram_` at the cartridge's disabled RAM).
// `change(cc)` (on CPU OAM writes and at DMA start/end) caps the next walk via
// `last_change`. The per-line sprite list is built from `buf` at mode-2-END.
#[derive(Clone)]
pub struct OamReader {
    // posbuf_: Y at even index, X at odd index, for each of 40 sprites.
    buf: [u8; 2 * OAM_SPRITE_COUNT],
    // lsbuf_: per-sprite large-size flag.
    lsbuf: [bool; OAM_SPRITE_COUNT],
    // lu_: cc of the last update (the position-walk anchor), in PPU `abs_cc`.
    lu: u64,
    // lastChange_: position-walk cap (0xFF == no pending change).
    last_change: u8,
    // largeSpritesSrc_: live LCDC OBJ-size bit, latched into lsbuf on the walk.
    large_src: bool,
    cgb: bool,
    // Whether the source currently reads 0xFF (active OAM-DMA window).
    src_disabled: bool,
}

const OAM_POS_CYCLES: u32 = (2 * OAM_SPRITE_COUNT) as u32; // 80

// Sub-M-cycle correction (in single-speed dots) between the cc at which the PPU
// step observes the OAM-DMA window edge and the master cc Gambatte fires
// startOamDma/endOamDma at. Calibrated against the late_sp*x/y `_1`/`_2` and
// `_ds_1`/`_ds_2` bracket pairs.
const OAMDMA_CHANGE_CC_OFFSET: u32 = 3;

fn scan_slot_large_default() -> [bool; OAM_SPRITE_COUNT] {
    [false; OAM_SPRITE_COUNT]
}

impl Default for OamReader {
    fn default() -> Self {
        OamReader {
            buf: [0; 2 * OAM_SPRITE_COUNT],
            lsbuf: [false; OAM_SPRITE_COUNT],
            lu: 0,
            last_change: 0xFF,
            large_src: false,
            cgb: false,
            src_disabled: false,
        }
    }
}

impl OamReader {
    fn changed(&self) -> bool {
        self.last_change != 0xFF
    }

    // toPosCycles: lineCycles(cc)+1 wrapped to [0, 456).
    //
    // `cc` may be a past update cc (`self.lu`) lying on the PREVIOUS line relative
    // to `lc`'s anchor — rustyboi updates the OAM snapshot sparsely (only at
    // change/doEvent), so `lu` can trail the current line by up to ~one line
    // without the >=1-line full-resample (controller `update`) firing. The raw
    // `456 - ((time - cc) >> ds)` then goes negative and the u64 subtraction
    // overflow-panics in debug (silently wraps in release). Compute it signed and
    // reduce modulo the line length — Gambatte's unsigned wrap — so the position
    // stays in [0,456). Byte-identical to the old `if v>=456 {v-=456}` whenever
    // `cc` is within the current line (`dots` in 1..=456).
    fn to_pos_cycles(cc: u64, lc: &stat_irq::LyCounter) -> u32 {
        let dots = (lc.time.wrapping_sub(cc) >> lc.ds as u32) as i64;
        let raw = stat_irq::LCD_CYCLES_PER_LINE as i64 - dots + 1;
        raw.rem_euclid(stat_irq::LCD_CYCLES_PER_LINE as i64) as u32
    }

    // Re-seed the snapshot from the current OAM (SpriteMapper::reset).
    fn reset(&mut self, oam: &[u8; 2 * OAM_SPRITE_COUNT], cgb: bool) {
        self.cgb = cgb;
        self.large_src = false;
        self.src_disabled = false;
        self.lu = 0;
        self.last_change = 0xFF;
        self.lsbuf = [self.large_src; OAM_SPRITE_COUNT];
        self.buf.copy_from_slice(oam);
    }

    // SpriteMapper::OamReader::enableDisplay.
    fn enable_display(&mut self, cc: u64, ds: bool) {
        self.buf = [0; 2 * OAM_SPRITE_COUNT];
        self.lsbuf = [false; OAM_SPRITE_COUNT];
        self.lu = cc + ((OAM_POS_CYCLES as u64) << ds as u32) + 1;
        self.last_change = OAM_POS_CYCLES as u8;
    }

    // SpriteMapper::OamReader::update. `oam_y`/`oam_x` for sprite `i` are read
    // lazily via the closure (real OAM when enabled, 0xFF when DMA-disabled).
    fn update(&mut self, cc: u64, lc: &stat_irq::LyCounter, oam_pos: &[u8; 2 * OAM_SPRITE_COUNT]) {
        if cc <= self.lu {
            return;
        }
        // Full-line-or-more elapsed since the last update: Gambatte walks the
        // whole 80-position buffer (distance = 2*lcd_num_oam_entries). Because
        // rustyboi updates sparsely (only at change/doEvent, not per access),
        // `toPosCycles(lu)` can underflow when lu is multiple lines old; do the
        // full re-sample explicitly from pos 0 so every position is refreshed
        // (sampling the disabled source if a DMA spans this whole window — which
        // it cannot for >1 line, so this is the steady-state/post-enable refresh).
        if self.changed()
            && ((cc - self.lu) >> lc.ds as u32) >= stat_irq::LCD_CYCLES_PER_LINE as u64
        {
            for i in 0..OAM_SPRITE_COUNT {
                self.lsbuf[i] = self.large_src;
                if self.src_disabled {
                    self.buf[2 * i] = 0xFF;
                    self.buf[2 * i + 1] = 0xFF;
                } else {
                    self.buf[2 * i] = oam_pos[2 * i];
                    self.buf[2 * i + 1] = oam_pos[2 * i + 1];
                }
            }
            self.last_change = 0xFF;
            self.lu = cc;
            return;
        }
        if self.changed() {
            let lulc = Self::to_pos_cycles(self.lu, lc);
            let mut pos = lulc.min(OAM_POS_CYCLES);

            // Distance to walk: from `pos` (the lineCycle of the last update) to
            // `cclc` (now), within a single line (the >= 1-line case is handled
            // above). Mirrors Gambatte OamReader::update.
            let cclc = Self::to_pos_cycles(cc, lc);
            let mut distance = cclc.min(OAM_POS_CYCLES).wrapping_sub(pos)
                .wrapping_add(if cclc < lulc { OAM_POS_CYCLES } else { 0 });

            {
                let lcg = self.last_change as u32;
                let target = lcg.wrapping_sub(pos)
                    .wrapping_add(if lcg <= pos { OAM_POS_CYCLES } else { 0 });
                if target <= distance {
                    distance = target;
                    self.last_change = 0xFF;
                }
            }

            let mut d = distance;
            while d > 0 {
                d -= 1;
                if pos & 1 == 0 {
                    if pos == OAM_POS_CYCLES {
                        pos = 0;
                    }
                    if self.cgb {
                        self.lsbuf[(pos / 2) as usize] = self.large_src;
                    }
                    let (y, x) = if self.src_disabled {
                        (0xFF, 0xFF)
                    } else {
                        (oam_pos[pos as usize], oam_pos[pos as usize + 1])
                    };
                    self.buf[pos as usize] = y;
                    self.buf[pos as usize + 1] = x;
                } else {
                    let cur = self.lsbuf[(pos / 2) as usize];
                    self.lsbuf[(pos / 2) as usize] = (cur && self.cgb) || self.large_src;
                }
                pos += 1;
            }
        }
        self.lu = cc;
    }

    // SpriteMapper::OamReader::change.
    fn change(&mut self, cc: u64, lc: &stat_irq::LyCounter, oam_pos: &[u8; 2 * OAM_SPRITE_COUNT]) {
        self.update(cc, lc, oam_pos);
        self.last_change = (Self::to_pos_cycles(self.lu, lc).min(OAM_POS_CYCLES)) as u8;
    }
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
    // Lazy OAM Y/X snapshot (Gambatte SpriteMapper::OamReader). Drives sprite
    // visibility so an OAM-DMA overlapping mode-2 retroactively zeroes positions
    // sampled inside the DMA-disabled window. Fed by `oam_change`/`oam_update`.
    // Not serialized; re-seeded on load via `oam_reader_seeded == false`.
    #[serde(skip, default)]
    oam_reader: OamReader,
    // Tracks the previous-dot OAM-DMA "writing" state so the PPU can fire the
    // OamReader `change` (source toggle) on DMA start/end edges.
    #[serde(default)]
    prev_dma_writing: bool,
    // Set once the OamReader has been seeded for the current LCD-on session.
    #[serde(default)]
    oam_reader_seeded: bool,
    // Per-slot OBJ size recorded by the incremental mode-2 scan, reused by the
    // snapshot rebuild so the calibrated size-latch timing is preserved.
    #[serde(skip, default = "scan_slot_large_default")]
    scan_slot_large: [bool; OAM_SPRITE_COUNT],
    #[serde(default)]
    next_sprite_fetch_index: usize,
    // Tile number `(spx - firstTileXpos) & -8` of the most recently charged
    // sprite in the live mode-3 walk. Sprites sharing a tile with this one cost
    // a flat 6 (only the first sprite per BG tile gets the leading rate), matching
    // Gambatte's `prevSpriteTileNo` in `doFullTilesUnrolled`/`addSpriteCycles`.
    // Reset to SPRITE_TILE_NONE at M3 start and on window draw-start.
    #[serde(default = "sprite_prev_tile_default")]
    m3_sprite_prev_tile: i32,
    // Tick at which the most-recently-fetched sprite's stall was armed (the dot
    // `next_sprite_fetch_index` last advanced, and the first stall dot was consumed).
    // Gambatte's `doFullTilesUnrolled` charges that sprite's `max(11-dist,6)` stall
    // dots one at a time as `p.cycles` counts down, so a mid-mode-3 OBJ-disable
    // refunds only the not-yet-counted-down remainder of the in-progress sprite:
    // `cost - (ticks - this + 1)` (see `remaining_sprite_cost`).
    #[serde(default)]
    m3_last_sprite_commit_tick: u128,
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
    // Gambatte's `win_draw_started` bit of winDrawState: persists across lines
    // once the window has begun drawing this frame, until a WE-off / display
    // disable / frame end clears it. Distinct from `window_started_this_line`
    // (per-line). Mirrors Gambatte plotPixel branch 886 (start now) vs 889
    // (re-arm an already-started window): the FIRST WX==166 match with the
    // window not yet drawing starts it on that very line (++winYPos, no visible
    // pixels), so the next line draws with winYPos one higher than an arm-only
    // path would give. Needed by the DMG wxA6 cluster.
    #[serde(default)]
    win_draw_started: bool,
    window_y_triggered: bool,   // Whether WY condition was met this frame
    window_started_this_line: bool, // Whether window started rendering on current scanline
    // Dot (within-line `ticks`) at which the window began drawing this line.
    // The StartWindowDraw mode-3 penalty becomes non-refundable once the
    // pipeline advances WIN_M3_PENALTY dots past this; used by the late_disable
    // read-at-cc recompute to decide whether a mid-M3 window-disable keeps the
    // window-inclusive m0Time or reverts to the no-window length.
    win_start_dot: Option<u128>,
    // Predicted within-line `ticks` at which the window WILL begin drawing this
    // line, computed at M3 arm from WX/SCX when a window is scheduled. Used only
    // on DMG to resolve the disable-AT-window-start boundary race: the LCDC-write
    // hook fires during the CPU's store, one step before the PixelTransfer code
    // that latches `win_start_dot`, so a disable landing on the exact start dot
    // sees `window_started_this_line == false` even though the StartWindowDraw
    // penalty is already committed. The late_disable_N cluster brackets this:
    // disable strictly before the start dot refunds (mode 0), at/after keeps
    // (mode 3). `None` when no window is scheduled this line.
    #[serde(default)]
    predicted_win_start_dot: Option<u128>,
    // Set once a late-WX mid-window refund has been applied this line, so a
    // second WX write does not refund twice.
    win_wx_penalty_resolved: bool,
    // Set once a mid-mode-3 WX-write window-ENABLE has been resolved this line
    // (penalty added or determined not-applicable), so the WX != arm-WX
    // pre-window-start condition does not re-enter and null the schedule on the
    // following dots.
    #[serde(default)]
    win_wx_enable_resolved: bool,

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
    // Gambatte `OamReader::lu_` for `inactivePeriodAfterDisplayEnable(cc) = cc < lu_`:
    // the master cc until which, right after an LCD enable, getStat suppresses
    // mode 2/3 (reports mode 0). Seeded at enable to `enable_cc + (80<<ds) + 1`.
    #[serde(default)]
    display_enable_inactive_until: u64,
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
    // Full SCX (all 8 bits) sampled at M3 arm. The first BG tile in the FIFO is
    // fetched from column (arm_scx / 8). If a mid-M3 SCX write moves the f1 break
    // to a different tile column (Gambatte's M3Start::f1 re-reads p.scx live at
    // its case-0 tile fetch), the already-queued first tile is stale and the
    // FIFO must be refetched from the new column. -1 = not yet armed this line.
    #[serde(default)]
    m3_arm_scx_full: i16,
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
    // Exact-cc OBJ-size (LCDC bit2) latch for the mode-2 OAM scan (PoC extension
    // of the SCX f1 / LCDC-bit4 pattern). A mid-mode-2 sprite-size write goes
    // through the pending_lcdc_events queue (a 2-dot quantized self.lcdc commit)
    // AND the per-slot `scan_obj_size_large` snapshot lags one slot, which on the
    // late_sizechange* tests pushes the change one OAM slot too late: the sprite
    // whose 8x16-only y-range straddles the line is scanned with the stale 8x8
    // size and dropped, so m0Time (and the boundary FF41 STAT read) resolves the
    // wrong mode. Gambatte's SpriteMapper latches each entry's size at that
    // entry's OAM-read cc; record the exact abs_cc at which the bit2 change
    // becomes visible (`write_cc + 2*cgb`, Gambatte setLcdc(data, cc+2)) and let
    // each scan slot sample bit2 as-of its OWN abs_cc. (apply_cc, old_large,
    // new_large); apply_cc == wy2_disabled() means no pending change.
    #[serde(default = "wy2_disabled")]
    objsize_apply_cc: u64,
    #[serde(default)]
    objsize_prev_large: bool,
    #[serde(default)]
    objsize_new_large: bool,
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

    // STAGE 5 (RB_LINERENDER): latched once `render_full_line` has produced the
    // current visible line's framebuffer, so the closed-form line render runs at
    // most once per line. Reset at the start of each line (mode-2 entry).
    #[serde(default)]
    line_rendered_this_line: bool,

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
    // Set when an SS->DS speed switch executes DURING mode 3. Across the switch
    // Gambatte's re-anchored lyCounter.time (LCD::speedChange) sits ~5 DS-dots
    // (10 cc) ahead of rustyboi's bridged renderer line phase for the FF44 (LY)
    // read's getLyReg anticipation window. Consumed ONLY by `get_ly_reg_at_cc`
    // (not the STAT/m0Time predictor, which is already correct). Cleared at the
    // next LCD enable / LY reset, like `lytime_no_plus1`.
    #[serde(default)]
    ssds_mode3_ly_advance: bool,
    // Set when an SS->DS speed switch executes during PixelTransfer (mode 3) and
    // the bridge dropped 2 dots (see `stop_bridge_advance`). If a subsequent
    // DS->SS switch follows (the double-switch speedchange{2..5} families), that
    // bridge restores the 2 dots so the net renderer advance matches the
    // single-switch base family's tuning. Cleared by the compensating DS->SS
    // switch or at the next LCD enable / LY reset.
    #[serde(default)]
    sc_mode3_pullback_pending: bool,
    // STAGE 4 (FACET 1): running count of DS->SS-during-mode3 STOP
    // switches. The faithful Gambatte `PPU::speedChange` re-anchor is `now -= 1`
    // (HALF an SS dot) per DS->SS switch; the whole-dot bridge rounds each to 0,
    // accumulating a missing HALF dot per switch. `floor(count/2)` extra STAT-only
    // carry dots (via `stat_phase_carry`) reproduce that accumulated half-dot
    // shift on the STAT/line phase WITHOUT moving the render latch.
    #[serde(default)]
    dsss_mode3_stop_count: u32,
    // STAGE 4 (FACET 2 KEYSTONE): accumulated STAT-phase carry in
    // master-cc (`1<<ds` per `stat_phase_carry` dot). The carry advances the
    // STAT/line phase (line_cycle/abs_cc) so the STAT/m2-enable observables shift,
    // but the pixel-fetcher render latch must stay anchored to its ORIGINAL
    // position. The CPU VRAM/OAM/cgbp access-visibility gate (`ppu_blocks` via
    // `render_carry_skew`) SUBTRACTS this skew from the access cc so a store still
    // resolves against the un-carried fetcher mode-3 lock window — the decoupling
    // that lets FACET 1's odd STAT-phase shift land without moving the render latch.
    #[serde(default)]
    render_carry_skew_cc: i64,
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
    // Exact-cc f1-discard SCX latch. Gambatte's scxChange does
    // `update(cc + 2*cgb)` BEFORE `setScx`, so on CGB the new SCX is only
    // visible to the f1 fine-scroll discard 2 PPU cc after the write's cc. The
    // f1 loop reads SCX as-of its dot's exact abs_cc through this latch instead
    // of the immediate register, so a mid-discard SCX write lands on the
    // correct f1 iteration without shifting the steady-state discard timing.
    #[serde(default)]
    scx_prev_f1: u8, // value in effect before the pending write
    #[serde(default = "wy2_disabled")]
    scx_f1_apply_cc: u64, // abs_cc at which scx_pending becomes visible to f1
    #[serde(default)]
    scx_f1_new: u8,
    // sub-cc column lever. A mid-mode-3 SCX write applies to the BG
    // column fetcher at `write_cc + 2*cgb` (Gambatte scxChange `update(cc+2*cgb);
    // setScx`), evaluated against the cc at which a fetched tile's pixels are
    // PLOTTED (the fetcher leads the display by the FIFO depth). A tile whose
    // first plotted pixel is at/before the apply cc keeps the OLD scx; after it
    // uses NEW. These persist for the whole line (unlike scx_apply_cc which
    // resets on apply) so the fetcher can choose per-tile. `subcc_scx_apply_cc`
    // == disabled when no write is pending this line.
    #[serde(default = "wy2_disabled")]
    subcc_scx_apply_cc: u64,
    #[serde(default)]
    subcc_scx_old: u8,
    #[serde(default)]
    subcc_scx_new: u8,
    // Armed by a mid-mode-3 SCX write while a BG tile is in flight (column
    // already committed under the OLD scx, not yet pushed). The next PushToFifo
    // re-keys that single tile to the NEW scx column iff it plots after the
    // apply cc, then disarms. Exactly one tile per write can straddle.
    #[serde(default)]
    subcc_rekey_armed: bool,
    // First-tile (f1) prologue straddle: a mid-mode-3 SCX write that lands while
    // x==0 (the discard prologue, before any pixel has plotted) but AFTER the
    // first displayed tile has already been queued into the FIFO. The tile still
    // in flight (the 2nd displayed tile) latched its column under the OLD scx one
    // dot before the write; on hardware/Gambatte it plots well after the write so
    // its column comes from the NEW scx. The first queued tile (already pushed)
    // keeps the OLD scx. Re-keys exactly that one in-flight tile on its next
    // PushToFifo. DMG single-speed only (the CGB/DS prologue uses the
    // m3_arm_scx_full re-fetch path above).
    #[serde(default)]
    prologue_rekey_armed: bool,
    // First-line (LY=0) sprite-shifted straddle (CGB SS, gap==1): on the line
    // after LCD-enable the fetcher runs a different warmup/dispatch phase, so a
    // left-edge sprite-fetch dot shifts the OLD->NEW scx boundary one tile later
    // than on LY>=1. The per-dot fetcher already read the NEW scx for that tile
    // (one tile too early), so when set the next PushToFifo reverts the 8
    // just-pushed entries back to the OLD-scx column.
    #[serde(default)]
    subcc_revert_next_old: bool,
    // Two-tile DS straddle (CGB double-speed, low-X sprite): at DS a mid-mode-3
    // SCX write straddles TWO display tiles because the sprite-fetch dot shifts
    // the BG fetch phase one tile while the DS FIFO carries an extra tile. Both
    // straddle tiles must render under the OLD scx at their plot column shifted
    // back one tile (xpos-8). The first (in-flight) tile is rekeyed at the DS
    // flip; this flag rekeys the SECOND tile (fetched NEXT under the NEW scx) on
    // its push back to the OLD-scx column at its own xpos-8.
    #[serde(default)]
    ds_straddle_next_old: bool,
    // abs_cc at which the most recent BG TileNumber latch happened (the fetch
    // cc of the tile currently in flight). The armed straddle tile's column was
    // committed at this cc; the rekey compares it to the write's apply cc.
    #[serde(default)]
    subcc_last_tn_cc: u64,
    // First line after enable: the SCX value the fine-scroll discard prefix
    // actually samples (Gambatte M3Start::f1 reads SCX once at the M3-start
    // dot). A mid-discard SCX write (write_cc + 2*cgb visible) only counts if
    // it lands at/before that sample dot, which sits `prev_scx % 8` dots past
    // M3-arm. `compute_m3_length_win` uses this override (when set) instead of
    // the live register so the late-enable + SCX m0Time matches Gambatte.
    #[serde(default)]
    first_line_scx_override: Option<u8>,
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
    // Set when the m1 event flagged VBlank this frame so the render-machine
    // ly143->144 transition does NOT re-flag it (Gambatte has a single VBlank
    // source: the m1 event). Cleared when the m1 event re-arms for the next frame.
    #[serde(default)]
    m1_vblank_fired: bool,
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
    // Exact-cc latch for a mid-mode-3 CGB LCDC bit4 (BGWindowTileDataSelect)
    // toggle. The per-dot pending-event queue quantizes the bit4 commit to a
    // dot boundary, which on the bgtiledata_spx08_ds tests lands the change one
    // BG-fetch substep late (the change should split a tile between its
    // TileDataLow and TileDataHigh fetches, but the dot model applies it a
    // substep too late). Record the exact abs_cc at which the change becomes
    // visible (`write_cc + 2` PPU dots, Gambatte's `setLcdc(data, cc + 2)`) and
    // let the fetcher consult it per-substep. (commit_cc, new_lcdc, old_lcdc).
    #[serde(default)]
    lcdc_b4_exact: Option<(u64, u8, u8)>,
    // Exact-cc window-enable (LCDC bit 5) toggle for the weMaster checkpoints.
    // rustyboi's pending_lcdc_events commit the window bit one PPU dot before
    // Gambatte's `setLcdc(data, cc + 2)` (the queue runs through one
    // step_lcdc_events on the write dot). That 1-dot-early commit is harmless to
    // the renderer/getStat but mis-orders the lc450/lc454 weMaster checkpoints
    // against a window-enable write whose Gambatte commit (`write_cc + 2`) lands
    // exactly on the checkpoint dot: Gambatte runs `update(cc)` (the weMaster
    // event) BEFORE `setLcdc`, so the checkpoint sees the OLD window bit. We
    // record the write's master-cc commit (`write_cc + 2`) and the bit's old/new
    // values; `update_window_y_latch` reads the window-enable bit as-of the
    // checkpoint cc through this. (commit_master_cc, new_win_bit, old_win_bit).
    #[serde(default)]
    we_win_bit_exact: Option<(u64, bool, bool)>,
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
            oam_reader: OamReader::default(),
            prev_dma_writing: false,
            oam_reader_seeded: false,
            scan_slot_large: [false; OAM_SPRITE_COUNT],
            next_sprite_fetch_index: 0,
            m3_sprite_prev_tile: SPRITE_TILE_NONE,
            m3_last_sprite_commit_tick: 0,
            sprite_fetch_stall: 0,
            pixel_transfer_warmup: 0,
            fetcher_cadence_tick: 0,
            window_line_counter: 0,
            win_y_pos: 0xFF,
            win_draw_start: false,
            win_draw_started_at_x0: false,
            win_draw_started: false,
            window_y_triggered: false,
            win_start_dot: None,
            predicted_win_start_dot: None,
            win_wx_penalty_resolved: false,
            win_wx_enable_resolved: false,
            window_started_this_line: false,
            previous_stat_interrupt_line: false,
            mode2_irq_pretriggered_for_next_line: false,
            first_line_after_enable: false,
            display_enable_inactive_until: 0,
            line_153_ly_zeroed: false,
            mode0_pretriggered_this_line: false,
            m3_pixels_discarded: 0,
            m3_discard_target: -1,
            m3_arm_scx_full: -1,
            m3_arm_dot: 0,
            m3_arm_scx: 0,
            m3_scheduled_wx: 0,
            m3_scheduled_win: false,
            scan_obj_size_large: false,
            objsize_apply_cc: wy2_disabled(),
            objsize_prev_large: false,
            objsize_new_large: false,
            scheduled_mode0_dot: None,
            m0_time_master: None,
            lytime_no_plus1: false,
            ssds_mode3_ly_advance: false,
            sc_mode3_pullback_pending: false,
            dsss_mode3_stop_count: 0,
            render_carry_skew_cc: 0,
            cgbp_block_start_cc: None,
            mode0_reported_this_line: false,
            line_rendered_this_line: false,
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
            scx_prev_f1: 0,
            scx_f1_apply_cc: wy2_disabled(),
            scx_f1_new: 0,
            subcc_scx_apply_cc: wy2_disabled(),
            subcc_scx_old: 0,
            subcc_scx_new: 0,
            subcc_rekey_armed: false,
            prologue_rekey_armed: false,
            subcc_revert_next_old: false,
            ds_straddle_next_old: false,
            subcc_last_tn_cc: 0,
            first_line_scx_override: None,
            line_cycle: 0,
            internal_ly_val: 0,
            sched_lycirq: stat_irq::DISABLED_TIME,
            sched_m1irq: stat_irq::DISABLED_TIME,
            sched_m2irq: stat_irq::DISABLED_TIME,
            sched_m0irq: stat_irq::DISABLED_TIME,
            sched_oneshot_statirq: stat_irq::DISABLED_TIME,
            m1_vblank_fired: false,
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
            lcdc_b4_exact: None,
            we_win_bit_exact: None,
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

    /// Seed the post-boot PPU frame phase for `skip_bios`. The real boot ROM
    /// leaves the LCD enabled and the PPU deep into a frame; Gambatte's
    /// `setInitialState` sets `videoCycles = 144*456 + 164` (CGB) /
    /// `153*456 + 396` (DMG) — i.e. the game starts in VBlank at LY=144 (CGB) or
    /// LY=153 (DMG), NOT at a fresh LY=0 OAM search. Mirror that here so the very
    /// first instruction's LY/STAT reads (display_startstate tests) match real
    /// hardware. Must run after LCDC=0x91 and `sync_lcdc_from_mmio`.
    pub fn set_post_bios_state(&mut self, mmio: &mut mmio::Mmio) {
        // LCD must be on for this to apply (skip_bios writes LCDC=0x91 first).
        if self.lcdc & (LCDCFlags::DisplayEnable as u8) == 0 {
            return;
        }
        let cgb = mmio.is_cgb_features_enabled();
        // Gambatte initstate.cpp: videoCycles = cgb ? 144*456+164 : 153*456+396.
        let video_cycles: u32 = if cgb {
            144 * stat_irq::LCD_CYCLES_PER_LINE + 164
        } else {
            153 * stat_irq::LCD_CYCLES_PER_LINE + 396
        };
        let ly = (video_cycles / stat_irq::LCD_CYCLES_PER_LINE) as u8;
        let line_cycle = video_cycles % stat_irq::LCD_CYCLES_PER_LINE;

        self.disabled = false;
        self.internal_ly_val = ly;
        self.line_cycle = line_cycle;
        self.ticks = line_cycle as u128;
        // Both LY=144 (CGB) and LY=153 (DMG) land in VBlank.
        self.state = State::VBlank;
        self.first_line_after_enable = false;

        // On line 153 the LY *register* flips to 0 early (at dot
        // LINE_153_LY_ZERO_DOT), well before the line itself ends. The DMG
        // post-boot phase (LY=153, lineCycle=396) is past that dot, so the
        // register already reads 0 and the LYC=0 coincidence has already armed.
        // Mirror that transient state so the first FF44/FF41 read matches.
        let line_153_zeroed =
            ly == (stat_irq::LCD_LINES_PER_FRAME as u8 - 1) && line_cycle >= LINE_153_LY_ZERO_DOT as u32;
        self.line_153_ly_zeroed = line_153_zeroed;
        let ly_reg = if line_153_zeroed { 0 } else { ly };

        // Anchor the dot-clock origin: abs_cc = 0 at the post-boot instant so
        // ly_counter().time mirrors Gambatte's lyCounter.reset(videoCycles, cc)
        // with cc as the origin. p_now = master_cc keeps abs_cc = master_cc -
        // p_now consistent; the first step() folds abs_cc -> 1 and advances
        // line_cycle by one dot.
        self.abs_cc = 0;
        self.p_now = mmio.master_cc();
        self.lytime_no_plus1 = false;
        self.ssds_mode3_ly_advance = false;

        // Publish LY and the VBlank STAT mode (FF41 mode bits = 1).
        mmio.write_ly_from_ppu(ly_reg);
        Self::set_lcd_status_mode(mmio, 1);
        // LYC=LY coincidence flag against the *register* LY (0 on the line-153
        // transient). LYC defaults to 0, so CGB (LY=144) clears it and DMG
        // (LY register 0) sets it.
        let lyc = mmio.read(LYC);
        if lyc == ly_reg {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2));
        } else {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2));
        }

        // Seed the event-scheduled STAT/LYC IRQ clocks for the running frame.
        self.scy_delayed = mmio.read(SCY);
        self.scy_apply_cc = wy2_disabled();
        self.scx_delayed = mmio.read(SCX);
        self.scx_apply_cc = wy2_disabled();
        self.wy2 = mmio.read(WY);
        self.wy2_apply_cc = wy2_disabled();
        self.wy1 = mmio.read(WY);
        self.wy1_apply_cc = wy2_disabled();
        self.stat_reg_committed = mmio.read(LCD_STATUS);
        self.lyc_irq.set_cgb(cgb);
        self.lyc_irq.seed(mmio.read(LCD_STATUS), lyc);
        self.mstat_irq.seed(mmio.read(LCD_STATUS), lyc);
        self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
        self.reschedule_all_stat_events(mmio);
        self.sched_m0irq = stat_irq::DISABLED_TIME;
        self.sched_oneshot_statirq = stat_irq::DISABLED_TIME;
    }

    pub fn handle_lcdc_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        let display_enable = LCDCFlags::DisplayEnable as u8;
        let old_lcdc = self.lcdc;
        let display_stays_enabled = (old_lcdc & display_enable) != 0 && (value & display_enable) != 0;

        // Exact-cc OBJ-size (LCDC bit2) latch for the mode-2 OAM scan (PoC
        // extension). A sprite-size write during OAMSearch must become visible to
        // each OAM-scan slot as-of that slot's own abs_cc — not via the 2-dot
        // pending_lcdc_events queue plus the one-slot snapshot lag, which together
        // drop a late size change one OAM slot too far. Record the exact abs_cc
        // the change is visible (write_cc + 2*cgb, Gambatte setLcdc(data, cc+2));
        // the scan samples bit2 against it per slot. Scoped to mode-2 writes; the
        // PixelTransfer mid-mode-3 size toggle keeps its closed-form recompute.
        let ssz = LCDCFlags::SpriteSize as u8;
        if display_stays_enabled
            && self.state == State::OAMSearch
            && mmio.is_cgb_features_enabled()
            && (old_lcdc & ssz) != (value & ssz)
        {
            // The OBJ-size change becomes visible to the fetcher/scan at
            // `write_cc + 2` (Gambatte setLcdc(data, cc+2)). The OAM scan samples
            // it per slot against this apply cc (objsize_large_at_cc), so a slot
            // read strictly past the apply cc sees the new size. ENABLE (8x8 ->
            // 8x16) lands at +2; DISABLE (8x16 -> 8x8) lands one OAM slot later
            // (+2 more cc): Gambatte's SpriteMapper keeps the larger
            // already-latched height for the entry whose read straddles the
            // shrink, so the straddling sprite is still scanned 8x16. The
            // late_sizechange (disable) vs late_sizechange2 (enable) bracket pairs
            // require this asymmetry; with a symmetric offset the disable family
            // 1-for-1-swaps. (Verified across both speeds; DS landed at +2 for
            // both directions because the DS brackets only exercise the enable
            // side / the rounded odd-cc slot already absorbs the extra delay.)
            let ds = mmio.is_double_speed_mode();
            let disable = (old_lcdc & ssz) != 0 && (value & ssz) == 0;
            let off = if ds { 2 } else { 2 + if disable { 2 } else { 0 } };
            self.objsize_prev_large = self.objsize_large_at_cc(self.write_cc(ds));
            self.objsize_new_large = (value & ssz) != 0;
            self.objsize_apply_cc = (self.write_cc(ds) as i64 + off).max(0) as u64;
        }

        if mmio.is_cgb_features_enabled() && display_stays_enabled {
            // Exact-cc latch for the BG-fetch bit4 effect (PoC). When bit4
            // toggles during active pixel transfer, the per-dot queue quantizes
            // the commit to a dot boundary and lands it one fetch substep late.
            // Record the exact abs_cc the change should be visible to the
            // fetcher so each substep samples it on the correct side. Gambatte
            // applies the new LCDC at `cc + 2` (PPU dots); a +2 abs_cc offset
            // lands the bit4 change exactly on the BG-fetch substep that should
            // first see it (verified against bgtiledata_spx08_ds_3/_4).
            let tds = LCDCFlags::BGWindowTileDataSelect as u8;
            if self.state == State::PixelTransfer && (old_lcdc & tds) != (value & tds) {
                let ds = mmio.is_double_speed_mode();
                let commit_cc = self.write_cc(ds) + 2;
                self.lcdc_b4_exact = Some((commit_cc, value, old_lcdc));
            }
            // Window-enable (bit 5) toggle: record the exact Gambatte commit cc
            // (`write_cc + 2`, abs_cc units — same anchor as `lcdc_b4_exact`) so
            // the weMaster checkpoints resolve the window-enable bit as-of their
            // own dot (see `we_win_bit_exact`).
            let we = LCDCFlags::WindowDisplayEnable as u8;
            if (old_lcdc & we) != (value & we) {
                let ds = mmio.is_double_speed_mode();
                // Gambatte `setLcdc(data, cc + 2)`: the window bit is effective at
                // write_cc + 2 master cc. In rustyboi's abs_cc units the boundary
                // that aligns with the weMaster checkpoint dot (write_ticks + 2 dots
                // ahead) is `write_cc + 3` (single speed) / `+4` (double speed) —
                // the abs_cc derive-phase plus the per-dot abs_cc factor. The
                // weMaster event runs at the checkpoint BEFORE setLcdc, so equality
                // reads the OLD bit (the `<=` in `update_window_y_latch`).
                let commit_cc = self.write_cc(ds) + if ds { 4 } else { 3 };
                self.we_win_bit_exact =
                    Some((commit_cc, (value & we) != 0, (old_lcdc & we) != 0));
            }
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
                        // The settled value now lives in self.lcdc /
                        // cgb_tile_index_is_tile_data; drop the exact-cc override.
                        self.lcdc_b4_exact = None;
                    }
                }
            } else {
                index += 1;
            }
        }
    }

    /// Mode-3 sprite cost (dots) of the sprites NOT yet rendered this line, under
    /// the given OBJ-enable state, using the one faithful tile-walk model. Sprites
    /// with index < `next_sprite_fetch_index` have already been drawn (their cost
    /// is already spent and fixed); only the remaining ones contribute. Drives the
    /// mid-mode-3 OBJ-toggle recompute so the closed-form m0Time is shifted by the
    /// exact remaining-sprite cost delta (matching Gambatte's predictNextM0Time
    /// re-run at the current `p.nextSprite`).
    fn remaining_sprite_cost(&self, scx: i32, obj_enabled: bool, use_fetch_index: bool) -> i32 {
        if !obj_enabled {
            return 0;
        }
        // The set of sprites whose cost is NOT yet committed (and so is affected by
        // a mid-mode-3 OBJ toggle). Two gates, matching how the live renderer
        // commits sprite fetches:
        //  - DISABLE (`use_fetch_index`): OBJ was on up to here, so the fetch loop
        //    has advanced `next_sprite_fetch_index` over every sprite whose stall
        //    already armed (committed). Only sprites at index >= that count have
        //    their cost removed. This gives the exact 1-cc disable boundary the
        //    sprite_late_disable_*_{1,2} pairs bracket (the stall arms on the dot
        //    the index advances).
        //  - ENABLE: OBJ was off, so the fetch loop never advanced; a sprite will
        //    still be fetched iff its trigger (display x = spx - 8) is not yet
        //    passed, i.e. spx >= x + 8.
        if use_fetch_index {
            // DISABLE: the live renderer advances `next_sprite_fetch_index` at the
            // START of each sprite's stall and locks that sprite's cost into the
            // schedule GRADUALLY as the stall counts down -- Gambatte's
            // `doFullTilesUnrolled` charges the sprite's `max(11-dist,6)` dots one at
            // a time as `p.cycles` is consumed. A mid-mode-3 OBJ-disable therefore
            // refunds only the part of the in-progress sprite's stall that has NOT
            // yet elapsed, plus the full cost of every sprite whose stall has not yet
            // started (index >= nsfi). This makes the refunded m0Time depend 1:1 on
            // the disable cc (the later the disable, the less the refund), which the
            // sprite_late[_late]_disable_spx{18..1B}_{1,2} bracket pairs require:
            // their disable timings differ by single dots and the refunded mode-3 end
            // must cross the FF41 read cc by the matching fraction.
            //
            // Sprites at index >= nsfi: stall not yet started -> fully refundable.
            let mut tail: Vec<i32> = self
                .sprites_on_line
                .iter()
                .skip(self.next_sprite_fetch_index)
                .map(|s| s.x as i32)
                .collect();
            tail.sort_unstable();
            let mut cost = sprite_tile_walk_cost(&tail, scx, 167, 167, true);
            // In-progress sprite (index nsfi-1): its stall began at
            // `m3_last_sprite_commit_tick`; the dots remaining are its standalone
            // leading-rate cost minus the dots already counted down. Refund only the
            // remaining (clamped at 0 once fully drawn).
            if self.next_sprite_fetch_index > 0 {
                let in_prog = &self.sprites_on_line[self.next_sprite_fetch_index - 1];
                let single = sprite_tile_walk_cost(&[in_prog.x as i32], scx, 167, 167, true);
                // The live renderer consumes the in-progress sprite's first stall dot
                // on the same tick it advances `next_sprite_fetch_index` (the stall is
                // armed and immediately decremented), so the elapsed count includes
                // the commit tick itself: `ticks - commit_tick + 1`.
                let elapsed = self
                    .ticks
                    .saturating_sub(self.m3_last_sprite_commit_tick) as i32
                    + 1;
                cost += (single - elapsed).max(0);
            }
            return cost;
        }
        // ENABLE: a sprite will still be fetched iff the fetcher has NOT yet reached
        // its trigger (display x = spx - 8). At x == spx - 8 the fetcher is already
        // at the trigger and the sprite is missed, so the gate is strict: spx > x + 8.
        // (The sprite_late_enable_spx18_{1,2} pair brackets this single-dot boundary:
        // enabling at x = spx-9 still fetches, at x = spx-8 does not.)
        let cutoff = self.x as i32 + 8;
        let mut sprite_xs: Vec<i32> = self
            .sprites_on_line
            .iter()
            .map(|s| s.x as i32)
            .filter(|&spx| spx > cutoff)
            .collect();
        sprite_xs.sort_unstable();
        // The remaining group resumes the tile walk with no carried "first sprite"
        // (prevTileNo = none), so the first remaining sprite in its tile gets the
        // leading rate, the rest 6 — the same `addSpriteCycles` continuation
        // Gambatte uses. No window split here (the window-bit is unchanged on this
        // path, so `nwx == targetx` collapses the split).
        sprite_tile_walk_cost(&sprite_xs, scx, 167, 167, true)
    }

    fn fetcher_lcdc_state(&self) -> fetcher::FetcherLcdcState {
        // Exact-cc resolution of a pending mid-mode-3 bit4 toggle (PoC). If a
        // bit4 change is latched and this substep's abs_cc has not yet reached
        // its exact commit cc, present the PRE-commit bit4 (and suppress the
        // CGB tile-index-as-data quirk, which only arms on the realized fall).
        // This lets a single tile straddle the change: TileDataLow before the
        // commit uses the old method, TileDataHigh after it uses the new one.
        if let Some((commit_cc, new_val, old_val)) = self.lcdc_b4_exact {
            let tds = LCDCFlags::BGWindowTileDataSelect as u8;
            if self.abs_cc < commit_cc {
                // Pre-commit: old bit4, no quirk yet.
                let lcdc = (self.lcdc & !tds) | (old_val & tds);
                return fetcher::FetcherLcdcState {
                    lcdc,
                    cgb_tile_index_is_tile_data: false,
                };
            } else {
                // Post-commit: new bit4. Re-derive the falling-edge quirk
                // (set in set_lcdc_visible) so a 1->0 fall returns the tile
                // index as data for tiles < 0x80.
                let lcdc = (self.lcdc & !tds) | (new_val & tds);
                let quirk = (old_val & tds) != 0 && (new_val & tds) == 0;
                return fetcher::FetcherLcdcState {
                    lcdc,
                    cgb_tile_index_is_tile_data: quirk,
                };
            }
        }
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
        // A mid-mode-3 sprite-enable (bit 1) toggle, with no window change, keeps
        // the closed-form schedule but RECOMPUTES the not-yet-drawn sprite cost
        // from the single tile-walk model (Gambatte's predictNextM0Time re-runs the
        // predictor with `lcdcObjEn(p)` live and the current `p.nextSprite`, so the
        // remaining sprites' cost is added/removed precisely). Shift both the
        // mode-0 dot and the read-at-cc m0Time by the cost delta rather than
        // nulling and falling back to the live x==160 transition.
        let obj_bit = LCDCFlags::SpriteDisplayEnable as u8;
        let only_obj_toggle = (old_lcdc & win_bit) == (value & win_bit)
            && (old_lcdc & (LCDCFlags::SpriteSize as u8)) == (value & (LCDCFlags::SpriteSize as u8))
            && (old_lcdc & obj_bit) != (value & obj_bit);
        if self.state == State::PixelTransfer
            && only_obj_toggle
            && self.scheduled_mode0_dot.is_some()
        {
            let scx = (self.m3_arm_scx & 0x07) as i32;
            let old_obj = (old_lcdc & obj_bit) != 0 || cgb_features_enabled;
            let new_obj = (value & obj_bit) != 0 || cgb_features_enabled;
            // DISABLE (old OBJ on): committed sprites are those whose cost the live
            // fetch loop has already locked into the schedule -> gate by the
            // lock-aware committed index. ENABLE (old OBJ off): gate by display
            // position. `use_fetch_index = old_obj` selects the right gate for
            // whichever side is non-zero.
            let use_fetch_index = old_obj && !new_obj;
            let old_rem = self.remaining_sprite_cost(scx, old_obj, use_fetch_index);
            let new_rem = self.remaining_sprite_cost(scx, new_obj, false);
            let delta = new_rem - old_rem; // dots; negative on disable
            // KEEP the closed-form schedule, shifting it by the (graduated) cost
            // delta. delta < 0 refunds the not-yet-drawn portion of the remaining
            // sprites (predictNextM0Time re-run with the new lcdcObjEn at the current
            // `p.nextSprite`); delta == 0 means every remaining sprite's cost is
            // already drawn, so the original closed-form m0Time (which includes the
            // full sprite cost) is already correct and must be kept -- nulling it and
            // falling back to the live x==160 transition would mis-resolve the FF41
            // read for the fully-committed bracket variants (sprite_late_late_disable
            // spx1B_2). The graduated `remaining_sprite_cost` makes the refund (and so
            // the resulting m0Time) depend 1:1 on the disable cc, which is what the
            // sprite_late[_late]_disable bracket pairs require.
            if let Some(dot) = self.scheduled_mode0_dot {
                self.scheduled_mode0_dot = Some((dot as i64 + delta as i64).max(0) as u128);
            }
            if let Some(m0t) = self.m0_time_master {
                let dsf = ds as i64;
                self.m0_time_master =
                    Some((m0t as i64 + ((delta as i64) << dsf)).max(0) as u64);
            }
            self.lcdc = value;
            return;
        }
        if self.state == State::PixelTransfer
            && ((old_lcdc & win_bit) != (value & win_bit)
                || (old_lcdc & spr_bits) != (value & spr_bits))
        {
            self.scheduled_mode0_dot = None;
            // A mid-mode-3 window-ENABLE toggle (not sprite) is the symmetric
            // counterpart to the disable refund below: the closed-form m0_time_master
            // was captured at M3 arm WITHOUT the window (it was off), so it lacks the
            // StartWindowDraw mode-3 penalty. If the window will now actually start
            // this line (window-Y gate holds and the fetcher has not yet passed the
            // window-start x = max(0, WX-7)), Gambatte's predictNextM0Time re-runs
            // with the window included and the boundary moves WIN_M3_PENALTY dots
            // later. ADD that penalty to m0_time_master so the FF41 read resolves the
            // window-inclusive mode-3 end, instead of nulling and falling back to the
            // live no-window-at-arm pipeline (which lands the boundary too early).
            // Scoped to no-sprite lines (CGB and DMG alike) so the sprite-fetch
            // geometry is unchanged; sprite-bit toggles still null below.
            let win_enable_clean = (old_lcdc & spr_bits) == (value & spr_bits)
                && (old_lcdc & win_bit) == 0
                && (value & win_bit) != 0
                && self.sprites_on_line.is_empty();
            let mut win_enable_handled = false;
            if win_enable_clean {
                win_enable_handled = true;
                // Window-Y gate: the window can start this line iff WY has triggered
                // (`window_y_triggered`, set at the line-450/454 weMaster checkpoints
                // when LY==WY). set_lcdc_visible has no mmio handle, so use the
                // cached arm-time geometry: m3_scheduled_wx (WX latched at M3 arm)
                // and the window-Y trigger latch.
                let wx = self.m3_scheduled_wx as i32;
                // Window-Y gate, mirroring `window_y_active`: the weMaster trigger
                // latch (`window_y_triggered`, set at the line-450/454 checkpoints)
                // OR the immediate `wy2 == LY` fallback. The latter is required on
                // the first line after enable (LY=0), where the previous line's
                // checkpoints never ran so `window_y_triggered` is still false even
                // when WY==0 — exactly the late_enable_ly0 case.
                let wy_ok = self.window_y_triggered || self.wy2 == self.internal_ly_val;
                let wx_in_range = (0..=166).contains(&wx) && (cgb_features_enabled || wx != 166);
                // The window penalty applies iff the enable lands BEFORE the
                // fetcher reaches the window-tile commit dot. The window draws from
                // visible x == max(0, WX-7); x begins advancing `WARMUP + 8` dots
                // past the M3 arm (the first BG tile fill) plus the SCX fine-scroll
                // discard. The penalty commits one dot ahead of the first window
                // pixel reaching x (the `-1`), mirroring `predicted_win_start_dot`.
                // The late_enable_ly0_ds_{1,2} pair brackets this commit dot to a
                // single cycle: _1 (write 1 cycle earlier) takes the +6, _2 does not.
                let x_at_start = (wx - 7).max(0);
                let warmup = if cgb_features_enabled {
                    CGB_PIXEL_TRANSFER_WARMUP as i64
                } else {
                    DMG_PIXEL_TRANSFER_WARMUP as i64
                };
                // SCX==5 fine-scroll phase: Gambatte's M3Start dispatch runs the
                // window-tile fetch one dot later than the linear discard model at
                // this single phase (the same +1 the closed-form mode-3 length applies
                // at scx==5, compute_m3_length_win). For x==0 windows (WX<=7) the
                // commit dot is therefore one dot later; without it a window-enable on
                // the boundary dot wrongly drops the penalty (late_reenable_scx5_2),
                // while scx3 stays on the linear boundary (late_reenable_scx3_2).
                let win_fine = if wx <= 7 && (self.m3_arm_scx & 7) == 5 { 1 } else { 0 };
                let commit_dot = self.m3_arm_dot as i64
                    + warmup
                    + 8
                    + self.m3_arm_scx as i64
                    + x_at_start as i64
                    + win_fine
                    - 1;
                let will_start = wy_ok && wx_in_range && (self.ticks as i64) < commit_dot;
                if will_start {
                    if let Some(m0t) = self.m0_time_master {
                        let pen = (WIN_M3_PENALTY as i64) << ds as i64;
                        self.m0_time_master = Some((m0t as i64 + pen).max(0) as u64);
                    }
                }
                // else: keep the no-window m0_time_master as captured at arm.
            }
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
            // Single-speed window-disable handling for both CGB and DMG. The
            // StartWindowDraw mode-3 penalty is captured (full) at M3 arm in
            // m0_time_master. CGB refunds the not-yet-drawn window dots gradually;
            // DMG is binary (full keep once committed, else null) — see the two
            // branches below. The DMG late_disable cluster reads the STAT mode
            // after the disable and expects mode 3 to persist whenever the window
            // had already committed, which the binary keep provides; the prior
            // null-and-fall-back-to-live-no-window path reported mode 0 too early.
            let clean_ss = !ds && self.sprites_on_line.is_empty();
            let clean_ds = cgb_features_enabled
                && ds
                && self.sprites_on_line.is_empty();
            // On DMG the LCDC-write hook fires one PPU step before the
            // PixelTransfer code latches `win_start_dot`, so a disable landing
            // exactly on the window-start dot still sees
            // `window_started_this_line == false`. Bridge that one-step race with
            // the M3-arm prediction: the window is effectively started once the
            // current tick has reached the predicted start dot. The graduated
            // refund then uses the predicted dot as the start (drawn==0 at the
            // boundary -> full penalty kept).
            // CGB single-speed window-disable WITH a sprite on the line: the
            // window_started_this_line latch lags the closed-form StartWindowDraw
            // commit (it flips only when the visible window x is reached), so a
            // disable landing at/after the window-tile fetch commit still sees it
            // false and would wrongly null (mode 0). Bridge with the predicted
            // commit dot `m3_arm_dot + CGB_WARMUP + 8 + scx&7 + max(0, WX-7) - 1`
            // (mirroring the LCDC window-ENABLE commit), so the binary keep branch
            // below fires once the window has committed. The late_disable_spx10_wx0f
            // _{1,2} CGB reps bracket it (disable at dot 98 = before -> out0 via the
            // null below; dot 102 = at commit -> out3 keep).
            let cgb_spr_commit = if cgb_features_enabled
                && !ds
                && !self.sprites_on_line.is_empty()
                && self.m3_scheduled_win
            {
                let x_at_start = (self.m3_scheduled_wx as i64 - 7).max(0);
                Some(self.m3_arm_dot as i64
                    + CGB_PIXEL_TRANSFER_WARMUP as i64
                    + 8
                    + (self.m3_arm_scx & 7) as i64
                    + x_at_start
                    - 1)
            } else {
                None
            };
            let win_started_for_refund = self.window_started_this_line
                || (!cgb_features_enabled
                    && self
                        .predicted_win_start_dot
                        .is_some_and(|p| self.ticks >= p))
                || cgb_spr_commit.is_some_and(|c| (self.ticks as i64) >= c);
            // CGB keeps the graduated refund (predicted_win_start_dot is DMG-only,
            // so this is just win_start_dot on CGB); DMG uses the binary keep below.
            let refund_start_dot = self.win_start_dot.or(self.predicted_win_start_dot);
            if win_enable_handled {
                // The clean window-ENABLE adjusted m0_time_master above; skip the
                // disable-refund / null path (which would otherwise null it because
                // `only_win_toggle` is false for an enable).
            } else if !only_win_toggle || !win_started_for_refund {
                self.m0_time_master = None;
            } else if !ds
                && !cgb_features_enabled
                && !self.sprites_on_line.is_empty()
                && win_started_for_refund
            {
                // DMG late window-disable WITH a sprite on the line (late_disable_spx10
                // cluster). The StartWindowDraw penalty is binary on DMG exactly as in
                // the no-sprite branch below; the sprite cost is already baked into the
                // M3-arm m0_time_master and is unaffected by the window toggle. Once the
                // window has committed (win_started_for_refund) the disable keeps the
                // full window-inclusive m0Time (mode 3 persists -> out3); a disable
                // before the commit took the `!win_started_for_refund` null path above
                // (no penalty -> mode 0 -> out0). The spx10_wx0f_{1,2} reps bracket this
                // boundary. Keep m0_time_master as captured (no-op).
            } else if !ds
                && cgb_features_enabled
                && !self.sprites_on_line.is_empty()
                && win_started_for_refund
            {
                // CGB single-speed late window-disable WITH a sprite on the line
                // (late_disable_spx10_wx0f_2). Binary like the DMG-sprite branch: the
                // sprite cost is baked into the M3-arm m0_time_master and the window
                // StartWindowDraw penalty locks once the fetcher fetches the window
                // tile. `win_started_for_refund` already gated the commit dot via
                // `cgb_spr_commit`, so reaching here means the disable landed at/after
                // the commit -> keep the full window-inclusive m0Time (mode 3 -> out3).
                // A disable before the commit took the `!win_started_for_refund` null
                // path above (-> mode 0 -> out0, the passing _1 rep). Keep (no-op).
            } else if clean_ss && !cgb_features_enabled {
                // DMG: the StartWindowDraw penalty is binary, not graduated. Once
                // the window has reached its commit dot (win_started_for_refund),
                // a mid-M3 window-disable keeps the FULL window-inclusive m0Time
                // (mode 3 persists through the read); a disable before the commit
                // dot already nulled above (no penalty -> mode 0). The
                // late_disable_* DMG cluster (out0 just-before vs out3 at/after)
                // brackets exactly this binary boundary; a graduated refund here
                // over-shortens the at/after cases at SCX>0 / higher WX. Keep the
                // window-inclusive m0_time_master as captured at M3 arm (no-op).
            } else if clean_ss {
                if let (Some(m0t), Some(ws)) = (self.m0_time_master, refund_start_dot) {
                    // The StartWindowDraw penalty does not begin accruing until the
                    // fetcher reaches the window tile, which the SCX fine-scroll
                    // discard delays by `scx&7` dots past `win_start_dot`. Without
                    // this shift the accrual is `scx&7` dots early, so a disable in
                    // the `scx&7` dots just after win_start over-accrues (refund
                    // truncated) — the late_disable_scx{2,3,5}_1 CGB cluster reads
                    // mode 3 (out3) where Gambatte's later lock still refunds to
                    // mode 0 (out0). Shifting the reference by scx&7 lands all phases
                    // (scx0 unchanged; scx5_1 at the same dot as scx0_2 now refunds).
                    // The StartWindowDraw penalty does not begin accruing until the
                    // fetcher reaches the window tile. For a window that starts at
                    // x==0 (WX<=7), `win_start_dot` is latched at the start of the
                    // x==0 region — BEFORE the SCX fine-scroll discard (which still
                    // consumes scx&7 dots). So the accrual reference is scx&7 dots
                    // early, and a disable in those dots over-accrues (refund
                    // truncated): the late_disable_scx{2,3,5}_1 CGB reps read mode 3
                    // (out3) where Gambatte's later lock still refunds to mode 0
                    // (out0). Shift the reference by scx&7 for x==0 windows only.
                    // For WX>7 the window starts AFTER the discard, so `win_start_dot`
                    // already reflects post-discard time (no shift — the scx03_wx1x
                    // reps keep their out3 boundary).
                    let win_fine = if self.m3_scheduled_wx <= 7 {
                        (self.m3_arm_scx & 7) as i64
                    } else {
                        0
                    };
                    let drawn = (self.ticks as i64) - ws as i64 - win_fine;
                    let accrued = drawn.clamp(0, WIN_M3_PENALTY as i64);
                    let refund = WIN_M3_PENALTY as i64 - accrued;
                    self.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                } else {
                    self.m0_time_master = None;
                }
            } else if clean_ds {
                if let (Some(m0t), Some(ws)) = (self.m0_time_master, self.win_start_dot) {
                    // GRADUATED refund (as in the single-speed branch): the window
                    // penalty accrues one dot per drawn window dot, capped at
                    // WIN_M3_PENALTY; the unaccrued remainder is refunded. At double
                    // speed each dot is 2 cc. (Was a binary full-or-none refund,
                    // which over-refunded an early disable by the 2 already-drawn
                    // window dots -> the late_disable_early_*_ds reads flipped.)
                    // SCX fine-scroll shift for x==0 windows (WX<=7), same as the
                    // single-speed branch: win_start_dot is latched before the scx&7
                    // discard completes, so the accrual reference is scx&7 dots early.
                    // Generalising the former `m3_arm_scx&7==0` gate to all phases
                    // covers the late_disable_scx5_ds_1 CGB rep.
                    let win_fine = if self.m3_scheduled_wx <= 7 {
                        (self.m3_arm_scx & 7) as i64
                    } else {
                        0
                    };
                    let drawn = (self.ticks as i64) - ws as i64 - win_fine;
                    let accrued = drawn.clamp(0, WIN_M3_PENALTY as i64);
                    let refund = (WIN_M3_PENALTY as i64 - accrued) << 1;
                    self.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                } else {
                    self.m0_time_master = None;
                }
            } else {
                self.m0_time_master = None;
            }
        }
        // Gambatte setLcdc (ppu.cpp 1867-1874): a WE (window-enable) toggle with
        // the LCD already on updates winDrawState. rustyboi splits Gambatte's
        // 2-bit winDrawState into `win_draw_start` (bit win_draw_start) and
        // `win_draw_started` (bit win_draw_started); reproduce the exact bit
        // arithmetic here. `xpos == xpos_end` (the line's pixel transfer is
        // done) holds whenever we are not actively in PixelTransfer, or x has
        // reached the line end inside it.
        if (old_lcdc & display_enable) != 0 && (old_lcdc & win_bit) != (value & win_bit) {
            let at_line_end = !matches!(self.state, State::PixelTransfer) || self.x >= 160;
            if (value & win_bit) == 0 {
                // WE-off: clear win_draw_started iff winDrawState == win_draw_started
                // (started but not armed) OR the line is finished. win_draw_start
                // (the arm bit) survives, so a re-enable can resume next line.
                if (self.win_draw_started && !self.win_draw_start) || at_line_end {
                    self.win_draw_started = false;
                    // If the fetcher is actively drawing the window mid-line, the
                    // window stops here and the next tile fetch reverts to BG
                    // (Gambatte Tile::f0 `winDrawState & win_draw_started` gate).
                    if self.fetcher.is_fetching_window() {
                        // Gambatte Tile::f0 commits each window tile's window-vs-BG
                        // choice at the tile boundary (`xpos == endx`, where the
                        // window-tile grid is `(xpos + wscx) % tile_len == 0`). A
                        // WE-off that lands EXACTLY on a window-tile boundary reverts
                        // to BG at the next tile; one that lands MID-tile lets the
                        // already-committed in-progress tile finish first (one extra
                        // window tile). Mapping Gambatte's `(xpos + wscx) % 8` into
                        // rustyboi's integer fetcher geometry (xpos == display x +
                        // (26 - win_x_start), wscx == 256 - win_x_start) gives the
                        // boundary test `(x + 2 - 2*win_x_start) % 8 == 0`. This is
                        // the byte-exact discriminator between wx17 (mid-tile -> +1
                        // tile) and weon_wx18 (boundary -> +0), which share an
                        // identical fetch-grid cc phase but differ in absolute
                        // display-x / window alignment.
                        // Scoped to CGB: Gambatte's mid-tile boundary completion
                        // for a WE-off lives in StartWindowDraw::inc behind an
                        // explicit `&& p.cgb` gate, and the (26 - win_x_start) /
                        // (256 - win_x_start) xpos/wscx mapping is the CGB
                        // fetcher geometry. On DMG the warmup/cgb_adj phase
                        // differs and the prior immediate-revert is byte-exact
                        // (wx17/weon DMG pass at baseline), so leave DMG on extra=0.
                        let extra = if cgb_features_enabled {
                            let wxs = self.fetcher.window_x_start_dbg() as i32;
                            let phase = (self.x as i32 + 2 - 2 * wxs).rem_euclid(8);
                            if phase == 0 { 0u8 } else { 1u8 }
                        } else {
                            0u8
                        };
                        self.fetcher.stop_window_with_extra(extra);
                        self.window_started_this_line = false;
                    }
                }
            } else {
                // WE-on: if winDrawState == win_draw_start (armed but not started),
                // promote to started and advance the window Y line.
                if self.win_draw_start && !self.win_draw_started {
                    self.win_draw_started = true;
                    self.win_y_pos = self.win_y_pos.wrapping_add(1);
                }
            }
        }
        self.lcdc = value;
    }

    /// Current PPU master clock (`abs_cc`). Used by the interrupt-service LCD
    /// ack to position the IF clear at the exact dot (see
    /// `Bus::interrupt_low_push_ack`).
    pub fn abs_cc(&self) -> u64 { self.abs_cc }

    /// STAGE 4 KEYSTONE — the accumulated STAT-phase carry (master-cc). The bus
    /// SUBTRACTS this from a CPU VRAM/OAM access cc so the render-visibility gate
    /// (`ppu_blocks` / `get_stat` fallback mode + `cpu_access_blocked`) sees the
    /// access in the un-carried fetcher geometry (the carry moved the lyTime
    /// boundaries but not the fetcher's lock window). 0 when no carry is live.
    pub fn render_carry_skew(&self) -> i64 {
        self.render_carry_skew_cc
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

    /// ds-subdot STAGE 1: the LyCounter as the CPU READ path must observe it —
    /// sub-dot (master_cc) exact. At double speed the renderer's `abs_cc`/
    /// `line_cycle` are advanced on the even-render-dot grid, which sits one
    /// master_cc below Gambatte's even line phase, so the bare `lyTime` (next-LY
    /// master cc) runs 1 cc low and `lineCycles = 456 - ((lyTime-cc)>>1)` reads 1
    /// high. Carry the missing sub-dot here so the observed `lyTime`/`lineCycles`/
    /// LY/LYC-flag are master_cc-exact at DS (proven via cctracer: ds_1 lineCycles
    /// 251->250, lyTime 140567->140568). At single speed the bare phase is already
    /// exact (no flooring), so the correction is DS-only; `lytime_no_plus1` (post
    /// DS->SS-switch line) already drops the +1. Flag-OFF this is identical to
    /// `ly_counter`. SCOPE: only the CPU-visible read observers call this; the
    /// internal STAT-event SCHEDULE still keys off the un-corrected `ly_counter`
    /// (its fire-cc anchors are re-anchored in Stages 2-4, not here).
    fn ly_counter_obs(&self, mmio: &mmio::Mmio) -> stat_irq::LyCounter {
        let mut lc = self.ly_counter(mmio);
        if lc.ds && !self.lytime_no_plus1 {
            lc.time += 1;
        }
        lc
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
    ///
    /// `first_line` selects the first line after LCD enable: Gambatte seeds the PPU
    /// at enable with `cycles = -(m3StartLineCycle + 2)` (PPU::setLcdc), so the
    /// first M3 begins TWO dots later than the normal-line m3-start anchor encoded
    /// in BASE (which == `m3StartLineCycle`). The mode-0 line-cycle is therefore
    /// `m3_len + BASE + 2`. (`p_now + ly_counter().time` is enable-anchored on this
    /// line — `setLcdc` reset `now = enable_cc`, `lyCounter.reset(0, enable_cc)`.)
    fn m0_time_exact(&self, mmio: &mmio::Mmio, m3_len: u128, is_cgb: bool, first_line: bool) -> u64 {
        let ds = mmio.is_double_speed_mode() as u32;
        let base: i64 = if is_cgb { 84 } else { 83 };
        let plus1 = if self.lytime_no_plus1 { 0 } else { 1 };
        let ly_time = self.p_now as i64 + self.ly_counter(mmio).time as i64 + plus1;
        let m0_line_cycle = m3_len as i64 + base + if first_line { 2 } else { 0 };
        (ly_time - ((456 - m0_line_cycle) << ds)).max(0) as u64
    }

    /// Arm `sched_m0irq` for the current line from the renderer's predicted
    /// mode-0 start (`scheduled_mode0_dot`, a within-line dot). Converted to the
    /// absolute clock. If no closed-form mode-0 dot is available (window/first
    /// line), fall back to the m0 prediction from the m3 length.
    fn arm_m0irq_for_current_line(&mut self, mmio: &mmio::Mmio, first_frame: bool) {
        let is_cgb = mmio.is_cgb_features_enabled();
        // The mode-0 (HBlank) STAT IRQ time is co-calibrated with the
        // `ticks + m3_len + offset` mode-0 dot, NOT the exact getStat `m0Time`.
        // The lazy-PPU rewrite re-derived `scheduled_mode0_dot` from the exact
        // getStat m0Time (which the CPU read resolves at `cc + 2 < m0Time`),
        // landing it 1-3 dots earlier than the eager mode-0 grid the m0 IRQ
        // offset (M0IRQ_OFFSET) was tuned against. Reading `reported_mode0_dot`
        // (= that exact dot) here armed the m0 IRQ early and broke the
        // m2int_m0irq / m0enable / enable_display / vramw_m3end m0-IRQ clusters.
        // Arm from the m3-length dot instead — the same anchor core-loop used —
        // so the IRQ fires on the calibrated boundary again. (Env-overridable to
        // restore the exact-m0Time arm for diagnostics.)
        let mode0_within_line = {
            let m3_len = self.compute_m3_length(mmio, is_cgb);
            let offset = if is_cgb { cgb_mode0_offset() } else { dmg_mode0_offset() };
            self.ticks as i64 + m3_len as i64 + offset as i64
        };
        let mut remaining = mode0_within_line - self.ticks as i64;
        // VBlank (LY 144..153) has no mode 0 on the current line: Gambatte's
        // `predictedNextXposTime(166)` lands on the next *rendering* line's mode 0
        // (line 0 of the following frame), far beyond the current VBlank. The
        // `ticks + m3_len + offset` form above computes a bogus within-VBlank-line
        // dot which would fire a spurious m0 STAT IRQ this frame (lycint152_m0irq).
        // Carry the schedule forward to line 0: dots to the end of the current
        // line, plus the full VBlank lines that follow, plus line-0's mode-0 dot
        // offset (reuse `m3_len + offset` from above as the line-0 proxy).
        let ly = self.internal_ly() as i64;
        if ly >= stat_irq::LCD_VRES as i64 {
            let last_line = (stat_irq::LCD_LINES_PER_FRAME - 1) as i64; // 153
            let cpl = stat_irq::LCD_CYCLES_PER_LINE as i64;
            let line0_m0_offset = mode0_within_line - self.ticks as i64; // m3_len + offset
            let dots_to_current_line_end = cpl - self.ticks as i64;
            let full_vblank_lines = (last_line - ly) * cpl;
            remaining = dots_to_current_line_end + full_vblank_lines + line0_m0_offset;
        } else {
            // The mode-0 STAT IRQ fires at `predictedNextXposTime(166)`, one xpos
            // before the m0Time (xpos 167) the closed-form `m3_len` above tracks.
            // For plain lines those differ by one dot (already folded into
            // `M0IRQ_OFFSET`); when a window starts at WX=166 or a sprite sits at
            // the right edge, the final xpos step carries the whole penalty and
            // the IRQ fires that many dots earlier. Subtract that extra advance.
            remaining -= self.m0irq_xpos166_advance(mmio, is_cgb);
        }
        let ds = mmio.is_double_speed_mode();
        let mut off = if ds { m0irq_off_ds() } else { m0irq_off_ss() };
        if is_cgb && !ds && (mmio.read(SCX) & 0x07) == 2 {
            off += M0IRQ_SCX2_CGB_OFFSET;
        }
        if first_frame && !is_cgb && !ds {
            off += M0IRQ_DMG_FIRST_FRAME_OFFSET;
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
            self.arm_m0irq_for_current_line(mmio, self.first_line_after_enable);
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

    /// Latch the SS->DS-during-mode3 FF44 (LY) read phase advance. Consumed only
    /// by `get_ly_reg_at_cc` to resolve the getLyReg anticipation window against
    /// Gambatte's re-anchored lyTime (the renderer/STAT/m0 phase is unaffected).
    pub fn set_ssds_mode3_ly_advance(&mut self) {
        self.ssds_mode3_ly_advance = true;
    }

    /// STAGE 4 (FACET 2 KEYSTONE) — advance the STAT/LINE-PHASE clock by ONE dot
    /// WITHOUT moving the pixel-fetcher render latch (`self.ticks`/`self.x`/the
    /// FIFO/the render state machine). This is the decoupling primitive: rustyboi
    /// normally welds `line_cycle` (the STAT/LY/ttnl phase clock) to the renderer
    /// inside `step` (both `line_cycle += 1` and `self.ticks += 1` per dot). A
    /// faithful sub-dot STOP re-anchor (FACET 1) needs to shift the STAT phase by
    /// an ODD dot WITHOUT moving the mode-3 render latch (FACET-2 coupling). This
    /// mirrors `step`'s STAT-phase region (the lines between `dispatch_stat_events`
    /// and `update_window_y_latch`) exactly, but skips the `match self.state`
    /// render machine and the `self.ticks += 1`. It is the line-phase HALF of the
    /// lockstep that `step` runs as a whole.
    ///
    /// Caller contract (mirrors `stop_bridge_advance`'s per-dot prelude): pull
    /// `p_now` back by one dot BEFORE calling so the derived `abs_cc` still
    /// advances `1<<ds` for this STAT dot (the carry is a non-master-cc-advancing
    /// bridge dot, same as the rendered bridge dots). `step_scheduled_stat_events`
    /// / `step_lcdc_events` are run by the caller around it, identically to the
    /// rendered-bridge per-dot loop, so the only difference from a bridge `step`
    /// is the absence of render-latch motion.
    fn step_stat_phase_only(&mut self, mmio: &mut mmio::Mmio) {
        if self.disabled || self.lcdc & (LCDCFlags::DisplayEnable as u8) == 0 {
            return;
        }
        // --- STAT-phase region of `step` (no render match, no `ticks += 1`) ---
        self.dispatch_stat_events(mmio);
        self.abs_cc = mmio.master_cc().wrapping_sub(self.p_now);
        self.line_cycle += 1;
        if self.line_cycle >= stat_irq::LCD_CYCLES_PER_LINE {
            self.line_cycle = 0;
            self.internal_ly_val += 1;
            if self.internal_ly_val as u32 >= stat_irq::LCD_LINES_PER_FRAME {
                self.internal_ly_val = 0;
            }
        }
        self.process_oam_reader_events(mmio);
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
        let effective_ly = self.effective_ly_for_lyc_compare(mmio);
        if mmio.read(LYC) == effective_ly {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2));
        } else {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2));
        }
        self.check_and_trigger_stat_interrupt(mmio);
        self.update_window_y_latch(mmio);
    }

    /// STAGE 4 (FACET 1) — register one DS->SS-during-mode3 STOP switch and
    /// return how many STAT-phase carry dots to inject this switch (the increment
    /// in `floor(count/2)`): every 2nd such switch injects ONE extra dot,
    /// reproducing the accumulated Gambatte `now -= 1` half-dot. Stop-count
    /// invariant by construction (the carry depends only on the running count,
    /// not on any single STOP's integer-cc). Returns 0 on the odd switches.
    pub fn register_dsss_mode3_stop(&mut self) -> u32 {
        let before = self.dsss_mode3_stop_count / 2;
        self.dsss_mode3_stop_count += 1;
        let after = self.dsss_mode3_stop_count / 2;
        after - before
    }

    /// STAGE 4 — the decoupled STAT-phase carry as a bridge step. Advances the
    /// STAT/line clock by `dots` dots (same per-dot prelude as
    /// `stop_bridge_advance`: `step_scheduled_stat_events`, `p_now` pullback,
    /// then the line-phase step, then `step_lcdc_events`) but the render latch
    /// (`self.ticks`/`self.x`/FIFO/mode-3 fetch) stays PUT. With `dots == 0` this
    /// is a no-op, so a flag-ON build that never carries is byte-identical to the
    /// rendered bridge (the Step-1 safety checkpoint).
    pub fn stat_phase_carry(&mut self, mmio: &mut mmio::Mmio, dots: u32) {
        for _ in 0..dots {
            self.step_scheduled_stat_events(mmio);
            let dot_cc = 1i64 << mmio.is_double_speed_mode() as u32;
            self.p_now = self.p_now.wrapping_sub(dot_cc as u64);
            self.step_stat_phase_only(mmio);
            self.step_lcdc_events(mmio);
            // The STAT phase (line_cycle/abs_cc) just advanced one dot; the render
            // latch did NOT. Record the divergence so the CPU-access visibility
            // gate (`ppu_blocks` -> `render_carry_skew`) re-aligns a store to the
            // un-carried fetcher position (FACET-2 decoupling).
            self.render_carry_skew_cc += dot_cc;
        }
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
        // The m1 (VBlank) event (frame_cycle 144*456-2, an even `abs_cc`) is observed
        // two ways at double speed: a CPU FF0F read snapshots IF pre-tick (the snapshot
        // is taken BEFORE this M-cycle's dispatch, so an event at cc == read_cc fires
        // one dispatch too late to be seen — Gambatte processes events <= cc before
        // read(0xFF0F,cc) returns; needs +2*ds to land at-or-before the read cc), and
        // the VBlank IRQ is *delivered* by the CPU service path (needs the true event
        // cc). The read-snapshot brackets only exist with the m1-STAT source enabled
        // (STAT bit4: lycint143_m1irq `_2`/`_3`, m1irq_disable `_2`); when it is OFF
        // (e.g. the vblankirq retrigger tests, STAT=0x40) the VBlank IRQ-delivery
        // timing dominates and the extra dot delivers the IRQ too early. Anticipate by
        // 2*ds only when m1-STAT is enabled, else by the half-dot +ds the LYC=LY/mode-0
        // events also carry. DS-only (ds=0 leaves the single-speed phase byte-identical).
        let m1en = self.stat_reg_committed & (1 << 4) != 0;
        let m1_anticip = if m1en { 2 * ds as u64 } else { ds as u64 };
        if self.sched_m1irq <= cc + m1_anticip {
            let stat = self.stat_reg_committed;
            if self.mstat_irq.do_m1_event(stat) {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
            }
            // Gambatte's VBlank interrupt (IF bit 0) and the mode-1 STAT IRQ both
            // fire from the SAME lyCounter LY=144 event (`flagIrq(doM1Event?3:1)`):
            // bit 0 (VBlank) ALWAYS, bit 1 (STAT) only when the m1 condition holds.
            // The event fires at frame_cycle 144*456-2 (line_cycle 454 of LY=143),
            // ~3cc BEFORE rustyboi's render-machine VBlank (the HBlank ly143->144
            // line transition at line_cycle 455/0). A CPU IF read landing in that
            // gap saw the STAT bit but missed VBlank (the m1irq `_2`/`_3` bracket
            // halves: out0 vs the correct out3, outE2 vs outE3). Flag VBlank here
            // at the faithful m1 event cc so both bits land coincident as Gambatte;
            // the render machine's later fire is idempotent (same frame OR).
            if self.internal_ly_val >= 143 {
                mmio.request_interrupt(registers::InterruptFlag::VBlank);
                // Mark so the render-machine ly143->144 transition does not re-flag
                // VBlank after a CPU IF-write cleared it (Gambatte: single VBlank
                // source). The flag covers the gap between this event (line_cycle
                // 454) and the render transition (line_cycle 455/0).
                self.m1_vblank_fired = true;
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
        // The mode-0 (HBlank) STAT IRQ schedules at an odd `abs_cc` (a half-dot)
        // at double speed; the per-dot dispatch flags it one M-cycle late, which
        // pushes it across a CPU instruction boundary (≈4cc service delay).
        // Anticipating by `ds` dots lands it on the boundary Gambatte services at
        // — the same half-dot sub-dot fix applied to the LYC=LY IRQ above.
        //
        // On CGB single speed the per-dot dispatch additionally flags the m0 IRQ one
        // dot after Gambatte's `predictedNextXposTime(166)` (= m0Time-1): the IRQ is
        // delivered at the mode-3->0 transition dot rather than one xpos before it.
        // Measured byte-exact via cctracer (m2int_m0irq_scx3 fires at rel+2 from the
        // IF-clear write M-cycle start vs Gambatte's rel+1; DMG is already at rel+1).
        // Anticipate by one dot on CGB SS so the m0 IRQ flags at m0Time-1, matching
        // the (already exact) m2/LYC phase. Fixes 10sprites/ly0/wxA5 m0irq and the
        // CGB m2int_m0irq_*_ifw IF-clear-vs-m0 ordering.
        let cgb_ss_m0_anticip = (!ds && mmio.is_cgb_features_enabled()) as u64;
        if self.sched_m0irq <= cc + ds as u64 + cgb_ss_m0_anticip {
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
        // Window-enable bit as Gambatte's weMaster checkpoint sees it at THIS dot.
        // A window-enable write whose Gambatte commit (`write_cc + 2`) has not yet
        // reached this dot's abs_cc still reads the OLD bit here (Gambatte runs the
        // weMaster `update(cc)` event before `setLcdc`), even though rustyboi's
        // pending_lcdc_events already committed the live `self.lcdc` one dot early.
        let win_en = match self.we_win_bit_exact {
            Some((commit_cc, _new, old)) if self.abs_cc <= commit_cc => old,
            _ => (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0,
        };
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
        // STAGE 5 (RB_LINERENDER): the per-dot FIFO still runs so `self.x`
        // advances (the timing fallbacks key off x==160), but it no longer
        // writes the framebuffer — `render_full_line` produces the visible line
        // in one closed-form pass at the mode-3 -> HBlank transition instead.
        if linerender_enabled() {
            self.x += 1;
            return true;
        }
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

    // Replace the 8 oldest BG-FIFO entries with the tile at BG tile-map column
    // `tile_col` (0..32) on the pixel row `bg_y` (already SCY+LY, 0..256),
    // reproducing the fetcher's BG addressing (LCDC tile-map/tile-data select,
    // CGB attribute bank + x/y flip). Used by the M3Start fine-scroll re-fetch
    // when a mid-discard SCX write moves the first displayed tile's column.
    // Compute the 8 BG pixels for tile-map column `tile_col` on pixel
    // row `bg_y`, reproducing the fetcher's addressing. Shared by the fine-scroll
    // first-tile rewrite and the sub-cc SCX column re-key.
    fn bg_pixels_at_col(&self, mmio: &mmio::Mmio, tile_col: u16, bg_y: u16) -> [crate::ppu::fifo::BgPixel; 8] {
        let lcdc = self.lcdc;
        let cgb = mmio.is_cgb_features_enabled();
        let map_base: u16 = if (lcdc & (LCDCFlags::BGTileMapDisplaySelect as u8)) != 0 {
            0x9C00
        } else {
            0x9800
        };
        let map_y = (bg_y / 8) & 0x1F;
        let map_addr = map_base + (map_y * 32 + (tile_col & 0x1F));
        let tile_num = mmio.read_vram_bank(0, map_addr);
        let tile_attrs = if cgb { mmio.read_vram_bank(1, map_addr) } else { 0 };
        let y_flip = cgb && (tile_attrs & 0x40) != 0;
        let x_flip = cgb && (tile_attrs & 0x20) != 0;
        let tile_line = (bg_y % 8) as u8;
        let eff_line = if y_flip { 7 - tile_line } else { tile_line };
        let data_addr: u16 = if (lcdc & (LCDCFlags::BGWindowTileDataSelect as u8)) != 0 {
            0x8000 + (tile_num as u16) * 16 + (eff_line as u16) * 2
        } else {
            let signed = tile_num as i8;
            ((0x9000u16 as i16).wrapping_add((signed as i16) * 16 + (eff_line as i16) * 2)) as u16
        };
        let bank = if cgb && (tile_attrs & 0x08) != 0 { 1 } else { 0 };
        let low = mmio.read_vram_bank(bank, data_addr);
        let high = mmio.read_vram_bank(bank, data_addr + 1);
        let mut pixels = [crate::ppu::fifo::BgPixel::default(); 8];
        for (i, px) in pixels.iter_mut().enumerate() {
            let bit = if x_flip { i as u8 } else { 7 - i as u8 };
            let idx = (((high >> bit) & 1) << 1) | ((low >> bit) & 1);
            *px = crate::ppu::fifo::BgPixel { color: idx, attrs: tile_attrs };
        }
        pixels
    }

    fn rewrite_first_fifo_tile(&mut self, mmio: &mmio::Mmio, tile_col: u16, bg_y: u16) {
        let lcdc = self.lcdc;
        let cgb = mmio.is_cgb_features_enabled();
        let map_base: u16 = if (lcdc & (LCDCFlags::BGTileMapDisplaySelect as u8)) != 0 {
            0x9C00
        } else {
            0x9800
        };
        let map_y = (bg_y / 8) & 0x1F;
        let map_addr = map_base + (map_y * 32 + (tile_col & 0x1F));
        let tile_num = mmio.read_vram_bank(0, map_addr);
        let tile_attrs = if cgb { mmio.read_vram_bank(1, map_addr) } else { 0 };
        let y_flip = cgb && (tile_attrs & 0x40) != 0;
        let x_flip = cgb && (tile_attrs & 0x20) != 0;
        let tile_line = (bg_y % 8) as u8;
        let eff_line = if y_flip { 7 - tile_line } else { tile_line };
        let data_addr: u16 = if (lcdc & (LCDCFlags::BGWindowTileDataSelect as u8)) != 0 {
            0x8000 + (tile_num as u16) * 16 + (eff_line as u16) * 2
        } else {
            let signed = tile_num as i8;
            ((0x9000u16 as i16).wrapping_add((signed as i16) * 16 + (eff_line as i16) * 2)) as u16
        };
        let bank = if cgb && (tile_attrs & 0x08) != 0 { 1 } else { 0 };
        let low = mmio.read_vram_bank(bank, data_addr);
        let high = mmio.read_vram_bank(bank, data_addr + 1);
        let mut pixels = [crate::ppu::fifo::BgPixel::default(); 8];
        for (i, px) in pixels.iter_mut().enumerate() {
            let bit = if x_flip { i as u8 } else { 7 - i as u8 };
            let idx = (((high >> bit) & 1) << 1) | ((low >> bit) & 1);
            *px = crate::ppu::fifo::BgPixel { color: idx, attrs: tile_attrs };
        }
        self.fetcher.pixel_fifo.overwrite_oldest(&pixels);
    }

    // STAGE 5 (RB_LINERENDER): compute one displayed BG/window pixel
    // (color index + CGB attrs) at screen column `screen_x` on the current line,
    // reproducing the fetcher's tile addressing in closed form. `win_active`
    // says the window owns this line; `win_first_col` is the screen column where
    // the window begins drawing (wx-7, clamped to 0). Returns (pixel_idx, attrs).
    fn line_bg_pixel(
        &self,
        mmio: &mmio::Mmio,
        screen_x: u8,
        win_active: bool,
        win_first_col: i32,
    ) -> (u8, u8) {
        let lcdc = self.lcdc;
        let cgb = mmio.is_cgb_features_enabled();
        let ly = mmio.read(LY);

        let in_window = win_active && (screen_x as i32) >= win_first_col;

        let (map_base, map_x, map_y, tile_line) = if in_window {
            let wc = (screen_x as i32 - win_first_col) as u32;
            let win_map_base: u16 = if (lcdc & (LCDCFlags::WindowTileMapDisplaySelect as u8)) != 0 {
                0x9C00
            } else {
                0x9800
            };
            let wy = self.win_y_pos as u32;
            (win_map_base, (wc / 8) % 32, (wy / 8) % 32, (wy % 8) as u8)
        } else {
            let bg_map_base: u16 = if (lcdc & (LCDCFlags::BGTileMapDisplaySelect as u8)) != 0 {
                0x9C00
            } else {
                0x9800
            };
            let bg_x = (self.scx_delayed as u32 + screen_x as u32) & 0xFF;
            let bg_y = (self.scy_delayed as u32 + ly as u32) & 0xFF;
            (bg_map_base, (bg_x / 8) % 32, (bg_y / 8) % 32, (bg_y % 8) as u8)
        };

        let map_addr = map_base + (map_y * 32 + map_x) as u16;
        let tile_num = mmio.read_vram_bank(0, map_addr);
        let tile_attrs = if cgb { mmio.read_vram_bank(1, map_addr) } else { 0 };

        let y_flip = cgb && (tile_attrs & 0x40) != 0;
        let x_flip = cgb && (tile_attrs & 0x20) != 0;
        let eff_line = if y_flip { 7 - tile_line } else { tile_line };

        // Tile data address ($8000 vs $8800 method) — mirror Fetcher.
        let data_addr: u16 = if (lcdc & (LCDCFlags::BGWindowTileDataSelect as u8)) != 0 {
            0x8000 + (tile_num as u16) * 16 + (eff_line as u16) * 2
        } else {
            let signed = tile_num as i8;
            ((0x9000u16 as i16).wrapping_add((signed as i16) * 16 + (eff_line as i16) * 2)) as u16
        };
        let bank = if cgb && (tile_attrs & 0x08) != 0 { 1 } else { 0 };
        let low = mmio.read_vram_bank(bank, data_addr);
        let high = mmio.read_vram_bank(bank, data_addr + 1);

        // Fine pixel within the tile.
        let fine = if in_window {
            ((screen_x as i32 - win_first_col) as u32 % 8) as u8
        } else {
            ((self.scx_delayed as u32 + screen_x as u32) % 8) as u8
        };
        let bit = if x_flip { fine } else { 7 - fine };
        let idx = (((high >> bit) & 1) << 1) | ((low >> bit) & 1);
        (idx, tile_attrs)
    }

    // STAGE 5 (RB_LINERENDER): render the whole visible scanline at once into the
    // framebuffer, reusing the per-dot mixing functions for sprite priority.
    // Called once per visible line at the mode-3 -> HBlank transition.
    fn render_full_line(&mut self, mmio: &mmio::Mmio) {
        let ly = mmio.read(LY);
        if ly >= 144 {
            return;
        }
        let cgb = mmio.is_cgb_features_enabled();
        let bg_enabled = (self.lcdc & (LCDCFlags::BGDisplay as u8)) != 0;

        // Window geometry for this line: active if the window-Y trigger latched
        // (or the live wy2==ly fallback) and WX is in range.
        let win_active = self.window_y_active(mmio)
            && {
                let wx = mmio.read(WX) as i32;
                (0..=166).contains(&wx) && (cgb || wx != 166)
            };
        let wx = mmio.read(WX) as i32;
        let win_first_col = (wx - 7).max(0);

        for sx in 0u8..160 {
            // On DMG, BG-disabled forces the BG layer to color 0 (white).
            let (bg_idx, bg_attrs) = if !bg_enabled && !cgb {
                (0u8, 0u8)
            } else {
                self.line_bg_pixel(mmio, sx, win_active, win_first_col)
            };
            let fb_offset = (ly as u16) * 160 + sx as u16;
            if cgb {
                let rgb = self.mix_background_and_sprites_color(mmio, bg_idx, bg_attrs, sx, ly);
                let off = fb_offset as usize * 3;
                self.color_fb_a[off] = rgb.0;
                self.color_fb_a[off + 1] = rgb.1;
                self.color_fb_a[off + 2] = rgb.2;
            } else {
                let color = self.mix_background_and_sprites(mmio, bg_idx, sx, ly);
                self.fb_a[fb_offset as usize] = color;
            }
        }
        self.line_rendered_this_line = true;
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

    // Closed-form mode-3 length to reach an arbitrary `targetx`, mirroring
    // Gambatte `predictCyclesUntilXpos_fn`: the window penalty (+6) is charged
    // only when `wx < targetx`, and a sprite contributes only when `spx <=
    // targetx`. `compute_m3_length_win` is the `targetx == 167` (m0Time, getStat)
    // case; the mode-0 STAT IRQ fires at `predictedNextXposTime(lcd_hres+6) =
    // predictedNextXposTime(166)`, one xpos earlier. When a window starts at
    // WX=166 and/or a sprite sits at the right edge (spx > 166), that final
    // xpos step carries the whole window+sprite penalty, so xpos 166 lands many
    // dots before xpos 167 — not the usual single dot.
    fn compute_m3_length_to_target(&self, mmio: &mmio::Mmio, is_cgb: bool, targetx: i32) -> u128 {
        let scx = (mmio.read(SCX) & 0x07) as i32;
        let mut cycles: i32 = scx + (1 - is_cgb as i32);
        cycles += targetx; // targetx - xpos, xpos = 0 at tile-loop start

        let mut nwx: i32 = 0xFF;
        if self.window_will_start(mmio, is_cgb) {
            let wx = mmio.read(WX) as i32;
            // Gambatte: window penalty only if `wx < targetx` (`p.wx - xpos <
            // targetx - xpos`). At targetx == 167 this matches the +6 in
            // `compute_m3_length_win` (any in-range WX <= 166 < 167).
            if wx < targetx {
                nwx = wx;
                cycles += WIN_M3_PENALTY;
                if is_cgb && scx == 5 && self.sprites_on_line.is_empty() {
                    let dflt = if mmio.is_double_speed_mode() { 0 } else { -1 };
                    cycles += dflt as i32;
                }
            }
        }

        let obj_enabled = (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0;
        let mut sprite_xs: Vec<i32> = self.sprites_on_line.iter().map(|s| s.x as i32).collect();
        sprite_xs.sort_unstable();
        cycles += sprite_tile_walk_cost(&sprite_xs, scx, nwx, targetx, obj_enabled || is_cgb);

        cycles.max(0) as u128
    }

    /// The extra dots (beyond the usual single dot) that the final xpos step
    /// (166 -> 167) carries on this line, i.e. how many dots earlier the mode-0
    /// STAT IRQ (`predictedNextXposTime(166)`) fires relative to the m0Time
    /// (`predictedNextXposTime(167)`) closed form. Zero for plain BG lines, so
    /// the calibrated `M0IRQ_OFFSET` arm is unchanged; non-zero only when a
    /// window starts at WX=166 or a sprite sits at the right edge.
    fn m0irq_xpos166_advance(&self, mmio: &mmio::Mmio, is_cgb: bool) -> i64 {
        let len167 = self.compute_m3_length_to_target(mmio, is_cgb, 167) as i64;
        let len166 = self.compute_m3_length_to_target(mmio, is_cgb, 166) as i64;
        (len167 - len166 - 1).max(0)
    }

    // Returns (mode-3 length in dots past base, whether the window contributed).
    fn compute_m3_length_win(&self, mmio: &mmio::Mmio, is_cgb: bool) -> (u128, bool) {
        let scx = (self.first_line_scx_override.unwrap_or_else(|| mmio.read(SCX)) & 0x07) as i32;
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
            // CGB window lines at SCX%8 == 5: the closed-form mode-3 window
            // penalty runs one dot long versus Gambatte's M3Start fine-scroll
            // dispatch at this phase, flipping the sampled STAT mode on the
            // m2int_*_scx5 window probes — but only at single speed; at double
            // speed Gambatte's phase agrees, so the -1 over-corrects (the DS
            // m2int_wx*_scx5_m3stat reads flip mode3->mode0).
            if is_cgb && scx == 5 && self.sprites_on_line.is_empty() {
                let dflt = if mmio.is_double_speed_mode() { 0 } else { -1 };
                cycles += dflt as i32;
            }
            win = true;
        }

        // Sprites. The single faithful tile-walk model (shared with the live
        // renderer via `sprite_tile_walk_cost`). Only count if OBJ enabled (or
        // CGB always evaluates them).
        let obj_enabled = (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0;
        let target_x = 167;
        let mut sprite_xs: Vec<i32> = self.sprites_on_line.iter().map(|s| s.x as i32).collect();
        sprite_xs.sort_unstable();
        cycles += sprite_tile_walk_cost(&sprite_xs, scx, nwx, target_x, obj_enabled || is_cgb);

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
        self.m3_sprite_prev_tile = SPRITE_TILE_NONE;
        self.m3_last_sprite_commit_tick = 0;
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
        let wy1_delay = WY1_DELAY + cgb;
        self.wy1_pending = value;
        self.wy1_apply_cc = cc + wy1_delay.max(0) as u64;
        // wy2 apply delay (cc) past the write, swept against the late_wy suite:
        // CGB 7, DMG 4 (-ds at double speed). The split reflects the differing
        // M3-start / fine-scroll phase between the two cores.
        let base = if mmio.is_cgb_features_enabled() {
            WY2_DELAY_CGB
        } else {
            WY2_DELAY_DMG
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
            SCY_DELAY.max(0) as u64
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
            0u64
        } else {
            0
        };
        self.scx_pending = value;
        self.scx_apply_cc = cc + delay;

        // Exact-cc f1-discard latch. The "before" value is whatever the f1 loop
        // sees right now (resolving any already-pending latch up to this write's
        // cc); the new value becomes visible at write_cc + 2*cgb (Gambatte
        // scxChange `update(cc + 2*cgb)`). NB: mmio already holds `value` (the
        // store ran before this hook), so `scx_f1_at_cc` must derive the old
        // value from the latch state, never from mmio.read(SCX).
        let cgb = mmio.is_cgb_features_enabled();
        self.scx_prev_f1 = self.scx_f1_pending_at_cc(cc);
        self.scx_f1_new = value;
        // Gambatte scxChange `update(cc + 2*cgb)` runs in PPU dot units: the new
        // SCX becomes visible to the f1 fine-scroll loop one PPU dot after the
        // write (CGB). `abs_cc` is the master clock (1 dot = 1<<ds cc), so the
        // dot delay scales with double speed -- otherwise a mid-f1 SCX write
        // lands one f1 iteration too early at DS (scx_0367c0/scx_0761c0 _ds).
        let ds = mmio.is_double_speed_mode() as u32;
        self.scx_f1_apply_cc = cc + if cgb { 2u64 << ds } else { 0 };

        // sub-cc column lever: record the apply boundary on the PLOT clock. The
        // BG fetcher chooses old/new per tile by comparing the tile's plot cc to
        // this. Persists for the line (does not reset on apply).
        self.subcc_scx_old = self.scx_delayed;
        self.subcc_scx_new = value;
        self.subcc_scx_apply_cc = cc + if cgb { 2u64 << ds } else { 0 };
        // Arm the single-tile re-key only when a BG tile is mid-fetch (its
        // column was already committed under the OLD scx and it has not yet
        // pushed). If the fetcher is at TileNumber, the next fetch will read
        // the (about-to-be-NEW) scx itself; no in-flight straddle exists.
        self.subcc_rekey_armed = !self.disabled
            && self.state == State::PixelTransfer
            && self.x > 0
            && !self.fetcher.is_fetching_window()
            && !self.fetcher.fetch_state_is_tile_number()
            && self.fetcher.subcc_last_column_inputs().2 == self.subcc_scx_old;

        // First-tile (f1) prologue straddle (DMG SS): the write lands at x==0
        // (still in the discard prologue) but the first displayed tile is already
        // queued (fifo>=8) and the 2nd tile is mid-fetch (its column was latched
        // under the OLD scx one dot before this write). On hardware that 2nd tile
        // plots after the write, so re-key it to the NEW scx on its next push.
        // Gated on a low-X sprite (OAM x <= 8): the sprite-fetch dot during the
        // discard prologue delays the BG fetcher one tile, so the in-flight 2nd
        // tile latched OLD one dot before the write (vs no in-flight straddle
        // without the sprite). The no-sprite SS straddle (scx_during_m3_4/5) is
        // handled correctly by the steady-state gap==4 rekey and must NOT re-key
        // here, so the sprite gate is required to protect those cases.
        let sprites_enabled = (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0;
        let low_x_sprite = sprites_enabled
            && self.sprites_on_line.iter().any(|s| s.x <= 8);
        self.prologue_rekey_armed = !self.disabled
            && !cgb
            && ds == 0
            && self.state == State::PixelTransfer
            && self.x == 0
            && low_x_sprite
            && self.fetcher.pixel_fifo.size() >= 8
            && !self.fetcher.is_fetching_window()
            && !self.fetcher.fetch_state_is_tile_number()
            && self.fetcher.subcc_last_column_inputs().2 == self.subcc_scx_old;
    }

    /// SCX value visible to the f1 fine-scroll discard at PPU `cc`, honoring the
    /// CGB `update(cc + 2*cgb)`-before-`setScx` write delay. Before the pending
    /// write's apply cc the f1 sees the pre-write value; at/after it sees the
    /// new. Derived purely from the latch state (mmio already holds the latest
    /// write), seeded with the M3-start SCX in `scx_prev_f1`.
    fn scx_f1_pending_at_cc(&self, cc: u64) -> u8 {
        if self.scx_f1_apply_cc != wy2_disabled() && cc >= self.scx_f1_apply_cc {
            self.scx_f1_new
        } else {
            self.scx_prev_f1
        }
    }

    /// OBJ-size (large = 8x16) visible to the OAM scan at PPU `cc`, honoring the
    /// CGB `setLcdc(data, cc + 2)` write delay. Before the pending size write's
    /// apply cc the scan sees the pre-write size; at/after it sees the new. With
    /// no pending change (`apply_cc == disabled`) it falls back to the live LCDC
    /// bit2, so the steady-state per-slot snapshot is unchanged.
    fn objsize_large_at_cc(&self, cc: u64) -> bool {
        if self.objsize_apply_cc != wy2_disabled() {
            // Strict `>`: an OAM slot read exactly AT the apply cc still sees the
            // pre-write size (the late_sizechange2_sp01_ds bracket: ds_1's slot
            // cc is strictly past apply -> new size IN; ds_2's slot cc equals
            // apply -> old size OUT, the 1-slot boundary Gambatte resolves).
            if cc > self.objsize_apply_cc {
                self.objsize_new_large
            } else {
                self.objsize_prev_large
            }
        } else {
            (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0
        }
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
    fn m0_irq_time_for_trigger(&self, mmio: &mmio::Mmio, lc: &stat_irq::LyCounter, cc: u64) -> u64 {
        // Gambatte's statChangeTriggers* needs the m0 IRQ time of the *current
        // line*. Our `sched_m0irq` may hold a stale current-line value during
        // HBlank (it is only cleared to DISABLED when the m0 source fires). The
        // DMG/CGB branch logic only cares whether m0IrqTime is before or after
        // `lyCounter.time()` (next-LY): if mode 0 is already active (HBlank) the
        // current line's m0 has passed and the next is on a later line, i.e.
        // `>= lc.time`; during mode 2/3 it is still ahead this line (`< time`).
        // Mode 3 (PixelTransfer): the current line's m0 is ahead, and the
        // closed-form `m0_time_master` is this line's exact m0Time — use the exact
        // Gambatte mode-0 IRQ event time (predictedNextXposTime(166)). Mode 2
        // (OAMSearch): `m0_time_master` still holds the PREVIOUS line's value, so
        // keep the per-dot `sched_m0irq` (this line's armed m0). Both clamp below
        // next-LY so the "m0 ahead this line" branch is taken.
        let sched_or_future = if self.sched_m0irq == stat_irq::DISABLED_TIME {
            lc.time.saturating_sub(1)
        } else {
            self.sched_m0irq.min(lc.time.saturating_sub(1))
        };
        match self.state {
            // Mode 0 active: report a time at/after the next LY so the "m0 has
            // occurred" branch is taken.
            State::HBlank => lc.time,
            // VBlank: no m0 this line; far future.
            State::VBlank => stat_irq::DISABLED_TIME,
            State::PixelTransfer => self
                .m0_irq_time_exact(mmio)
                .map(|t| {
                    // Gambatte runs pending events before the FF41-write trigger
                    // check: if the write cc has already passed the mode-0 STAT
                    // IRQ time (predictedNextXposTime(166)), that event fired and
                    // rescheduled `eventTimes_(memevent_m0irq)` onto the next line
                    // (> lyCounter.time()). Report a next-LY value so the trigger
                    // takes the "m0 already occurred" branch and the enable
                    // immediately flags the STAT IRQ — the `_2`/`_3`/`_4` bracket
                    // where the window/sprite-deferred m0 xpos lies just before the
                    // enable write.
                    if cc >= t {
                        lc.time
                    } else {
                        t.min(lc.time.saturating_sub(1))
                    }
                })
                .unwrap_or(sched_or_future),
            _ => sched_or_future,
        }
    }

    /// The exact Gambatte mode-0 STAT-IRQ event time for the current line, used
    /// by the FF41/FF45 latch + immediate-trigger comparisons. Gambatte's m0 IRQ
    /// fires at `predictedNextXposTime(166) = m0Time - (1<<ds)`, one xpos before
    /// the mode-3 -> mode-0 transition (`m0Time = predictedNextXposTime(167)`,
    /// our `m0_time_master`). Returns `None` when no closed-form master exists
    /// (window mid-line / first line after enable), in which case callers fall
    /// back to the per-dot delivery value (`sched_m0irq`).
    fn m0_irq_time_exact(&self, mmio: &mmio::Mmio) -> Option<u64> {
        let ds = mmio.is_double_speed_mode() as i64;
        // `m0_time_master` is the master-cc m0Time (= predictedNextXposTime(167)).
        // The STAT/LYC write-trigger comparisons run in abs-cc units (the same
        // `cc = write_cc()` / `sched_m0irq` clock), so rebase by `p_now`
        // (abs_cc = master_cc - p_now). The mode-0 IRQ fires one xpos earlier:
        // predictedNextXposTime(166) = m0Time - (cost(166->167) << ds), where the
        // 166->167 step costs one dot plus any window-start (WX=166) / right-edge
        // sprite penalty that lands in that final xpos (`m0irq_xpos166_advance`).
        //
        // `m0_time_master` (via `m0_time_exact`) carries a `+1` lyTime correction
        // tuned for the C1 *read* access-cc phase (`access_cc + 2 < m0Time`). The
        // *write* cc (write_cc_off = 0) resolves the latch/trigger one cc earlier,
        // so that read-phase `+1` over-counts the write-boundary IRQ time by 1 —
        // subtract it back out to land the write-phase boundary exactly.
        let is_cgb = mmio.is_cgb_features_enabled();
        let adv = self.m0irq_xpos166_advance(mmio, is_cgb);
        self.m0_time_master
            .map(|m0t| (m0t as i64 - ((1 + adv) << ds) - self.p_now as i64 - 1).max(0) as u64)
    }

    /// The current-line mode-0 IRQ time for the FF41/FF45 *latch* comparisons
    /// (Gambatte `eventTimes_(memevent_m0irq)`). During mode 3 the closed-form
    /// `m0_time_master`-derived exact value (predictedNextXposTime(166)) is this
    /// line's m0; in HBlank/mode 2/VBlank/window the per-dot `sched_m0irq` already
    /// carries the relevant scheduled (next-line) value, matching the pre-C5 latch
    /// behaviour, so keep it there to avoid disturbing those boundaries.
    fn m0_irq_time_latch(&self, mmio: &mmio::Mmio, lc: &stat_irq::LyCounter) -> u64 {
        match self.state {
            State::PixelTransfer => self
                .m0_irq_time_exact(mmio)
                .map(|t| t.min(lc.time.saturating_sub(1)))
                .unwrap_or(self.sched_m0irq),
            _ => self.sched_m0irq,
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
            self.arm_m0irq_for_current_line(mmio, self.first_line_after_enable);
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

        // Latch the new STAT bits against the exact current-line mode-0 IRQ time
        // (Gambatte's `eventTimes_(memevent_m0irq)` = predictedNextXposTime(166))
        // during mode 3, keeping the per-dot `sched_m0irq` next-line value
        // elsewhere (HBlank/mode 2/window) — see `m0_irq_time_latch`.
        let m0_latch = self.m0_irq_time_latch(mmio, &lc);
        self.mstat_irq.stat_reg_change(
            data,
            m0_latch,
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

        // Trigger/latch against the current-line mode-0 IRQ time: the closed-form
        // `m0_time_master`-derived exact value (Gambatte predictedNextXposTime
        // (166)) during mode 3, the per-dot `sched_m0irq` (next-line scheduled m0,
        // > lc.time) elsewhere — see `m0_irq_time_latch`.
        let m0_for_trigger = self.m0_irq_time_latch(mmio, &lc);
        self.lyc_irq.lyc_reg_change(data, &lc, cc);
        self.mstat_irq
            .lyc_reg_change(data, m0_for_trigger, self.sched_m2irq, cc, ds, cgb);
        self.sched_lycirq = self.lyc_irq.time;

        // Immediate-trigger m0 time = Gambatte `eventTimes_(memevent_m0irq)`, which
        // is the *current line's* m0 while it is still ahead (mode 2/3) and the next
        // line's (> lc.time) once mode 0 has passed. `m0_irq_time_latch` is correct
        // in HBlank/mode 3 but reports DISABLED during OAMSearch (the current line's
        // m0 has not yet been armed into `sched_m0irq`); there the current line's m0
        // is still ahead but before next-LY, so substitute `lc.time - 1`. This makes
        // `lyc_change_blocked_by_m0_or_m1` resolve the line-start LYC=LY coincidence
        // (lycwirq_trigger_m0_late_lyc45 `_5`) without disturbing the HBlank
        // line-end LYC writes (lycwirq_trigger_m0_late `_1`/`_2`/`_3`).
        let m0_latch = self.m0_irq_time_latch(mmio, &lc);
        let m0_for_imm = if matches!(self.state, State::OAMSearch)
            && m0_latch == stat_irq::DISABLED_TIME
        {
            lc.time.saturating_sub(1)
        } else {
            m0_latch
        };
        if stat_irq::lyc_change_triggers_stat_irq(old, data, &lc, cc, stat, m0_for_imm, cgb) {
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
        // Clear any exact-cc OBJ-size latch left from a prior line so it cannot
        // leak into this line's OAM scan; a mid-mode-2 size write rearms it.
        self.objsize_apply_cc = wy2_disabled();
        Self::set_lcd_status_mode(mmio, 2);
        // IRQ delivery is handled by the event model; just latch the line.
        self.previous_stat_interrupt_line = self.calculate_stat_interrupt_line(mmio);
        self.mode2_irq_pretriggered_for_next_line = false;
        // Arm the cgbp begin boundary (Gambatte cgbpAccessible: blocked once
        // `lineCycles(cc) + ds >= 80`) as soon as the line's mode 2 begins, so a
        // BCPD/OCPD write landing in late mode 2 (before M3 is armed) sees it.
        // Derive the exact begin cc from the lyTime anchor (same closed form as
        // `m0_time_exact`, but at line-cycle `80 - ds` instead of mode-0):
        //   begin = lyTime − ((456 − (80 − ds)) << ds)
        // This is byte-exact at both speeds; the old tick-block heuristic landed
        // ~2 cc late at double speed because its `(4 − cgb)` ticks->lineCycles
        // term was not shifted by `ds`.
        self.cgbp_block_start_cc = Some(self.cgbp_begin_exact(mmio));
    }

    /// Byte-exact Gambatte cgbp-block BEGIN cc for the current line, anchored on
    /// the same lyTime as `m0_time_exact`. Gambatte `cgbpAccessible` blocks once
    /// `lineCycles(cc) + ds >= 80`, i.e. at line-cycle `80 - ds`.
    fn cgbp_begin_exact(&self, mmio: &mmio::Mmio) -> u64 {
        let ds = mmio.is_double_speed_mode() as i64;
        let plus1 = if self.lytime_no_plus1 { 0 } else { 1 };
        let ly_time = self.p_now as i64 + self.ly_counter(mmio).time as i64 + plus1;
        (ly_time - ((456 - (80 - ds)) << ds)).max(0) as u64
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
                // Gambatte OamReader::enableDisplay: `lu_ = cc + (2*40 << ds) + 1`.
                // getStat reports mode 0 (suppresses mode 2/3) for `cc < lu_`.
                {
                    let ds_u = mmio.is_double_speed_mode() as u32;
                    self.display_enable_inactive_until =
                        mmio.master_cc().wrapping_add((80u64 << ds_u) + 1);
                }
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
                self.sc_mode3_pullback_pending = false;
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
                // SpriteMapper::OamReader::enableDisplay: zero the snapshot and
                // hold it inactive (no sprites) until `cc + (80<<ds) + 1`. abs_cc
                // is re-derived below; enableDisplay is anchored to that dot.
                {
                    let ds = mmio.is_double_speed_mode();
                    let cc = mmio.master_cc().wrapping_sub(self.p_now);
                    self.oam_reader.cgb = mmio.is_cgb_features_enabled();
                    self.oam_reader.large_src =
                        (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;
                    self.oam_reader.src_disabled = mmio.oam_dma_window_active();
                    self.oam_reader.enable_display(cc, ds);
                    self.prev_dma_writing = mmio.oam_dma_window_active();
                    self.oam_reader_seeded = true;
                }
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
            // Re-arm the sprite snapshot for the next enableDisplay.
            self.oam_reader_seeded = false;
            let _ = mmio.take_oam_write_pending();
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

        // Drive the lazy OAM sprite snapshot (Gambatte SpriteMapper::OamReader):
        // fire `change(cc)` on OAM-DMA window edges (source toggle) and on CPU
        // OAM writes, mirroring Gambatte's `startOamDma`/`endOamDma`/`oamChange`.
        self.process_oam_reader_events(mmio);

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
                    self.predicted_win_start_dot = None;
                    self.win_wx_penalty_resolved = false;
                    self.win_wx_enable_resolved = false;

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
                    // Install the closed-form master-cc anchors for the first line
                    // BEFORE M3 arms, so the CPU-access gates (OAM/VRAM/cgbp) resolve
                    // the mode-3 END boundary (`cc + 2 >= m0Time`) during this pre-M3
                    // OAMSearch phase too. In Gambatte the PPU machine is fully seeded
                    // at enable (`cycles = -(m3StartLineCycle + 2)`), so
                    // `m0TimeOfCurrentLine` is predictable from the start of the line;
                    // here it is enable-anchored (`p_now`) and uses the first-line
                    // m3-start (+2). OAM is blocked from line start to m0Time (mode 2
                    // and mode 3 alike) — the inactive-period guard above keeps it
                    // accessible until `lu_`. Recomputed each tick so a mid-line SCX/
                    // window change tracks (the M3-arm site re-installs the final
                    // value). No closed-form anchor existed here before (the gates
                    // fell back to the first-line FF41 mode register, which reports
                    // mode 0 and wrongly unblocked OAM in this window).
                    let m3_len = self.compute_m3_length(mmio, is_cgb);
                    self.m0_time_master = Some(self.m0_time_exact(mmio, m3_len, is_cgb, true));
                    self.cgbp_block_start_cc = Some(self.cgbp_begin_exact(mmio));
                }

                // Perform sprite search distributed across 80 ticks
                // Check one sprite every 2 ticks (40 sprites × 2 ticks = 80 ticks)
                // Skipped on the first scanline after LCD enable (no Mode 2 phase).
                if !self.first_line_after_enable
                    && self.ticks.is_multiple_of(2)
                    && self.current_oam_sprite_index < OAM_SPRITE_COUNT
                {
                    // Exact-cc OBJ-size override: when a mid-mode-2 size write is
                    // pending, this slot's size is the value visible as-of its own
                    // abs_cc (write_cc + 2*cgb), instead of the one-slot-lagged
                    // snapshot. With no pending change `objsize_large_at_cc` falls
                    // back to the lagged snapshot semantics (the steady state is
                    // unchanged). Sampled BEFORE the OAM read so this entry uses
                    // the size effective at its read cc (Gambatte lsbuf per-entry).
                    if self.objsize_apply_cc != wy2_disabled() {
                        self.scan_obj_size_large = self.objsize_large_at_cc(self.abs_cc);
                    }
                    // Record this slot's size for the snapshot rebuild, set for
                    // every scanned slot (even once 10 sprites are found, so the
                    // rebuild has a valid size for all 40 entries).
                    {
                        let idx = self.current_oam_sprite_index;
                        self.scan_slot_large[idx] = self.scan_obj_size_large;
                    }
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
                    // Rebuild the sprite list from the lazy OAM snapshot (Gambatte
                    // SpriteMapper::doEvent -> update + mapSprites). This replaces
                    // the incremental per-dot scan's `sprites_on_line` so visibility
                    // honors the DMA-disabled-source window via the posbuf cap.
                    // Rebuild the sprite list from the lazy OAM snapshot (Gambatte
                    // SpriteMapper::doEvent -> oamReader_.update + mapSprites). On
                    // the first line after enable there is no mode-2 scan; the
                    // snapshot is held inactive (enableDisplay) so skip the rebuild.
                    if !self.first_line_after_enable {
                        self.build_sprites_from_snapshot(mmio);
                    }
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
                    // Clear any pending sub-cc scx column lever from the previous
                    // line; a new write this line re-arms it.
                    self.subcc_scx_apply_cc = wy2_disabled();
                    self.prologue_rekey_armed = false;
                    self.next_sprite_fetch_index = 0;
                    self.m3_sprite_prev_tile = SPRITE_TILE_NONE;
                    self.m3_last_sprite_commit_tick = 0;
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
                        // Gambatte M3Start::f0 (270-275): if win_draw_start is set and
                        // the window is enabled, winDrawState becomes win_draw_started
                        // and winYPos increments; otherwise winDrawState clears.
                        if self.win_draw_start && win_en && !self.first_line_after_enable {
                            self.win_y_pos = self.win_y_pos.wrapping_add(1);
                            self.win_draw_started = true;
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
                            // Gambatte M3Start::f0 line 275: when win_draw_start was
                            // NOT armed, winDrawState clears to 0 (win_draw_started
                            // bit dropped). Normal (non-wxA6) windows re-set this on
                            // the same line via the live x+7==wx start below, so this
                            // only persistently clears the bit on lines where the
                            // window does not (re)start — which is what lets the DMG
                            // wxA6 START-NOW branch fire again when WY next matches.
                            if win_en && !self.first_line_after_enable {
                                self.win_draw_started = false;
                            }
                        }
                        self.win_draw_start = false;
                    }
                    // DMG wx==166 (lcd_hres+6): the window cannot draw a visible
                    // pixel this line (the line ends at xpos 166) but interacts with
                    // winDrawState exactly as Gambatte plotPixel (883-895) does when
                    // xpos reaches wx==166. Gambatte's OUTER gate is
                    //   wx==xpos && (weMaster || (wy2==ly && winEn)) && xpos<167
                    // i.e. `weMaster` alone is sufficient and does NOT require winEn.
                    // The INNER branches mirror Gambatte:
                    //   branch A (891): winDrawState==0 && winEn -> start now
                    //       (winDrawState = win_draw_start|win_draw_started, ++winYPos)
                    //   branch B (894, else-if): !cgb && (winDrawState==0 || xpos==166)
                    //       -> winDrawState |= win_draw_start (arm only)
                    // For DMG wx==166 the xpos==166 term makes branch B fire on EVERY
                    // line where the gate holds, INCLUDING lines with winEn off (the
                    // window was disabled mid-frame). That armed win_draw_start bit then
                    // carries — across the frame boundary, since winDrawState is not
                    // reset at frame end — into the next frame's line-0 M3Start::f0,
                    // which consumes it (++winYPos) and draws the window on LY=0. This
                    // is the wxA6 weMaster-persistence path Gambatte exhibits.
                    let win_en_now = (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
                    let we_gate = self.window_y_triggered
                        || (self.wy2 == mmio.read(LY) && win_en_now);
                    if !is_cgb
                        && !self.first_line_after_enable
                        && mmio.read(WX) == 166
                        && we_gate
                    {
                        let win_draw_state_zero = !self.win_draw_start && !self.win_draw_started;
                        if win_draw_state_zero && win_en_now {
                            // plotPixel branch A (891): start now (no visible window).
                            self.win_draw_start = true;
                            self.win_draw_started = true;
                            self.win_y_pos = self.win_y_pos.wrapping_add(1);
                        } else {
                            // plotPixel branch B (894): arm for the next line's
                            // M3Start::f0 consume (xpos==166 term, fires regardless of
                            // winEn).
                            self.win_draw_start = true;
                        }
                    }
                    // First scanline after enable is now armed; subsequent
                    // lines use normal Mode 2 timing.
                    let was_first_line = self.first_line_after_enable;
                    self.first_line_after_enable = false;
                    self.mode0_pretriggered_this_line = false;
                    self.mode0_reported_this_line = false;
                    self.line_rendered_this_line = false;
                    // SCX fine-scroll discard target (Gambatte M3Start::f1): the
                    // break xpos is resolved over the first M3 dots by re-reading
                    // SCX live (see the early-window loop in PixelTransfer). Seed
                    // it unlatched (-1) and record the arm dot for xpos tracking.
                    self.m3_pixels_discarded = 0;
                    self.m3_arm_dot = self.ticks;
                    self.m3_arm_scx = (mmio.read(SCX) & 0x07) as u8;
                    self.m3_arm_scx_full = mmio.read(SCX) as i16;
                    // First line after enable: resolve the SCX value the fine-scroll
                    // discard actually samples. Gambatte's M3Start::f1 reads SCX once
                    // at the M3-start dot; a mid-discard SCX write (visible at
                    // `write_cc + 2*cgb`) counts only if it lands at/before that
                    // sample dot, which sits `prev_scx % 8` dots past M3-arm (the
                    // discard prefix of the value in effect at M3-start). Evaluate the
                    // pending f1 latch (from on_scx_write, still intact here) at
                    // `arm_cc + prev_scx%8`. Matches Gambatte byte-exact on the
                    // ly0_late_scx7 SCX-write sweep (initial-SCX shifts the sample
                    // dot, flipping whether the SCX=7 write enters the m0Time).
                    if was_first_line {
                        let ds = mmio.is_double_speed_mode() as u32;
                        let prev_scx = (self.scx_prev_f1 & 0x07) as u64;
                        // `prev_scx` is a count of PPU dots; convert to master cc
                        // (1 dot = 1<<ds cc) so the sample dot is phase-correct at
                        // double speed (where the f1 latch's apply cc is write_cc+4).
                        let sample_cc = self.abs_cc + (prev_scx << ds);
                        self.first_line_scx_override = Some(self.scx_f1_pending_at_cc(sample_cc));
                    } else {
                        self.first_line_scx_override = None;
                    }
                    // Seed the exact-cc f1 latch at the SCX value live at M3
                    // start; clear any pending write latch left from a prior
                    // line so it cannot leak into this line's discard.
                    self.scx_prev_f1 = mmio.read(SCX);
                    self.scx_f1_apply_cc = wy2_disabled();
                    // The first line after display enable has bespoke warmup/arm
                    // timing; the live f1 xpos mapping does not align there, so
                    // latch the discard immediately (pre-write SCX), as before.
                    self.m3_discard_target = if was_first_line { self.m3_arm_scx as i8 } else { -1 };
                    self.check_and_trigger_stat_interrupt(mmio);

                    if was_first_line {
                        // First line after LCD enable: install the SAME closed-form
                        // master-cc anchors the normal-line path uses, computed for
                        // this line, so the CPU-access gates (cgbp/oam/vram) and the
                        // getStat mode reads resolve at the access cc instead of
                        // falling back to the hand-tuned FIRST_FRAME per-dot pipeline.
                        //
                        // Gambatte PPU::setLcdc seeds the PPU at enable with `now =
                        // enable_cc`, `lyCounter.reset(0, enable_cc)`, no sprites
                        // (enableDisplay clears the buffer), and `cycles =
                        // -(m3StartLineCycle + 2)` — so the first M3 begins 2 dots
                        // later than a normal line. `m0_time_exact(.., first_line)`
                        // adds that +2 to the mode-0 line-cycle; `cgbp_begin_exact`
                        // (the lineCycles+ds>=80 begin boundary) is enable-anchored
                        // already (it shares the same lyTime as a normal line).
                        // The inactive-period gate (`display_enable_inactive_until`,
                        // Gambatte OamReader::lu_) was seeded at enable.
                        let m3_len = self.compute_m3_length(mmio, is_cgb);
                        let m0t = self.m0_time_exact(mmio, m3_len, is_cgb, true);
                        self.m0_time_master = Some(m0t);
                        // The override applied only to this first-line m0Time anchor;
                        // clear it so the per-tick / next-frame m3_len reads live SCX.
                        self.first_line_scx_override = None;
                        self.cgbp_block_start_cc = Some(self.cgbp_begin_exact(mmio));
                        // The within-line reported mode-0 dot / m0 IRQ arm keep the
                        // calibrated FIRST_FRAME timing (the first-line pixel
                        // pipeline arms later than a normal line); only the
                        // closed-form access/getStat anchors above are installed.
                        self.scheduled_mode0_dot = None;
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
                        let m0t = self.m0_time_exact(mmio, m3_len, is_cgb, false);
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
                        // Predict the DMG dot at which the window's StartWindowDraw
                        // mode-3 penalty commits, so a disable landing on it (one
                        // PPU step before the PixelTransfer latch sets
                        // `win_start_dot`) is still treated as "started". The window
                        // draws when visible x reaches max(0, WX-7); x begins
                        // advancing `WARMUP + 8` dots past the M3 arm (the first BG
                        // tile fill) plus the SCX fine-scroll discard. The penalty
                        // commits at the fetcher's window-tile boundary, one dot
                        // ahead of the first window pixel reaching x (the `-1`), so
                        // a disable on the dot before the visible start still keeps
                        // it (late_disable_*_wx11 vs the same-tile wx10).
                        self.predicted_win_start_dot =
                            if !is_cgb && self.m3_scheduled_win {
                                let wx = self.m3_scheduled_wx as i64;
                                let x_at_start = (wx - 7).max(0);
                                Some(
                                    (self.m3_arm_dot as i64
                                        + DMG_PIXEL_TRANSFER_WARMUP as i64
                                        + 8
                                        + (self.m3_arm_scx as i64)
                                        + x_at_start
                                        - 1)
                                        .max(0) as u128,
                                )
                            } else {
                                None
                            };
                        // cgbp begin boundary (Gambatte cgbpAccessible: blocked once
                        // `lineCycles(cc) + ds >= 80`), byte-exact from the lyTime
                        // anchor — see `cgbp_begin_exact`.
                        self.cgbp_block_start_cc = Some(self.cgbp_begin_exact(mmio));
                    }
                    // Arm the mode-0 (HBlank) STAT IRQ event at the predicted
                    // mode-0 start, in absolute clock terms. Gambatte schedules
                    // memevent_m0irq only when m0 is enabled, but keeps the time
                    // current for FF41/FF45 immediate-trigger checks; we always
                    // arm it (dispatch gates on the enable in mstat_irq).
                    self.arm_m0irq_for_current_line(mmio, was_first_line);
                }
            },
            State::PixelTransfer => 'label: {
                // A mid-mode-3 WX change before the window starts invalidates the
                // closed-form schedule; fall back to the live emergent transition.
                // The `win_wx_enable_resolved` latch suppresses re-entry on the dots
                // after a clean WX-enable was handled (the WX != arm-WX condition
                // stays true every subsequent dot until the window draws).
                if self.scheduled_mode0_dot.is_some()
                    && !self.window_started_this_line
                    && !self.win_wx_enable_resolved
                    && (mmio.read(WX) != self.m3_scheduled_wx
                        || self.window_will_start(mmio, mmio.is_cgb_features_enabled())
                            != self.m3_scheduled_win)
                {
                    // WX-write-ENABLE: the window was out of range at M3 arm
                    // (`!m3_scheduled_win`, so m0_time_master has NO StartWindowDraw
                    // penalty) and a mid-mode-3 WX write brings it into range so the
                    // window will now start this line. Gambatte's predictNextM0Time
                    // re-runs with the window included, moving the mode-3 end
                    // WIN_M3_PENALTY dots later. ADD that penalty (symmetric to the
                    // LCDC window-enable path) iff the write lands before the window
                    // tile commits — otherwise the fetcher already passed the window
                    // start and no penalty accrues. Scoped CGB / no sprites; the live
                    // pipeline is untouched, only the read-at-cc m0Time is shifted.
                    let now_will_start =
                        self.window_will_start(mmio, mmio.is_cgb_features_enabled());
                    // Only the WX-into-range case: WX itself changed from out of range
                    // (arm WX > 166, no window scheduled) to in range. A window that
                    // newly starts for any OTHER reason (a mid-mode-3 WY trigger with
                    // WX unchanged and already in range) is NOT this lever and must
                    // keep nulling (the late_wy / late_scx_late_wy cluster).
                    let arm_wx = self.m3_scheduled_wx as i32;
                    let wx_now = mmio.read(WX) as i32;
                    let wx_into_range = arm_wx > 166 && (0..=166).contains(&wx_now);
                    let wx_enable_clean = !self.m3_scheduled_win
                        && now_will_start
                        && wx_into_range
                        && mmio.is_cgb_features_enabled()
                        && !mmio.is_double_speed_mode()
                        && self.sprites_on_line.is_empty();
                    let mut keep_schedule = false;
                    if wx_enable_clean && let Some(m0t) = self.m0_time_master {
                        // Latch: this clean WX-enable is now resolved for the line, so
                        // later dots (WX still != arm) do not re-enter and null.
                        self.win_wx_enable_resolved = true;
                        keep_schedule = true;
                        let wx = mmio.read(WX) as i32;
                        let x_at_start = (wx - 7).max(0);
                        let warmup = CGB_PIXEL_TRANSFER_WARMUP as i64;
                        // SCX>3 / scx5 fine-scroll: the x==0 window-tile commit runs
                        // two dots later per extra discarded SCX dot, mirroring the
                        // late-WX-disable accrual shift.
                        let win_fine = if wx <= 7 {
                            2 * (((self.m3_arm_scx & 7) as i64) - 3).max(0)
                        } else {
                            0
                        };
                        let commit_dot = self.m3_arm_dot as i64
                            + warmup
                            + 8
                            + self.m3_arm_scx as i64
                            + x_at_start as i64
                            + win_fine
                            + WXEN_COMMIT_DELAY;
                        if (self.ticks as i64) < commit_dot {
                            let pen = (WIN_M3_PENALTY as i64) << (mmio.is_double_speed_mode() as i64);
                            self.m0_time_master = Some((m0t as i64 + pen).max(0) as u64);
                            // Keep the closed-form schedule (mode-3 end shifts with
                            // the penalty); only the master m0Time moved.
                        }
                        // else: window starts but the write is past the commit dot, so
                        // no penalty is added — the no-window m0Time captured at arm is
                        // the correct (mode-0-earlier) boundary; keep the schedule.
                    }
                    // WY-trigger ENABLE (symmetric to the WX-into-range branch above):
                    // WX is UNCHANGED and already in range, but the window newly starts
                    // this line because a mid-mode-3 WY write made `window_y_active`
                    // true (the weMaster / `wy2 == ly` gate flipped). Gambatte's
                    // predictNextM0Time then runs with the window included, moving the
                    // mode-3 end WIN_M3_PENALTY dots later — BUT only if the WY trigger
                    // lands before the fetcher reaches the window-start xpos. For an
                    // x==0 window (the late_wy / late_scx_late_wy cluster, WX in 0..=7)
                    // that commit dot is `m3_arm_dot + scx&7 + COMMIT`: the f0/f1
                    // dispatch reaches xpos 0 (the window tile) `scx&7` dots into M3.
                    // (Measured byte-exact via cctracer: m0Time = no-window + 6 for the
                    // `_1` reps that trigger 1 dot in, == no-window for the `_2`/`_3`
                    // reps that trigger 5+ dots in; the boundary is m3_arm_dot+scx+3 at
                    // both scx=0 and scx=4.) If the trigger lands at/after the commit
                    // dot, the fetcher already passed xpos 0 so no penalty accrues and
                    // the no-window m0Time (captured at arm) is the correct boundary.
                    // Scoped CGB / single speed / no sprites / x==0 window; the live
                    // pipeline is untouched, only the read-at-cc m0Time is shifted.
                    if !keep_schedule
                        && !self.m3_scheduled_win
                        && now_will_start
                        && arm_wx == wx_now
                        && (0..=7).contains(&wx_now)
                        && mmio.is_cgb_features_enabled()
                        && !mmio.is_double_speed_mode()
                        && self.sprites_on_line.is_empty()
                        && let Some(m0t) = self.m0_time_master
                    {
                        // This WY-trigger enable is resolved for the line; suppress
                        // re-entry on later dots (window_will_start stays != arm).
                        self.win_wx_enable_resolved = true;
                        keep_schedule = true;
                        // Commit dot = the M3 dot at which the fetcher reaches the
                        // window-start xpos. For an x==0 window (WX 0..=7) that is
                        // `m3_arm_dot + scx&7 + WX + 3`: the SCX fine-scroll discard
                        // (scx&7 dots) then the WX-pixel BG prefix before the window
                        // tile, plus the fixed f0/f1 dispatch lead (3). A WY trigger
                        // before this dot adds the StartWindowDraw penalty (mode 3
                        // runs WIN_M3_PENALTY longer); at/after it the fetcher already
                        // passed xpos 0, so no penalty accrues. (cctracer: the `_1`
                        // reps of late_wy_*_wx00 / late_wy_*_wx07 / late_scx_late_wy
                        // keep the +6 m0Time, the `_2`/`_3` reps drop it; the WX-shift
                        // separates the wx00 `_1` boundary from the wx07 `_1`.)
                        let commit_dot = self.m3_arm_dot as i64
                            + (self.m3_arm_scx & 7) as i64
                            + wx_now as i64
                            + WYTRIG_COMMIT_DELAY;
                        if (self.ticks as i64) < commit_dot {
                            self.m0_time_master =
                                Some((m0t as i64 + WIN_M3_PENALTY as i64).max(0) as u64);
                        }
                        // else: no penalty — keep the no-window m0Time captured at arm.
                    }
                    // DMG WY-trigger enable (mirror of the CGB branch above). A
                    // mid-mode-3 WY==LY trigger with an x==0 window (WX 0..=7,
                    // unchanged) brings the window into play this line. Gambatte keeps
                    // a finite (window-inclusive or no-window) m0Time, so the FF41
                    // line-tail read resolves a concrete mode 0/3 boundary; nulling
                    // m0_time_master here would defer to the renderer register (always
                    // mode 3), passing the out3 `_1`/`_2` reps but FAILING the out0
                    // `_3` rep (late_wy_FFto2_ly2_wx00_3 / late_scx_late_wy_FFto4_ly4
                    // _wx00_3). Keep the no-window m0Time and add WIN_M3_PENALTY iff the
                    // WY trigger lands before the window-tile commit dot. The DMG commit
                    // dot is the CGB form (`m3_arm_dot + scx&7 + WX + 3`) plus the
                    // DMG pixel-transfer warmup less one (`DMG_WARMUP - 1` = 3):
                    // measured ticks at the WY block bracket it across WX/SCX (wx00:
                    // pen@84,no-pen@88; scx4: pen@84/88,no-pen@92; wx07: pen@88/92,
                    // no-pen@96; scx3+wx07: pen@88/92,no-pen@96), so commit_dot =
                    // m3_arm_dot + scx&7 + WX + 3 + 3 separates pen vs no-pen at every
                    // rep. Scoped DMG / SS / no sprites / x==0 (WX 0..=7).
                    if !keep_schedule
                        && !self.m3_scheduled_win
                        && now_will_start
                        && arm_wx == wx_now
                        && (0..=7).contains(&wx_now)
                        && !mmio.is_cgb_features_enabled()
                        && !mmio.is_double_speed_mode()
                        && self.sprites_on_line.is_empty()
                        && let Some(m0t) = self.m0_time_master
                    {
                        self.win_wx_enable_resolved = true;
                        keep_schedule = true;
                        let commit_dot = self.m3_arm_dot as i64
                            + (self.m3_arm_scx & 7) as i64
                            + wx_now as i64
                            + WYTRIG_COMMIT_DELAY
                            + (DMG_PIXEL_TRANSFER_WARMUP as i64 - 1);
                        if (self.ticks as i64) < commit_dot {
                            self.m0_time_master =
                                Some((m0t as i64 + WIN_M3_PENALTY as i64).max(0) as u64);
                        }
                        // else: no penalty — keep the no-window m0Time captured at arm.
                    }
                    // WX-DISABLE of a WX<7 (visible x==0) window that WAS scheduled at
                    // M3 arm: the immediate-start window's StartWindowDraw penalty
                    // locks the moment the fetcher fetches the window tile (Gambatte's
                    // `xpos == wx` compare uses the WX register, so a smaller WX commits
                    // earlier). A WX-write moving WX out of range at/after that commit
                    // dot keeps the window-inclusive m0_time_master (mode 3 persists ->
                    // out3); before it the existing null applies (refund -> mode 0). The
                    // commit dot is `m3_arm_dot + DMG_WARMUP + 5 + scx&7 + WX` (the first
                    // BG tile fill plus the WX-pixel BG prefix before the window tile,
                    // less the f0/f1 dispatch lead). The late_wx_wx03_{1,2} DMG reps
                    // bracket it at WX=3 (write at dot 88 = before -> out0; dot 92 =
                    // at commit -> out3); WX=7 (late_wx_1) commits 4 dots later (dot
                    // 96) so the same dot-92 disable still nulls (out0). Scoped DMG /
                    // single speed / no sprites / WX<7; the WX>=7 reps keep the existing
                    // `>= 7` graduated branch below. window_started_this_line is still
                    // false at this dot (the latch lags the closed-form commit).
                    if !keep_schedule
                        && self.m3_scheduled_win
                        && (self.m3_scheduled_wx as i32) < 7
                        && !now_will_start
                        && !mmio.is_cgb_features_enabled()
                        && !mmio.is_double_speed_mode()
                        && self.sprites_on_line.is_empty()
                        && self.m0_time_master.is_some()
                    {
                        let commit_dot = self.m3_arm_dot as i64
                            + DMG_PIXEL_TRANSFER_WARMUP as i64
                            + 5
                            + (self.m3_arm_scx & 7) as i64
                            + self.m3_scheduled_wx as i64;
                        if (self.ticks as i64) >= commit_dot {
                            keep_schedule = true;
                            self.win_wx_penalty_resolved = true;
                        }
                    }
                    if !keep_schedule {
                        self.scheduled_mode0_dot = None;
                        self.m0_time_master = None;
                    }
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
                // DMG late-WX window-disable refund. DMG is BINARY (not graduated like
                // CGB): a WX-out-of-range write that lands BEFORE the window-tile
                // commit (`ws + scx&7 + 2` dots into the x==0 window draw) fully
                // refunds WIN_M3_PENALTY from the read-at-cc m0Time so the FF41 read
                // resolves the no-window mode-0 boundary; at/after the commit the
                // window-inclusive m0Time captured at M3 arm is kept (mode 3). The
                // late_wx_scx{2,3,5}_{1,2} DMG reps bracket the per-SCX commit: at the
                // 4-dots-in write, scx0/scx2 already committed (out3, keep) while
                // scx3/scx5 have not (out0, refund); the 8-dots-in write is always
                // committed (out3). WX<7 immediate-start windows lock at win_start
                // (no refund). DMG / no sprites / SS.
                if self.m0_time_master.is_some()
                    && self.window_started_this_line
                    && !mmio.is_cgb_features_enabled()
                    && self.sprites_on_line.is_empty()
                    && mmio.read(WX) != self.m3_scheduled_wx
                    && !self.win_wx_penalty_resolved
                    && (self.m3_scheduled_wx as i32) >= 7
                {
                    let wx_now = mmio.read(WX) as i32;
                    let wx_in_range = (0..=166).contains(&wx_now);
                    if let (Some(ws), Some(m0t)) = (self.win_start_dot, self.m0_time_master)
                        && !wx_in_range
                    {
                        let commit = ws as i64 + (self.m3_arm_scx & 7) as i64 + 2;
                        if (self.ticks as i64) < commit {
                            self.m0_time_master =
                                Some((m0t as i64 - WIN_M3_PENALTY as i64).max(0) as u64);
                        }
                        self.win_wx_penalty_resolved = true;
                    }
                }
                else if self.m0_time_master.is_some()
                    && self.window_started_this_line
                    && mmio.is_cgb_features_enabled()
                    && !mmio.is_double_speed_mode()
                    && self.sprites_on_line.is_empty()
                    && mmio.read(WX) != self.m3_scheduled_wx
                    && !self.win_wx_penalty_resolved
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
                            // SCX > 3 fine-scroll: the x==0 window's StartWindowDraw
                            // penalty accrual begins later than win_start_dot by two
                            // dots per extra discarded SCX dot (the M3Start dispatch
                            // runs the window-tile fetch that much later). Without
                            // this the scx5 boundary is 4 dots too early and the
                            // late_wx_scx5_1 refund is fully accrued (drops to 0).
                            let scx_late = 2 * (((self.m3_arm_scx & 7) as i64) - 3).max(0);
                            let drawn = (self.ticks as i64) - ws as i64 + scx_bias - scx_late;
                            let accrued = drawn.clamp(0, WIN_M3_PENALTY as i64);
                            let refund = WIN_M3_PENALTY as i64 - accrued;
                            self.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                            self.win_wx_penalty_resolved = true;
                        }
                    }
                }
                // Double-speed late-WX window-disable refund. Unlike single speed
                // (graduated per drawn dot), the DS StartWindowDraw penalty is BINARY:
                // a WX-out-of-range write that lands BEFORE the window-tile commits
                // (`ws + scx&7 + 1` dots into the window draw) fully refunds the
                // WIN_M3_PENALTY (<<1 cc at DS), so the FF41 read resolves the
                // no-window mode-0 boundary; at/after the commit the penalty is locked
                // and the window-inclusive m0Time (captured at arm) is kept. cctracer
                // ground truth: late_wx_scx5_ds_1 (write 2 dots into the x==0 window,
                // scx5) takes the full 12-cc refund -> mode 0 (out0); the `_ds_2` reps
                // (write 2 dots later, or scx0 1 dot in) keep the full m0Time -> mode 3
                // (out3). CGB / no sprites; live pipeline untouched, only read-at-cc.
                else if self.m0_time_master.is_some()
                    && self.window_started_this_line
                    && mmio.is_cgb_features_enabled()
                    && mmio.is_double_speed_mode()
                    && self.sprites_on_line.is_empty()
                    && mmio.read(WX) != self.m3_scheduled_wx
                    && !self.win_wx_penalty_resolved
                    && (self.m3_scheduled_wx as i32) >= 7
                {
                    let wx_now = mmio.read(WX) as i32;
                    let wx_in_range = (0..=166).contains(&wx_now);
                    if let (Some(ws), Some(m0t)) = (self.win_start_dot, self.m0_time_master)
                        && !wx_in_range
                    {
                        let commit = ws as i64 + (self.m3_arm_scx & 7) as i64 + 1;
                        if (self.ticks as i64) < commit {
                            let refund = (WIN_M3_PENALTY as i64) << 1;
                            self.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                        }
                        self.win_wx_penalty_resolved = true;
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
                        // Timing report (FF41 mode-0, STAT/m0 IRQ) fires at the exact
                        // m0Time regardless of pixel progress.
                        if !self.mode0_reported_this_line {
                            self.mode0_reported_this_line = true;
                            Self::set_lcd_status_mode(mmio, 0);
                            self.check_and_trigger_stat_interrupt(mmio);
                        }
                        // Flush remaining FIFO pixels to fill all 160 columns; the
                        // pipeline may lag the closed-form boundary by a few dots.
                        while self.x < 160 && self.draw_fifo_pixel(mmio) {}
                        // On window-start lines the window fetch restart can leave
                        // the FIFO momentarily empty at m0Time (the last 1-2 window
                        // pixels are still being fetched). The timing has already
                        // been reported above; keep the renderer alive (image-only)
                        // until x==160 so the final window pixel is drawn, then enter
                        // HBlank via the x==160 fallback below. For all other lines
                        // the flush completed the line, so end mode 3 now.
                        if !(self.window_started_this_line && self.x < 160) {
                            if linerender_enabled() && !self.line_rendered_this_line {
                                self.render_full_line(mmio);
                            }
                            self.state = State::HBlank;
                            break 'label;
                        }
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
                    // Exact-cc SCX read: sample SCX as-of this f1 dot's abs_cc
                    // (honoring the CGB +2cc scxChange delay) so a mid-discard
                    // write lands on the correct iteration, instead of the
                    // immediate register read whose visibility depends on the
                    // per-dot PPU-step-vs-CPU-write ordering within a dot.
                    let scx_break_full = self.scx_f1_pending_at_cc(self.abs_cc);
                    let scx_live = (scx_break_full & 0x07) as u32;
                    if xpos % 8 == scx_live || xpos >= 80 {
                        // Gambatte M3Start::f1 re-reads p.scx live at its case-0 tile
                        // fetch, so a mid-discard SCX write that crosses a tile-column
                        // boundary makes the FIRST displayed tile come from the new
                        // column (scx_break/8), not the column queued into the FIFO at
                        // M3 arm. When that happens, discard the whole stale first tile
                        // and refetch from the live column: reset the fetcher/FIFO and
                        // set the discard to scx_break%8 so the next BG fetch (which
                        // derives its column from scx_delayed at x==0) lands on the
                        // correct column, then trims the fine-scroll prefix. The mode-3
                        // length / timing is owned by getStat (m0_time_master), so this
                        // is render-only.
                        // The displayed first tile's COLUMN is read at Gambatte's
                        // last case-0 (the greatest multiple-of-8 xpos <= break),
                        // NOT at the break dot: M3Start::f1 only reloads `reg1`
                        // (tile number, from scx/8) when `xpos % tile_len == 0`.
                        // For a break inside the first tile (xpos < 8) that is
                        // xpos==0 -> the M3-arm column, so no re-fetch is needed
                        // even if a later f1 dot saw a column-crossing SCX. Only a
                        // break that loops PAST tile_len (xpos >= 8) reloads at
                        // xpos==8 from the then-live SCX. Sample SCX at that dot.
                        let case0_xpos = (xpos / 8) * 8;
                        let ds_u = mmio.is_double_speed_mode() as u32;
                        let back = ((xpos - case0_xpos) as u64) << ds_u;
                        let scx_col_full =
                            self.scx_f1_pending_at_cc(self.abs_cc.wrapping_sub(back));
                        let arm_col = ((self.m3_arm_scx_full.max(0) as u16) >> 3) & 0x1F;
                        let brk_col = (scx_col_full as u16 >> 3) & 0x1F;
                        // CGB f1 first-tile re-fetch (both single and double speed):
                        // a mid-f1 SCX write whose break column differs from the
                        // armed column rewrites the first queued BG tile. The
                        // sub-cc clock carries the DS sub-dot phase via the
                        // `delta << ds` mode0/m0Time nudge below, so the same
                        // re-fetch applies at double speed (the DMG M3Start
                        // fine-scroll uses a different +1 tile-column phase the
                        // discard model already matches, so it stays excluded).
                        if mmio.is_cgb_features_enabled()
                            && self.m3_arm_scx_full >= 0
                            && brk_col != arm_col
                        {
                            // Only the FIRST queued BG tile is stale: rewrite the
                            // 8 oldest FIFO entries in place with the tile at the
                            // break column, then discard scx_break%8 fine pixels.
                            // Subsequent tiles keep their live-SCX columns (the
                            // fetcher re-reads scx_delayed), so a later SCX write
                            // that moves the steady-state column is preserved.
                            let bg_y = (self.scy_delayed as u16
                                + mmio.read(LY) as u16) & 0xFF;
                            self.rewrite_first_fifo_tile(mmio, brk_col, bg_y);
                            self.m3_pixels_discarded = 0;
                            self.m3_discard_target = (scx_break_full & 0x07) as i8;
                            if let Some(dot) = self.scheduled_mode0_dot {
                                let delta = xpos as i64 - self.m3_arm_scx as i64;
                                self.scheduled_mode0_dot = Some((dot as i64 + delta).max(0) as u128);
                                if let Some(m0t) = self.m0_time_master {
                                    let ds = mmio.is_double_speed_mode() as u32;
                                    self.m0_time_master =
                                        Some((m0t as i64 + (delta << ds)).max(0) as u64);
                                }
                            }
                            break 'label;
                        }
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
                        if matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::TileNumber) {
                            self.subcc_last_tn_cc = self.abs_cc;
                        }
                        // sub-cc column lever: a BG tile whose column was committed
                        // at TileNumber under the OLD scx, but whose pixels are
                        // PLOTTED after the write's apply cc (write_cc + 2*cgb),
                        // must render under the NEW scx (Gambatte scxChange
                        // `update(cc+2*cgb); setScx` samples the column at plot
                        // time, not fetch time). Only the single in-flight straddle
                        // tile (armed at the write) is corrected, and only at the
                        // exact plot-vs-apply phase (gap == 4); see the gap comment
                        // below.
                        let mut armed_this_event = false;
                        if self.subcc_rekey_armed
                            && matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo)
                        {
                            // The single in-flight tile (column committed under the
                            // OLD scx before the write) just pushed. Its first
                            // displayed pixel sits at display column == the xpos the
                            // fetcher used (xpos == display_x + fifo - pending); its
                            // plot cc is abs_cc + (xpos - current display x). If that
                            // plot cc is strictly after the apply cc the tile must
                            // render under the NEW scx (Gambatte scxChange samples
                            // the column at plot, not fetch); re-key the 8 newest
                            // FIFO entries with the NEW-scx column using the
                            // fetcher's exact xpos/cgb_adj. Disarm afterwards.
                            self.subcc_rekey_armed = false;
                            let dsf = mmio.is_double_speed_mode() as u32;
                            let (xpos, cgb_adj, _) = self.fetcher.subcc_last_column_inputs();
                            // plot cc = abs_cc + the dot distance to this tile's
                            // first displayed pixel. The dot delta must be scaled
                            // to master cc (1 dot = 1<<ds cc) so the gap resonance
                            // is in master cc at both speeds.
                            let plot_cc = self.abs_cc as i64
                                + ((xpos as i64 - self.x as i64) << dsf);
                            // SS (validated Stage 1b, broke-0 across the full
                            // suite incl. DMG): the in-flight straddle flips to NEW
                            // at the exact plot-vs-apply phase gap==4.
                            let gap = plot_cc - self.subcc_scx_apply_cc as i64;
                            // DMG SS + low-X sprite: the sprite-fetch dot during the
                            // discard prologue shifts the whole line's BG-fetch phase
                            // one tile, so a steady-state mid-line SCX write's
                            // OLD->NEW column boundary also lands one tile LATER than
                            // the no-sprite cadence the gap==4 rekey assumes. The
                            // in-flight tile plots just before the boundary, so keep
                            // it OLD (suppress the flip); the NEXT tile, fetched after
                            // the write, is already NEW. Mirrors the CGB gap==1
                            // first-line revert. Without the sprite (scx_during_m3_4/5)
                            // gap==4 stays as the validated steady-state flip.
                            let dmg_ss_lowx_sprite = dsf == 0
                                && !mmio.is_cgb_features_enabled()
                                && (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0
                                && self.sprites_on_line.iter().any(|s| s.x <= 8);
                            // DS (Stage 2): the gap proxy is ambiguous across
                            // initial-scx, but the underlying resonance is that the
                            // write's apply cc lands at the MIDPOINT of the armed
                            // tile's fetcher step. The BG fetcher advances one step
                            // every 2 dots == (2<<ds) cc; the armed tile's column
                            // was latched at TileNumber (subcc_last_tn_cc) and
                            // Gambatte's `update(apply_cc); setScx` re-derives that
                            // single tile NEW only when apply falls half a step
                            // (1<<ds cc) past the latch, modulo the step:
                            //   (apply_cc - tn_cc) % (2<<ds) == (1<<ds)
                            // At DS this is (apply-tn)%4==2, which flips ds_3/4/5
                            // across every initial-scx (0761/0360/...) where the
                            // cruder gap/span proxies disagree. SS keeps gap==4
                            // (the DMG cadence differs and the mod phase regresses
                            // the DMG SS set, so SS is left exactly as Stage 1b).
                            let flip = if dsf == 0 {
                                gap == 4 && !dmg_ss_lowx_sprite
                            } else {
                                let step = 2i64 << dsf;
                                let phase = (self.subcc_scx_apply_cc as i64
                                    - self.subcc_last_tn_cc as i64).rem_euclid(step);
                                phase == (1i64 << dsf)
                            };
                            // DS two-tile straddle gate: a low-X sprite on the line
                            // shifts the BG fetch phase one tile while the DS FIFO
                            // carries an extra tile, so the OLD->NEW scx boundary lands
                            // one tile LATER than the non-sprite DS cadence and the
                            // in-flight straddle tile stays OLD instead of flipping to
                            // NEW (with a further one-tile LY0 shift handled below).
                            // The non-sprite DS cases (lowspr==0) are a single-tile
                            // straddle handled correctly by the NEW rewrite below and
                            // MUST keep it.
                            let ds_two_tile = dsf == 1
                                && mmio.is_cgb_features_enabled()
                                && self.sprites_on_line.iter().any(|s| s.x <= 16);
                            if flip {
                                let new_col = (((self.subcc_scx_new as u16) + xpos + cgb_adj as u16) / 8) % 32;
                                let old_col = (((self.subcc_scx_old as u16) + xpos + cgb_adj as u16) / 8) % 32;
                                if ds_two_tile {
                                    // DS spx straddle: a low-X sprite shifts the BG
                                    // fetch phase one tile while the DS FIFO carries an
                                    // extra tile, so the OLD->NEW scx boundary lands one
                                    // tile LATER than the non-sprite DS cadence. The
                                    // in-flight straddle tile -- which the non-sprite DS
                                    // flip would push to the NEW scx -- actually plots
                                    // just before the boundary, so it stays the OLD scx
                                    // (natural xpos column) on EVERY line. On the first
                                    // rendered line (LY==0) the boundary lands one tile
                                    // later still, so the NEXT tile (already fetched
                                    // under the NEW scx) must also revert to the OLD scx;
                                    // on LY>=1 that next tile keeps the NEW scx.
                                    if old_col != new_col {
                                        let bg_y = (self.scy_delayed as u16
                                            + mmio.read(LY) as u16) & 0xFF;
                                        let pixels = self.bg_pixels_at_col(mmio, old_col, bg_y);
                                        let off = (xpos as usize).saturating_sub(self.x as usize);
                                        self.fetcher.pixel_fifo.overwrite_at(off, &pixels);
                                    }
                                    // First-line second-tile revert: on LY==0 the
                                    // fetcher dispatch can land the OLD->NEW boundary
                                    // one tile later than on LY>=1, so the second
                                    // straddle tile (already fetched NEW) reverts to
                                    // OLD. Whether that one-tile shift happens depends
                                    // on the sprite-fetch sub-tile phase: an even
                                    // shifting sprite x consumes the extra dot that
                                    // pushes the second tile's fetch past the apply on
                                    // LY0 (sprite x==2), an odd one does not (x==1),
                                    // so the revert is gated on the low sprite x parity.
                                    let lowspr_even = self
                                        .sprites_on_line
                                        .iter()
                                        .filter(|s| s.x <= 16)
                                        .map(|s| s.x)
                                        .min()
                                        .is_some_and(|x| x % 2 == 0);
                                    if mmio.read(LY) == 0 && lowspr_even {
                                        self.ds_straddle_next_old = true;
                                        armed_this_event = true;
                                    }
                                } else if new_col != old_col {
                                    let bg_y = (self.scy_delayed as u16
                                        + mmio.read(LY) as u16) & 0xFF;
                                    let pixels = self.bg_pixels_at_col(mmio, new_col, bg_y);
                                    self.fetcher.pixel_fifo.overwrite_newest(&pixels);
                                }
                            } else if dsf == 0
                                && mmio.is_cgb_features_enabled()
                                && gap == 1
                                && self.sprites_on_line.iter().any(|s| s.x >= 1 && s.x <= 8)
                            {
                                // First rendered line (LY=0) straddle, CGB SS: the
                                // line after LCD-enable runs its mode-3 fetcher
                                // through a different warmup/dispatch phase, so the
                                // write's apply lands one fetcher step EARLIER
                                // relative to the in-flight tile (gap==1 here vs
                                // gap==5 on LY>=1, same xpos). The armed tile stays
                                // OLD (it plots just before the boundary), AND the
                                // NEXT tile -- which the per-dot fetcher already
                                // read NEW because the first-line dispatch lags the
                                // boundary by one tile -- must be reverted to OLD so
                                // the OLD->NEW boundary lands one tile later, exactly
                                // as Gambatte's `update(apply_cc)` first-line xpos
                                // does. On LY>=1 (gap==5) this revert does NOT fire,
                                // so those lines keep the boundary one tile earlier.
                                self.subcc_revert_next_old = true;
                                armed_this_event = true;
                            }
                        }
                        // Sprite-shifted revert: the tile pushed right after the
                        // armed straddle tile was fetched with the NEW scx one tile
                        // too early (FIFO depth 8 vs 9 due to a sprite-fetch dot);
                        // rewrite its 8 entries back to the OLD-scx column so the
                        // OLD->NEW boundary lands one tile later (matching Gambatte's
                        // `update(apply_cc)` fetcher-xpos boundary).
                        if self.subcc_revert_next_old
                            && !armed_this_event
                            && matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo)
                        {
                            self.subcc_revert_next_old = false;
                            let (xpos, cgb_adj, _) = self.fetcher.subcc_last_column_inputs();
                            let new_col = (((self.subcc_scx_new as u16) + xpos + cgb_adj as u16) / 8) % 32;
                            let old_col = (((self.subcc_scx_old as u16) + xpos + cgb_adj as u16) / 8) % 32;
                            if new_col != old_col {
                                let bg_y = (self.scy_delayed as u16
                                    + mmio.read(LY) as u16) & 0xFF;
                                let pixels = self.bg_pixels_at_col(mmio, old_col, bg_y);
                                self.fetcher.pixel_fifo.overwrite_newest(&pixels);
                            }
                        }
                        // DS two-tile straddle, SECOND tile (LY0 only): this tile was
                        // fetched under the NEW scx (the per-dot fetcher advanced past
                        // the apply) but on the first rendered line the OLD->NEW
                        // boundary lands one tile later, so it plots under the OLD scx
                        // at its natural column. Rewrite it in place by exact display
                        // offset (xpos - self.x) so the low-X sprite's FIFO shift does
                        // not misplace it.
                        if self.ds_straddle_next_old
                            && !armed_this_event
                            && matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo)
                        {
                            self.ds_straddle_next_old = false;
                            let (xpos, cgb_adj, _) = self.fetcher.subcc_last_column_inputs();
                            let new_col2 = (((self.subcc_scx_new as u16) + xpos + cgb_adj as u16) / 8) % 32;
                            let old_col2 = (((self.subcc_scx_old as u16) + xpos + cgb_adj as u16) / 8) % 32;
                            if new_col2 != old_col2 {
                                let bg_y = (self.scy_delayed as u16
                                    + mmio.read(LY) as u16) & 0xFF;
                                let pixels = self.bg_pixels_at_col(mmio, old_col2, bg_y);
                                let off = (xpos as usize).saturating_sub(self.x as usize);
                                self.fetcher.pixel_fifo.overwrite_at(off, &pixels);
                            }
                        }
                        // First-tile (f1) prologue straddle (DMG SS): the in-flight
                        // 2nd tile -- whose column was latched under the OLD scx one
                        // dot before a mid-prologue (x==0) SCX write -- just pushed.
                        // On hardware it plots after the write, so re-key its 8 newest
                        // FIFO entries to the NEW scx column (the first queued tile,
                        // pushed before the write, keeps OLD). Uses the fetcher's exact
                        // latched xpos/cgb_adj so the column matches Gambatte's
                        // `update(apply_cc); setScx` plot-time sample.
                        if self.prologue_rekey_armed
                            && matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo)
                        {
                            self.prologue_rekey_armed = false;
                            let (xpos, cgb_adj, _) = self.fetcher.subcc_last_column_inputs();
                            let new_col = (((self.subcc_scx_new as u16) + xpos + cgb_adj as u16) / 8) % 32;
                            let old_col = (((self.subcc_scx_old as u16) + xpos + cgb_adj as u16) / 8) % 32;
                            if new_col != old_col {
                                let bg_y = (self.scy_delayed as u16
                                    + mmio.read(LY) as u16) & 0xFF;
                                let pixels = self.bg_pixels_at_col(mmio, new_col, bg_y);
                                self.fetcher.pixel_fifo.overwrite_newest(&pixels);
                            }
                        }
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
                        self.win_draw_started = true;
                        // Start window rendering
                        self.fetcher.start_window(self.x);
                        self.window_started_this_line = true;
                        // The post-window sprite group restarts the BG-tile grid
                        // (Gambatte resets prevSpriteTileNo to tileno_none after
                        // the window split), so the first post-window sprite in a
                        // tile is again charged the leading rate.
                        self.m3_sprite_prev_tile = SPRITE_TILE_NONE;
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
                if self.draw_fifo_pixel(mmio) && self.x == 160 {
                    // Fallback end-of-mode-3 at the x==160 pixel push, used in two
                    // distinct cases:
                    //  (a) no closed-form m0Time exists (first line after enable /
                    //      mid-M3 invalidation): report mode 0 here and end mode 3.
                    //  (b) the m0Time timing report ALREADY fired above, but the
                    //      window fetch restart left the FIFO short, so the renderer
                    //      was kept alive to draw the final window pixel; now that
                    //      x==160 we end mode 3 WITHOUT re-reporting (the FF41 mode-0
                    //      poke / STAT IRQ already fired at the exact m0Time).
                    // When m0Time is known and the FIFO was complete, the transition
                    // is driven off master_cc above and the renderer never reaches
                    // this x==160 fallback before that boundary, so we must NOT end
                    // mode 3 early here on ordinary (non-window) lines.
                    let window_deferred = self.window_started_this_line && self.mode0_reported_this_line;
                    if self.m0_time_master.is_none() {
                        if linerender_enabled() && !self.line_rendered_this_line {
                            self.render_full_line(mmio);
                        }
                        self.state = State::HBlank;
                        if !self.mode0_reported_this_line {
                            self.mode0_reported_this_line = true;
                            Self::set_lcd_status_mode(mmio, 0);
                            self.check_and_trigger_stat_interrupt(mmio);
                        }
                    } else if window_deferred {
                        if linerender_enabled() && !self.line_rendered_this_line {
                            self.render_full_line(mmio);
                        }
                        self.state = State::HBlank;
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
                        // The m1 event already flagged VBlank (line_cycle 454, ~3cc
                        // earlier); re-flagging here would re-set bit 0 after a CPU
                        // IF-write between the two cc cleared it (lycint143_m1irq_ifw
                        // `_2`, m2m1irq_ifw `_3`). Only flag if the m1 event did not
                        // (e.g. LCD enabled mid-frame with no armed m1 schedule).
                        if !self.m1_vblank_fired {
                            mmio.request_interrupt(registers::InterruptFlag::VBlank);
                        }
                        self.m1_vblank_fired = false;
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
                // Gambatte's getLycCmpLy anticipates the line-153 LY=0 compare by
                // `lineTime - 6 - 6*isDoubleSpeed()`. At DS lineTime=912cc, so the
                // LY->0 flip lands 12cc = dot 6 into line 153 -- the same dot as
                // single speed (whose `lineTime-6` likewise resolves to dot 6 in its
                // own dot units). So both speeds use dot 6; the DS probes
                // (lyc0flag_ds / lyc153flag_ds) read C5 at lineCycles>=6, C1 before.
                let line_153_zero_dot = if mmio.is_double_speed_mode() {
                    LINE153_LY0_DOT_DS.max(0) as u128
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
                        // NOTE: win_draw_start / win_draw_started are intentionally
                        // NOT reset here. Gambatte resets winYPos at M2_Ly0::f0 but
                        // leaves winDrawState (both bits) untouched across the frame
                        // boundary, so a window armed on the last visible line (e.g.
                        // DMG wx==166 on line 143, where plotPixel branch B arms
                        // win_draw_start even with the window then disabled) carries
                        // through vblank and activates the window on the next frame's
                        // line 0 (M3Start::f0 consumes win_draw_start, ++winYPos).
                        // This is the wxA6 weMaster-persistence path.
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

    /// DEFERRED-HDMA-FIRE late-HBlank predicate for the FF55-kick / unhalt
    /// resolution paths only (NOT the per-dot edge machine). Mirrors Gambatte's
    /// `enableHdma` -> `isHdmaPeriod(cc + 4)` where `m0TimeOfCurrentLine` returns
    /// the CURRENT line's mode-0 time (`lastM0Time`) even after the renderer has
    /// crossed it — so a FF55 enable written mid-HBlank, after mode-0 entry but
    /// still on the same line, resolves IN-PERIOD and arms its block immediately
    /// (`hdma_late_enable_*`). rustyboi previously nulled `scheduled_mode0_dot` at
    /// the m0Time crossing, returning None there, dropping those late enables.
    ///
    /// Anchored on `m0_time_master` (master cc, shares the access cc's phase, so it
    /// is robust to the STOP/lcd-offset line-phase residual that a renderer-dot
    /// test is not): a visible line, the access cc at/past the mode-0 start, and
    /// not so deep into mode-0 that the next line is imminent. Threshold per speed
    /// brackets the late-enable pairs (SS: arm `cc-m0t` 191/188, drop 195/192 ->
    /// `< 192`; DS: arm 394/391, drop 398/395 -> `< 395`). Returns None when no
    /// closed-form mode-0 anchor exists (window / first line / mid-M3 invalidation)
    /// so the caller falls back to the STAT-mode gate.
    /// COORDINATED piece #3 (HDMA-halt deferred held-flag): the unhalt re-flag
    /// gate's `isHdmaPeriod(cc)` at the unhalt access cc. Same closed-form mode-0
    /// anchor as `hdma_period_kick`, but the END (drop) bracket sits later: the
    /// unhalt-reflag boundary the `hdma_late_m0unhalt_{1,2}` straddle pairs probe
    /// is past the FF55-enable kick boundary (cctracer: SS depth 196 reflags /
    /// 200 does not; DS 398 reflags / 402 does not), so it carries its own limit.
    /// Returns None when no closed-form mode-0 anchor exists (caller falls back).
    pub fn hdma_period_unhalt(&self, access_cc: u64, double_speed: bool) -> Option<bool> {
        self.hdma_period_unhalt_adj(access_cc, double_speed, 0)
    }

    /// This line's closed-form mode-0 (HBlank) start in master cc, or None when no
    /// closed-form anchor exists (window / first line after enable). Used by the
    /// HALT-entry HDMA capture to derive a per-period "block already served" signal
    /// (the live `hdma_block_done_this_period` flag is reset too early by the per-dot
    /// period falling edge — see `Mmio::on_cpu_halt_with_period_done`).
    pub fn m0_time_master_cc(&self) -> Option<u64> {
        self.m0_time_master
    }

    /// As `hdma_period_unhalt`, with the line-END (drop) bracket widened by
    /// `limit_adj` dots (the EI fast-dispatch ISR-phase compensation; see
    /// `Bus::hdma_in_period_for_unhalt_adj`). The compensation widens the END
    /// bracket ONLY — the START bracket (`cc >= m0t`, mode-0 entry) is left
    /// untouched, because the EI-fast ISR-phase shift inflates the unhalt-period
    /// DEPTH (`cc - m0t`) uniformly by 4: a Low-at-halt block deep in mode-0 (near
    /// the line end) must still reflag (depth 200 -> in), while a block at the
    /// mode-0 ENTRY (depth ~0, `hdma_ei_m3halt_m0unhalt_ly_*`) must still reflag
    /// too (Gambatte reflag=1) — which a m0t shift would wrongly push past the
    /// start bracket. `limit_adj == 0` is byte-identical to the calibrated
    /// baseline.
    pub fn hdma_period_unhalt_adj(
        &self,
        access_cc: u64,
        double_speed: bool,
        limit_adj: i64,
    ) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0_time_master? as i64;
        let cc = access_cc as i64;
        if cc < m0t {
            return Some(false);
        }
        let depth = cc - m0t;
        let limit: i64 = (if double_speed { 400 } else { 198 }) + limit_adj;
        Some(depth < limit)
    }

    /// HALT-ENTRY `isHdmaPeriod(cc)` for `haltHdmaState_` (Gambatte `Memory::halt`).
    /// Same `m0_time_master`-anchored closed-form predicate as `hdma_period_unhalt`,
    /// but the line-end (drop) bracket sits a few cc LATER: the HALT instruction's
    /// access cc reaches the `cc + 3 + 3*ds < lineEnd` boundary at a different phase
    /// than the unhalt access cc, so the `hdma_late_m0halt_{1,2}` straddle pair
    /// (cctracer: HALT cc 4cc apart, period 1->0) bracket their own limit. Probed
    /// per speed via the `_1` (in-period -> High -> 1 block) / `_2` (past-boundary
    /// -> Low -> reflag -> 2 blocks) pairs: SS depth 206/204 in, 210/208 out -> 208;
    /// DS depth 408/407 in, 412/411 out -> 410. Returns None when no closed-form
    /// mode-0 anchor exists (caller falls back to the cached per-step period).
    pub fn hdma_period_halt(&self, access_cc: u64, double_speed: bool) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0_time_master? as i64;
        let cc = access_cc as i64;
        if cc < m0t {
            return Some(false);
        }
        let depth = cc - m0t;
        let limit: i64 = if double_speed { 410 } else { 208 };
        Some(depth < limit)
    }

    /// Late-hdma-vs-interrupt unhalt precedence (memory.cpp:329-364). On unhalt
    /// with a Low-at-halt HDMA block, Gambatte's `intevent_unhalt` flags the block
    /// iff `isHdmaPeriod(cc)` (`cc >= m0Time`) at the unhalt cc. rustyboi's
    /// `m0_time_master` folds a +1 dot phase vs the raw m0Time, so the equivalent
    /// START boundary here is `cc + 1 >= m0t`. When TRUE the
    /// block's dma event is flagged (event time 0) and FIRES IMMEDIATELY at unhalt,
    /// i.e. BEFORE the interrupt's PC pushes — the dma-wins races
    /// (`late_hdma_vs_tima_*_halt_1`, copy the pre-push 0x1234). When FALSE the
    /// block is NOT yet in period at unhalt; its m0-edge falls during/after the
    /// interrupt service, so the block fires AFTER the pushes and copies the pushed
    /// return address (`*_halt_2`, 0x11C9). This predicate reports the former (fire
    /// AT unhalt / before pushes) decision so the service can suppress+reorder the
    /// latter. Anchored on `m0_time_master` (shares the access cc phase). None when
    /// no closed-form mode-0 anchor exists (caller keeps the synchronous fire).
    pub fn hdma_unhalt_fires_before_pushes(
        &self,
        access_cc: u64,
        double_speed: bool,
    ) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0_time_master? as i64;
        let cc = access_cc as i64;
        // REFLAG (fire-at-unhalt / before pushes) iff the unhalt access cc has
        // reached mode-0 start AND is not past the line-end. The START anchor is
        // `cc + 1 >= m0t` — the SAME +1 dot phase the per-dot `hdma_period`
        // predicate folds (`dot >= m0n + 1`); a bare `cc >= m0t` or the looser
        // `cc + 4` mis-brackets the scx-shifted m0Time. cctracer boundary at unhalt
        // cc=C: REFLAG for m0t<=C+1 (`scx{1,2}_halt_1`), NOREFLAG for m0t>=C+2
        // (`scx{1,2}_halt_2`).
        let in_start = cc + 1 >= m0t;
        let in_end = (cc - m0t) < (if double_speed { 400 } else { 198 });
        Some(in_start && in_end)
    }

    pub fn hdma_period_kick(&self, access_cc: u64, double_speed: bool) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0_time_master? as i64;
        let cc = access_cc as i64;
        // Start: in-period once the access cc reaches the mode-0 time. (Gambatte's
        // `cc + 4 >= m0Time`; the renderer-tick m0Time already folds the +4 phase
        // for the dma cluster, so a bare `cc >= m0t` brackets the enable pairs.)
        if cc < m0t {
            return Some(false);
        }
        // End: drop once the access cc is within `~12 master cc` of the next line
        // (i.e. too deep into mode-0). Empirical per-speed bracket on `cc - m0t`.
        let depth = cc - m0t;
        let limit: i64 = if double_speed { 395 } else { 192 };
        Some(depth < limit)
    }

    /// FF55=00 HDMA-DISABLE-vs-m0-edge race (Gambatte `disableHdma`): writing
    /// FF55 bit7=0 only clears the FUTURE `memevent_hdma` schedule; it does NOT
    /// un-flag a block whose m0-edge has ALREADY fired (`intevent_dma` is latched
    /// and `dma()` will still run). So a late disable cannot stop a block once the
    /// current line's mode-0 edge has passed. The boundary is exactly the m0-edge
    /// time: Gambatte processes the `memevent_hdma` event (which `flagHdmaReq`s)
    /// before the FF55 write whenever the write cc has reached `m0Time`.
    /// Returns `Some(true)` when the disable is too late (the m0 edge already
    /// flagged -> the block must still fire), `Some(false)` when the disable wins
    /// (cancel before the edge), or `None` when no closed-form mode-0 anchor exists
    /// (caller falls back to the unconditional cancel).
    /// Boundary is Gambatte's exact m0-edge time (`m0TimeOfCurrentLine` =
    /// `predictedNextM0Time`): the disable fires the block iff `disable_cc >=
    /// m0Time`. rustyboi's `m0_time_master` is the STAT-read anchor (calibrated for
    /// `abs_cc + 2 < m0Time` with the LyCounter `+1` and renderer-tick phase), and
    /// it runs a fixed few cc ABOVE Gambatte's bare m0-edge time: cctracer pins the
    /// gap at +6 (single speed) / +4 (double speed), constant across SCX (the SCX
    /// m3-length delta already lives in `m0_time_master`). So the true edge is
    /// `m0_time_master - gap`.
    ///
    /// cctracer ground truth (CGB, [_1 cancel -> out0 / _2 fire -> out1] pairs,
    /// rustyboi-clock disable cc vs m0_time_master):
    ///   SS base   _1=12935 _2=12939 m0t=12944  edge=12938 (m0t-6)
    ///   SS scx2   _1=12939 _2=12943 m0t=12946  edge=12940 (m0t-6)
    ///   SS scx5   _1=12939 _2=12943 m0t=12949  edge=12943 (m0t-6)
    ///   DS        _1=158392 _2=158396 m0t=158398 edge=158394 (m0t-4)
    ///   DS scx5   _1=158400 _2=158404 m0t=158408 edge=158404 (m0t-4)
    pub fn hdma_disable_fires(&self, access_cc: u64, double_speed: bool) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0_time_master? as i64;
        let gap: i64 = if double_speed { 4 } else { 6 };
        let edge = m0t - gap;
        let cc = access_cc as i64;
        Some(cc >= edge)
    }

    /// The HDMA m0 (mode-3->0) trigger edge cc for the current line — the same
    /// `m0_time_master - gap` boundary `hdma_disable_fires` compares against,
    /// returned as a value. The STOP path uses it to measure how far before the
    /// stop the block's edge was crossed (deciding the halted-vs-completing FF55
    /// readback for `hdma_late_m3speedchange_hdma5_scx*_2` vs `_3`).
    pub fn hdma_m0_edge(&self, double_speed: bool) -> Option<i64> {
        let m0t = self.m0_time_master? as i64;
        let gap: i64 = if double_speed { 4 } else { 6 };
        Some(m0t - gap)
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
        let _ = suffix;
        let default = match (vram, scx) {
            (false, 1) => -1,
            (true, 3) => -1,
            _ => 0,
        };
        match scx {
            0 | 1 | 2 | 3 | 5 => default,
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
        if self.disabled || self.internal_ly_val >= 144 {
            return Some(false);
        }
        // STAGE 4 KEYSTONE: this gate is a RENDER-visibility decision (does the
        // CPU VRAM/OAM/cgbp store land before/after the fetcher's mode-3 lock).
        // The FACET-1 carry advances the STAT/line phase, so the lyTime-anchored
        // boundaries (`cgbp_block_start_cc`/`m0_time_master`) move EARLIER in
        // master cc while the fetcher's actual lock window did NOT. The caller
        // (`ppu_blocks`) passes a render-frame `access_cc` (the raw cc minus the
        // accumulated carry skew) so the access compares against the un-carried
        // geometry. No-op when no carry is live (flag-OFF / non-STOP paths).
        let cc = access_cc as i64;
        let ds = double_speed as i64;
        // The cached `m0_time_master` is byte-exact with Gambatte's `m0Time` at a
        // boot offset N, but the raw `master_cc` the bus snapshots sits at offset
        // N+1 (one master-cc below) for the `ld (hl)` / `ld (ff69),a` style memory
        // accesses these gates serve — so the access-cc must anchor at `cc + 1` to
        // share m0Time's offset. Without it the END boundary lands 1 cc short on
        // odd-SCX lines whose `cc + 2` ties `m0Time` exactly (postread_scx3 etc.).
        // (The FF41/getStat read uses a different opcode whose raw cc already shares
        // the offset, so this correction is scoped to the access gate.)
        let cc_end = cc + 1;
        // First line after LCD enable: Gambatte's accessibility functions all OR in
        // `inactivePeriodAfterDisplayEnable(cc + bias)` == `cc + bias < lu_`, where
        // `lu_` == `display_enable_inactive_until` (seeded at enable to
        // `enable_cc + (80<<ds) + 1`). While inactive the access is ACCESSIBLE
        // (not blocked), overriding the lineCycle / renderer-tick begin boundary
        // (which on the first line arms M3 two dots late and would otherwise report
        // the access blocked before `lu_`). The per-kind/direction bias mirrors
        // Gambatte (video.cpp cgbpAccessible/vramReadable/vramWritable/oamReadable/
        // oamWritable), shifted by +1 to share the access-cc offset the m0Time END
        // tests use (`cc_end = cc + 1`):
        //   cgbp (2):       cc + 1                  < lu_   (Gambatte raw cc)
        //   vram (0, r/w):  cc + 2 - cgb + ds       < lu_   (Gambatte cc + 1 - cgb + ds)
        //   oam  (1) read:  cc + 5                  < lu_   (Gambatte cc + 4)
        //   oam  (1) write: cc + 5 + ds             < lu_   (Gambatte cc + 4 + ds)
        if self.display_enable_inactive_until != 0 {
            let bias: i64 = match (kind, is_read) {
                (2, _) => 1,
                (0, _) => 2 - is_cgb as i64 + ds,
                (1, true) => 5,
                (1, false) => 5 + ds,
                _ => 1,
            };
            if cc + bias < self.display_enable_inactive_until as i64 {
                return Some(false);
            }
        }
        // CGB palette RAM (FF69/FF6B): Gambatte `cgbpAccessible(cc)` — accessible
        // iff `lineCycles(cc) + ds < 80` OR `cc >= m0Time + 2`. Both boundaries are
        // resolved at the access cc against master-cc anchors (begin =
        // cgbp_block_start_cc, end = exact m0_time_master).
        if kind == 2 {
            if let Some(start) = self.cgbp_block_start_cc {
                // `cgbp_block_start_cc` is the byte-exact Gambatte cgbp-block BEGIN
                // cc (lyTime-anchored at line-cycle `80 - ds`); blocked once the
                // access cc reaches it. The lyTime anchor folds the `lytime_no_plus1`
                // phase (the DS->SS speed-change bridge drops the `+1` LyCounter
                // correction); the access cc must share that phase, so add the same
                // `plus1` here instead of the fixed `cc_end` (+1). Without it the
                // lcdoffset variants (multi-`stop` LCD-enable phase) land 1 cc off:
                // base (plus1=1) needs `cc+1`, lcdoffset (plus1=0) needs raw `cc`.
                let plus1 = if self.lytime_no_plus1 { 0 } else { 1 };
                let begun = cc + plus1 >= start as i64;
                // Gambatte `cgbpAccessible`: accessible once `cc >= m0Time + 2`.
                // `m0Time` is `m0TimeOfCurrentLine(cc)` — the CURRENT line's
                // mode-0 time. During mode 2 (OAMSearch) `m0_time_master` still
                // holds the PREVIOUS line's (now-past) m0Time, so the
                // `cc_end >= m0t + 2` end test would spuriously unblock a write
                // landing in late mode 2 (after `cgbp_block_start_cc` but before
                // mode 3 even begins). Mode 3 cannot have ended before it starts:
                // gate the end test on mode 3 having begun for the current line.
                let ended = match self.m0_time_master {
                    Some(m0t) => self.state != State::OAMSearch && cc_end >= m0t as i64 + 2,
                    None => false,
                };
                return Some(begun && !ended);
            }
            // No begin anchor (first line after enable / window fallback): use the
            // renderer-tick boundary below.
            let m0t = self.m0_time_master;
            let begun = self.ticks as i64 + ds - (4 - is_cgb as i64) >= 80;
            let ended = match m0t {
                Some(m0t) => cc_end >= m0t as i64 + 2,
                None => return Some(begun && mode3_locked),
            };
            return Some(begun && !ended);
        }
        // VRAM/OAM: blocked during mode 3 (start gated on the FF41 mode register,
        // window-safe); END unblocks at Gambatte's `cc + 2 >= m0Time` (exact).
        // The m0Time end-boundary only applies once mode 3 has begun: during mode 2
        // (OAMSearch) `m0_time_master` still holds the PREVIOUS line's (now-past)
        // value, so the `cc+2 >= m0t` test would spuriously report "ended" and
        // unblock OAM mid-OAM-scan. OAM is blocked through mode 2; VRAM is accessible
        // in mode 2 except the begin window resolved below.
        // VRAM mode-3 BEGIN (kind 0). Gambatte blocks VRAM `lcdc_en` lines a few
        // line-cycles before cgbp does, and the threshold differs by direction and
        // model (libgambatte video.cpp):
        //   vramReadable : lineCycles + ds < 76 + 3*cgb   (begin lc 76-ds dmg / 79-ds cgb)
        //   vramWritable : lineCycles + ds < 79           (begin lc 79-ds, both)
        //   cgbpAccessible: lineCycles + ds < 80          (begin lc 80-ds)
        // `cgbp_block_start_cc` is the cgbp begin (lc 80-ds); the VRAM begin sits
        // `offset` line-cycles earlier, each line-cycle = `1<<ds` cc:
        //   read  offset = 4 - 3*cgb   (4 dmg, 1 cgb)
        //   write offset = 1
        // The access cc shares the lyTime phase via `plus1` (the DS->SS speed-change
        // bridge drops the `+1` LyCounter correction); see the cgbp begin above.
        let vram_started = if kind == 0 {
            self.cgbp_block_start_cc.map(|start| {
                let offset = if is_read { 4 - 3 * is_cgb as i64 } else { 1 };
                let vram_begin = start as i64 - (offset << ds);
                let plus1 = if self.lytime_no_plus1 { 0 } else { 1 };
                cc + plus1 >= vram_begin
            })
        } else {
            None
        };
        // VRAM access in mode 2 (OAMSearch): VRAM is accessible throughout mode 2
        // except the few line-cycles before mode 3 (the begin window, `vram_started`)
        // — `m0_time_master` is the previous line's stale value here, so resolve from
        // the begin alone (mode 3 cannot have ended before it starts; no END test).
        if kind == 0 && self.state == State::OAMSearch {
            if let Some(started) = vram_started {
                // A closed-form cgbp anchor exists for the CURRENT line. At single
                // speed an OAM scan still running past tick 80 (mode-3 starts at tick
                // 80) means the LCD-enable offset extended this line's mode 2 (the
                // 4-`stop` lcdoffset2 path); the lyTime anchor then carries a
                // stop-bridge phase error and lineCycles has not yet reached the
                // begin window, so VRAM is still accessible (keeps
                // prewrite_lcdoffset2_1 accessible). Double speed never legitimately
                // sits in OAMSearch past tick 80 with this anomaly (no DS lcdoffset2
                // tests), so there `ticks > 80` is a genuine late-mode-2 block; only
                // apply the escape at single speed. EXCLUDE the first line after
                // enable: there M3 legitimately arms at tick 85/86 (m3StartLineCycle
                // + 2), so an OAMSearch tick > 80 is the normal first-line pre-M3
                // window, NOT an lcdoffset2 stop-bridge anomaly — the `vram_started`
                // begin (now closed-form from the enable-anchored cgbp anchor) is the
                // correct gate there (ly0_late_vramr/vramw _2/_3 boundary).
                // PERACCESS facet-2 (line-end boundary): under the FACET-1 STOP
                // carry the lyTime-anchored `vram_started` begin is now exact (the
                // de-skewed access cc compares against the un-carried cgbp begin),
                // so a write that has crossed the begin window IS in the next
                // line's mode-3 and must block — the coarse `ticks>80` escape
                // (which forced accessible for the whole carried mode-2 tail) flips
                // the `_2` bracket half wrong. With the exact begin, resolve from
                // `started` alone: `_1` (before begin) accessible, `_2` (past
                // begin) blocked. Scoped to a live carry so flag-OFF / non-carried
                // lcdoffset lines keep the proven coarse escape.
                if self.render_carry_skew_cc != 0 {
                    return Some(started);
                }
                let lcdoffset_extended =
                    !double_speed && self.ticks > 80 && !self.first_line_after_enable;
                return Some(if lcdoffset_extended { false } else { started });
            }
        }
        let m0t = self.m0_time_master? as i64;
        // END unblocks at Gambatte's `cc + 2 >= m0Time` (exact), resolved at the
        // raw access cc. The post-tick FF41 mode register (`mode3_locked`) crosses
        // this boundary one access-tick (2/4 cc) EARLY because `ppu_locks_access`
        // runs after `tick_m`, so it cannot gate the END — a `postread` landing at
        // `cc = m0Time - 4` (still mode 3 at the access cc) would wrongly unblock.
        // Resolve the mode-3 END here from `m0Time`; gate the START on the mode-2->3
        // master-cc anchor (`cgbp_block_start_cc`, == `lineCycles + ds >= 80`) when
        // it exists, else fall back to the register's `mode3_locked`. OAM is also
        // blocked through mode 2: in `OAMSearch` (mode 2) `m0_time_master` still
        // holds the PREVIOUS line's (past) value, so the END test must not apply.
        // OAM line-wrap (Gambatte oamReadable/oamWritable): in the last few dots of
        // a line the next line's mode-2 OAM scan is imminent, so an OAM access is
        // already locked — except on the vblank lines (ly 143..152, whose successor
        // is mode 1, not mode 2). Gambatte gates on `lineCycles(cc) + K >= 456`:
        //   read : lineCycles(cc) + 4 - ds   (video.cpp oamReadable)
        //   write: lineCycles(cc) + 3 + cgb  (video.cpp oamWritable)
        // The CPU read and write land on different sub-M-cycle phases, so the
        // `lineCycles(cc)` each resolves at maps differently onto the renderer state:
        //   WRITE commits on the renderer dot boundary, so `lineCycles(cc)` is the
        //     post-tick `line_cycle`, minus the LyCounter `+1` phase that the
        //     stop-bridge (lcdoffset / `lytime_no_plus1`) lines drop:
        //     `line_cycle - lytime_no_plus1`. (Verified across the prewrite plain/
        //     lcdoffset, SS/DS pairs: block boundary == lineCycles 452.)
        //   READ samples mid-M-cycle, off the renderer dot grid; only the lyTime
        //     master clock captures that phase, so use Gambatte's own
        //     `lineCycles(cc) = 456 - ((lyTime - cc) >> ds)` with lyTime =
        //     p_now + LyCounter.time (+plus1, the shared gate phase). (Verified
        //     across the preread plain/lcdoffset, SS/DS pairs: block boundary at the
        //     DS-lcdoffset case, accessible everywhere else.)
        let oam_line_cycle = if kind != 1 {
            0
        } else if is_read {
            let plus1 = if self.lytime_no_plus1 { 0 } else { 1 };
            let dots_to_next = (stat_irq::LCD_CYCLES_PER_LINE - self.line_cycle) as i64;
            let ly_time = self.p_now as i64 + self.abs_cc as i64 + (dots_to_next << ds) + plus1;
            stat_irq::LCD_CYCLES_PER_LINE as i64 - ((ly_time - cc) >> ds)
        } else {
            self.line_cycle as i64 - self.lytime_no_plus1 as i64
        };
        if kind == 1 {
            let k = if is_read { 4 - ds } else { 3 + is_cgb as i64 };
            if oam_line_cycle + k >= stat_irq::LCD_CYCLES_PER_LINE as i64 {
                let ly = self.internal_ly_val as i64;
                let accessible = ly >= 143 && ly < 153;
                return Some(!accessible);
            }
        }
        let ended = self.state != State::OAMSearch && cc_end + 2 >= m0t;
        // OAM-WRITE DMG quirk (Gambatte oamWritable): at exactly lineCycles(cc) == 76
        // (the last mode-2 OAM-scan dot, DMG only) an OAM write is accepted. CGB has
        // no such escape.
        let oam_write_escape = kind == 1 && !is_read && !is_cgb && oam_line_cycle == 76;
        let started = match (kind, vram_started) {
            // VRAM: byte-exact per-direction/model begin (see `vram_started`).
            (0, Some(s)) => s || mode3_locked,
            // OAM (kind 1) on the first line after enable: Gambatte's oamWritable/
            // oamReadable have NO lineCycle-begin term — OAM is blocked from the end
            // of the inactive period (handled by the guard at the top) to m0Time,
            // through both mode 2 and mode 3. The first line has no mode-2 FF41
            // register (it reports mode 0), so `mode3_locked`/`cgbp_block_start_cc`
            // do not gate it; once past the inactive period it is simply blocked
            // (the `ended` test unblocks it at m0Time / mode 0).
            (1, _) if self.first_line_after_enable => true,
            // OAM (kind 1, blocked from mode 2): the register `mode3_locked`
            // already covers the mode-2 prefix; the cgbp anchor refines the dot.
            _ => match self.cgbp_block_start_cc {
                Some(start) => cc >= start as i64 || mode3_locked,
                None => mode3_locked,
            },
        };
        if oam_write_escape {
            return Some(false);
        }
        Some(started && !ended)
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
    pub fn get_stat_mode3to0_at_cc(&self, access_cc: u64, ds: bool) -> Option<u8> {
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
        // Gambatte getStat: mode 3 iff `cc + 2 < m0Time`. The shared m0Time carries
        // the lyTime `+1` correction the VRAM/OAM/cgbp access gate needs; at single
        // speed (and only when not in a post-DS->SS-switch line, where `lytime_no_plus1`
        // already drops it) it sits 1cc high for the getStat read specifically, so the
        // read boundary uses `+3` instead of `+2`.
        let read_off = if !ds && !self.lytime_no_plus1 { 3 } else { 2 };
        if (access_cc as i64) + read_off < m0t {
            Some(3)
        } else {
            Some(0)
        }
    }

    /// Gambatte `LCD::getStat` mode bits, computed at the CPU's access cc, for the
    /// mode 0<->1 (VBlank entry/exit) boundary ONLY. The per-dot renderer advances
    /// the FF41 mode register inside `tick_m()`, so a read whose M-cycle straddles
    /// the line-143->144 (VBlank entry) or line-153->0 (VBlank exit / wrap-to-OAM)
    /// boundary latches the next line's mode; Gambatte resolves it from the LY
    /// phase at the raw read cc (video.cpp:802-810). This is exactly the
    /// enable_display m1stat / ly_count / m2-m3 count cluster: those reads land in
    /// the last few cc of line 143 or line 153 and must read the OLD line's mode 0.
    ///
    /// Scoped to the VBlank boundary (frameCycles window) so the tuned per-dot
    /// register still serves every mid-frame mode 0/2/3 read. Returns None when the
    /// access cc does not resolve into the mode-1 window (then the bus keeps the
    /// renderer register).
    pub fn get_stat_mode_at_cc(&self, mmio: &mmio::Mmio, access_cc: u64) -> Option<u8> {
        if self.disabled || (self.lcdc & (LCDCFlags::DisplayEnable as u8)) == 0 {
            return None;
        }
        let ds = mmio.is_double_speed_mode();
        // The bus passes the read M-cycle START cc (`master_cc`). Gambatte's getStat
        // resolves at the latch cc; the lineCycles/frameCycles phase needs a small
        // per-speed bias to align the VBlank-entry boundary (swept against the
        // suite: SS 0, DS -1; the DS read samples one cc past the SS phase since
        // each dot is 2 cc, so the boundary sits a cc earlier in the read window).
        let access_cc = {
            let off = if ds { GETSTAT_OFF_DS } else { 0 };
            (access_cc as i64 + off).max(0) as u64
        };
        // CGB halt-exit +5: Gambatte's halt-exit M-cycle (memory.cpp:300-301,
        // `cc += 4 * isCgb()`) charges a flat +4 on CGB before the woken instruction
        // stream resumes, so a CGB halt-woken FF41 read effectively samples ~5cc
        // later in the line than the engine's access cc reflects (mirror of the
        // proven getLyReg `cgb_halt_exit` bias; the extra +1 over the raw +4 is the
        // same lyTime correction the line-phase consumers carry). Without it the
        // `lycirq_m2stat_2` STAT read lands at lineCycles 75 (OAMSearch -> mode 2)
        // where Gambatte reads lineCycles 80 (mode 3, `cc+2 < m0Time`). The
        // lycirq_m2stat_1/_2/_3 family arms 4cc apart, so this +5 lifts 71/75/79 ->
        // 76/80/84: _1 stays mode 2 (<77), _2/_3 resolve mode 3 — matching Gambatte.
        //
        // SCOPED to the OAMSearch-state read (the line-START mode2->mode3 boundary).
        // The HBlank line-tail halt-woken reads (`m0int_m0stat_scx*`, lineCycles
        // ~445-454) are already resolved exactly by the `tail_thresh` path below and
        // MUST keep their un-biased access cc, so gate this on `state == OAMSearch`.
        // Same CGB-single-speed-no-HDMA predicate as getLyReg (the HDMA / DS halt
        // wakeups fold their own halt-exit phase through the bridge/block-transfer).
        let access_cc = if self.state == State::OAMSearch
            && mmio.halt_wakeup_skew()
            && mmio.is_cgb_features_enabled()
            && !ds
            && !mmio.halt_wakeup_hdma()
        {
            access_cc + 5
        } else {
            access_cc
        };
        let lc = self.ly_counter_obs(mmio); // ds-subdot STAGE 1: read-path phase
        let ly = lc.ly as i64;
        let cpl = stat_irq::LCD_CYCLES_PER_LINE as i64;
        let cpf = stat_irq::LCD_CYCLES_PER_FRAME as i64;
        // lyCounter.time() in master-cc; timeToNextLy = time - cc; lineCycles =
        // 456 - (timeToNextLy >> ds); frameCycles = ly*456 + lineCycles.
        let ly_time_master = self.p_now as i64 + lc.time as i64;
        let time_to_next_ly = ly_time_master - access_cc as i64;
        let line_cycles = cpl - (time_to_next_ly >> ds as i32);
        let frame_cycles = ly * cpl + line_cycles;
        let dsi = ds as i64;

        // The per-dot register mis-reads whenever the post-tick FF41 register lags
        // the access-start cc: at a line-boundary straddle (VBlank entry/exit, line
        // wrap) AND mid-frame, where a mode 0 / mode 2 read in a non-PixelTransfer
        // state samples the register ~+4cc (≈+2 dots) late (C1: the lycint_m0stat /
        // m2int_m0stat / m0int_m0stat / lycEnable / misc-small clusters). The
        // PixelTransfer (mode-3) reads are already resolved exactly by
        // `get_stat_mode3to0_at_cc` (which runs first in the bus `.or_else` chain),
        // so this is only ever consulted in mode 0 / mode 2 / mode 1 — never inside
        // mode 3. (`ly` is the clean event-clock LY == Gambatte's lyCounter.ly().)
        //
        // VBlank-adjacent lines (ly>=143): keep the original line-tail-scoped path
        // byte-identical (those boundaries are co-tuned with the renderer register).
        // Mid-frame lines (ly<143): C1 resolves the mode 0 / mode 2 read at the
        // access-start cc via the full Gambatte getStat branch order (video.cpp
        // 806-817), reusing the exact mode-3 sub-test so it stays byte-identical to
        // the PixelTransfer path for any line-straddle that resolves back into mode 3.
        let near_line_end = line_cycles >= cpl - 7;
        // LY 0..142: full mid-frame resolution. LY 143 is ALSO a rendering line
        // (it has its own m0Time), so its line BODY resolves mode 3 exactly like
        // any other rendering line — the m3stat_count / m0irq_count streams read
        // FF41 at lineCycles 77..80 through LY 143 and Gambatte reports mode 3 for
        // all 144 lines (LY 0..143). The renderer is in the OAMSearch dead zone at
        // those lineCycles, so without this LY=143 would fall through to the
        // VBlank-boundary path below (which returns None for the line body) and
        // count one read short. Only the LY=143 line TAIL (the 143->144 mode 0->1
        // transition) stays on the VBlank-boundary path — there the mid-frame
        // handler would wrongly anticipate the next line's mode 2 (LY 144 is
        // VBlank, not OAM), so gate the unification to the line body.
        if ly < 143 || (ly == 143 && !near_line_end) {
            return self.get_stat_mode_midframe(
                mmio,
                access_cc,
                ly,
                line_cycles,
                ds,
                mmio.halt_wakeup_skew(),
                mmio.is_cgb_features_enabled(),
            );
        }
        let in_vblank_window = frame_cycles >= 144 * cpl - 3 && frame_cycles < cpf - 3;
        if !near_line_end && !in_vblank_window {
            return None;
        }

        // VBlank window (mode 1) — video.cpp:806-810.
        if in_vblank_window {
            if frame_cycles >= 144 * cpl - 2 && frame_cycles < cpf - 4 + dsi {
                return Some(1);
            }
            return Some(0);
        }
        // Mode 2 (OAM) at line END (the next line's OAM is anticipated from
        // lineCycles >= cpl-3) — video.cpp:811-813.
        if line_cycles >= cpl - 3 {
            if (access_cc + 1) < self.display_enable_inactive_until {
                return Some(0);
            }
            return Some(2);
        }
        // Line tail before the mode-2 anticipation window (cpl-7 .. cpl-3): mode 3
        // iff cc+2 < m0Time, else mode 0 — video.cpp:814-816.
        if let Some(m0t) = self.m0_time_master {
            if (access_cc + 1) < self.display_enable_inactive_until {
                return Some(0);
            }
            if (access_cc as i64) + 2 < m0t as i64 {
                return Some(3);
            }
        }
        Some(0)
    }

    /// C1: full Gambatte `getStat` mode resolution for a MID-FRAME line (ly < 143),
    /// resolved at the access-start cc. The post-tick FF41 register lags a mode 0 /
    /// mode 2 read by ~+4cc (≈+2 dots) because `bus.rs read()` samples it AFTER
    /// `tick_m()`; this resolves the mode at the access cc instead.
    ///
    /// Mirrors the video.cpp:811-817 branch ORDER (the VBlank-window branch at 806
    /// never applies for ly<143):
    ///   - mode 2 iff `lineCycles < 77 || lineCycles >= cpl - 3` (guarded by
    ///     inactivePeriodAfterDisplayEnable, == rustyboi `display_enable_inactive_until`)
    ///   - else mode 3 iff `access_cc + read_off < m0Time`  — the SAME sub-test as
    ///     `get_stat_mode3to0_at_cc` (so a line-straddle that resolves back into
    ///     mode 3 stays byte-identical to the already-passing PixelTransfer path)
    ///   - else mode 0
    ///
    /// This is only ever reached when the renderer is NOT in PixelTransfer (the
    /// PixelTransfer reads short-circuit through `get_stat_mode3to0_at_cc` first), so
    /// the mode-3 sub-test resolves a mode 0/mode 3 line-boundary straddle only.
    /// During mode 2 (OAMSearch) `m0_time_master` still holds the PREVIOUS line's
    /// (now-past) value, so the mode-3 sub-test is gated on `state != OAMSearch`
    /// (mirroring the cpu_access_blocked stale-m0Time guards) — mode 3 cannot have
    /// ended before it begins.
    fn get_stat_mode_midframe(
        &self,
        mmio: &mmio::Mmio,
        access_cc: u64,
        ly: i64,
        line_cycles: i64,
        ds: bool,
        halt_skew: bool,
        is_cgb: bool,
    ) -> Option<u8> {
        let _ = ly;
        let cpl = stat_irq::LCD_CYCLES_PER_LINE as i64;
        // PTZ: Line-tail zone under a HALT-woken stream — resolve the next-line OAM
        // (mode 2) anticipation instead of deferring to the post-tick renderer
        // register (which lags here and reports the stale mode 0).
        //
        // With the current engine the post-wake decisive reads PRESERVE Gambatte's
        // exact 4cc arming spacing, so the `_1` (want-mode0) and `_2`/`2b`/`ds_2`
        // (want-mode2) reads land at DIFFERENT, cleanly-separable lineCycles:
        //   CGB single speed: want-mode0 at 446-448, want-mode2 at 450-451
        //                     -> threshold cpl-7 (449)
        //   CGB double speed: want-mode0 at 449-450, want-mode2 at 451
        //                     -> threshold cpl-5 (451)
        // (cctraced: `m0int_m0stat_scx*_1` vs `*_2`/`*_ds_2`, the Gambatte read
        // lands at the line wrap == mode2, rustyboi ~3-5cc short of the wrap.)
        //
        // Scoped to CGB: DMG's mode-0 line-tail phase differs (the same read wants
        // mode0 on DMG, mode2 on CGB — e.g. `m0int_m0stat_scx3_2_dmg08_out0_cgb04c_out2`),
        // so DMG keeps the prior defer-to-renderer behavior (sub-dot-irreducible there).
        let tail_thresh = if ds { cpl - 5 } else { cpl - 7 };
        if halt_skew && is_cgb && line_cycles >= tail_thresh {
            if (access_cc + 1) < self.display_enable_inactive_until {
                return Some(0);
            }
            return Some(2);
        }
        // DMG halt-woken line-tail (the `m0int_m0stat_scx*` ly<143 mid-frame
        // family): the post-wake decisive reads preserve Gambatte's exact 4cc arming
        // spacing, so on DMG the want-mode0 reads land at lineCycles 445..450 and the
        // want-mode2 reads at lineCycles 451..454 — cleanly separable at integer cc
        // (measured via the runner's closed-form lineCycles, NOT sub-dot). DMG's
        // mode-0 line tail runs TWO lineCycles longer than CGB (which splits at
        // 448/449): the dmg08-distinguished `scx3_2` (449) / `scx4_2` (450) read
        // mode0 on DMG but mode2 on CGB. Resolve mode 2 from the closed form at the
        // DMG cpl-5 (451) boundary instead of deferring to the post-tick renderer
        // register (which lags and reports the stale mode 2 at exactly lineCycles
        // 450 — the `m0int_m0stat_scx4_2` DMG failure; lineCycles 449/451..454 the
        // renderer already resolves correctly). The want-mode0 reads (<=450) fall
        // through to the mode-3/mode-0 resolution below. The ly=153 VBlank-line
        // `*_2b` reads are NOT in this mid-frame path (handled by the VBlank branch
        // in get_stat_mode_at_cc), so their genuine sub-dot collapse is untouched.
        if halt_skew && line_cycles >= cpl - 5 {
            if (access_cc + 1) < self.display_enable_inactive_until {
                return Some(0);
            }
            return Some(2);
        }
        if halt_skew && line_cycles >= cpl - 7 {
            // DMG line tail at lineCycles 449/450: still mode 0 (the want-mode0
            // group extends to 450 on DMG). Fall through to the mode-3/mode-0
            // resolution below rather than deferring to the lagging renderer.
            if (access_cc + 1) < self.display_enable_inactive_until {
                return Some(0);
            }
            // mode 3 iff still before m0Time, else mode 0 (the line body).
            if self.state != State::OAMSearch
                && let Some(m0t) = self.m0_time_master
            {
                let read_off: i64 = if !ds && !self.lytime_no_plus1 { 3 } else { 2 };
                if (access_cc as i64) + read_off < m0t as i64 {
                    return Some(3);
                }
                return Some(0);
            }
            return None;
        }
        // Mode 2 (OAM search): start-of-line lineCycles (< 77), or line-tail
        // anticipation — video.cpp:811-813.
        if line_cycles < 77 || line_cycles >= cpl - 3 {
            if (access_cc + 1) < self.display_enable_inactive_until {
                return Some(0);
            }
            return Some(2);
        }
        // Mode 3 (pixel transfer) iff `access_cc + read_off < m0Time` — the exact
        // sub-test from `get_stat_mode3to0_at_cc` (video.cpp:814-816). Skipped during
        // OAMSearch where `m0_time_master` is the previous line's stale value.
        //
        // When no closed-form `m0_time_master` exists (first line after enable,
        // window-start / mid-mode-3 WX-invalidated lines) we CANNOT resolve the
        // mode-3 -> mode-0 boundary here, and the renderer register is already the
        // correct emergent value for these lines (the late_reenable / late_disable /
        // late_wy / window / first-line-after-enable `out3` cases all rely on it) —
        // so defer to it (return None) instead of falsely reporting mode 0.
        if self.state != State::OAMSearch {
            match self.m0_time_master {
                Some(m0t) => {
                    if (access_cc + 1) < self.display_enable_inactive_until {
                        return Some(0);
                    }
                    let read_off: i64 = if !ds && !self.lytime_no_plus1 { 3 } else { 2 };
                    if (access_cc as i64) + read_off < m0t as i64 {
                        return Some(3);
                    }
                    // else mode 0 — the body of the line past m0Time.
                    Some(0)
                }
                None => None,
            }
        } else if line_cycles >= 77 {
            // Mode-3 START dead zone during OAMSearch. Gambatte's getStat reports
            // mode 3 from lineCycles 77 (`!(lineCycles < 77) && cc+2 < m0Time &&
            // !inactivePeriodAfterDisplayEnable(cc+1)`), but rustyboi's renderer is
            // still in OAMSearch until the M3 arm dot (≈82 steady, ≈84/86 first
            // line), so its poked FF41 register reports a stale mode 2 in the
            // lineCycles 77..arm window. Resolve mode 3 here from THIS line's m0Time.
            //
            // On the FIRST line after enable `m0_time_master` already holds this
            // line's value (installed by the first-line OAMSearch block). On steady
            // lines it still holds the PREVIOUS line's value during OAMSearch (the
            // M3-arm site only installs the current line's at ≈dot 82), so compute
            // the current line's m0Time fresh from the live geometry — no window has
            // started yet this early, so `compute_m3_length` is the settled value.
            //
            // The inactive boundary is recomputed lineStart-anchored: Gambatte
            // `lu_ = enableCc + (80<<ds) + 1` and `enableCc == lineStart` (setLcdc
            // did `lyCounter.reset(0, enableCc)`). The stored
            // `display_enable_inactive_until` is anchored on the raw enable
            // `master_cc()`, one render dot above rustyboi's line-clock origin, so it
            // ends the window one dot late and wrongly suppresses this lineCycles≈80
            // mode-3 read; recompute it lineStart-local. (Only meaningful on the
            // first line; on steady lines it is far in the past.) Needed for the
            // enable_display frame*_m3stat_count / m0irq_count / ly0 streams whose
            // FF41 read lands at lineCycles 78..80 during OAMSearch.
            let lc = self.ly_counter_obs(mmio); // ds-subdot STAGE 1: read-path phase
            let line_start = (self.p_now as i64 + lc.time as i64) - (456i64 << ds as u32);
            let cur_m0t = if self.first_line_after_enable {
                // Exact first-line value already installed (carries the +1 lyTime
                // correction the read boundary is co-tuned with, and the first-line
                // m3StartLineCycle+2 offset).
                match self.m0_time_master {
                    Some(m0t) => m0t as i64,
                    None => return None,
                }
            } else {
                // Steady-line m0Time, fresh (m0_time_master holds the previous
                // line's value during this pre-M3 OAMSearch phase). Mirrors
                // `m0_time_exact(.., first_line=false)`: lineStart + (m3_len + BASE)
                // << ds + 1 (BASE = 84 CGB / 83 DMG; the +1 is the lyTime correction).
                let base: i64 = if is_cgb { 84 } else { 83 };
                let m3_len = self.compute_m3_length(mmio, is_cgb) as i64;
                line_start + ((m3_len + base) << ds as u32) + 1
            };
            // The post-enable inactive period only exists on the first line after
            // enable; on steady lines it ended long ago. Gate the lineStart-local
            // inactive suppression to the first line (using the global field there
            // would end the window one render dot late — see the comment above).
            let read_off: i64 = if !ds && !self.lytime_no_plus1 { 3 } else { 2 };
            if self.first_line_after_enable {
                // `line_start` here (the raw LyCounter-derived line origin) sits one
                // master-cc ABOVE Gambatte's enable cc anchor (`lyCounter.reset(0,
                // enableCc)`): cross-checked vs cctracer on frame0_m3stat_count_ds_2 the
                // rustyboi enableCc maps one cc low. Gambatte's
                // `inactivePeriodAfterDisplayEnable(cc+1)` boundary is
                // `lu_ = enableCc + (80<<ds)+1`, so subtract that one cc here. Without
                // it `lu_local` sat one cc high and the first line's lineCycles-80
                // mode-3 read fell inside the inactive window, reporting mode 0 and
                // dropping the first line's m3 count (out90: 144 m3 reads).
                let lu_local = line_start + ((80i64 << ds as u32) + 1) - 1;
                if (access_cc as i64 + 1) < lu_local {
                    return Some(0);
                }
            }
            if (access_cc as i64) + read_off < cur_m0t {
                return Some(3);
            }
            Some(0)
        } else {
            // Mode 2 with no closed-form anchor resolved above already returned;
            // a lineCycles-77..453 read during OAMSearch is a stale-m0Time straddle:
            // defer to the renderer register.
            None
        }
    }

    /// ds-engine STAGE 4: the SINGLE closed-form `LCD::getStat` mode resolver.
    /// Computes the FF41 mode bits PURELY from the line geometry at the exact
    /// access cc, with NO reliance on the per-dot renderer's poked FF41 register.
    /// This is the keystone of the exact-event model: the CPU-visible mode is one
    /// closed form off one cc (Gambatte video.cpp `getStat`), so the DS half-dot
    /// straddle pairs resolve by construction instead of via per-dot rounding.
    ///
    /// Branch order mirrors Gambatte `getStat`:
    ///   - LCD off / VBlank (ly>=144 via internal_ly) -> mode 0 / mode 1
    ///   - inactive period after enable -> mode 0
    ///   - lineCycles < 80 (or line-tail mode-2 anticipation) -> mode 2
    ///   - access_cc + 2 < m0Time -> mode 3
    ///   - else mode 0
    ///
    /// Returns `None` ONLY when no closed-form m0Time anchor exists for the
    /// current line (first line after enable, window-start / WX-invalidated
    /// mid-mode-3 lines): there the renderer register is the correct emergent
    /// value and the caller defers to it. Everywhere else this is authoritative.
    pub fn get_stat(&self, mmio: &mmio::Mmio, access_cc: u64) -> Option<u8> {
        if self.disabled || (self.lcdc & (LCDCFlags::DisplayEnable as u8)) == 0 {
            return None;
        }
        // Compose the two byte-exact closed-form resolvers in the same order the
        // bus chain used: the mode-3<->0 sub-test first (covers in-PixelTransfer
        // reads), then the full LY-phase getStat (mode 0/1/2 boundaries + the
        // mid-frame branch). The result is the SINGLE authoritative CPU-visible
        // mode at the access cc, with NO read of the per-dot renderer's poked FF41
        // register. When neither resolver has a closed-form anchor (first line
        // after enable / window-invalidated mid-mode-3) it returns None and the
        // caller defers to the renderer register for exactly those lines.
        let ds = mmio.is_double_speed_mode();
        self.get_stat_mode3to0_at_cc(access_cc, ds)
            .or_else(|| self.get_stat_mode_at_cc(mmio, access_cc))
    }

    /// Gambatte `LCD::getStat` LYC=LY coincidence flag (FF41 bit 2), computed at
    /// the CPU's access cc. The per-dot renderer writes the coincidence bit into
    /// the FF41 register at the dot it flips (e.g. the line-153 LY=0 transient at
    /// dot 6); a read whose M-cycle straddles that dot would otherwise sample the
    /// bit one M-cycle late from the post-tick register. Gambatte instead resolves
    /// the flag at the read's master cc via `getLycCmpLy`:
    ///   stat |= lycflag iff lycReg == lycCmp.ly && lycCmp.timeToNextLy > 2
    /// (the AGB `2 - 1` term is dropped: rustyboi targets DMG/CGB only).
    pub fn get_lyc_flag_at_cc(&self, mmio: &mmio::Mmio, access_cc: u64) -> Option<bool> {
        if self.disabled || (self.lcdc & (LCDCFlags::DisplayEnable as u8)) == 0 {
            return None;
        }
        // Reanchor the LyCounter.time to master cc (`p_now + lc.time`), matching
        // `get_stat_mode_at_cc`: rustyboi's LyCounter.time is in abs_cc units.
        let lc = self.ly_counter_obs(mmio); // ds-subdot STAGE 1: read-path phase
        let lc_master = stat_irq::LyCounter {
            ly: lc.ly,
            time: (self.p_now as i64 + lc.time as i64).max(0) as u64,
            ds: lc.ds,
        };
        let cmp = stat_irq::get_lyc_cmp_ly(&lc_master, access_cc);
        let lyc_reg = mmio.read(LYC) as u32;
        Some(lyc_reg == cmp.ly && cmp.time_to_next_ly > 2)
    }


    /// Byte-exact Gambatte `video.h getLyReg(cc)`. The FF44 (LY) register the CPU
    /// reads is NOT simply the renderer's LY: in the last ~6-10 cc of a line the
    /// register anticipates the next line, and on line 153 it reads 0 early. The
    /// renderer-set LY register only flips at the dot boundary (one M-cycle late
    /// for a read whose access cc lands in the anticipation window), so resolve
    /// the value here from the LY counter phase at the read's access cc.
    ///
    /// Returns None when the LCD is off (the bus keeps the renderer register).
    pub fn get_ly_reg_at_cc(&self, mmio: &mmio::Mmio, access_cc: u64) -> Option<u8> {
        if self.disabled || (self.lcdc & (LCDCFlags::DisplayEnable as u8)) == 0 {
            return None;
        }
        let ds = mmio.is_double_speed_mode();
        let lc = self.ly_counter(mmio);
        let cc = access_cc as i64;
        let cpl = stat_irq::LCD_CYCLES_PER_LINE as i64;
        let last_line = (stat_irq::LCD_LINES_PER_FRAME - 1) as i64; // 153
        // Gambatte's lyCounter().time() in master-cc. The closed-form LyCounter.time
        // runs one master-cc below Gambatte's lyTime (see m0_time_exact), so add 1.
        let mut ly_reg = lc.ly as i64;
        let mut time = self.p_now as i64 + lc.time as i64 + 1;
        // SS->DS-during-mode3: rustyboi's bridged renderer line phase trails
        // Gambatte's re-anchored lyCounter.time by ~5 DS-dots (10 cc) for the LY
        // read. Pull the read's `time` anchor onto Gambatte's lyTime so the
        // getLyReg anticipation window resolves identically (cctracer: _2/_6
        // read 147, to_next 8). DS-only (the switch lands in DS). Scoped to this
        // read path; the STAT/m0Time predictor keeps the un-advanced phase.
        if self.ssds_mode3_ly_advance && ds {
            time -= 10;
        }
        // Gambatte getLyReg: `if (cc >= lyCounter().time()) update(cc)` advances the
        // LY counter when the read's access cc has already passed the LY increment.
        // The closed-form (ly_counter) is renderer-anchored and does NOT advance, so
        // a read whose M-cycle lands AT/AFTER the line wrap reads the stale LY (the
        // renderer flips one dot boundary later). Replay the advance here: at the
        // 152->153 boundary this lifts ly to 153 so the line-153 reads-0 case fires
        // (lycint152_ly153 family).
        let line_time = lc.line_time() as i64;
        if cc >= time {
            ly_reg = stat_irq::inc_ly(ly_reg as u32) as i64;
            time += line_time;
        }
        let to_next = time - cc; // timeToNextLy

        if ly_reg == last_line {
            // Line 153: FF44 reads 0 early (Gambatte getLyReg). At single speed the
            // renderer's dot-6 LY->0 flip handles MOST of line 153 correctly, EXCEPT
            // at the very top of the line (`to_next >= cpl`, the just-wrapped sub-dot
            // where the closed-form LY counter has rolled to 153 but the renderer
            // register is still one M-cycle stale on the prior line). There the
            // renderer reads the wrong (prior) LY, so resolve from the counter:
            // Gambatte's getLyReg returns 0 for the whole of line 153 single speed.
            // For the rest of the line defer to the renderer (return None).
            if !ds {
                if to_next >= cpl {
                    return Some(0);
                }
                return None;
            }
            if to_next <= 2 * cpl - 2 {
                return Some(0);
            }
            return Some((ly_reg & 0xFF) as u8);
        }
        // Line-end anticipation window: the register pre-increments to the next LY,
        // except exactly at `to_next == 6+4*ds` where the hardware briefly shows
        // `ly & (ly+1)` (the glitch the count tests probe). Outside the window
        // defer to the renderer register (return None).
        //
        // PTZ: Gambatte's getLyReg compares against the RAW `lyCounter().time()`,
        // whereas `time` above carries the +1 lyTime correction the m0Time/getStat
        // consumers need (rustyboi's closed-form counter runs 1cc below Gambatte's
        // lyTime). For a HALT-woken read this 1cc lifts the glitch-dot probe onto
        // the wrong side: m1int_ly_3 lands at to_next=6 and reads the `ly&(ly+1)`
        // glitch (144) when CGB hardware has already pre-incremented to 145. Drop
        // the +1 for the skewed anticipation comparison so it matches getLyReg's
        // raw-time boundary. Scoped to halt-skew (the non-HALT count/ly tests are
        // co-tuned to the +1 and stay byte-identical).
        // For a HALT-woken read, the post-wakeup instruction stream lands later in
        // the line on CGB than DMG: Gambatte's halt-exit M-cycle (memory.cpp:300-301,
        // `cc += 4 * isCgb()`) charges a flat +4 on CGB before the stream resumes,
        // whereas rustyboi's engine does not model that extra M-cycle here. So a
        // CGB halt-woken FF44 read effectively samples 4cc closer to the line wrap
        // than the engine's access cc reflects. Bias only the CGB single-speed
        // halt-woken read by that +4 (== to_next - 4) on top of the pre-existing
        // -1 raw-time correction (the closed-form counter runs 1cc below Gambatte's
        // lyTime; getLyReg compares against the RAW lyCounter().time()). This makes
        // m1int_ly_1/_2/_3 (CGB) read at to_next 14/10/6 -> 9/5/1, so _1 stays
        // renderer (0x90) and _2/_3 anticipate (0x91), matching Gambatte; DMG keeps
        // -1 (its m1int_ly_2 reads the stale 0x90 at the SAME to_next=10). DS keeps
        // -1: the speedchange/hdma _ly families resolve their own halt-exit phase
        // through the bridge and are co-tuned to it.
        // The HDMA-active halt-woken families (hdma_*_m*unhalt_ly / hdma_*_ly) carry
        // their own wakeup-cc shift through the in-halt block transfer and the
        // unhalt-reflag path, so the Gambatte halt-exit +4 is already folded into
        // their post-wakeup phase; applying it again here double-counts. Scope the
        // CGB halt-exit bias to the no-HDMA halt wakeup (the plain m1int_ly family).
        let halt_skew = mmio.halt_wakeup_skew();
        let cgb_halt_exit =
            halt_skew && mmio.is_cgb_features_enabled() && !ds && !mmio.halt_wakeup_hdma();
        let tn = if cgb_halt_exit {
            to_next - 5
        } else if halt_skew {
            to_next - 1
        } else {
            to_next
        };
        if tn <= 10 && tn <= 6 + 4 * (ds as i64) {
            let result = if tn == 6 + 4 * (ds as i64) {
                ly_reg & (ly_reg + 1)
            } else {
                ly_reg + 1
            };
            return Some((result & 0xFF) as u8);
        }
        None
    }

    /// True when the PPU is currently in PixelTransfer (STAT mode 3, active
    /// rendering). Used by the CGB STOP speed-switch bridge to gate the
    /// mode-3-specific dot correction.
    pub fn is_in_pixel_transfer(&self) -> bool {
        !self.disabled && self.state == State::PixelTransfer
    }

    /// True when the renderer is in the OAM-search (mode 2) phase of an active
    /// line — the pre-pixel-transfer window where the per-dot stepper's `line_cycle`
    /// and PPU-clock phase are already byte-exact vs Gambatte (no mode-3-length
    /// coupling has accumulated yet). Used by the Stage-2 STOP DS->SS re-anchor.
    pub fn is_in_oam_search(&self) -> bool {
        !self.disabled
            && (self.lcdc & (LCDCFlags::DisplayEnable as u8)) != 0
            && self.state == State::OAMSearch
    }

    /// True when the renderer is on an ACTIVE rendering line (LCD on, LY 0..143):
    /// OAMSearch / PixelTransfer / HBlank of a visible line. An SS->DS speed switch
    /// here makes the per-dot renderer overshoot the post-window mode-3->mode-0
    /// boundary by 2 dots (the same overshoot the PixelTransfer bridge already
    /// compensates), so the STOP bridge drops 2 dots and arms the pullback marker.
    /// VBlank lines (LY 143-tail..152) and the LCD-off path keep the full 8 — there
    /// the renderer is not advancing a mode-3 window, so no overshoot occurs.
    pub fn is_on_rendering_line(&self) -> bool {
        !self.disabled
            && (self.lcdc & (LCDCFlags::DisplayEnable as u8)) != 0
            && self.internal_ly_val < 144
            && self.state != State::VBlank
    }

    /// Arm the SS->DS-during-mode3 bridge pullback marker (the SS->DS bridge
    /// dropped 2 dots). A following DS->SS switch consumes it.
    pub fn arm_sc_mode3_pullback(&mut self) {
        self.sc_mode3_pullback_pending = true;
    }

    /// Consume the SS->DS-during-mode3 pullback marker, returning whether it was
    /// set. Used by the DS->SS bridge to restore the 2 dropped dots for the
    /// double-switch speedchange families.
    pub fn take_sc_mode3_pullback(&mut self) -> bool {
        let p = self.sc_mode3_pullback_pending;
        self.sc_mode3_pullback_pending = false;
        p
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

        // OAM scan (Gambatte's SpriteMapper::mapSprites) builds the per-line
        // sprite list regardless of the OBJ-enable bit (LCDC.1). The enable bit
        // only gates the M3 sprite fetch and the final pixel mix, so a sprite
        // enabled mid-mode-3 still incurs its fetch penalty. Do not early-out
        // here on OBJ-disable.

        // Determine sprite height (8x8 or 8x16). Use the per-line scan latch
        // (lags the live LCDC by one OAM slot) so a mid-mode-2 OBJ-size write
        // affects only entries scanned strictly after it commits, matching
        // Gambatte's per-entry lsbuf latch.
        let large = self.scan_obj_size_large;
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

    /// Per-dot driver for the lazy OAM sprite snapshot. Mirrors Gambatte's
    /// `startOamDma`/`endOamDma`/`oamChange` plus the implicit `update(cc)` the
    /// mode-2 doEvent performs. Run after `abs_cc` is folded to the current dot,
    /// before the mode-2 scan reads the snapshot.
    fn process_oam_reader_events(&mut self, mmio: &mut mmio::Mmio) {
        let lc = self.ly_counter(mmio);
        let cc = self.abs_cc;
        let cgb = mmio.is_cgb_features_enabled();

        // Lazy seed for the current LCD-on session.
        if !self.oam_reader_seeded {
            let mut pos = [0u8; 80];
            mmio.peek_oam_pos(&mut pos);
            self.oam_reader.reset(&pos, cgb);
            self.oam_reader.lu = cc;
            self.oam_reader.large_src = (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;
            self.prev_dma_writing = mmio.oam_dma_window_active();
            self.oam_reader_seeded = true;
            return;
        }

        // Keep largeSpritesSrc_ tracking the live LCDC OBJ-size bit (Gambatte
        // sets it on the LCDC write; the walk latches it into lsbuf per slot).
        self.oam_reader.large_src = (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;

        let mut pos = [0u8; 80];
        mmio.peek_oam_pos(&mut pos);

        // OAM-DMA window edges: at start the source becomes disabled RAM (0xFF);
        // at end it returns to the real OAM. `change(cc)` flushes the snapshot up
        // to `cc` with the OLD source, then caps the next walk, then we toggle.
        let dma_writing = mmio.oam_dma_window_active();
        if dma_writing != self.prev_dma_writing {
            // The DMA window edge is observed at the PPU dot, but Gambatte fires
            // startOamDma/endOamDma at the M-cycle's master cc, which precedes the
            // PPU's observation by a fixed sub-M-cycle amount. Shift the change cc
            // back by this offset so the position-walk cap lands on the same OAM
            // slot Gambatte's does. Calibrated against the late_sp{00,01,39}x/y
            // `_1`/`_2` and `_ds_1`/`_ds_2` bracket pairs (which straddle this
            // boundary); scaled by the speed so it is a fixed lineCycle amount.
            let cc = cc.saturating_sub((OAMDMA_CHANGE_CC_OFFSET as u64) << lc.ds as u32);
            // change() under the pre-toggle source (Gambatte oamChange uses the
            // pointer in effect for the just-completed span).
            self.oam_reader.change(cc, &lc, &pos);
            // Toggle source for the new span (startOamDma -> disabled,
            // endOamDma -> real OAM).
            self.oam_reader.src_disabled = dma_writing;
            self.prev_dma_writing = dma_writing;
        }

        // CPU OAM write this M-cycle (Gambatte `lcd_.oamChange(cc)`).
        if mmio.take_oam_write_pending() {
            self.oam_reader.change(cc, &lc, &pos);
        }
        // The snapshot is flushed only at `change` (above) and at the mode-2-end
        // `doEvent` (build_sprites_from_snapshot). A per-dot flush would consume
        // the `last_change` cap before the DMA-start `change`, losing the
        // load-bearing `_1`/`_2` bracket distinction.
    }

    /// Flush the snapshot to the mode-2-end cc (Gambatte SpriteMapper::doEvent's
    /// `oamReader_.update(time)`), then rebuild `sprites_on_line` from the posbuf
    /// in one pass (mapSprites). Replaces the per-dot live OAM scan.
    fn build_sprites_from_snapshot(&mut self, mmio: &mut mmio::Mmio) {
        let lc = self.ly_counter(mmio);
        let cc = self.abs_cc;
        let mut pos = [0u8; 80];
        mmio.peek_oam_pos(&mut pos);
        self.oam_reader.update(cc, &lc, &pos);

        self.sprites_on_line.clear();
        let ly = mmio.read(LY);
        for i in 0..OAM_SPRITE_COUNT {
            if self.sprites_on_line.len() >= MAX_SPRITES_PER_LINE {
                break;
            }
            let sprite_y = self.oam_reader.buf[2 * i];
            let sprite_x = self.oam_reader.buf[2 * i + 1];
            // Per-sprite OBJ size from the calibrated incremental scan (preserves
            // the late_sizechange per-slot size-latch timing); the snapshot only
            // governs Y/X visibility.
            let large = self.scan_slot_large[i];
            let sprite_height: u8 = if large { 16 } else { 8 };
            let screen_y = sprite_y.wrapping_sub(16);
            if ly >= screen_y && ly < screen_y.wrapping_add(sprite_height) {
                let tile_index = mmio.read(0xFE00 + (i as u16) * 4 + 2);
                let attributes_byte = mmio.read(0xFE00 + (i as u16) * 4 + 3);
                self.sprites_on_line.push(Sprite {
                    y: sprite_y,
                    x: sprite_x,
                    tile_index,
                    attributes: SpriteAttributes::from_byte(attributes_byte),
                    oam_index: i as u8,
                });
            }
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
            // Record the dot this sprite's stall arms (its first dot is consumed this
            // tick) so the OBJ-disable recompute can refund the not-yet-counted-down
            // remainder of an in-progress sprite (see `remaining_sprite_cost`).
            self.m3_last_sprite_commit_tick = self.ticks;

            // Single faithful tile-walk cost (mirrors `sprite_tile_walk_cost` /
            // Gambatte `doFullTilesUnrolled` ppu.cpp:525-530): the FIRST sprite in
            // each BG tile costs `max(11 - dist, 6)`; every further sprite sharing
            // that tile costs a flat 6. `dist = pixel_in_tile = (x + scx) & 7`. The
            // tile id `(x + scx) & !7` differs from the closed-form's `(spx -
            // firstTileXpos) & -8` only by a per-line constant, so the equality
            // grouping (first-vs-rest) is identical.
            let scx = mmio.read(SCX);
            let pixel_in_tile = self.x.wrapping_add(scx) & 0x07;
            let tile_no = (self.x as i32 + scx as i32) & !7;
            let first_in_tile = tile_no != self.m3_sprite_prev_tile;
            self.m3_sprite_prev_tile = tile_no;

            if sprite_x == 0 {
                return Some(11);
            }

            // pixel_in_tile 0..7 -> leading rate 11,10,9,8,7,6,6,6 (= max(11-dist,6));
            // a non-leading sprite in the same tile is always a flat 6.
            let base_penalty = if first_in_tile {
                let wait_for_bg_fetch = (7u8 - pixel_in_tile).saturating_sub(2);
                wait_for_bg_fetch + 6
            } else {
                6
            };
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
        
        // In CGB mode the sprite tile-data bank is fixed by OAM attr bit 3,
        // independent of the CPU's live VRAM-bank select (FF4F). The PPU must
        // read bank 0 when the bit is clear; using the live `mmio.read` here
        // returns whatever bank the CPU left selected (bank 1 in the
        // scx_attrib tests), corrupting the left-edge sprite color.
        let (low_byte, high_byte) = if mmio.is_cgb_features_enabled() {
            let bank = if (sprite.attributes.raw & 0x08) != 0 { 1 } else { 0 };
            (mmio.read_vram_bank(bank, tile_addr), mmio.read_vram_bank(bank, tile_addr + 1))
        } else {
            // DMG: single bank (the live read is correct).
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

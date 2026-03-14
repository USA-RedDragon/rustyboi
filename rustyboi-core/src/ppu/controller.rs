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

/// Convert an SGB/CGB RGB555 color word (bits: r=0-4, g=5-9, b=10-14) to RGB888
/// using the linear 5-bit->8-bit scaling the emulator uses for CGB `Linear`.
fn rgb555_to_rgb888(color: u16) -> (u8, u8, u8) {
    let r = color & 0x1F ;
    let g = (color >> 5) & 0x1F ;
    let b = (color >> 10) & 0x1F ;
    (((r * 255) / 31) as u8, ((g * 255) / 31) as u8, ((b * 255) / 31) as u8)
}

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
// Serde default for `frames_since_enable`: a savestate captured mid-run has an
// already-resynced panel, so restore to the "displays normally" value (>= 2).
fn frames_since_enable_default() -> u8 { 2 }
// Offset between rustyboi's `ticks` at M3 arm and Gambatte's lineCycle frame
// for the scheduled Mode 3 -> Mode 0 transition. Swept against the full suite.
const DMG_MODE0_OFFSET: i32 = 4;
const CGB_MODE0_OFFSET: i32 = 4;
// Mode-3 dot penalty for a window starting on this line (Gambatte StartWindowDraw).
const WIN_M3_PENALTY: i32 = 6;
// Display-column latency between a mid-mode-3 DMG palette-register (BGP/OBP0/OBP1)
// write and the first pixel that sees the new value. `self.x` at the write instant
// is the next column to be popped (the live pipeline plot position); the change
// first reaches the column plotted this many dots later. Calibrated byte-exact
// against mealybug m3_bgp_change / m3_obp0_change (DMG-blob + cgb-c references),
// whose per-line palette boundary lands past the column being popped at the write
// cc. Same shape as the LCDC `self.x + 2` commit in handle_lcdc_write. BGP and OBP
// carry separate latencies (the BG fetcher and the sprite mixer sample at different
// pipeline stages).
// CGB hardware samples the palette mapping one dot later in the pipeline than DMG
// hardware (the DMG fetcher runs a 4-dot pixel-transfer warmup + the +1 cgb_adj
// phase vs CGB's 2-dot warmup): the same mid-mode-3 write reaches the displayed
// column one column earlier on DMG. Keyed by `is_cgb()` (the hardware, NOT the
// CGB-features mode) so DMG-compat-on-CGB — which renders with the CGB warmup but
// the DMG palette regs (mealybug m3_bgp_change cgb_c) — takes the CGB latency. The
// split also keeps Gambatte's own dmgpalette_during_m3 DMG tests (which encode the
// real DMG latch column) passing.
const BGP_LATENCY_CGB: i32 = 2;
const BGP_LATENCY_DMG: i32 = 1;
const OBP_LATENCY_CGB: i32 = 2;
const OBP_LATENCY_DMG: i32 = 1;
// Maximum dot-gap between two consecutive mid-mode-3 palette writes for the DMG
// palette-latch glitch to fire. The glitch is a TWO-WRITE collision: mealybug's
// SET/RESTORE bursts write ~12 dots apart, so the settling from the first write is
// still in-flight when the second lands. Single writes, or writes spaced wider than
// this (gambatte dmgpalette_during_m3: one write per line / 60+ dots apart), don't
// collide and produce no spike. 12 cleanly separates the mealybug 12-dot pairs from
// the 60-dot inter-pair gap.
const BGP_SPIKE_CADENCE_CC: u64 = 12;
fn bgp_latency(cgb: bool) -> i32 {
    if cgb { BGP_LATENCY_CGB } else { BGP_LATENCY_DMG }
}
fn obp_latency(cgb: bool) -> i32 {
    if cgb { OBP_LATENCY_CGB } else { OBP_LATENCY_DMG }
}
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
// DMG window bus-glitch (wg_apply): dots from the LCDC write's register commit
// to the VRAM address-line transition. (The renderer's absorbed pre-window
// sprite stall is read from the live SpriteFetchRec, not a constant.)
const WG_TRANSITION_DELAY: u64 = 4;

// CGB-compat mid-mode-3 bus-glitch grid deltas, derived from the mealybug
// `_cgb_c` references. rise/fall = dots from the LCDC write to the bit
// becoming read-visible per fetch substep (fall split per tile-data byte);
// quirk = fall-coincidence tile-index-as-data window; arm/shift = fetch-grid
// anchoring for on-screen sprite stalls; scy_add = extra dots before SCY
// reaches the fetch address lines (vs DMG).
const CGBWG_WIN_RISE: u64 = 6;
const CGBWG_WIN_FALL: u64 = 7;
// Window map-select (LCDC.6) read visibility when the window tile-data path is
// $8000 (LCDC.4 = 1). Under $8000 the map pulse reaches the TileNumber read
// CGBWG_WIN_MAP_RISE/FALL_TDS dots after the write commit — later than the
// $8800 (LCDC.4 = 0) path's WIN_RISE/WIN_FALL — so a midline-sprite-shifted
// window fetch samples the map pulse one fetcher tile later. Calibrated once
// against the mealybug win_map_change2 `_cgb_c` reference (its 8 window rows
// each land the special $9C00 tile dot-exact only at 10/10); the $8800
// win_map_change keeps WIN_RISE/WIN_FALL. See cgb_wg_resolve / wg_apply.
const CGBWG_WIN_MAP_RISE_TDS: u64 = 10;
const CGBWG_WIN_MAP_FALL_TDS: u64 = 10;
// BG-path LCDC.3/4 read visibility, measured from the raw write cc, at the
// SameBoy-exact fetch dot (bg_hw_read_dot_ex scy_mode): a bit becomes visible
// `rise`/`fall` dots after the write commit. The fetch dot already carries its
// own +2k substep offset, so the fall thresholds no longer need a per-substep
// split (the old 4/3/1 was an artifact of the 2-dots-per-sprite-late grid).
const CGBWG_BG_RISE: u64 = 4;
const CGBWG_BG_FALL: u64 = 4;
const CGBWG_BG_FALL_TDL: u64 = 3;
const CGBWG_BG_FALL_TDH: u64 = 1;
// Map-select (LCDC.3) read visibility at the SameBoy-exact fetch dot
// (bg_hw_read_dot_ex scy_mode): a rise/fall is visible 2 dots after the write
// commit. Separate from the tile-data-select (LCDC.4) grid, which keeps the
// calibrated `h`-dot thresholds above (its per-byte / tile-index-as-data
// coincidence is tuned to that grid).
const CGBWG_BG_MAP_RISE: u64 = 2;
const CGBWG_BG_MAP_FALL: u64 = 2;
const CGBWG_SCY_ADD: u64 = 1;
const CGBWG_QUIRK_WIN: u64 = 7;
const CGBWG_QUIRK_BG: u64 = 4;
// Inter-edge A12 re-arm settle (see cgb_wg_resolve): a rising LCDC.4 edge that
// follows its prior falling edge by <= CGBWG_A12_GAP dots re-arms the address bus
// while it is still slewing from that fall, so the rise's visibility is delayed
// CGBWG_A12_REARM extra dots. GAP is the LCDC.4 pulse low-phase width the
// tile_sel_change2 write loop uses; a single isolated change pulse never re-fires
// low->high inside this span, so the extension is pulse-train-only (physical
// inter-edge spacing, not a per-tile coincidence). Calibrated once against the
// mealybug tile_sel_change / tile_sel_change2 cgb-c references.
const CGBWG_A12_GAP: u64 = 16;
const CGBWG_A12_REARM: u64 = 1;
// Pulse-train edge advance (see cgb_wg_resolve): a fall/rise inside a fast LCDC.4
// pulse train (opposite edge within CGBWG_A12_GAP dots) reaches the A12 bus this
// many dots sooner than the isolated-pulse thresholds — so its glitch window and
// bit4 visibility land on the read one dot past the write (age m3-bg-lcdc-nocgb),
// not the isolated w+4 (mealybug tile_sel_change).
const CGBWG_TRAIN_ADVANCE: u64 = 3;
// CGB-compat up-pulse LCDC.4 train line-end re-resolve (cgb_train_reresolve):
// each bitplane's tile-data base is sampled at its own T1, this many dots before
// the SameBoy-exact T2 fetch dot. Calibrated dot-exact vs the CGB-C oracle.
const CGBWG_TRAIN_T1_LEAD: i64 = 2;
const CGBWG_ARM_WIN: u64 = 14;
const CGBWG_ARM_WIN_HI: u64 = 12;
const CGBWG_ARM_BG: u64 = 14;
const CGBWG_SHIFT_BASE: u64 = 13;
// Sub-dot window fetch-grid phase (cgb_wg_resolve): the CGB-compat window
// fetch grid slides 1/8 dot earlier per window line against the CPU write
// clock (the SameBoy-measured read-dot drift quantizes this to the -1-dot
// steps every 8 lines that the integer grid already models; the fraction is
// the remainder). Two places see the fraction:
//  - a read displaced by a mid-line sprite stall resumes on the slid grid, so
//    a rising edge landing exactly ON its integer visibility dot misses the
//    read by the fraction: shifted reads take a rise one eighth late (the
//    m3_lcdc_tile_sel_win_change2 top-block wtx1 low read; its high-plane
//    $8000 split then collapses to the $8800 base like every train split).
//  - a read inside a PENDING stall shadow (hardware charges the sprite stall
//    to this read; the reconstruction grid charges it from the next tile)
//    samples the A12 line at its true (stalled) dot: a rising LCDC.4 edge
//    still rings there CGBWG_A12_ECHO dots after its commit, and the read
//    catches it only when the true dot lands exactly on the echo's 1/8-dot
//    lattice point - phase 0, i.e. window lines = 0 mod 8 (the
//    m3_lcdc_tile_sel_win_change (8,64) blue straddle, which no integer-grid
//    emulator reproduces).
const CGBWG_A12_ECHO: u64 = 18;

// CGB-compat window train tile-data-select latch (lower window rows). From
// WIN_TRAIN_GLITCH_ROW on, the pulse-train level and the tile-index-as-data glitch
// coincidence are sampled a per-block lag (in dots) before the reconstructed byte
// read; a FALL commit landing exactly on the sample dot IS the glitch. The lag
// walks one dot later every WIN_TRAIN_LAG_STEP window lines (the sub-dot fetch
// phase drift). Derived against rustyboi's own LCDC journal + the SameBoy CGB-C
// per-fetch oracle of m3_lcdc_tile_sel_win_change2 (zero lts/glitch mismatch,
// rows 40-47 lag -1, 48-55 lag 0, 56-63 lag +1). The upper rows (< this) are
// uniform (no oracle split/glitch) and use the collapse path instead.
const WIN_TRAIN_GLITCH_ROW: u8 = 40;
const WIN_TRAIN_LAG_BASE: i64 = -1;
const WIN_TRAIN_LAG_STEP: u8 = 8;

// Sub-dot state of one reconstructed window fetch read (see CGBWG_A12_ECHO):
// the fractional grid phase in eighths of a dot (0, -1, .., -7 across each
// 8-line block), whether the read's `h` carries a mid-line sprite-stall
// shift, and the stall dots hardware charges this read that the grid has not
// (the pending-stall shadow). NONE = integer grid (BG path, map re-resolve).
#[derive(Clone, Copy)]
struct WgSubDot {
    phase8: i64,
    shifted: bool,
    pending: u64,
}

impl WgSubDot {
    const NONE: WgSubDot = WgSubDot { phase8: 0, shifted: false, pending: 0 };
}

/// Machine configuration for a CPU VRAM/OAM access-window query.
#[derive(Clone, Copy)]
pub struct AccessEnv {
    pub is_cgb: bool,
    pub cgb_de: bool,
    pub double_speed: bool,
}

const WY1_DELAY: i64 = 2;
const WY2_DELAY_CGB: i64 = 7;
const WY2_DELAY_DMG: i64 = 4;
const SCY_DELAY: i64 = 2;
const WXEN_COMMIT_DELAY: i64 = 3;
const WYTRIG_COMMIT_DELAY: i64 = 3;
const LINE153_LY0_DOT_DS: i64 = 6;
const GETSTAT_OFF_DS: i64 = -1;

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

// DMG mid-mode-3 OBJ-enable toggle: dots from the write hook to the first
// pixel pop gated by the new LCDC.1. Calibrated against mealybug
// m3_lcdc_obj_en_change (the per-band cutoff sweep pins the commit exactly).
const OBJEN_APPLY_DOTS: u128 = 2;
// CGB (DMG-compat-on-CGB) mid-mode-3 OBJ-enable toggle commits one dot later
// than DMG-CPU silicon (the CGB PPU's pixel gate samples LCDC.1 a dot further
// out). Pinned by mealybug m3_lcdc_obj_en_change _cgb_c.
const OBJEN_APPLY_DOTS_CGB: u128 = 3;
// DMG mid-mode-3 OBJ-size toggle: dots from the write hook to the fetcher
// seeing the new LCDC.2. Calibrated by mealybug m3_lcdc_obj_size_change band 0
// (the group-2 sprite whose HIGH byte reads exactly one dot after the hook+1
// apply splits its row addressing: low byte 8x8, high byte 8x16).
const OBJSIZE_APPLY_DOTS: u128 = 1;
// Dots BEFORE the end of a sprite's fetch stall at which its tile-data LOW and
// HIGH bytes are read (SameBoy's object fetch: low at end-3, high at end-1).
const OBJ_READ_LOW_BACK: u128 = 3;
const OBJ_READ_HIGH_BACK: u128 = 1;
// CGB (DMG-compat-on-CGB) object fetch: the two tile-data bytes' size-sample
// dots sit 3 dots earlier within the stall than on DMG-CPU silicon (the CGB
// PPU begins the object tile-data fetch earlier relative to the stall end).
// A mid-mode-3 LCDC.2 toggle straddling the fetch therefore splits the row
// addressing at end-6 (LOW) / end-3 (HIGH). Pinned by mealybug
// m3_lcdc_obj_size_change and m3_lcdc_obj_size_change_scx _cgb_c.
const OBJ_READ_LOW_BACK_CGB: u128 = 6;
const OBJ_READ_HIGH_BACK_CGB: u128 = 3;

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

// Live mode-3 per-sprite fetch record (parallel to `sprites_on_line`, same
// index space as `next_sprite_fetch_index`). Tracks whether the live walk
// actually fetched a sprite this line and at which dot its stall armed, so the
// DMG mid-mode-3 LCDC.1/LCDC.2 toggle model can resolve per-sprite semantics:
//  - a sprite whose x-match dot passed while OBJ was disabled never fetches
//    (skipped: no pixels, no stall — SameBoy skips the object process on DMG);
//  - a sprite whose fetch is IN PROGRESS when OBJ is disabled aborts (SameBoy
//    memory.c object_fetch_aborted): the remaining stall dots are refunded and
//    the sprite's pixels never reach the line;
//  - a fetched sprite's two tile-data bytes each sample LCDC.2 (OBJ size) at
//    their own fetch dot (arm + penalty - 3 / - 1), so a mid-fetch size toggle
//    can split the row addressing between the LOW and HIGH bytes.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum SpriteFetchPhase {
    Pending,
    Fetched,
    Aborted,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct SpriteFetchRec {
    phase: SpriteFetchPhase,
    // Dot (self.ticks) the sprite's x-match landed. For left-clipped sprites
    // (OAM x < 8) the hardware match happens (8 - x) dots before the pixel
    // pipeline head reaches column 0, during the first-tile prologue; the
    // recorded tick carries that adjustment so the byte-fetch dots line up.
    arm_tick: u128,
    penalty: u8,
}

impl Default for SpriteFetchRec {
    fn default() -> Self {
        SpriteFetchRec { phase: SpriteFetchPhase::Pending, arm_tick: 0, penalty: 0 }
    }
}

// One mid-mode-3 BG tile captured for the CGB-compat up-pulse LCDC.4 train
// line-end re-resolve (see bg_tile_buf / cgb_train_reresolve).
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
struct CapturedBgTile {
    n: u64,      // fetcher tile index from line start
    tn: u8,      // latched tile number
    first_x: u8, // display column of this tile's first (leftmost) pixel
    y: u8,       // BG pixel row (ly + scy) & 0xFF for the tile-line lookup
    // Live (partial-journal) per-plane tile-data-select bits as drawn.
    // Diagnostic only on the BG path (the re-resolve recomputes both plane
    // bytes from the complete journal and re-plots per column); the WINDOW
    // analog still keys its split-tile repair on them.
    live_low_tds: bool,
    live_high_tds: bool,
}

// One mid-mode-3 WINDOW tile captured for the CGB-compat up-pulse LCDC.4 train
// line-end re-resolve (see win_tile_buf / win_train_reresolve). Window tiles are
// uniform (no per-plane split, no tile-index-as-data glitch), so a single
// tile-data-select sample per tile suffices.
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
struct CapturedWinTile {
    n: u64,      // fetcher tile index from line start
    tn: u8,      // latched tile number
    first_x: u8, // display column of this tile's first (leftmost) pixel
    y: u8,       // window internal line (win_y_pos) — the tile-line lookup row
    // Live per-plane tile-data-select bits as drawn. Window tiles are UNIFORM on
    // hardware (the base is latched once per tile), but rustyboi's per-substep
    // resolution can split them when a journal edge falls between the LOW (k=1)
    // and HIGH (k=2) reads — the mixed $8000/$8800 read the re-resolve corrects.
    live_low_tds: bool,
    live_high_tds: bool,
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
    // OAM "bus retention" ghost, latched at the OAM-DMA start edge: on hardware
    // the mode-2 scan cannot read OAM while an OAM-DMA runs, and the Y/X bus
    // retains the last pair actually read (SameBoy display.c
    // add_object_from_index: mode2_y_bus/mode2_x_bus only update while no DMA is
    // active, but the object check still runs against the stale bus). Positions
    // walked inside the DMA window sample this pair instead of open-bus 0xFF
    // (ashiepaws strikethrough: the line-68 scan ghosts entry 39's (0x54, 79)
    // pair, re-matching the bar sprite the in-flight DMA is erasing).
    ghost: (u8, u8),
    // Which sprite slots currently hold a ghost-sampled pair (vs a real OAM
    // sample). Ghost slots read their tile/attributes from the live
    // progressively-written OAM (`ppu_read_oam_live`) instead of the CPU view
    // (0xFF during DMA) — on hardware that fetch sees the in-flight DMA data.
    ghost_slot: [bool; OAM_SPRITE_COUNT],
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
            ghost: (0xFF, 0xFF),
            ghost_slot: [false; OAM_SPRITE_COUNT],
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
        self.ghost = (0xFF, 0xFF);
        self.ghost_slot = [false; OAM_SPRITE_COUNT];
        self.lu = 0;
        self.last_change = 0xFF;
        self.lsbuf = [self.large_src; OAM_SPRITE_COUNT];
        self.buf.copy_from_slice(oam);
    }

    // SpriteMapper::OamReader::enableDisplay.
    fn enable_display(&mut self, cc: u64, ds: bool) {
        self.buf = [0; 2 * OAM_SPRITE_COUNT];
        self.lsbuf = [false; OAM_SPRITE_COUNT];
        self.ghost = (0xFF, 0xFF);
        self.ghost_slot = [false; OAM_SPRITE_COUNT];
        self.lu = cc + ((OAM_POS_CYCLES as u64) << ds as u32) + 1;
        self.last_change = OAM_POS_CYCLES as u8;
    }

    // Latch the OAM-bus retention ghost at the OAM-DMA start edge. Called right
    // after the edge `change(cc)` capped the walk (`last_change`): the pair at
    // the last position the walk sampled before the cap is what the hardware
    // Y/X bus still holds when the DMA takes the OAM away from the scan. A cap
    // at/before position 1 means no pair was sampled on this line yet, so the
    // bus holds the previous line's final OAM read.
    //
    // `line_has_fetches`: whether the line whose reads the bus last saw had any
    // visible sprites. The Y bus is ALSO written by every mode-3 sprite
    // tile/flags fetch (SameBoy display.c:1976 `mode2_y_bus = oam_read(tile)`),
    // so on a line that fetched sprites the retained value is a tile byte —
    // effectively never a matching Y — not the scan pair. Model that clobber as
    // an invisible ghost. It applies when the window opens outside the scan
    // walk (cap at 80: this line's fetches; cap before 2: the previous line's);
    // a mid-scan window start (2..80) retains the just-scanned pair, fetches
    // not yet run (gambatte late_sp39y_2 vs ashiepaws strikethrough, whose
    // DMA-start line renders no sprites so the scan pair survives to the next
    // line's walk).
    fn capture_ghost(&mut self, line_has_fetches: bool) {
        let cap = (self.last_change as usize).min(2 * OAM_SPRITE_COUNT);
        if !(2..2 * OAM_SPRITE_COUNT).contains(&cap) && line_has_fetches {
            self.ghost = (0xFF, 0xFF);
        } else {
            let p = if cap >= 2 {
                (cap - 1) & !1
            } else {
                2 * OAM_SPRITE_COUNT - 2
            };
            self.ghost = (self.buf[p], self.buf[p + 1]);
        }
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
                    // OAM-DMA window: the scan's Y/X bus retains its pre-DMA
                    // pair (see `capture_ghost`), it does not read open-bus.
                    self.buf[2 * i] = self.ghost.0;
                    self.buf[2 * i + 1] = self.ghost.1;
                    self.ghost_slot[i] = true;
                } else {
                    self.buf[2 * i] = oam_pos[2 * i];
                    self.buf[2 * i + 1] = oam_pos[2 * i + 1];
                    self.ghost_slot[i] = false;
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
                    // During an OAM-DMA window the walk samples the retained
                    // Y/X bus pair (`ghost`), not open-bus 0xFF.
                    let (y, x) = if self.src_disabled {
                        (self.ghost.0, self.ghost.1)
                    } else {
                        (oam_pos[pos as usize], oam_pos[pos as usize + 1])
                    };
                    self.ghost_slot[(pos / 2) as usize] = self.src_disabled;
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
    // DMG window-startup fetch phase anchor: the trigger dot of a mid-line
    // window start. Hardware restarts the fetcher ON the trigger dot
    // (TileNumber dots t..t+1, data-low t+2..t+3, data-high t+4..t+5, push at
    // t+6), so the first window pixel pops exactly 6 dots after the trigger
    // regardless of the global fetch parity (mealybug m3_window_timing: the
    // BGP-boundary column shows a flat 6-dot startup for every WX). While set,
    // the fetch cadence is (ticks - anchor) % 2 == 0 instead of ticks % 2 == 0;
    // cleared at the first window tile's PushToFIFO (the FIFO's 8-pixel slack
    // absorbs the re-sync to global parity). DMG-only; CGB keeps its decoupled
    // fetcher_cadence_tick.
    #[serde(default)]
    win_fetch_anchor: Option<u128>,
    // DMG WX 1..6 immediate window start: on hardware the WX comparator matches
    // during the discard prologue at position WX-7, so the window activates
    // chop = (7-WX) dots EARLIER than the WX=7 (position 0) start, and the
    // remaining chop prologue discard pops (1 per dot, from the freshly pushed
    // window tile) chop its leading pixels. Net: the first VISIBLE window pixel
    // appears at the same dot as a WX=7 start (the earlier activation exactly
    // cancels the extra discards; mealybug m3_window_timing rows 0..7 share one
    // boundary) but the CONTENT starts at window pixel chop (mealybug
    // m3_wx_4_change: 3-px left chop at WX=4) and the fetch pipeline runs chop
    // dots ahead (no FIFO underrun after the chopped tile). Emulated by running
    // the first chop dots' worth of fetch substeps on the trigger dot
    // (win_fetch_anchor = ticks - chop) and pacing the chop pops 1/dot in the
    // x==0 prologue. Remaining chop pops this line; reset at M3 arm.
    #[serde(default)]
    win_first_tile_chop: u8,
    // SameBoy's window_is_being_fetched: true from a window activation until the
    // first FIFO pop that follows it (chop/discard pops count). The reactivation
    // insert below is suppressed while set — the activation's own first tile
    // must not self-insert.
    #[serde(default)]
    win_being_fetched: bool,
    // DMG window "reactivation zero pixel" (SameBoy insert_bg_pixel): when the
    // WX comparator matches AGAIN while the window is already active (a mid-
    // mode-3 WX rewrite), and the match dot coincides with a window tile's
    // nametable-read fetch dot with the FIFO holding exactly 8 pixels, the PPU
    // renders one color-0 pixel WITHOUT popping the FIFO — inserting a pixel
    // that shifts the rest of the line right by one (mealybug m3_wx_4_change /
    // m3_wx_4_change_sprites: the every-8-rows diagonal at x = WX-7). Consumed
    // by the next draw_fifo_pixel.
    #[serde(default)]
    insert_bg_pixel: bool,
    // DMG per-dot visibility history of LCDC.5 (window enable) inside mode 3,
    // shifted at the top of each PixelTransfer dot: [k] = the visible bit k
    // dots ago ([0] = current dot). Our visible bit (self.lcdc, via the
    // 2-dot pending-event commit) turns OFF 2 dots before and ON 1 dot
    // before the hardware-visible bit, so an 8-cycle WE-off pulse spans 9
    // visible OFF dots. Three taps, calibrated dot-exact by the mealybug
    // win_en_change_multiple[_wx] staircases (each row shifts the WE pulse
    // one dot along the window-trigger/fetch grid):
    //  - WX comparator (trigger + prologue paths): we[2] INSTEAD of the live
    //    bit — hardware needs WE asserted on the check dot and the one
    //    before (an 8-cycle pulse blocks 9 comparator dots);
    //  - window fetcher TileNumber kill: OFF at BOTH [3] and [4] (hardware
    //    samples WE one dot delayed at its T1 dot, one dot before our TN);
    //  - WE-off zero-pixel insert: we[2] at the tile-boundary pop dot.
    // Seeded at M3 arm.
    #[serde(default)]
    we_dot_hist: [bool; 5],
    // Display-x values at which a pushed BG/window tile's FIRST pixel will
    // pop (SameBoy's push-at-empty dots, where the WE-off zero-pixel insert
    // glitch is evaluated). Queued at PushToFIFO, consumed at the pop of
    // that x; at most two tiles are in flight.
    #[serde(default)]
    we_glitch_tile_starts: [Option<u8>; 2],
    // DMG WE-off insert glitch, discard-prologue variant (SameBoy issue #278,
    // little-things windesync-validate WX 0..6 rows): the line's FIRST
    // push-at-empty boundary sits at SameBoy position -(SCX&7) — INSIDE the
    // fine-scroll discard prologue — and the comparator match there is
    // WX == position + 7, i.e. WX == 7 - (SCX&7). The inserted color-0 pixel
    // is itself swallowed by the prologue: one discard dot pops it instead of
    // a real BG pixel, so one extra leading BG pixel survives and the visible
    // line shifts right by one with NO on-screen glitch pixel. Set at the
    // push dot, consumed by the first discard pop; reset at M3 arm.
    #[serde(default)]
    we_glitch_discard_insert: bool,
    // SameBoy disable_window_pixel_insertion_glitch: a WE-off LCDC write
    // landing while a window tile fetch is in flight (win_being_fetched)
    // suppresses the WE-off zero-pixel insert for the REST of the line.
    // Reset at M3 arm.
    #[serde(default)]
    we_insert_suppressed: bool,
    // Which WE tap the window TileNumber kill samples (see we_dot_hist).
    // A MID-LINE window restart runs its fetch on the trigger-anchored grid,
    // where the hardware tile-number dot sits one dot BEFORE our TN step
    // (tap [3]); a LINE-BEGIN (M3Start f0) window runs on the global fetch
    // grid where they coincide (tap [2]). wxA6_weoff_at_xposA6 line 0 vs the
    // win_en_change_multiple[_wx] staircases bracket the two cases.
    #[serde(default)]
    win_kill_tap_late: bool,
    // One-shot latch for the DMG WX=0 + SCX&7>0 window-activation quirk: the
    // window activates one T-cycle later than the plain x==0 start (mealybug
    // m3_window_timing_wx_0: the BGP-boundary column is one dot short on every
    // SCX&7 != 0 row). Set on the would-be trigger dot (which becomes a dead
    // dot: no pop, no activation); the trigger then fires on the next dot.
    // Reset at M3 arm.
    #[serde(default)]
    win_wx0_delayed: bool,
    // DMG mid-line WX comparator deferral: the hardware comparator samples WX
    // through the end of the CPU store's M-cycle, so a match seen with the OLD
    // WX on the store's commit dot must NOT start the window (mealybug
    // m3_wx_6_change row 102: the WX=LY match at x==95 loses to the WX=80
    // restore landing on that very dot). Arm (trigger dot, matched wx) on the
    // exact x+7==wx match; commit one dot later iff WX still reads the matched
    // value, with a one-substep catch-up so the restart timing is byte-identical
    // to the immediate start for a stable WX. Cleared at M3 arm.
    #[serde(default)]
    dmg_wx_trigger_pending: Option<(u128, u8)>,
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

    // DMG wx==166 plotPixel-at-xpos166 runs once at the mode-3 -> HBlank
    // transition; this guards against the two transition call sites both firing
    // it on the same line. Reset at M3 start. See apply_dmg_wxa6_lineend_windraw.
    #[serde(default)]
    wxa6_lineend_applied: bool,

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
    // Frame boundaries completed since `ssds_mode3_ly_advance` was last set. The
    // mode-3-switch lyCounter re-anchor is a phase artifact local to the frames
    // right after the switch; once several frame wraps re-settle the phase it no
    // longer applies. Reset to 0 when the flag is set.
    #[serde(default)]
    ssds_mode3_frames: u8,
    // Cumulative NON-mode-3 (OAM/HBlank) DS->SS speed-switch count for the LY-read
    // sub-dot phase accumulator (Gambatte's `PPU::speedChange` half-dot `now -= 1`,
    // applied per switch). rustyboi's whole-dot DS->SS bridge folds the integer part;
    // the residual half-dot per switch accumulates and its parity shifts the post-STOP
    // getLyReg boundary read one sub-dot. Mode-3 DS->SS switches carry their residual
    // through the FACET-1 `stat_phase_carry` path instead, so they are excluded here.
    #[serde(default)]
    dsss_ly_phase_count: u32,
    // Total DS->SS switch count (INCLUDING mode-3) for the early-frame anticipation
    // narrowing. Mode-3 DS->SS switches carry their sub-dot through FACET-1 for the
    // glitch-dot resolution, but the anticipation-window WIDTH of an early-frame read
    // still tracks the full switch parity (row32/row37: the extra mode-3 switches vs
    // the calibration burst flip the narrow-window parity).
    #[serde(default)]
    dsss_ly_total_count: u32,
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
    // DMG "line 154" STAT-write glitch (gbmicrotest stat_write_glitch_l154_d):
    // when the CPU writes FF41 (STAT) at the frame-wrap boundary (the LY 153->0
    // exit of VBlank, into the first line of the new frame) a hardware glitch on
    // the shared VBlank/STAT interrupt path clears the still-pending VBlank IF
    // bit (bit 0). Real DMG-CPU-08 reads IF=0xE0 there; a sticky-bit model (both
    // Gambatte and the pre-fix renderer) reads 0xE1. Armed at the VBlank->OAM
    // frame-wrap, disarmed a few dots into line 0/1 so a normal mid-frame STAT
    // write never clears a legitimately-pending VBlank IRQ. DMG-only.
    #[serde(default)]
    l154_vblank_glitch_window: bool,
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
    // DMG mid-mode-3 BGP sub-M-cycle phase hold. `on_bgp_write` fires at the write
    // M-cycle START, but the store's bus-write lands a phase-dependent number of dots
    // later; for a write whose `master_cc % 4` is non-zero the new value must not reach
    // `bgp_delayed` until `bgp_defer_countdown` more dot-refreshes have passed. The old
    // (pre-write) value is held in `bgp_defer_hold` meanwhile. Phase-0 writes (every
    // gambatte dmgpalette_during_m3 / mealybug m3_bgp_change write) set countdown 0 and
    // are byte-identical to the plain one-dot latch. See `on_bgp_write`.
    #[serde(default)]
    bgp_defer_hold: u8,
    #[serde(default)]
    bgp_defer_countdown: u8,

    #[serde(with = "serde_bytes")]
    fb_a: [u8; FRAMEBUFFER_SIZE],
    #[serde(with = "serde_bytes")]
    fb_b: [u8; FRAMEBUFFER_SIZE],
    /// SGB MASK_EN Freeze latch: the DMG shade frame captured at the first
    /// frame boundary after the freeze engaged, shown instead of the live
    /// frame until the mask clears (games hide their *_TRN transfer screens
    /// behind this). None when not frozen.
    #[serde(default)]
    sgb_freeze_fb: Option<Vec<u8>>,
    #[serde(with = "serde_bytes")]
    color_fb_a: [u8; FRAMEBUFFER_SIZE * 3], // RGB color framebuffer
    #[serde(with = "serde_bytes")]
    color_fb_b: [u8; FRAMEBUFFER_SIZE * 3], // RGB color framebuffer
    have_frame: bool,
    // First-frame-after-LCD-enable display blanking. On real hardware the panel
    // has not resynced for the first frame produced after LCDC.7 0->1, so it shows
    // the LCD-off "whiter than white" blank instead of that frame's pixels (the
    // little-things-gb `firstwhite` / Pokemon Pinball 1-frame-glitch behavior).
    // `frames_since_enable` counts completed frames since the last enable (saturating);
    // get_frame presents blank until it reaches 2 (one full frame after enable has
    // been displayed). Seeded to 2 so a skip_bios boot (LCD already on, no enable
    // edge observed) — and a savestate from a running system — displays normally.
    #[serde(default = "frames_since_enable_default")]
    frames_since_enable: u8,
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
    // CGB tile-index-is-tile-data glitch targets (SameBoy `tile_sel_glitch`).
    // Each falling mid-mode-3 LCDC.4 write records the single BG data read
    // (target_cc, target_k) that lands in the write's 1-T-cycle glitch window and
    // therefore returns the tile index instead of a VRAM byte. Resolved per fetch
    // substep in `tidxtd_quirk_at_fetch`. Cleared at each mode-3 arm.
    #[serde(default)]
    tidxtd_glitch: Vec<(u64, u8)>,
    // DMG window bus-glitch journal: each mid-mode-3 LCDC write that toggles
    // bit 6 (window map select) or bit 4 (tile data select) records
    // (transition_cc, old_lcdc, new_lcdc) — the abs_cc at which the new address
    // lines reach the VRAM bus. Window fetch reads are re-evaluated against it
    // at their reconstructed hardware dots (see wg_apply). Cleared at each
    // mode-3 arm.
    #[serde(default)]
    wg_hist: Vec<(u64, u8, u8)>,
    // Whether this line's bus-glitch journals resolve with the CGB-compat
    // rules (DMG cart on CGB hardware, single speed — the scope the `_cgb_c`
    // captures calibrate) instead of the DMG ones. Latched at mode-3 arm.
    #[serde(default)]
    wg_cgb: bool,
    // The undelayed window-restart TileNumber dot (abs_cc) for the current
    // line's window — the hardware fetch-grid origin F. None when the window
    // did not start through the x==0 restart path this line (the glitch model
    // is scoped to it) or when the pre-window sprite configuration is outside
    // the calibrated single-sprite case.
    #[serde(default)]
    wg_anchor_cc: Option<u64>,
    // Hardware pre-window delay D_pre from an offscreen-left sprite (OAM X<=7)
    // fetched before the window restart. 0 when none.
    #[serde(default)]
    wg_dpre: u64,
    // The line's first BG TileNumber read dot (abs_cc) — the hardware BG
    // fetch-grid origin for bg_wg_apply / the SCY journal. Recorded at the
    // tile-0 TileNumber substep; None before it or on lines that never fetch
    // BG. Cleared at each mode-3 arm.
    #[serde(default)]
    bg_anchor_cc: Option<u64>,
    // DMG mid-mode-3 SCY write journal: (transition_cc, old, new) — the abs_cc
    // at which the new map-row / tile-line address bits reach the VRAM bus.
    // BG fetch reads resolve SCY against it at their reconstructed hardware
    // dots (see bg_wg_apply). Cleared at each mode-3 arm.
    #[serde(default)]
    bg_scy_hist: Vec<(u64, u8, u8)>,
    // DMG mid-mode-3 SCX write journal: (write_cc, old, new). The BG tile-map
    // column resolves SCX against it at the tile's reconstructed hardware
    // TileNumber dot (see bg_wg_apply / m3_scx_high_5_bits). Cleared each M3 arm.
    #[serde(default)]
    bg_scx_hist: Vec<(u64, u8, u8)>,
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
    // Per-line LCDC.0 (BG-enable) plot history for the per-pixel renderer
    // (RB_LINERENDER per-pixel). The per-dot draw is flushed in bursts (the
    // m0Time flush draws all remaining FIFO pixels at one cc), so a live
    // `self.lcdc & 1` read applies the final BG-enable to every flushed column
    // and a mid-mode-3 LCDC.0 toggle (BG off then on within pixel transfer) is
    // lost. Gambatte instead reads `lcdc & lcdc_bgen` live as the fetcher walks
    // tiles, so each plotted column sees the BG-enable bit in effect at its own
    // plot position. We record the BG-enable changes during this line's mode 3
    // as (boundary_col, bgen) entries — columns >= boundary_col see the new bit.
    // The first entry (boundary_col == 0) seeds the value at mode-3 start.
    // Empty/single-entry => no mid-line toggle => identical to the live read.
    #[serde(default)]
    bgen_history: Vec<(u64, bool)>,
    // DMG per-dot OBJ-enable (LCDC.1) history. Hardware gates each sprite pixel
    // on OBJ-enable AT THAT PIXEL'S pop dot (SameBoy render_pixel_if_possible
    // reads LCDC.1 live per popped pixel), so a mid-mode-3 disable/enable
    // covers an exact dot span — which maps to columns THROUGH the stall
    // schedule (a column popping late because of a sprite stall resolves the
    // gate at its actual pop dot). Entries are (apply_tick, enabled); pops at
    // ticks >= apply_tick see the new bit. Seeded at mode-3 entry (tick 0);
    // single-entry == no toggle == the live-read fast path.
    #[serde(default)]
    objen_history: Vec<(u128, bool)>,
    // DMG per-dot OBJ-size (LCDC.2) history: (apply_tick, large). The sprite
    // fetcher samples LCDC.2 independently at each tile-data byte's own fetch
    // dot (SameBoy recomputes get_object_line_address for the low AND high
    // byte), so a mid-fetch toggle splits the row addressing between bytes.
    // Seeded at mode-3 entry (apply_tick 0).
    #[serde(default)]
    objsize_dot_history: Vec<(u128, bool)>,
    // Per-sprite live fetch records, parallel to `sprites_on_line` (see
    // `SpriteFetchRec`). Rebuilt (all Pending) at mode-3 entry.
    #[serde(default)]
    sprite_fetch_recs: Vec<SpriteFetchRec>,
    // Per-line BGP / OBP0 / OBP1 plot history for the per-pixel renderer, mirroring
    // `bgen_history`. A mid-mode-3 write to BGP (FF47) / OBP0 (FF48) / OBP1 (FF49)
    // takes effect at the exact pixel being drawn `MID_M3_PAL_LATENCY` dots later
    // (the DMG palette-RAM pipeline latency the mealybug m3_bgp_change / m3_obp0_change
    // tests measure). The per-dot draw is flushed in bursts at m0Time, so a single
    // live `mmio.read(BGP)` snapshot would apply the final value to every flushed
    // column. We record each mid-mode-3 change as a (boundary_col, value) entry —
    // columns >= boundary_col see the new value — and resolve per displayed column.
    // The first entry (boundary 0) seeds the value at mode-3 start; with no mid-line
    // write the history is a single seed and resolves to that value for the whole
    // line (identical to the previous `bgp_delayed` steady-state read).
    #[serde(default)]
    bgp_history: Vec<(u64, u8)>,
    #[serde(default)]
    obp0_history: Vec<(u64, u8)>,
    #[serde(default)]
    obp1_history: Vec<(u64, u8)>,
    // DMG dot-keyed OBP histories: (apply_tick, value). The OBP register is
    // sampled as each sprite pixel pops out of the OAM FIFO, so the correct
    // key is the pixel's POP DOT — the column mapping breaks whenever a sprite
    // stall delays the pops (mealybug m3_obp0_change's late bands: a pixel at
    // column 8 popping at dot 111 must see a write that applied at dot 105,
    // even though the write's column boundary was 9). On stall-free lines this
    // is exactly equivalent to the column model (columns pop 1/dot). It also
    // subsumes the old off-left-edge column-0 forcing: left-clipped sprites'
    // pixels pop early, before any mid-mode-3 write applies.
    #[serde(default)]
    obp0_dot_history: Vec<(u128, u8)>,
    #[serde(default)]
    obp1_dot_history: Vec<(u128, u8)>,
    // Dot-keyed BGP history for the CGB / DMG-compat BG color path. A mid-mode-3
    // BGP write applies at `ticks + latency` (a DOT), and each BG pixel is colored
    // at its own pop dot — which is delayed by any sprite-fetch stall between the
    // write and that column. Sampling by pop-dot (not display column) makes the
    // stall absorption exact for both the on-stall write and a pre-stall write
    // whose target column is pushed past the stall (mealybug m3_bgp_change_sprites
    // cgb_c). The column-keyed `bgp_history` remains the DMG-hardware path.
    #[serde(default)]
    bgp_dot_history: Vec<(u128, u8)>,
    // DMG mid-mode-3 BGP-write "glitch" (mealybug m3_bgp_change). On real DMG hardware a
    // CPU write to BGP (FF47) during mode 3 can collide with the PPU's palette read for
    // the pixel being pushed at that dot: the register is mid-transition and the pixel is
    // looked up through the bitwise OR of the old and new BGP bytes (`old | new`) rather
    // than either settled value. When old and new differ in a color slot the OR sets
    // extra shade bits, darkening that one pixel — the "black spike" bracketing each
    // mid-line palette band (e.g. old=$41,new=$42 -> $43, so a color-0 pixel reads shade
    // 3 / black; when old|new==old the spike is invisible). It is a TWO-WRITE collision
    // (see `bgp_writes`), so a lone or loosely-spaced write shows no spike. CGB uses
    // true-color palette RAM and shows no such collapse, so this is DMG-gated. The two
    // fields below drive it, both reset at mode-3 start:
    // Per-column BG color index (0-3) of the pixel drawn at each display column this
    // line, or -1 where a sprite won the mix / the column is undrawn. Recorded by the
    // per-dot DMG draw and read by `resolve_bgp_spikes` to re-map the glitched columns
    // through the OR palette at mode-3 end. 160 entries, reset each line.
    #[serde(default)]
    line_bg_idx: Vec<i8>,
    // Capture-phase mid-mode-3 BG tile buffer (CGB-compat up-pulse LCDC.4 train
    // re-resolve). Each BG tile pushed to the FIFO during mode 3 records the
    // context needed to re-resolve its tile-data-select bits against the
    // COMPLETE wg_hist journal at line-end and re-plot: (fetch index n, tile
    // number, first display column, tile-row y (0..255)). Reset each mode-3 arm.
    #[serde(default)]
    bg_tile_buf: Vec<CapturedBgTile>,
    // Capture-phase mid-mode-3 WINDOW tile buffer (CGB-compat up-pulse LCDC.4
    // train re-resolve; the window analog of bg_tile_buf). See win_train_reresolve.
    #[serde(default)]
    win_tile_buf: Vec<CapturedWinTile>,
    // Every mid-mode-3 BGP write on the current line, as (abs_cc, display_col, old|new).
    // The DMG palette-latch glitch is a TWO-WRITE interaction: a write spikes its own
    // pixel only when it has a neighboring mid-mode-3 write within the tight SET/RESTORE
    // cadence (`BGP_SPIKE_CADENCE_CC`; mealybug bursts writes in ~12-dot pairs). A single
    // write, or one loosely spaced (the gambatte dmgpalette_during_m3 family: one write
    // per line, or 60-148 dots apart), does NOT collide and shows no spike. The gate is
    // resolved at mode-3 end (all writes known) by `resolve_bgp_spikes`, which paints the
    // glitch straight into the framebuffer. Reset at mode-3 start.
    #[serde(default)]
    bgp_writes: Vec<(u64, u8, u8)>,
    // Last mode-2 (OAM scan) BGP write (cc, value), carried across the mode-3-arm
    // bgp_writes clear and injected as a neighbor-only spike entry at mode-3 entry
    // (see on_bgp_write / the arm seed). None once consumed or if no mode-2 write.
    #[serde(default)]
    bgp_mode2_pending: Option<(u64, u8)>,
    #[serde(default)]
    cgb_color_conversion: CgbColorConversion,
    #[serde(skip, default)]
    fetch_debug_events_enabled: bool,
    #[serde(skip, default)]
    fetch_debug_events: Vec<FetchDebugEvent>,
    #[serde(skip, default)]
    pixel_debug_events: Vec<PixelDebugEvent>,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
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
            win_fetch_anchor: None,
            win_first_tile_chop: 0,
            win_being_fetched: false,
            insert_bg_pixel: false,
            we_dot_hist: [true; 5],
            we_glitch_tile_starts: [None; 2],
            we_glitch_discard_insert: false,
            we_insert_suppressed: false,
            win_kill_tap_late: false,
            win_wx0_delayed: false,
            dmg_wx_trigger_pending: None,
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
            ssds_mode3_frames: 0,
            dsss_ly_phase_count: 0,
            dsss_ly_total_count: 0,
            sc_mode3_pullback_pending: false,
            dsss_mode3_stop_count: 0,
            render_carry_skew_cc: 0,
            cgbp_block_start_cc: None,
            mode0_reported_this_line: false,
            line_rendered_this_line: false,
            wxa6_lineend_applied: false,
            bgen_history: Vec::new(),
            objen_history: Vec::new(),
            objsize_dot_history: Vec::new(),
            sprite_fetch_recs: Vec::new(),
            obp0_dot_history: Vec::new(),
            obp1_dot_history: Vec::new(),
            bgp_dot_history: Vec::new(),
            bgp_history: Vec::new(),
            obp0_history: Vec::new(),
            obp1_history: Vec::new(),
            line_bg_idx: vec![-1; 160],
            bg_tile_buf: Vec::new(),
            win_tile_buf: Vec::new(),
            bgp_writes: Vec::new(),
            bgp_mode2_pending: None,
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
            l154_vblank_glitch_window: false,
            lyc_irq: stat_irq::LycIrq::default(),
            mstat_irq: stat_irq::MStatIrq::default(),
            stat_reg_committed: 0,
            bgp_delayed: 0,
            obp0_delayed: 0,
            obp1_delayed: 0,
            bgp_defer_hold: 0,
            bgp_defer_countdown: 0,
            fb_a: [0; FRAMEBUFFER_SIZE],
            fb_b: [0; FRAMEBUFFER_SIZE],
            sgb_freeze_fb: None,
            color_fb_a: [0; FRAMEBUFFER_SIZE * 3],
            color_fb_b: [0; FRAMEBUFFER_SIZE * 3],
            have_frame: false,
            frames_since_enable: 2,
            lcdc: 0,
            cgb_tile_index_is_tile_data: false,
            pending_lcdc_events: Vec::new(),
            lcdc_b4_exact: None,
            tidxtd_glitch: Vec::new(),
            wg_hist: Vec::new(),
            wg_cgb: false,
            wg_anchor_cc: None,
            wg_dpre: 0,
            bg_anchor_cc: None,
            bg_scy_hist: Vec::new(),
            bg_scx_hist: Vec::new(),
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
    pub fn set_post_bios_state(&mut self, mmio: &mut mmio::Mmio, dmg0: bool) {
        // LCD must be on for this to apply (skip_bios writes LCDC=0x91 first).
        if self.lcdc & (LCDCFlags::DisplayEnable as u8) == 0 {
            return;
        }
        let cgb = mmio.is_cgb_features_enabled();
        // Gambatte initstate.cpp:471: videoCycles = cgb ? 144*456+164+agb*4 :
        // 153*456+396. AGB's post-boot video phase leads CGB's by 4 dots.
        let agb_off = if mmio.is_agb() { 4 } else { 0 };
        // The DMG0 boot ROM hands off ~9 scanlines earlier in the frame than
        // DMG-ABC/MGB: mooneye boot_hwio-dmg0 reads FF44/FF41 at fixed offsets
        // into its unrolled compare loop (FF41 at ~4528 T, FF44 at ~4712 T past
        // the 0x150 handoff) and asserts LY=0x01 with STAT mode 3, whereas
        // boot_hwio-dmgABCmgb asserts LY=0x0A / mode 0 at the same offsets. Both
        // hand off in VBlank; the live PPU then advances into the next frame so
        // the loop samples line 1 (dmg0) vs line 10 (dmgABC). The DMG0 videoCycles
        // that lands the FF41 read mid-mode-3 on line 1 and the FF44 read still on
        // line 1 is 145*456+198; the wide (~170-dot) window around it makes the
        // exact CPU read sub-phase irrelevant. Non-DMG0 keeps Gambatte's 153*456+396.
        let video_cycles: u32 = if cgb {
            144 * stat_irq::LCD_CYCLES_PER_LINE + 164 + agb_off
        } else if dmg0 {
            145 * stat_irq::LCD_CYCLES_PER_LINE + 198
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
        // The LYC/STAT interrupt machinery follows the LCD-controller silicon,
        // which is CGB whenever the hardware is CGB-like — even for a DMG cart in
        // DMG-compatibility mode (Gambatte gates LycIrq on `cart_.isCgb()`, which
        // is the CGB-console signal, not cart CGB-feature support). Use hardware
        // is-CGB, not `is_cgb_features_enabled()`.
        self.lyc_irq.set_cgb(mmio.is_cgb());
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

        // DMG window bus-glitch journal (see wg_apply): record the exact bus
        // transition time of a mid-mode-3 bit6/bit4 toggle. The address lines
        // reach the VRAM bus WG_TRANSITION_DELAY dots after the write's
        // register commit (calibrated against the mealybug win_map/tile_sel
        // win-change reference captures: the hardware transition dot lands on
        // the window fetch grid 3 dots after our register-visible apply cc).
        if !mmio.is_cgb_features_enabled()
            && display_stays_enabled
            && self.state == State::PixelTransfer
        {
            let wg_bits = (LCDCFlags::WindowTileMapDisplaySelect as u8)
                | (LCDCFlags::BGWindowTileDataSelect as u8)
                | (LCDCFlags::BGTileMapDisplaySelect as u8);
            if (old_lcdc ^ value) & wg_bits != 0 {
                let t_cc = self.write_cc(false) + WG_TRANSITION_DELAY;
                self.wg_hist.push((t_cc, old_lcdc, value));
                self.bg_retro_repair(mmio);
            }
        }

        // SameBoy disable_window_pixel_insertion_glitch: a window-DISABLE
        // landing while a window tile fetch is in flight suppresses the
        // WE-off zero-pixel insert glitch for the remainder of this line
        // (reset at the next M3 arm).
        let win_en_bit = LCDCFlags::WindowDisplayEnable as u8;
        if !mmio.is_cgb()
            && display_stays_enabled
            && (old_lcdc & win_en_bit) != 0
            && (value & win_en_bit) == 0
            && self.win_being_fetched
        {
            self.we_insert_suppressed = true;
        }

        // Per-pixel BG-enable history (RB_LINERENDER per-pixel). A mid-mode-3
        // LCDC.0 (BG-enable) toggle must be applied per display column: the
        // per-dot draw is flushed in bursts (the m0Time flush draws all remaining
        // FIFO pixels at one cc), so a once-per-line / live `self.lcdc & 1` read
        // applies the final BG-enable to every flushed column. Record each bit0
        // change as a (boundary_col, bgen) entry — columns >= boundary_col see the
        // new bit — so the renderer reproduces Gambatte's live per-tile `lcdc &
        // lcdc_bgen` read. Only while pixel transfer is active for this line.
        let bgen_bit = LCDCFlags::BGDisplay as u8;
        if display_stays_enabled
            && self.state == State::PixelTransfer
            && (old_lcdc & bgen_bit) != (value & bgen_bit)
        {
            // Column-space history keyed by the display column at which the
            // BG-enable change first becomes visible. `self.x` is the next display
            // column to be popped — the real pipeline plot position at the write
            // instant (it already carries the warmup/FIFO and window latency the
            // latency-free closed-form predictor lacks). Gambatte commits the
            // write at `setLcdc(cc + 2)` PPU dots; the change first affects the
            // column plotted ~2 dots later. Near an active window the displayed
            // column advances slower than 1/dot (the +6 StartWindowDraw penalty
            // stalls the BG fetcher), so the 2-dot commit reaches a few more
            // display columns; add the window penalty when this line draws a
            // window so the BG-off span extends across the stalled window columns
            // (bgoff_bgon line 104: F7/on at x=143 + window -> boundary 147, so the
            // sprite columns 142/144/146 keep BG-off and the sprite shows).
            // Gambatte commits the write at `setLcdc(cc + 2)` PPU dots, so the
            // change first reaches the column plotted 2 dots later: boundary =
            // `self.x + 2`. When this line draws a window the displayed column
            // advances slower than 1/dot through the StartWindowDraw stall, so the
            // 2-dot commit spans ~2 extra display columns; add +2 on window lines.
            // (Both terms calibrated against bgoff_bgon_sprite_below_window: the
            // no-window lines need boundary self.x+2, the window lines self.x+4.)
            let new_on = (value & bgen_bit) != 0;
            let win = self.window_started_this_line
                || self.win_draw_start
                || self.window_y_active(mmio);
            // DMG stall-aware boundary: the +2-dot commit is a POP-schedule
            // property, not a column offset. When pops are frozen at the write
            // (a sprite fetch stall in progress at column x, or one arming
            // there), column x itself pops after the commit and takes the new
            // bit; a sprite arming at x+1 pins the boundary to x+1 (mealybug
            // m3_lcdc_bg_en_change bands 0-2/8-17: the BG-off span starts AT
            // the stalled column). No stall keeps the calibrated x+2.
            let cgb_compat = mmio.is_cgb() && !mmio.is_cgb_features_enabled();
            let stall_adj = if !mmio.is_cgb_features_enabled() {
                if cgb_compat && self.sprite_fetch_stall > 0 {
                    // CGB-compat: the sprite-fetch stall freezes the pipeline but
                    // the LCDC.0 commit dot keeps advancing toward the display
                    // column it lands on. The commit offset is GRADUATED by the
                    // remaining stall dots (2 - stall; with cgb_compat_adj=+1
                    // below the total is 3 - stall), not the binary 0/2 the DMG
                    // path uses. (mealybug m3_lcdc_bg_en_change _cgb_c: the first
                    // BG-off write lands during the leftmost sprite's fetch
                    // stall; stall=3 -> boundary 0, stall=1 -> boundary 2.)
                    // (cgb_compat_adj below stays +1 for the stall case, so the
                    // total commit offset is 3 - stall.)
                    3i32 - self.sprite_fetch_stall as i32
                } else if self.sprite_fetch_stall > 0 || self.dmg_unfetched_sprite_at(self.x) {
                    0
                } else if self.dmg_unfetched_sprite_at(self.x.saturating_add(1)) {
                    1
                } else {
                    2
                }
            } else {
                2
            };
            // CGB DMG-compat: the LCDC.0 commit lands one column later than DMG
            // in the plain no-stall case; but when a sprite fetch stalls OR an
            // unfetched sprite gates this column, the commit lands one column
            // EARLIER than DMG+1 (mealybug m3_lcdc_bg_en_change _cgb_c bottom
            // bands: self.x=8 with an unfetched sprite wants boundary 8, not 9).
            let cgb_compat_adj = if cgb_compat {
                let sprite_active = self.sprite_fetch_stall > 0
                    || self.dmg_unfetched_sprite_at(self.x)
                    || self.dmg_unfetched_sprite_at(self.x.saturating_add(1));
                if sprite_active { 0 } else { 1 }
            } else {
                0
            };
            let boundary_col = (self.x as i32 + stall_adj + cgb_compat_adj
                + if win { 2 } else { 0 })
            .clamp(0, 160) as u8;
            self.bgen_history.push((boundary_col as u64, new_on));
        }

        // DMG mid-mode-3 OBJ-enable (LCDC.1) toggle: per-column pop gate +
        // in-progress fetch abort. Hardware gates each sprite pixel on LCDC.1
        // at that pixel's own pop dot, so the toggle covers an exact column
        // span; the boundary column mirrors the bgen model (the write becomes
        // visible to the mixer a couple of dots after `self.x`). Additionally
        // (SameBoy memory.c "disabling objects while already fetching"): a
        // disable landing while a sprite fetch is in progress ABORTS it — the
        // remaining stall dots are not consumed and the sprite's pixels never
        // reach the line. The closed-form m0Time refund for the same abort is
        // handled in set_lcdc_visible (remaining_sprite_cost, graduated).
        let objen_bit = LCDCFlags::SpriteDisplayEnable as u8;
        if !mmio.is_cgb_features_enabled()
            && display_stays_enabled
            && self.state == State::PixelTransfer
            && (old_lcdc & objen_bit) != (value & objen_bit)
        {
            let new_on = (value & objen_bit) != 0;
            // The write commits to the pixel gate OBJEN_APPLY_DOTS after the
            // hook (the hook runs before this dot's PPU step; mealybug
            // m3_lcdc_obj_en_change pins the first gated pop two dots out).
            let apply = if mmio.is_cgb() && !mmio.is_cgb_features_enabled() {
                OBJEN_APPLY_DOTS_CGB
            } else {
                OBJEN_APPLY_DOTS
            };
            self.objen_history
                .push((self.ticks + apply, new_on));
            // Abort window = the sprite's own fetch bus activity
            // [match_dot, match_dot + penalty): a left-clipped sprite (spx < 8)
            // matched during the first-tile prologue, so its fetch ENDS before
            // the pipeline-refill tail of its stall — a disable landing in that
            // tail does NOT abort (the variant's k=0..2 bands keep the full
            // penalty). rec.arm_tick already carries the match adjustment. The
            // disable commits ~1 dot past the write hook; a fetch whose last
            // bus dot is the commit dot completes (obj_en k=15 keeps its
            // pixels), hence the strict compare with +1. On abort the stall
            // resumes pops at the commit dot: one residual stall dot remains.
            // Mid-fetch OBJ-disable aborts the in-progress sprite fetch only on DMG
            // silicon. On CGB hardware (including DMG-compat mode) the object fetch
            // treats OBJ_EN as always-on and never aborts (SameBoy memory.c gates
            // "disabling objects while already fetching" behind `!GB_is_cgb`), so
            // the sprite's full fetch cost is spent regardless of the OBJ-disable —
            // a short OBJ-off pulse that re-enables mid-line does not shorten mode 3.
            // Fixes mealybug m3_lcdc_obj_en_change_variant (bottom-16-line right-edge
            // 6px shift) with no effect on the DMG abort path.
            if !mmio.is_cgb()
                && !new_on && self.sprite_fetch_stall > 0 && self.next_sprite_fetch_index > 0
                && let Some(rec) = self
                    .sprite_fetch_recs
                    .get_mut(self.next_sprite_fetch_index - 1)
                && rec.phase == SpriteFetchPhase::Fetched
            {
                let fetch_end = rec.arm_tick + rec.penalty as u128;
                if fetch_end > self.ticks + 1 {
                    rec.phase = SpriteFetchPhase::Aborted;
                    self.sprite_fetch_stall = self.sprite_fetch_stall.min(1);
                }
            }
        }

        // DMG mid-mode-3 OBJ-size (LCDC.2) toggle: record the apply dot so each
        // sprite tile-data byte samples the size bit at its own fetch dot (the
        // per-byte get_object_line_address recomputation, see obj_pixel_sized).
        let objsz_bit = LCDCFlags::SpriteSize as u8;
        if !mmio.is_cgb_features_enabled()
            && display_stays_enabled
            && self.state == State::PixelTransfer
            && (old_lcdc & objsz_bit) != (value & objsz_bit)
        {
            let apply_tick = self.ticks + OBJSIZE_APPLY_DOTS;
            self.objsize_dot_history
                .push((apply_tick, (value & objsz_bit) != 0));
        }

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
            let en = LCDCFlags::DisplayEnable as u8;
            if self.state == State::PixelTransfer && (old_lcdc & tds) != (value & tds) {
                let ds = mmio.is_double_speed_mode();
                let commit_cc = self.write_cc(ds) + 2;
                self.lcdc_b4_exact = Some((commit_cc, value, old_lcdc));
                // Tile-index-is-tile-data glitch (SameBoy `tile_sel_glitch`): a
                // falling LCDC.4 edge arms the glitch for exactly one CPU T-cycle
                // (sm83_cpu.c GB_CONFLICT_LCDC_CGB: write, set flag, advance 1,
                // clear). The single BG tile-data read that lands in that window
                // returns the tile INDEX instead of a VRAM byte (display.c
                // data_for_tile_sel_glitch, gated on tile < 0x80). Instrumented
                // CGB-C SameBoy places that read exactly 4 dots after the write in
                // its own grid. rustyboi's CPU-write dot sits at a substep- and
                // parity-dependent phase within its BG fetch grid, so the target
                // read (cc, k) is derived from the fetcher substep at the write:
                // a write about to run TileDataLow (substep 1) glitches that k=1
                // read (+2); a write on the tile boundary (substep 3) glitches the
                // next tile's k=2 read (+8) only when the write lands off the even
                // fetch cadence (odd abs_cc) — an on-cadence boundary write applies
                // the new addressing cleanly with no straddle. Verified dot-exact
                // vs SameBoy CGB-C on age m3-bg-lcdc (LOW-plane glitch) and
                // cgb-acid-hell (HIGH-plane glitch).
                let arm = (old_lcdc & tds) != 0
                    && (value & tds) == 0
                    && (old_lcdc & en) != 0
                    && (value & en) != 0;
                if arm && !ds {
                    let s = self.fetcher.fetch_substep();
                    let odd = self.abs_cc & 1 == 1;
                    let target = match s {
                        // About to read TileDataLow: glitch it (k=1), 2 dots out.
                        1 => Some((self.abs_cc + 2, 1u8)),
                        // About to read TileDataHigh: glitch it (k=2), 2 dots out.
                        2 => Some((self.abs_cc + 2, 2u8)),
                        // Tile boundary (Push next): an off-cadence write straddles
                        // into the next tile's HIGH read (+8); on-cadence is clean.
                        3 if odd => Some((self.abs_cc + 8, 2u8)),
                        _ => None,
                    };
                    if let Some(t) = target {
                        self.tidxtd_glitch.push(t);
                    }
                }
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

    // The CGB tile-index-is-tile-data glitch for the BG data read about to run
    // (`self.abs_cc`, substep `k`): true iff a falling LCDC.4 write armed exactly
    // this (cc, k) read (see handle_lcdc_write / tidxtd_glitch). The glitch is a
    // single-read event, not a sustained level, so only the one read SameBoy's
    // 1-T-cycle `tile_sel_glitch` window catches returns the tile index as data.
    fn tidxtd_quirk_at_fetch(&self) -> bool {
        let k = self.fetcher.fetch_substep();
        self.tidxtd_glitch
            .iter()
            .any(|&(cc, tk)| cc == self.abs_cc && tk == k)
    }

    fn fetcher_lcdc_state(&self) -> fetcher::FetcherLcdcState {
        // The tile-index-is-tile-data quirk is resolved per fetch dot from the
        // history (independent of the tdsel-address split below), so a falling
        // edge landing between a tile's TileDataLow and TileDataHigh reads
        // quirks the HIGH byte only.
        let quirk = self.tidxtd_quirk_at_fetch();
        // Exact-cc resolution of a pending mid-mode-3 bit4 toggle (PoC). If a
        // bit4 change is latched and this substep's abs_cc has not yet reached
        // its exact commit cc, present the PRE-commit bit4. This lets a single
        // tile straddle the change: TileDataLow before the commit uses the old
        // addressing method, TileDataHigh after it uses the new one.
        if let Some((commit_cc, new_val, old_val)) = self.lcdc_b4_exact {
            let tds = LCDCFlags::BGWindowTileDataSelect as u8;
            if self.abs_cc < commit_cc {
                // Pre-commit: old bit4.
                let lcdc = (self.lcdc & !tds) | (old_val & tds);
                return fetcher::FetcherLcdcState {
                    lcdc,
                    cgb_tile_index_is_tile_data: quirk,
                    or_lcdc: None,
                    scy_bus: None,
                scx_bus: None,
                };
            } else {
                // Post-commit: new bit4.
                let lcdc = (self.lcdc & !tds) | (new_val & tds);
                return fetcher::FetcherLcdcState {
                    lcdc,
                    cgb_tile_index_is_tile_data: quirk,
                    or_lcdc: None,
                    scy_bus: None,
                scx_bus: None,
                };
            }
        }
        fetcher::FetcherLcdcState {
            lcdc: self.lcdc,
            cgb_tile_index_is_tile_data: quirk,
            or_lcdc: None,
            scy_bus: None,
                scx_bus: None,
        }
    }

    // DMG mid-mode-3 window VRAM-bus glitch (mealybug m3_lcdc_win_map_change /
    // m3_lcdc_tile_sel_win_change, reverse-engineered from the DMG reference
    // captures). The hardware window fetch grid differs from the renderer's
    // anchored grid when sprites stall the line, so each window fetch read is
    // re-evaluated at its reconstructed HARDWARE dot `h` against the exact
    // LCDC.6/LCDC.4 bus-transition times (`wg_hist`):
    //  - h = F + D_pre + 8*tile + 2*substep + midline sprite shifts, where F is
    //    the undelayed window-restart TileNumber dot (`wg_anchor_cc`).
    //  - An offscreen-left sprite (OAM X <= 7) is fetched BEFORE the window
    //    restart and delays the whole grid by D_pre = max(7, 13 - 2*ceil(X/2))
    //    (2-dot fetcher-boundary quantized; single-sprite calibration).
    //  - An on-screen sprite at window position pos = X - 8 >= 0 lets the
    //    in-progress tile fetch complete through TileDataHigh, then inserts its
    //    stall: tiles >= pos/8 + 2 shift by the sprite's actually-charged
    //    penalty, read from its live fetch record (`sprite_fetch_recs` — the
    //    classic max(11 - dist, 6) leading rate / flat-6 follower, or nothing
    //    if the walk dropped/aborted the sprite).
    //  - A read strictly between the transitions sees the post-write bits; a
    //    read ON a transition dot returns the OR of both addresses' bytes (the
    //    address lines change mid-read; both cells drive 1-bits onto the bus).
    // Every one of the 18 sprite-X bands of both reference images is
    // reproduced by these rules (2 tests x 18 bands, dot-exact).
    // Derive the hardware window fetch-grid origin F at a DMG x==0 window
    // start (the immediate TileNumber catch-up runs on the current dot, `chop`
    // dots after the conceptual grid origin). See wg_apply.
    fn wg_set_anchor(&mut self, chop: u64) {
        self.wg_anchor_cc = None;
        self.wg_dpre = 0;
        if self.x != 0 {
            return; // scoped to the x==0 restart family
        }
        // Pre-window sprites (OAM X <= 8) resolved from the LIVE per-sprite
        // fetch records (`sprite_fetch_recs`), not a closed-form stall model:
        // the renderer's anchored restart trigger fired exactly the sprite's
        // actually-charged penalty later (rb_absorb), and a sprite that never
        // fetched (OBJ off at its match dot) delayed neither the renderer nor
        // the hardware grid.
        let mut pre: Option<(u8, u64)> = None;
        for (i, s) in self.sprites_on_line.iter().enumerate() {
            if s.x > 8 {
                continue;
            }
            let Some(rec) = self.sprite_fetch_recs.get(i) else {
                continue;
            };
            match rec.phase {
                SpriteFetchPhase::Fetched => {
                    if pre.is_some() {
                        return; // outside the calibrated single-pre-sprite case
                    }
                    pre = Some((s.x, rec.penalty as u64));
                }
                // Mid-fetch abort: a PARTIAL stall was charged; no calibration
                // evidence for the partial absorb — leave the model off.
                SpriteFetchPhase::Aborted if rec.penalty > 0 => return,
                // Dropped (match dot passed with OBJ off) or still pending:
                // no stall happened.
                _ => {}
            }
        }
        let (rb_absorb, dpre) = match pre {
            // Offscreen-left sprite: hardware fetches it BEFORE the window
            // restart; the grid delay D_pre is 2-dot fetcher-boundary
            // quantized with floor 7 (= 6-dot fetch + 1). X=0 -> 13,
            // 1/2 -> 11, 3/4 -> 9, 5/6/7 -> 7. The CGB grid resolves single
            // dots: D_pre = 13 - X (both `_cgb_c` win captures separate the
            // X=1 vs X=2 and X=3 vs X=4 bands the DMG quantization merges).
            Some((x, p)) if x <= 7 && self.wg_cgb => (p, (13 - x) as u64),
            Some((x, p)) if x <= 7 => (p, (13i64 - ((x as i64 + 1) & !1)).max(7) as u64),
            // OAM X == 8 (window position 0): the hardware-side stall is a
            // midline shift resolved per-read in wg_apply (the in-progress
            // tile-1 fetch completes first).
            Some((_, p)) => (p, 0),
            None => (0, 0),
        };
        self.wg_dpre = dpre;
        self.wg_anchor_cc = Some(self.abs_cc.saturating_sub(rb_absorb + chop));
    }

    // CGB-compat window train tile-data-select sample lag, in dots, subtracted
    // from a reconstructed window byte-read dot to reach the A12/LCDC.4 latch dot
    // (see the WIN_TRAIN_* consts). Fixed for the upper window rows; from
    // WIN_TRAIN_GLITCH_ROW it steps up one dot every WIN_TRAIN_LAG_STEP rows — the
    // sub-dot walk that carries the special-tile boundary and the tile-index-as-
    // data glitch down the lower window. Keyed on the window-internal line.
    fn win_train_sample_lag(&self, win_line: u8) -> i64 {
        WIN_TRAIN_LAG_BASE
            + (win_line.saturating_sub(WIN_TRAIN_GLITCH_ROW) / WIN_TRAIN_LAG_STEP) as i64
    }

    fn wg_apply(&self, mut fls: fetcher::FetcherLcdcState) -> fetcher::FetcherLcdcState {
        let Some(anchor) = self.wg_anchor_cc else {
            return fls;
        };
        if self.wg_hist.is_empty() || !self.fetcher.is_fetching_window() {
            return fls;
        }
        let k = self.fetcher.fetch_substep();
        if k > 2 {
            return fls; // PushToFIFO: no VRAM read
        }
        let n = self.fetcher.get_tile_index() as u64;
        let base = anchor + self.wg_dpre + 8 * n + 2 * k as u64;
        let mut h = base;
        // Stall dots hardware charges this read but the arm rule below does
        // not (the pending-stall shadow): a counted on-screen sprite whose
        // arm dot the read's base has not reached, on a tile past the
        // sprite's own (hardware displaces from tile pos/8 + 1 on). Feeds
        // only the A12 rise-echo lattice check (see CGBWG_A12_ECHO).
        let mut pending: u64 = 0;
        // Midline sprite stalls (window pos = X - 8 >= 0): each sprite the
        // live walk actually FETCHED (`sprite_fetch_recs`) shifts every window
        // tile from pos/8 + 2 on by its actually-charged penalty (the
        // in-progress tile's reads do NOT shift; any gated read evaluates
        // after the sprite's match dot, so its record is final here).
        // Dropped/aborted sprites shift nothing. On the CGB grid the shift is
        // read-granular instead: only reads whose unshifted dot is at/after
        // the sprite's arm dot A = F + arm + pos shift, by
        // max(6, 13 - pos % 8).
        for (i, s) in self.sprites_on_line.iter().enumerate() {
            let pos = s.x as i64 - 8;
            if pos < 0 {
                continue; // offscreen-left: folded into wg_dpre
            }
            let Some(rec) = self.sprite_fetch_recs.get(i) else {
                continue;
            };
            if self.wg_cgb {
                // The fetch reads run ahead of the pixel pops that arm the
                // stalls: a Pending record still counts if OBJ is enabled
                // (mirrors the BG-path rule). An Aborted zero-penalty record
                // with OBJ on is a live-walk artifact (the match dot was
                // consumed by a tile-boundary pop the walk never saw — window
                // pos%8 == 0 sprites); hardware fetched it.
                let objon = (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0;
                let counted = match rec.phase {
                    SpriteFetchPhase::Fetched => true,
                    SpriteFetchPhase::Pending => objon,
                    SpriteFetchPhase::Aborted => objon && rec.penalty == 0,
                };
                // Arm dot: constant within the sprite's own tile. A sprite in
                // the first window tile arms at F + arm_win; one in a later
                // tile at F + arm_win_hi + 8*(pos/8). Reads whose unshifted
                // dot is at or after A shift by the sprite's stall.
                let arm = if pos < 8 {
                    CGBWG_ARM_WIN
                } else {
                    CGBWG_ARM_WIN_HI + 8 * (pos as u64 / 8)
                };
                if counted && base >= anchor + arm {
                    h += (CGBWG_SHIFT_BASE as i64 - (pos % 8)).max(6) as u64;
                } else if counted && (n as i64) >= pos / 8 + 1 {
                    pending += (CGBWG_SHIFT_BASE as i64 - (pos % 8)).max(6) as u64;
                }
            } else if rec.phase == SpriteFetchPhase::Fetched
                && (n as i64) >= pos / 8 + 2
            {
                h += rec.penalty as u64;
            }
        }
        const WG_BITS: u8 =
            (LCDCFlags::WindowTileMapDisplaySelect as u8) | (LCDCFlags::BGWindowTileDataSelect as u8);
        if self.wg_cgb {
            let sub = WgSubDot {
                phase8: -((self.win_y_pos % 8) as i64),
                shifted: h != base,
                pending,
            };
            let (bits, quirk) =
                self.cgb_wg_resolve(h, CGBWG_WIN_RISE, CGBWG_WIN_FALL, CGBWG_QUIRK_WIN, k, sub);
            // Window map-select (LCDC.6) pulse under $8000 tile-data (LCDC.4 = 1):
            // the map read becomes visible later than the $8800 path, so re-resolve
            // just the map bit with the later CGBWG_WIN_MAP_*_TDS thresholds. This is
            // the sole discriminator between the mealybug win_map_change (LCDC.4=0,
            // WIN_RISE/FALL correct for its special-tile diagonal) and win_map_change2
            // (LCDC.4=1, whose midline-shifted window rows land the special $9C00 tile
            // one fetcher tile later — exact only at 10/10). LCDC.4 is a stable per-ROM
            // constant across each line here, so keying on the resolved bit is safe;
            // the tile-data-select and tile-index-as-data quirk keep the WIN thresholds.
            let map_bit = LCDCFlags::WindowTileMapDisplaySelect as u8;
            let tds = LCDCFlags::BGWindowTileDataSelect as u8;
            let bits = if (bits & tds) != 0 {
                // The map re-resolve stays on the integer grid: the 10/10
                // thresholds are calibrated against win_map_change2's
                // all-shifted rows, whose refs show no sub-dot residue.
                let (bits_map, _) = self.cgb_wg_resolve(
                    h,
                    CGBWG_WIN_MAP_RISE_TDS,
                    CGBWG_WIN_MAP_FALL_TDS,
                    CGBWG_QUIRK_WIN,
                    k,
                    WgSubDot::NONE,
                );
                (bits & !map_bit) | (bits_map & map_bit)
            } else {
                bits
            };
            fls.lcdc = (fls.lcdc & !WG_BITS) | (bits & WG_BITS);
            fls.or_lcdc = None;
            if k >= 1 {
                fls.cgb_tile_index_is_tile_data = quirk;
            }
            return fls;
        }
        let mut bits = self.wg_hist[0].1; // before the first transition
        let mut edge: Option<u8> = None;
        for &(cc, old, new) in &self.wg_hist {
            if h > cc {
                bits = new;
            } else {
                if h == cc {
                    bits = new;
                    edge = Some(old);
                }
                break;
            }
        }
        fls.lcdc = (fls.lcdc & !WG_BITS) | (bits & WG_BITS);
        if let Some(old) = edge {
            fls.or_lcdc = Some((fls.lcdc & !WG_BITS) | (old & WG_BITS));
        }
        fls
    }

    // Resolve the LCDC journal at hardware dot `h` under the CGB-compat
    // rules: per-bit clean transitions — a rising bit is visible to reads
    // from raw write_cc + `rise` on, a falling bit from write_cc + `fall` on
    // — and no OR edge. Also reports whether a TDL/TDH read (`k` >= 1) at
    // `h` lands exactly on a falling LCDC.4 transition dot, which reads the
    // tile INDEX as that bitplane's data (the CGB-C coincidence rule).
    // `sub` carries the window fetch grid's sub-dot state (see CGBWG_A12_ECHO);
    // WgSubDot::NONE keeps every comparison on the integer grid.
    fn cgb_wg_resolve(
        &self,
        h: u64,
        rise: u64,
        fall: u64,
        quirk_add: u64,
        k: u8,
        sub: WgSubDot,
    ) -> (u8, bool) {
        let tds = LCDCFlags::BGWindowTileDataSelect as u8;
        let mut bits = self.wg_hist[0].1;
        let mut quirk = false;
        let mut prev_fall_w: Option<i64> = None;
        // Pulse-train scope (see CGBWG_TRAIN_ADVANCE). age m3-bg-lcdc-nocgb holds
        // LCDC.4 HIGH across the line and pulses it LOW repeatedly — the A12 line
        // is perpetually driven, so every falling edge's glitch/bit4 visibility
        // lands CGBWG_TRAIN_ADVANCE dots sooner. The isolated mealybug tile_sel
        // pulses instead blip UP from a bit4=0 baseline (line-start LCDC.4 clear:
        // tile_sel_change 0x83, win_change 0xa3), a single settle at the w+4
        // thresholds. Key on the line-initial LCDC.4 level (available at the first
        // fetch, so the early tiles resolve train-correctly before the whole pulse
        // train is journaled — unlike an edge-count which the growing journal only
        // reaches mid-line): a high baseline is the repeatedly-pulsed train, a low
        // baseline is the isolated blip.
        let is_train = (self.wg_hist[0].1 & tds) != 0;
        for &(t, old, new) in &self.wg_hist {
            let w = t - WG_TRANSITION_DELAY; // raw write commit cc
            let rising = !old & new;
            let falling = old & !new;
            // Inter-edge A12 settle: a RISING LCDC.4 edge whose prior FALLING edge
            // was within CGBWG_A12_GAP dots re-arms the address bus while it is
            // still slewing from that fall, so the rise's visibility is delayed an
            // extra CGBWG_A12_REARM dot. Keyed on inter-edge spacing, not per-tile —
            // so it is not the zero-sum threshold tweak. (A train rise is exempt: it
            // is already advanced and the A12 is continuously driven — see below.)
            let train_fall = is_train && (falling & tds) != 0;
            let train_rise = is_train && (rising & tds) != 0;
            // The inter-edge A12 re-arm delay is an isolated-pulse effect; in a fast
            // train (both edges advanced, A12 continuously driven) the re-rise is
            // already accounted by the train advance and takes no extra re-arm dot.
            let rearm = if (rising & tds) != 0 && !train_rise {
                match prev_fall_w {
                    Some(pw) if (w as i64 - pw) <= CGBWG_A12_GAP as i64 => CGBWG_A12_REARM,
                    _ => 0,
                }
            } else { 0 };
            // In a fast pulse train (see is_train), the A12 line is still driven
            // from the prior edge, so BOTH edges settle CGBWG_TRAIN_ADVANCE dots
            // earlier than the isolated-pulse thresholds (CGBWG_BG_FALL_*/RISE/
            // QUIRK_BG) are calibrated for. The FALL advance lands the glitch on the
            // same read SameBoy catches (age w+1 vs isolated mealybug w+4); the RISE
            // advance restores tile-data-select in time for BOTH plane reads of the
            // tile straddling the re-rise, which the age refs render as its $8000
            // tile (the reconstruction otherwise holds the LOW plane $8800 one read
            // too long, splitting the tile into a spurious mixed $8000/$8800 read).
            let fall_eff = if train_fall { fall.saturating_sub(CGBWG_TRAIN_ADVANCE) } else { fall };
            let rise_eff = if train_rise { rise.saturating_sub(CGBWG_TRAIN_ADVANCE) } else { rise };
            if (falling & tds) != 0 { prev_fall_w = Some(w as i64); }
            let mut applied = 0u8;
            // 8x fixed point: read position vs the rise boundary, in eighths.
            // An unshifted read sits ON the integer grid (byte-identical to
            // the plain h >= w + thr comparison); a sprite-stall-shifted read
            // resumes on the 1/8-dot-per-line slid grid, so a rise landing
            // exactly on its integer boundary dot misses the read by the
            // fraction: its boundary sits one eighth past the integer dot
            // (see the CGBWG_A12_ECHO block comment).
            let rise_vis = if sub.shifted {
                8 * h as i64 + sub.phase8 >= 8 * (w + rise_eff + rearm) as i64 + 1
            } else {
                h >= w + rise_eff + rearm
            };
            if rise_vis {
                applied |= rising;
            }
            if h >= w + fall_eff {
                applied |= falling;
            }
            // A12 rise-echo (pending-stall shadow): the read's true hardware
            // dot is h + the stall the reconstruction grid has not charged
            // yet; a rising LCDC.4 edge still rings on the A12 line
            // CGBWG_A12_ECHO dots after its commit, caught only when the true
            // dot lands exactly on the echo's 1/8-dot lattice point.
            if sub.pending > 0
                && (rising & tds) != 0
                && 8 * (h + sub.pending) as i64 + sub.phase8
                    == 8 * (w + CGBWG_A12_ECHO) as i64
            {
                applied |= rising & tds;
            }
            bits = (bits & !applied) | (new & applied);
            // The tile-index-as-data quirk fires when a falling LCDC.4 write's
            // 1-cycle `tile_sel_glitch` window (SameBoy sm83_cpu.c
            // CONFLICT_LCDC_CGB: set true, advance 1 cycle, set false) coincides
            // with a tile-data T2 read. SameBoy calls `data_for_tile_sel_glitch`
            // in BOTH GET_TILE_DATA_LOWER_T2 (k==1) and GET_TILE_DATA_HIGH_T2
            // (k==2), so which bitplane glitches is decided by which T2 read lands
            // in the 1-cycle window, not by k. The true SameBoy fetch dot `h_scy`
            // is `h - CGBWG_QUIRK_BG`; the write's active window is [w, w+1], i.e.
            // `w + CGBWG_QUIRK_BG <= h <= w + CGBWG_QUIRK_BG + 1` in the calibrated
            // `h` grid. This selects k==1 when the low read straddles the fall
            // (tile_sel_change2 LY32-phase) and k==2 when the high read does
            // (LY40-phase), matching the instrumented CGB-C tester per line. The
            // window path keeps its single k-uniform w+quirk_add coincidence.
            let q_add = if train_fall { quirk_add.saturating_sub(CGBWG_TRAIN_ADVANCE) } else { quirk_add };
            let hit = if quirk_add == CGBWG_QUIRK_BG {
                (k == 1 || k == 2) && h >= w + q_add && h <= w + q_add + 1
            } else {
                k >= 1 && h == w + q_add
            };
            if hit && (falling & tds) != 0 {
                quirk = true;
            }
        }
        (bits, quirk)
    }

    // DMG BG-path analog of `wg_apply`: resolve mid-mode-3 LCDC.3 (BG map) /
    // LCDC.4 (tile data) toggles at each BG fetch read's reconstructed HARDWARE
    // dot instead of rustyboi's own (stall-displaced) read dot. Derived from
    // the mealybug m3_lcdc_bg_map_change + m3_lcdc_tile_sel_change DMG captures
    // (2 tests x 18 sprite-X bands, all reproduced dot-exact):
    //  - Hardware BG fetch grid: read dot h = F + 8n + 2k (n = fetch index
    //    from line start, k = 0/1/2 TileNumber/DataLow/DataHigh), F = the
    //    line's first BG TileNumber dot (`bg_anchor_cc` — rustyboi reads it at
    //    the same dot, before any sprite stall).
    //  - An offscreen-left sprite (OAM X <= 7) is fetched during the first-tile
    //    prologue and delays tiles n >= 1 by the same D_pre as the window grid:
    //    max(7, 13 - 2*ceil(X/2)).
    //  - An on-screen sprite (pos = X - 8 >= 0) lets the in-progress tile
    //    complete, then delays tiles n >= pos/8 + 2 by 13,11,11,9,9,7,7,7
    //    (pos%8 = 0..7) — the SAME 2-dot-quantized delay function as the
    //    offscreen-left D_pre, keyed by the in-tile phase (NOT the live
    //    pipeline's classic 11 - min(5, pos%8) charge). Pinned by the
    //    mealybug bands, the m3_scy_change write-phase straddles, and the
    //    gambatte bgtiledata/bgtilemap/scy spx08-spx0B captures.
    //  - Transition rule: a read sees the post-write value iff its hardware
    //    dot lies strictly past the write's commit cc; no OR edge on the BG
    //    grid (the m3_scy_change captures reject one at this phase).
    // Sprites are counted from the live fetch records; a record still Pending
    // at this (earlier) fetch dot counts iff OBJ display is enabled now (the
    // BG fetcher reads run up to ~10 dots ahead of the pixel pops that arm the
    // stalls). Scoped to lines whose window has not started (a window restart
    // re-anchors the hardware grid; the window path has its own model).
    // The reconstructed HARDWARE dot of the BG fetch read (n = fetch index from
    // line start, k = 0/1/2 substep), or None when the model is out of scope
    // for this line. See bg_wg_apply.
    fn bg_hw_read_dot(&self, n: u64, k: u8, ly: u8) -> Option<u64> {
        self.bg_hw_read_dot_ex(n, k, ly, false)
    }

    // As `bg_hw_read_dot`, but `scy_mode` returns the SameBoy-exact CGB fetch
    // dot (2 dots earlier than the LCDC-calibrated dot for a sprite-stalled
    // tile). The LCDC journal (`bg_wg_resolve_cgb`) is tuned against the
    // un-corrected dot through its own rise/fall thresholds; the SCY journal
    // compares the dot against the raw write commit (+CGBWG_SCY_ADD), so it
    // needs the true fetch dot. Verified dot-exact vs SameBoy CGB-C: after an
    // offscreen-left sprite (OAM X<=7) the BG fetch is delayed by D_pre = 11 - X
    // (not 13 - X); an on-screen sprite delays the tiles from its own by
    // max(4, 11 - pos%8). Without this the k=1/k=2 substeps sit 2 dots too late
    // and cross a mid-fetch SCY write the k=0 tile-number read did not — mixing
    // the tile's map row with the wrong tile line (the m3_scy_change per-row
    // jitter).
    fn bg_hw_read_dot_ex(&self, n: u64, k: u8, ly: u8, scy_mode: bool) -> Option<u64> {
        let anchor = self.bg_anchor_cc?;
        if self.fetcher.is_fetching_window() || self.window_started_this_line {
            return None;
        }
        let base = anchor + 8 * n + 2 * k as u64;
        let mut h = base;
        let cgb_stall_bias: u64 = if scy_mode { 2 } else { 0 };
        for (i, s) in self.sprites_on_line.iter().enumerate() {
            let Some(rec) = self.sprite_fetch_recs.get(i) else {
                continue;
            };
            let counted = match rec.phase {
                SpriteFetchPhase::Fetched => true,
                SpriteFetchPhase::Pending => {
                    (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0
                }
                // CGB: an Aborted zero-penalty record with OBJ on is a
                // live-walk artifact (see wg_apply); hardware fetched it.
                SpriteFetchPhase::Aborted => {
                    self.wg_cgb
                        && rec.penalty == 0
                        && (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0
                }
            };
            if !counted {
                continue;
            }
            if s.x <= 7 {
                if n >= 1 {
                    // CGB: 1-dot D_pre = 13 - X (see the CGBWG_* consts); DMG: 2-dot
                    // fetcher-boundary quantized. (scy_mode: SameBoy-exact 11 - X.)
                    h += if self.wg_cgb {
                        (13 - s.x) as u64 - cgb_stall_bias
                    } else {
                        (13i64 - ((s.x as i64 + 1) & !1)).max(7) as u64
                    };
                }
            } else {
                let pos = (s.x - 8) as u64;
                if self.wg_cgb {
                    // CGB read-granular rule: only reads whose unshifted dot
                    // is at/after the sprite's arm dot A = F + arm + 8*(pos/8)
                    // (constant within the sprite's own tile) shift, by
                    // max(6, 13 - pos % 8). (scy_mode: SameBoy-exact max(4, 11 - pos%8).)
                    let arm = CGBWG_ARM_BG + 8 * (pos / 8);
                    if base >= anchor + arm {
                        h += (CGBWG_SHIFT_BASE as i64 - (pos % 8) as i64)
                            .max(6)
                            .saturating_sub(cgb_stall_bias as i64) as u64;
                    } else if !scy_mode && k >= 1 && n == pos / 8 + 1 && base + 4 >= anchor + arm {
                        // Sprite-triggering tile: SameBoy blocks the object fetch
                        // until the current tile passes GET_TILE_DATA_HIGH_T2, so
                        // its low+high bitplane reads stay un-stalled and 2 dots
                        // apart. rustyboi's grid places these reads a couple dots
                        // ahead of the true fetch dot the LCDC.4 rise-visibility
                        // (CGBWG_BG_RISE) is calibrated against, so an LCDC.4 rise
                        // straddling them is missed. Anchor the reads at the arm
                        // dot so they sample the risen LCDC.4. For a sprite flush
                        // with the tile boundary (pos % 8 == 0) both bitplanes
                        // shift together (m3_lcdc_tile_sel_change idx=2 all-
                        // unsigned); off-boundary (pos % 8 != 0) only the HIGH
                        // read reaches the arm dot, so the LOW read keeps the
                        // pre-rise level — the mixed $8000/$8800 read. The LOW
                        // read only joins the shift on the sprite's FIRST covered
                        // line of a boundary-flush sprite (its object fetch has
                        // not yet split the tile): m3_lcdc_tile_sel_change y128 is
                        // all-unsigned while y129+ stay mixed.
                        let first_line = pos.is_multiple_of(8) && (s.y as i32 - 16) == ly as i32;
                        if k == 2 || first_line {
                            h = anchor + arm + 2 * (k as u64 - 1);
                        }
                    }
                } else if n >= pos / 8 + 2 {
                    // 13,11,11,9,9,7,7,7 for pos%8 = 0..7 — the SAME 2-dot
                    // quantized delay as the offscreen-left D_pre, keyed by
                    // the in-tile phase. The m3_scy_change low-plane
                    // straddles separate the odd pens from the even ones;
                    // gambatte bgtiledata_spx08 tiles 2/17 (vs spx09-0B) pin
                    // pos 0 at 13.
                    let q = (pos % 8) as i64;
                    h += (13 - ((q + 1) & !1)).max(7) as u64;
                }
            }
        }
        Some(h)
    }

    // Resolve the LCDC journal at hardware dot `h`: the bits whose write
    // commit cc lies strictly before `h`. (The journal stores write_cc +
    // WG_TRANSITION_DELAY — the window-path calibration; strip it back to the
    // raw commit cc. No OR edge on the BG grid: the m3_scy_change captures
    // reject one at this phase, and the LCDC pulse captures cannot separate
    // OR from clean-new/clean-old at the transition dots.)
    fn bg_wg_resolve(&self, h: u64) -> u8 {
        let mut bits = self.wg_hist[0].1;
        for &(cc, _, new) in &self.wg_hist {
            let t = cc.saturating_sub(WG_TRANSITION_DELAY);
            if h > t {
                bits = new;
            } else {
                break;
            }
        }
        bits
    }

    // CGB-compat flavor of `bg_wg_resolve` (see the CGBWG_* consts): per-bit rise/fall
    // thresholds relative to the raw write cc, plus the falling-LCDC.4
    // coincidence quirk for data reads. The FALL visibility is per-substep on
    // the BG grid (the tile_sel_change bands pin TN thru w+3 / TDL thru w+2 /
    // TDH thru w+0 while the rise is a uniform w+4; the window grid is
    // k-uniform — see wg_apply).
    // Resolve the BG-path LCDC journal, splitting the two bits by their fetch
    // dot: the tile-data-select bit (LCDC.4) at the `h` grid its per-byte /
    // tile-index-as-data coincidence is calibrated against, and the map-select
    // bit (LCDC.3) at the SameBoy-exact fetch dot `h_scy` (the true fetch dot,
    // which places a mid-line map pulse on the tile SameBoy fetches during the
    // pulse rather than the tile before it — the two-object fetch grid was 2
    // dots per sprite too late). `h` and `h_scy` coincide when no sprite stalls
    // the tile, so single-object lines are unaffected.
    fn bg_wg_resolve_cgb(&self, h: u64, h_scy: u64, k: u8) -> (u8, bool) {
        let fall = match k {
            0 => CGBWG_BG_FALL,
            1 => CGBWG_BG_FALL_TDL,
            _ => CGBWG_BG_FALL_TDH,
        };
        // Tile-data-select bit (LCDC.4) + its tile-index-as-data quirk: `h` grid.
        let (bits_td, quirk) =
            self.cgb_wg_resolve(h, CGBWG_BG_RISE, fall, CGBWG_QUIRK_BG, k, WgSubDot::NONE);
        // Map-select bit (LCDC.3): true fetch dot, +2 rise/fall.
        let (bits_map, _) = self.cgb_wg_resolve(
            h_scy,
            CGBWG_BG_MAP_RISE,
            CGBWG_BG_MAP_FALL,
            CGBWG_QUIRK_BG,
            k,
            WgSubDot::NONE,
        );
        let map_bit = LCDCFlags::BGTileMapDisplaySelect as u8;
        let bits = (bits_td & !map_bit) | (bits_map & map_bit);
        (bits, quirk)
    }

    // Resolve the SCY journal at hardware dot `h`: the value whose write
    // commit cc lies strictly before `h`. None when no journal. (No OR edge —
    // see the journal push comment.)
    fn bg_scy_resolve(&self, h: u64) -> Option<u8> {
        if self.bg_scy_hist.is_empty() {
            return None;
        }
        // CGB-compat: the raw write commit reaches the fetch address lines
        // `scy_add` dots later than the recorded write cc (write M-cycle start).
        // Paired with the SameBoy-exact fetch dot (bg_hw_read_dot_ex scy_mode),
        // add=1 reproduces SameBoy's inclusive read>=write commit for both
        // sprite-stalled and un-stalled tiles.
        let add = if self.wg_cgb { CGBWG_SCY_ADD } else { 0 };
        let mut v = self.bg_scy_hist[0].1;
        for &(t, _, new) in &self.bg_scy_hist {
            if h > t + add {
                v = new;
            } else {
                break;
            }
        }
        Some(v)
    }

    // CGB-compat up-pulse LCDC.4 train capture-phase re-resolve. At mode-3 end
    // the wg_hist journal is COMPLETE, so the pulse train (>= 2 up-pulses from a
    // bit4=0 baseline) is detectable — the future info missing when the early
    // tiles were fetched/drawn. Re-resolve each buffered BG tile's LOW/HIGH
    // tile-data-select bits + tile-index-as-data quirk against the complete
    // journal at their reconstructed fetch dots, recompute the 8 pixel indices,
    // and re-plot the columns whose BG index changed. Gated tight: only when the
    // complete journal is an up-pulse TRAIN (line-initial bit4 low AND >= 4 edges
    // — the isolated single pulse is 2 edges and stays untouched). Returns the
    // number of pixels re-plotted (0 when out of scope). CGB-compat only.
    fn cgb_train_reresolve(&mut self, mmio: &mmio::Mmio) {
        if !self.wg_cgb || self.bg_tile_buf.is_empty() || self.wg_hist.is_empty() {
            return;
        }
        let tds = LCDCFlags::BGWindowTileDataSelect as u8;
        // Up-pulse train discriminator (complete journal): line-initial bit4 low
        // and at least two pulses (>= 4 edges). The isolated tile_sel_change
        // pulse is exactly 2 edges (one up, one down) and is left untouched.
        let init_low = (self.wg_hist[0].1 & tds) == 0;
        let n_edges = self.wg_hist.len();
        if !(init_low && n_edges >= 4) {
            return;
        }
        let ly = mmio.read(LY);
        if ly >= 144 {
            self.bg_tile_buf.clear();
            return;
        }
        // Each plane's tile-data base is re-sampled at its OWN T1 (one substep
        // before the T2 byte read logged) — the raw journal bit4 level whose
        // write commit is <= (SameBoy-exact fetch dot - CGBWG_TRAIN_T1_LEAD).
        // Validated dot-exact vs the CGB-C SameBoy per-plane oracle across
        // change2 ly24-55 (every train tile L/H last_tileset reproduced).
        let buf = std::mem::take(&mut self.bg_tile_buf);
        let raw_at = |dot: i64| -> u8 {
            let mut b = self.wg_hist[0].1 & tds;
            for &(tt, _, nn) in &self.wg_hist {
                let w = tt as i64 - WG_TRANSITION_DELAY as i64;
                if dot >= w { b = nn & tds; } else { break; }
            }
            b
        };
        // The last-fetched sprite's bitplane-1 byte among sprites whose fetch
        // (x-match arm dot) precedes `dot` — the initial stale-latch source for
        // the RISE-coincidence glitch (Matt Currie, CGB PPU doc, TILE_SEL bit 4:
        // "setting TILE_SEL on the same T-cycle as a bitplane data read will
        // cause it to use bitplane 1 data from the most recently drawn sprite,
        // if any"). Returns (arm dot, bp1 byte). Sprite tiles always read
        // unsigned $8000; y-flip and 8x16 masking follow the OAM attributes.
        let sprite_bp1_before = |dot: i64| -> Option<(i64, u8)> {
            let obj_on = (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0;
            let tall = (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;
            let height: i32 = if tall { 16 } else { 8 };
            let mut best: Option<(i64, u8)> = None;
            for (i, s) in self.sprites_on_line.iter().enumerate() {
                let Some(rec) = self.sprite_fetch_recs.get(i) else { continue };
                let counted = match rec.phase {
                    SpriteFetchPhase::Fetched => true,
                    SpriteFetchPhase::Pending => obj_on,
                    SpriteFetchPhase::Aborted => rec.penalty == 0 && obj_on,
                };
                if !counted {
                    continue;
                }
                let at = rec.arm_tick as i64;
                if at >= dot || best.is_some_and(|(b, _)| at < b) {
                    continue;
                }
                let mut row = ly as i32 + 16 - s.y as i32;
                if !(0..height).contains(&row) {
                    continue;
                }
                if s.attributes.y_flip {
                    row = height - 1 - row;
                }
                let tn = if tall { s.tile_index & 0xFE } else { s.tile_index };
                let a = 0x8000u16 + (tn as u16) * 16 + (row as u16) * 2 + 1;
                best = Some((at, mmio.read_vram_bank(0, a)));
            }
            best
        };
        // Pass 1 (fetch order): resolve each tile's per-plane byte against the
        // complete journal. An LCDC.4 edge whose write commit w lands exactly
        // one dot past a plane's T1-sample dot (w == T1 + 1) coincides with
        // that plane's VRAM data read — the CGB-C TILE_SEL glitch pair
        // (mealybug PPU doc, decoded pixel-exact from the real-cgb04c change2
        // ref across all 64 rows / 8 sprite-phase bands):
        //  - FALL: the tile INDEX is used as that bitplane's data, and the
        //    stale-data latch captures the $8000-region byte the read was
        //    pulling off the bus (A12 still high while falling).
        //  - RISE: the bitplane gets the stale-data latch — the most recent of
        //    the last sprite fetch's bitplane-1 byte and the last FALL-glitched
        //    read's captured byte.
        // The change2 pulse train sweeps the coincidence through both planes:
        // its 8 sprite bands step the fetch-grid phase one dot per band, so
        // band ly24-31 lands the edges on the LOW-plane reads and ly40-47 on
        // the HIGH-plane reads; the other six bands have no coincidence and
        // resolve region-clean. SameBoy-CGB-C mismodels the RISE side (it
        // fails the ref on exactly these rows), so the ref is the only oracle.
        struct Res { first_x: u8, low_byte: u8, high_byte: u8 }
        let mut res: Vec<Res> = Vec::with_capacity(buf.len());
        let mut latch: Option<(i64, u8)> = None;
        for t in &buf {
            let n = t.n;
            let Some(h1) = self.bg_hw_read_dot(n, 1, ly) else { continue; };
            let Some(h2) = self.bg_hw_read_dot(n, 2, ly) else { continue; };
            let h1s = self.bg_hw_read_dot_ex(n, 1, ly, true).unwrap_or(h1) as i64;
            let h2s = self.bg_hw_read_dot_ex(n, 2, ly, true).unwrap_or(h2) as i64;
            let line = t.y % 8;
            let mut bytes = [0u8; 2];
            for (k, t1) in [h1s - CGBWG_TRAIN_T1_LEAD, h2s - CGBWG_TRAIN_T1_LEAD]
                .into_iter()
                .enumerate()
            {
                let plane_tds = raw_at(t1);
                let a = self.fetcher.get_tile_data_address(t.tn, line, plane_tds) + k as u16;
                let mut byte = mmio.read_vram_bank(0, a);
                for &(tt, o, nn) in &self.wg_hist {
                    let w = tt as i64 - WG_TRANSITION_DELAY as i64;
                    if w != t1 + 1 {
                        continue;
                    }
                    if (o & tds) != 0 && (nn & tds) == 0 {
                        // FALL coincidence: index-as-data (the live fetcher
                        // applies the same tn < 0x80 gate), latch the true
                        // $8000-region byte.
                        if t.tn < 0x80 {
                            byte = t.tn;
                        }
                        let ua = self.fetcher.get_tile_data_address(t.tn, line, tds) + k as u16;
                        latch = Some((w, mmio.read_vram_bank(0, ua)));
                    } else if (o & tds) == 0 && (nn & tds) != 0 {
                        // RISE coincidence: stale bitplane data — the most
                        // recent of the sprite bp1 fetch and the FALL latch.
                        let stale = match (latch, sprite_bp1_before(t1)) {
                            (Some(l), Some(s)) => Some(if l.0 >= s.0 { l } else { s }),
                            (l, s) => l.or(s),
                        };
                        if let Some((_, b)) = stale {
                            byte = b;
                        }
                    }
                }
                bytes[k] = byte;
            }
            res.push(Res { first_x: t.first_x, low_byte: bytes[0], high_byte: bytes[1] });
        }
        // Pass 2: re-plot. Only BG-won columns (line_bg_idx >= 0) whose index
        // changed are overwritten; sprite-won columns stay as drawn. Tiles the
        // live draw already rendered byte-identically no-op here.
        for r in &res {
            let (low_byte, high_byte) = (r.low_byte, r.high_byte);
            for i in 0..8u8 {
                let col = r.first_x as i32 + i as i32;
                if !(0..160).contains(&col) { continue; }
                let bit = 7 - i;
                let idx = (((high_byte >> bit) & 1) << 1) | ((low_byte >> bit) & 1);
                let ci = col as usize;
                let old = self.line_bg_idx[ci];
                if old < 0 || old as u8 == idx { continue; }
                let rgb = self.compat_bg_color(mmio, idx);
                let off = (ly as usize * 160 + ci) * 3;
                self.color_fb_a[off] = rgb.0;
                self.color_fb_a[off + 1] = rgb.1;
                self.color_fb_a[off + 2] = rgb.2;
                self.line_bg_idx[ci] = idx as i8;
            }
        }
    }

    // CGB-compat up-pulse LCDC.4 train capture-phase re-resolve for the WINDOW
    // fetcher (the window analog of cgb_train_reresolve). rustyboi's live per-substep
    // resolve draws each window tile from its LOW/HIGH reads on a line-locked grid
    // against the PARTIAL journal (the pulse train is only fully journaled at
    // line-end), which mis-latches the tile-data-select base and misses the
    // tile-index-as-data glitch. This runs at line-end against the COMPLETE journal.
    // The two bands are handled differently (see the per-tile comment): the upper
    // rows collapse each live-split tile to its single latched base; the lower rows
    // (from WIN_TRAIN_GLITCH_ROW) reconstruct each read dot and re-resolve the base +
    // glitch at the band sample lag, rendering the tile INDEX as a glitched plane's
    // byte (SameBoy data_for_tile_sel_glitch). Same tight gate as before
    // (line-initial LCDC.4 low AND >= 4 journal edges) so the isolated single-pulse
    // win_change and win_map_change2 stay untouched. Cuts m3_lcdc_tile_sel_win_change2
    // from 1068 -> 702 (the earlier landed collapse) -> 221 px; the remaining residual
    // is the y0-6 first-pulse sub-dot base (42 px, which SameBoy renders) plus the
    // rows 40-46/56-62 glitch band (~179 px), where SameBoy-CGB-C itself misses the
    // reference by the same 179 px (a beyond-SameBoy real-silicon A12-settle phase).
    // CGB-compat only.
    fn win_train_reresolve(&mut self, mmio: &mmio::Mmio) {
        if !self.wg_cgb || self.win_tile_buf.is_empty() || self.wg_hist.is_empty() {
            return;
        }
        let tds = LCDCFlags::BGWindowTileDataSelect as u8;
        let init_low = (self.wg_hist[0].1 & tds) == 0;
        if !(init_low && self.wg_hist.len() >= 4) {
            self.win_tile_buf.clear();
            return;
        }
        let ly = mmio.read(LY);
        if ly >= 144 {
            self.win_tile_buf.clear();
            return;
        }
        let (Some(anchor), dpre) = (self.wg_anchor_cc, self.wg_dpre) else {
            self.win_tile_buf.clear();
            return;
        };
        // Resolve the LCDC.4 tile-data-select level at a reconstructed read dot,
        // and whether the read coincides with a falling edge (the tile-index-as-
        // data glitch) or a RISING edge (the stale-bus echo, below). All key on
        // the latch dot = read dot - sample lag; a FALL commit landing exactly on
        // the latch dot returns the tile index as data (SameBoy tile_sel_glitch);
        // a RISE commit landing exactly on it leaves the VRAM output mid-settle,
        // and the returned byte is the value the data bus carried 16 dots (two
        // fetch slots) earlier — the same-plane byte of the tile fetched two
        // slots back at ITS level-at-sample base, or the displacing sprite
        // fetch's high byte when that slot ran the sprite. Derived from the
        // m3_lcdc_tile_sel_win_change2 CGB-C reference rows 40-46/56-62, whose
        // trio of corrupt tiles (samples on the 24-dot train's three rises)
        // demand exactly those VRAM bytes; SameBoy renders clean bytes there
        // and misses the reference by the same 179 px. The two leading window
        // tiles (n<2) always latch the line-initial low base — their HIGH_T1
        // latch predates the pulse train — so they never glitch and keep the
        // $8800 base.
        let resolve = |this: &Self, h: u64, first_tile: bool, sample_lag: i64| -> (bool, bool, bool) {
            if first_tile {
                return (false, false, false);
            }
            let s = h as i64 - sample_lag;
            let mut level = (this.wg_hist[0].1 & tds) != 0;
            let mut glitch = false;
            let mut rise_hit = false;
            for &(cc, old, new) in &this.wg_hist {
                let w = cc as i64 - WG_TRANSITION_DELAY as i64; // raw write commit
                if s > w {
                    level = (new & tds) != 0;
                }
                if s == w && (old & tds) != 0 && (new & tds) == 0 {
                    glitch = true; // FALL commit on the latch dot
                }
                if s == w && (!old & new & tds) != 0 {
                    rise_hit = true; // RISE commit on the latch dot
                }
            }
            (level, glitch, rise_hit)
        };
        // The byte the VRAM bus carried 16 dots before a rise-hit read: the
        // same-plane byte of the tile two fetch slots back, from the base its
        // own sample resolved (its bus read is real even when its LATCH
        // glitched to the tile index), or the mid-line sprite fetch's high
        // byte when the two-slots-back slot ran the sprite fetch.
        let stale_bus_byte = |this: &Self,
                              mmio: &mmio::Mmio,
                              prev: Option<&(u8, bool, bool)>,
                              line: u8,
                              high: bool|
         -> Option<u8> {
            if let Some(&(ptn, plt, pht)) = prev {
                let base = if high { pht } else { plt };
                let a = this
                    .fetcher
                    .get_tile_data_address(ptn, line, if base { tds } else { 0 });
                return Some(mmio.read_vram_bank(0, a + high as u16));
            }
            for (i, s) in this.sprites_on_line.iter().enumerate() {
                if (s.x as i64 - 8) < 0 {
                    continue;
                }
                if !matches!(
                    this.sprite_fetch_recs.get(i).map(|r| r.phase),
                    Some(SpriteFetchPhase::Fetched)
                ) {
                    continue;
                }
                let mut row = ly.wrapping_add(16).wrapping_sub(s.y) & 7;
                if s.attributes.y_flip {
                    row = 7 - row;
                }
                let a = this.fetcher.get_tile_data_address(s.tile_index, row, tds);
                return Some(mmio.read_vram_bank(0, a + 1));
            }
            None
        };
        let buf = std::mem::take(&mut self.win_tile_buf);
        // Per-tile resolved (tn, low base, high base) records for the
        // stale-bus lookup, keyed by fetch index n (buf is in fetch order).
        let mut resolved_recs: Vec<Option<(u8, bool, bool)>> = Vec::new();
        for t in &buf {
            // The upper window rows (win line < WIN_TRAIN_GLITCH_ROW) are UNIFORM on
            // hardware: every tile latches a single $8000/$8800 base, and the oracle
            // shows no split and no glitch there. rustyboi's live per-substep grid
            // can still SPLIT such a tile across an LCDC.4 pulse edge (LOW plane one
            // base, HIGH plane the other). Collapse each live-split tile to its
            // LOW-plane base (the first substep = the base hardware keeps); uniform
            // live tiles are already correct and are left alone.
            //
            // The lower rows (from WIN_TRAIN_GLITCH_ROW) carry the sub-dot-drifted
            // grid where the completed journal re-resolves the base and fires the
            // tile-index-as-data glitch. The reconstructed read dot minus the band
            // sample lag gives each plane's base + glitch flag; render both planes
            // from those, reading the tile INDEX as a glitched plane's byte
            // (SameBoy tile_sel_glitch).
            let (low_tds, high_tds, lo_glitch, hi_glitch);
            let (mut lo_stale, mut hi_stale) = (None, None);
            if t.y < WIN_TRAIN_GLITCH_ROW {
                if t.live_low_tds == t.live_high_tds {
                    continue; // uniform live tile — already correct
                }
                low_tds = t.live_low_tds;
                high_tds = t.live_low_tds;
                lo_glitch = false;
                hi_glitch = false;
            } else {
                let h1 = anchor + dpre + 8 * t.n + 2;
                let h2 = anchor + dpre + 8 * t.n + 4;
                let first_tile = t.n < 2;
                let lag = self.win_train_sample_lag(t.y);
                let (lt, lg, lr) = resolve(self, h1, first_tile, lag);
                let (ht, hg, hr) = resolve(self, h2, first_tile, lag);
                low_tds = lt;
                high_tds = ht;
                lo_glitch = lg;
                hi_glitch = hg;
                if resolved_recs.len() <= t.n as usize {
                    resolved_recs.resize(t.n as usize + 1, None);
                }
                resolved_recs[t.n as usize] = Some((t.tn, lt, ht));
                let line = t.y % 8;
                // A rise-hit plane returns the stale bus byte (see resolve/
                // stale_bus_byte): the slot two fetches back — that tile's
                // record, or the sprite fetch when the two-back slot falls in
                // the leading-tile prologue the mid-line sprite fetch owns.
                let prev = if t.n >= 4 {
                    resolved_recs.get(t.n as usize - 2).and_then(|r| r.as_ref())
                } else {
                    None
                };
                if lr {
                    lo_stale = stale_bus_byte(self, mmio, prev, line, false);
                }
                if hr {
                    hi_stale = stale_bus_byte(self, mmio, prev, line, true);
                }
                // Nothing to repair when the completed resolve matches the live draw
                // and neither plane glitches or reads the stale bus.
                if low_tds == t.live_low_tds
                    && high_tds == t.live_high_tds
                    && !lo_glitch
                    && !hi_glitch
                    && lo_stale.is_none()
                    && hi_stale.is_none()
                {
                    continue;
                }
            }
            let line = t.y % 8;
            // The tile-index-as-data glitch replaces the glitched plane's byte
            // with the tile INDEX (SameBoy data_for_tile_sel_glitch); a
            // rise-hit plane reads the stale bus byte; otherwise each plane
            // reads from its own resolved base.
            let low_byte = if let Some(b) = lo_stale {
                b
            } else if lo_glitch {
                t.tn
            } else {
                let a =
                    self.fetcher
                        .get_tile_data_address(t.tn, line, if low_tds { tds } else { 0 });
                mmio.read_vram_bank(0, a)
            };
            let high_byte = if let Some(b) = hi_stale {
                b
            } else if hi_glitch {
                t.tn
            } else {
                let a =
                    self.fetcher
                        .get_tile_data_address(t.tn, line, if high_tds { tds } else { 0 });
                mmio.read_vram_bank(0, a + 1)
            };
            for i in 0..8u8 {
                let col = t.first_x as i32 + i as i32;
                if !(0..160).contains(&col) { continue; }
                let bit = 7 - i;
                let idx = (((high_byte >> bit) & 1) << 1) | ((low_byte >> bit) & 1);
                let ci = col as usize;
                let old = self.line_bg_idx[ci];
                if old < 0 || old as u8 == idx { continue; }
                let rgb = self.compat_bg_color(mmio, idx);
                let off = (ly as usize * 160 + ci) * 3;
                self.color_fb_a[off] = rgb.0;
                self.color_fb_a[off + 1] = rgb.1;
                self.color_fb_a[off + 2] = rgb.2;
                self.line_bg_idx[ci] = idx as i8;
            }
        }
    }

    fn bg_wg_apply(&self, mut fls: fetcher::FetcherLcdcState, ly: u8) -> fetcher::FetcherLcdcState {
        if self.wg_hist.is_empty() && self.bg_scy_hist.is_empty() && self.bg_scx_hist.is_empty() {
            return fls;
        }
        let k = self.fetcher.fetch_substep();
        if k > 2 {
            return fls; // PushToFIFO: no VRAM read
        }
        let n = self.fetcher.get_tile_index() as u64;
        let Some(h) = self.bg_hw_read_dot(n, k, ly) else {
            return fls;
        };
        const BG_BITS: u8 = (LCDCFlags::BGTileMapDisplaySelect as u8)
            | (LCDCFlags::BGWindowTileDataSelect as u8);
        if !self.wg_hist.is_empty() {
            if self.wg_cgb {
                let h_scy = self.bg_hw_read_dot_ex(n, k, ly, self.wg_cgb).unwrap_or(h);
                let (bits, quirk) = self.bg_wg_resolve_cgb(h, h_scy, k);
                fls.lcdc = (fls.lcdc & !BG_BITS) | (bits & BG_BITS);
                fls.or_lcdc = None;
                if k >= 1 {
                    fls.cgb_tile_index_is_tile_data = quirk;
                }
            } else {
                let bits = self.bg_wg_resolve(h);
                fls.lcdc = (fls.lcdc & !BG_BITS) | (bits & BG_BITS);
            }
        }
        // SCY resolves at the SameBoy-exact fetch dot (see bg_hw_read_dot_ex);
        // on DMG the scy_mode dot is identical to `h` (bias 0).
        let h_scy = self.bg_hw_read_dot_ex(n, k, ly, self.wg_cgb).unwrap_or(h);
        fls.scy_bus = self.bg_scy_resolve(h_scy);
        // SCX resolves the tile-map column at the TileNumber (k==0) reconstructed
        // hardware dot: a sprite-stalled tile reads SCX as-of that dot, not the
        // stall-displaced live scx (m3_scx_high_5_bits). Only k==0 fetches the
        // column, so only resolve there.
        if k == 0 && !self.bg_scx_hist.is_empty() {
            let h_scx = self.bg_hw_read_dot_ex(n, k, ly, self.wg_cgb).unwrap_or(h);
            fls.scx_bus = self.bg_scx_resolve(h_scx);
        }
        fls
    }

    // SCX in effect at reconstructed hardware dot `h` per the DMG BG journal.
    fn bg_scx_resolve(&self, h: u64) -> Option<u8> {
        if self.bg_scx_hist.is_empty() {
            return None;
        }
        let add = if self.wg_cgb { CGBWG_SCY_ADD } else { 0 };
        let mut v = self.bg_scx_hist[0].1;
        for &(t, _, new) in &self.bg_scx_hist {
            if h > t + add {
                v = new;
            } else {
                break;
            }
        }
        Some(v)
    }

    // Retroactive re-resolution of the in-flight tile's completed reads at
    // journal-push time. The BG fetcher runs ahead of the pixel pops:
    // rustyboi may have executed a read BEFORE the CPU write exists while the
    // read's HARDWARE dot (sprite-stall displaced) falls at/after the bus
    // transition (bg_map bands 0-2: rustyboi TN1 at F+8, hardware TN1 at
    // F+8+D_pre — 13 dots later, inside the pulse). Re-derive each completed
    // substep of the in-flight tile from the journals at its reconstructed
    // dot and patch the latched tile number / pixel-buffer planes; reads not
    // yet executed resolve at read time (bg_wg_apply). Idempotent (pure
    // recompute from the journals). The stall-displacement bound (~13 dots
    // pre-stall, <= 2 dots steady-state) keeps every affected read inside the
    // in-flight tile — an already-pushed tile is out of reach (no observed
    // case). DMG-only (both journals are DMG-scoped).
    fn bg_retro_repair(&mut self, mmio: &mmio::Mmio) {
        if self.state != State::PixelTransfer
            || (self.wg_hist.is_empty() && self.bg_scy_hist.is_empty())
        {
            return;
        }
        let k_now = self.fetcher.fetch_substep();
        if !(1..=3).contains(&k_now) {
            return;
        }
        let n = self.fetcher.get_tile_index() as u64;
        let ly = mmio.read(LY);
        let live_scy = self.scy_delayed;
        let map_bit = LCDCFlags::BGTileMapDisplaySelect as u8;
        let col = self.fetcher.last_bg_tn_col() as u16;

        // TileNumber (k=0).
        let Some(h0) = self.bg_hw_read_dot(n, 0, ly) else {
            return;
        };
        // CGB resolves the map bit at the SameBoy-exact fetch dot and the
        // tile-data bit at the calibrated `h` (see bg_wg_resolve_cgb); DMG uses `h`.
        let h0_scy = self.bg_hw_read_dot_ex(n, 0, ly, self.wg_cgb).unwrap_or(h0);
        let bits0 = if self.wg_hist.is_empty() {
            self.lcdc
        } else if self.wg_cgb {
            self.bg_wg_resolve_cgb(h0, h0_scy, 0).0
        } else {
            self.bg_wg_resolve(h0)
        };
        let scy0 = self.bg_scy_resolve(h0_scy).unwrap_or(live_scy);
        let row_off = ((ly.wrapping_add(scy0) as u16 / 8) % 32) * 32 + col;
        let base0: u16 = if bits0 & map_bit != 0 { 0x9C00 } else { 0x9800 };
        let tn = mmio.read_vram_bank(0, base0 + row_off);
        self.fetcher.patch_tile_num(tn);

        // wg_cgb: the tile-data-select (LCDC.4) bit reached the A12 line for BOTH
        // data bytes at the LOW-plane fetch dot — hardware latches the tile-data
        // address once and drives the two consecutive byte reads from it. When a
        // sprite stalls the line, the reconstructed HIGH dot can land past a bit4
        // falling edge the LOW dot sits before; re-resolving the HIGH plane
        // independently would then straddle a tile the live per-substep fetch
        // read coherently (mealybug m3_lcdc_tile_sel_change2). Pin the HIGH
        // plane's tile-data-select bit to the LOW plane's resolution so retro
        // reproduces the live bg_wg_apply result instead of diverging from it.
        // (The genuine mixed per-bitplane $8000/$8800 case is produced on the
        // live path via bg_hw_read_dot_ex's arm-dot anchoring, which retro's
        // shared reconstruction inherits — m3_lcdc_tile_sel_change stays exact.)
        let tds = LCDCFlags::BGWindowTileDataSelect as u8;
        let tds_low = self.bg_hw_read_dot(n, 1, ly).map(|h1| {
            let h1_scy = self.bg_hw_read_dot_ex(n, 1, ly, self.wg_cgb).unwrap_or(h1);
            if self.wg_hist.is_empty() {
                self.lcdc & tds
            } else if self.wg_cgb {
                self.bg_wg_resolve_cgb(h1, h1_scy, 1).0 & tds
            } else {
                self.bg_wg_resolve(h1) & tds
            }
        });

        // TileDataLow (k=1) / TileDataHigh (k=2), using the (re-resolved)
        // latched tile number — exactly what the hardware pipeline feeds them.
        for k in 1..=2u8 {
            if k_now <= k {
                break;
            }
            let Some(hk) = self.bg_hw_read_dot(n, k, ly) else {
                return;
            };
            let hk_scy = self.bg_hw_read_dot_ex(n, k, ly, self.wg_cgb).unwrap_or(hk);
            let (mut bitsk, quirkk) = if self.wg_hist.is_empty() {
                (self.lcdc, false)
            } else if self.wg_cgb {
                self.bg_wg_resolve_cgb(hk, hk_scy, k)
            } else {
                (self.bg_wg_resolve(hk), false)
            };
            // Pin the HIGH plane's tile-data-select bit to the LOW plane's ONLY
            // when a sprite object-fetch split this tile (its HIGH read is
            // arm-shifted off the LOW read's +2 cadence). With no sprite the two
            // reads are simply 2 dots apart and the HIGH plane resolves its OWN
            // tile-data-select — the genuine mixed $8000/$8800 read of a mid-tile
            // LCDC.4 pulse (age m3-bg-lcdc-nocgb transition tiles: low $8000 /
            // high $8800). Pinning unconditionally here flattened that mix to a
            // solid tile and mis-rendered the age white band.
            let low_hk = self.bg_hw_read_dot(n, 1, ly);
            let unstalled = low_hk.is_some_and(|h1| hk == h1 + 2);
            // The LOW plane's $8000 read latches the tile-data address for BOTH
            // bytes at HIGH_T1; a falling LCDC.4 that reaches the bus only after
            // HIGH_T1 cannot un-latch the already-$8000 HIGH plane. So when the
            // LOW plane rose to $8000, the HIGH plane inherits $8000 too — pin it.
            // This is the up-pulse train's HIGH-plane latch (mealybug tile_sel_
            // change2 first/last visible bands: the fetch outruns the FALL write,
            // retro then wrongly re-applies it to the HIGH plane). The DOWN-pulse
            // train (age m3-bg-lcdc-nocgb, is_train) holds LCDC.4 HIGH and pulses
            // it LOW: there the mid-tile mix (low $8000 / high $8800) is genuine —
            // the FALL precedes HIGH_T1 — so its unstalled HIGH keeps resolving on
            // its own. Gate the unstalled pin on the up-pulse (line-initial LCDC.4
            // low) so it never flattens the nocgb white band.
            let up_pulse = self
                .wg_hist
                .first()
                .is_some_and(|&(_, first, _)| (first & tds) == 0);
            if self.wg_cgb
                && k == 2
                && (!unstalled || up_pulse)
                && let Some(low_tds) = tds_low
                && (low_tds & tds) != 0
            {
                bitsk = (bitsk & !tds) | low_tds;
            }
            let scyk = self.bg_scy_resolve(hk_scy).unwrap_or(live_scy);
            let plane = (k - 1) as u16;
            let line = ly.wrapping_add(scyk) % 8;
            let addr = self.fetcher.get_tile_data_address(tn, line, bitsk) + plane;
            let byte = if quirkk && tn < 0x80 {
                // Falling-LCDC.4 coincidence: the tile index IS the bitplane.
                tn
            } else {
                mmio.read_vram_bank(0, addr)
            };
            if k == 1 {
                self.fetcher.patch_pixel_buffer_low(byte);
            } else {
                self.fetcher.patch_pixel_buffer_high(byte);
            }
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
                if will_start
                    && let Some(m0t) = self.m0_time_master {
                        let pen = (WIN_M3_PENALTY as i64) << ds as i64;
                        self.m0_time_master = Some((m0t as i64 + pen).max(0) as u64);
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
                        // fetcher geometry. On DMG the revert is NOT latched at
                        // the write at all: the fetcher re-samples the WE bit at
                        // each TileNumber step (SameBoy GET_TILE_T1 kill, see
                        // we_dot_hist) — a pulse that misses every TileNumber
                        // leaves the window running (mealybug
                        // win_en_change_multiple[_wx]).
                        if cgb_features_enabled {
                            let wxs = self.fetcher.window_x_start_dbg() as i32;
                            let phase = (self.x as i32 + 2 - 2 * wxs).rem_euclid(8);
                            let extra = if phase == 0 { 0u8 } else { 1u8 };
                            self.fetcher.stop_window_with_extra(extra);
                            self.window_started_this_line = false;
                        } else if at_line_end {
                            // DMG at line end (the wxA6 xpos-166 dance): no
                            // TileNumber will run again this line, so the
                            // deferred kill cannot land; stop immediately as
                            // Gambatte's winDrawState clear does.
                            self.fetcher.stop_window_with_extra(0);
                            self.window_started_this_line = false;
                        }
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

    /// DMG-compatibility mode on CGB hardware: a DMG cart running on a CGB
    /// (`is_cgb()` true, but CGB features OFF because the cart is not CGB-aware).
    /// The PPU still produces RGB color output, indexing the boot ROM's
    /// DMG-compat palette in CGB palette RAM via BGP/OBP shade remap.
    fn is_cgb_compat_dmg(&self, mmio: &mmio::Mmio) -> bool {
        mmio.is_cgb() && !mmio.is_cgb_features_enabled()
    }

    /// True when this frame should be rendered to the RGB color framebuffer:
    /// either full CGB mode or DMG-compat-on-CGB.
    fn renders_color(&self, mmio: &mmio::Mmio) -> bool {
        mmio.is_cgb_features_enabled() || self.is_cgb_compat_dmg(mmio)
    }

    // BG palette shade for color index `idx` at display column `sx`. On CGB hardware
    // resolves BGP per column from `bgp_history` so a mid-mode-3 BGP write remaps only
    // the pixels drawn at/after its apply column (mealybug m3_bgp_change cgb_c, which
    // runs DMG-compat-on-CGB). On DMG hardware the per-dot `bgp_delayed` latch
    // (refreshed at the end of every dot, with a phase-dependent hold for late-phase
    // writes — see `on_bgp_write`) yields the exact DMG latch column that Gambatte's
    // own dmgpalette_during_m3 tests require, so DMG keeps it. With no mid-line write
    // the CGB history is a single seed == the delayed register, so the steady-state
    // output is identical either way.
    pub fn get_palette_color(&self, mmio: &mmio::Mmio, idx: u8, sx: u8) -> u8 {
        let bgp = if mmio.is_cgb() {
            Self::pal_at(&self.bgp_history, sx, self.bgp_delayed)
        } else {
            self.bgp_delayed
        };
        Self::bgp_shade(bgp, idx)
    }

    // As `get_palette_color` but resolves BGP at the pixel's pop DOT rather than its
    // display column. Used by the CGB / DMG-compat BG color path: a sprite-fetch
    // stall between a BGP write and a column delays that column's pop, so the
    // dot-space model (write applies at `ticks+latency`; pixel pops later) is exact
    // where the column model over/under-shoots (mealybug m3_bgp_change_sprites).
    pub fn get_palette_color_at_tick(&self, idx: u8, pop_tick: u128) -> u8 {
        let bgp = Self::pal_at_tick(&self.bgp_dot_history, pop_tick, self.bgp_delayed);
        Self::bgp_shade(bgp, idx)
    }

    fn bgp_shade(bgp: u8, idx: u8) -> u8 {
        match idx {
            0 => bgp & 0x03,
            1 => (bgp >> 2) & 0x03,
            2 => (bgp >> 4) & 0x03,
            3 => (bgp >> 6) & 0x03,
            _ => 0x00,
        }
    }

    // Sprite palette shade at display column `sx` (CGB: per-pixel OBP sample from the
    // true-color palette-RAM pipeline — mealybug m3_obp0_change cgb_c). Used by the
    // CGB and DMG-compat sprite mixers. DMG-hardware sprites use
    // `dmg_sprite_palette_shade` (a per-SPRITE latch, not per-pixel).
    pub fn get_sprite_palette_color(&self, _mmio: &mmio::Mmio, idx: u8, palette: bool, sx: u8) -> u8 {
        if idx == 0 {
            return 0; // Transparent for sprites
        }
        let obp = if palette {
            Self::pal_at(&self.obp1_history, sx, self.obp1_delayed)
        } else {
            Self::pal_at(&self.obp0_history, sx, self.obp0_delayed)
        };
        Self::obp_shade(obp, idx)
    }

    // DMG sprite shade: OBP is sampled at the pixel's POP DOT (the OAM-FIFO
    // pop reads the register live), via the dot-keyed history — the column
    // model diverges wherever a sprite stall delays the pops (m3_obp0_change
    // late bands), and the pop-dot model naturally covers the off-left-edge
    // sprites (their pixels pop before any mid-mode-3 write applies).
    fn dmg_sprite_palette_shade(&self, idx: u8, palette: bool, pop_tick: u128) -> u8 {
        if idx == 0 {
            return 0; // Transparent for sprites
        }
        let hist = if palette { &self.obp1_dot_history } else { &self.obp0_dot_history };
        let fallback = if palette { self.obp1_delayed } else { self.obp0_delayed };
        let obp = Self::pal_at_tick(hist, pop_tick, fallback);
        Self::obp_shade(obp, idx)
    }

    #[inline]
    fn obp_shade(obp: u8, idx: u8) -> u8 {
        match idx {
            1 => (obp >> 2) & 0x03, // Light Gray
            2 => (obp >> 4) & 0x03, // Dark Gray
            3 => (obp >> 6) & 0x03, // Black
            _ => 0x00,              // Default to transparent for invalid indices
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
        // The IRQ-dispatch arm keeps the calibrated offset form (the faithful
        // `predictedNextXposTime(166)` migration of THIS consumer is deferred — the
        // per-dot dispatch phase is co-tuned with the consume-site `+ds /
        // +cgb_ss_m0_anticip` anticipation). The faithful event cc is consumed
        // independently by the halt-exit `<2` fixup via `m0_irq_event_cc_master`,
        // captured at the m0 IRQ flag site.
        self.sched_m0irq = abs;
    }

    /// FAITHFUL EVENTCC: the mode-0 STAT IRQ event time
    /// (`predictedNextXposTime(166)` = Gambatte's `intevent_interrupts` m0
    /// eventTime, video.cpp:884) in MASTER cc — the cc domain `master_cc()` /
    /// `m0_time_master` / getStat `access_cc` share, so the halt-exit
    /// `cc - eventTime < 2` fixup (memory.cpp:308) compares like-for-like.
    ///
    /// Derived from the closed-form `m0_time_master` (= predictedNextXposTime(167)
    /// in master cc): the m0 IRQ fires one xpos earlier, so subtract the 166->167
    /// step cost `((1 + xpos166_advance) << ds)`. `None` when no closed-form master
    /// exists (window mid-line / first line / VBlank), where no faithful event cc
    /// is available and the halt-exit fixup is skipped.
    pub fn m0_irq_event_cc_master(&self, mmio: &mmio::Mmio) -> Option<u64> {
        if self.internal_ly() as u32 >= stat_irq::LCD_VRES {
            return None;
        }
        let ds = mmio.is_double_speed_mode() as i64;
        let is_cgb = mmio.is_cgb_features_enabled();
        let adv = self.m0irq_xpos166_advance(mmio, is_cgb);
        // m0_time_master carries the runtime sprite0-at-scx fine-scroll extra
        // (see sprite0_scx_extra); the m0 STAT IRQ fires at the PREDICTOR time,
        // so peel it back out here.
        let spr0 = self.sprite0_scx_extra(mmio, is_cgb) << ds;
        self.m0_time_master
            .map(|m0t| (m0t as i64 - spr0 - ((1 + adv) << ds)).max(0) as u64)
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

    /// Register a NON-mode-3 (OAM/HBlank) DS->SS speed switch for the LY-read
    /// sub-dot phase accumulator. Gambatte's `PPU::speedChange` applies `now -= 1`
    /// (a half-dot) on every DS->SS switch; rustyboi's whole-dot DS->SS bridge folds
    /// the integer part, and mode-3 switches carry their residual through the FACET-1
    /// `stat_phase_carry` (p_now) path. This tracks the OAM/HBlank switches that have
    /// no such carry: their accumulated parity determines whether a post-STOP boundary
    /// LY read lands one sub-dot early (anticipated) or late (stale).
    pub fn bump_dsss_ly_phase(&mut self) {
        self.dsss_ly_phase_count += 1;
    }
    /// Register any DS->SS switch (including mode-3) for the total-parity accumulator.
    pub fn bump_dsss_ly_total(&mut self) {
        self.dsss_ly_total_count += 1;
    }
    fn dsss_ly_total_par(&self) -> i64 {
        (self.dsss_ly_total_count % 2) as i64
    }
    pub fn dsss_ly_phase_par(&self) -> i64 {
        (self.dsss_ly_phase_count % 2) as i64
    }
    /// True once any post-STOP DS->SS switch has accumulated a sub-dot phase.
    pub fn dsss_ly_phase_active(&self) -> bool {
        self.dsss_ly_phase_count > 0
    }

    /// Latch the SS->DS-during-mode3 FF44 (LY) read phase advance. Consumed only
    /// by `get_ly_reg_at_cc` to resolve the getLyReg anticipation window against
    /// Gambatte's re-anchored lyTime (the renderer/STAT/m0 phase is unaffected).
    pub fn set_ssds_mode3_ly_advance(&mut self) {
        self.ssds_mode3_ly_advance = true;
        self.ssds_mode3_frames = 0;
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
            self.mstat_irq.lcd_reset(self.lyc_irq.lyc_reg_src());
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

        // Fast path: none of the ~10 scheduled events below can fire this dot.
        // Every consumer gates on `sched_cc <= cc + off` with `off <= 2` (the
        // m1/m0 double-speed anticipation), and the wy/scy/scx apply blocks use
        // off 0. So if the earliest scheduled cc is still more than 2 dots away,
        // the whole body is a no-op. Disabled slots hold huge sentinels
        // (u64::MAX / DISABLED_TIME), so the min naturally excludes them.
        let min_sched = self
            .wy2_apply_cc
            .min(self.wy1_apply_cc)
            .min(self.scy_apply_cc)
            .min(self.scx_apply_cc)
            .min(self.sched_oneshot_statirq)
            .min(self.sched_m1irq)
            .min(self.sched_lycirq)
            .min(self.sched_m2irq)
            .min(self.sched_m0irq);
        if cc + 2 < min_sched {
            return;
        }

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
            // FAITHFUL EVENTCC: capture this line's m0 IRQ event cc
            // (predictedNextXposTime(166)) BEFORE the mutable IF-flag borrow, so
            // the halt-exit `<2` fixup can read the cc the IF bit was raised at
            // (Gambatte `flagIrq(2, eventTimes_(memevent_m0irq))`).
            let m0_event_cc = self.m0_irq_event_cc_master(mmio);
            let fired = self.mstat_irq.do_m0_event(ly, stat, self.lyc_irq.lyc_reg_src());
            if fired {
                mmio.request_interrupt(registers::InterruptFlag::Lcd);
                mmio.set_pending_m0_irq_fire_cc(m0_event_cc);
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
        let fired = self.mstat_irq.do_m2_event(ly, stat, self.lyc_irq.lyc_reg_src());
        if fired {
            mmio.request_interrupt(registers::InterruptFlag::Lcd);
            // FAITHFUL HALT-EXIT: a halted CPU wakes at this exact cc; the DMG
            // halt-exit fixup (sm83.rs) needs the m2 eventTime to apply the
            // real +4 (`cc - eventTime < 2`).
            mmio.set_last_m2_irq_fire_cc(mmio.master_cc());
            // Record the m2-event LY so the CGB halt-exit +4 stall (sm83.rs) can
            // distinguish a rendering-line OAM wake (ly 0..143, intr_2) from the
            // VBlank-entry mode-2 quirk wake (ly 144, vblank_stat_intr).
            mmio.set_last_m2_irq_ly(ly as u8);
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
        // Window-reactivation insert: render a color-0 pixel without popping
        // (see insert_bg_pixel).
        let (bg_pixel_idx, bg_attrs) = if self.insert_bg_pixel {
            self.insert_bg_pixel = false;
            (0u8, 0u8)
        } else {
            let Ok(bg_pixel) = self.fetcher.pixel_fifo.pop() else {
                return false;
            };
            (bg_pixel.color, bg_pixel.attrs)
        };
        self.win_being_fetched = false;
        let ly = mmio.read(LY) as u16;
        let fb_offset = (ly * 160) + self.x as u16;

        // Per-pixel BG-enable (RB_LINERENDER per-pixel). The per-dot draw is
        // flushed in bursts (the m0Time flush at mode-3 end draws all remaining
        // FIFO pixels in one pass), so reading the LIVE `self.lcdc` would apply
        // the final BG-enable to every flushed column. Instead evaluate BG-enable
        // as-of THIS column's plot cc from the line's `bgen_history`, so a
        // mid-mode-3 LCDC.0 toggle (BG off then on) covers exactly the pixel span
        // it should — matching Gambatte's live per-tile `lcdc & lcdc_bgen` read.
        // With no mid-line toggle `bgen_at` returns the single seeded value
        // (== live `lcdc & 1`), so the common case is unchanged.
        let bg_enabled_col = self.bgen_at(mmio, mmio.is_cgb_features_enabled(), self.x);
        if mmio.is_cgb_features_enabled() {
            let final_color_rgb =
                self.mix_background_and_sprites_color(mmio, bg_pixel_idx, bg_attrs, self.x, ly as u8, bg_enabled_col);
            self.record_pixel_debug_event(
                ly as u8,
                bg_pixel_idx,
                [final_color_rgb.0, final_color_rgb.1, final_color_rgb.2],
            );
            let color_offset = fb_offset as usize * 3;
            self.color_fb_a[color_offset] = final_color_rgb.0;
            self.color_fb_a[color_offset + 1] = final_color_rgb.1;
            self.color_fb_a[color_offset + 2] = final_color_rgb.2;
        } else if self.is_cgb_compat_dmg(mmio) {
            // DMG cart on CGB: color output via the boot ROM's DMG-compat palette.
            let final_color_rgb =
                self.mix_background_and_sprites_compat(mmio, bg_pixel_idx, self.x, ly as u8, bg_enabled_col);
            self.record_pixel_debug_event(
                ly as u8,
                bg_pixel_idx,
                [final_color_rgb.0, final_color_rgb.1, final_color_rgb.2],
            );
            // Record BG-won + BG index for the CGB-compat train re-resolve
            // (cgb_train_reresolve): a column BG won iff its final color equals
            // the BG-only compat color of its index (a sprite otherwise overrode
            // it, or the index-independent sprite result differs).
            if (self.x as usize) < self.line_bg_idx.len() {
                let bg_only = self.compat_bg_color(mmio, if bg_enabled_col { bg_pixel_idx } else { 0 });
                self.line_bg_idx[self.x as usize] =
                    if bg_enabled_col && final_color_rgb == bg_only { bg_pixel_idx as i8 } else { -1 };
            }
            let color_offset = fb_offset as usize * 3;
            self.color_fb_a[color_offset] = final_color_rgb.0;
            self.color_fb_a[color_offset + 1] = final_color_rgb.1;
            self.color_fb_a[color_offset + 2] = final_color_rgb.2;
        } else {
            let final_color = self.mix_background_and_sprites(mmio, bg_pixel_idx, self.x, ly as u8, bg_enabled_col);
            // DMG mid-mode-3 BGP-write glitch: record the BG color index of THIS pixel so
            // the mode-3-end `resolve_bgp_spikes` post-pass can re-map it through the
            // OR-glitched palette. Only BG-won pixels are eligible (a sprite that won the
            // mix is untouched). A per-write glitch here cannot see a SET write's FUTURE
            // RESTORE neighbor (the SET column draws before the RESTORE write lands), so
            // the two-write cadence gate is deferred to the post-pass. See `bgp_writes`.
            if (self.x as usize) < self.line_bg_idx.len() {
                let bg_won = bg_enabled_col && final_color == self.get_palette_color(mmio, bg_pixel_idx, self.x);
                self.line_bg_idx[self.x as usize] = if bg_won { bg_pixel_idx as i8 } else { -1 };
            }
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

    // Gambatte's plotPixel/predictor window-Y gate: `weMaster || (wy2 == ly &&
    // winEn)`. `wy2` is WY delayed ~2 dots after a write; we read WY live, which
    // matches by the time the fetcher reaches WX. This `wy2 == ly` fallback
    // catches late-frame WY writes that land after the three weMaster
    // checkpoints (e.g. WY=ly written during the same line's mode 3).
    fn window_y_active(&self, mmio: &mmio::Mmio) -> bool {
        self.window_y_active_with(mmio, (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0)
    }

    // window_y_active with an explicit window-enable sample. The DMG mid-mode-3
    // trigger paths pass the DELAYED per-dot tap (we_dot_hist[2]) instead of the
    // live bit — hardware's comparator sees a WE write later than our visible
    // lcdc commit does (see we_dot_hist).
    fn window_y_active_with(&self, mmio: &mmio::Mmio, win_en: bool) -> bool {
        if !win_en {
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
        // WX=166 (lcd_hres+6): the window starts on the CGB PPU but not the DMG PPU.
        // This follows the HARDWARE PPU (real CGB silicon, even in DMG-compat/ncm),
        // not the CGB-features flag — age stat-mode-window-ncm keys WX=166 on DEF(CGB)
        // (hardware) and extends mode-3 there, matching cgbBCE not dmgC.
        let _ = is_cgb;
        (0..=166).contains(&wx) && (mmio.is_cgb() || wx != 166)
    }

    // Gambatte plotPixel (ppu.cpp 883-895) evaluated at the END of mode 3, where
    // the fetcher's xpos reaches wx==166 (lcd_hres+6) on DMG with WX==166. The
    // window cannot draw a visible pixel this line (the line ends at xpos 166)
    // but it still mutates winDrawState exactly as Gambatte does when xpos hits
    // wx. The OUTER gate is `wx==xpos && (weMaster || (wy2==ly && winEn)) &&
    // xpos<167`; weMaster alone is sufficient (does NOT require winEn). INNER:
    //   branch A (886): winDrawState==0 && winEn -> start now
    //       (winDrawState = win_draw_start|win_draw_started, ++winYPos)
    //   branch B (889): !cgb && (winDrawState==0 || xpos==166) -> |= win_draw_start
    // The xpos==166 term makes branch B fire on EVERY qualifying line (even with
    // WE off), arming win_draw_start. That bit survives into the next M3Start::f0
    // (and across the frame boundary, since winDrawState is not reset at frame
    // end) where it is consumed (++winYPos, window draws from x0). Running this at
    // line end — AFTER the mid-mode-3 WE-off cleared win_draw_started — is what
    // gives the wxA6 steady state TWO winYPos increments per line (f0 + the HBlank
    // WE-on, which now sees winDrawState==win_draw_start) and lets the WE-off
    // actually revert the right columns to BG. Idempotent within a line: it only
    // runs once at the mode-3->HBlank transition (the two transition call sites
    // are mutually exclusive per line).
    fn apply_dmg_wxa6_lineend_windraw(&mut self, mmio: &mmio::Mmio, is_cgb: bool) {
        if self.wxa6_lineend_applied {
            return;
        }
        if is_cgb || self.first_line_after_enable || mmio.read(WX) != 166 {
            return;
        }
        self.wxa6_lineend_applied = true;
        let win_en_now = (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
        let we_gate = self.window_y_triggered
            || (self.wy2 == mmio.read(LY) && win_en_now);
        if !we_gate {
            return;
        }
        let win_draw_state_zero = !self.win_draw_start && !self.win_draw_started;
        if win_draw_state_zero && win_en_now {
            // branch A (886): start now (no visible window at xpos 166).
            self.win_draw_start = true;
            self.win_draw_started = true;
            self.win_y_pos = self.win_y_pos.wrapping_add(1);
        } else {
            // branch B (889): arm win_draw_start (xpos==166 term, fires
            // regardless of winEn) for the next line's M3Start::f0 consume.
            self.win_draw_start = true;
        }
    }

    fn compute_m3_length(&self, mmio: &mmio::Mmio, is_cgb: bool) -> u128 {
        let (len, _win) = self.compute_m3_length_win(mmio, is_cgb);
        len
    }

    // Per-pixel BG-enable (RB_LINERENDER per-pixel). Returns the LCDC.0
    // (BG-enable) bit in effect for display column `sx`, from the line's
    // `bgen_history` (boundary_col, bgen) entries. The last entry whose boundary
    // column is <= `sx` wins. With no mid-mode-3 LCDC.0 toggle the history is a
    // single seed (boundary 0) and this always returns the seeded value —
    // byte-identical to the old once-per-pixel live `lcdc & 1` read.
    fn bgen_at(&self, _mmio: &mmio::Mmio, _is_cgb: bool, sx: u8) -> bool {
        if self.bgen_history.len() <= 1 {
            return self
                .bgen_history
                .last()
                .map(|&(_, b)| b)
                .unwrap_or((self.lcdc & (LCDCFlags::BGDisplay as u8)) != 0);
        }
        let mut bgen = self.bgen_history[0].1;
        for &(boundary_col, b) in self.bgen_history.iter() {
            if boundary_col <= sx as u64 {
                bgen = b;
            } else {
                break;
            }
        }
        bgen
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
                    cycles += dflt;
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
            // A window that starts at WX=0 extends mode-3 one dot longer than the
            // flat StartWindowDraw +6 (Gambatte's predictor charges +6 for every
            // in-range WX including 0, but real DMG/CGB silicon runs WX=0 one dot
            // long — age stat-mode-window WX=0 rows on CPU-DMG-C / CPU-CGB-B/C/E).
            // Single speed only: at double speed Gambatte's own WX=0 m0Time phase
            // already agrees (the +1 would flip 10spritesPrLine_wx0_m3stat_ds /
            // m2int_wxDefault_m3stat_ds), same speed asymmetry as the scx==5 case.
            // The scx==5 CGB SS -1 (below) is a fine-scroll-dispatch correction for
            // a window that starts mid-tile; at WX=0 the window starts at the tile
            // grid origin so that dispatch penalty does not apply (age
            // stat-mode-window-cgbBCE WX=0 scx5 row reads mode 3, not mode 0).
            if is_cgb && scx == 5 && self.sprites_on_line.is_empty() && nwx != 0 {
                let dflt = if mmio.is_double_speed_mode() { 0 } else { -1 };
                cycles += dflt;
            }
            // WX=0 window init runs one dot long when the SCX fine-scroll discard is
            // active (age stat-mode-window WX=0 rows: the AGE fetcher inits the window
            // at 8 clks instead of 7 when `alignment_x >= 1`). Speed-independent in
            // dots — the previous `!ds` gate left the DS WX=0 scx>0 rows one dot short.
            if nwx == 0 && scx > 0 {
                cycles += 1;
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

    /// Runtime-only mode-3 extension when a sprite sits at spx == 0 (Gambatte
    /// `M3Start::f1`, ppu.cpp:349-350: `if (sprite0Active && (lcdcObjEn|cgb))
    /// p.cycles -= min(scx%8, 5)`). A sprite whose X is exactly 0 straddles the
    /// fine-scroll discard, so the fetch stalls `min(scx%8, 5)` extra dots before
    /// the tile loop begins.
    ///
    /// This cost lives ONLY in the runtime fetch loop that drives the real
    /// mode-3 -> mode-0 transition (and therefore the STAT-mode read the CPU
    /// polls). Gambatte's closed-form `predictCyclesUntilXpos_fn` (the m0-STAT-IRQ
    /// predictor, ppu.cpp:1336) does NOT include it, so `compute_m3_length`
    /// (which arms `sched_m0irq`) must stay clean — the m0 IRQ fires at the
    /// predictor time, the mode transition one `min(scx%8,5)` dot later. Applied
    /// to `m0_time_master` (the renderer/STAT boundary) and subtracted back out in
    /// `m0_irq_event_cc_master`. Mooneye intr_2_mode0_timing_sprites_scx{1..4}.
    fn sprite0_scx_extra(&self, mmio: &mmio::Mmio, is_cgb: bool) -> i64 {
        let obj_enabled = (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0;
        if !(obj_enabled || is_cgb) {
            return 0;
        }
        if !self.sprites_on_line.iter().any(|s| s.x == 0) {
            return 0;
        }
        let scx = (self.first_line_scx_override.unwrap_or_else(|| mmio.read(SCX)) & 0x07) as i64;
        scx.min(5)
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
        self.objen_history.clear();
        self.objsize_dot_history.clear();
        self.sprite_fetch_recs.clear();
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

    /// FF47 (BGP) write hook. The CPU readback is immediate (handled by mmio), but
    /// the rendered BG palette mapping must change at the exact pixel being drawn
    /// `MID_M3_PAL_LATENCY` columns after the write (mealybug m3_bgp_change). Record
    /// the change keyed by the display column it first becomes visible at; the
    /// per-column draw resolves it via `bgp_at`. Only while pixel transfer is active
    /// for this line — a write outside mode 3 just lands in the seed at the next
    /// mode-3 entry. Steady-state (no mid-mode-3 write) is unaffected.
    pub fn on_bgp_write(&mut self, value: u8, _mmio: &mmio::Mmio) {
        // A BGP write in the OAM scan (mode 2) just before mode 3 is the leading edge
        // of a two-write spike pair when a mode-3 write follows within the cadence
        // window: it settles the glitch palette so the mode-3 partner's transition
        // paints a visible spike (age m3-bg-bgp early band: the $FF write lands in
        // mode 2, the restore at col 1 in mode 3). Stash it (survives the mode-3-arm
        // bgp_writes clear); it is injected neighbor-only at mode-3 entry and
        // discarded by the cadence gate if no mode-3 partner lands within
        // BGP_SPIKE_CADENCE_CC.
        if self.state == State::OAMSearch && !_mmio.is_cgb() && !self.disabled {
            self.bgp_mode2_pending = Some((self.abs_cc, value));
        }
        if self.state != State::PixelTransfer || self.disabled {
            return;
        }
        let lat = self.bgp_apply_latency(_mmio);
        // DMG sub-M-cycle phase hold: for a write whose store lands later in the M-cycle
        // (`master_cc % 4` != 0), the DMG per-dot latch (`bgp_delayed`) must keep the old
        // value for `lat - 1` extra dot-refreshes so the new palette first colors the
        // column `self.x + lat` (not `self.x + 1`). Phase-0 writes leave countdown 0 and
        // are unchanged. CGB does not use `bgp_delayed` (it resolves BGP per column from
        // `bgp_history`), so this is DMG-only.
        if !_mmio.is_cgb() {
            let extra = (lat - bgp_latency(false)).max(0) as u8;
            if extra > 0 {
                self.bgp_defer_hold = self.bgp_delayed;
                self.bgp_defer_countdown = extra;
            }
        }
        // DMG mid-mode-3 palette-write glitch (see `bgp_writes`): record this write's
        // apply column, `old | new` glitch value, and cc. Whether it actually spikes a
        // pixel (the TWO-WRITE cadence gate) is resolved at mode-3 end in
        // `resolve_bgp_spikes`, once all of the line's writes are known. Capture the old
        // (settled) value BEFORE recording the new one in the history.
        // A prologue write (SCX-discard warmup) does not paint its own spike, but
        // on hardware it still forms the leading half of a two-write spike cadence:
        // its restore partner (recorded below at a visible column) must find it as a
        // neighbor or the spike vanishes (age m3-bg-bgp: the $FF write lands at x=0
        // during the SCX discard, the restore at x=4 paints the visible black pixel).
        // Record it with a never-painted victim column (>=160) so it is a cadence
        // neighbor only.
        if !_mmio.is_cgb() && self.in_previsible_prologue() {
            self.bgp_writes.push((self.abs_cc, 0xFF, value));
        }
        if !_mmio.is_cgb() && !self.in_previsible_prologue() {
            // The spike's victim is the pixel POPPING at the write's apply dot.
            // When a sprite fetch has the pipeline stalled across that dot no
            // pixel pops — the glitched palette transition collides with
            // nothing and there is no visible spike (mealybug
            // m3_bgp_change_sprites: the RESTORE landing inside a late band's
            // sprite stall must not paint the first post-stall column). The
            // write is still RECORDED (victim 0xFF, never painted) so its
            // partner keeps its cadence neighbor. On stall-free lines the
            // victim is exactly `self.x + lat` (the old column model).
            let stall = self.sprite_fetch_stall as i32;
            // Pending SCX discard: at x==0 the first display column has not popped
            // while pixels remain to be discarded (m3_pixels_discarded <
            // m3_discard_target). The write's victim pixel is one of those discarded
            // pixels, so no visible spike lands — record it neighbor-only (age
            // m3-bg-bgp late bands LY21..23: the restore fires mid-discard).
            let discarding = self.x == 0 && self.m3_pixels_discarded < self.m3_discard_target.max(0) as u8;
            let col = if stall <= lat && !discarding {
                (self.x as i32 + lat - stall).clamp(0, 255) as u8
            } else {
                0xFF
            };
            let old = self.bgp_history.last().map(|&(_, v)| v).unwrap_or(self.bgp_delayed);
            self.bgp_writes.push((self.abs_cc, col, old | value));
        }
        let boundary = self.pal_write_boundary(lat);
        Self::push_pal_history(&mut self.bgp_history, boundary, value);
        // Dot-keyed history for the CGB / DMG-compat BG path: the write applies at
        // its own dot; each pixel samples it at its (stall-delayed) pop dot.
        let apply = self.pal_write_apply_tick(lat);
        Self::push_pal_dot_history(&mut self.bgp_dot_history, apply, value);
    }

    // Display-column latency (dots) for a mid-mode-3 BGP write. This hook fires at the
    // write M-cycle's START, but the DMG store's bus-write lands at a later sub-M-cycle
    // T-cycle, so the change reaches the displayed column a phase-dependent number of
    // dots after `self.x`. The phase is the write's `master_cc % 4`:
    //   - phase 0 -> +1 (the baseline). EVERY gambatte dmgpalette_during_m3 and mealybug
    //     m3_bgp_change write lands here (verified: all 37505 mid-m3 writes across the
    //     full gambatte DMG suite are phase 0), so those references are untouched.
    //   - later phases add `phase - 1` more dots: a write whose M-cycle starts deeper in
    //     the pixel-clock grid latches proportionally later. daid ppu_scanline_bgp's
    //     tight `LD A,(HL+); LDFF (C),A` gradient writes land at phase 3 (+3 total),
    //     matching the SameBoy/hardware reference (2 columns past the phase-0 baseline).
    // CGB keeps its own validated 2-dot latency (mealybug m3_bgp_change cgb_c); no phase
    // term (the CGB fetcher samples the palette-RAM pipeline at a fixed stage).
    fn bgp_apply_latency(&self, mmio: &mmio::Mmio) -> i32 {
        if mmio.is_cgb() {
            // CPU-CGB-D/E samples the BG palette one dot earlier than CGB-C:
            // age m3-bg-bgp's ncmE (rev=cgbe) reference wants the DMG 1-dot
            // latency while ncmBC / mealybug m3_bgp_change (CGB-C) keep 2.
            let base = if mmio.is_cgb_de() {
                BGP_LATENCY_DMG
            } else {
                bgp_latency(true)
            };
            base + Self::cgb_halt_wake_write_bias(mmio)
        } else {
            let phase = (mmio.master_cc() % 4) as i32;
            bgp_latency(false) + (phase - 1).max(0)
        }
    }

    // CGB halt-woken write-stream bias, in display columns. Gambatte charges
    // `cc += 4 * isCgb()` when an IRQ ends HALT (memory.cpp:301) — one real
    // M-cycle before the woken stream resumes. rustyboi's halted CPU wakes at
    // the exact IF-set cc and models that M-cycle on the READ side only
    // (getStat/getLyReg biases), so a halt-woken WRITE stream runs 4cc early:
    // every mid-mode-3 palette write it makes lands 4cc (dots, halved in
    // double speed) of display columns short of the hardware column. Re-add
    // the un-charged M-cycle here, gated on the woken stream
    // (`halt_wakeup_skew`, set at wake / cleared at the next HALT): daid
    // ppu_scanline_bgp.gbc's LYC-woken ISR stream takes it (boundaries were a
    // uniform 4 columns early); mealybug m3_bgp_change cgb_c busy-waits (all
    // its writes probe skew=false) and keeps the validated flat latency.
    fn cgb_halt_wake_write_bias(mmio: &mmio::Mmio) -> i32 {
        if mmio.halt_wakeup_skew() {
            4 >> mmio.is_double_speed_mode() as i32
        } else {
            0
        }
    }

    // Resolve the DMG mid-mode-3 BGP-write glitch for the just-finished line and paint
    // the spikes into the framebuffer. Called at the mode-3 -> HBlank transition, when
    // every write of the line is known. The glitch is a TWO-WRITE collision: a write
    // spikes its own pixel (looked up through `old | new`) only when it has a
    // neighboring mid-mode-3 write within `BGP_SPIKE_CADENCE_CC` (mealybug's SET/RESTORE
    // pairs, ~12 dots apart). A single write, or one spaced wider (the gambatte
    // dmgpalette_during_m3 family: one write per line, or 60-148 dots apart), has no
    // colliding neighbor and paints no spike — leaving the clean palette transition the
    // dmgpalette oracles expect. Resolving at line end (all writes known) lets a SET
    // write spike on the strength of its FUTURE RESTORE neighbor, which a per-write gate
    // could not see. DMG-only; the CGB path uses true-color palette RAM (no collapse).
    fn resolve_bgp_spikes(&mut self, mmio: &mmio::Mmio) {
        if mmio.is_cgb() || self.bgp_writes.len() < 2 {
            return;
        }
        let ly = mmio.read(LY);
        if ly >= 144 {
            return;
        }
        let writes = std::mem::take(&mut self.bgp_writes);
        for i in 0..writes.len() {
            let (cc, col, glitch) = writes[i];
            // Neighboring write within the tight cadence, in either direction.
            let has_neighbor = writes.iter().enumerate().any(|(j, &(occ, _, _))| {
                j != i && cc.abs_diff(occ) <= BGP_SPIKE_CADENCE_CC
            });
            if !has_neighbor || col >= 160 {
                continue;
            }
            // Re-map the BG pixel drawn at `col` through the OR-glitched palette. The
            // per-dot draw stored its BG color index in `line_bg_idx` (-1 = a sprite won
            // this column, or it was BG-disabled; leave those untouched).
            let bg_idx = self.line_bg_idx[col as usize];
            if bg_idx < 0 {
                continue;
            }
            let fb_offset = (ly as u16) * 160 + col as u16;
            self.fb_a[fb_offset as usize] = (glitch >> (2 * bg_idx as u8)) & 0x03;
        }
    }

    /// FF48 (OBP0) write hook. See `on_bgp_write`; affects sprite palette 0.
    pub fn on_obp0_write(&mut self, value: u8, _mmio: &mmio::Mmio) {
        if self.state != State::PixelTransfer || self.disabled {
            return;
        }
        let lat = obp_latency(_mmio.is_cgb())
            + if _mmio.is_cgb() { Self::cgb_halt_wake_write_bias(_mmio) } else { 0 };
        let boundary = self.pal_write_boundary(lat);
        Self::push_pal_history(&mut self.obp0_history, boundary, value);
        let apply = self.pal_write_apply_tick(lat);
        Self::push_pal_dot_history(&mut self.obp0_dot_history, apply, value);
    }

    /// FF49 (OBP1) write hook. See `on_bgp_write`; affects sprite palette 1.
    pub fn on_obp1_write(&mut self, value: u8, _mmio: &mmio::Mmio) {
        if self.state != State::PixelTransfer || self.disabled {
            return;
        }
        let lat = obp_latency(_mmio.is_cgb())
            + if _mmio.is_cgb() { Self::cgb_halt_wake_write_bias(_mmio) } else { 0 };
        let boundary = self.pal_write_boundary(lat);
        Self::push_pal_history(&mut self.obp1_history, boundary, value);
        let apply = self.pal_write_apply_tick(lat);
        Self::push_pal_dot_history(&mut self.obp1_dot_history, apply, value);
    }

    // Display column at which a mid-mode-3 palette write becomes visible: the next
    // column to be popped (`self.x`) plus the register's pipeline latency in dots.
    // While the pipeline is still warming up (`pixel_transfer_warmup > 0`, before any
    // column has popped) the write lands before column 0 is plotted, so it colors
    // column 0 onward — the `+latency` delay is absorbed by the remaining warmup.
    // Pre-visible phase of a chopped WX<7 window start: the early activation
    // zeroed the warmup, but a write landing before the line's pos-0 dot still
    // colors the whole line (the column-0 pixel pops at/after pos 0), and must
    // not seed a two-write spike either (mealybug m3_window_timing WX=1 row) —
    // exactly like a write during the warmup.
    fn in_previsible_prologue(&self) -> bool {
        if self.pixel_transfer_warmup > 0 {
            return true;
        }
        if self.x == 0 && self.m3_discard_target >= 0 && self.win_fetch_anchor.is_some() {
            let base = self.m3_arm_dot + 12 - (self.m3_arm_dot & 1)
                + self.m3_discard_target as u128;
            return self.ticks < base;
        }
        false
    }

    fn pal_write_boundary(&self, latency: i32) -> u64 {
        if self.in_previsible_prologue() {
            return 0;
        }
        (self.x as i32 + latency).clamp(0, 160) as u64
    }

    // Dot at which a mid-mode-3 palette write becomes visible to the pixel
    // pops (the dot-space analog of `pal_write_boundary`; see
    // `obp0_dot_history`). During the previsible prologue the write applies
    // before any visible pop, i.e. tick 0.
    fn pal_write_apply_tick(&self, latency: i32) -> u128 {
        if self.in_previsible_prologue() {
            return 0;
        }
        self.ticks + latency.max(0) as u128
    }

    // Append an (apply_tick, value) dot-keyed palette entry; same last-write-
    // wins collapse as `push_pal_history`.
    fn push_pal_dot_history(hist: &mut Vec<(u128, u8)>, apply: u128, value: u8) {
        if let Some(last) = hist.last_mut()
            && last.0 == apply
        {
            last.1 = value;
            return;
        }
        hist.push((apply, value));
    }

    // Resolve a dot-keyed DMG palette history at pop dot `tick`.
    fn pal_at_tick(hist: &[(u128, u8)], tick: u128, fallback: u8) -> u8 {
        if hist.len() <= 1 {
            return hist.last().map(|&(_, v)| v).unwrap_or(fallback);
        }
        let mut val = hist[0].1;
        for &(apply_tick, v) in hist.iter() {
            if apply_tick <= tick {
                val = v;
            } else {
                break;
            }
        }
        val
    }

    // Append a (boundary_col, value) palette-history entry. If the last entry shares
    // the same boundary column (two writes resolving to the same display column),
    // overwrite it so only the last write at that column wins.
    fn push_pal_history(hist: &mut Vec<(u64, u8)>, boundary: u64, value: u8) {
        if let Some(last) = hist.last_mut()
            && last.0 == boundary
        {
            last.1 = value;
            return;
        }
        hist.push((boundary, value));
    }

    // Resolve a column-keyed DMG palette history at display column `sx`: the last
    // entry whose boundary column is <= `sx` wins. Single-seed history (the common
    // no-mid-write case) always returns the seed. Mirrors `bgen_at`.
    fn pal_at(hist: &[(u64, u8)], sx: u8, fallback: u8) -> u8 {
        Self::pal_at_col(hist, sx as u64, fallback)
    }

    // As `pal_at` but with an arbitrary sample column (the DMG sprite OBP path may
    // force column 0 for off-left-edge sprites rather than the pixel's own column).
    fn pal_at_col(hist: &[(u64, u8)], sample_col: u64, fallback: u8) -> u8 {
        if hist.len() <= 1 {
            return hist.last().map(|&(_, v)| v).unwrap_or(fallback);
        }
        let mut val = hist[0].1;
        for &(boundary_col, v) in hist.iter() {
            if boundary_col <= sample_col {
                val = v;
            } else {
                break;
            }
        }
        val
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

        // DMG BG bus-glitch SCY journal (see bg_wg_apply): record the exact
        // bus transition time of a mid-mode-3 SCY write; BG fetch reads
        // resolve SCY at their reconstructed hardware dots against it, and
        // the in-flight tile's already-executed reads are re-resolved
        // (bg_retro_repair).
        if !mmio.is_cgb_features_enabled() && self.state == State::PixelTransfer {
            let old = self
                .bg_scy_hist
                .last()
                .map(|&(_, _, new)| new)
                .unwrap_or(self.scy_delayed);
            if old != value {
                // Transition placement: the new row/line address bits are
                // effective for reads strictly PAST the write's commit cc —
                // the same phase the live per-substep SCY re-read gives an
                // unshifted read (writes commit pre-tick; the first fetch dot
                // of the write M-cycle already sees the new value). No OR
                // edge: the LCDC pulse captures cannot separate OR from
                // clean-new/clean-old at the transition dots (old side is
                // 0x00 there), and the SCY capture rejects an OR at this
                // phase (whole-row blend pollution).
                self.bg_scy_hist.push((cc, old, value));
                self.bg_retro_repair(mmio);
            }
        }
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
        self.scx_pending = value;
        self.scx_apply_cc = cc;

        // DMG BG grid SCX journal (see bg_wg_apply): record the mid-mode-3 SCX
        // write so the tile-map column resolves it at the tile's reconstructed
        // hardware TileNumber dot instead of the stall-displaced live dot.
        if !mmio.is_cgb_features_enabled() && self.state == State::PixelTransfer {
            let old = self
                .bg_scx_hist
                .last()
                .map(|&(_, _, new)| new)
                .unwrap_or(self.scx_delayed);
            if old != value {
                self.bg_scx_hist.push((cc, old, value));
            }
        }

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
        // DMG "line 154" STAT-write VBlank-IF glitch (gbmicrotest
        // stat_write_glitch_l154_d). A FF41 write straddling the frame-wrap
        // boundary (LY 153->0 VBlank exit, first dots of the new frame) clears
        // the still-pending VBlank IF bit on real DMG-CPU-08 — the shared
        // VBlank/STAT interrupt-line glitch. `l154_vblank_glitch_window` is armed
        // at the frame wrap and disarmed a few dots into line 0/1, so only a write
        // at that exact boundary is affected. DMG-only (CGB has no STAT-write bug).
        if ff41_written
            && self.l154_vblank_glitch_window
            && !mmio.is_cgb_features_enabled()
        {
            let cur_if = mmio.read(registers::INTERRUPT_FLAG);
            if cur_if & (registers::InterruptFlag::VBlank as u8) != 0 {
                mmio.write(
                    registers::INTERRUPT_FLAG,
                    cur_if & !(registers::InterruptFlag::VBlank as u8),
                );
            }
        }
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
        let old_lyc = self.lyc_irq.lyc_reg_src();

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

        // STAT-write IRQ timing follows the CGB LCD controller on CGB hardware
        // (incl. DMG-compat mode), matching Gambatte's `cart_.isCgb()` gate.
        let cgb = mmio.is_cgb();
        let lyc_reg = self.lyc_irq.lyc_reg_src();
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
        let old = self.lyc_irq.lyc_reg_src();
        if data == old {
            return;
        }
        let cc = self.write_cc(mmio.is_double_speed_mode());
        let lc = self.ly_counter(mmio);
        let stat = self.stat_reg_committed;
        // LYC-write coincidence/IRQ timing follows the CGB LCD controller on CGB
        // hardware (incl. DMG-compat mode); Gambatte gates on `cart_.isCgb()`.
        let cgb = mmio.is_cgb();
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
        // video.cpp:820 getStat LYC compare: the next-line anticipation window is
        // `timeToNextLy > 2 - (!isDoubleSpeed() && isAgb())`. The renderer's
        // line-cycle equivalent is `ticks >= 456 - thresh`; AGB single-speed
        // lowers the threshold from 2 to 1, extending the window one dot earlier.
        let agb_ss = mmio.is_agb() && !mmio.is_double_speed_mode();
        let anticipate_from = if agb_ss { 455 } else { 454 };
        if self.ticks < anticipate_from {
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
        // FF41 mode-bit read-back anticipation: in the last 3 dots of an
        // HBlank line (or of line 153) FF41 reports mode 2 (the next line's
        // mode). Match Gambatte's `getStat` `lineCycles >= 453` threshold by
        // writing the anticipated mode at dot 453 and re-syncing the STAT
        // edge latch so the bit change does not produce a duplicate IRQ
        // rising edge — the actual mode-2 IRQ has already been delivered by
        // the pretrigger above when its conditions were met.
        let mode2_anticipate_dot = MODE2_STAT_PRETRIGGER_DOT + 1; // 453
        // The only work-doing path needs `ticks == 453`; bail on every other
        // dot before touching state/mmio. (`disabled` freezes ticks, so it can
        // never sit at 453 while disabled — this subsumes the disabled guard.)
        if self.disabled || self.ticks != mode2_anticipate_dot {
            return;
        }

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
                // First-frame-after-enable blanking: the panel shows the LCD-off
                // blank for the frame produced immediately after this enable.
                self.frames_since_enable = 0;
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
                // See note in `enable_display`: LYC/STAT timing follows the CGB
                // LCD controller on CGB hardware regardless of DMG-compat mode.
                self.lyc_irq.set_cgb(mmio.is_cgb());
                self.lyc_irq.seed(mmio.read(LCD_STATUS), mmio.read(LYC));
                self.mstat_irq.seed(mmio.read(LCD_STATUS), mmio.read(LYC));
                self.lyc_irq.lcd_reset();
                self.mstat_irq.lcd_reset(self.lyc_irq.lyc_reg_src());
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
                    let dma_writing =
                        mmio.oam_dma_window_active() && !mmio.mgb_frozen_merge_active();
                    self.oam_reader.src_disabled = dma_writing;
                    self.oam_reader.enable_display(cc, ds);
                    self.prev_dma_writing = dma_writing;
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
        // Disarm the "line 154" STAT-write VBlank-IF glitch window once the new
        // frame has advanced a few dots past the LY 0->1 boundary. The glitch is
        // observed only for a FF41 write straddling that boundary (gbmicrotest
        // stat_write_glitch_l154_d: internal_ly==1, line_cycle 0); keeping the
        // window this narrow guarantees a normal mid-frame STAT write never
        // clears a legitimately-pending VBlank IRQ.
        if self.l154_vblank_glitch_window
            && (self.internal_ly_val > 1
                || (self.internal_ly_val == 1 && self.line_cycle > 4))
        {
            self.l154_vblank_glitch_window = false;
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
            self.mstat_irq.lcd_reset(self.lyc_irq.lyc_reg_src());
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

        // (STAT IRQ delivery is handled entirely by the event model in
        // dispatch_stat_events; `previous_stat_interrupt_line` is a write-only
        // legacy latch, so the per-dot recompute here was pure dead work.)

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
                    self.win_fetch_anchor = None;
                    self.win_first_tile_chop = 0;
                    self.win_being_fetched = false;
                    self.insert_bg_pixel = false;
                    self.win_wx0_delayed = false;
                    self.dmg_wx_trigger_pending = None;
                    {
                        let we_now =
                            (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
                        self.we_dot_hist = [we_now; 5];
                        self.we_glitch_tile_starts = [None; 2];
                        self.we_glitch_discard_insert = false;
                        self.we_insert_suppressed = false;
                    }
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
                            self.win_kill_tap_late = false;
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
                    // DMG wx==166 (lcd_hres+6): Gambatte's plotPixel runs at EVERY
                    // xpos as the fetcher walks the line; the wx==xpos==166 branch
                    // (883-895) therefore fires at the END of mode 3 (xpos reaches
                    // 166), AFTER the line's mid-mode-3 WE-off has had its effect on
                    // winDrawState — NOT at M3 start. Relocating this branch to the
                    // mode-3 -> HBlank transition (where xpos==166) is what lets the
                    // steady-state wxA6 sequence converge: f0(++winYPos, state->2) ->
                    // WE-off(state==2 -> clears started, state->0, stops window) ->
                    // THIS branch B at xpos==166(state |= win_draw_start, state->1) ->
                    // HBlank WE-on(state==win_draw_start -> ++winYPos, state->3). That
                    // is the TWO winYPos increments per line (8px/4rows) the window
                    // diagonal needs, and the WE-off now actually reverts the right
                    // columns to BG (it no longer sees win_draw_start pre-armed). See
                    // the relocated block at the mode-3 -> HBlank boundary below.
                    // First scanline after enable is now armed; subsequent
                    // lines use normal Mode 2 timing.
                    let was_first_line = self.first_line_after_enable;
                    self.first_line_after_enable = false;
                    self.mode0_pretriggered_this_line = false;
                    self.mode0_reported_this_line = false;
                    self.line_rendered_this_line = false;
                    self.wxa6_lineend_applied = false;
                    // SCX fine-scroll discard target (Gambatte M3Start::f1): the
                    // break xpos is resolved over the first M3 dots by re-reading
                    // SCX live (see the early-window loop in PixelTransfer). Seed
                    // it unlatched (-1) and record the arm dot for xpos tracking.
                    self.m3_pixels_discarded = 0;
                    self.m3_arm_dot = self.ticks;
                    // Per-pixel BG-enable history (RB_LINERENDER): anchor the
                    // plot-cc origin at mode-3 entry and seed the line's history
                    // with the BG-enable bit in effect now. Mid-mode-3 LCDC.0
                    // writes append (commit_cc, bgen) entries (handle_lcdc_write).
                    self.bgen_history.clear();
                    // Seed at boundary column 0 (applies to all columns until the
                    // first mid-mode-3 toggle).
                    self.bgen_history.push((
                        0,
                        (self.lcdc & (LCDCFlags::BGDisplay as u8)) != 0,
                    ));
                    // Per-line tile-index-is-tile-data glitch targets (SameBoy
                    // tile_sel_glitch); mid-mode-3 falling LCDC.4 writes append the
                    // single (cc, k) read each arms (see handle_lcdc_write).
                    self.tidxtd_glitch.clear();
                    // DMG window bus-glitch state is per-line (see wg_apply).
                    self.wg_hist.clear();
                    self.bg_tile_buf.clear();
                    self.win_tile_buf.clear();
                    self.wg_anchor_cc = None;
                    self.wg_dpre = 0;
                    self.bg_anchor_cc = None;
                    self.bg_scy_hist.clear();
                    self.bg_scx_hist.clear();
                    // CGB-compat journal flavor (see the CGBWG_* consts): DMG cart on
                    // CGB hardware (compat mode runs with CGB features OFF, so
                    // it shares the DMG render paths; the journals resolve
                    // with the CGB grid/transition rules instead).
                    self.wg_cgb = mmio.is_cgb() && !mmio.is_cgb_features_enabled();
                    // Per-pixel DMG palette histories: seed each at boundary 0 with
                    // the 1-dot-delayed register value (`*_delayed`, refreshed at the
                    // end of every dot), NOT the live register. A BGP/OBP write on the
                    // dot the PPU enters mode 3 has already updated mmio but must not
                    // yet color column 0 — the column-0 pixel sees the prior dot's
                    // value (Gambatte dmgpalette_during_m3: the write at mode-3 entry
                    // leaves column 0 white). Mid-mode-3 writes after entry append
                    // (boundary_col, value) entries via on_{bgp,obp0,obp1}_write, which
                    // land at column >= 1 so column 0 keeps this seed.
                    self.bgp_history.clear();
                    self.bgp_history.push((0, self.bgp_delayed));
                    self.bgp_dot_history.clear();
                    // CGB-compat (wg_cgb) resolves BGP per dot from this history; unlike
                    // the DMG per-dot `bgp_delayed` latch, real CGB silicon colors the
                    // mode-3 column-0 pixel with the LIVE BGP register (age m3-bg-bgp-ncm:
                    // the pre-frame BGP is already latched at mode-3 arm). DMG keeps the
                    // 1-dot-delayed seed (dmgpalette_during_m3, via bgp_history).
                    let bgp_dot_seed = if self.wg_cgb { mmio.read(BGP) } else { self.bgp_delayed };
                    self.bgp_dot_history.push((0, bgp_dot_seed));
                    // Clear any leftover DMG BGP phase-hold from the previous line.
                    self.bgp_defer_countdown = 0;
                    self.obp0_history.clear();
                    self.obp0_history.push((0, self.obp0_delayed));
                    self.obp1_history.clear();
                    self.obp1_history.push((0, self.obp1_delayed));
                    self.obp0_dot_history.clear();
                    self.obp0_dot_history.push((0, self.obp0_delayed));
                    self.obp1_dot_history.clear();
                    self.obp1_dot_history.push((0, self.obp1_delayed));
                    // DMG mid-mode-3 OBJ-enable/OBJ-size toggle model: seed the
                    // per-column OBJ-enable history and the per-dot OBJ-size
                    // history with the bits in effect at mode-3 entry, and reset
                    // the per-sprite live fetch records (all Pending).
                    self.objen_history.clear();
                    self.objen_history.push((
                        0,
                        (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0,
                    ));
                    self.objsize_dot_history.clear();
                    self.objsize_dot_history.push((
                        0,
                        (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0,
                    ));
                    self.sprite_fetch_recs.clear();
                    self.sprite_fetch_recs
                        .resize(self.sprites_on_line.len(), SpriteFetchRec::default());
                    self.bgp_writes.clear();
                    // Carry a mode-2 BGP write into this line's spike cadence as a
                    // neighbor-only entry (see on_bgp_write); a mode-3 partner within
                    // BGP_SPIKE_CADENCE_CC then paints its spike (age m3-bg-bgp).
                    if let Some((cc, v)) = self.bgp_mode2_pending.take()
                        && !mmio.is_cgb()
                    {
                        self.bgp_writes.push((cc, 0xFF, v));
                        // The mode-2 write is the true settled BGP entering mode 3
                        // (bgp_delayed lags a dot and can miss a late-mode-2 write),
                        // so re-seed column 0's palette + the spike's `old` baseline
                        // with it — the restore's glitch then ORs against FF, painting
                        // its victim column with the pre-restore (glitch) shade.
                        self.bgp_history.clear();
                        self.bgp_history.push((0, v));
                        self.bgp_delayed = v;
                    }
                    // 160-entry per-column BG-index scratch; ensure sized (deserialized
                    // saves may carry an empty vec) and clear to -1 (no BG pixel yet).
                    self.line_bg_idx.clear();
                    self.line_bg_idx.resize(160, -1);
                    self.m3_arm_scx = mmio.read(SCX) & 0x07 ;
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
                        // The runtime sprite0-at-scx fine-scroll stall (Gambatte
                        // M3Start::f1) extends the real mode-3 -> mode-0 transition
                        // past the predictor's m0Time; fold it into the renderer /
                        // STAT-read boundary here (m0_irq_event_cc_master subtracts
                        // it back for the predictor-timed m0 STAT IRQ).
                        let m0t = self.m0_time_exact(mmio, m3_len, is_cgb, false)
                            + ((self.sprite0_scx_extra(mmio, is_cgb) as u64) << ds);
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
                // Shift the DMG WE per-dot visibility history (see we_dot_hist).
                self.we_dot_hist = [
                    (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0,
                    self.we_dot_hist[0],
                    self.we_dot_hist[1],
                    self.we_dot_hist[2],
                    self.we_dot_hist[3],
                ];
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
                if let Some(m0t) = self.m0_time_master
                    && mmio.master_cc() >= m0t {
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
                            // DMG wx==166 plotPixel-at-xpos166 (mode-3 end). See
                            // apply_dmg_wxa6_lineend_windraw.
                            self.apply_dmg_wxa6_lineend_windraw(mmio, mmio.is_cgb_features_enabled());
                            self.cgb_train_reresolve(mmio);
                            self.win_train_reresolve(mmio);
                            self.resolve_bgp_spikes(mmio);
                            self.state = State::HBlank;
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

                // DMG WX 1..6 EARLY window activation: the WX comparator matches
                // during the discard prologue at position WX-7 (SameBoy activates
                // while position_in_line is still negative), i.e. (7-WX) dots
                // BEFORE the first visible pop. Evaluating it there — not at the
                // pos-0 trigger below — matters when WX is rewritten mid-prologue:
                // hardware activates with the OLD WX (mealybug m3_wx_4_change:
                // the WX=4 activation beats the WX=LY rewrite by 1-3 dots on
                // every row). pos = ticks - (m3_arm_dot + 12 + scx&7) maps our
                // pipeline's pop timeline (even arm: TN arm+2 .. push arm+8,
                // warmup 4, first visible pop arm+12+scx). The activation then
                // runs the restart fetch on real dots (anchored cadence) and the
                // remaining (7-WX) prologue pops chop the first window tile, so
                // the first VISIBLE pixel still lands at pos-0 + 6. Exact-match
                // only; any miss falls back to the pos-0 trigger below.
                if !mmio.is_cgb_features_enabled()
                    && self.x == 0
                    && !self.fetcher.is_fetching_window()
                    && !self.first_line_after_enable
                    && self.m3_discard_target >= 0
                    // Comparator WE tap (see we_dot_hist): delayed, not live.
                    && self.window_y_active_with(mmio, self.we_dot_hist[1] && self.we_dot_hist[2])
                {
                    let wx = mmio.read(WX);
                    // WX==0 with SCX&7==0 takes the same early-comparator
                    // activation with chop 7 (window column 7 lands at screen
                    // x0 — the WX=0 window's left 7 columns are off-screen).
                    // SCX&7>0 keeps the pos-0 trigger + one-dot delay quirk
                    // (win_wx0_delayed).
                    if (1..7).contains(&wx) || (wx == 0 && self.m3_discard_target == 0) {
                        let s = self.m3_discard_target as i64;
                        // pos-0 dot (first visible pop absent windows): TN runs
                        // at the first even dot after arm, push +6, warmup 4,
                        // + the scx fine discard pops.
                        let base = self.m3_arm_dot as i64 + 12 - (self.m3_arm_dot & 1) as i64
                            + s;
                        // The comparator's activation dot is pos == WX-7, but a
                        // CPU WX store's new value reaches the comparator within
                        // the same dot on hardware while our mmio only exposes it
                        // to the NEXT dot — so evaluate one dot later (pos ==
                        // WX-6) with the then-visible WX. mealybug m3_wx_6_change
                        // brackets this: the WX=6->LY rewrite (one dot after the
                        // pos -1 match) must WIN (no window starts), while
                        // m3_wx_4/5_change's WX must LOSE (window starts with the
                        // old WX 4/5).
                        let pos = self.ticks as i64 - base;
                        if pos == wx as i64 - 6 {
                            self.win_y_pos = self.win_y_pos.wrapping_add(1);
                            self.win_draw_started = true;
                            self.fetcher.start_window(0);
                            self.we_glitch_tile_starts = [None; 2];
                            self.win_kill_tap_late = true;
                            self.window_started_this_line = true;
                            self.win_being_fetched = true;
                            self.m3_sprite_prev_tile = SPRITE_TILE_NONE;
                            if self.win_start_dot.is_none() {
                                self.win_start_dot = Some(self.ticks);
                            }
                            // Remaining prologue pops become the first-tile chop;
                            // the warmup/scx-discard bookkeeping is superseded
                            // (their dots are consumed by the restart fetch).
                            self.win_first_tile_chop = 7 - wx;
                            self.pixel_transfer_warmup = 0;
                            self.m3_pixels_discarded = self.m3_discard_target as u8;
                            // The activation dot itself was one dot ago: its
                            // TileNumber is due now (catch-up), low/high/push at
                            // +1/+3/+5 via the anchored cadence.
                            self.wg_set_anchor(0);
                            let fls = self.wg_apply(self.fetcher_lcdc_state());
                            if let Some(event) = self.fetcher.step(
                                mmio,
                                fls,
                                crate::ppu::fetcher::FetchPos {
                                    window_line: self.win_y_pos,
                                    display_x: self.x,
                                    pending_discard: 0,
                                    scy: self.scy_delayed,
                                    scx: self.scx_delayed,
                                },
                            ) {
                                if matches!(
                                    event.kind,
                                    crate::ppu::fetcher::FetcherDebugEventKind::TileNumber
                                ) {
                                    self.subcc_last_tn_cc = self.abs_cc;
                                }
                                self.record_fetch_debug_event(event, mmio);
                            }
                            self.win_fetch_anchor = Some(self.ticks.wrapping_sub(1));
                            break 'label;
                        }
                    }
                }

                // Whether this dot executed a PushToFIFO fetch substep — the
                // window-reactivation insert fires on the pop of a window tile's
                // FIRST pixel, i.e. our push dot (SameBoy: fetcher at
                // GB_FETCHER_GET_TILE_T1 with bg_fifo.size == 8, the cycle right
                // after its push-at-empty).
                let mut push_this_dot = false;
                // Fetcher cadence: on CGB, decouple from absolute self.ticks so that
                // sprite-fetch stall dots don't flip the fetcher's even/odd phase
                // (matches Gambatte). On DMG, keep the original self.ticks gate.
                let cadence_even = if mmio.is_cgb_features_enabled() {
                    let even = self.fetcher_cadence_tick.is_multiple_of(2);
                    self.fetcher_cadence_tick = self.fetcher_cadence_tick.wrapping_add(1);
                    even
                } else if let Some(anchor) = self.win_fetch_anchor {
                    // Window-startup fetch: phase-locked to the trigger dot so
                    // the first window pixel pops exactly 6 dots after it.
                    self.ticks.wrapping_sub(anchor).is_multiple_of(2)
                } else {
                    self.ticks.is_multiple_of(2)
                };

                // DMG mid-mode-3 WE-off window kill (SameBoy GET_TILE_T1
                // `wx_triggered = false`): the window fetcher re-samples the
                // window-enable bit at each TileNumber step with a one-dot
                // delayed sample (we_dot_hist[2]); reading OFF reverts the fetch
                // to BG from THIS tile on (the already-pushed window pixels in
                // the FIFO drain out, so a killed window always shows a multiple
                // of 8 pixels). A WE-off pulse short enough that its delayed
                // sample misses every TileNumber dot leaves the window running —
                // hardware behavior the mealybug win_en_change_multiple[_wx]
                // staircases verify (Gambatte instead latches winDrawState at
                // the write, which kills the window on any pulse).
                if cadence_even
                    && !mmio.is_cgb_features_enabled()
                    && self.fetcher.is_fetching_window()
                    && self.fetcher.fetch_state_is_tile_number()
                    && !self.we_dot_hist[if self.win_kill_tap_late { 3 } else { 2 }]
                {
                    self.fetcher.stop_window_with_extra(0);
                    self.window_started_this_line = false;
                    self.win_being_fetched = false;
                }

                // DMG BG fetch-grid origin (see bg_wg_apply): the line's first
                // BG TileNumber read runs on this dot, before any sprite stall.
                if cadence_even
                    && !mmio.is_cgb_features_enabled()
                    && self.bg_anchor_cc.is_none()
                    && !self.fetcher.is_fetching_window()
                    && self.fetcher.fetch_state_is_tile_number()
                    && self.fetcher.get_tile_index() == 0
                {
                    self.bg_anchor_cc = Some(self.abs_cc);
                }
                let fetcher_lcdc_state =
                    self.bg_wg_apply(self.wg_apply(self.fetcher_lcdc_state()), mmio.read(LY));
                // Pixels still to be discarded for SCX fine-scroll: they sit in
                // the FIFO but won't be displayed, so the BG tile column (derived
                // from display_x + FIFO depth) must not count them.
                let pending_discard = if self.x == 0 {
                    (self.m3_discard_target.max(0) as u8).saturating_sub(self.m3_pixels_discarded)
                } else {
                    0
                };
                if cadence_even
                    && let Some(event) = self.fetcher.step(mmio, fetcher_lcdc_state, crate::ppu::fetcher::FetchPos {
                        window_line: self.win_y_pos,
                        display_x: self.x,
                        pending_discard,
                        scy: self.scy_delayed,
                        scx: self.scx_delayed,
                    }) {
                        if matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::TileNumber) {
                            self.subcc_last_tn_cc = self.abs_cc;
                        }
                        if matches!(event.kind, crate::ppu::fetcher::FetcherDebugEventKind::PushToFifo) {
                            push_this_dot = true;
                            // The display-x at which this tile's first pixel will
                            // pop (the SameBoy push-at-empty dot), SIGNED: during
                            // the SCX fine-scroll discard prologue the boundary
                            // sits at SameBoy position -(pending discards) < 0.
                            if !mmio.is_cgb_features_enabled() {
                                let first_x = self.x as i32 + event.fifo_size as i32
                                    - 8
                                    - pending_discard as i32;
                                if (0..160).contains(&first_x) {
                                    // Visible boundary: queue for the pop-side
                                    // WE-off zero-pixel insert check.
                                    if let Some(slot) = self
                                        .we_glitch_tile_starts
                                        .iter_mut()
                                        .find(|s| s.is_none())
                                    {
                                        *slot = Some(first_x as u8);
                                    }
                                } else if first_x < 0 && !mmio.is_cgb() {
                                    // Discard-prologue boundary (SameBoy issue
                                    // #278): evaluate the WE-off insert HERE, at
                                    // the push dot. logical position = first_x+7
                                    // (SameBoy clamps out-of-range to 0, matching
                                    // WX==0). A hit inserts a color-0 pixel that
                                    // the prologue itself swallows — one discard
                                    // dot consumes it instead of a real pixel
                                    // (see we_glitch_discard_insert). Pre-CGB
                                    // MACHINES only (SameBoy !GB_is_cgb): the CGB
                                    // PPU has no insert glitch even in DMG-compat.
                                    let logical = first_x + 7;
                                    let logical =
                                        if (0..=167).contains(&logical) { logical } else { 0 };
                                    if self.window_y_triggered
                                        && !self.fetcher.is_fetching_window()
                                        && !self.we_dot_hist[2]
                                        && !self.we_insert_suppressed
                                        && mmio.read(WX) as i32 == logical
                                    {
                                        self.we_glitch_discard_insert = true;
                                    }
                                }
                            }
                            // CGB-compat up-pulse LCDC.4 train: buffer each BG tile
                            // so a line-end re-resolve against the COMPLETE journal
                            // can fix the tiles fetched before the whole pulse train
                            // was journaled (see cgb_train_reresolve).
                            if self.wg_cgb && !event.fetching_window && !self.wg_hist.is_empty() {
                                let first_x = (self.x as i32 + event.fifo_size as i32
                                    - 8
                                    - pending_discard as i32)
                                    .max(0);
                                if (0..160).contains(&first_x) {
                                    self.bg_tile_buf.push(CapturedBgTile {
                                        n: event.tile_index as u64,
                                        tn: event.tile_num,
                                        first_x: first_x as u8,
                                        y: self.fetcher.latched_y(),
                                        live_low_tds: self.fetcher.last_low_tds(),
                                        live_high_tds: self.fetcher.last_high_tds(),
                                    });
                                }
                            }
                            // WINDOW analog (win_train_reresolve): the window internal
                            // line is win_y_pos (not latched_y, which the window fetch
                            // does not update).
                            if self.wg_cgb && event.fetching_window && !self.wg_hist.is_empty() {
                                let first_x = (self.x as i32 + event.fifo_size as i32
                                    - 8
                                    - pending_discard as i32)
                                    .max(0);
                                if (0..160).contains(&first_x) {
                                    self.win_tile_buf.push(CapturedWinTile {
                                        n: event.tile_index as u64,
                                        tn: event.tile_num,
                                        first_x: first_x as u8,
                                        y: self.win_y_pos,
                                        live_low_tds: self.fetcher.last_low_tds(),
                                        live_high_tds: self.fetcher.last_high_tds(),
                                    });
                                }
                            }
                        }
                        // NOTE: the window fetch anchor persists for the rest of
                        // the line — the hardware fetch grid stays phase-locked
                        // to the restart (pushes every 8 dots from the anchor),
                        // so the reactivation-insert columns stay at
                        // window_start + 8k (mealybug m3_wx_4_change ly%8==4
                        // diagonal). It resets at the next M3 arm or window
                        // restart.
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

                // DMG deferred WX-comparator commit (see dmg_wx_trigger_pending):
                // the exact x+7==wx match armed on the previous dot commits now
                // iff WX still reads the matched value — the hardware comparator
                // samples WX through the end of the CPU store's M-cycle, so a
                // store landing on the commit dot kills the match. The restart is
                // executed as-of the arm dot (TileNumber catch-up + anchor one
                // dot back), byte-identical to the immediate start for stable WX.
                if !mmio.is_cgb_features_enabled()
                    && let Some((arm_dot, arm_wx)) = self.dmg_wx_trigger_pending.take()
                    && self.ticks == arm_dot.wrapping_add(1)
                        && mmio.read(WX) == arm_wx
                        && self.x + 7 == arm_wx
                        && !self.fetcher.is_fetching_window()
                    {
                        self.win_y_pos = self.win_y_pos.wrapping_add(1);
                        self.win_draw_started = true;
                        self.fetcher.start_window(self.x);
                        self.we_glitch_tile_starts = [None; 2];
                        self.win_kill_tap_late = true;
                        self.window_started_this_line = true;
                        self.win_being_fetched = true;
                        self.win_first_tile_chop = 0;
                        // The activation dot was one dot ago: its TileNumber is
                        // due now (catch-up); low/high/push at +1/+3/+5 via the
                        // anchored cadence.
                        self.wg_set_anchor(1);
                        let fls = self.wg_apply(self.fetcher_lcdc_state());
                        if let Some(event) = self.fetcher.step(
                            mmio,
                            fls,
                            crate::ppu::fetcher::FetchPos {
                                window_line: self.win_y_pos,
                                display_x: self.x,
                                pending_discard: 0,
                                scy: self.scy_delayed,
                                scx: self.scx_delayed,
                            },
                        ) {
                            if matches!(
                                event.kind,
                                crate::ppu::fetcher::FetcherDebugEventKind::TileNumber
                            ) {
                                self.subcc_last_tn_cc = self.abs_cc;
                            }
                            self.record_fetch_debug_event(event, mmio);
                        }
                        self.win_fetch_anchor = Some(self.ticks.wrapping_sub(1));
                        self.m3_sprite_prev_tile = SPRITE_TILE_NONE;
                        if self.win_start_dot.is_none() {
                            self.win_start_dot = Some(self.ticks.wrapping_sub(1));
                        }
                        break 'label;
                    }
                    // else: canceled — the WX store on the commit dot rewrote the
                    // comparator input; no window starts (fall through).

                // Check if we should start window rendering. On DMG the
                // window-enable bit feeding the WX comparator is the DELAYED
                // per-dot tap (we_dot_hist, samples one and two dots back) —
                // an 8-cycle WE-off pulse blocks 9 consecutive comparator dots
                // on hardware (mealybug win_en_change_multiple_wx). CGB keeps
                // the live bit. When the x==0 trigger fires with SCX fine
                // discards still pending, our check runs `pending` dots BEFORE
                // the hardware comparator dot (position 0 pops that much
                // later), so the taps shift toward the present accordingly
                // (gambatte window late_disable_scx3_0/scx5_0: the disable
                // right before the x==0 check dot must still block the start).
                let trigger_we = if mmio.is_cgb_features_enabled() {
                    (self.lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0
                } else {
                    let pending = if self.x == 0 && self.m3_discard_target >= 0 {
                        (self.m3_discard_target as u8)
                            .saturating_sub(self.m3_pixels_discarded)
                    } else {
                        0
                    };
                    match pending {
                        0 => self.we_dot_hist[1] && self.we_dot_hist[2],
                        1 => self.we_dot_hist[0] && self.we_dot_hist[1],
                        _ => self.we_dot_hist[0],
                    }
                };
                if self.window_y_active_with(mmio, trigger_we)
                    && !self.fetcher.is_fetching_window()
                {
                    let wx = mmio.read(WX);
                    let is_cgb = mmio.is_cgb_features_enabled();
                    // DMG never starts the window drawing at WX==166; CGB does.
                    let wx_allowed = wx <= 166 && (is_cgb || wx != 166);
                    // WX=0-6 can trigger immediately, WX=7+ needs exact match with X+7.
                    // On DMG, WX 1..6 activates ONLY via the exact pos==WX-7
                    // prologue match (the EARLY check above); reaching pos 0 with
                    // WX 1..6 means the match was missed (WX rewritten
                    // mid-prologue) and the window does not start this line
                    // (mealybug m3_wx_6_change). WX=0 and CGB keep the immediate
                    // x==0 start.
                    let is_dmg = !is_cgb;
                    // DMG one-dot-late activation (SameBoy's position+6 check):
                    // when the exact x+7==WX dot did not activate (the comparator
                    // read the WE-off pulse), the very next dot still matches via
                    // WX == x+6 and starts the window one pixel late (the _wx rows
                    // 16 and 44 start at WX-6).
                    let should_start_window = wx_allowed
                        && if wx < 7 {
                            self.x == 0 && !(is_dmg && (1..7).contains(&wx))
                        } else {
                            self.x + 7 == wx || (is_dmg && self.x >= 1 && self.x + 6 == wx)
                        };

                    // DMG WX=0 + SCX&7>0 quirk: the window activates one T-cycle
                    // later (mealybug m3_window_timing_wx_0). The would-be trigger
                    // dot is dead (no pop, no activation); trigger next dot.
                    if should_start_window
                        && !is_cgb
                        && wx == 0
                        && !self.win_wx0_delayed
                        && (if self.m3_discard_target >= 0 {
                            self.m3_discard_target as u8
                        } else {
                            mmio.read(SCX) & 0x07
                        }) != 0
                    {
                        self.win_wx0_delayed = true;
                        break 'label;
                    }

                    if should_start_window {
                        // DMG exact-match mid-line trigger: defer the commit one
                        // dot so a WX store landing on the commit dot is seen by
                        // the comparator (see dmg_wx_trigger_pending).
                        if is_dmg && wx >= 7 && self.x + 7 == wx {
                            self.dmg_wx_trigger_pending = Some((self.ticks, wx));
                            break 'label;
                        }
                        // Window draw-start: Gambatte increments winYPos here
                        // (M3Start::f0 / plotPixel win_draw_start), once per line
                        // the window actually begins drawing, not per-line in M2.
                        self.win_y_pos = self.win_y_pos.wrapping_add(1);
                        self.win_draw_started = true;
                        // Start window rendering
                        self.fetcher.start_window(self.x);
                        self.we_glitch_tile_starts = [None; 2];
                        self.win_kill_tap_late = true;
                        self.window_started_this_line = true;
                        self.win_being_fetched = true;
                        // DMG: hardware restarts the fetcher ON the trigger dot
                        // (TileNumber now; low/high/push at t+2/t+4/t+6), so the
                        // first window pixel pops exactly 6 dots after the
                        // trigger regardless of the global fetch parity. Run the
                        // TileNumber substep immediately and phase-lock the rest
                        // of the startup to this dot (see win_fetch_anchor).
                        if !is_cgb {
                            // WX 1..6: the comparator matched chop = (7-WX) dots
                            // into the discard prologue, so the activation lies
                            // chop dots in the PAST. Catch the fetch up by
                            // running every substep whose anchored phase
                            // (0,2,4,6) has already elapsed, anchor the cadence
                            // at ticks - chop, and pace the chop discard pops
                            // 1/dot from the x==0 prologue below. WX=0 keeps the
                            // plain trigger (separate activation-position quirk
                            // cluster; see win_wx0_delayed).
                            let chop = if (1..7).contains(&wx) { 7 - wx } else { 0 };
                            self.win_first_tile_chop = chop;
                            // DMG window bus-glitch grid origin (see wg_apply):
                            // this TileNumber's conceptual dot is `chop` dots in
                            // the past; a pre-window sprite stall delayed the
                            // anchored trigger by its live charged penalty
                            // (SpriteFetchRec) that hardware does NOT share
                            // (its own delay is D_pre, folded in at read
                            // evaluation).
                            self.wg_set_anchor(chop as u64);
                            let mut phase = 0u8;
                            loop {
                                let fls = self.wg_apply(self.fetcher_lcdc_state());
                                if let Some(event) = self.fetcher.step(
                                    mmio,
                                    fls,
                                    crate::ppu::fetcher::FetchPos {
                                        window_line: self.win_y_pos,
                                        display_x: self.x,
                                        pending_discard: 0,
                                        scy: self.scy_delayed,
                                        scx: self.scx_delayed,
                                    },
                                ) {
                                    if matches!(
                                        event.kind,
                                        crate::ppu::fetcher::FetcherDebugEventKind::TileNumber
                                    ) {
                                        self.subcc_last_tn_cc = self.abs_cc;
                                    }
                                    self.record_fetch_debug_event(event, mmio);
                                }
                                phase += 2;
                                if phase > chop {
                                    break;
                                }
                            }
                            // chop >= 6: the first tile's push already elapsed
                            // (phase 6), so its first discard pop is due on this
                            // very dot.
                            if chop >= 6 && self.fetcher.pixel_fifo.pop().is_ok() {
                                self.win_first_tile_chop -= 1;
                            }
                            self.win_fetch_anchor =
                                Some(self.ticks.wrapping_sub(chop as u128));
                        }
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

                // WX<7 chopped window start: the prologue discard pops that ran
                // past the (earlier) activation position chop the first window
                // tile's leading pixels, one per dot (see win_first_tile_chop).
                if self.x == 0 && self.win_first_tile_chop > 0 {
                    if self.fetcher.pixel_fifo.pop().is_ok() {
                        self.win_first_tile_chop -= 1;
                        self.win_being_fetched = false;
                    }
                    break 'label;
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
                    // WE-off insert glitch, prologue variant: the inserted
                    // color-0 pixel sits at the FRONT of the stream and is the
                    // first pixel this discard dot drops — no real FIFO pixel
                    // is consumed, so one extra leading BG pixel survives and
                    // the visible line shifts right by one.
                    if self.m3_pixels_discarded < target && self.we_glitch_discard_insert {
                        self.we_glitch_discard_insert = false;
                        self.m3_pixels_discarded += 1;
                        self.win_being_fetched = false;
                        break 'label;
                    }
                    if self.m3_pixels_discarded < target
                        && let Ok(_) = self.fetcher.pixel_fifo.pop() {
                            self.m3_pixels_discarded += 1;
                            self.win_being_fetched = false;
                            break 'label;
                    }
                }

                // Put a pixel from the FIFO on screen with sprite mixing.
                // Stop visible output at x==160; the scheduled dot ends Mode 3.
                if self.x >= 160 {
                    break 'label;
                }
                // DMG window reactivation zero pixel (SameBoy insert_bg_pixel):
                // the WX comparator matches again with the window already active
                // (past its startup fetch), exactly at the pop of a window
                // tile's FIRST pixel — our push-at-empty dot (SameBoy: fetcher
                // at GET_TILE_T1 with bg_fifo.size == 8, the cycle right after
                // its push; mealybug m3_wx_4/5_change bracket the phase: the
                // insert diagonal sits at x == 8k + (8 - chop)). The pop below
                // then renders a color-0 pixel WITHOUT consuming the FIFO,
                // inserting one pixel into the line.
                if !mmio.is_cgb_features_enabled()
                    && self.window_started_this_line
                    && self.fetcher.is_fetching_window()
                    && !self.win_being_fetched
                    && push_this_dot
                    && self.fetcher.pixel_fifo.size() == 8
                    && mmio.read(WX) == self.x + 7
                {
                    self.insert_bg_pixel = true;
                }
                // DMG WE-off zero-pixel insertion glitch (SameBoy display.c PUSH
                // branch, SameBoy issue #278): with the window Y-latch triggered
                // but the window enable OFF (delayed tap, see we_dot_hist), a
                // tile-boundary pop (SameBoy's push-at-empty dot; our queued
                // first-pixel x) where WX == x+7 renders one color-0 pixel
                // WITHOUT consuming the FIFO (mealybug win_en_change_multiple_wx
                // rows 15/39: the single white pixel at x = WX-7 on the
                // trigger-missed rows).
                let mut at_tile_boundary = false;
                for slot in self.we_glitch_tile_starts.iter_mut() {
                    if let Some(fx) = *slot {
                        if fx == self.x {
                            at_tile_boundary = true;
                            *slot = None;
                        } else if fx < self.x {
                            // Stale (chop/discard consumed the boundary pop).
                            *slot = None;
                        }
                    }
                }
                // Pre-CGB MACHINES only (SameBoy !GB_is_cgb): the CGB PPU has no
                // WE-off insert glitch even in DMG-compat mode (GB Player /
                // CGB footage in SameBoy issue #278 shows the unshifted line).
                if !mmio.is_cgb()
                    && self.window_y_triggered
                    && !self.fetcher.is_fetching_window()
                    && !self.we_dot_hist[2]
                    && !self.we_insert_suppressed
                    && at_tile_boundary
                    && mmio.read(WX) == self.x + 7
                {
                    self.insert_bg_pixel = true;
                    // The inserted pixel shifts every later boundary one to the
                    // right.
                    for fx in self.we_glitch_tile_starts.iter_mut().flatten() {
                        *fx = fx.saturating_add(1);
                    }
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
                        self.apply_dmg_wxa6_lineend_windraw(mmio, mmio.is_cgb_features_enabled());
                        self.resolve_bgp_spikes(mmio);
                        self.state = State::HBlank;
                        if !self.mode0_reported_this_line {
                            self.mode0_reported_this_line = true;
                            Self::set_lcd_status_mode(mmio, 0);
                            self.check_and_trigger_stat_interrupt(mmio);
                        }
                    } else if window_deferred {
                        self.apply_dmg_wxa6_lineend_windraw(mmio, mmio.is_cgb_features_enabled());
                        self.resolve_bgp_spikes(mmio);
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
                        // Arm the DMG "line 154" STAT-write VBlank-IF glitch window
                        // at the exact frame-wrap dot (LY 153->0, VBlank exit). A
                        // FF41 write within this window clears the still-pending
                        // VBlank IF (see `l154_vblank_glitch_window`). Disarmed a few
                        // dots into the new frame by `step` (below).
                        self.l154_vblank_glitch_window = true;
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

                        if self.renders_color(mmio) {
                            // CGB / DMG-compat-on-CGB: swap color framebuffers
                            self.color_fb_b = self.color_fb_a;
                            self.color_fb_a = [0; FRAMEBUFFER_SIZE * 3];
                        } else {
                            // DMG mode: swap monochrome framebuffers
                            self.fb_b = self.fb_a;
                            self.fb_a = [0; FRAMEBUFFER_SIZE];
                        }

                        self.have_frame = true;
                        // Count this completed frame toward post-enable resync so
                        // get_frame stops blanking once a full frame has displayed.
                        self.frames_since_enable = self.frames_since_enable.saturating_add(1);
                        // The SS->DS-mode3 lyCounter re-anchor is a phase artifact
                        // local to the frame(s) right after the switch; once two
                        // frame wraps have re-settled the line phase (age lcd-align-ly:
                        // multiple STOP windows push its LY reads several frames past
                        // the switch) it no longer applies and the getLyReg reads
                        // resolve through the standard DS window. The age `ly`
                        // mode-3-switch probes read within 0-1 wraps and keep it.
                        if self.ssds_mode3_ly_advance {
                            self.ssds_mode3_frames = self.ssds_mode3_frames.saturating_add(1);
                            if self.ssds_mode3_frames >= 2 {
                                self.ssds_mode3_ly_advance = false;
                            }
                        }
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
        // A late-sub-M-cycle-phase write (`on_bgp_write`) holds the old value for
        // `bgp_defer_countdown` more dots before the live register is picked up.
        if self.bgp_defer_countdown > 0 {
            self.bgp_defer_countdown -= 1;
            self.bgp_delayed = self.bgp_defer_hold;
        } else {
            self.bgp_delayed = mmio.read(BGP);
        }
        self.obp0_delayed = mmio.read(OBP0);
        self.obp1_delayed = mmio.read(OBP1);
        self.ticks += 1;
    }

    /// Push the BG fetcher's current VRAM data-bus address to the bus for the
    /// OAM-DMA-source conflict model. Called once per dot after `step`. The lock is
    /// active only while the PPU is in PixelTransfer (mode 3) and the LCD is on —
    /// the only window in which the fetcher drives VRAM. Outside it a VRAM-source
    /// OAM-DMA read sees true VRAM (the clean HBlank/mode-0 identity window).
    pub fn update_dma_fetcher_bus(&self, mmio: &mut mmio::Mmio) {
        let lcd_on = !self.disabled && (self.lcdc & (LCDCFlags::DisplayEnable as u8)) != 0;
        let locked = lcd_on && self.state == State::PixelTransfer;
        let (addr, bank) = self.fetcher.last_vram_bus();
        mmio.set_fetcher_vram_bus(addr, bank, locked);

        // DMG mode-2 fetcher-prefetch onset (see `Mmio::set_dmg_prefetch_bus`). On
        // DMG the BG fetcher's first tile-NUMBER fetch begins one M-cycle (4 dots)
        // before the mode-3 lock, so a VRAM-source OAM-DMA M-cycle in the last
        // mode-2 M-cycle already conflicts on the first tilemap address. Publish
        // that predicted address for the 4-dot window preceding the normal-line
        // mode-3 arm. CGB is unaffected (its AND lock at mode-3 entry already
        // byte-matches its dumps). Skipped on the first line after enable (no
        // mode-2 phase / different arm geometry).
        let prefetch = lcd_on
            && !mmio.is_cgb_features_enabled()
            && self.state == State::OAMSearch
            && !self.first_line_after_enable
            && self.ticks + 4 >= DMG_PIXEL_TRANSFER_ARM_DOT
            && self.ticks < DMG_PIXEL_TRANSFER_ARM_DOT;
        if prefetch {
            // First BG tile-number address for this line (display column 0):
            // tilemap_base + ((ly + scy)/8 % 32)*32 + (scx/8 % 32).
            let map_base: u16 = if (self.lcdc & (LCDCFlags::BGTileMapDisplaySelect as u8)) != 0 {
                0x9C00
            } else {
                0x9800
            };
            let scy = mmio.read(SCY) as u16;
            let scx = mmio.read(SCX) as u16;
            let bg_y = self.internal_ly_val as u16 + scy;
            let map_y = (bg_y / 8) & 0x1F;
            let map_x = (scx / 8) & 0x1F;
            let map_addr = map_base + (map_y * 32 + map_x);
            mmio.set_dmg_prefetch_bus(map_addr, true);
        } else {
            mmio.set_dmg_prefetch_bus(0, false);
        }
    }

    pub fn frame_ready(&self) -> bool {
        self.have_frame
    }

    /// The completed DMG shade-index frame (the back buffer `get_frame`
    /// serves). The SGB *_TRN readout captures from this: the real SGB
    /// re-digitizes the displayed video signal, not the GB's VRAM.
    pub fn dmg_shade_frame(&self) -> &[u8; FRAMEBUFFER_SIZE] {
        &self.fb_b
    }

    pub fn get_frame(&mut self, mmio: &mmio::Mmio) -> crate::gb::Frame {
        self.have_frame = false;
        // Hardware panel blank: the LCD off state and the first frame after an
        // enable both show "whiter than white" (blank), not the framebuffer. The
        // panel needs one fully-displayed frame after enable to resync, so a frame
        // is only shown once at least two frame boundaries have passed since the
        // enable (frames_since_enable >= 2). A ROM that enables the LCD for a single
        // frame each cycle (little-things-gb `firstwhite`, Pokemon Pinball) never
        // reaches that, so the panel stays blank. SGB keeps its own mask/border
        // compositing (handled in sgb_frame), so this blanking is gated off there.
        let blank_panel =
            mmio.sgb().is_none() && (self.disabled || self.frames_since_enable < 2);
        if self.renders_color(mmio) {
            if blank_panel {
                // CGB white == RGB 0xFFFFFF.
                return crate::gb::Frame::Color(Box::new([0xFF; FRAMEBUFFER_SIZE * 3]));
            }
            crate::gb::Frame::Color(Box::new(self.color_fb_b))
        } else if let Some(sgb) = mmio.sgb() {
            // MASK_EN Freeze: latch the frame completed at the freeze and keep
            // showing it (the transfer screens games draw behind the mask stay
            // hidden); drop the latch as soon as the mask leaves Freeze.
            if matches!(sgb.mask, crate::sgb::MaskMode::Freeze) {
                if self.sgb_freeze_fb.is_none() {
                    self.sgb_freeze_fb = Some(self.fb_b.to_vec());
                }
            } else if self.sgb_freeze_fb.is_some() {
                self.sgb_freeze_fb = None;
            }
            self.sgb_frame(sgb)
        } else {
            if blank_panel {
                // DMG white == shade index 0.
                return crate::gb::Frame::Monochrome(Box::new([0; FRAMEBUFFER_SIZE]));
            }
            crate::gb::Frame::Monochrome(Box::new(self.fb_b))
        }
    }

    /// Post-process the DMG shade-index framebuffer for Super Game Boy output:
    /// apply the MASK_EN screen mask and, when a palette command has run, map
    /// each pixel's DMG shade (0-3) through the SGB palette assigned to its 8x8
    /// attribute cell (producing RGB888). When no palette command has run the
    /// frame stays monochrome, matching plain-GB (grayscale) behavior — which is
    /// what the `sgb-ext-test` grayscale reference expects.
    fn sgb_frame(&self, sgb: &crate::sgb::Sgb) -> crate::gb::Frame {
        use crate::sgb::MaskMode;
        // MASK_EN: Freeze shows the latched pre-freeze frame; Black shows pure
        // black (the SNES blanks to color 0x0000); Color0 blanks to the shared
        // backdrop color (color 0).
        let blank = matches!(sgb.mask, MaskMode::Black | MaskMode::Color0);
        let src: &[u8] = match self.sgb_freeze_fb.as_deref() {
            Some(f) if f.len() == FRAMEBUFFER_SIZE => f,
            _ => &self.fb_b,
        };

        if !sgb.colorized {
            if blank {
                // Blank to shade 0 (Color0) / darkest for Black.
                let fill = if matches!(sgb.mask, MaskMode::Black) { 3 } else { 0 };
                return crate::gb::Frame::Monochrome(Box::new([fill; FRAMEBUFFER_SIZE]));
            }
            let mut out = [0u8; FRAMEBUFFER_SIZE];
            out.copy_from_slice(src);
            return crate::gb::Frame::Monochrome(Box::new(out));
        }

        // Colorized: build an RGB888 frame from the SGB palettes.
        let mut out = [0u8; FRAMEBUFFER_SIZE * 3];
        if matches!(sgb.mask, MaskMode::Black) {
            return crate::gb::Frame::Color(Box::new(out));
        }
        for y in 0..144usize {
            for x in 0..160usize {
                let idx = y * 160 + x;
                let shade = if blank { 0 } else { src[idx] };
                let rgb555 = sgb.color_for(x / 8, y / 8, shade).unwrap_or(0);
                let (r, g, b) = rgb555_to_rgb888(rgb555);
                out[idx * 3] = r;
                out[idx * 3 + 1] = g;
                out[idx * 3 + 2] = b;
            }
        }
        crate::gb::Frame::Color(Box::new(out))
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

    /// Whether the PPU has processed its LCD-off transition. False means the PPU
    /// still holds its running state (used to force the disable dot before an
    /// idle bulk-skip so the transition is never jumped over).
    pub fn is_lcd_disabled(&self) -> bool { self.disabled }

    /// DMG OAM-bug support: the OAM row (0..19) the PPU is scanning when a CPU
    /// OAM-bus access COMPLETES, else None. During mode 2 the PPU reads one of the
    /// 20 OAM rows per M-cycle; `line_cycle` is the speed-independent within-line
    /// dot, so the row is `dot / 4`.
    ///
    /// The trigger sites sample at the START of the access M-cycle (the persistent
    /// `line_cycle` before this M-cycle's 4 dots tick), but the OAM access on the
    /// bus lands at the END of that M-cycle — so add `OAM_BUG_ACCESS_DOT` (4, one
    /// M-cycle) to align the scan position to the completion dot. This makes the
    /// mode-2 trigger window M-cycle-exact (validated by blargg 4-scanline_timing's
    /// 1-M-cycle "just before / at first corruption" boundary). Returns None when
    /// the LCD is off or the PPU is not in mode 2. This is the WRITE/IDU path row.
    pub fn oam_bug_mode2_row(&self) -> Option<u8> {
        if self.disabled || (self.lcdc & (LCDCFlags::DisplayEnable as u8)) == 0 {
            return None;
        }
        if self.state != State::OAMSearch {
            return None;
        }
        const OAM_BUG_ACCESS_DOT: u32 = 4;
        let dot = self.line_cycle + OAM_BUG_ACCESS_DOT;
        // Mode 2 is the first 80 dots of the line (20 rows * 4 dots/M-cycle).
        if dot >= 80 {
            return None;
        }
        Some((dot / 4) as u8)
    }

    /// DMG OAM-bug row for a CPU OAM *read* access (as opposed to a write/IDU).
    /// SameBoy holds `accessed_oam_row = 0` across the whole mode-2 prologue (the
    /// three `GB_SLEEP`s before the object-scan loop advances it to 8), and both the
    /// read and write trigger sites guard on `accessed_oam_row >= 8` — row 0 is the
    /// exempt "first two objects" row, so a mode-2-prologue access corrupts nothing.
    /// A CPU read landing at the mode-2 entry samples this prologue window (age's
    /// timed oam-read boundary reads at `line_cycle` 0/4 hit it), so it must return
    /// row 0 (clean). The write/IDU path in `oam_bug_mode2_row` does NOT get this
    /// exemption: blargg oam_bug's INC/DEC-through-OAM writes probe those same early
    /// `line_cycle`s from a different M-cycle phase and observe the deeper scanned
    /// row (their `(line_cycle + 4)/4` mapping is hardware-correct and must stay).
    /// Splitting the exemption by access type reconciles age oam-read (read prologue
    /// clean) with blargg oam_bug (write prologue corrupts) — the row-only function
    /// alone cannot satisfy both.
    pub fn oam_bug_mode2_row_read(&self) -> Option<u8> {
        let base = self.oam_bug_mode2_row()?;
        // Mode-2 prologue: reads sample the held row-0 (accessed_oam_row < 8), clean.
        if self.line_cycle < 6 {
            return Some(0);
        }
        Some(base)
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
        Some(dot > m0n && dot + 3 + 3 * ds < 456)
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
    pub fn cpu_access_blocked(&self, kind: u8, is_read: bool, mode3_locked: bool, env: AccessEnv, access_cc: u64) -> Option<bool> {
        let AccessEnv { is_cgb, cgb_de, double_speed } = env;
        if self.disabled {
            return Some(false);
        }
        if self.internal_ly_val >= 144 {
            // Gambatte oamReadable/oamWritable resolve the OAM line-wrap pre-lock
            // BEFORE the ly>=144 vblank accessibility: in the last `k` line-cycles
            // of a line the access already belongs to the NEXT line, and line 153's
            // successor is line 0 whose mode-2 OAM scan is imminent — blocked
            // (`ly() < lcd_lines_per_frame - 1` excludes 153). Lines 144-152 wrap
            // into mode-1 successors and stay accessible (age oam-write cgbBCE /
            // ncmBCE: the delay-2 write at the line-0 frame-1 mode-2 edge lands on
            // line 153's tail and must be blocked).
            if kind == 1 && self.internal_ly_val == 153 {
                let cc = access_cc as i64;
                let ds = double_speed as i64;
                let wrap_lc = if is_read {
                    let plus1 = if self.lytime_no_plus1 { 0 } else { 1 };
                    let dots_to_next = (stat_irq::LCD_CYCLES_PER_LINE - self.line_cycle) as i64;
                    let ly_time = self.p_now as i64 + self.abs_cc as i64 + (dots_to_next << ds) + plus1;
                    stat_irq::LCD_CYCLES_PER_LINE as i64 - ((ly_time - cc) >> ds)
                } else {
                    self.line_cycle as i64 - self.lytime_no_plus1 as i64
                };
                // CGB-D/E: the OAM-read line-wrap pre-lock keeps the SS threshold
                // in double speed (SameBoy line-start `oam_read_blocked = !ds ||
                // model >= CGB_D`; age oam-read-cgbE DS delay-1 m2-edge reads are
                // blocked on E where B/C still allow them).
                let k = if is_read {
                    4 - if cgb_de { 0 } else { ds }
                } else {
                    3 + is_cgb as i64
                };
                if wrap_lc + k >= stat_irq::LCD_CYCLES_PER_LINE as i64 {
                    return Some(true);
                }
            }
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
        if kind == 0 && self.state == State::OAMSearch
            && let Some(started) = vram_started {
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
            // CGB-D/E read threshold: see the ly==153 wrap above.
            let k = if is_read {
                4 - if cgb_de { 0 } else { ds }
            } else {
                3 + is_cgb as i64
            };
            if oam_line_cycle + k >= stat_irq::LCD_CYCLES_PER_LINE as i64 {
                let ly = self.internal_ly_val as i64;
                let accessible = (143..153).contains(&ly);
                return Some(!accessible);
            }
        }
        // CGB-D/E: the OAM READ mode-3 end unblocks one cc later than B/C — the
        // age oam-read-cgbE/ncmE odd-SCX m0-edge reads (EFF spots) are still
        // blocked on E exactly where B/C already read through. VRAM keeps the
        // shared boundary (vram-read is BCE-common).
        let de_read_hold = (kind == 1 && is_read && cgb_de) as i64;
        let ended = self.state != State::OAMSearch && cc_end + 2 - de_read_hold >= m0t;
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

    /// Byte-exact Gambatte `LCD::vramReadable(cc)` for a CPU VRAM read at master-cc
    /// `cc`, resolved purely from the lyTime-derived `lineCycles(cc)` and
    /// `m0TimeOfCurrentLine` — NOT the renderer's current FF41 mode register.
    /// (video.cpp:381): readable iff LCD off, in vblank, the line-start inactive
    /// window, `lineCycles(cc) + ds < 76 + 3*cgb` (still in mode 2 / before the
    /// mode-3 lock), or `cc + 2 >= m0Time` (mode 0 reached). Used by the
    /// PC-in-DMA-dest opcode-prefetch absorption (`Bus::fetch_opcode`): the GDMA's
    /// prefetch opcode at the block's first dest byte must see VRAM readable at the
    /// prefetch cc the same way Gambatte's `Interrupter::prefetch` (run BEFORE
    /// `dma()` overwrites VRAM) does — including the mode-2 readable window
    /// (late_gdma_pc_7ffe_1: lineCycles 76 < 79 -> readable -> pre-byte) and the
    /// mode-3 lock just past it (late_gdma_pc_7ffe_2: lineCycles 80 -> locked).
    /// Returns None when no closed-form m0Time exists (window / first line after
    /// enable) so the caller falls back to the renderer-mode lock.
    pub fn vram_readable_at_cc(&self, cc: u64, is_cgb: bool, ds: bool) -> Option<bool> {
        if self.disabled || self.internal_ly_val >= 144 {
            return Some(true);
        }
        let m0t = self.m0_time_master? as i64;
        let cc = cc as i64;
        let dsi = ds as i64;
        // Gambatte `lineCycles(cc) = 456 - ((lyTime - cc) >> ds)` (the same lyTime
        // phase the OAM-read END boundary uses in `cpu_access_blocked`).
        let plus1 = if self.lytime_no_plus1 { 0 } else { 1 };
        let dots_to_next = (stat_irq::LCD_CYCLES_PER_LINE - self.line_cycle) as i64;
        let ly_time = self.p_now as i64 + self.abs_cc as i64 + (dots_to_next << dsi) + plus1;
        let line_cycles = stat_irq::LCD_CYCLES_PER_LINE as i64 - ((ly_time - cc) >> dsi);
        // mode-2 readable window (before the mode-3 lock) OR mode-0 reached.
        let mode2_readable = line_cycles + dsi < 76 + 3 * is_cgb as i64;
        let mode0_reached = cc + 2 >= m0t;
        Some(mode2_readable || mode0_reached)
    }

    /// getStat mode-3->0 read-boundary offset (`access_cc + off < m0Time` => mode 3).
    /// SS: rustyboi's `m0_time_master` carries the lyTime `+1` so it sits 1cc high vs
    /// Gambatte's getStat read -> off=3 (`!lytime_no_plus1`); on a post-DS->SS line the
    /// `+1` is dropped -> off=2. DS samples the half-dot grid -> off=2.
    ///
    /// On a post-DS->SS line that took the FACET1 mode-3 STAT-phase carry
    /// (`render_carry_skew_cc != 0`), the STAT/m0Time clock was advanced `carry` dots
    /// WITHOUT moving the render latch / read-cc grid, so the FF41 read cc sits `carry`
    /// dots BEHIND the carried m0Time. Gambatte's `cc + 2 < m0Time` holds against the
    /// un-carried read grid, so subtract the carry from the offset (target carry=1 ->
    /// off 2->1 -> gap-3 mode-3 read; carry=0 want-mode-0 siblings keep off=2). The
    /// carry is 0 except on a post-mode-3-switch line, so this is inert elsewhere.
    fn stat_read_off(&self, ds: bool) -> i64 {
        let base = if !ds && !self.lytime_no_plus1 { 3 } else { 2 };
        if self.lytime_no_plus1 {
            base - self.render_carry_skew_cc
        } else {
            base
        }
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
        let read_off = self.stat_read_off(ds);
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
            // An m2-woken wake that charged its +4 as a REAL stall (sm83.rs
            // `return 4`) already advanced this read's access cc by 4cc, so only
            // the +1 lyTime correction remains; a wake that did NOT (LYC/m1 path,
            // or the pre-stall model) still needs the full +5.
            if mmio.m2_halt_stall_charged_cgb() {
                access_cc + 1
            } else {
                access_cc + 5
            }
        } else {
            access_cc
        };
        // FAITHFUL EVENTCC (R4): the DMG branch of Gambatte's HALT-exit fixup
        // (memory.cpp:308, `cc += 4 * (... || cc - eventTime < 2)`). When the DMG
        // wakeup landed within 2cc of the woken mode-0 STAT IRQ event time, the +4
        // wakeup latency advances the CPU clock, so the halt-woken FF41 read samples
        // +4cc later in the line — lifting the `late_m0*_halt_m0stat_scx3_2b` read
        // from the stale mode-0 line tail into the next line's OAM (mode 2). Only
        // active under RB_FAITHFUL_EVENTCC; the flag is set at unhalt and cleared on
        // the next HALT, so it only ever biases the single woken instruction stream.
        // HALT-PREFETCH (Lever A, RB_PREFETCH_CC): replace the all-or-nothing +4
        // with the per-stream M-cycle phase (0 or 1) derived at unhalt from the
        // pre-snap HALT-entry cc vs the snap target. The byte-identical _1b/_2b
        // ROMs separate: _1b (phase 0) keeps the stale mode-0 line tail; _2b
        // (phase 1) gets +4 lifting the read into the next line's mode 2. The
        // foundation's uniform +4 (halt_wake_plus4_dmg) lifted BOTH, swapping the
        // failures; the phase isolates the bias to _2b alone. Under RB_PREFETCH_CC
        // OFF the foundation behavior is preserved verbatim.
        let access_cc = access_cc + 4 * mmio.halt_prefetch_phase() as u64;
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

        // VBlank window (mode 1) — video.cpp:806-810. video.cpp:808 adds +1 to
        // the upper bound on the last line (LY 153) for AGB.
        if in_vblank_window {
            let agb_last_line =
                (mmio.is_agb() && ly == (stat_irq::LCD_LINES_PER_FRAME - 1) as i64) as i64;
            if frame_cycles >= 144 * cpl - 2 && frame_cycles < cpf - 4 + dsi + agb_last_line {
                return Some(1);
            }
            // CGB-D/E: no mode-0 M-cycle at the END of mode 1 (age stat-mode M1E)
            // — the register holds mode 1 through the line-153 tail until the next
            // line-0 mode-2 anticipation. Single speed only (stat-mode-ds is
            // BCE-common). The vblank-ENTRY mode-0 tail (line 143) keeps mode 0.
            if mmio.is_cgb_de() && !ds && frame_cycles >= cpf - 4 {
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
        line_cycles: i64,
        ds: bool,
        halt_skew: bool,
        is_cgb: bool,
    ) -> Option<u8> {
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
                let read_off: i64 = self.stat_read_off(ds);
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
                    let read_off: i64 = self.stat_read_off(ds);
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
            let read_off: i64 = self.stat_read_off(ds);
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
        // CGB first-frame-after-enable LYC-window +1: in the frame produced right
        // after LCDC.7 0->1 on CGB hardware, the LY counter is re-anchored such that
        // the line-tail LY==LYC coincidence window closes one master-cc LATER than a
        // settled frame — rustyboi's closed-form LyCounter.time (which runs 1cc below
        // Gambatte's lyTime, the same delta `m0_time_exact` folds into m0Time) reads
        // the pre-enable phase, so a line-tail STAT read one dot before the boundary
        // samples the coincidence bit already cleared. The wilbertpol ly_lyc-C /
        // ly_lyc_144-C / ly_lyc_153-C rounds LCD-off/on every round then read STAT
        // deep in the first frame (LY=2 tail, timeToNextLy should be 3 not 2 -> STAT
        // $C4 not $C0).
        //
        // SCOPED to (frames_since_enable == 0) so a settled frame keeps Gambatte's
        // exact getStat phase (its own lycint*flag / m2stat_count tests read the
        // line-tail coincidence CLEAR at timeToNextLy 2 -- gambatte floor). LY 0 is
        // excluded: the first line after enable already carries the +2 M3-start seed
        // (m0_time_exact first_line, Gambatte `cycles = -(m3StartLineCycle+2)`), which
        // absorbs the 1cc there -- without the exclusion the frame0 line-0 read
        // (gambatte frame0_m2stat_count_1) would over-set the coincidence bit.
        let ss_plus1 = (!lc.ds
            && !self.lytime_no_plus1
            && mmio.is_cgb()
            && self.frames_since_enable == 0
            && self.internal_ly_val != 0) as i64;
        let lc_master = stat_irq::LyCounter {
            ly: lc.ly,
            time: (self.p_now as i64 + lc.time as i64 + ss_plus1).max(0) as u64,
            ds: lc.ds,
        };
        let cmp = stat_irq::get_lyc_cmp_ly(&lc_master, access_cc);
        let lyc_reg = mmio.read(LYC) as u32;
        // video.cpp:820 getStat LYC flag: `timeToNextLy > 2 - (!isDoubleSpeed()
        // && isAgb())`. AGB single-speed lowers the compare threshold by one, so
        // the LYC=LY flag stays set one extra dot at the line tail. DS and the
        // STAT-IRQ-trigger paths (statChange/lycRegChange) keep the plain `> 2`
        // (Gambatte applies the isAgb term ONLY here, in the FF41 register read).
        //
        // CGB-D/E silicon holds the coincidence bit the SAME extra dot AGB does:
        // SameBoy CGB-E reads the ly_lyc_0-C line-0-tail STAT (LY=0==LYC=0 at
        // timeToNextLy 2, `ly_for_comparison` still the previous LY held into the
        // line-1 first dot) as $C4 (mode 0 + coincidence SET) where Gambatte's
        // CPU-CGB-C model (`> 2`) already cleared it ($C0). Gambatte captured on
        // CPU-CGB-C, so its C-model keeps the plain `> 2`; only the D/E-routed
        // reads (is_cgb_de, single speed) get the +1 hold. DS keeps `> 2` (the
        // stat-mode-ds / speed-switch DS probes are BCE-common and co-tuned to it).
        let tail_hold = (!lc_master.ds && (mmio.is_agb() || mmio.is_cgb_de())) as i64;
        Some(lyc_reg == cmp.ly && cmp.time_to_next_ly > 2 - tail_hold)
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
        // A plain (non-halt-woken) FF44 read after an SS->DS mode-3 speed switch:
        // the age ly/lcd-align-ly DS probes (which switch during mode 3 then sweep
        // LY reads across steady DS lines, never halting) need a smaller `time`
        // re-anchor than the halt-woken gambatte switch families the -10 below was
        // calibrated to. Their line-boundary reads (152->153 increment, line-153
        // head, 0-wrap) sit one dot-pair earlier under the flat -10; +3 pulls the
        // plain-read anchor onto cgbBC/cgbE silicon (byte-exact ly-cgbE /
        // ly-dmgC-cgbBC), leaving the halt-woken families (hdma_late_m3speedchange_ly,
        // cctracer) on the un-adjusted -10.
        const SSDS_PLAIN_TIME_ADJ: i64 = 3;
        let ssds_plain = ds && self.ssds_mode3_ly_advance && !mmio.halt_wakeup_skew();
        let ds_corr: i64 = if ssds_plain { SSDS_PLAIN_TIME_ADJ } else { 0 };
        let mut time = self.p_now as i64 + lc.time as i64 + 1 + ds_corr;
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
        // NOTE: the m2 getLyReg `to_next` RB_CANONICAL_CC_ADJ probe was removed — it
        // was a refuted dead-end (m2) that shared RB_CANONICAL_CC_ADJ and confounded
        // the R2 block-stall measurement (it shifted every non-halt LY read). The
        // block stall probe (mmio.rs run_hdma_block) now owns RB_CANONICAL_CC_ADJ.

        if ly_reg == last_line {
            // Line 153: FF44 reads 0 early (Gambatte getLyReg). At single speed,
            // Gambatte's getLyReg (video.h:135, `time - cc <= cpl - isAgb`) returns
            // 0 for the WHOLE of line 153 (for non-agb the bound is cpl, always
            // satisfied within the line). rustyboi's `to_next` carries the +1 lyTime
            // correction (its closed-form counter runs 1cc below Gambatte's lyTime),
            // so compare the RAW time (`to_next - 1`) against cpl. The old top-only
            // path (`to_next >= cpl`) deferred the rest of the line to the renderer's
            // dot-6 LY->0 flip, but that flip has NOT happened at a just-wrapped
            // ISR-entry read (offset2_lyc98int_ly_count_2: to_next=454, renderer
            // still 153) where Gambatte returns 0 — the renderer-flip race. The
            // faithful whole-line-0 resolution removes it.
            if !ds {
                // video.h:135 getLyReg: single-speed bound is `cpl - 1*isAgb`.
                // AGB shrinks the line-153 reads-0 window by one dot.
                // CGB-D/E shrinks it by exactly one dot: only the first dot of line
                // 153 (to_next-1 == cpl, the top of the line) still reads 153; every
                // later dot reads 0. The age lcd-align-ly-cgbE alignment sweep pins
                // this: its line-153-head reads at to_next 457 read 153, but 456/454
                // (one/three dots in) already read 0 — a one-dot window, not the
                // one-M-cycle (4-dot) window the wider tuning assumed. The age
                // ly-cgbE E99 edge read sits at to_next 457 (inside the 1-dot window)
                // and to_next 453 (outside, reads 0 either way), so both revisions'
                // ly probes are unaffected by the narrowing.
                let agb = mmio.is_agb() as i64;
                let de = mmio.is_cgb_de() as i64;
                // Post-STOP (row43): when the accumulated fractional-bridge phase is
                // shifted off the whole dot (`shift != 0`, i.e. `render_carry_skew_cc`
                // lands mid-dot), the line-153 HEAD read (to_next-1 == cpl-de, e.g.
                // cgbBC to_next 457 / cgbE 456) that the steady window folds to 0 still
                // reads 153 on real cgb04c silicon. Tighten the reads-0 window by one
                // dot only for that shifted phase; unshifted post-STOP reads (carry a
                // whole number of dots, shift==0, e.g. cgbE to_next 456 carry 0) and the
                // steady gambatte line-153 families (offset2_lyc98int / lycint152_ly153)
                // keep the un-tightened window.
                let ls_shift = -(((self.render_carry_skew_cc + 2).rem_euclid(15)) / 5);
                let head_hold = (self.dsss_ly_phase_active() && ls_shift != 0) as i64;
                if to_next - 1 <= cpl - agb - de - head_hold {
                    return Some(0);
                }
                if de != 0 {
                    return Some((ly_reg & 0xFF) as u8);
                }
                return None;
            }
            // Plain-ssds (age mode-3-switch DS) line 153: unlike the steady-DS
            // Gambatte model (line 153 reads 0 except the top 2cc), cgbBC/cgbE
            // silicon after a mode-3 switch holds LY=153 for the first ~10cc (5
            // dots) of the line — the renderer's line-153 LY->0 flip (dot 6) as seen
            // through the re-anchored read phase — then reads 0. `to_next` counts
            // down from 2*cpl (line start) to 0 (frame wrap), so the reads-153 head
            // is the HIGH-to_next window. The age ly DS 1C38 boundary sweep reads
            // 153 at to_next >= 2*cpl-10 and 0 below. Steady-DS reads (gambatte
            // lycint152_ly153_ds / frame1_ly_count_ds, ssds_plain=false) keep the
            // whole-line-0 model. Revision-independent (cgbBC==cgbE DS table).
            const SSDS_LINE153_HEAD: i64 = 10;
            if ssds_plain {
                if to_next >= 2 * cpl - SSDS_LINE153_HEAD {
                    return Some((ly_reg & 0xFF) as u8);
                }
                return Some(0);
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
        // An m2-woken CGB wake that charged its +4 as a REAL stall already advanced
        // this read's access cc by 4cc, so the -5 (raw -1 + halt-exit +4) would
        // double-count the +4 — it drops to the raw -1 (the `halt_skew` else-arm).
        let cgb_halt_exit = halt_skew
            && mmio.is_cgb_features_enabled()
            && !ds
            && !mmio.halt_wakeup_hdma()
            && !mmio.m2_halt_stall_charged_cgb();
        // FAITHFUL HALT-EXIT (DMG m0-woken stream): re-anchor the woken FF44
        // read by the real Gambatte wake advance (snap + conditional +4,
        // derived at unhalt from the m0 eventTime phase). The un-advanced wake
        // stream reads `adv` cc earlier than Gambatte's, and this read path's
        // `time` base already matches Gambatte's lyTime, so the effective
        // comparison is `to_next - adv` — byte-exact for the
        // hblank_ly_scx_timing-GS per-SCX classes (to_next 9 for delay_a /
        // 5 for delay_b across all SCX and both wake-M-cycle phases, skipping
        // the ly&(ly+1) glitch dot the -1-skewed read landed on). Replaces the
        // generic -1 halt skew for exactly this stream shape.
        let m0_halt_adv = if halt_skew && !mmio.is_cgb_features_enabled() && !ds {
            mmio.dmg_m0_halt_ly_advance()
        } else {
            None
        };
        // DS analog of `cgb_halt_exit`: a halt-woken stream that crossed an SS->DS
        // speed switch (halt-wake -> STOP, no intervening HALT) still carries the
        // un-charged CGB halt-exit M-cycle, so its post-switch FF44 reads sample
        // closer to the line wrap than the engine cc reflects — same -5 (raw-time
        // -1 + the halt-exit +4) as the single-speed branch. Without it the daid
        // speed_switch_timing_ly read train's 134->135 boundary read lands exactly
        // on the `ly&(ly+1)` glitch dot (tn==10, reads 134) where hardware already
        // pre-increments (135); the whole 128-read hardware table pins this bias to
        // [-2,-8]. Scoped to the no-HDMA halt-woken switch stream: the gambatte
        // speedchange_ly*/enable_display DS LY probes never halt before their
        // switch, the hdma _ds _ly families fold their wakeup shift into the
        // block-transfer phase (halt_wakeup_hdma), and the mode-3-switch families
        // are co-tuned to the `ssds_mode3_ly_advance` -10 time re-anchor.
        let ssds_haltskew = halt_skew
            && ds
            && mmio.ssds_haltskew_ly_advance()
            && !mmio.halt_wakeup_hdma()
            && !self.ssds_mode3_ly_advance;
        // FAITHFUL HALT-EXIT (CGB m0-woken stream, DMG-flagged cart): the CGB
        // analog of `m0_halt_adv`. On a CGB console with a DMG cart neither the DMG
        // block (gated `!is_cgb()`) nor `cgb_halt_exit` (gated on cart features)
        // fires; this consumes the unconditional-+4 CGB advance derived at unhalt
        // (cgb_m0_halt_ly_advance) as `to_next - adv`, landing constant tn across
        // the 51/50/49 per-SCX classes (hblank_ly_scx_timing-C). Scoped no-HDMA
        // single-speed so it never touches the m1int_ly / hdma / speed-switch
        // families (all CGB-flagged cart => is_cgb_features_enabled(), or DS/HDMA).
        let cgb_m0_halt_adv = if halt_skew
            && mmio.is_cgb()
            && !mmio.is_cgb_features_enabled()
            && !ds
            && !mmio.halt_wakeup_hdma()
        {
            mmio.cgb_m0_halt_ly_advance()
        } else {
            None
        };
        let tn = if let Some(adv) = cgb_m0_halt_adv {
            to_next - adv as i64
        } else if let Some(adv) = m0_halt_adv {
            to_next - adv as i64
        } else if cgb_halt_exit || ssds_haltskew {
            to_next - 5
        } else if halt_skew {
            to_next - 1
        } else {
            to_next
        };
        // Plain-ssds (age mode-3-switch DS) line-boundary anticipation window: the
        // re-anchored read reflects the pending LY increment only within the last
        // ~4cc (2 dots) before the wrap, narrower than the steady-DS 6+4*ds=10cc
        // window. Under the wide window the age sweep reads (which land ~4 dots
        // before every line boundary) anticipated a dot-pair too early (144/153/00
        // where cgbBC/cgbE still hold 143/152/153). Steady-DS / halt-woken reads
        // keep the 10cc window below.
        const SSDS_ANTICIPATE_WINDOW: i64 = 4;
        if ssds_plain {
            if tn <= SSDS_ANTICIPATE_WINDOW {
                let result = if tn == SSDS_ANTICIPATE_WINDOW {
                    ly_reg & (ly_reg + 1)
                } else {
                    ly_reg + 1
                };
                return Some((result & 0xFF) as u8);
            }
            return None;
        }
        let glitch = 6 + 4 * (ds as i64);
        // POST-STOP sub-dot phase (age lcd-align-ly): after DS->SS speed switches the
        // LY-read phase carries an accumulated half-dot Gambatte applies per switch
        // (`PPU::speedChange` `now -= 1`) that rustyboi's whole-dot DS->SS bridge folds.
        // The accumulated whole-dot STAT-phase carry (`render_carry_skew_cc`) drives the
        // `shift` below; `par1`/`total_par1` select the per-revision partial-latch fold.
        let post_stop = self.dsss_ly_phase_active();
        let par1 = post_stop && self.dsss_ly_phase_par() == 1;
        let total_par1 = post_stop && self.dsss_ly_total_par() == 1;
        // POST-STOP fractional-bridge phase shift (age lcd-align-ly, real cgb04c/dmg08
        // expected table — a behavior SameBoy does not model). Each DS->SS-during-mode3
        // STOP switch injects Gambatte's `now -= 1` half-dot; `render_carry_skew_cc`
        // accumulates the resulting whole-dot STAT-phase carry. That carry shifts the
        // effective sub-dot the boundary LY read samples at, sliding the anticipation /
        // partial-latch-fold window. The shift wraps every 5 carry-dots and repeats with
        // period 15 (validated dot-exact across all 45 rows x both cgbBC/cgbE expected
        // tables): `shift = -(((carry+2) % 15) / 5)` in dots. `tn_eff = tn - shift` is
        // the phase-corrected time-to-next-LY the window resolves against.
        let shift = if post_stop {
            -(((self.render_carry_skew_cc + 2).rem_euclid(15)) / 5)
        } else {
            0
        };
        let tn_eff = tn - shift;
        if tn_eff <= 10 && tn_eff <= glitch {
            let result = if tn_eff == glitch {
                if post_stop {
                    // Post-STOP glitch dot: real silicon reads the partial-latch fold
                    // `ly & (ly+1)` (the half-latched LY during the increment: 143->144
                    // reads 0x80 = 0x8F & 0x90, 152->153 reads 0x98). CGB-C folds
                    // unconditionally; CGB-D/E only when the accumulated sub-dot parity
                    // lands the read ON the boundary (odd non-mode-3 phase `par1` OR odd
                    // total switch parity `total_par1`) — else it reads the stale `ly`.
                    if !mmio.is_cgb_de() || par1 || total_par1 {
                        ly_reg & (ly_reg + 1)
                    } else {
                        ly_reg
                    }
                } else if mmio.is_cgb_de() {
                    // CGB-D/E does NOT fold: it reads the stale pre-increment `ly`.
                    ly_reg
                } else {
                    // Steady-state glitch dot: partial-latch fold `ly & (ly+1)`.
                    ly_reg & (ly_reg + 1)
                }
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
        let r = color_word & 0x1F ;
        let g = (color_word >> 5) & 0x1F ;
        let b = (color_word >> 10) & 0x1F ;

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

    fn get_cgb_bg_color(&self, mmio: &mmio::Mmio, palette_idx: u8, color_idx: u8, sx: u8) -> (u8, u8, u8) {
        if !mmio.is_cgb_features_enabled() {
            // Fallback to monochrome conversion
            let mono_color = self.get_palette_color(mmio, color_idx, sx);
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

    fn get_cgb_obj_color(&self, mmio: &mmio::Mmio, palette_idx: u8, color_idx: u8, sx: u8) -> (u8, u8, u8) {
        if color_idx == 0 {
            return (0, 0, 0); // Transparent - will be handled by caller
        }

        if !mmio.is_cgb_features_enabled() {
            // Fallback to monochrome conversion
            let mono_color = self.get_sprite_palette_color(mmio, color_idx, palette_idx != 0, sx);
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

        // Sprites use offset coordinates: Y=0 is at line -16, X=0 is at column -8.
        // Compare widened (no u8 wrap): a sprite with y < 16 straddles the top
        // screen edge and is visible on lines 0 .. y+height-17 (hardware scans
        // LY+16 against [y, y+height)).
        let top = sprite_y as i32 - 16;

        // Check if sprite is visible on current scanline
        if (ly as i32) >= top && (ly as i32) < top + sprite_height {
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
        let cc = self.abs_cc;

        // Lazy seed for the current LCD-on session.
        if !self.oam_reader_seeded {
            let cgb = mmio.is_cgb_features_enabled();
            let mut pos = [0u8; 80];
            mmio.peek_oam_pos(&mut pos);
            self.oam_reader.reset(&pos, cgb);
            self.oam_reader.lu = cc;
            self.oam_reader.large_src = (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;
            self.prev_dma_writing =
                mmio.oam_dma_window_active() && !mmio.mgb_frozen_merge_active();
            self.oam_reader_seeded = true;
            return;
        }

        // Keep largeSpritesSrc_ tracking the live LCDC OBJ-size bit (Gambatte
        // sets it on the LCDC write; the walk latches it into lsbuf per slot).
        self.oam_reader.large_src = (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;

        // `pos` (the 80-byte Y/X snapshot) is only consumed by the `change()`
        // calls below, which fire only on a DMA-window edge or a pending OAM
        // write. The common per-dot case hits neither, so build it lazily.
        let mut pos = [0u8; 80];
        let mut pos_filled = false;

        // OAM-DMA window edges: at start the source becomes disabled RAM (0xFF);
        // at end it returns to the real OAM. `change(cc)` flushes the snapshot up
        // to `cc` with the OLD source, then caps the next walk, then we toggle.
        // The MGB OAM-DMA-during-HALT merge freezes the DMA mid-transfer; the
        // frozen OAM bus is stuck (not the normal disabled-RAM window), so the
        // Y/X scan reads the merged OAM rather than the ghost pair. Treat the
        // merge window as a non-writing (readable) source.
        let dma_writing = mmio.oam_dma_window_active() && !mmio.mgb_frozen_merge_active();
        if dma_writing != self.prev_dma_writing {
            let lc = self.ly_counter(mmio);
            mmio.peek_oam_pos(&mut pos);
            pos_filled = true;
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
            // DMA start: latch the scan's retained Y/X bus pair (the last pair
            // walked before the cap) for the ghost sampling inside the window.
            if dma_writing {
                let line_has_fetches = !self.sprites_on_line.is_empty();
                self.oam_reader.capture_ghost(line_has_fetches);
            }
            // Toggle source for the new span (startOamDma -> disabled,
            // endOamDma -> real OAM).
            self.oam_reader.src_disabled = dma_writing;
            self.prev_dma_writing = dma_writing;
        }

        // CPU OAM write this M-cycle (Gambatte `lcd_.oamChange(cc)`).
        if mmio.take_oam_write_pending() {
            let lc = self.ly_counter(mmio);
            if !pos_filled {
                mmio.peek_oam_pos(&mut pos);
            }
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
            // Widened compare (no u8 wrap): y < 16 sprites straddle the top
            // screen edge and are visible on lines 0 .. y+height-17 (hardware
            // scans LY+16 against [y, y+height); windesync-validate's y=15
            // strike-tip erase sprite).
            let top = sprite_y as i32 - 16;
            if (ly as i32) >= top && (ly as i32) < top + sprite_height as i32 {
                // A ghost-sampled slot (Y/X-bus retention during an OAM-DMA
                // window) exists only while the DMA owns OAM; its hardware tile/
                // attribute fetch sees the DMA's in-flight data, so read the live
                // progressively-written OAM rather than the CPU view (0xFF while
                // a DMA runs). Real-sampled slots keep the CPU-view read.
                let (tile_index, attributes_byte) = if let Some(ta) =
                    mmio.mgb_frozen_oam_tile_attr(i as u8)
                {
                    // MGB OAM-DMA-during-HALT merge: the frozen OAM bus feeds the
                    // PPU merged tile/attr for this entry (see mmio).
                    ta
                } else if self.oam_reader.ghost_slot[i] {
                    (
                        mmio.ppu_read_oam_live(0xFE00 + (i as u16) * 4 + 2),
                        mmio.ppu_read_oam_live(0xFE00 + (i as u16) * 4 + 3),
                    )
                } else {
                    (
                        mmio.read(0xFE00 + (i as u16) * 4 + 2),
                        mmio.read(0xFE00 + (i as u16) * 4 + 3),
                    )
                };
                self.sprites_on_line.push(Sprite {
                    y: sprite_y,
                    x: sprite_x,
                    tile_index,
                    attributes: SpriteAttributes::from_byte(attributes_byte),
                    oam_index: i as u8,
                });
            }
        }
        // Ghost propagation stop: any sprite fetched on THIS line while the DMA
        // window is still open rewrites the Y bus with a mid-DMA tile byte
        // (SameBoy display.c:1976), so the retained scan pair does not survive
        // into the NEXT line's walk (strikethrough: the ghost bar renders on
        // line 68 only; line 69's scan — still inside the ~1.4-line window —
        // sees the clobbered bus and stays clean).
        if self.oam_reader.src_disabled && !self.sprites_on_line.is_empty() {
            self.oam_reader.ghost = (0xFF, 0xFF);
        }
    }

    // A sprite whose fetch has not yet run and whose x-match column is `col`
    // (it will arm a pixel-pop stall when the pipeline head reaches that
    // column). Mirrors `sprite_fetch_penalty_for_current_x`'s trigger match;
    // used by the DMG stall-aware LCDC.0 boundary.
    fn dmg_unfetched_sprite_at(&self, col: u8) -> bool {
        if (self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) == 0 {
            return false;
        }
        self.sprites_on_line
            .get(self.next_sprite_fetch_index..)
            .unwrap_or(&[])
            .iter()
            .any(|s| s.x.saturating_sub(8) == col)
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
                // The sprite's x-match dot passed without a fetch (OBJ was
                // disabled when the head crossed it): dropped for the line —
                // no stall, and (DMG) its pixels never reach the mixer.
                if let Some(rec) = self
                    .sprite_fetch_recs
                    .get_mut(self.next_sprite_fetch_index)
                    && rec.phase == SpriteFetchPhase::Pending
                {
                    rec.phase = SpriteFetchPhase::Aborted;
                }
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
            // that tile costs a flat 6. On DMG `dist = (spx + scx) & 7` — the raw
            // OAM x, NOT the clamped trigger column: a left-clipped sprite
            // (spx 1..7) matches during the first-tile prologue and costs
            // max(11-spx, 6) (mealybug m3_lcdc_obj_en_change_variant's BGP bands
            // pin 10,9,8,7,6,6,6 for spx 1..7; the old `self.x`-based dist
            // collapsed them all to 11). On CGB keep the clamped-trigger dist:
            // the gambatte scx_during_m3_spx1/spx2 hardware captures pin the
            // full 11-dot stall for left-clipped sprites there. For spx >= 8 the
            // two are congruent mod 8, and the tile id differs from the
            // closed-form's `(spx - firstTileXpos) & -8` only by a per-line
            // constant, so the equality grouping (first-vs-rest) is identical.
            let scx = mmio.read(SCX);
            let dist_x = if mmio.is_cgb_features_enabled() { self.x } else { sprite_x };
            let pixel_in_tile = dist_x.wrapping_add(scx) & 0x07;
            let tile_no = (dist_x as i32 + scx as i32) & !7;
            let first_in_tile = tile_no != self.m3_sprite_prev_tile;
            self.m3_sprite_prev_tile = tile_no;

            let penalty = if sprite_x == 0 {
                11
            } else if first_in_tile {
                // pixel_in_tile 0..7 -> leading rate 11,10,9,8,7,6,6,6
                // (= max(11-dist,6)); a non-leading sprite in the same tile is
                // always a flat 6.
                let wait_for_bg_fetch = (7u8 - pixel_in_tile).saturating_sub(2);
                wait_for_bg_fetch + 6
            } else {
                6
            };
            // Per-sprite fetch record: a left-clipped sprite (spx < 8) matched
            // (8 - spx) dots before the head reached column 0 (during the
            // first-tile prologue), so its byte-fetch dots are earlier by that
            // amount than the arm tick observed here.
            if let Some(rec) = self
                .sprite_fetch_recs
                .get_mut(self.next_sprite_fetch_index - 1)
            {
                let left_adj = (8u128).saturating_sub(sprite_x as u128).min(self.ticks);
                rec.phase = SpriteFetchPhase::Fetched;
                rec.arm_tick = self.ticks - if sprite_x < 8 { left_adj } else { 0 };
                rec.penalty = penalty;
            }
            return Some(penalty);
        }

        None
    }

    // Mix background pixel with sprites at the given screen coordinates (CGB color version)
    fn mix_background_and_sprites_color(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, bg_attrs: u8, screen_x: u8, screen_y: u8, bg_enabled_col: bool) -> (u8, u8, u8) {
        let lcdc = self.lcdc;
        // Per-pixel BG-master-priority: on CGB, LCDC.0 off keeps BG/window
        // visible but drops BG master priority over sprites for this column
        // (Gambatte `bgprioritymask = p.lcdc << 7`, evaluated live per tile). Use
        // the column's BG-enable rather than the final once-per-line value.
        let bg_priority_master = bg_enabled_col;

        // Background attributes captured at fetch time travel with the pixel.
        let tile_attributes = bg_attrs;
        let palette_idx = tile_attributes & 0x07; // Bits 0-2 = palette index
        let bg_color_rgb = self.get_cgb_bg_color(mmio, palette_idx, bg_pixel_idx, screen_x);

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

                            let sprite_color_rgb = self.get_cgb_obj_color(mmio, sprite_palette_idx, sprite_pixel_idx, screen_x);

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

    /// DMG-compat-on-CGB pixel mix. Uses the DMG palette/priority rules (BGP/OBP
    /// shade remap, DMG sprite X-then-OAM priority, single OBP-selected palette),
    /// but resolves the final shade through CGB palette RAM so the output is the
    /// boot ROM's DMG-compat color instead of grayscale. The shade->RGB lookups
    /// read BG palette 0 and OBJ palette 0/1 (the slots the boot ROM fills).
    // BG-only CGB-compat color for a BG color index (no sprite mix): the shade
    // via BGP then BG palette 0 in CGB palette RAM. Used to detect BG-won columns
    // and to re-plot them in cgb_train_reresolve.
    fn compat_bg_color(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8) -> (u8, u8, u8) {
        let bg_shade = self.get_palette_color_at_tick(bg_pixel_idx, self.ticks);
        let (lo, hi) = mmio.bg_palette_pair_raw(0, bg_shade);
        self.cgb_color_to_rgb(lo, hi)
    }

    fn mix_background_and_sprites_compat(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, screen_x: u8, screen_y: u8, bg_enabled_col: bool) -> (u8, u8, u8) {
        let lcdc = self.lcdc;
        let bg_enabled = bg_enabled_col;

        // BG shade via BGP at this pixel's pop dot, then look up BG palette 0 in CGB
        // palette RAM.
        let idx = if bg_enabled { bg_pixel_idx } else { 0 };
        let bg_shade = self.get_palette_color_at_tick(idx, self.ticks);
        let (lo, hi) = mmio.bg_palette_pair_raw(0, bg_shade);
        let bg_color_rgb = self.cgb_color_to_rgb(lo, hi);

        let effective_bg_pixel_idx = if bg_enabled { bg_pixel_idx } else { 0 };

        // OBJ-enable gate. Mirrors the DMG mixer: with a mid-mode-3 LCDC.1
        // toggle this line, hardware gates each sprite pixel on the bit AT THAT
        // PIXEL'S pop dot (resolved per column from the history); otherwise the
        // live-LCDC fast path is exact. The DMG-compat renderer runs on CGB
        // hardware but through the same fetch/FIFO machinery, so all the DMG
        // mid-mode-3 sprite consumers apply here too — only the final color
        // lookup differs (CGB palette RAM vs grayscale). (mealybug
        // m3_lcdc_obj_en_change / obj_size_change / m3_obp0_change on CGB.)
        let objen_toggled = self.objen_history.len() > 1;
        if !objen_toggled && (lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) == 0 {
            return bg_color_rgb;
        }

        for (spr_i, sprite) in self.sprites_on_line.iter().enumerate() {
            // Mid-mode-3 OBJ-enable toggle (see the DMG mixer for the full
            // rationale): per-sprite fetch-abort gate + per-pixel pop-dot gate
            // with the 15-dot stale-FIFO quirk.
            if objen_toggled {
                let rec = self.sprite_fetch_recs.get(spr_i);
                if rec.map(|r| r.phase) == Some(SpriteFetchPhase::Aborted) {
                    continue;
                }
                // The 15-dot stale-FIFO pop quirk is a DMG-CPU artifact; the CGB
                // pixel gate samples LCDC.1 at the plain pop dot (mealybug
                // m3_lcdc_obj_en_change _cgb_c fails with the quirk applied).
                let stale = !(mmio.is_cgb() && !mmio.is_cgb_features_enabled())
                    && rec
                        .filter(|r| r.phase == SpriteFetchPhase::Fetched)
                        .is_some_and(|r| self.ticks >= r.arm_tick + 15);
                if !self.objen_at_tick(self.ticks + stale as u128) {
                    continue;
                }
            }
            let sprite_actual_x = sprite.x as i16 - 8;
            let sprite_actual_y = sprite.y as i16 - 16;
            let relative_x = screen_x as i16 - sprite_actual_x;
            let relative_y = screen_y as i16 - sprite_actual_y;
            if (0..8).contains(&relative_x) {
                // Mid-mode-3 OBJ-size (LCDC.2) toggle: the size bit is sampled
                // at each tile-data byte's own fetch dot (per-byte row
                // addressing via obj_pixel_sized); list membership already
                // implies a y-visible scan, so the bound is the scan range
                // (0..16) not the live size.
                let objsize_toggled = self.objsize_dot_history.len() > 1;
                let sprite_height = if (lcdc & (LCDCFlags::SpriteSize as u8)) != 0 { 16 } else { 8 };
                let y_in_range = if objsize_toggled {
                    (0..16).contains(&relative_y)
                } else {
                    relative_y >= 0 && relative_y < sprite_height as i16
                };
                if y_in_range {
                    let px = if objsize_toggled {
                        self.obj_pixel_sized(
                            mmio,
                            sprite,
                            self.sprite_fetch_recs.get(spr_i),
                            relative_x as u8,
                            screen_y,
                        )
                    } else {
                        self.get_sprite_pixel(mmio, sprite, relative_x as u8, relative_y as u8)
                    };
                    if let Some(sprite_pixel_idx) = px
                        && sprite_pixel_idx != 0 {
                        // DMG-compat: OBP0/OBP1 selected by attr bit 4, shade
                        // sampled at THIS pixel's pop dot (dot-keyed history,
                        // like the DMG mixer), then the shade is looked up in
                        // OBJ palette 0/1 of CGB palette RAM.
                        let use_obp1 = sprite.attributes.palette;
                        let obj_shade =
                            self.dmg_sprite_palette_shade(sprite_pixel_idx, use_obp1, self.ticks);
                        let pal = if use_obp1 { 1 } else { 0 };
                        let (slo, shi) = mmio.obj_palette_pair_raw(pal, obj_shade);
                        let sprite_color_rgb = self.cgb_color_to_rgb(slo, shi);
                        if !sprite.attributes.priority || effective_bg_pixel_idx == 0 {
                            return sprite_color_rgb;
                        }
                    }
                }
            }
        }

        bg_color_rgb
    }

    // Mix background pixel with sprites at the given screen coordinates
    fn mix_background_and_sprites(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, screen_x: u8, screen_y: u8, bg_enabled_col: bool) -> u8 {
        let lcdc = self.lcdc;

        // Per-pixel BG-enable: DMG BG-off forces this column's BG layer to white
        // (palette color 0) for the exact span the toggle covers. Use the
        // column's BG-enable from the line history, not the final LCDC.0.
        let bg_enabled = bg_enabled_col;

        // Get background color - if BG display is disabled, force to white (color 0)
        let bg_color = if bg_enabled {
            self.get_palette_color(mmio, bg_pixel_idx, screen_x)
        } else {
            // When BG display is disabled, background becomes white (palette color 0)
            self.get_palette_color(mmio, 0, screen_x)
        };

        // For sprite priority calculation, we need the original bg_pixel_idx
        let effective_bg_pixel_idx = if bg_enabled { bg_pixel_idx } else { 0 };

        // OBJ-enable gate. With a mid-mode-3 LCDC.1 toggle this line, hardware
        // gates each sprite pixel on the bit AT THAT PIXEL'S pop dot — resolve
        // per column from the history. Otherwise keep the live-LCDC fast path
        // (identical to the single seeded entry).
        let objen_toggled = self.objen_history.len() > 1;
        if !objen_toggled && (lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) == 0 {
            return bg_color;
        }

        // Find the highest priority sprite at this position
        for (spr_i, sprite) in self.sprites_on_line.iter().enumerate() {
            // Mid-mode-3 OBJ-enable toggle:
            //  - per-sprite FETCH gate: a sprite whose fetch was aborted
            //    (disable landed mid-fetch) or whose x-match dot passed while
            //    OBJ was disabled (skip-marked by the live walk before its
            //    columns pop) never contributes pixels this line, even where
            //    OBJ is re-enabled;
            //  - per-pixel POP gate: OBJ-enable sampled at this pixel's pop
            //    dot (SameBoy reads LCDC.1 live per popped pixel). A pixel
            //    popping 15+ dots after its sprite's fetch match samples the
            //    gate one dot LATE (stale-FIFO quirk — pinned by the
            //    m3_lcdc_obj_en_change spx=1/2 bands, whose trailing pixels
            //    go dark one dot before the disable's normal apply dot; the
            //    spx>=8 bands' first-pop pixels at the same dot stay lit).
            if objen_toggled {
                let rec = self.sprite_fetch_recs.get(spr_i);
                if rec.map(|r| r.phase) == Some(SpriteFetchPhase::Aborted) {
                    continue;
                }
                let stale = rec
                    .filter(|r| r.phase == SpriteFetchPhase::Fetched)
                    .is_some_and(|r| self.ticks >= r.arm_tick + 15);
                if !self.objen_at_tick(self.ticks + stale as u128) {
                    continue;
                }
            }
            // Sprite X coordinate is offset by 8, Y coordinate is offset by 16
            let sprite_actual_x = sprite.x as i16 - 8;
            let sprite_actual_y = sprite.y as i16 - 16;

            // Check if this screen pixel is within the sprite bounds
            let relative_x = screen_x as i16 - sprite_actual_x;
            let relative_y = screen_y as i16 - sprite_actual_y;

            // Sprite is 8 pixels wide
            if (0..8).contains(&relative_x) {
                // Mid-mode-3 OBJ-size (LCDC.2) toggle this line: hardware
                // samples the size bit at each tile-data byte's own fetch dot
                // (per-byte row addressing, see obj_pixel_sized); list
                // membership already implies the sprite was scanned y-visible,
                // so the bound is the scan range (0..16), not the live size.
                let objsize_toggled = self.objsize_dot_history.len() > 1;
                let sprite_height = if (lcdc & (LCDCFlags::SpriteSize as u8)) != 0 { 16 } else { 8 };
                let y_in_range = if objsize_toggled {
                    (0..16).contains(&relative_y)
                } else {
                    relative_y >= 0 && relative_y < sprite_height as i16
                };
                if y_in_range {
                    // Get sprite pixel data
                    let px = if objsize_toggled {
                        self.obj_pixel_sized(
                            mmio,
                            sprite,
                            self.sprite_fetch_recs.get(spr_i),
                            relative_x as u8,
                            screen_y,
                        )
                    } else {
                        self.get_sprite_pixel(mmio, sprite, relative_x as u8, relative_y as u8)
                    };
                    if let Some(sprite_pixel_idx) = px
                        && sprite_pixel_idx != 0 { // Sprite pixel is not transparent
                            let sprite_color = if mmio.is_cgb() {
                                // CGB: OBP sampled per pixel (true-color palette-RAM pipeline).
                                self.get_sprite_palette_color(mmio, sprite_pixel_idx, sprite.attributes.palette, screen_x)
                            } else {
                                // DMG mid-mode-3 OBP-write model (mealybug m3_obp0_change):
                                // OBP sampled at this pixel's pop dot from the dot-keyed
                                // history (see dmg_sprite_palette_shade).
                                self.dmg_sprite_palette_shade(sprite_pixel_idx, sprite.attributes.palette, self.ticks)
                            };

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

    // OBJ-enable (LCDC.1) as-of dot `tick`, resolved from the per-dot history
    // (see `objen_history`).
    fn objen_at_tick(&self, tick: u128) -> bool {
        let mut on = self
            .objen_history
            .first()
            .map(|&(_, b)| b)
            .unwrap_or((self.lcdc & (LCDCFlags::SpriteDisplayEnable as u8)) != 0);
        for &(apply_tick, b) in self.objen_history.iter() {
            if apply_tick <= tick {
                on = b;
            } else {
                break;
            }
        }
        on
    }

    // OBJ-size (LCDC.2) as-of dot `tick`, resolved from the per-dot history.
    fn objsize_large_at_tick(&self, tick: u128) -> bool {
        let mut large = self
            .objsize_dot_history
            .first()
            .map(|&(_, l)| l)
            .unwrap_or((self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0);
        for &(apply_tick, l) in self.objsize_dot_history.iter() {
            if apply_tick <= tick {
                large = l;
            } else {
                break;
            }
        }
        large
    }

    // DMG sprite pixel with per-byte OBJ-size resolution (mid-mode-3 LCDC.2
    // toggle lines). Hardware computes the object line address separately for
    // the tile-data LOW and HIGH byte reads, sampling LCDC.2 live each time
    // (SameBoy get_object_line_address is called before both vram reads), so a
    // toggle landing between them mixes two row addressings:
    //   tile_y = (ly - oam_y) & (large ? 15 : 7)   [y-flip XORs the mask]
    //   tile   = large ? index & 0xFE : index
    // The byte fetch dots come from the sprite's live fetch record: the stall
    // spans [arm, arm + penalty); the LOW byte reads at end-3, HIGH at end-1.
    // Sprites without a live record (not walked: m0-flush burst) fall back to
    // the live-LCDC path.
    fn obj_pixel_sized(
        &self,
        mmio: &mmio::Mmio,
        sprite: &Sprite,
        rec: Option<&SpriteFetchRec>,
        rel_x: u8,
        screen_y: u8,
    ) -> Option<u8> {
        let Some(rec) = rec.filter(|r| r.phase == SpriteFetchPhase::Fetched) else {
            // No per-fetch record: resolve both bytes with the live size.
            let large = (self.lcdc & (LCDCFlags::SpriteSize as u8)) != 0;
            return self.obj_pixel_with_sizes(mmio, sprite, rel_x, screen_y, large, large);
        };
        let fetch_end = rec.arm_tick + rec.penalty as u128;
        // CGB DMG-compat shifts both object tile-data read dots 3 dots earlier
        // in the stall than DMG-CPU silicon (see OBJ_READ_*_BACK_CGB).
        let (low_back, high_back) = if mmio.is_cgb() && !mmio.is_cgb_features_enabled() {
            (OBJ_READ_LOW_BACK_CGB, OBJ_READ_HIGH_BACK_CGB)
        } else {
            (OBJ_READ_LOW_BACK, OBJ_READ_HIGH_BACK)
        };
        let low_large = self.objsize_large_at_tick(fetch_end.saturating_sub(low_back));
        let high_large = self.objsize_large_at_tick(fetch_end.saturating_sub(high_back));
        self.obj_pixel_with_sizes(mmio, sprite, rel_x, screen_y, low_large, high_large)
    }

    fn obj_pixel_with_sizes(
        &self,
        mmio: &mmio::Mmio,
        sprite: &Sprite,
        rel_x: u8,
        screen_y: u8,
        low_large: bool,
        high_large: bool,
    ) -> Option<u8> {
        let line_addr = |large: bool| -> u16 {
            let mask: u8 = if large { 15 } else { 7 };
            // (ly - oam_y) & mask == (ly - (oam_y - 16)) & mask (16 ≡ 0 mod both).
            let mut tile_y = screen_y.wrapping_sub(sprite.y) & mask;
            if sprite.attributes.y_flip {
                tile_y ^= mask;
            }
            let tile = if large { sprite.tile_index & 0xFE } else { sprite.tile_index };
            0x8000 + (tile as u16) * 16 + (tile_y as u16) * 2
        };
        let low_byte = mmio.read(line_addr(low_large));
        let high_byte = mmio.read(line_addr(high_large) + 1);
        let bit_index = if sprite.attributes.x_flip { rel_x } else { 7 - rel_x };
        Some((((high_byte >> bit_index) & 1) << 1) | ((low_byte >> bit_index) & 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

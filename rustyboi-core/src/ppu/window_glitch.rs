use crate::memory::{mmio, Addressable};
use crate::ppu::fetcher;
use super::controller::{
    LCDCFlags, Ppu, SpriteFetchPhase, State, LY, WG_TRANSITION_DELAY,
};

// CGB-compat mid-mode-3 bus-glitch grid deltas. rise/fall = dots from the LCDC
// write to the bit becoming read-visible per fetch substep (fall split per
// tile-data byte); quirk = fall-coincidence tile-index-as-data window; arm/shift
// = fetch-grid anchoring for on-screen sprite stalls; scy_add = extra dots before
// SCY reaches the fetch address lines (vs DMG).
// Base: LCDC is modifiable mid-scanline (Pan Docs: LCDC) and SCY is re-read per
// tile-fetch / per-bitplane pre-CGB-D (Pan Docs: Scrolling "Mid-frame behavior").
// The sub-dot read-visibility grid, tile-index-as-data coincidence, and A12 re-arm
// are not in Pan Docs, TCAGBD, or GBCTR — sub-dot render timing from mealybug-tearoom-tests refs.
const CGBWG_WIN_RISE: u64 = 6;
const CGBWG_WIN_FALL: u64 = 7;
// Window map-select (LCDC.6) read visibility when the window tile-data path is
// $8000 (LCDC.4 = 1). Under $8000 the map pulse reaches the TileNumber read
// CGBWG_WIN_MAP_RISE/FALL_TDS dots after the write commit — later than the
// $8800 (LCDC.4 = 0) path's WIN_RISE/WIN_FALL — so a midline-sprite-shifted
// window fetch samples the map pulse one fetcher tile later; the $8800 path keeps
// WIN_RISE/WIN_FALL. See cgb_wg_resolve / wg_apply.
const CGBWG_WIN_MAP_RISE_TDS: u64 = 10;
const CGBWG_WIN_MAP_FALL_TDS: u64 = 10;
// BG-path LCDC.3/4 read visibility, measured from the raw write cc, at the
// hardware-exact fetch dot (bg_hw_read_dot_ex scy_mode): a bit becomes visible
// `rise`/`fall` dots after the write commit. The fetch dot already carries its
// own +2k substep offset, so the fall thresholds no longer need a per-substep
// split (the old 4/3/1 was an artifact of the 2-dots-per-sprite-late grid).
const CGBWG_BG_RISE: u64 = 4;
const CGBWG_BG_FALL: u64 = 4;
const CGBWG_BG_FALL_TDL: u64 = 3;
const CGBWG_BG_FALL_TDH: u64 = 1;
// Map-select (LCDC.3) read visibility at the hardware-exact fetch dot
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
// tile_sel-change write loop uses; a single isolated change pulse never re-fires
// low->high inside this span, so the extension is pulse-train-only (physical
// inter-edge spacing, not a per-tile coincidence).
const CGBWG_A12_GAP: u64 = 16;
const CGBWG_A12_REARM: u64 = 1;
// Pulse-train edge advance (see cgb_wg_resolve): a fall/rise inside a fast LCDC.4
// pulse train (opposite edge within CGBWG_A12_GAP dots) reaches the A12 bus this
// many dots sooner than the isolated-pulse thresholds — so its glitch window and
// bit4 visibility land on the read one dot past the write, not the isolated w+4.
const CGBWG_TRAIN_ADVANCE: u64 = 3;
// CGB-compat up-pulse LCDC.4 train line-end re-resolve (cgb_train_reresolve):
// each bitplane's tile-data base is sampled at its own T1, this many dots before
// the hardware-exact T2 fetch dot.
const CGBWG_TRAIN_T1_LEAD: i64 = 2;
const CGBWG_ARM_WIN: u64 = 14;
const CGBWG_ARM_WIN_HI: u64 = 12;
const CGBWG_ARM_BG: u64 = 14;
const CGBWG_SHIFT_BASE: u64 = 13;
// Sub-dot window fetch-grid phase (cgb_wg_resolve): the CGB-compat window
// fetch grid slides 1/8 dot earlier per window line against the CPU write
// clock (the hardware-measured read-dot drift quantizes this to the -1-dot
// steps every 8 lines that the integer grid already models; the fraction is
// the remainder). Two places see the fraction:
// - a read displaced by a mid-line sprite stall resumes on the slid grid, so
// a rising edge landing exactly ON its integer visibility dot misses the
// read by the fraction: shifted reads take a rise one eighth late (the
// m3_lcdc_tile_sel_win_change2 top-block wtx1 low read; its high-plane
// $8000 split then collapses to the $8800 base like every train split).
// - a read inside a PENDING stall shadow (hardware charges the sprite stall
// to this read; the reconstruction grid charges it from the next tile)
// samples the A12 line at its true (stalled) dot: a rising LCDC.4 edge
// still rings there CGBWG_A12_ECHO dots after its commit, and the read
// catches it only when the true dot lands exactly on the echo's 1/8-dot
// lattice point - phase 0, i.e. window lines = 0 mod 8.
const CGBWG_A12_ECHO: u64 = 18;

// CGB-compat window train tile-data-select latch (lower window rows). From
// WIN_TRAIN_GLITCH_ROW on, the pulse-train level and the tile-index-as-data glitch
// coincidence are sampled a per-block lag (in dots) before the reconstructed byte
// read; a FALL commit landing exactly on the sample dot IS the glitch. The lag
// walks one dot later every WIN_TRAIN_LAG_STEP window lines (the sub-dot fetch
// phase drift): rows 40-47 lag -1, 48-55 lag 0, 56-63 lag +1. The upper rows
// (< this) are uniform (no split/glitch) and use the collapse path instead.
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

impl Ppu {
    #[inline(always)]
    pub(in crate::ppu) fn begin_window_draw(&mut self, window_x: u8) {
        self.begin_window_draw_at_tile(window_x, 0);
    }

    pub(in crate::ppu) fn begin_window_draw_at_tile(&mut self, window_x: u8, start_tile: u8) {
        self.win_y_pos = self.win_y_pos.wrapping_add(1);
        self.win_draw_started = true;
        self.fetcher.start_window_at_tile(window_x, start_tile);
        self.we_glitch_tile_starts = [None; 2];
        self.win_kill_tap_late = true;
        self.window_started_this_line = true;
        self.win_being_fetched = true;
    }

    pub(in crate::ppu) fn wg_set_anchor(&mut self, chop: u64) {
        self.wg.wg_anchor_cc = None;
        self.wg.wg_dpre = 0;
        if self.x != 0 {
            return; // scoped to the x==0 restart family
        }
        // Pre-window sprites (OAM X <= 8) resolved from the LIVE per-sprite
        // fetch records (`sprite_fetch_recs`), not a closed-form stall model:
        // the renderer's anchored restart trigger fired exactly the sprite's
        // actually-charged penalty later (rb_absorb), and a sprite that never
        // fetched (OBJ off at its match dot) delayed neither the renderer nor
        // the hardware grid.
        // Not in Pan Docs, TCAGBD, or GBCTR; sub-dot render timing from mealybug-tearoom-tests refs.
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
                        return; // outside the single-pre-sprite case
                    }
                    pre = Some((s.x, rec.penalty as u64));
                }
                // Mid-fetch abort: a PARTIAL stall was charged; no evidence
                // for the partial absorb — leave the model off.
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
            // dots: D_pre = 13 - X (it separates the X=1 vs X=2 and X=3 vs X=4
            // bands the DMG quantization merges).
            Some((x, p)) if x <= 7 && self.wg.wg_cgb => (p, (13 - x) as u64),
            Some((x, p)) if x <= 7 => (p, (13i64 - ((x as i64 + 1) & !1)).max(7) as u64),
            // OAM X == 8 (window position 0): the hardware-side stall is a
            // midline shift resolved per-read in wg_apply (the in-progress
            // tile-1 fetch completes first).
            Some((_, p)) => (p, 0),
            None => (0, 0),
        };
        self.wg.wg_dpre = dpre;
        self.wg.wg_anchor_cc = Some(self.abs_cc.saturating_sub(rb_absorb + chop));
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

    /// Window-glitch journal front door: no anchor / empty journal (the
    /// overwhelmingly common case) is an inlined check.
    #[inline]
    pub(in crate::ppu) fn wg_apply(&self, fls: fetcher::FetcherLcdcState) -> fetcher::FetcherLcdcState {
        if self.wg.wg_anchor_cc.is_none() || self.wg.wg_hist.is_empty() {
            return fls;
        }
        self.wg_apply_slow(fls)
    }

    fn wg_apply_slow(&self, mut fls: fetcher::FetcherLcdcState) -> fetcher::FetcherLcdcState {
        let Some(anchor) = self.wg.wg_anchor_cc else {
            return fls;
        };
        if self.wg.wg_hist.is_empty() || !self.fetcher.is_fetching_window() {
            return fls;
        }
        let k = self.fetcher.fetch_substep();
        if k > 2 {
            return fls; // PushToFIFO: no VRAM read
        }
        let n = self.fetcher.get_tile_index() as u64;
        let base = anchor + self.wg.wg_dpre + 8 * n + 2 * k as u64;
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
            if self.wg.wg_cgb {
                // The fetch reads run ahead of the pixel pops that arm the
                // stalls: a Pending record still counts if OBJ is enabled
                // (mirrors the BG-path rule). An Aborted zero-penalty record
                // with OBJ on is a live-walk artifact (the match dot was
                // consumed by a tile-boundary pop the walk never saw — window
                // pos%8 == 0 sprites); hardware fetched it.
                let objon = self.lcdc_has(LCDCFlags::SpriteDisplayEnable);
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
                } else if counted && (n as i64) > pos / 8 {
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
        if self.wg.wg_cgb {
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
            // the sole discriminator between the LCDC.4=0 case (WIN_RISE/FALL correct
            // for its special-tile diagonal) and the LCDC.4=1 case, whose
            // midline-shifted window rows land the special $9C00 tile one fetcher
            // tile later. LCDC.4 is a stable per-ROM constant across each line here,
            // so keying on the resolved bit is safe; the tile-data-select and
            // tile-index-as-data quirk keep the WIN thresholds.
            let map_bit = LCDCFlags::WindowTileMapDisplaySelect as u8;
            let tds = LCDCFlags::BGWindowTileDataSelect as u8;
            let bits = if (bits & tds) != 0 {
                // The map re-resolve stays on the integer grid: the all-shifted
                // window rows show no sub-dot residue.
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
        let mut bits = self.wg.wg_hist[0].1; // before the first transition
        let mut edge: Option<u8> = None;
        for &(cc, old, new) in &self.wg.wg_hist {
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
        let mut bits = self.wg.wg_hist[0].1;
        let mut quirk = false;
        let mut prev_fall_w: Option<i64> = None;
        // Pulse-train scope (see CGBWG_TRAIN_ADVANCE). A line that holds LCDC.4
        // HIGH and pulses it LOW repeatedly keeps the A12 line perpetually driven,
        // so every falling edge's glitch/bit4 visibility lands CGBWG_TRAIN_ADVANCE
        // dots sooner. An isolated pulse instead blips UP from a bit4=0 baseline
        // (line-start LCDC.4 clear), a single settle at the w+4 thresholds. Key on
        // the line-initial LCDC.4 level (available at the first
        // fetch, so the early tiles resolve train-correctly before the whole pulse
        // train is journaled — unlike an edge-count which the growing journal only
        // reaches mid-line): a high baseline is the repeatedly-pulsed train, a low
        // baseline is the isolated blip.
        let is_train = (self.wg.wg_hist[0].1 & tds) != 0;
        for &(t, old, new) in &self.wg.wg_hist {
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
            // same read hardware catches (train w+1 vs isolated w+4); the RISE
            // advance restores tile-data-select in time for BOTH plane reads of the
            // tile straddling the re-rise, which renders as its $8000 tile (the
            // reconstruction otherwise holds the LOW plane $8800 one read too long,
            // splitting the tile into a spurious mixed $8000/$8800 read).
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
                8 * h as i64 + sub.phase8 > 8 * (w + rise_eff + rearm) as i64
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
            // 1-cycle tile-select-glitch window (on hardware the write sets the
            // glitch flag: set true, advance 1 cycle, set false) coincides
            // with a tile-data T2 read. Hardware uses the glitch data
            // in BOTH the low-plane T2 read (k==1) and the high-plane T2 read
            // (k==2), so which bitplane glitches is decided by which T2 read lands
            // in the 1-cycle window, not by k. The true hardware fetch dot `h_scy`
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
    // dot instead of our own (stall-displaced) read dot.
    // Base: LCDC.3/.4 are modifiable mid-scanline (Pan Docs: LCDC). The sub-dot BG
    // fetch-grid reconstruction and transition rule are not in Pan Docs, TCAGBD, or
    // GBCTR — sub-dot render timing from mealybug-tearoom-tests refs.
    // - Hardware BG fetch grid: read dot h = F + 8n + 2k (n = fetch index
    // from line start, k = 0/1/2 TileNumber/DataLow/DataHigh), F = the
    // line's first BG TileNumber dot (`bg_anchor_cc` — rustyboi reads it at
    // the same dot, before any sprite stall).
    // - An offscreen-left sprite (OAM X <= 7) is fetched during the first-tile
    // prologue and delays tiles n >= 1 by the same D_pre as the window grid:
    // max(7, 13 - 2*ceil(X/2)).
    // - An on-screen sprite (pos = X - 8 >= 0) lets the in-progress tile
    // complete, then delays tiles n >= pos/8 + 2 by 13,11,11,9,9,7,7,7
    // (pos%8 = 0..7) — the SAME 2-dot-quantized delay function as the
    // offscreen-left D_pre, keyed by the in-tile phase (NOT the live
    // pipeline's classic 11 - min(5, pos%8) charge).
    // - Transition rule: a read sees the post-write value iff its hardware
    // dot lies strictly past the write's commit cc; no OR edge on the BG
    // grid at this phase.
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

    // As `bg_hw_read_dot`, but `scy_mode` returns the hardware-exact CGB fetch
    // dot (2 dots earlier than the LCDC-calibrated dot for a sprite-stalled
    // tile). The LCDC journal (`bg_wg_resolve_cgb`) is tuned against the
    // un-corrected dot through its own rise/fall thresholds; the SCY journal
    // compares the dot against the raw write commit (+CGBWG_SCY_ADD), so it
    // needs the true fetch dot. After an offscreen-left sprite (OAM X<=7) the BG
    // fetch is delayed by D_pre = 11 - X (not 13 - X); an on-screen sprite delays
    // the tiles from its own by max(4, 11 - pos%8). Without this the k=1/k=2
    // substeps sit 2 dots too late and cross a mid-fetch SCY write the k=0
    // tile-number read did not — mixing the tile's map row with the wrong tile
    // line (per-row jitter).
    fn bg_hw_read_dot_ex(&self, n: u64, k: u8, ly: u8, scy_mode: bool) -> Option<u64> {
        let anchor = self.wg.bg_anchor_cc?;
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
                    self.lcdc_has(LCDCFlags::SpriteDisplayEnable)
                }
                // CGB: an Aborted zero-penalty record with OBJ on is a
                // live-walk artifact (see wg_apply); hardware fetched it.
                SpriteFetchPhase::Aborted => {
                    self.wg.wg_cgb
                        && rec.penalty == 0
                        && self.lcdc_has(LCDCFlags::SpriteDisplayEnable)
                }
            };
            if !counted {
                continue;
            }
            if s.x <= 7 {
                if n >= 1 {
                    // CGB: 1-dot D_pre = 13 - X (see the CGBWG_* consts); DMG: 2-dot
                    // fetcher-boundary quantized. (scy_mode: hardware-exact 11 - X.)
                    h += if self.wg.wg_cgb {
                        (13 - s.x) as u64 - cgb_stall_bias
                    } else {
                        (13i64 - ((s.x as i64 + 1) & !1)).max(7) as u64
                    };
                }
            } else {
                let pos = (s.x - 8) as u64;
                if self.wg.wg_cgb {
                    // CGB read-granular rule: only reads whose unshifted dot
                    // is at/after the sprite's arm dot A = F + arm + 8*(pos/8)
                    // (constant within the sprite's own tile) shift, by
                    // max(6, 13 - pos % 8). (scy_mode: hardware-exact max(4, 11 - pos%8).)
                    let arm = CGBWG_ARM_BG + 8 * (pos / 8);
                    if base >= anchor + arm {
                        h += (CGBWG_SHIFT_BASE as i64 - (pos % 8) as i64)
                            .max(6)
                            .saturating_sub(cgb_stall_bias as i64) as u64;
                    } else if !scy_mode && k >= 1 && n == pos / 8 + 1 && base + 4 >= anchor + arm {
                        // Sprite-triggering tile: hardware blocks the object fetch
                        // until the current tile passes its high-plane T2 read, so
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
                    // bgtiledata_spx08 tiles 2/17 (vs spx09-0B) pin
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
        let mut bits = self.wg.wg_hist[0].1;
        for &(cc, _, new) in &self.wg.wg_hist {
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
    // bit (LCDC.3) at the hardware-exact fetch dot `h_scy` (the true fetch dot,
    // which places a mid-line map pulse on the tile hardware fetches during the
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
        if self.wg.bg_scy_hist.is_empty() {
            return None;
        }
        // CGB-compat: the raw write commit reaches the fetch address lines
        // `scy_add` dots later than the recorded write cc (write M-cycle start).
        // Paired with the hardware-exact fetch dot (bg_hw_read_dot_ex scy_mode),
        // add=1 reproduces the hardware inclusive read>=write commit for both
        // sprite-stalled and un-stalled tiles.
        let add = if self.wg.wg_cgb { CGBWG_SCY_ADD } else { 0 };
        let mut v = self.wg.bg_scy_hist[0].1;
        for &(t, _, new) in &self.wg.bg_scy_hist {
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
    pub(in crate::ppu) fn cgb_train_reresolve(&mut self, mmio: &mmio::Mmio) {
        if !self.wg.wg_cgb || self.wg.bg_tile_buf.is_empty() || self.wg.wg_hist.is_empty() {
            return;
        }
        let tds = LCDCFlags::BGWindowTileDataSelect as u8;
        // Up-pulse train discriminator (complete journal): line-initial bit4 low
        // and at least two pulses (>= 4 edges). The isolated tile_sel_change
        // pulse is exactly 2 edges (one up, one down) and is left untouched.
        let init_low = (self.wg.wg_hist[0].1 & tds) == 0;
        let n_edges = self.wg.wg_hist.len();
        if !(init_low && n_edges >= 4) {
            return;
        }
        let ly = mmio.read(LY);
        if ly >= 144 {
            self.wg.bg_tile_buf.clear();
            return;
        }
        // Each plane's tile-data base is re-sampled at its OWN T1 (one substep
        // before the T2 byte read logged) — the raw journal bit4 level whose
        // write commit is <= (hardware-exact fetch dot - CGBWG_TRAIN_T1_LEAD).
        // Validated dot-exact vs CGB-C per-plane hardware across
        // change2 ly24-55 (every train tile L/H last_tileset reproduced).
        let buf = std::mem::take(&mut self.wg.bg_tile_buf);
        let raw_at = |dot: i64| -> u8 {
            let mut b = self.wg.wg_hist[0].1 & tds;
            for &(tt, _, nn) in &self.wg.wg_hist {
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
            let obj_on = self.lcdc_has(LCDCFlags::SpriteDisplayEnable);
            let tall = self.lcdc_has(LCDCFlags::SpriteSize);
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
        // that plane's VRAM data read — the CGB-compat TILE_SEL glitch pair:
        // - FALL: the tile INDEX is used as that bitplane's data, and the
        // stale-data latch captures the $8000-region byte the read was
        // pulling off the bus (A12 still high while falling).
        // - RISE: the bitplane gets the stale-data latch — the most recent of
        // the last sprite fetch's bitplane-1 byte and the last FALL-glitched
        // read's captured byte.
        // A pulse train sweeps the coincidence through both planes: successive
        // sprite bands step the fetch-grid phase one dot per band, so an early
        // band lands the edges on the LOW-plane reads and a later band on the
        // HIGH-plane reads; the other bands have no coincidence and resolve clean.
        // Not in Pan Docs, TCAGBD, or GBCTR; sub-dot render timing from mealybug-tearoom-tests refs.
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
                for &(tt, o, nn) in &self.wg.wg_hist {
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
                let old = self.plot.line_bg_idx[ci];
                if old < 0 || old as u8 == idx { continue; }
                let rgb = self.compat_bg_color(mmio, idx);
                let off = (ly as usize * 160 + ci) * 3;
                self.out.color_fb_a[off] = rgb.0;
                self.out.color_fb_a[off + 1] = rgb.1;
                self.out.color_fb_a[off + 2] = rgb.2;
                self.plot.line_bg_idx[ci] = idx as i8;
            }
        }
    }

    // CGB-compat up-pulse LCDC.4 train capture-phase re-resolve for the WINDOW
    // fetcher (the window analog of cgb_train_reresolve). The live per-substep
    // resolve draws each window tile from its LOW/HIGH reads on a line-locked grid
    // against the PARTIAL journal (the pulse train is only fully journaled at
    // line-end), which mis-latches the tile-data-select base and misses the
    // tile-index-as-data glitch. This runs at line-end against the COMPLETE journal.
    // The two bands are handled differently (see the per-tile comment): the upper
    // rows collapse each live-split tile to its single latched base; the lower rows
    // (from WIN_TRAIN_GLITCH_ROW) reconstruct each read dot and re-resolve the base +
    // glitch at the band sample lag, rendering the tile INDEX as a glitched plane's
    // byte. Tight gate (line-initial LCDC.4 low AND >= 4 journal edges) so an
    // isolated single pulse stays untouched. A residual glitch band remains where
    // the exact A12-settle phase is not observable from the refs. CGB-compat only.
    // Not in Pan Docs, TCAGBD, or GBCTR; sub-dot render timing from mealybug-tearoom-tests refs.
    pub(in crate::ppu) fn win_train_reresolve(&mut self, mmio: &mmio::Mmio) {
        if !self.wg.wg_cgb || self.wg.win_tile_buf.is_empty() || self.wg.wg_hist.is_empty() {
            return;
        }
        let tds = LCDCFlags::BGWindowTileDataSelect as u8;
        let init_low = (self.wg.wg_hist[0].1 & tds) == 0;
        if !(init_low && self.wg.wg_hist.len() >= 4) {
            self.wg.win_tile_buf.clear();
            return;
        }
        let ly = mmio.read(LY);
        if ly >= 144 {
            self.wg.win_tile_buf.clear();
            return;
        }
        let (Some(anchor), dpre) = (self.wg.wg_anchor_cc, self.wg.wg_dpre) else {
            self.wg.win_tile_buf.clear();
            return;
        };
        // Resolve the LCDC.4 tile-data-select level at a reconstructed read dot,
        // and whether the read coincides with a falling edge (the tile-index-as-
        // data glitch) or a RISING edge (the stale-bus echo, below). All key on
        // the latch dot = read dot - sample lag; a FALL commit landing exactly on
        // the latch dot returns the tile index as data; a RISE commit landing
        // exactly on it leaves the VRAM output mid-settle, and the returned byte is
        // the value the data bus carried 16 dots (two fetch slots) earlier — the
        // same-plane byte of the tile fetched two slots back at ITS level-at-sample
        // base, or the displacing sprite fetch's high byte when that slot ran the
        // sprite. The two leading window tiles (n<2) always latch the line-initial
        // low base — their HIGH_T1 latch predates the pulse train — so they never
        // glitch and keep the $8800 base.
        let resolve = |this: &Self, h: u64, first_tile: bool, sample_lag: i64| -> (bool, bool, bool) {
            if first_tile {
                return (false, false, false);
            }
            let s = h as i64 - sample_lag;
            let mut level = (this.wg.wg_hist[0].1 & tds) != 0;
            let mut glitch = false;
            let mut rise_hit = false;
            for &(cc, old, new) in &this.wg.wg_hist {
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
        let buf = std::mem::take(&mut self.wg.win_tile_buf);
        // Per-tile resolved (tn, low base, high base) records for the
        // stale-bus lookup, keyed by fetch index n (buf is in fetch order).
        let mut resolved_recs: Vec<Option<(u8, bool, bool)>> = Vec::new();
        for t in &buf {
            // The upper window rows (win line < WIN_TRAIN_GLITCH_ROW) are UNIFORM on
            // hardware: every tile latches a single $8000/$8800 base, and it
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
            // (the hardware tile-select glitch).
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
            // with the tile INDEX (the hardware tile-select glitch); a
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
                let old = self.plot.line_bg_idx[ci];
                if old < 0 || old as u8 == idx { continue; }
                let rgb = self.compat_bg_color(mmio, idx);
                let off = (ly as usize * 160 + ci) * 3;
                self.out.color_fb_a[off] = rgb.0;
                self.out.color_fb_a[off + 1] = rgb.1;
                self.out.color_fb_a[off + 2] = rgb.2;
                self.plot.line_bg_idx[ci] = idx as i8;
            }
        }
    }

    /// Journal-application front door: the journals only fill on DMG
    /// mid-mode-3 SCY/SCX/window-glitch writes, so the common per-dot case is
    /// the inlined empty check.
    #[inline(always)]
    pub(in crate::ppu) fn bg_wg_apply(&self, fls: fetcher::FetcherLcdcState, ly: u8) -> fetcher::FetcherLcdcState {
        if self.wg.wg_hist.is_empty() && self.wg.bg_scy_hist.is_empty() && self.wg.bg_scx_hist.is_empty() {
            return fls;
        }
        self.bg_wg_apply_slow(fls, ly)
    }

    fn bg_wg_apply_slow(&self, mut fls: fetcher::FetcherLcdcState, ly: u8) -> fetcher::FetcherLcdcState {
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
        if !self.wg.wg_hist.is_empty() {
            if self.wg.wg_cgb {
                let h_scy = self.bg_hw_read_dot_ex(n, k, ly, self.wg.wg_cgb).unwrap_or(h);
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
        // SCY resolves at the hardware-exact fetch dot (see bg_hw_read_dot_ex);
        // on DMG the scy_mode dot is identical to `h` (bias 0).
        let h_scy = self.bg_hw_read_dot_ex(n, k, ly, self.wg.wg_cgb).unwrap_or(h);
        fls.scy_bus = self.bg_scy_resolve(h_scy);
        // SCX resolves the tile-map column at the TileNumber (k==0) reconstructed
        // hardware dot: a sprite-stalled tile reads SCX as-of that dot, not the
        // stall-displaced live scx (m3_scx_high_5_bits). Only k==0 fetches the
        // column, so only resolve there.
        if k == 0 && !self.wg.bg_scx_hist.is_empty() {
            let h_scx = self.bg_hw_read_dot_ex(n, k, ly, self.wg.wg_cgb).unwrap_or(h);
            fls.scx_bus = self.bg_scx_resolve(h_scx);
        }
        fls
    }

    // SCX in effect at reconstructed hardware dot `h` per the DMG BG journal.
    fn bg_scx_resolve(&self, h: u64) -> Option<u8> {
        if self.wg.bg_scx_hist.is_empty() {
            return None;
        }
        let add = if self.wg.wg_cgb { CGBWG_SCY_ADD } else { 0 };
        let mut v = self.wg.bg_scx_hist[0].1;
        for &(t, _, new) in &self.wg.bg_scx_hist {
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
    pub(in crate::ppu) fn bg_retro_repair(&mut self, mmio: &mmio::Mmio) {
        if self.state != State::PixelTransfer
            || (self.wg.wg_hist.is_empty() && self.wg.bg_scy_hist.is_empty())
        {
            return;
        }
        let k_now = self.fetcher.fetch_substep();
        if !(1..=3).contains(&k_now) {
            return;
        }
        let n = self.fetcher.get_tile_index() as u64;
        let ly = mmio.read(LY);
        let live_scy = self.latch.scy_delayed;
        let map_bit = LCDCFlags::BGTileMapDisplaySelect as u8;
        let col = self.fetcher.last_bg_tn_col() as u16;

        // TileNumber (k=0).
        let Some(h0) = self.bg_hw_read_dot(n, 0, ly) else {
            return;
        };
        // CGB resolves the map bit at the hardware-exact fetch dot and the
        // tile-data bit at the calibrated `h` (see bg_wg_resolve_cgb); DMG uses `h`.
        let h0_scy = self.bg_hw_read_dot_ex(n, 0, ly, self.wg.wg_cgb).unwrap_or(h0);
        let bits0 = if self.wg.wg_hist.is_empty() {
            self.lcdc.reg
        } else if self.wg.wg_cgb {
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
        // read coherently. Pin the HIGH plane's tile-data-select bit to the LOW
        // plane's resolution so retro reproduces the live bg_wg_apply result
        // instead of diverging from it. (The genuine mixed per-bitplane
        // $8000/$8800 case is produced on the live path via bg_hw_read_dot_ex's
        // arm-dot anchoring, which retro's shared reconstruction inherits.)
        let tds = LCDCFlags::BGWindowTileDataSelect as u8;
        let tds_low = self.bg_hw_read_dot(n, 1, ly).map(|h1| {
            let h1_scy = self.bg_hw_read_dot_ex(n, 1, ly, self.wg.wg_cgb).unwrap_or(h1);
            if self.wg.wg_hist.is_empty() {
                self.lcdc.reg & tds
            } else if self.wg.wg_cgb {
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
            let hk_scy = self.bg_hw_read_dot_ex(n, k, ly, self.wg.wg_cgb).unwrap_or(hk);
            let (mut bitsk, quirkk) = if self.wg.wg_hist.is_empty() {
                (self.lcdc.reg, false)
            } else if self.wg.wg_cgb {
                self.bg_wg_resolve_cgb(hk, hk_scy, k)
            } else {
                (self.bg_wg_resolve(hk), false)
            };
            // Pin the HIGH plane's tile-data-select bit to the LOW plane's ONLY
            // when a sprite object-fetch split this tile (its HIGH read is
            // arm-shifted off the LOW read's +2 cadence). With no sprite the two
            // reads are simply 2 dots apart and the HIGH plane resolves its OWN
            // tile-data-select — the genuine mixed $8000/$8800 read of a mid-tile
            // LCDC.4 pulse (transition tiles: low $8000 / high $8800). Pinning
            // unconditionally here would flatten that mix to a solid tile.
            let low_hk = self.bg_hw_read_dot(n, 1, ly);
            let unstalled = low_hk.is_some_and(|h1| hk == h1 + 2);
            // The LOW plane's $8000 read latches the tile-data address for BOTH
            // bytes at HIGH_T1; a falling LCDC.4 that reaches the bus only after
            // HIGH_T1 cannot un-latch the already-$8000 HIGH plane. So when the
            // LOW plane rose to $8000, the HIGH plane inherits $8000 too — pin it.
            // This is the up-pulse train's HIGH-plane latch: the fetch outruns the
            // FALL write, and the retro pass would otherwise wrongly re-apply it to
            // the HIGH plane. The DOWN-pulse train (is_train) holds LCDC.4 HIGH and
            // pulses it LOW: there the mid-tile mix (low $8000 / high $8800) is
            // genuine — the FALL precedes HIGH_T1 — so its unstalled HIGH keeps
            // resolving on its own. Gate the unstalled pin on the up-pulse
            // (line-initial LCDC.4 low) so it never flattens the down-pulse mix.
            let up_pulse = self
                .wg
                .wg_hist
                .first()
                .is_some_and(|&(_, first, _)| (first & tds) == 0);
            if self.wg.wg_cgb
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
}

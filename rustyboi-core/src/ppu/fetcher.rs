use crate::ppu;
use crate::ppu::fifo;
use crate::memory::mmio;
use crate::memory::Addressable;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
enum State {
    TileNumber,
    TileDataLow,
    TileDataHigh,
    PushToFIFO,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FetcherDebugEventKind {
    TileNumber,
    TileDataLow,
    TileDataHigh,
    PushToFifo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FetcherDebugEvent {
    pub kind: FetcherDebugEventKind,
    pub tile_index: u8,
    pub tile_num: u8,
    pub tile_attributes: u8,
    pub tile_line: u8,
    pub addr: Option<u16>,
    pub value: Option<u8>,
    pub lcdc: u8,
    pub tile_index_is_tile_data: bool,
    pub fifo_size: usize,
    pub fetching_window: bool,
}

/// Per-step position/scroll context for the fetcher.
#[derive(Clone, Copy, Default)]
pub(crate) struct FetchPos {
    pub(crate) window_line: u8,
    pub(crate) display_x: u8,
    pub(crate) pending_discard: u8,
    pub scy: u8,
    pub scx: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FetcherLcdcState {
    pub lcdc: u8,
    pub(crate) cgb_tile_index_is_tile_data: bool,
    // DMG window bus-glitch OR-read: when set, this substep's VRAM read
    // coincides with an LCDC.6/LCDC.4 bus transition (the CPU write's address
    // lines change mid-read), and the read returns the bitwise OR of the bytes
    // at the pre- and post-transition addresses. `lcdc` carries the
    // post-transition bits; `or_lcdc` the pre-transition ones. Derived from the
    // mealybug m3_lcdc_win_map_change / m3_lcdc_tile_sel_win_change DMG
    // reference captures (both pulse edges show the union of both sources).
    pub(crate) or_lcdc: Option<u8>,
    // DMG BG-path SCY bus state (see bg_wg_apply): the SCY value in effect at
    // this substep's reconstructed hardware dot. None = use the live `scy`
    // argument.
    pub(crate) scy_bus: Option<u8>,
    // DMG BG-path SCX bus state (see bg_wg_apply): the SCX value in effect at
    // the tile's reconstructed hardware TileNumber dot. Used for the tile-map
    // column so a sprite-stalled tile reads SCX as-of its true hardware fetch
    // dot instead of the stall-displaced live dot (mealybug m3_scx_high_5_bits).
    // None = use the live `scx` argument.
    pub(crate) scx_bus: Option<u8>,
}

// Tile data addressing constants
const TILE_DATA_8000_BASE: u16 = 0x8000; // $8000 method base (unsigned addressing)
const TILE_DATA_8800_BASE: u16 = 0x9000; // $8800 method base (signed addressing)

// Tile map addressing constants
const TILE_MAP_9800_BASE: u16 = 0x9800; // Tile map area 0
const TILE_MAP_9C00_BASE: u16 = 0x9C00; // Tile map area 1

#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Fetcher {
    state: State,
    pub(crate) pixel_fifo: fifo::Fifo,

    tile_num: u8,
    tile_index: u8,
    tile_attributes: u8, // CGB tile attributes from VRAM bank 1
    pixel_buffer: [u8; 8],

    // Window support
    fetching_window: bool,
    window_x_start: u8,
    // WE-off / tile-fetch boundary: when a mid-mode-3 window-disable
    // lands mid-window-tile (xpos != endx), the in-progress tile
    // (whose tilemap was committed window at its fetch start) finishes drawing before
    // the revert. Count of additional window-tile fetches to draw before
    // reverting to BG. 0 = stop at the very next TileNumber (the boundary case).
    #[serde(default)]
    stop_window_after_tiles: u8,
    // Set at the last deferred-extra window tile's TileNumber; its PushToFIFO
    // flips fetching_window off so the following tile fetches BG.
    #[serde(default)]
    window_revert_at_push: bool,

    // Latched (LY + SCY) for the current tile fetch. Captured when the
    // fetcher enters `TileNumber` and reused by `TileDataLow`/`High` for
    // the tile-line offset, so a mid-tile SCY write does not shift the
    // in-progress fetch. Window fetches ignore this and use the internal
    // `window_line` counter instead.
    #[serde(default)]
    latched_y: u8,
    #[serde(default)]
    subcc_xpos: u16,
    #[serde(default)]
    subcc_cgb_adj: u8,
    #[serde(default)]
    subcc_used_scx: u8,

    // OAM-DMA-source bus-conflict model: the VRAM data-bus address the BG fetcher
    // most recently drove (the tile-data / tile-number read), plus its VRAM bank.
    // During mode 3 a VRAM-source OAM-DMA read does NOT see VRAM[src]; the BG
    // fetcher and the DMA both drive the VRAM address bus, so the array is indexed
    // by the bitwise AND of the two addresses (real-silicon address-line conflict).
    #[serde(default)]
    last_vram_addr: u16,
    #[serde(default)]
    last_vram_bank: u8,

    // The BG tile-map COLUMN the last BG TileNumber used. The DMG BG
    // bus-glitch retro repair re-derives the map cell for the in-flight tile
    // (the row is SCY-dependent and re-resolved; the column is not).
    #[serde(default)]
    last_bg_tn_col: u8,
    // The tile-data-select (LCDC.4) bit actually used for the last tile's LOW /
    // HIGH bitplane reads (the live, partial-journal resolution). The CGB-compat
    // train re-resolve compares against these so it only re-plots tiles whose
    // base moved — leaving the glitch bands the live draw already got right.
    #[serde(default)]
    last_low_tds: bool,
    #[serde(default)]
    last_high_tds: bool,
}

impl Fetcher {
    pub(super) fn new() -> Self {
        Fetcher {
            state: State::TileNumber,
            pixel_fifo: fifo::Fifo::new(),
            tile_num: 0,
            tile_index: 0,
            tile_attributes: 0,
            pixel_buffer: [0; 8],
            fetching_window: false,
            window_x_start: 0,
            stop_window_after_tiles: 0,
            window_revert_at_push: false,
            latched_y: 0,
            subcc_xpos: 0,
            subcc_cgb_adj: 0,
            subcc_used_scx: 0,
            last_vram_addr: 0xFFFF,
            last_vram_bank: 0,
            last_low_tds: false,
            last_high_tds: false,
            last_bg_tn_col: 0,
        }
    }

    /// The VRAM address/bank the BG fetcher most recently drove onto the VRAM bus
    /// (tile-number or tile-data read). Used by the OAM-DMA-source bus-conflict
    /// model. Stable between fetch substeps, so it reflects the byte latched on the
    /// VRAM data bus at the current dot.
    pub(super) fn last_vram_bus(&self) -> (u16, u8) {
        (self.last_vram_addr, self.last_vram_bank)
    }

    pub(super) fn reset(&mut self) {
        self.state = State::TileNumber;
        self.pixel_fifo.reset();
        self.tile_num = 0;
        self.tile_index = 0;
        self.tile_attributes = 0;
        self.pixel_buffer = [0; 8];
        self.fetching_window = false;
        self.window_x_start = 0;
        self.stop_window_after_tiles = 0;
        self.window_revert_at_push = false;
        // Hold the OAM-DMA conflict bus at AND-identity for the new line until the
        // fetcher drives a real address (the first locked read is suppressed in
        // mmio; this guards against a stale cross-line address conflicting).
        self.last_vram_addr = 0xFFFF;
    }

    // Start fetching window tiles when WX condition is met
    pub(super) fn start_window(&mut self, window_x: u8) {
        self.start_window_at_tile(window_x, 0);
    }

    // Start fetching window tiles from a specific window tilemap column. The
    // mid-line WX-match path starts at column 0; the M3Start::f0 line-begin
    // "already started" path (DMG wx==166 wraparound) starts at column
    // wscx/8 == (tile_len + scx%8)/8 == 1, since the hardware seeds
    // wscx = tile_len + scx%8 there.
    pub(super) fn start_window_at_tile(&mut self, window_x: u8, start_tile: u8) {
        self.fetching_window = true;
        self.window_x_start = window_x;
        self.tile_index = start_tile;
        self.pixel_fifo.reset(); // Clear FIFO when switching to window
        self.state = State::TileNumber; // Start fetching immediately
        // A fresh window start cancels any pending deferred WE-off revert.
        self.stop_window_after_tiles = 0;
        self.window_revert_at_push = false;
    }

    // Stop the window, but draw `extra` additional full window tiles first.
    // The hardware commits each tile's window-vs-BG choice at the tile
    // boundary (`xpos == endx`); a WE-off that lands mid-tile (`xpos != endx`)
    // lets the already-committed in-progress tile finish before reverting. The
    // controller passes extra=1 in that mid-tile case, 0 at a tile boundary.
    pub(super) fn stop_window_with_extra(&mut self, extra: u8) {
        if extra == 0 {
            self.fetching_window = false;
            self.stop_window_after_tiles = 0;
        } else {
            // Keep fetching window for `extra` more tile fetches, then revert.
            self.stop_window_after_tiles = extra;
        }
    }

    // Calculate the correct tile map base address based on LCDC.6 (WindowTileMapDisplaySelect)
    fn get_window_tile_map_base(&self, lcdc: u8) -> u16 {
        let window_tile_map_select = ppu::lcdc_has(lcdc, ppu::LCDCFlags::WindowTileMapDisplaySelect);

        if window_tile_map_select {
            TILE_MAP_9C00_BASE // LCDC.6 = 1: Use $9C00-$9FFF
        } else {
            TILE_MAP_9800_BASE // LCDC.6 = 0: Use $9800-$9BFF
        }
    }

    // Calculate the correct tile map base address based on LCDC.3 (BGTileMapDisplaySelect)
    fn get_tile_map_base(&self, lcdc: u8) -> u16 {
        let bg_tile_map_select = ppu::lcdc_has(lcdc, ppu::LCDCFlags::BGTileMapDisplaySelect);

        if bg_tile_map_select {
            TILE_MAP_9C00_BASE // LCDC.3 = 1: Use $9C00-$9FFF
        } else {
            TILE_MAP_9800_BASE // LCDC.3 = 0: Use $9800-$9BFF
        }
    }

    // Calculate the correct tile data address based on LCDC.4 (BGWindowTileDataSelect)
    pub(crate) fn get_tile_data_address(&self, tile_id: u8, tile_line: u8, lcdc: u8) -> u16 {
        let bg_window_tile_data_select = ppu::lcdc_has(lcdc, ppu::LCDCFlags::BGWindowTileDataSelect);

        if bg_window_tile_data_select {
            // $8000 method: unsigned addressing
            // Tiles 0-127 are in block 0 ($8000-$87FF)
            // Tiles 128-255 are in block 1 ($8800-$8FFF)
            let offset = (tile_id as u16) * 16 + (tile_line as u16) * 2;
            TILE_DATA_8000_BASE + offset
        } else {
            // $8800 method: signed addressing
            // Tiles 0-127 are in block 2 ($9000-$97FF)
            // Tiles 128-255 (interpreted as -128 to -1) are in block 1 ($8800-$8FFF)
            let tile_id_signed = tile_id as i8;
            let offset = (tile_id_signed as i16) * 16 + (tile_line as i16) * 2;
            ((TILE_DATA_8800_BASE as i16) + offset) as u16
        }
    }

    // One bitplane of a BG/window tile-data fetch, shared by the TileDataLow and
    // TileDataHigh substeps. Returns the address read, the byte, and this tile's
    // x-flip so the caller can run its own bitplane merge (the two merges differ:
    // low assigns, high ORs in shifted by one).
    //
    // The two planes differ in exactly two side effects, both preserved here:
    //   - the tile-data-select latch goes to `last_low_tds` vs `last_high_tds`;
    //   - only the HIGH plane publishes `last_vram_addr`/`last_vram_bank`. The
    //     tile-data-LOW read is a transient on the VRAM address bus; the OAM-DMA
    //     conflict bus keeps holding the tile-NUMBER address driven on the
    //     previous substep through this read, and only the tile-data-high
    //     address takes over. Publishing on the low plane would break those
    //     hardware bus hold windows.
    fn fetch_tile_data_byte(
        &mut self,
        mmio: &mmio::Mmio,
        lcdc_state: FetcherLcdcState,
        tile_line: u8,
        high: bool,
    ) -> (u16, u8, bool) {
        let cgb = mmio.is_cgb_features_enabled();
        let y_flip = cgb && (self.tile_attributes & 0x40) != 0;
        let x_flip = cgb && (self.tile_attributes & 0x20) != 0;
        let eff_line = if y_flip { 7 - tile_line } else { tile_line };
        let plane = u16::from(high);
        let addr = self.get_tile_data_address(self.tile_num, eff_line, lcdc_state.lcdc) + plane;
        let tds = ppu::lcdc_has(lcdc_state.lcdc, ppu::LCDCFlags::BGWindowTileDataSelect);
        if high {
            self.last_high_tds = tds;
        } else {
            self.last_low_tds = tds;
        }

        // In CGB mode, use VRAM bank specified in tile attributes (bit 3)
        let tile_data_bank = if cgb && (self.tile_attributes & 0x08) != 0 {
            1
        } else {
            0
        };
        if high {
            self.last_vram_addr = addr;
            self.last_vram_bank = tile_data_bank;
        }
        let mut byte = if lcdc_state.cgb_tile_index_is_tile_data && self.tile_num < 0x80 {
            self.tile_num
        } else {
            mmio.read_vram_bank(tile_data_bank, addr)
        };
        // DMG bus-glitch OR-read: mid-read tile-data-select transition
        // (LCDC.4, address bit A12) returns the union of both banks'
        // bytes for this bitplane.
        if let Some(l2) = lcdc_state.or_lcdc {
            let alt = self.get_tile_data_address(self.tile_num, eff_line, l2) + plane;
            if alt != addr {
                byte |= mmio.read_vram_bank(tile_data_bank, alt);
            }
        }
        (addr, byte, x_flip)
    }

    pub(super) fn step(
        &mut self,
        mmio: &mut mmio::Mmio,
        lcdc_state: FetcherLcdcState,
        pos: FetchPos,
    ) -> Option<FetcherDebugEvent> {
        let FetchPos { window_line, display_x, pending_discard, scy, scx } = pos;
        let ly = mmio.read(ppu::LY);
        // Re-read (scy + ly) live at every BG fetch substep. The fetcher runs
        // ahead of the display by the FIFO depth, so sampling SCY as late as
        // possible (through TileDataHigh) lets a mid-M3 SCY write land on the
        // last fetched tile, matching the hardware's late tileline sample. Window
        // fetches use the internal `window_line` counter, independent of SCY.
        // A DMG mid-mode-3 SCY write resolves at the substep's reconstructed
        // hardware dot instead (`scy_bus`, see bg_wg_apply).
        let scy_eff = lcdc_state.scy_bus.unwrap_or(scy);
        let y = if self.fetching_window {
            window_line
        } else if matches!(self.state, State::TileNumber | State::TileDataLow | State::TileDataHigh) {
            let new_y = ly.wrapping_add(scy_eff);
            self.latched_y = new_y;
            new_y
        } else {
            self.latched_y
        };
        let tile_line = y % 8;

        match self.state {
            State::TileNumber => {
                // Deferred WE-off (tile-fetch mid-tile boundary): when a
                // window-disable landed mid-window-tile, the controller armed
                // `stop_window_after_tiles` extra window tiles. Each TileNumber
                // that begins while armed consumes one; the LAST one keeps
                // fetching_window true for its own tile, then reverts so the
                // following tile fetches BG. (The in-flight tile at the disable
                // dot is already past TileNumber, so it is never counted here.)
                if self.fetching_window && self.stop_window_after_tiles > 0 {
                    self.stop_window_after_tiles -= 1;
                    if self.stop_window_after_tiles == 0 {
                        // This tile is the last window tile; revert after it.
                        // Set a marker by leaving fetching_window true for this
                        // fetch but scheduling the flip at its PushToFIFO.
                        self.window_revert_at_push = true;
                    }
                }
                // Fetch the tile number from VRAM using the correct tile map
                let (tile_map_base, map_offset) = if self.fetching_window {
                    let window_tile_map_base = self.get_window_tile_map_base(lcdc_state.lcdc);
                    // Window always starts from tile (0, 0) in the window tile map
                    // The tile_index for window represents how many tiles we've moved horizontally from window start
                    let window_tile_x = self.tile_index;
                    let window_tile_y = window_line / 8;
                    let map_offset = (window_tile_y as u16) * 32 + (window_tile_x as u16);
                    (window_tile_map_base, map_offset)
                } else {
                    let bg_tile_map_base = self.get_tile_map_base(lcdc_state.lcdc);
                    // For background, account for SCX scrolling. The hardware derives
                    // tileMapXpos = (scx + xpos) / 8 from the DISPLAY position of the
                    // tile's first pixel, re-reading SCX live at each tile fetch. The
                    // tile being fetched will be displayed at column
                    // display_x + (pixels currently queued in FIFO), so derive the
                    // column from that rather than the free-running tile_index. This
                    // makes a mid-M3 SCX write land at the correct display column
                    // despite FIFO latency.
                    // The DMG +1 phase adjustment (tileMapXpos =
                    // (scx + xpos + 1 - cgb) / 8) applies only past the M3Start
                    // discard prologue; the first tile (display_x == 0) is fetched
                    // at scx/8 with no adjustment.
                    let cgb_adj: u16 = if mmio.is_cgb() || display_x == 0 { 0 } else { 1 };
                    let xpos = (display_x as u16 + self.pixel_fifo.size() as u16)
                        .saturating_sub(pending_discard as u16);
                    // DMG BG grid: a sprite-stalled tile reads SCX at its
                    // reconstructed hardware dot (scx_bus), not the stall-displaced
                    // live scx (see bg_wg_apply / m3_scx_high_5_bits).
                    let scx_eff = lcdc_state.scx_bus.unwrap_or(scx);
                    let bg_tile_x = (scx_eff as u16 + xpos + cgb_adj) / 8 % 32;
                    // sub-cc lever: remember the exact (xpos, scx, cgb_adj) used to
                    // derive this tile's column so the controller can recompute
                    // the column under a different (NEW) scx with identical inputs.
                    self.subcc_xpos = xpos;
                    self.subcc_cgb_adj = cgb_adj as u8;
                    self.subcc_used_scx = scx;
                    let bg_tile_y = (y as u16 / 8) % 32;
                    let map_offset = bg_tile_y * 32 + bg_tile_x;
                    self.last_bg_tn_col = bg_tile_x as u8;
                    (bg_tile_map_base, map_offset)
                };

                let map_addr = tile_map_base + map_offset;
                self.last_vram_addr = map_addr;
                self.last_vram_bank = 0;
                self.tile_num = mmio.read_vram_bank(0, map_addr);
                // DMG bus-glitch OR-read: mid-read map-select transition drives
                // both map addresses within the read dot; the latched tile
                // number is the union of both cells' 1-bits. The corrupted
                // number feeds this tile's data reads (hardware pipeline latch).
                if let Some(l2) = lcdc_state.or_lcdc {
                    let alt_base = if self.fetching_window {
                        self.get_window_tile_map_base(l2)
                    } else {
                        self.get_tile_map_base(l2)
                    };
                    if alt_base != tile_map_base {
                        self.tile_num |= mmio.read_vram_bank(0, alt_base + map_offset);
                    }
                }

                // In CGB mode, read tile attributes from VRAM bank 1
                self.tile_attributes = if mmio.is_cgb_features_enabled() {
                    mmio.read_vram_bank(1, map_addr)
                } else {
                    0 // No attributes in DMG mode
                };

                self.state = State::TileDataLow;
                Some(FetcherDebugEvent {
                    kind: FetcherDebugEventKind::TileNumber,
                    tile_index: self.tile_index,
                    tile_num: self.tile_num,
                    tile_attributes: self.tile_attributes,
                    tile_line,
                    addr: Some(map_addr),
                    value: Some(self.tile_num),
                    lcdc: lcdc_state.lcdc,
                    tile_index_is_tile_data: lcdc_state.cgb_tile_index_is_tile_data,
                    fifo_size: self.pixel_fifo.size(),
                    fetching_window: self.fetching_window,
                })
            }
            State::TileDataLow => {
                // Fetch the low byte of the tile data using the correct addressing method
                let (addr, low_byte, x_flip) =
                    self.fetch_tile_data_byte(mmio, lcdc_state, tile_line, false);

                for i in 0..8 {
                    let bit = if x_flip { i } else { 7 - i };
                    self.pixel_buffer[i] = (low_byte >> bit) & 0x01;
                }
                self.state = State::TileDataHigh;
                Some(FetcherDebugEvent {
                    kind: FetcherDebugEventKind::TileDataLow,
                    tile_index: self.tile_index,
                    tile_num: self.tile_num,
                    tile_attributes: self.tile_attributes,
                    tile_line,
                    addr: Some(addr),
                    value: Some(low_byte),
                    lcdc: lcdc_state.lcdc,
                    tile_index_is_tile_data: lcdc_state.cgb_tile_index_is_tile_data,
                    fifo_size: self.pixel_fifo.size(),
                    fetching_window: self.fetching_window,
                })
            }
            State::TileDataHigh => {
                // Fetch the high byte of the tile data using the correct addressing method
                let (addr, high_byte, x_flip) =
                    self.fetch_tile_data_byte(mmio, lcdc_state, tile_line, true);

                for i in 0..8 {
                    let bit = if x_flip { i } else { 7 - i };
                    self.pixel_buffer[i] |= ((high_byte >> bit) & 0x01) << 1;
                }
                self.state = State::PushToFIFO;
                Some(FetcherDebugEvent {
                    kind: FetcherDebugEventKind::TileDataHigh,
                    tile_index: self.tile_index,
                    tile_num: self.tile_num,
                    tile_attributes: self.tile_attributes,
                    tile_line,
                    addr: Some(addr),
                    value: Some(high_byte),
                    lcdc: lcdc_state.lcdc,
                    tile_index_is_tile_data: lcdc_state.cgb_tile_index_is_tile_data,
                    fifo_size: self.pixel_fifo.size(),
                    fetching_window: self.fetching_window,
                })
            }
            State::PushToFIFO => {
                if self.pixel_fifo.size() > 8 {
                    return None;
                }

                for i in 0..8 {
                    self.pixel_fifo.push(fifo::BgPixel {
                        color: self.pixel_buffer[i],
                        attrs: self.tile_attributes,
                    });
                }
                self.tile_index = self.tile_index.wrapping_add(1);
                self.state = State::TileNumber;
                // Deferred WE-off revert: the last extra window tile has now
                // been pushed; revert to BG for subsequent fetches.
                if self.window_revert_at_push {
                    self.window_revert_at_push = false;
                    self.fetching_window = false;
                }
                Some(FetcherDebugEvent {
                    kind: FetcherDebugEventKind::PushToFifo,
                    tile_index: self.tile_index.wrapping_sub(1),
                    tile_num: self.tile_num,
                    tile_attributes: self.tile_attributes,
                    tile_line,
                    addr: None,
                    value: None,
                    lcdc: lcdc_state.lcdc,
                    tile_index_is_tile_data: lcdc_state.cgb_tile_index_is_tile_data,
                    fifo_size: self.pixel_fifo.size(),
                    fetching_window: self.fetching_window,
                })
            }
        }
    }

    pub(super) fn get_pixel_buffer(&self) -> [u8; 8] {
        self.pixel_buffer
    }

    pub(super) fn get_fifo_size(&self) -> usize {
        self.pixel_fifo.size()
    }

    pub(super) fn get_tile_index(&self) -> u8 {
        self.tile_index
    }

    // Retroactive in-flight-tile patches (DMG BG bus-glitch model): the
    // controller re-resolves the tile's completed reads at their reconstructed
    // hardware dots when a mid-fetch LCDC.3/4 or SCY write lands after our
    // (earlier) read executed. Only valid before the tile's PushToFIFO.
    pub(super) fn patch_tile_num(&mut self, tile_num: u8) {
        self.tile_num = tile_num;
    }

    pub(super) fn last_bg_tn_col(&self) -> u8 {
        self.last_bg_tn_col
    }

    // The (scy+ly) pixel row latched at the tile's TileNumber read — the row the
    // fetcher used to address this tile's data bytes (CGB-compat train re-resolve).
    pub(super) fn latched_y(&self) -> u8 {
        self.latched_y
    }

    // The live tile-data-select bits used for the last tile's LOW / HIGH reads.
    pub(super) fn last_low_tds(&self) -> bool {
        self.last_low_tds
    }
    pub(super) fn last_high_tds(&self) -> bool {
        self.last_high_tds
    }

    // Replace the low bitplane of the in-flight pixel buffer (DMG: no x-flip).
    pub(super) fn patch_pixel_buffer_low(&mut self, low_byte: u8) {
        for i in 0..8 {
            self.pixel_buffer[i] = (self.pixel_buffer[i] & !1) | ((low_byte >> (7 - i)) & 0x01);
        }
    }

    // Replace the high bitplane of the in-flight pixel buffer (DMG: no x-flip).
    pub(super) fn patch_pixel_buffer_high(&mut self, high_byte: u8) {
        for i in 0..8 {
            self.pixel_buffer[i] =
                (self.pixel_buffer[i] & !2) | (((high_byte >> (7 - i)) & 0x01) << 1);
        }
    }

    pub(super) fn is_fetching_window(&self) -> bool {
        self.fetching_window
    }

    pub(super) fn window_x_start_dbg(&self) -> u8 {
        self.window_x_start
    }

    // True when the next step() will run the TileNumber substep (the one that
    // derives the BG tile-map column). The sub-cc column lever only reroutes SCX
    // on that substep.
    pub(super) fn fetch_state_is_tile_number(&self) -> bool {
        matches!(self.state, State::TileNumber)
    }

    // The substep the next step() will run: 0 = TileNumber, 1 = TileDataLow,
    // 2 = TileDataHigh, 3 = PushToFIFO. Drives the DMG window bus-glitch
    // hardware-dot reconstruction (each substep is one VRAM read, 2 dots apart).
    pub(super) fn fetch_substep(&self) -> u8 {
        match self.state {
            State::TileNumber => 0,
            State::TileDataLow => 1,
            State::TileDataHigh => 2,
            State::PushToFIFO => 3,
        }
    }

    // The (xpos, cgb_adj, scx) the last BG TileNumber used to derive its column.
    // The controller recomputes the column under a NEW scx with the
    // same xpos/cgb_adj to re-key the just-pushed tile.
    pub(super) fn subcc_last_column_inputs(&self) -> (u16, u8, u8) {
        (self.subcc_xpos, self.subcc_cgb_adj, self.subcc_used_scx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Addressable;

    const TILE_ID: u8 = 0x02;
    const TILE_DATA_ADDR: u16 = TILE_DATA_8000_BASE + (TILE_ID as u16) * 16;

    fn cgb_mmio() -> mmio::Mmio {
        let mut mmio = mmio::Mmio::new();
        mmio.set_cgb_features_enabled(true);
        mmio.write(ppu::LCD_CONTROL, ppu::LCDCFlags::BGWindowTileDataSelect as u8);
        mmio.write(ppu::SCX, 0);
        mmio.write(ppu::SCY, 0);
        mmio.write_ly_from_ppu(0);
        mmio
    }

    fn write_bank0_tile_number_and_data(mmio: &mut mmio::Mmio, low_byte: u8, high_byte: u8) {
        mmio.write(mmio::REG_VBK, 0);
        mmio.write(TILE_MAP_9800_BASE, TILE_ID);
        mmio.write(TILE_DATA_ADDR, low_byte);
        mmio.write(TILE_DATA_ADDR + 1, high_byte);
    }

    fn lcdc_state(mmio: &mmio::Mmio, cgb_tile_index_is_tile_data: bool) -> FetcherLcdcState {
        FetcherLcdcState {
            lcdc: mmio.read(ppu::LCD_CONTROL),
            cgb_tile_index_is_tile_data,
            or_lcdc: None,
            scy_bus: None,
            scx_bus: None,
        }
    }

    #[test]
    fn cgb_fetch_uses_bank0_tile_numbers_when_cpu_vbk_is_bank1() {
        let mut mmio = cgb_mmio();
        write_bank0_tile_number_and_data(&mut mmio, 0b1010_1010, 0b0101_0101);

        mmio.write(mmio::REG_VBK, 1);
        mmio.write(TILE_MAP_9800_BASE, 0x07);
        mmio.write(TILE_DATA_ADDR, 0x11);
        mmio.write(TILE_DATA_ADDR + 1, 0x22);

        let mut fetcher = Fetcher::new();
        let state = lcdc_state(&mmio, false);
        let tile_number = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert_eq!(tile_number.kind, FetcherDebugEventKind::TileNumber);
        assert_eq!(tile_number.tile_num, TILE_ID);
        assert_eq!(tile_number.tile_attributes, 0x07);

        let tile_data_low = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert_eq!(tile_data_low.kind, FetcherDebugEventKind::TileDataLow);
        assert_eq!(tile_data_low.value, Some(0b1010_1010));

        let tile_data_high = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert_eq!(tile_data_high.kind, FetcherDebugEventKind::TileDataHigh);
        assert_eq!(tile_data_high.value, Some(0b0101_0101));
    }

    #[test]
    fn cgb_fetch_uses_attribute_bank_for_tile_data() {
        let mut mmio = cgb_mmio();
        write_bank0_tile_number_and_data(&mut mmio, 0x11, 0x22);

        mmio.write(mmio::REG_VBK, 1);
        mmio.write(TILE_MAP_9800_BASE, 0x08);
        mmio.write(TILE_DATA_ADDR, 0x33);
        mmio.write(TILE_DATA_ADDR + 1, 0x44);

        let mut fetcher = Fetcher::new();
        let state = lcdc_state(&mmio, false);
        let tile_number = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert_eq!(tile_number.kind, FetcherDebugEventKind::TileNumber);
        assert_eq!(tile_number.tile_num, TILE_ID);
        assert_eq!(tile_number.tile_attributes, 0x08);

        let tile_data_low = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert_eq!(tile_data_low.kind, FetcherDebugEventKind::TileDataLow);
        assert_eq!(tile_data_low.value, Some(0x33));

        let tile_data_high = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert_eq!(tile_data_high.kind, FetcherDebugEventKind::TileDataHigh);
        assert_eq!(tile_data_high.value, Some(0x44));
    }

    #[test]
    fn cgb_lcdc_tile_data_falling_edge_can_return_tile_index_as_data() {
        let mut mmio = cgb_mmio();
        write_bank0_tile_number_and_data(&mut mmio, 0xFF, 0xFF);

        mmio.write(
            ppu::LCD_CONTROL,
            ppu::LCDCFlags::DisplayEnable as u8 | ppu::LCDCFlags::BGWindowTileDataSelect as u8,
        );
        mmio.write(ppu::LCD_CONTROL, ppu::LCDCFlags::DisplayEnable as u8);

        let mut fetcher = Fetcher::new();
        let state = lcdc_state(&mmio, true);
        let tile_number = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert!(tile_number.tile_index_is_tile_data);

        let tile_data_low = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert_eq!(tile_data_low.kind, FetcherDebugEventKind::TileDataLow);
        assert_eq!(tile_data_low.value, Some(TILE_ID));

        let tile_data_high = fetcher.step(&mut mmio, state, FetchPos::default()).unwrap();
        assert_eq!(tile_data_high.kind, FetcherDebugEventKind::TileDataHigh);
        assert_eq!(tile_data_high.value, Some(TILE_ID));

        let state = lcdc_state(&mmio, false);
        assert!(!state.cgb_tile_index_is_tile_data);
    }
}

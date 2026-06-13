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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FetcherLcdcState {
    pub lcdc: u8,
    pub cgb_tile_index_is_tile_data: bool,
}

// Tile data addressing constants
const TILE_DATA_8000_BASE: u16 = 0x8000; // $8000 method base (unsigned addressing)
const TILE_DATA_8800_BASE: u16 = 0x9000; // $8800 method base (signed addressing)

// Tile map addressing constants
const TILE_MAP_9800_BASE: u16 = 0x9800; // Tile map area 0
const TILE_MAP_9C00_BASE: u16 = 0x9C00; // Tile map area 1

#[derive(Serialize, Deserialize, Clone)]
pub struct Fetcher {
    state: State,
    pub pixel_fifo: fifo::Fifo,

    tile_num: u8,
    tile_index: u8,
    tile_attributes: u8, // CGB tile attributes from VRAM bank 1
    pixel_buffer: [u8; 8],
    
    // Window support
    fetching_window: bool,
    window_x_start: u8,

    // Latched (LY + SCY) for the current tile fetch. Captured when the
    // fetcher enters `TileNumber` and reused by `TileDataLow`/`High` for
    // the tile-line offset, so a mid-tile SCY write does not shift the
    // in-progress fetch. Window fetches ignore this and use the internal
    // `window_line` counter instead.
    #[serde(default)]
    latched_y: u8,
}

impl Fetcher {
    pub fn new() -> Self {
        Fetcher {
            state: State::TileNumber,
            pixel_fifo: fifo::Fifo::new(),
            tile_num: 0,
            tile_index: 0,
            tile_attributes: 0,
            pixel_buffer: [0; 8],
            fetching_window: false,
            window_x_start: 0,
            latched_y: 0,
        }
    }

    pub fn reset(&mut self) {
        self.state = State::TileNumber;
        self.pixel_fifo.reset();
        self.tile_num = 0;
        self.tile_index = 0;
        self.tile_attributes = 0;
        self.pixel_buffer = [0; 8];
        self.fetching_window = false;
        self.window_x_start = 0;
    }
    
    // Start fetching window tiles when WX condition is met
    pub fn start_window(&mut self, window_x: u8) {
        self.fetching_window = true;
        self.window_x_start = window_x;
        self.tile_index = 0; // Reset tile index for window
        self.pixel_fifo.reset(); // Clear FIFO when switching to window
        self.state = State::TileNumber; // Start fetching immediately
    }

    // Calculate the correct tile map base address based on LCDC.6 (WindowTileMapDisplaySelect)
    fn get_window_tile_map_base(&self, lcdc: u8) -> u16 {
        let window_tile_map_select = (lcdc & (ppu::LCDCFlags::WindowTileMapDisplaySelect as u8)) != 0;
        
        if window_tile_map_select {
            TILE_MAP_9C00_BASE // LCDC.6 = 1: Use $9C00-$9FFF
        } else {
            TILE_MAP_9800_BASE // LCDC.6 = 0: Use $9800-$9BFF
        }
    }

    // Calculate the correct tile map base address based on LCDC.3 (BGTileMapDisplaySelect)
    fn get_tile_map_base(&self, lcdc: u8) -> u16 {
        let bg_tile_map_select = (lcdc & (ppu::LCDCFlags::BGTileMapDisplaySelect as u8)) != 0;
        
        if bg_tile_map_select {
            TILE_MAP_9C00_BASE // LCDC.3 = 1: Use $9C00-$9FFF
        } else {
            TILE_MAP_9800_BASE // LCDC.3 = 0: Use $9800-$9BFF
        }
    }

    // Calculate the correct tile data address based on LCDC.4 (BGWindowTileDataSelect)
    fn get_tile_data_address(&self, tile_id: u8, tile_line: u8, lcdc: u8) -> u16 {
        let bg_window_tile_data_select = (lcdc & (ppu::LCDCFlags::BGWindowTileDataSelect as u8)) != 0;
        
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

    pub fn step(
        &mut self,
        mmio: &mut mmio::Mmio,
        window_line: u8,
        lcdc_state: FetcherLcdcState,
    ) -> Option<FetcherDebugEvent> {
        let ly = mmio.read(ppu::LY);
        // For data-fetch states (`TileDataLow`/`High`), reuse the y latched
        // when this fetch began at `TileNumber`. For `TileNumber` itself,
        // re-read SCY so the latch reflects the current scroll. Window
        // fetches always use the internal `window_line` counter, which is
        // already independent of SCY.
        // Gambatte samples (scy + ly) at the tile-data fetch (xpos%8==2),
        // i.e. our `TileDataLow` substep, not at `TileNumber`. Latch there so
        // a mid-M3 SCY write lands in the correct fetch window, then reuse the
        // latch for `TileDataHigh`. Window fetches use `window_line` instead.
        let y = if self.fetching_window {
            window_line
        } else if matches!(self.state, State::TileNumber | State::TileDataLow) {
            let new_y = ly.wrapping_add(mmio.read(ppu::SCY));
            self.latched_y = new_y;
            new_y
        } else {
            self.latched_y
        };
        let tile_line = y % 8;

        match self.state {
            State::TileNumber => {
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
                    // For background, account for SCX scrolling
                    let scx = mmio.read(ppu::SCX);
                    let bg_tile_x = (self.tile_index as u16).wrapping_add(scx as u16 / 8) % 32;
                    let bg_tile_y = (y as u16 / 8) % 32;
                    let map_offset = bg_tile_y * 32 + bg_tile_x;
                    (bg_tile_map_base, map_offset)
                };
                
                let map_addr = tile_map_base + map_offset;
                self.tile_num = mmio.read_vram_bank(0, map_addr);
                
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
                let cgb = mmio.is_cgb_features_enabled();
                let y_flip = cgb && (self.tile_attributes & 0x40) != 0;
                let x_flip = cgb && (self.tile_attributes & 0x20) != 0;
                let eff_line = if y_flip { 7 - tile_line } else { tile_line };
                // Fetch the low byte of the tile data using the correct addressing method
                let addr = self.get_tile_data_address(self.tile_num, eff_line, lcdc_state.lcdc);

                // In CGB mode, use VRAM bank specified in tile attributes (bit 3)
                let tile_data_bank = if cgb && (self.tile_attributes & 0x08) != 0 {
                    1
                } else {
                    0
                };
                let low_byte = if lcdc_state.cgb_tile_index_is_tile_data && self.tile_num < 0x80 {
                    self.tile_num
                } else {
                    mmio.read_vram_bank(tile_data_bank, addr)
                };

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
                let cgb = mmio.is_cgb_features_enabled();
                let y_flip = cgb && (self.tile_attributes & 0x40) != 0;
                let x_flip = cgb && (self.tile_attributes & 0x20) != 0;
                let eff_line = if y_flip { 7 - tile_line } else { tile_line };
                // Fetch the high byte of the tile data using the correct addressing method
                let addr = self.get_tile_data_address(self.tile_num, eff_line, lcdc_state.lcdc) + 1;

                // In CGB mode, use VRAM bank specified in tile attributes (bit 3)
                let tile_data_bank = if cgb && (self.tile_attributes & 0x08) != 0 {
                    1
                } else {
                    0
                };
                let high_byte = if lcdc_state.cgb_tile_index_is_tile_data && self.tile_num < 0x80 {
                    self.tile_num
                } else {
                    mmio.read_vram_bank(tile_data_bank, addr)
                };

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

    pub fn get_pixel_buffer(&self) -> [u8; 8] {
        self.pixel_buffer
    }

    pub fn get_fifo_size(&self) -> usize {
        self.pixel_fifo.size()
    }

    pub fn get_tile_index(&self) -> u8 {
        self.tile_index
    }
    
    pub fn is_fetching_window(&self) -> bool {
        self.fetching_window
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
        let tile_number = fetcher.step(&mut mmio, 0, state).unwrap();
        assert_eq!(tile_number.kind, FetcherDebugEventKind::TileNumber);
        assert_eq!(tile_number.tile_num, TILE_ID);
        assert_eq!(tile_number.tile_attributes, 0x07);

        let tile_data_low = fetcher.step(&mut mmio, 0, state).unwrap();
        assert_eq!(tile_data_low.kind, FetcherDebugEventKind::TileDataLow);
        assert_eq!(tile_data_low.value, Some(0b1010_1010));

        let tile_data_high = fetcher.step(&mut mmio, 0, state).unwrap();
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
        let tile_number = fetcher.step(&mut mmio, 0, state).unwrap();
        assert_eq!(tile_number.kind, FetcherDebugEventKind::TileNumber);
        assert_eq!(tile_number.tile_num, TILE_ID);
        assert_eq!(tile_number.tile_attributes, 0x08);

        let tile_data_low = fetcher.step(&mut mmio, 0, state).unwrap();
        assert_eq!(tile_data_low.kind, FetcherDebugEventKind::TileDataLow);
        assert_eq!(tile_data_low.value, Some(0x33));

        let tile_data_high = fetcher.step(&mut mmio, 0, state).unwrap();
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
        let tile_number = fetcher.step(&mut mmio, 0, state).unwrap();
        assert!(tile_number.tile_index_is_tile_data);

        let tile_data_low = fetcher.step(&mut mmio, 0, state).unwrap();
        assert_eq!(tile_data_low.kind, FetcherDebugEventKind::TileDataLow);
        assert_eq!(tile_data_low.value, Some(TILE_ID));

        let tile_data_high = fetcher.step(&mut mmio, 0, state).unwrap();
        assert_eq!(tile_data_high.kind, FetcherDebugEventKind::TileDataHigh);
        assert_eq!(tile_data_high.value, Some(TILE_ID));

        let state = lcdc_state(&mmio, false);
        assert!(!state.cgb_tile_index_is_tile_data);
    }
}

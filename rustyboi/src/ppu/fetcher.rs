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
    pixel_buffer: [u8; 8],
    
    // Window support
    fetching_window: bool,
    window_x_start: u8,
}

impl Fetcher {
    pub fn new() -> Self {
        Fetcher {
            state: State::TileNumber,
            pixel_fifo: fifo::Fifo::new(),
            tile_num: 0,
            tile_index: 0,
            pixel_buffer: [0; 8],
            fetching_window: false,
            window_x_start: 0,
        }
    }

    pub fn reset(&mut self) {
        self.state = State::TileNumber;
        self.pixel_fifo.reset();
        self.tile_num = 0;
        self.tile_index = 0;
        self.pixel_buffer = [0; 8];
        self.fetching_window = false;
        self.window_x_start = 0;
    }
    
    // Reset and apply SCX offset for background scrolling
    pub fn reset_with_scx_offset(&mut self, mmio: &mut mmio::Mmio) {
        self.reset();
        
        // Apply SCX pixel offset by pre-fetching and discarding pixels
        let scx = mmio.read(ppu::SCX);
        let pixel_offset = scx % 8;
        
        if pixel_offset > 0 {
            // We need to pre-fetch enough tiles to have pixels to discard
            // Keep stepping the fetcher until we have enough pixels in the FIFO
            while self.pixel_fifo.size() < pixel_offset as usize {
                self.step(mmio, 0); // Fetch tiles until we have enough pixels
            }
            
            // Discard the first (pixel_offset) pixels from the FIFO
            for _ in 0..pixel_offset {
                let _ = self.pixel_fifo.pop(); // Discard pixels for sub-tile alignment
            }
        }
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
    fn get_window_tile_map_base(&self, mmio: &mmio::Mmio) -> u16 {
        let lcdc = mmio.read(ppu::LCD_CONTROL);
        let window_tile_map_select = (lcdc & (ppu::LCDCFlags::WindowTileMapDisplaySelect as u8)) != 0;
        
        if window_tile_map_select {
            TILE_MAP_9C00_BASE // LCDC.6 = 1: Use $9C00-$9FFF
        } else {
            TILE_MAP_9800_BASE // LCDC.6 = 0: Use $9800-$9BFF
        }
    }

    // Calculate the correct tile map base address based on LCDC.3 (BGTileMapDisplaySelect)
    fn get_tile_map_base(&self, mmio: &mmio::Mmio) -> u16 {
        let lcdc = mmio.read(ppu::LCD_CONTROL);
        let bg_tile_map_select = (lcdc & (ppu::LCDCFlags::BGTileMapDisplaySelect as u8)) != 0;
        
        if bg_tile_map_select {
            TILE_MAP_9C00_BASE // LCDC.3 = 1: Use $9C00-$9FFF
        } else {
            TILE_MAP_9800_BASE // LCDC.3 = 0: Use $9800-$9BFF
        }
    }

    // Calculate the correct tile data address based on LCDC.4 (BGWindowTileDataSelect)
    fn get_tile_data_address(&self, tile_id: u8, tile_line: u8, mmio: &mmio::Mmio) -> u16 {
        let lcdc = mmio.read(ppu::LCD_CONTROL);
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

    pub fn step(&mut self, mmio: &mut mmio::Mmio, window_line: u8) {
        let ly = mmio.read(ppu::LY);
        let y = if self.fetching_window {
            // For window, use the internal window line counter
            window_line
        } else {
            // For background, use LY + SCY
            ly.wrapping_add(mmio.read(ppu::SCY))
        };
        let tile_line = y % 8;

        match self.state {
            State::TileNumber => {
                // Fetch the tile number from VRAM using the correct tile map
                let (tile_map_base, map_offset) = if self.fetching_window {
                    let window_tile_map_base = self.get_window_tile_map_base(mmio);
                    // Window always starts from tile (0, 0) in the window tile map
                    // The tile_index for window represents how many tiles we've moved horizontally from window start
                    let window_tile_x = self.tile_index;
                    let window_tile_y = window_line / 8;
                    let map_offset = (window_tile_y as u16) * 32 + (window_tile_x as u16);
                    (window_tile_map_base, map_offset)
                } else {
                    let bg_tile_map_base = self.get_tile_map_base(mmio);
                    // For background, account for SCX scrolling
                    let scx = mmio.read(ppu::SCX);
                    let bg_tile_x = (self.tile_index as u16).wrapping_add(scx as u16 / 8) % 32;
                    let bg_tile_y = (y as u16 / 8) % 32;
                    let map_offset = bg_tile_y * 32 + bg_tile_x;
                    (bg_tile_map_base, map_offset)
                };
                
                let map_addr = tile_map_base + map_offset;
                self.tile_num = mmio.read(map_addr);
                self.state = State::TileDataLow;
            }
            State::TileDataLow => {
                // Fetch the low byte of the tile data using the correct addressing method
                let addr = self.get_tile_data_address(self.tile_num, tile_line, mmio);
                let low_byte = mmio.read(addr);
                for i in 0..8 {
                    self.pixel_buffer[i] = (low_byte >> (7 - i)) & 0x01;
                }
                self.state = State::TileDataHigh;
            }
            State::TileDataHigh => {
                // Fetch the high byte of the tile data using the correct addressing method
                let addr = self.get_tile_data_address(self.tile_num, tile_line, mmio) + 1;
                let high_byte = mmio.read(addr);
                for i in 0..8 {
                    // Combine low and high bytes to form the pixel data
                    self.pixel_buffer[i] |= ((high_byte >> (7 - i)) & 0x01) << 1;
                }
                self.state = State::PushToFIFO;
            }
            State::PushToFIFO => {
                // Push the fetched tile data to the FIFO
                if self.pixel_fifo.size() <= 8 {
                    for i in 0..8 {
                        self.pixel_fifo.push(self.pixel_buffer[i]);
                    }
                }
                self.tile_index = self.tile_index.wrapping_add(1);
                self.state = State::TileNumber;
            }
        }
    }

    pub fn get_pixel_buffer(&self) -> [u8; 8] {
        self.pixel_buffer
    }
    
    pub fn is_fetching_window(&self) -> bool {
        self.fetching_window
    }
}

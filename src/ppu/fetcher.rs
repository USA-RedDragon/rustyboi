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

const BACKGROUND_MAP_OFFSET: u16 = 6144;

// Tile data addressing constants
const TILE_DATA_8000_BASE: u16 = 0x8000; // $8000 method base (unsigned addressing)
const TILE_DATA_8800_BASE: u16 = 0x9000; // $8800 method base (signed addressing)

#[derive(Serialize, Deserialize, Clone)]
pub struct Fetcher {
    state: State,
    pub pixel_fifo: fifo::FIFO,

    tile_num: u8,
    tile_index: u8,
    pixel_buffer: [u8; 8],
}

impl Fetcher {
    pub fn new() -> Self {
        Fetcher {
            state: State::TileNumber,
            pixel_fifo: fifo::FIFO::new(),
            tile_num: 0,
            tile_index: 0,
            pixel_buffer: [0; 8],
        }
    }

    pub fn reset(&mut self) {
        self.state = State::TileNumber;
        self.pixel_fifo.reset();
        self.tile_num = 0;
        self.tile_index = 0;
        self.pixel_buffer = [0; 8];
    }

    // Calculate the correct tile data address based on LCDC.4 (BGWindowTileDataSelect)
    fn get_tile_data_address(&self, tile_id: u8, tile_line: u8, mmio: &mmio::MMIO) -> u16 {
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

    pub fn step(&mut self, mmio: &mut mmio::MMIO) {
        let y = mmio.read(ppu::LY).wrapping_add(mmio.read(ppu::SCY));
        let tile_line = y % 8;

        match self.state {
            State::TileNumber => {
                // Fetch the tile number from VRAM
                let map_addr = BACKGROUND_MAP_OFFSET + (y as u16 / 8) * 32;
                self.tile_num = mmio.read(mmio::VRAM_START + self.tile_index as u16 + map_addr);
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
}

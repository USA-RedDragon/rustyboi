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
                // Fetch the low byte of the tile data
                let offset = self.tile_num as u16 * 16; // Each tile is 16 bytes (8 lines, 2 bytes per line)
                let addr = offset + (tile_line as u16 * 2);
                let low_byte = mmio.read(mmio::VRAM_START + addr);
                for i in 0..8 {
                    self.pixel_buffer[i] = (low_byte >> i) & 0x01;
                }
                self.state = State::TileDataHigh;
            }
            State::TileDataHigh => {
                // Fetch the high byte of the tile data
                let offset = self.tile_num as u16 * 16; // Each tile is 16 bytes (8 lines, 2 bytes per line)
                let addr = offset + (tile_line as u16 * 2) + 1;
                let high_byte = mmio.read(mmio::VRAM_START + addr);
                for i in 0..8 {
                    // Combine low and high bytes to form the pixel data
                    self.pixel_buffer[i] |= ((high_byte >> i) & 0x01) << 1;
                }
                self.state = State::PushToFIFO;
            }
            State::PushToFIFO => {
                // Push the fetched tile data to the FIFO
                if self.pixel_fifo.size() <= 8 {
                    for i in (0..8).rev() {
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

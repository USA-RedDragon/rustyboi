use crate::cpu;
use crate::cpu::registers;
use crate::memory::mmio;
use crate::memory::Addressable;
use crate::ppu::fetcher;
use serde::{Deserialize, Serialize};

pub const LCD_CONTROL: u16 = 0xFF40;
pub const LCD_STATUS: u16 = 0xFF41;
pub const LY: u16 = 0xFF44;
pub const SCY: u16 = 0xFF42;
pub const LYC: u16 = 0xFF45;
pub const BGP: u16 = 0xFF47;

pub const FRAMEBUFFER_SIZE: usize = 160 * 144;

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

#[derive(Serialize, Deserialize, Clone)]
pub enum State {
    OAMSearch,
    PixelTransfer,
    HBlank,
    VBlank,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PPU {
    fetcher: fetcher::Fetcher,
    disabled: bool,
    state: State,
    ticks: u128,
    x: u8,

    #[serde(with = "serde_bytes")]
    fb_a: [u8; FRAMEBUFFER_SIZE],
    #[serde(with = "serde_bytes")]
    fb_b: [u8; FRAMEBUFFER_SIZE],
    have_frame: bool,
}

impl PPU {
    pub fn new() -> Self {
        PPU {
            fetcher: fetcher::Fetcher::new(),
            disabled: true,
            state: State::OAMSearch,
            ticks: 0,
            x: 0,
            fb_a: [0; FRAMEBUFFER_SIZE],
            fb_b: [0; FRAMEBUFFER_SIZE],
            have_frame: false,
        }
    }

    pub fn get_palette_color(&self, mmio: &mut mmio::MMIO, idx: u8) -> u8 {
        match idx {
            0 => mmio.read(BGP)&0x03,        // White
            1 => (mmio.read(BGP)>>2)&0x03, // Light Gray
            2 => (mmio.read(BGP)>>4)&0x03, // Dark Gray
            3 => (mmio.read(BGP)>>6)&0x03, // Black
            _ => 0x00, // Default to black for invalid indices
        }
    }

    pub fn step(&mut self, cpu: &mut cpu::SM83, mmio: &mut mmio::MMIO) {
        if self.disabled {
            if mmio.read(LCD_CONTROL)&(LCDCFlags::DisplayEnable as u8) != 0 {
                self.disabled = false;
                self.state = State::OAMSearch;
            } else {
                return;
            }
        } else {
            if mmio.read(LCD_CONTROL)&(LCDCFlags::DisplayEnable as u8) == 0 {
                mmio.write(LY, 0);
                self.x = 0;
                self.disabled = true;
                return;
            }
        }

        if mmio.read(LCD_CONTROL)&(LCDCFlags::BGWindowTileDataSelect as u8) == 1 {
            println!("PPU: BG/Window Tile Data Select is set to 1, this is not supported in DMG mode");
        }

        if mmio.read(LYC) == mmio.read(LY) {
            mmio.write(LCD_STATUS, mmio.read(LCD_STATUS) | (1 << 2)); // Set the LYC=LY flag
        } else {
            mmio.write(LCD_STATUS, mmio.read(LCD_STATUS) & !(1 << 2)); // Clear the LYC=LY flag
        }
        match self.state {
            State::OAMSearch => {
                // TODO: find sprites
                if self.ticks == 80 {
                    self.x = 0;
                    self.fetcher.reset();
                    self.state = State::PixelTransfer
                }
            },
            State::PixelTransfer => 'label: {
                if self.ticks%2 == 0 {
                    self.fetcher.step(mmio);
                }
                if self.fetcher.pixel_fifo.size() <= 8 {
                    break 'label;
                }

                // Put a pixel from the FIFO on screen.
                if let Ok(pixel_idx) = self.fetcher.pixel_fifo.pop() {
                    let ly = mmio.read(LY) as u16;
                    let fb_offset = (ly * 160) + self.x as u16;
                    self.fb_a[fb_offset as usize] = self.get_palette_color(mmio, pixel_idx);

                    self.x += 1;
                    if self.x == 160 {
                        self.state = State::HBlank
                    }
                } else {
                    break 'label;
                }
            },
            State::HBlank => {
                // no-ops
                if self.ticks == 339 {
                    self.ticks = 0;
                    mmio.write(LY, mmio.read(LY) + 1);
                    if mmio.read(LY) == 144 {
                        self.fb_b = self.fb_a;
                        self.fb_a = [0; FRAMEBUFFER_SIZE];
                        self.have_frame = true;
                        self.state = State::VBlank;
                        cpu.set_interrupt_flag(registers::InterruptFlag::VBlank, true, mmio);
                    } else {
                        self.state = State::OAMSearch;
                    }
                }
            },
            State::VBlank => {
                // no-ops
                if self.ticks == 4560 {
                    self.ticks = 0;
                    mmio.write(LY, mmio.read(LY) + 1);
                    if mmio.read(LY) == 153 {
                        mmio.write(LY, 0);
                        self.state = State::OAMSearch;
                    }
                }
            },
        }
        self.ticks += 1;
    }

    pub fn next_event_in_cycles(&self) -> u64 {
        // Return the number of cycles until the next PPU event
        500 // Placeholder value
    }

    pub fn frame_ready(&self) -> bool {
        self.have_frame
    }

    pub fn get_frame(&mut self) -> [u8; FRAMEBUFFER_SIZE] {
        self.have_frame = false;
        self.fb_b
    }
}

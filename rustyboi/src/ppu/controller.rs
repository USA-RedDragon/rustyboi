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
pub const SCX: u16 = 0xFF43;
pub const LYC: u16 = 0xFF45;
pub const BGP: u16 = 0xFF47;
pub const OBP0: u16 = 0xFF48; // Object Palette 0 Data
pub const OBP1: u16 = 0xFF49; // Object Palette 1 Data
pub const WY: u16 = 0xFF4A;  // Window Y Position
pub const WX: u16 = 0xFF4B;  // Window X Position

// CGB
pub const BCPS: u16 = 0xFF68; // Background Color Palette Specification
pub const BCPD: u16 = 0xFF69; // Background Color Palette Data
pub const OCPS: u16 = 0xFF6A; // Object Color Palette Specification
pub const OCPD: u16 = 0xFF6B; // Object Color Palette Data

pub const FRAMEBUFFER_SIZE: usize = 160 * 144;

// OAM constants
pub const OAM_SPRITE_COUNT: usize = 40; // 40 sprites total in OAM
pub const OAM_BYTES_PER_SPRITE: usize = 4; // 4 bytes per sprite
pub const MAX_SPRITES_PER_LINE: usize = 10; // Maximum 10 sprites per scanline

// Sprite attribute flags (from byte 3 of sprite data)
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct SpriteAttributes {
    pub priority: bool,    // 0 = above BG, 1 = behind BG colors 1-3
    pub y_flip: bool,      // 0 = normal, 1 = vertically mirrored
    pub x_flip: bool,      // 0 = normal, 1 = horizontally mirrored
    pub palette: bool,     // 0 = OBP0, 1 = OBP1
}

impl SpriteAttributes {
    pub fn from_byte(byte: u8) -> Self {
        SpriteAttributes {
            priority: (byte & 0x80) != 0,
            y_flip: (byte & 0x40) != 0,
            x_flip: (byte & 0x20) != 0,
            palette: (byte & 0x10) != 0,
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
    
    // Window state tracking
    window_line_counter: u8,    // Internal counter for window Y position
    window_y_triggered: bool,   // Whether WY condition was met this frame
    window_started_this_line: bool, // Whether window started rendering on current scanline
    
    #[serde(with = "serde_bytes")]
    fb_a: [u8; FRAMEBUFFER_SIZE],
    #[serde(with = "serde_bytes")]
    fb_b: [u8; FRAMEBUFFER_SIZE],
    have_frame: bool,
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
            window_line_counter: 0,
            window_y_triggered: false,
            window_started_this_line: false,
            fb_a: [0; FRAMEBUFFER_SIZE],
            fb_b: [0; FRAMEBUFFER_SIZE],
            have_frame: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
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

    pub fn step(&mut self, cpu: &mut cpu::SM83, mmio: &mut mmio::Mmio) {
        if self.disabled {
            if mmio.read(LCD_CONTROL)&(LCDCFlags::DisplayEnable as u8) != 0 {
                self.disabled = false;
                self.state = State::OAMSearch;
            } else {
                return;
            }
        } else if mmio.read(LCD_CONTROL)&(LCDCFlags::DisplayEnable as u8) == 0 {
            mmio.write(LY, 0);
            self.x = 0;
            self.disabled = true;
            return;
        }

        if mmio.read(LYC) == mmio.read(LY) {
            mmio.write(LCD_STATUS, mmio.read(LCD_STATUS) | (1 << 2)); // Set the LYC=LY flag
        } else {
            mmio.write(LCD_STATUS, mmio.read(LCD_STATUS) & !(1 << 2)); // Clear the LYC=LY flag
        }
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
                    let lcdc = mmio.read(LCD_CONTROL);
                    let window_enabled = (lcdc & (LCDCFlags::WindowDisplayEnable as u8)) != 0;
                    if window_enabled && self.window_y_triggered && ly > wy {
                        self.window_line_counter = self.window_line_counter.wrapping_add(1);
                    }
                    
                    // Reset window line flag for new scanline
                    self.window_started_this_line = false;
                    
                    // Initialize OAM search state
                    self.sprites_on_line.clear();
                    self.current_oam_sprite_index = 0;
                }
                
                // Perform sprite search distributed across 80 ticks
                // Check one sprite every 2 ticks (40 sprites × 2 ticks = 80 ticks)
                if self.ticks.is_multiple_of(2) && self.current_oam_sprite_index < OAM_SPRITE_COUNT {
                    self.check_single_sprite_for_scanline(mmio, self.current_oam_sprite_index);
                    self.current_oam_sprite_index += 1;
                }
                
                if self.ticks == 80 {
                    self.x = 0;
                    self.fetcher.reset_with_scx_offset(mmio);
                    mmio.write(LCD_STATUS, (mmio.read(LCD_STATUS) & !(1 << 1)) | (1 << 0)); // Set Mode 3 flag
                    self.state = State::PixelTransfer;
                }
            },
            State::PixelTransfer => 'label: {
                if self.ticks.is_multiple_of(2) {
                    self.fetcher.step(mmio, self.window_line_counter);
                }
                if self.fetcher.pixel_fifo.size() <= 8 {
                    break 'label;
                }

                // Check if we should start window rendering
                let lcdc = mmio.read(LCD_CONTROL);
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

                // Put a pixel from the FIFO on screen with sprite mixing
                if let Ok(bg_pixel_idx) = self.fetcher.pixel_fifo.pop() {
                    let ly = mmio.read(LY) as u16;
                    let fb_offset = (ly * 160) + self.x as u16;

                    // Get the final pixel color by mixing background and sprites
                    let final_color = self.mix_background_and_sprites(mmio, bg_pixel_idx, self.x, ly as u8);
                    self.fb_a[fb_offset as usize] = final_color;

                    self.x += 1;
                    if self.x == 160 {
                        self.state = State::HBlank;
                        mmio.write(LCD_STATUS, mmio.read(LCD_STATUS) & !((1 << 0) | (1 << 1))); // Set Mode 0 flag
                    }
                }
            },
            State::HBlank => {
                if self.ticks == 455 {
                    self.ticks = 0;
                    let current_ly = mmio.read(LY);
                    
                    if current_ly >= 143 {
                        mmio.write(LY, 144);
                        self.state = State::VBlank;
                        mmio.write(LCD_STATUS, (mmio.read(LCD_STATUS) & !(1 << 1)) | (1 << 0)); // Set Mode 1 flag
                        cpu.set_interrupt_flag(registers::InterruptFlag::VBlank, true, mmio);
                    } else {
                        // Continue to next visible scanline
                        let next_ly = current_ly.saturating_add(1);
                        mmio.write(LY, next_ly);
                        self.state = State::OAMSearch;
                        mmio.write(LCD_STATUS, mmio.read(LCD_STATUS) | (1 << 1)); // Set Mode 2 flag
                    }
                    return;
                }
            },
            State::VBlank => {
                if self.ticks == 455 {
                    self.ticks = 0;
                    let current_ly = mmio.read(LY);
                    
                    if current_ly >= 153 {
                        mmio.write(LY, 0);
                        self.state = State::OAMSearch;
                        mmio.write(LCD_STATUS, mmio.read(LCD_STATUS) | (1 << 1)); // Set Mode 2 flag
                        self.window_line_counter = 0;
                        self.window_y_triggered = false;
                        self.window_started_this_line = false;
                        self.fb_b = self.fb_a;
                        self.fb_a = [0; FRAMEBUFFER_SIZE];
                        self.have_frame = true;
                    } else if (144..153).contains(&current_ly) {
                        let next_ly = current_ly.saturating_add(1);
                        mmio.write(LY, next_ly);
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

    pub fn get_frame(&mut self) -> crate::gb::Frame {
        self.have_frame = false;
        crate::gb::Frame::Monochrome(self.fb_b)
    }

    // Debug methods
    pub fn get_fetcher_pixel_buffer(&self) -> [u8; 8] {
        self.fetcher.get_pixel_buffer()
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

    pub fn get_x(&self) -> u8 {
        self.x
    }

    pub fn has_frame(&self) -> bool {
        self.have_frame
    }

    pub fn get_sprites_on_line_count(&self) -> usize {
        self.sprites_on_line.len()
    }

    // Check a single sprite during distributed OAM search
    fn check_single_sprite_for_scanline(&mut self, mmio: &mmio::Mmio, sprite_index: usize) {
        // Skip if we already have the maximum sprites for this line
        if self.sprites_on_line.len() >= MAX_SPRITES_PER_LINE {
            return;
        }
        
        let ly = mmio.read(LY);
        let lcdc = mmio.read(LCD_CONTROL);
        
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

    // Mix background pixel with sprites at the given screen coordinates
    fn mix_background_and_sprites(&self, mmio: &mmio::Mmio, bg_pixel_idx: u8, screen_x: u8, screen_y: u8) -> u8 {
        let lcdc = mmio.read(LCD_CONTROL);
        
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
        let lcdc = mmio.read(LCD_CONTROL);
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
        let low_byte = mmio.read(tile_addr);
        let high_byte = mmio.read(tile_addr + 1);
        
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

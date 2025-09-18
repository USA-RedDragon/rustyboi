use crate::ppu;

pub struct Terminal {
    previous_frame: [u8; ppu::FRAMEBUFFER_SIZE],
    first_render: bool,
}

impl Terminal {
    pub fn new() -> Self {
        Self {
            previous_frame: [0; ppu::FRAMEBUFFER_SIZE],
            first_render: true,
        }
    }

    /// Get ANSI color code for a Game Boy palette color
    fn get_ansi_color(color: u8) -> &'static str {
        match color {
            0 => "\x1b[48;5;231m",  // White background
            1 => "\x1b[48;5;248m", // Light gray background (bright black)
            2 => "\x1b[48;5;240m",  // Dark gray background (black)
            3 => "\x1b[48;5;232m",  // Black background
            _ => "\x1b[48;5;232m",  // Default to black
        }
        // 0 => [0xFF, 0xFF, 0xFF, 0xFF], // White
        // 1 => [0xAA, 0xAA, 0xAA, 0xFF], // Light gray
        // 2 => [0x55, 0x55, 0x55, 0xFF], // Dark gray
        // 3 => [0x00, 0x00, 0x00, 0xFF], // Black
    }

    /// Get ANSI foreground color code for a Game Boy palette color
    fn get_ansi_fg_color(color: u8) -> &'static str {
        match color {
            0 => "\x1b[38;5;231m",  // White background
            1 => "\x1b[38;5;248m", // Light gray background (bright black)
            2 => "\x1b[38;5;240m",  // Dark gray background (black)
            3 => "\x1b[38;5;232m",  // Black background
            _ => "\x1b[38;5;232m",  // Default to black
        }
    }

    /// Generate the character and colors for a pair of pixels
    fn get_character_for_pixels(upper_pixel: u8, lower_pixel: u8) -> (&'static str, &'static str, &'static str) {
        match (upper_pixel, lower_pixel) {
            // Both pixels same color - use full block with that color as background
            (a, b) if a == b => ("█", Self::get_ansi_fg_color(a), Self::get_ansi_color(a)),
            
            // Upper pixel darker than lower - use upper half block
            (upper, lower) if upper > lower => ("▀", Self::get_ansi_fg_color(upper), Self::get_ansi_color(lower)),
            
            // Lower pixel darker than upper - use lower half block
            (upper, lower) if upper < lower => ("▄", Self::get_ansi_fg_color(lower), Self::get_ansi_color(upper)),
            
            // Fallback (shouldn't happen due to above conditions)
            (upper, lower) => ("▀", Self::get_ansi_fg_color(upper), Self::get_ansi_color(lower)),
        }
    }

    pub fn render_frame(&mut self, frame: &[u8; ppu::FRAMEBUFFER_SIZE]) {
        if self.first_render {
            // First render - clear screen and draw everything
            print!("\x1b[2J\x1b[H");
            self.render_full_frame(frame);
            self.first_render = false;
        } else {
            // Differential rendering - only update changed characters
            self.render_frame_diff(frame);
        }
        
        // Store current frame for next comparison
        self.previous_frame.copy_from_slice(frame);
        
        // Reset colors and flush output
        print!("\x1b[0m");
        use std::io::{self, Write};
        io::stdout().flush().unwrap();
    }

    fn render_full_frame(&self, frame: &[u8; ppu::FRAMEBUFFER_SIZE]) {
        for y in (0..144).step_by(2) {
            for x in 0..160 {
                let upper_pixel = frame[y * 160 + x];
                let lower_pixel = if y + 1 < 144 {
                    frame[(y + 1) * 160 + x]
                } else {
                    0
                };

                let (character, fg_color, bg_color) = Self::get_character_for_pixels(upper_pixel, lower_pixel);
                print!("{}{}{}\x1b[0m", bg_color, fg_color, character);
            }
            println!("\x1b[K");
        }
    }

    fn render_frame_diff(&self, frame: &[u8; ppu::FRAMEBUFFER_SIZE]) {
        for y in (0..144).step_by(2) {
            let terminal_row = y / 2 + 1; // Terminal rows are 1-indexed
            let mut col = 0;
            
            while col < 160 {
                let upper_pixel = frame[y * 160 + col];
                let lower_pixel = if y + 1 < 144 {
                    frame[(y + 1) * 160 + col]
                } else {
                    0
                };

                let prev_upper_pixel = self.previous_frame[y * 160 + col];
                let prev_lower_pixel = if y + 1 < 144 {
                    self.previous_frame[(y + 1) * 160 + col]
                } else {
                    0
                };

                // Check if this character position has changed
                if upper_pixel != prev_upper_pixel || lower_pixel != prev_lower_pixel {
                    // Move cursor to this position (1-indexed)
                    print!("\x1b[{};{}H", terminal_row, col + 1);
                    
                    // Render consecutive changed characters to minimize cursor movements
                    while col < 160 {
                        let upper = frame[y * 160 + col];
                        let lower = if y + 1 < 144 {
                            frame[(y + 1) * 160 + col]
                        } else {
                            0
                        };

                        let prev_upper = self.previous_frame[y * 160 + col];
                        let prev_lower = if y + 1 < 144 {
                            self.previous_frame[(y + 1) * 160 + col]
                        } else {
                            0
                        };

                        // Stop if this character hasn't changed
                        if upper == prev_upper && lower == prev_lower {
                            break;
                        }

                        let (character, fg_color, bg_color) = Self::get_character_for_pixels(upper, lower);
                        print!("{}{}{}\x1b[0m", bg_color, fg_color, character);
                        col += 1;
                    }
                } else {
                    col += 1;
                }
            }
        }
    }
}

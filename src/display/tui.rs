use crate::display;
use crate::ppu;

pub struct TUI {
}

impl TUI {
    pub fn new() -> Self {
        TUI {}
    }
}

impl display::Display for TUI {
    fn render_frame(&mut self, frame: [u8; ppu::FRAMEBUFFER_SIZE]) {
        // Simple TUI rendering: print the frame as ASCII art
        for y in 0..144 {
            for x in 0..160 {
                let pixel = frame[y * 160 + x];
                let symbol = match pixel {
                    0 => ' ',       // White
                    1 => '.',       // Light gray
                    2 => '*',       // Dark gray
                    3 => '#',       // Black
                    _ => ' ',       // Fallback
                };
                print!("{}", symbol);
            }
            println!();
        }
        println!("\nFrame rendered.\n");
    }
}
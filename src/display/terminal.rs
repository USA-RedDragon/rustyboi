use crate::ppu;

pub struct Terminal {
}

impl Terminal {
    pub fn render_frame(frame: &[u8; ppu::FRAMEBUFFER_SIZE]) {
        // Simple Terminal rendering: print the frame as ASCII art
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

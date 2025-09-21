#![forbid(unsafe_code)]

mod gb;
mod cartridge;
mod config;
mod cpu;
mod display;
mod input;
mod memory;
mod ppu;
mod timer;

use clap::Parser;
use pixels;

fn main() -> Result<(), pixels::Error> {
    let config = config::RawConfig::parse().clean();

    let mut gb = gb::GB::new(config.skip_bios);

    if let Some(state) = config.state.as_ref() {
        gb = gb::GB::from_state_file(state)
            .expect("Failed to load state file");
    }

    if let Some(rom) = config.rom.as_ref() {
        let cartridge = cartridge::Cartridge::load(rom)
            .expect("Failed to load ROM file");
        gb.insert(cartridge);
    }

    if let Some(bios) = config.bios.as_ref() {
        gb.load_bios(bios)
            .expect("Failed to load BIOS file");
    }

    if !config.cli {
        return display::run_with_gui(gb, &config);
    }

    // Create a stateful terminal instance for differential rendering
    let mut terminal = display::Terminal::new();
    
    loop {
        match gb.run_until_frame() {
            Some(frame) => {
                terminal.render_frame(&frame);
            }
            None => {
                // Emulator crashed, exit
                break;
            }
        }
    }
    Ok(())
}

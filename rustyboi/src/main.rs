#![forbid(unsafe_code)]

mod audio;
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
    #[cfg(target_arch = "wasm32")]
    {
        std::panic::set_hook(Box::new(console_error_panic_hook::hook));
        console_log::init_with_level(log::Level::Trace).expect("error initializing logger");

        let config = config::RawConfig::try_parse_from(std::iter::empty::<String>())
            .expect("Failed to create default config").clean();
        wasm_bindgen_futures::spawn_local(display::run_with_gui_async(gb::GB::new(true), config));
        return Ok(());
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
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
        let mut terminal = display::Terminal::new(config.palette);
        
        loop {
            // Update input from terminal (placeholder implementation)
            terminal.update_input(&config.keybinds);
            let (a, b, start, select, up, down, left, right) = terminal.get_input_state();
            gb.set_input_state(a, b, start, select, up, down, left, right);
            
            // Don't collect audio in terminal mode to keep it simple
            let (frame_data, _breakpoint_hit) = gb.run_until_frame(false);
            terminal.render_frame(&frame_data);
        }
    }
}

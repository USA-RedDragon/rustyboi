use rustyboi_core_lib::gb;

use crate::config;
use crate::display;
use clap::Parser;

pub async fn run() {
    #[cfg(target_arch = "wasm32")]
    {
        std::panic::set_hook(Box::new(console_error_panic_hook::hook));
        // Only initialize logger if it hasn't been initialized yet
        let _ = console_log::init_with_level(log::Level::Trace);

        let config = config::RawConfig::try_parse_from(std::iter::empty::<String>())
            .expect("Failed to create default config").clean();
        
        wasm_bindgen_futures::spawn_local(display::run_with_gui_async(gb::GB::new(config.hardware), config));
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustyboi_core_lib::cartridge;

        let config = config::RawConfig::parse().clean();

        let mut gb = gb::GB::new(config.hardware);

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

        if config.skip_bios {
            gb.skip_bios();
        }

        match display::run_with_gui(gb, config) {
            Ok(_) => {}
            Err(e) => eprintln!("Error: {}", e),
        }
    }
}

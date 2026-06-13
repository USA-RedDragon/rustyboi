use rustyboi_core_lib::gb;

use crate::config;
use crate::display;
use clap::Parser;

pub fn run() -> Result<(), pixels::Error> {
    #[cfg(target_arch = "wasm32")]
    {
        std::panic::set_hook(Box::new(console_error_panic_hook::hook));
        console_log::init_with_level(log::Level::Trace).expect("error initializing logger");

        let config = config::RawConfig::try_parse_from(std::iter::empty::<String>())
            .expect("Failed to create default config").clean();
        wasm_bindgen_futures::spawn_local(display::run_with_gui_async(Box::new(gb::GB::new(config.hardware)), config));
        return Ok(());
    }

    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        use rustyboi_core_lib::cartridge;

        let config = config::RawConfig::parse().clean();

        let mut gb = Box::new(gb::GB::new(config.hardware));

        if let Some(state) = config.state.as_ref() {
            *gb = gb::GB::from_state_file(state)
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

        display::run_with_gui(gb, &config)
    }

    #[cfg(target_os = "android")]
    {
        // android_main bootstraps Android via `run_android` below; this
        // entry point is only reachable on desktop targets.
        unreachable!("run() should not be invoked on Android; use run_android instead")
    }
}

/// Android entry point. Called from `android_main` with the `AndroidApp`
/// handle. Builds a default `CleanConfig` (no CLI on Android) and hands
/// control to the shared GUI loop, which lazily creates the rendering
/// surface on `Event::Resumed`.
#[cfg(target_os = "android")]
pub fn run_android(
    app: winit::platform::android::activity::AndroidApp,
) -> Result<(), pixels::Error> {
    use crate::android::raw_log;

    // We must stay on the JVM-spawned `android_main` thread because the
    // ALooper used by android-activity is associated with this thread; if
    // we moved the event loop to a worker thread, ALooper_pollAll would
    // return POLL_ERROR. `gb::GB` is large (VRAM/WRAM/palette/framebuffers
    // are inlined), so we heap-allocate it and pass `Box<GB>` through the
    // call chain to keep the stack small on this thread.
    raw_log("run_android: parsing default config");
    let config = config::RawConfig::try_parse_from(std::iter::empty::<String>())
        .expect("Failed to create default config")
        .clean();
    raw_log("run_android: building GB on heap");
    let mut gb = Box::new(gb::GB::new(config.hardware));
    // Android has no BIOS path and no CLI flag, so always skip the BIOS.
    // Without this, `World::load_rom` resets the GB and the CPU spins at
    // PC=0x0000 reading 0xFF (no BIOS mapped), so ROMs never start.
    // Setting `skip_bios` here both bootstraps the registers and ensures
    // every subsequent `gb.reset()` re-applies the skip.
    gb.skip_bios();
    raw_log("run_android: calling run_with_gui_android");
    let r = display::run_with_gui_android(app, gb, &config);
    raw_log("run_android: run_with_gui_android returned");
    r
}

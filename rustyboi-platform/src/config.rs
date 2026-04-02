//! CLI parsing + desktop keybinds. The presentation palette now lives in
//! `rustyboi-frontend` (`ColorPalette`); this module only re-exports it for the
//! call sites and owns the winit-`KeyCode` keybind table (a desktop concern).

use clap::Parser;
use rustyboi_core_lib::gb;
use winit::keyboard::KeyCode;

pub use rustyboi_frontend_lib::ColorPalette;

#[derive(Debug, Clone)]
pub struct KeyBinds {
    pub a: KeyCode,
    pub b: KeyCode,
    pub start: KeyCode,
    pub select: KeyCode,
    pub up: KeyCode,
    pub down: KeyCode,
    pub left: KeyCode,
    pub right: KeyCode,
}

impl Default for KeyBinds {
    fn default() -> Self {
        Self {
            a: KeyCode::KeyZ,
            b: KeyCode::KeyX,
            start: KeyCode::Enter,
            select: KeyCode::Space,
            up: KeyCode::ArrowUp,
            down: KeyCode::ArrowDown,
            left: KeyCode::ArrowLeft,
            right: KeyCode::ArrowRight,
        }
    }
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct RawConfig {
    /// BIOS file path, optional
    #[arg(short, long)]
    bios: Option<String>,

    // Hardware type (DMG, CGB, SGB, etc.)
    #[arg(short = 't', long, default_value = "cgb")]
    hardware: gb::Hardware,

    /// ROM file path, optional
    #[arg(short, long)]
    rom: Option<String>,

    /// Save state file path to load on startup, optional
    #[arg(long)]
    state: Option<String>,

    /// Scale factor for GUI
    #[arg(short, long, default_value_t = 5)]
    scale: u8,

    /// Color palette (grayscale, green, blue, brown, red)
    #[arg(short, long, default_value = "grayscale")]
    palette: String,

    /// Skip BIOS on startup
    #[arg(long, default_value_t = false)]
    skip_bios: bool,

    /// Attach a Game Boy Printer to the link port; captured prints are
    /// written as PNGs next to the ROM
    #[arg(long, default_value_t = false)]
    printer: bool,
}

pub struct CleanConfig {
    // path to BIOS file
    pub bios: Option<String>,
    // path to ROM file
    pub rom: Option<String>,
    // Hardware type (DMG, CGB, SGB, etc.)
    pub hardware: gb::Hardware,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    // path to save state to load on startup
    pub state: Option<String>,
    // GUI scale factor
    #[cfg(not(target_os = "android"))]
    pub scale: u8,
    // Color palette
    pub palette: ColorPalette,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    // skip BIOS on startup
    pub skip_bios: bool,
    // attach a Game Boy Printer to the link port at startup
    pub printer: bool,
    // keybinds configuration
    pub keybinds: KeyBinds,
}

impl RawConfig {
    pub fn clean(self) -> CleanConfig {
        let mut _skip_bios = self.skip_bios;
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            if self.bios.is_none() {
                _skip_bios = true;
            }
        }

        CleanConfig {
            bios: self.bios,
            rom: self.rom,
            hardware: self.hardware,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            state: self.state,
            #[cfg(not(target_os = "android"))]
            scale: self.scale,
            palette: ColorPalette::from_str(&self.palette).unwrap_or_default(),
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            skip_bios: _skip_bios,
            printer: self.printer,
            keybinds: KeyBinds::default(),
        }
    }
}

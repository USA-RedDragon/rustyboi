//! CLI parsing. The presentation palette is the shared
//! [`DmgPaletteChoice`](rustyboi_session::DmgPaletteChoice); this module re-exports it
//! for the call sites. GB-button bindings + hotkeys now live in the shared
//! `rustyboi_session::InputConfig` (persisted config), not a desktop-private
//! table.

use clap::Parser;
use rustyboi_core_lib::gb;

pub use rustyboi_frontend_lib::DmgPaletteChoice;

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

    /// Color palette (greenlcd, grayscale, green, pocket, ...)
    #[arg(short, long, default_value = "greenlcd")]
    palette: String,

    /// Skip BIOS on startup
    #[arg(long, default_value_t = false)]
    skip_bios: bool,

    /// Attach a Game Boy Printer to the link port; captured prints are
    /// written as PNGs next to the ROM
    #[arg(long, default_value_t = false)]
    printer: bool,

    /// Rendering backend for this run: auto, vulkan, metal, opengl, or
    /// software. Overrides (without persisting) the saved Settings choice;
    /// auto probes the platform's native API first (Vulkan, or Metal on
    /// Apple), then OpenGL, then the CPU software renderer.
    #[arg(long)]
    graphics: Option<String>,
}

pub struct CleanConfig {
    // path to BIOS file
    pub bios: Option<String>,
    // path to ROM file
    pub rom: Option<String>,
    // Hardware type (DMG, CGB, SGB, etc.)
    pub hardware: gb::Hardware,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    // path to save state to load on startup
    pub state: Option<String>,
    // GUI scale factor
    #[cfg(not(target_os = "android"))]
    pub scale: u8,
    // Color palette
    pub palette: DmgPaletteChoice,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    // skip BIOS on startup
    pub skip_bios: bool,
    // attach a Game Boy Printer to the link port at startup
    pub printer: bool,
    // rendering backend override for this run (None = use the saved Settings
    // choice); never persisted
    pub graphics: Option<rustyboi_session::GraphicsBackend>,
}

impl RawConfig {
    pub fn clean(self) -> CleanConfig {
        let mut _skip_bios = self.skip_bios;
        #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
        {
            if self.bios.is_none() {
                _skip_bios = true;
            }
        }

        CleanConfig {
            bios: self.bios,
            rom: self.rom,
            hardware: self.hardware,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
            state: self.state,
            #[cfg(not(target_os = "android"))]
            scale: self.scale,
            palette: DmgPaletteChoice::from_str(&self.palette).unwrap_or(DmgPaletteChoice::GreenLcd),
            #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
            skip_bios: _skip_bios,
            printer: self.printer,
            graphics: self.graphics.as_deref().and_then(|s| {
                let parsed = rustyboi_session::GraphicsBackend::from_option_id(s);
                if parsed.is_none() {
                    eprintln!("unknown --graphics value '{s}' (expected auto|vulkan|metal|opengl|software); using saved setting");
                }
                parsed
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> CleanConfig {
        RawConfig::try_parse_from(args).expect("args parse").clean()
    }

    #[test]
    fn clap_defaults_match_declarations() {
        let c = parse(&["rustyboi"]);
        assert_eq!(c.hardware, gb::Hardware::CGB);
        assert_eq!(c.palette, DmgPaletteChoice::GreenLcd);
        #[cfg(not(target_os = "android"))]
        assert_eq!(c.scale, 5);
    }

    #[test]
    fn garbage_palette_falls_back_to_green_lcd() {
        let c = parse(&["rustyboi", "--palette", "chartreuse"]);
        assert_eq!(c.palette, DmgPaletteChoice::GreenLcd);
    }

    #[test]
    fn known_palette_alias_is_honored() {
        // A recognized alias must NOT fall through to the Grayscale default.
        let c = parse(&["rustyboi", "--palette", "green"]);
        assert_eq!(c.palette, DmgPaletteChoice::GreenLinear);
    }

    #[test]
    fn unknown_graphics_value_is_none() {
        let c = parse(&["rustyboi", "--graphics", "banana"]);
        assert!(c.graphics.is_none(), "an unrecognized backend falls back to the saved setting");
    }

    #[test]
    fn known_graphics_value_parses() {
        let c = parse(&["rustyboi", "--graphics", "software"]);
        assert_eq!(c.graphics, Some(rustyboi_session::GraphicsBackend::Software));
    }

    #[test]
    fn no_graphics_flag_is_none() {
        assert!(parse(&["rustyboi"]).graphics.is_none());
    }

    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    #[test]
    fn desktop_skips_bios_when_no_bios_given() {
        // No --bios ⇒ skip_bios is forced true so the machine still boots.
        assert!(parse(&["rustyboi"]).skip_bios);
        // Supplying a BIOS leaves the (false) default in place.
        assert!(!parse(&["rustyboi", "--bios", "boot.bin"]).skip_bios);
    }
}

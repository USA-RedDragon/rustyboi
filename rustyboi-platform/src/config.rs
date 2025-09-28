use clap::Parser;
use winit::keyboard::KeyCode;
use rustyboi_core_lib::gb;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorPalette {
    Grayscale,
    OriginalGreen,
    Blue,
    Brown,
    Red,
}

impl Default for ColorPalette {
    fn default() -> Self {
        Self::Grayscale
    }
}

impl ColorPalette {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "grayscale" | "gray" | "grey" => Some(Self::Grayscale),
            "green" | "original" | "gameboy" => Some(Self::OriginalGreen),
            "blue" => Some(Self::Blue),
            "brown" | "sepia" => Some(Self::Brown),
            "red" => Some(Self::Red),
            _ => None,
        }
    }
    /// Returns RGBA colors for Game Boy palette values 0-3
    pub fn get_rgba_colors(&self) -> [[u8; 4]; 4] {
        match self {
            Self::Grayscale => [
                [0xFF, 0xFF, 0xFF, 0xFF], // White
                [0xAA, 0xAA, 0xAA, 0xFF], // Light gray
                [0x55, 0x55, 0x55, 0xFF], // Dark gray
                [0x00, 0x00, 0x00, 0xFF], // Black
            ],
            Self::OriginalGreen => [
                [0x9B, 0xBC, 0x0F, 0xFF], // Light green
                [0x8B, 0xAC, 0x0F, 0xFF], // Medium green
                [0x30, 0x62, 0x30, 0xFF], // Dark green
                [0x0F, 0x38, 0x0F, 0xFF], // Darkest green
            ],
            Self::Blue => [
                [0xE0, 0xF8, 0xFF, 0xFF], // Light blue
                [0x86, 0xC0, 0xEA, 0xFF], // Medium blue
                [0x2E, 0x59, 0x8D, 0xFF], // Dark blue
                [0x1A, 0x1C, 0x2C, 0xFF], // Darkest blue
            ],
            Self::Brown => [
                [0xFF, 0xF6, 0xD3, 0xFF], // Light brown
                [0xBF, 0x8B, 0x67, 0xFF], // Medium brown
                [0x7F, 0x4F, 0x24, 0xFF], // Dark brown
                [0x33, 0x20, 0x14, 0xFF], // Darkest brown
            ],
            Self::Red => [
                [0xFF, 0xE4, 0xE1, 0xFF], // Light red
                [0xFF, 0xA5, 0x9E, 0xFF], // Medium red
                [0xBF, 0x30, 0x30, 0xFF], // Dark red
                [0x7F, 0x0A, 0x0A, 0xFF], // Darkest red
            ],
        }
    }
}

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
}

#[derive(Clone)]
pub struct CleanConfig {
    // path to BIOS file
    pub bios: Option<String>,
    // path to ROM file
    pub rom: Option<String>,
    // Hardware type (DMG, CGB, SGB, etc.)
    pub hardware: gb::Hardware,
    #[cfg(not(target_arch = "wasm32"))]
    // path to save state to load on startup
    pub state: Option<String>,
    // GUI scale factor
    pub scale: u8,
    // Color palette
    pub palette: ColorPalette,
    #[cfg(not(target_arch = "wasm32"))]
    // skip BIOS on startup
    pub skip_bios: bool,
    // keybinds configuration
    pub keybinds: KeyBinds,
}

impl RawConfig {
    pub fn clean(self) -> CleanConfig {
        let mut _skip_bios = self.skip_bios;
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.bios.is_none() {
                _skip_bios = true;
            }
        }

        CleanConfig {
            bios: self.bios,
            rom: self.rom,
            hardware: self.hardware,
            #[cfg(not(target_arch = "wasm32"))]
            state: self.state,
            scale: self.scale,
            palette: ColorPalette::from_str(&self.palette).unwrap_or_default(),
            #[cfg(not(target_arch = "wasm32"))]
            skip_bios: _skip_bios,
            keybinds: KeyBinds::default(),
        }
    }
}

impl CleanConfig {
    /// Get default configuration by parsing empty arguments with clap
    pub fn default() -> Self {
        use clap::Parser;
        let raw_config = RawConfig::parse_from(std::iter::empty::<&str>());
        raw_config.clean()
    }
}

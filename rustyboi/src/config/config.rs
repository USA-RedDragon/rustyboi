use clap::Parser;
use winit::keyboard::KeyCode;

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

    /// Returns ANSI color codes for terminal display
    pub fn get_ansi_bg_colors(&self) -> [&'static str; 4] {
        match self {
            Self::Grayscale => [
                "\x1b[48;5;231m", // White
                "\x1b[48;5;248m", // Light gray
                "\x1b[48;5;240m", // Dark gray
                "\x1b[48;5;232m", // Black
            ],
            Self::OriginalGreen => [
                "\x1b[48;2;155;188;15m", // Light green
                "\x1b[48;2;139;172;15m", // Medium green
                "\x1b[48;2;48;98;48m",   // Dark green
                "\x1b[48;2;15;56;15m",   // Darkest green
            ],
            Self::Blue => [
                "\x1b[48;2;224;248;255m", // Light blue
                "\x1b[48;2;134;192;234m", // Medium blue
                "\x1b[48;2;46;89;141m",   // Dark blue
                "\x1b[48;2;26;28;44m",    // Darkest blue
            ],
            Self::Brown => [
                "\x1b[48;2;255;246;211m", // Light brown
                "\x1b[48;2;191;139;103m", // Medium brown
                "\x1b[48;2;127;79;36m",   // Dark brown
                "\x1b[48;2;51;32;20m",    // Darkest brown
            ],
            Self::Red => [
                "\x1b[48;2;255;228;225m", // Light red
                "\x1b[48;2;255;165;158m", // Medium red
                "\x1b[48;2;191;48;48m",   // Dark red
                "\x1b[48;2;127;10;10m",   // Darkest red
            ],
        }
    }

    /// Returns ANSI foreground color codes for terminal display
    pub fn get_ansi_fg_colors(&self) -> [&'static str; 4] {
        match self {
            Self::Grayscale => [
                "\x1b[38;5;231m", // White
                "\x1b[38;5;248m", // Light gray
                "\x1b[38;5;240m", // Dark gray
                "\x1b[38;5;232m", // Black
            ],
            Self::OriginalGreen => [
                "\x1b[38;2;155;188;15m", // Light green
                "\x1b[38;2;139;172;15m", // Medium green
                "\x1b[38;2;48;98;48m",   // Dark green
                "\x1b[38;2;15;56;15m",   // Darkest green
            ],
            Self::Blue => [
                "\x1b[38;2;224;248;255m", // Light blue
                "\x1b[38;2;134;192;234m", // Medium blue
                "\x1b[38;2;46;89;141m",   // Dark blue
                "\x1b[38;2;26;28;44m",    // Darkest blue
            ],
            Self::Brown => [
                "\x1b[38;2;255;246;211m", // Light brown
                "\x1b[38;2;191;139;103m", // Medium brown
                "\x1b[38;2;127;79;36m",   // Dark brown
                "\x1b[38;2;51;32;20m",    // Darkest brown
            ],
            Self::Red => [
                "\x1b[38;2;255;228;225m", // Light red
                "\x1b[38;2;255;165;158m", // Medium red
                "\x1b[38;2;191;48;48m",   // Dark red
                "\x1b[38;2;127;10;10m",   // Darkest red
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

    /// Run with CLI (no GUI)
    #[arg(long, default_value_t = false)]
    cli: bool,

    /// Skip BIOS on startup
    #[arg(long, default_value_t = false)]
    skip_bios: bool,
}

pub struct CleanConfig {
    // path to BIOS file
    pub bios: Option<String>,
    // path to ROM file
    pub rom: Option<String>,
    // path to save state to load on startup
    pub state: Option<String>,
    // GUI scale factor
    pub scale: u8,
    // Color palette
    pub palette: ColorPalette,
    // run in CLI mode (no GUI)
    pub cli: bool,
    // skip BIOS on startup
    pub skip_bios: bool,
    // keybinds configuration
    pub keybinds: KeyBinds,
}

impl RawConfig {
    pub fn clean(self) -> CleanConfig {
        let mut skip_bios = self.skip_bios;
        if self.bios.is_none() {
            skip_bios = true;
        }

        CleanConfig {
            bios: self.bios,
            rom: self.rom,
            state: self.state,
            scale: self.scale,
            palette: ColorPalette::from_str(&self.palette).unwrap_or(ColorPalette::default()),
            cli: self.cli,
            skip_bios: skip_bios,
            keybinds: KeyBinds::default(),
        }
    }
}

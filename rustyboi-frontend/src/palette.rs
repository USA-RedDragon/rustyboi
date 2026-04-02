//! The monochrome DMG presentation palettes. Pure data: maps a chosen palette
//! to the four RGBA shades used when converting a `Frame::Monochrome` for
//! display, and bridges to/from the egui `PaletteChoice` the Settings menu
//! surfaces. Lives in the frontend (not the platform) because it is display
//! logic every frontend shares, with no OS coupling.

use rustyboi_egui_lib::actions::PaletteChoice;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorPalette {
    #[default]
    Grayscale,
    OriginalGreen,
    Blue,
    Brown,
    Red,
}

impl ColorPalette {
    #[allow(clippy::should_implement_trait)]
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

    /// RGBA colors for Game Boy palette values 0-3 (lightest to darkest).
    pub fn get_rgba_colors(&self) -> [[u8; 4]; 4] {
        match self {
            Self::Grayscale => [
                [0xFF, 0xFF, 0xFF, 0xFF],
                [0xAA, 0xAA, 0xAA, 0xFF],
                [0x55, 0x55, 0x55, 0xFF],
                [0x00, 0x00, 0x00, 0xFF],
            ],
            Self::OriginalGreen => [
                [0x9B, 0xBC, 0x0F, 0xFF],
                [0x8B, 0xAC, 0x0F, 0xFF],
                [0x30, 0x62, 0x30, 0xFF],
                [0x0F, 0x38, 0x0F, 0xFF],
            ],
            Self::Blue => [
                [0xE0, 0xF8, 0xFF, 0xFF],
                [0x86, 0xC0, 0xEA, 0xFF],
                [0x2E, 0x59, 0x8D, 0xFF],
                [0x1A, 0x1C, 0x2C, 0xFF],
            ],
            Self::Brown => [
                [0xFF, 0xF6, 0xD3, 0xFF],
                [0xBF, 0x8B, 0x67, 0xFF],
                [0x7F, 0x4F, 0x24, 0xFF],
                [0x33, 0x20, 0x14, 0xFF],
            ],
            Self::Red => [
                [0xFF, 0xE4, 0xE1, 0xFF],
                [0xFF, 0xA5, 0x9E, 0xFF],
                [0xBF, 0x30, 0x30, 0xFF],
                [0x7F, 0x0A, 0x0A, 0xFF],
            ],
        }
    }

    /// The egui Settings-menu choice matching this palette.
    pub fn to_choice(self) -> PaletteChoice {
        match self {
            ColorPalette::Grayscale => PaletteChoice::Grayscale,
            ColorPalette::OriginalGreen => PaletteChoice::OriginalGreen,
            ColorPalette::Blue => PaletteChoice::Blue,
            ColorPalette::Brown => PaletteChoice::Brown,
            ColorPalette::Red => PaletteChoice::Red,
        }
    }

    /// Map an egui Settings-menu choice to a concrete palette.
    pub fn from_choice(choice: PaletteChoice) -> Self {
        match choice {
            PaletteChoice::Grayscale => ColorPalette::Grayscale,
            PaletteChoice::OriginalGreen => ColorPalette::OriginalGreen,
            PaletteChoice::Blue => ColorPalette::Blue,
            PaletteChoice::Brown => ColorPalette::Brown,
            PaletteChoice::Red => ColorPalette::Red,
        }
    }
}

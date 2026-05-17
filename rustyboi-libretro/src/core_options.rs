//! The libretro core-option table, generated from the shared `rustyboi_session`
//! enums so the enums are the SINGLE source of truth: add a variant and the
//! RetroArch option list, the parser ([`super::RustyboiCore::read_options`]),
//! and the GUI menus all update together.
//!
//! This produces a [`CoreOptions`] (the framework's declarative builder); turning
//! it into the C `retro_core_options_v2` table lives in `rustyboi-libretro-sys`.

use rustyboi_libretro_sys::{CoreOptions, OptionCategory, OptionDef, OptionValue};

use rustyboi_session::action::{GbcDmgPalette, HardwareChoice, PaletteChoice};
use rustyboi_session::CgbColorConversion;

/// Option keys, defined once and referenced by both the generated table and the
/// parser in [`super::RustyboiCore::read_options`], so a typo can't desync them.
pub const KEY_HARDWARE: &str = "rustyboi_hardware";
pub const KEY_REAL_BOOT_ROM: &str = "rustyboi_real_boot_rom";
pub const KEY_SGB_BORDER: &str = "rustyboi_sgb_border";
pub const KEY_DMG_PALETTE: &str = "rustyboi_dmg_palette";
pub const KEY_GBC_DMG_PALETTE: &str = "rustyboi_gbc_dmg_palette";
pub const KEY_GBC_COLOR_CORRECTION: &str = "rustyboi_gbc_color_correction";

/// Canonical on/off value ids (the libretro convention).
pub const OFF: &str = "disabled";
pub const ON: &str = "enabled";

/// The colour-correction modes as (id, label, value) — the single source for
/// both the generated option and its parser, so the two can't disagree.
pub const COLOR_CORRECTION: [(&str, &str, CgbColorConversion); 2] = [
    ("linear", "Linear (raw)", CgbColorConversion::Linear),
    ("lcd", "LCD (corrected)", CgbColorConversion::Lcd),
];

/// Parse a colour-correction id, or `None` if unrecognized.
pub fn parse_color_correction(id: &str) -> Option<CgbColorConversion> {
    COLOR_CORRECTION.iter().find(|(k, _, _)| *k == id).map(|(_, _, v)| *v)
}

fn value(id: impl Into<String>, label: impl Into<String>) -> OptionValue {
    OptionValue { value: id.into(), label: label.into() }
}

/// A two-state (disabled/enabled) value list.
fn on_off() -> Vec<OptionValue> {
    vec![value(OFF, "Disabled"), value(ON, "Enabled")]
}

/// Build the full option table from the shared enums.
pub fn build() -> CoreOptions {
    // "Auto" is libretro-only (header sniff); the concrete models come from the
    // shared enum so the list can never drift from what the core can emulate.
    let mut hardware_values = vec![value("auto", "Auto (CGB / DMG by header)")];
    hardware_values
        .extend(HardwareChoice::ALL.into_iter().map(|c| value(c.option_id(), c.label())));

    let palette_values: Vec<OptionValue> = PaletteChoice::ALL
        .into_iter()
        .map(|p| value(p.option_id(), p.label()))
        .collect();

    let gbc_dmg_values: Vec<OptionValue> = GbcDmgPalette::choices()
        .into_iter()
        .map(|(c, label)| value(c.option_id(), label))
        .collect();

    let color_values: Vec<OptionValue> =
        COLOR_CORRECTION.iter().map(|(id, label, _)| value(*id, *label)).collect();

    CoreOptions {
        categories: vec![
            OptionCategory {
                key: "system_settings",
                desc: "System",
                info: "Hardware emulation options.",
            },
            OptionCategory {
                key: "video_settings",
                desc: "Video",
                info: "Palette and colour options.",
            },
        ],
        options: vec![
            OptionDef {
                key: KEY_HARDWARE,
                desc: "System > Hardware Model",
                desc_categorized: "Hardware Model",
                info: "Which Game Boy model / silicon revision to emulate. 'Auto' picks CGB unless the ROM header marks it DMG-only. Takes effect on content reload.",
                category: "system_settings",
                values: hardware_values,
                default: "auto".into(),
            },
            OptionDef {
                key: KEY_REAL_BOOT_ROM,
                desc: "System > Use Real Boot ROM",
                desc_categorized: "Use Real Boot ROM",
                info: "Run the real boot ROM from the frontend's system directory (e.g. dmg_boot.bin, cgb_boot.bin) instead of a synthetic post-boot state. Falls back to skip-boot if absent. Takes effect on content reload.",
                category: "system_settings",
                values: on_off(),
                default: OFF.into(),
            },
            OptionDef {
                key: KEY_SGB_BORDER,
                desc: "Video > Super Game Boy Border",
                desc_categorized: "Super Game Boy Border",
                info: "On Super Game Boy hardware, output the 256x224 composited frame with the game's border.",
                category: "video_settings",
                values: on_off(),
                default: OFF.into(),
            },
            OptionDef {
                key: KEY_DMG_PALETTE,
                desc: "Video > DMG Palette",
                desc_categorized: "DMG Palette",
                info: "Colour palette for original Game Boy (monochrome) output. No effect on Game Boy Color titles, which supply their own colours.",
                category: "video_settings",
                values: palette_values,
                default: PaletteChoice::Grayscale.option_id().into(),
            },
            OptionDef {
                key: KEY_GBC_DMG_PALETTE,
                desc: "Video > GBC Palette (DMG games)",
                desc_categorized: "GBC Palette (DMG games)",
                info: "CGB colorization for original Game Boy games running in Game Boy Color mode. 'Auto' uses the boot ROM's per-title palette; the others force one of the boot-ROM button-combo schemes. No effect on DMG hardware or on Game Boy Color titles.",
                category: "video_settings",
                values: gbc_dmg_values,
                default: GbcDmgPalette::Auto.option_id().into(),
            },
            OptionDef {
                key: KEY_GBC_COLOR_CORRECTION,
                desc: "Video > GBC Colour Correction",
                desc_categorized: "GBC Colour Correction",
                info: "Colour conversion for Game Boy Color output. 'LCD' approximates the real hardware LCD; 'Linear' is the raw RGB555 values.",
                category: "video_settings",
                values: color_values,
                default: COLOR_CORRECTION[0].0.into(),
            },
        ],
    }
}

//! Runtime generation of the libretro core-option table from the shared
//! `rustyboi_session` enums.
//!
//! The model and palette value lists come straight from
//! [`HardwareChoice::ALL`] / [`PaletteChoice::ALL`] (+ their `option_id` /
//! `label`), so the enums are the SINGLE source of truth: add a variant and the
//! RetroArch option list, the parser ([`super::read_options`]), and the GUI menus
//! all update together. This replaces the old hand-maintained
//! `#[derive(CoreOptions)]` literal list, which had to be kept in sync by hand
//! and had already drifted (the DMG palette exposed 3 of 7 choices).

use rust_libretro::sys::{
    retro_core_option_v2_category, retro_core_option_v2_definition, retro_core_option_value,
    retro_core_options_v2,
};
use std::ffi::CString;
use std::os::raw::c_char;

use rustyboi_session::action::{HardwareChoice, PaletteChoice};
use rustyboi_session::CgbColorConversion;

/// libretro caps each option's value list (including the NULL terminator) at
/// this length.
const MAX_VALUES: usize = 128;

/// The option keys, defined once here and referenced by both the generated
/// table and the parser in [`super::read_options`], so a typo can't silently
/// desync the two (there is exactly one spelling of each key).
pub const KEY_HARDWARE: &str = "rustyboi_hardware";
pub const KEY_REAL_BOOT_ROM: &str = "rustyboi_real_boot_rom";
pub const KEY_SGB_BORDER: &str = "rustyboi_sgb_border";
pub const KEY_DMG_PALETTE: &str = "rustyboi_dmg_palette";
pub const KEY_GBC_COLOR_CORRECTION: &str = "rustyboi_gbc_color_correction";

/// Canonical on/off option value ids (the libretro convention). Shared by the
/// generated table below and the parser in [`super::read_options`].
pub const OFF: &str = "disabled";
pub const ON: &str = "enabled";

/// The colour-correction modes as (id, label, value) — the single source for
/// both the generated `rustyboi_gbc_color_correction` option and its parser, so
/// the two can't disagree.
pub const COLOR_CORRECTION: [(&str, &str, CgbColorConversion); 2] = [
    ("linear", "Linear (raw)", CgbColorConversion::Linear),
    ("lcd", "LCD (corrected)", CgbColorConversion::Lcd),
];

/// Parse a colour-correction id, or `None` if unrecognized.
pub fn parse_color_correction(id: &str) -> Option<CgbColorConversion> {
    COLOR_CORRECTION.iter().find(|(k, _, _)| *k == id).map(|(_, _, v)| *v)
}

/// Owns every `CString` the option table points into, plus the C arrays, so the
/// pointers handed to the frontend stay valid for the core's lifetime.
pub struct OwnedOptions {
    // Backing storage for every C string the arrays below reference. Never read
    // directly; kept alive so the raw pointers remain valid.
    _strings: Vec<CString>,
    categories: Vec<retro_core_option_v2_category>,
    definitions: Vec<retro_core_option_v2_definition>,
}

impl OwnedOptions {
    /// The `retro_core_options_v2` view to hand to `set_core_options_v2`. Valid
    /// only while `self` is alive (the arrays it points at are owned by `self`).
    pub fn as_v2(&self) -> retro_core_options_v2 {
        retro_core_options_v2 {
            categories: self.categories.as_ptr() as *mut _,
            definitions: self.definitions.as_ptr() as *mut _,
        }
    }
}

/// One selectable value: its stable id and its human label.
struct Value {
    id: String,
    label: String,
}

fn value(id: impl Into<String>, label: impl Into<String>) -> Value {
    Value { id: id.into(), label: label.into() }
}

/// A two-state (disabled/enabled or linear/lcd) value list.
fn pair(a: (&str, &str), b: (&str, &str)) -> Vec<Value> {
    vec![value(a.0, a.1), value(b.0, b.1)]
}

/// Declarative spec for one option; `desc` is the flat label (frontends without
/// category support), `desc_categorized` the label shown under `category`.
struct OptSpec {
    key: &'static str,
    desc: &'static str,
    desc_categorized: &'static str,
    info: &'static str,
    category: &'static str,
    values: Vec<Value>,
    default: String,
}

/// Push a NUL-terminated copy of `s` into `strings` and return a pointer to it.
/// The pointer stays valid across later pushes: `CString` owns a separate heap
/// buffer, so reallocating the `Vec` moves the handles, not the bytes.
fn push_cstr(strings: &mut Vec<CString>, s: &str) -> *const c_char {
    let c = CString::new(s).expect("option string has no interior NUL");
    let ptr = c.as_ptr();
    strings.push(c);
    ptr
}

/// Build the full option table from the shared enums.
pub fn build() -> OwnedOptions {
    // "Auto" is libretro-only (header sniff); the concrete models come from the
    // shared enum so the list can never drift from what the core can emulate.
    let mut hardware_values = vec![value("auto", "Auto (CGB / DMG by header)")];
    hardware_values
        .extend(HardwareChoice::ALL.into_iter().map(|c| value(c.option_id(), c.label())));

    let palette_values: Vec<Value> = PaletteChoice::ALL
        .into_iter()
        .map(|p| value(p.option_id(), p.label()))
        .collect();

    let specs = vec![
        OptSpec {
            key: KEY_HARDWARE,
            desc: "System > Hardware Model",
            desc_categorized: "Hardware Model",
            info: "Which Game Boy model / silicon revision to emulate. 'Auto' picks CGB unless the ROM header marks it DMG-only. Takes effect on content reload.",
            category: "system_settings",
            values: hardware_values,
            default: "auto".into(),
        },
        OptSpec {
            key: KEY_REAL_BOOT_ROM,
            desc: "System > Use Real Boot ROM",
            desc_categorized: "Use Real Boot ROM",
            info: "Run the real boot ROM from the frontend's system directory (e.g. dmg_boot.bin, cgb_boot.bin) instead of a synthetic post-boot state. Falls back to skip-boot if absent. Takes effect on content reload.",
            category: "system_settings",
            values: pair((OFF, "Disabled"), (ON, "Enabled")),
            default: OFF.into(),
        },
        OptSpec {
            key: KEY_SGB_BORDER,
            desc: "Video > Super Game Boy Border",
            desc_categorized: "Super Game Boy Border",
            info: "On Super Game Boy hardware, output the 256x224 composited frame with the game's border.",
            category: "video_settings",
            values: pair((OFF, "Disabled"), (ON, "Enabled")),
            default: OFF.into(),
        },
        OptSpec {
            key: KEY_DMG_PALETTE,
            desc: "Video > DMG Palette",
            desc_categorized: "DMG Palette",
            info: "Colour palette for original Game Boy (monochrome) output. No effect on Game Boy Color titles, which supply their own colours.",
            category: "video_settings",
            values: palette_values,
            default: PaletteChoice::Grayscale.option_id().into(),
        },
        OptSpec {
            key: KEY_GBC_COLOR_CORRECTION,
            desc: "Video > GBC Colour Correction",
            desc_categorized: "GBC Colour Correction",
            info: "Colour conversion for Game Boy Color output. 'LCD' approximates the real hardware LCD; 'Linear' is the raw RGB555 values.",
            category: "video_settings",
            values: COLOR_CORRECTION.iter().map(|(id, label, _)| value(*id, *label)).collect(),
            default: COLOR_CORRECTION[0].0.into(),
        },
    ];

    let cat_specs = [
        ("system_settings", "System", "Hardware emulation options."),
        ("video_settings", "Video", "Palette and colour options."),
    ];

    let mut strings: Vec<CString> = Vec::new();

    let mut categories: Vec<retro_core_option_v2_category> = cat_specs
        .iter()
        .map(|(k, d, i)| retro_core_option_v2_category {
            key: push_cstr(&mut strings, k),
            desc: push_cstr(&mut strings, d),
            info: push_cstr(&mut strings, i),
        })
        .collect();
    // NULL-key terminator.
    categories.push(unsafe { std::mem::zeroed() });

    let mut definitions: Vec<retro_core_option_v2_definition> = Vec::new();
    for spec in &specs {
        assert!(
            spec.values.len() < MAX_VALUES,
            "option {} has too many values",
            spec.key
        );
        // Zeroed array: the entry after the last value stays {null,null} = the
        // terminator libretro expects.
        let mut values: [retro_core_option_value; MAX_VALUES] = unsafe { std::mem::zeroed() };
        for (slot, v) in values.iter_mut().zip(spec.values.iter()) {
            slot.value = push_cstr(&mut strings, &v.id);
            slot.label = push_cstr(&mut strings, &v.label);
        }
        definitions.push(retro_core_option_v2_definition {
            key: push_cstr(&mut strings, spec.key),
            desc: push_cstr(&mut strings, spec.desc),
            desc_categorized: push_cstr(&mut strings, spec.desc_categorized),
            info: push_cstr(&mut strings, spec.info),
            info_categorized: std::ptr::null(),
            category_key: push_cstr(&mut strings, spec.category),
            values,
            default_value: push_cstr(&mut strings, &spec.default),
        });
    }
    // NULL-key terminator.
    definitions.push(unsafe { std::mem::zeroed() });

    OwnedOptions { _strings: strings, categories, definitions }
}

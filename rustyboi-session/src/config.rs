//! Persistent, host-agnostic session configuration.
//!
//! Serialized with serde to a JSON blob and stored through a [`Storage`] port
//! under a well-known key. Holds only host-agnostic settings: hardware model,
//! DMG palette choice, input remap, rewind tuning, and the fast-forward factor.
//! No host key codes, paths, or window state — those belong to the adapter.

use crate::action::{GbcDmgPalette, LcdEffect, ScalingMode, TextureFilter};
use crate::input::InputMap;
use crate::input_config::InputConfig;
use crate::ports::{Storage, StorageError};
use rustyboi_core_lib::gb::Hardware;
use rustyboi_core_lib::ppu::CgbColorConversion;
use serde::{Deserialize, Serialize};

/// Storage key the config blob lives under.
pub const CONFIG_KEY: &str = "config/session.json";

/// A four-shade DMG palette (RGBA8 per shade, lightest→darkest). Host-agnostic:
/// the adapter maps these to its own pixel format. Presentation-only; does not
/// affect emulation determinism.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmgPalette {
    /// Four shades, lightest (color 0) to darkest (color 3), each `[r,g,b,a]`.
    pub shades: [[u8; 4]; 4],
}

impl Default for DmgPalette {
    /// The classic green DMG LCD ramp.
    fn default() -> Self {
        DmgPalette {
            shades: [
                [0x9B, 0xBC, 0x0F, 0xFF],
                [0x8B, 0xAC, 0x0F, 0xFF],
                [0x30, 0x62, 0x30, 0xFF],
                [0x0F, 0x38, 0x0F, 0xFF],
            ],
        }
    }
}

/// Rewind ring-buffer tuning. `interval_frames` is how often a snapshot is
/// captured; `depth` is how many snapshots are retained (memory bound =
/// `depth * savestate_size`). `enabled` gates capture entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindConfig {
    pub enabled: bool,
    pub interval_frames: u32,
    pub depth: usize,
}

impl Default for RewindConfig {
    fn default() -> Self {
        // ~1s of rewind at 60fps snapshotting every 6 frames (10 steps/sec),
        // 90 snapshots ≈ 9s of history. Conservative default; adapters tune it.
        RewindConfig { enabled: true, interval_frames: 6, depth: 90 }
    }
}

/// The whole persisted config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Emulated hardware model.
    pub hardware: Hardware,
    /// DMG shade palette (monochrome tint, used on DMG/MGB hardware).
    pub dmg_palette: DmgPalette,
    /// CGB colorization for DMG games on CGB/AGB hardware (Auto / a boot-ROM
    /// scheme). `default` (`Auto`) so older blobs still load.
    #[serde(default)]
    pub gbc_dmg_palette: GbcDmgPalette,
    /// Abstract-button remap table.
    pub input_map: InputMap,
    /// Rewind buffer settings.
    pub rewind: RewindConfig,
    /// Fast-forward multiplier (GB frames per presented frame); clamped ≥ 1.
    pub fast_forward_factor: u32,
    /// Master output volume, 0..=100. Scales the session's drained audio copy
    /// only; the core/APU are untouched. `default` so older blobs still load.
    #[serde(default = "default_volume")]
    pub volume: u8,
    /// Frame letterboxing policy. `default` so older blobs still load.
    #[serde(default)]
    pub scaling: ScalingMode,
    /// CGB colour-correction curve (raw RGB555 vs a hardware-LCD approximation).
    /// Applied to the machine at every (re)build. `default` (`Linear`) so older
    /// blobs still load and reproduce the historical output.
    #[serde(default)]
    pub color_correction: CgbColorConversion,
    /// Whether to run a real boot ROM (when one has been supplied via
    /// [`set_boot_rom`](crate::session::Session::set_boot_rom)) instead of the
    /// synthetic post-boot state. `default` (false) so older blobs still load.
    #[serde(default)]
    pub use_real_boot_rom: bool,
    /// Upscale texture filter (presentation-only). `default` (`Nearest`).
    #[serde(default)]
    pub texture_filter: TextureFilter,
    /// LCD post-process effect (presentation-only). `default` (`Off`).
    #[serde(default)]
    pub lcd_effect: LcdEffect,
    /// Integer upscale factor applied to saved/downloaded Game Boy Printer
    /// output (the native image is a tiny 160px wide). `default` (1 = native).
    #[serde(default = "default_printer_scale")]
    pub printer_scale: u8,
    /// On-screen touch control opacity, 0..=100 (percent). `default` (100) is
    /// the full default look.
    #[serde(default = "default_touch_opacity")]
    pub touch_opacity: u8,
    /// Rebindable GB-button bindings + chord hotkeys. `default` so older blobs
    /// still load (they get the default arrows/Z=B/X=A/Enter=Start layout).
    #[serde(default)]
    pub input: InputConfig,
}

fn default_volume() -> u8 {
    100
}

/// Default printer upscale, matching the desktop default window scale
/// (`--scale`, 5×) so a saved print is a comfortable size out of the box.
fn default_printer_scale() -> u8 {
    5
}

fn default_touch_opacity() -> u8 {
    100
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hardware: Hardware::CGB,
            dmg_palette: DmgPalette::default(),
            gbc_dmg_palette: GbcDmgPalette::default(),
            input_map: InputMap::default(),
            rewind: RewindConfig::default(),
            fast_forward_factor: 4,
            volume: 100,
            scaling: ScalingMode::default(),
            color_correction: CgbColorConversion::default(),
            use_real_boot_rom: false,
            texture_filter: TextureFilter::default(),
            lcd_effect: LcdEffect::default(),
            printer_scale: default_printer_scale(),
            touch_opacity: default_touch_opacity(),
            input: InputConfig::default(),
        }
    }
}

impl Config {
    /// Load the config from storage, or return the default if absent /
    /// corrupt. (A corrupt blob is treated as absent so a bad write never
    /// bricks startup; the caller may re-`save` to heal it.)
    pub fn load(storage: &dyn Storage) -> Config {
        storage
            .read(CONFIG_KEY)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Persist the config to storage under [`CONFIG_KEY`].
    pub fn save(&self, storage: &mut dyn Storage) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|e| StorageError::Write(e.to_string()))?;
        storage.write(CONFIG_KEY, &bytes)
    }

    /// Fast-forward factor, clamped to a sane minimum of 1.
    pub fn ff_factor(&self) -> u32 {
        self.fast_forward_factor.max(1)
    }

    /// Master volume as a 0.0..=1.0 multiplier for the drained audio copy.
    pub fn volume_gain(&self) -> f32 {
        self.volume.min(100) as f32 / 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::MemStorage;

    #[test]
    fn config_serde_round_trips_through_storage() {
        let mut storage = MemStorage::new();
        let mut cfg = Config::default();
        cfg.hardware = Hardware::DMG;
        cfg.fast_forward_factor = 8;
        cfg.rewind.depth = 42;
        cfg.volume = 40;
        cfg.scaling = ScalingMode::Stretch;
        cfg.save(&mut storage).unwrap();

        let loaded = Config::load(&storage);
        assert_eq!(loaded, cfg);
        assert_eq!(loaded.ff_factor(), 8);
        assert_eq!(loaded.volume, 40);
        assert_eq!(loaded.scaling, ScalingMode::Stretch);
        assert_eq!(loaded.volume_gain(), 0.4);
    }

    // A config blob written before `volume`/`scaling` existed must still load,
    // defaulting the new fields (serde(default)) rather than failing to default.
    #[test]
    fn config_without_new_fields_defaults_them() {
        let mut storage = MemStorage::new();
        // Build a legacy blob by serializing a default config and dropping the
        // new keys — robust against the exact shapes of the other fields.
        let mut value: serde_json::Value =
            serde_json::to_value(Config::default()).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("volume");
        obj.remove("scaling");
        storage
            .write(CONFIG_KEY, serde_json::to_vec(&value).unwrap().as_slice())
            .unwrap();

        let loaded = Config::load(&storage);
        assert_eq!(loaded.volume, 100, "missing volume defaults to 100");
        assert_eq!(loaded.scaling, ScalingMode::FitAspect, "missing scaling defaults to FitAspect");
    }

    #[test]
    fn missing_config_is_default() {
        let storage = MemStorage::new();
        assert_eq!(Config::load(&storage), Config::default());
    }

    #[test]
    fn corrupt_config_falls_back_to_default() {
        let mut storage = MemStorage::new();
        storage.write(CONFIG_KEY, b"not json").unwrap();
        assert_eq!(Config::load(&storage), Config::default());
    }
}

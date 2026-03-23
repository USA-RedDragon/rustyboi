//! Persistent, host-agnostic session configuration.
//!
//! Serialized with serde to a JSON blob and stored through a [`Storage`] port
//! under a well-known key. Holds only host-agnostic settings: hardware model,
//! DMG palette choice, input remap, rewind tuning, and the fast-forward factor.
//! No host key codes, paths, or window state — those belong to the adapter.

use crate::input::InputMap;
use crate::ports::{Storage, StorageError};
use rustyboi_core_lib::gb::Hardware;
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
    /// DMG shade palette (ignored for CGB output).
    pub dmg_palette: DmgPalette,
    /// Abstract-button remap table.
    pub input_map: InputMap,
    /// Rewind buffer settings.
    pub rewind: RewindConfig,
    /// Fast-forward multiplier (GB frames per presented frame); clamped ≥ 1.
    pub fast_forward_factor: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hardware: Hardware::CGB,
            dmg_palette: DmgPalette::default(),
            input_map: InputMap::default(),
            rewind: RewindConfig::default(),
            fast_forward_factor: 4,
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
        cfg.save(&mut storage).unwrap();

        let loaded = Config::load(&storage);
        assert_eq!(loaded, cfg);
        assert_eq!(loaded.ff_factor(), 8);
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

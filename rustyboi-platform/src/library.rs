//! Persistent state for the Android ROM library.
//!
//! Stores the user-picked SAF tree URI (so the library survives
//! relaunches without re-picking) and a "last played" URI for future
//! auto-resume features. The state lives in `<filesDir>/library.json`
//! — the only thing kept in app-internal storage is *config metadata*;
//! actual save-game data lives next to the user's ROMs via SAF.

#![cfg(target_os = "android")]

use std::fs;
use std::io;
use std::path::PathBuf;

use serde_json::{Value, json};

use rustyboi_egui_lib::actions::LibraryEntry;

/// Maximum number of recently-opened ROMs to retain in the MRU list.
/// Sized so the recents section fits comfortably above the full list
/// without dominating the panel on small screens.
pub const MAX_RECENTS: usize = 10;

#[derive(Clone, Debug, Default)]
pub struct LibraryState {
    pub tree_uri: Option<String>,
    pub last_played_uri: Option<String>,
    /// SAF document URIs of recently opened ROMs, most-recent first.
    /// Capped at [`MAX_RECENTS`] entries.
    pub recents: Vec<String>,
    /// Cached results of the last successful library scan. Hydrated
    /// into the panel on resume so users see their library
    /// immediately, while a fresh scan runs in the background to pick
    /// up newly-added or removed ROMs.
    pub cached_entries: Vec<LibraryEntry>,
}

impl LibraryState {
    fn path() -> PathBuf {
        let mut p = crate::android::data_dir();
        p.push("library.json");
        p
    }

    /// Promote `uri` to the front of the recents list, dropping any
    /// existing duplicate and trimming the tail to [`MAX_RECENTS`].
    /// Also refreshes `last_played_uri` for backwards compatibility
    /// with the auto-resume hook.
    pub fn touch_recent(&mut self, uri: &str) {
        self.recents.retain(|u| u != uri);
        self.recents.insert(0, uri.to_owned());
        if self.recents.len() > MAX_RECENTS {
            self.recents.truncate(MAX_RECENTS);
        }
        self.last_played_uri = Some(uri.to_owned());
    }

    /// Load the persisted library state, returning a default-empty
    /// instance if the file is absent or malformed.
    pub fn load() -> Self {
        let path = Self::path();
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                log::warn!("library: failed to read {}: {e}", path.display());
                return Self::default();
            }
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("library: malformed json: {e}");
                return Self::default();
            }
        };
        let recents = value
            .get("recents")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .take(MAX_RECENTS)
                    .collect()
            })
            .unwrap_or_default();
        let cached_entries = value
            .get("cached_entries")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        let uri = v.get("uri").and_then(Value::as_str)?.to_owned();
                        let name = v.get("name").and_then(Value::as_str)?.to_owned();
                        let rel_path = v
                            .get("rel_path")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        let size_bytes =
                            v.get("size_bytes").and_then(Value::as_u64).unwrap_or(0);
                        Some(LibraryEntry {
                            uri,
                            name,
                            rel_path,
                            size_bytes,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            tree_uri: value
                .get("tree_uri")
                .and_then(Value::as_str)
                .map(String::from),
            last_played_uri: value
                .get("last_played_uri")
                .and_then(Value::as_str)
                .map(String::from),
            recents,
            cached_entries,
        }
    }

    /// Persist the current state. Errors are logged but not propagated:
    /// the library being temporarily un-persisted is not fatal to the
    /// running session.
    pub fn save(&self) {
        let path = Self::path();
        let cached: Vec<Value> = self
            .cached_entries
            .iter()
            .map(|e| {
                json!({
                    "uri": e.uri,
                    "name": e.name,
                    "rel_path": e.rel_path,
                    "size_bytes": e.size_bytes,
                })
            })
            .collect();
        let v = json!({
            "tree_uri": self.tree_uri,
            "last_played_uri": self.last_played_uri,
            "recents": self.recents,
            "cached_entries": cached,
        });
        match serde_json::to_vec(&v) {
            Ok(bytes) => {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Err(e) = fs::write(&path, bytes) {
                    log::warn!("library: failed to write {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("library: failed to serialize: {e}"),
        }
    }
}

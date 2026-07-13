//! Desktop (and Android) concrete implementations of the `rustyboi-session`
//! service ports.
//!
//! The session reaches every host touchpoint through these adapters. This
//! slice ships a real filesystem-backed [`FsStorage`] plus no-op [`NullRumble`]
//! / [`NullWebcam`] stubs; real gilrs rumble, a webcam source, and a link-cable
//! transport arrive in the follow-up. Each stub is a standalone type so
//! swapping in a real implementation is a one-line change at the
//! [`build_ports`] call site.

use std::path::{Path, PathBuf};

use rustyboi_session::ports::{Rumble, Storage, StorageError, Webcam};
use rustyboi_session::Ports;

/// A flat blob store rooted at a base directory. The session's string keys
/// (`state/<romhex>/slot0`, `config/session.json`, …) map straight onto a
/// relative file path under the base; the `/`-separated key becomes nested
/// directories, created on demand.
pub struct FsStorage {
    base: PathBuf,
}

impl FsStorage {
    /// Root the store at `base`, creating it if needed. Falls back to the
    /// current directory if creation fails (never panics: a savestate write
    /// failing later is preferable to refusing to launch).
    pub fn new(base: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&base);
        FsStorage { base }
    }

    /// Resolve a session key to an absolute path under the base. Keys are
    /// trusted (composed by the session, never user text), but we still strip
    /// any leading separators / `..` components so a key can never escape the
    /// base directory.
    fn path_for(&self, key: &str) -> PathBuf {
        let mut path = self.base.clone();
        for comp in key.split('/') {
            if comp.is_empty() || comp == "." || comp == ".." {
                continue;
            }
            path.push(comp);
        }
        path
    }

    /// Recover the original session key from a stored file path (for `list`).
    fn key_for(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.base).ok()?;
        let mut parts = Vec::new();
        for comp in rel.components() {
            parts.push(comp.as_os_str().to_string_lossy().into_owned());
        }
        Some(parts.join("/"))
    }

    /// Walk every file under `dir`, invoking `f` with each.
    fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::walk(&path, f);
            } else {
                f(&path);
            }
        }
    }
}

impl Storage for FsStorage {
    fn read(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path_for(key)).ok()
    }

    fn write(&mut self, key: &str, data: &[u8]) -> Result<(), StorageError> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StorageError::Write(e.to_string()))?;
        }
        std::fs::write(&path, data).map_err(|e| StorageError::Write(e.to_string()))
    }

    fn list(&self, prefix: &str) -> Vec<String> {
        let mut keys = Vec::new();
        Self::walk(&self.base, &mut |path| {
            if let Some(key) = self.key_for(path)
                && key.starts_with(prefix)
            {
                keys.push(key);
            }
        });
        keys
    }
}

/// No-op rumble adapter. Structured as its own type so the follow-up can drop
/// in a real gilrs-driven motor without touching the session wiring.
#[derive(Default)]
pub struct NullRumble;

impl Rumble for NullRumble {
    fn set(&mut self, _on: bool) {}
}

/// No-op webcam adapter: never yields a frame, so the Game Boy Camera holds its
/// last sensor image. A real capture source replaces this in the follow-up.
#[derive(Default)]
pub struct NullWebcam;

impl Webcam for NullWebcam {
    fn grab(&mut self) -> Option<Vec<u8>> {
        None
    }
}

/// The base directory savestates + config live under on desktop.
///
/// Prefers the platform data dir (`$XDG_DATA_HOME`/`~/.local/share`,
/// `~/Library/Application Support`, `%APPDATA%`), falling back to `~/.rustyboi`
/// and finally a `rustyboi` directory in the working dir. No env knobs of our
/// own — only the OS-standard `HOME`/`APPDATA` the platform itself defines.
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
pub fn desktop_save_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            #[cfg(target_os = "macos")]
            {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join("Library/Application Support"))
            }
            #[cfg(target_os = "windows")]
            {
                std::env::var_os("APPDATA").map(PathBuf::from)
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
            }
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("rustyboi")
}

/// Build the concrete port set for this platform, rooted at `base`.
pub fn build_ports(base: PathBuf) -> Ports {
    Ports {
        storage: Box::new(FsStorage::new(base)),
        rumble: Box::new(NullRumble),
        webcam: Box::new(NullWebcam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_lists_keys() {
        let dir = std::env::temp_dir().join(format!("rustyboi_ports_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = FsStorage::new(dir.clone());

        s.write("state/abc/slot0", b"hello").unwrap();
        s.write("state/abc/slot3", b"world").unwrap();
        s.write("config/session.json", b"{}").unwrap();

        assert_eq!(s.read("state/abc/slot0").as_deref(), Some(&b"hello"[..]));
        assert_eq!(s.read("missing"), None);

        let mut slots = s.list("state/abc/slot");
        slots.sort();
        assert_eq!(slots, vec!["state/abc/slot0", "state/abc/slot3"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_cannot_escape_base() {
        let dir = std::env::temp_dir().join(format!("rustyboi_esc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = FsStorage::new(dir.clone());
        // `..` components are dropped, so the path stays inside base.
        assert!(s.path_for("../../etc/passwd").starts_with(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

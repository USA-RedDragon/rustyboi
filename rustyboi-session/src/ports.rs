//! Service-port traits: the abstract I/O boundary every frontend implements.
//!
//! The session owns pure feature logic and reaches the outside world only
//! through these ports. Desktop (`std::fs`), web (IndexedDB), Android (SAF),
//! and libretro (VFS/rumble callbacks) each provide concrete adapters in a
//! later phase; this crate ships only the traits plus in-memory fakes for
//! tests. Nothing here touches the filesystem, a wall clock, or threads, so
//! the whole crate stays WASM-clean.
//!
//! Video and audio are deliberately NOT ports: `Session::run_frame` returns
//! the produced frame and the audio samples as values, and the adapter
//! presents them. Pacing/cadence is the adapter's job too — the session never
//! sleeps or reads a clock.

use core::fmt;

/// Why a [`Storage`] write failed. Deliberately coarse: adapters map their
/// native errors (io::Error, DOMException, JNI throwable, VFS rc) onto these
/// so the session's error surface is host-agnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageError {
    /// The key could not be written (permission, quota, transient failure).
    /// `detail` carries an adapter-provided human string for logging only.
    Write(String),
    /// The backend rejected the key itself (illegal characters, too long).
    BadKey(String),
    /// The backing store is unavailable (not mounted, closed, offline).
    Unavailable,
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Write(d) => write!(f, "storage write failed: {d}"),
            StorageError::BadKey(k) => write!(f, "storage rejected key: {k}"),
            StorageError::Unavailable => write!(f, "storage backend unavailable"),
        }
    }
}

impl std::error::Error for StorageError {}

/// A flat, string-keyed blob store: savestates, config, SRAM, movies. The
/// session composes keys itself (rom id + slot, etc.); the adapter only has to
/// map a `&str` key to a byte blob in whatever namespace it owns.
pub trait Storage {
    /// Read the blob stored at `key`, or `None` if absent.
    fn read(&self, key: &str) -> Option<Vec<u8>>;
    /// Write (create or overwrite) the blob at `key`.
    fn write(&mut self, key: &str, data: &[u8]) -> Result<(), StorageError>;
    /// Every stored key that starts with `prefix` (for slot enumeration, save
    /// listing, etc.). Order is adapter-defined; callers that need ordering
    /// sort themselves.
    fn list(&self, prefix: &str) -> Vec<String>;
}

/// The cartridge rumble motor. `set(true)` energizes it, `set(false)` stops
/// it. The session calls this once per frame with the emulated rumble state so
/// idempotent adapters are fine.
pub trait Rumble {
    fn set(&mut self, on: bool);
}

/// The Game Boy Camera image source. `grab` returns the current sensor frame
/// as 128x112 (== 14336) grayscale bytes, one byte per pixel, row-major, or
/// `None` when no frame is available (the session then leaves the last image
/// in place). The session validates the length before handing it to the core.
pub trait Webcam {
    fn grab(&mut self) -> Option<Vec<u8>>;
}

/// Expected pixel count of a [`Webcam::grab`] frame (128 x 112 grayscale).
pub const WEBCAM_PIXELS: usize = 128 * 112;

/// A byte-oriented transport for link cable / IR / Mobile Adapter traffic.
/// Stub-friendly: a null adapter that never sends and always returns empty is
/// a valid no-link implementation. `send` enqueues outbound bytes; `recv`
/// drains whatever inbound bytes have arrived since the last call.
pub trait NetTransport {
    fn send(&mut self, bytes: &[u8]);
    fn recv(&mut self) -> Vec<u8>;
}

// ---------------------------------------------------------------------------
// In-memory fakes (available to downstream tests + this crate's own tests).
// ---------------------------------------------------------------------------

/// A `HashMap`-backed [`Storage`] for tests and headless use. Never fails a
/// write. Not feature-gated so adapter crates can reuse it in their own tests.
#[derive(Default)]
pub struct MemStorage {
    map: std::collections::HashMap<String, Vec<u8>>,
}

impl MemStorage {
    pub fn new() -> Self {
        Self::default()
    }
    /// Number of stored keys (test convenience).
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Storage for MemStorage {
    fn read(&self, key: &str) -> Option<Vec<u8>> {
        self.map.get(key).cloned()
    }
    fn write(&mut self, key: &str, data: &[u8]) -> Result<(), StorageError> {
        self.map.insert(key.to_string(), data.to_vec());
        Ok(())
    }
    fn list(&self, prefix: &str) -> Vec<String> {
        self.map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }
}

/// A [`Rumble`] fake that records the last commanded state.
#[derive(Default)]
pub struct MemRumble {
    pub on: bool,
    /// Total number of `set` calls (to prove per-frame delivery in tests).
    pub calls: usize,
}

impl Rumble for MemRumble {
    fn set(&mut self, on: bool) {
        self.on = on;
        self.calls += 1;
    }
}

/// A [`Webcam`] fake yielding a fixed image (or `None`).
#[derive(Default)]
pub struct MemWebcam {
    pub next: Option<Vec<u8>>,
}

impl Webcam for MemWebcam {
    fn grab(&mut self) -> Option<Vec<u8>> {
        self.next.clone()
    }
}

/// A loopback [`NetTransport`] fake: sent bytes queue up and are returned by
/// the next `recv` (useful for single-process link smoke tests).
#[derive(Default)]
pub struct MemLoopback {
    queue: std::collections::VecDeque<u8>,
}

impl NetTransport for MemLoopback {
    fn send(&mut self, bytes: &[u8]) {
        self.queue.extend(bytes.iter().copied());
    }
    fn recv(&mut self) -> Vec<u8> {
        self.queue.drain(..).collect()
    }
}

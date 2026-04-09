//! `rustyboi-session` — frontend-agnostic feature logic for the rustyboi
//! Game Boy / Game Boy Color emulator.
//!
//! # Purpose: write each feature once
//!
//! rustyboi has four frontends — desktop (egui + winit + pixels), web (WASM),
//! Android, and libretro. Savestates, slots, rewind, TAS record/replay, cheats,
//! input remapping, fast-forward, and config all belong to *all four*. This
//! crate holds that logic exactly once and abstracts every host touchpoint
//! behind a small set of service-port traits; each frontend becomes a thin
//! adapter that implements the ports and drives [`Session::run_frame`].
//!
//! # WASM-clean by construction
//!
//! Nothing here does filesystem I/O, reads a wall clock, spawns threads, or
//! reads env knobs. Persistence goes through the [`ports::Storage`] port;
//! timestamps are *passed in* by the caller; pacing/cadence is the adapter's
//! job (it calls `run_frame` at the right rate). The crate builds for
//! `wasm32-unknown-unknown` — that build is the proof the layer is web-ready.
//!
//! # Video/audio are outputs, not ports
//!
//! [`Session::run_frame`] returns a [`session::FrameOutput`] carrying the
//! `Frame` and the audio samples generated during it; the adapter presents
//! them. Only *input* side-channels (storage, rumble, webcam, and later
//! net/link) are ports.
//!
//! # Service ports as boxed trait objects (not generics)
//!
//! [`Session`] holds its ports as `Box<dyn Trait>` ([`session::Ports`]) rather
//! than being generic (`Session<S: Storage, R: Rumble, …>`). Rationale:
//!
//! - **One concrete type.** A non-generic `Session` is trivial to store behind
//!   a C ABI (libretro `retro_*` callbacks, Android JNI handles) and in a WASM
//!   `#[wasm_bindgen]` struct. A monomorphized `Session<S,R,W>` leaks its type
//!   parameters into every signature the FFI wrapper must name.
//! - **Ports are cold-path.** `Storage`/`Rumble`/`Webcam` are hit at most once
//!   per frame (rumble/webcam) or on user actions (storage). Dynamic dispatch
//!   cost is irrelevant next to emulating 70k dots; the hot path is
//!   `GB::run_until_frame`, which is a direct static call.
//! - **Adapter-friendly.** Frontends mix and match adapters at runtime
//!   (headless null-rumble here, real motor there) without the session's type
//!   changing.
//!
//! The value types that DO benefit from monomorphization ([`input::InputMap`],
//! [`cheats::CheatSet`], [`rewind::RewindBuffer`]) are plain concrete structs.

pub mod action;
pub mod apply;
mod audio;
pub mod cheats;
pub mod config;
pub mod input;
pub mod overlay;
pub mod ports;
pub mod rewind;
pub mod session;
pub mod tas;

#[cfg(target_os = "android")]
pub use action::LibraryEntry;
pub use action::{
    ActionKind, CommandDescriptor, FileData, HardwareChoice, KeyBind, MenuCategory, PaletteChoice,
    SessionUiState, UiAction, COMMANDS,
};
pub use apply::{ActionOutcome, PlatformRequest};
pub use config::Config;
pub use input::{AbstractInput, GbButton, InputMap};
pub use overlay::{OverlayButton, OverlayRect, OverlayShape, TouchLayout};
pub use ports::{NetTransport, Rumble, Storage, StorageError, Webcam};
pub use session::{
    FrameOutput, Ports, RunMode, Session, SessionError, SlotMeta, GB_SIZE, QUICK_SLOT, SGB_SIZE,
};

// Re-export the core types adapters need so a frontend can depend on just this
// crate for the common path.
pub use rustyboi_core_lib::gb::{Frame, Hardware, GB};
pub use rustyboi_core_lib::input::ButtonState;
pub use rustyboi_core_lib::movie::{self, sha256, Movie};

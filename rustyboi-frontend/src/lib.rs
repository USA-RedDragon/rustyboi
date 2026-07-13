//! `rustyboi-frontend` — the portable app + UI + renderer for rustyboi.
//!
//! This crate owns everything a frontend needs that is *not* platform-specific:
//!
//! - [`renderer::Renderer`] — a raw-wgpu renderer that uploads the emulator
//!   frame as an RGBA texture (160x144, or 256x224 for the SGB border
//!   composite), draws it letterboxed into the region below the egui menu bar
//!   with a scaling pipeline, and composites the egui UI on top via
//!   `egui-wgpu`. It replaces both `pixels` and the old custom `game_renderer`.
//! - [`ui_host::UiHost`] — the egui host (context + `egui-winit` input bridge +
//!   the `rustyboi-egui` `Gui`), producing the paint jobs the renderer draws.
//! - [`app::App`] — the portable application: owns the `Session`, the UI, the
//!   palette, and the run/pause/error bookkeeping, driving emulation and UI. It
//!   surfaces OS-only work as [`app::PlatformRequest`]s for the platform. The
//!   DMG presentation palettes are the session's
//!   [`PaletteChoice`](rustyboi_session::PaletteChoice) (one source of truth).
//!
//! The `rustyboi-platform` crate is a thin adapter around this: it creates the
//! winit window + wgpu surface/device, pumps winit events, owns audio, file
//! dialogs, worker threads, and the Android JNI entry — then hands off to
//! [`app::App`]. Web (wgpu-WebGL2) and Android adapters reuse this crate.

pub mod app;
pub mod contract;
pub mod keymap;
pub mod renderer;
pub mod ui_host;

pub use app::{App, FrameStep, PlatformRequest, ResolvedAction};
pub use contract::{drive_action, Frontend, PauseHint};
pub use renderer::{GameFrame, PhysicalRect, Renderer, SourceSize};
pub use ui_host::{UiFrame, UiHost};

/// The egui event vocabulary the platform needs to synthesize input events it
/// injects each frame (Android IME Text/Backspace, which winit 0.29 drops). The
/// platform never depends on egui directly — it names these through here.
pub mod egui_events {
    pub use egui::{Event, Key, Modifiers};
}

// Re-export the egui action + UI-state types the platform must name to build
// the `SessionUiState` snapshot and match `GuiAction`s it resolves (file loads).
pub use rustyboi_egui_lib::actions::{self, FileData, GuiAction, HardwareChoice, PaletteChoice, SessionUiState};

// The Android JNI glue (in `rustyboi-platform`) installs handlers and drives the
// ROM-library panel through these; re-export them so the platform depends only
// on the frontend, never on egui directly.
#[cfg(target_os = "android")]
pub use rustyboi_egui_lib::android_bridge;
#[cfg(target_os = "ios")]
pub use rustyboi_egui_lib::ios_bridge;
#[cfg(target_os = "android")]
pub use rustyboi_egui_lib::library;

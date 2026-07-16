//! The UI-action contract, re-exported from the toolkit-agnostic
//! `rustyboi-session` crate.
//!
//! The canonical definitions now live in `rustyboi_session::action`; this module
//! re-exports them (and keeps the historical `GuiAction` name as an alias for
//! [`UiAction`]) so the egui widgets and the frontends that name `actions::…`
//! keep compiling. The egui widgets emit these actions and never implement their
//! behavior — `Session::apply` does.

pub use rustyboi_session::action::{
    ActionKind, CommandDescriptor, FileData, GbcDmgPalette, HardwareChoice, HardwareFamily,
    KeyBind, LcdEffect, MenuCategory, PaletteChoice, ScalingMode, SessionUiState, TextureFilter,
    UiAction, COMMANDS, FAST_FORWARD_SPEEDS, PRINTER_SCALES,
};
pub use rustyboi_session::CgbColorConversion;

#[cfg(target_os = "android")]
pub use rustyboi_session::action::LibraryEntry;

/// Historical name for the canonical [`UiAction`]. Kept so existing egui/host
/// code that matches on `GuiAction` compiles unchanged.
pub type GuiAction = UiAction;

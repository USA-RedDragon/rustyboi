#![warn(clippy::all)]
// `#[unsafe(no_mangle)] fn android_main(...)` requires unsafe; gate the
// lint so non-Android targets still get it. `deny` (not `forbid`) so the
// rewind worker's single, audited `unsafe impl Send for SendGb` (justified in
// rewind_worker.rs — transported `GB` clones carry no audio sink) can opt out
// locally; everywhere else unsafe is still a hard error.
#![cfg_attr(not(target_os = "android"), deny(unsafe_code))]

#[cfg(target_os = "android")]
pub mod android;
mod audio;
mod config;
mod display;
mod framework;
mod game_renderer;
#[cfg(target_os = "android")]
pub mod library;
mod ports;
// Native-desktop background workers. Not built for wasm (no threads) or Android
// (no print/rewind-offload sink on the mobile path).
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod png_worker;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod rewind_worker;
mod run;

pub use crate::run::run;

#[cfg(target_os = "android")]
pub use crate::run::run_android;

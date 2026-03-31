#![warn(clippy::all)]
// `#[unsafe(no_mangle)] fn android_main(...)` requires unsafe; gate the
// lint so non-Android targets still forbid it. No unsafe elsewhere: the rewind
// worker moves cloned `GB`s to its thread safely because `GB: Send` (its audio
// sink is `Box<dyn AudioOutput + Send>`).
#![cfg_attr(not(target_os = "android"), forbid(unsafe_code))]

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

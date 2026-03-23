#![warn(clippy::all)]
// `#[unsafe(no_mangle)] fn android_main(...)` requires unsafe; gate the
// forbid attribute so non-Android targets still get the lint.
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
mod run;

pub use crate::run::run;

#[cfg(target_os = "android")]
pub use crate::run::run_android;

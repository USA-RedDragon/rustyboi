#![warn(clippy::all)]
#![cfg_attr(not(target_os = "windows"), forbid(unsafe_code))]
#![cfg_attr(target_os = "windows", deny(unsafe_code))]
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
mod audio;
#[cfg(not(target_os = "android"))]
mod config;
#[cfg(not(target_os = "android"))]
mod display;
#[cfg(not(target_os = "android"))]
mod error;
#[cfg(not(target_os = "android"))]
mod ports;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod png_worker;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod rewind_worker;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod fetch_worker;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod no_intro_cache;
#[cfg(not(target_os = "android"))]
mod run;

#[cfg(not(target_os = "android"))]
fn main() -> Result<(), error::PlatformError> {
    run::run()
}

// On Android the entry point is `android_main` in lib.rs; this bin target is
// not built into the APK, but `cargo build -p rustyboi-platform` still
// compiles every target, so we need a no-op main for the host linker.
#[cfg(target_os = "android")]
fn main() {}

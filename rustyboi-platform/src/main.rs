#![warn(clippy::all)]
#![forbid(unsafe_code)]

#[cfg(not(target_os = "android"))]
mod audio;
#[cfg(not(target_os = "android"))]
mod config;
#[cfg(not(target_os = "android"))]
mod display;
#[cfg(not(target_os = "android"))]
mod framework;
#[cfg(not(target_os = "android"))]
mod game_renderer;
#[cfg(not(target_os = "android"))]
mod ports;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod png_worker;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod rewind_worker;
#[cfg(not(target_os = "android"))]
mod run;

#[cfg(not(target_os = "android"))]
fn main() -> Result<(), pixels::Error> {
    run::run()
}

// On Android the entry point is `android_main` in lib.rs; this bin target is
// not built into the APK, but `cargo build -p rustyboi-platform` still
// compiles every target, so we need a no-op main for the host linker.
#[cfg(target_os = "android")]
fn main() {}

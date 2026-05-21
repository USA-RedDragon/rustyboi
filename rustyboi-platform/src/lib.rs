#![warn(clippy::all)]
// `#[unsafe(no_mangle)] fn android_main(...)` requires unsafe; gate the
// lint so non-Android targets still forbid it. No unsafe elsewhere: the rewind
// worker moves cloned `GB`s to its thread safely because `GB: Send` (its audio
// sink is `Box<dyn AudioOutput + Send>`), and wgpu surface creation goes through
// the safe `Arc<Window>` handle path.
#![cfg_attr(not(any(target_os = "android", target_os = "ios")), forbid(unsafe_code))]

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "ios")]
pub mod ios;
mod audio;
mod config;
mod display;
mod error;
#[cfg(target_os = "android")]
pub mod library;
mod ports;
// Native-desktop background workers. Not built for wasm (no threads) or Android
// (no print/rewind-offload sink on the mobile path).
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod png_worker;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod rewind_worker;
// The cheat-DB HTTP fetch worker runs on both desktop and Android (both link
// ureq); only wasm (no threads, uses the browser `fetch`) opts out.
#[cfg(not(target_arch = "wasm32"))]
mod fetch_worker;
#[cfg(not(target_arch = "wasm32"))]
mod no_intro_cache;
mod run;

pub use crate::run::run;

#[cfg(target_os = "android")]
pub use crate::run::run_android;

#[cfg(target_os = "ios")]
pub use crate::run::run_ios;

/// iOS binary entry point. The Xcode app's `main.m` calls this symbol; it hands
/// off to the shared winit GUI loop (winit's UIKit backend takes over the app
/// lifecycle from `EventLoop::run_app`). Returns a C `int` exit status.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn rustyboi_ios_main() -> core::ffi::c_int {
    match run_ios() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

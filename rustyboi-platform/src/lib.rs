#![warn(clippy::all)]
#![forbid(unsafe_code)]

mod app;
mod audio;
mod config;
mod display;
mod framework;
mod input;
mod renderer;
mod run;

pub use crate::run::run;
pub use crate::renderer::WgpuRenderer;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

// Export the run function for WASM
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn start() {
    wasm_bindgen_futures::spawn_local(run::run());
}

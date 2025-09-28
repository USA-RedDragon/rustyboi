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

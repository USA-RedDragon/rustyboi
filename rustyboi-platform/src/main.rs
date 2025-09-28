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

use pollster;

fn main() {
    pollster::block_on(run::run());
}

#![warn(clippy::all)]
#![forbid(unsafe_code)]

mod audio;
mod config;
mod display;
mod framework;
mod run;

fn main() -> Result<(), pixels::Error> {
    run::run()
}

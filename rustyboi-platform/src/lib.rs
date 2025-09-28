#![warn(clippy::all)]
#![forbid(unsafe_code)]

mod audio;
mod config;
mod display;
mod framework;
mod run;

pub use crate::run::run;

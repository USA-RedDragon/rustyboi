//! Shared guts of the Game Boy test tooling.
//!
//! The suite runner (`rustyboi-test-runner`) and the dev bins (`sweep`,
//! `harness`, `movie`, `bench`) all live in this crate. Anything more than one
//! of them needs belongs here so it is compiled once and type-checked once:
//! before this lib existed the bins pulled each other's modules in via
//! `#[path = "shared/*.rs"]` + a module-wide `#![allow(dead_code)]`, which
//! recompiled the same code per bin and — because every item looked used
//! somewhere — hid genuinely dead code.
//!
//! Module split:
//!   * [`app`] — the suite runner binary's own logic (`src/main.rs` is a stub).
//!   * [`imaging`], [`masher`], [`script`] — the former `src/bin/shared/`.
//!   * `expectation`/`frame`/`report` stay crate-private: they are the runner's
//!     internals. [`runner`] is public but exposes only the handful of items the
//!     dev bins genuinely share (e.g. [`runner::bios_filename`]).

mod expectation;
mod frame;
mod report;

pub mod app;
pub mod imaging;
pub mod masher;
pub mod runner;
pub mod script;

//! Cartridge subsystem: the `Cartridge` container (ROM/RAM buffers, battery
//! persistence, header decode, RTC) plus the per-board mappers alongside it.
//!
//! The container lives in [`cartridge`]; header decode lives in [`header`]. The
//! mapper behavior is being migrated out into per-board modules behind a
//! `Mapper` enum (enum-dispatched, no `dyn`, so serde savestates and the hot
//! read/write path are preserved).

// The container file is deliberately `cartridge/cartridge.rs` (the `Cartridge`
// struct lives alongside the per-board mapper modules), so the inner module
// shares the subsystem's name.
#[allow(clippy::module_inception)]
mod cartridge;
mod header;

pub use cartridge::{Cartridge, UnlMapper, Vf001State};
pub use header::{find_logo_in_boot_rom, CgbSupport, Destination};
pub(crate) use cartridge::RtcTickKind;

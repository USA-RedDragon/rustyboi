//! MBC3 board: register state + address->bank math.

use super::*;
use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc3 {
    pub ram_enabled: bool,
    pub rom_bank_low: u8, // 7 bits (8 on MBC30)
    pub ram_bank: u8,     // 0x00-0x03 RAM (0x07 on MBC30), 0x08-0x0C RTC
    pub has_ram: bool,
    pub timer: bool,
}

impl Mbc3 {
    /// MBC30 wires an extra ROM- and RAM-bank bit (>2 MB ROM / >32 KB RAM).
    pub fn is_mbc30(&self, g: Geom) -> bool {
        g.rom_banks > 128 || g.ram_banks > 4
    }
}

impl Banking for Mbc3 {
    fn rom_bankn(&self, g: Geom) -> usize {
        let mask = if self.is_mbc30(g) { 0xFF } else { 0x7F };
        let bank = (self.rom_bank_low & mask) as usize;
        if bank == 0 { 1 } else { bank % g.rom_banks }
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        let mask = if self.is_mbc30(g) { 0x07 } else { 0x03 };
        (self.ram_bank & mask) as usize % g.ram_banks.max(1)
    }
}


// --- container-side board logic -----------------------------------------

impl Cartridge {
    /// MBC30: the large-capacity MBC3 variant (used by e.g. Japanese Pokémon
    /// Crystal) that wires 8 ROM-bank bits (256 banks / 4MB, vs MBC3's 7 bits /
    /// 2MB) and 3 RAM-bank bits (8 banks / 64KB, vs 2 bits / 32KB). There is no
    /// header flag for it; a cart wired for MBC3 addressing cannot exceed 2MB
    /// ROM / 32KB RAM, so exceeding either limit identifies the MBC30 per
    /// Pan Docs.
    pub(super) fn is_mbc30(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::MBC3 { .. })
            && (self.rom_banks > 128 || self.ram_banks > 4)
    }
}

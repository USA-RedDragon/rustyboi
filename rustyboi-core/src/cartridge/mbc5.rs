//! MBC5 board: register state + address->bank math.

use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc5 {
    pub ram_enabled: bool,
    pub regs: Mbc5State,
    pub has_ram: bool,
    pub rumble: bool,
    #[serde(skip, default)]
    pub rumble_motor: bool,
}

impl Banking for Mbc5 {
    fn rom_bankn(&self, g: Geom) -> usize {
        let bank =
            (self.regs.rom_bank_low as usize) | ((self.regs.rom_bank_high as usize & 0x01) << 8);
        bank % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        (self.regs.ram_bank & 0x0F) as usize % g.ram_banks.max(1)
    }
}


// --- state ---------------------------------------------------------------

/// MBC5 bank registers.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct Mbc5State {
    pub(super) rom_bank_low: u8,  // Lower 8 bits of ROM bank (0x2000-0x2FFF)
    pub(super) rom_bank_high: u8, // Upper 1 bit of ROM bank (0x3000-0x3FFF) - only bit 0 used
    pub(super) ram_bank: u8,      // RAM bank select (0x4000-0x5FFF) - 4 bits used (0x00-0x0F)
}

impl Default for Mbc5State {
    fn default() -> Self {
        Self { rom_bank_low: 1, rom_bank_high: 0, ram_bank: 0 }
    }
}


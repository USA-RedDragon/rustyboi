//! MBC2 board: register state + address->bank math.

use super::*;
use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc2 {
    pub ram_enabled: bool,
    pub rom_bank_low: u8, // low 4 bits
}

impl Banking for Mbc2 {
    fn rom_bankn(&self, g: Geom) -> usize {
        let bank = (self.rom_bank_low & 0x0F) as usize;
        if bank == 0 { 1 } else { bank % g.rom_banks }
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0 // MBC2 has built-in RAM, no banking
    }
}


// --- container-side board logic -----------------------------------------

impl Cartridge {
    /// Write a byte to MBC2 RAM and save file simultaneously (if battery-backed)
    pub(super) fn write_mbc2_ram_byte(&mut self, offset: usize, value: u8) -> Result<(), io::Error> {
        if !self.mbc2_ram.is_empty() {
            // Write to MBC2 RAM buffer (offset is already wrapped by caller)
            self.mbc2_ram[offset] = value & 0x0F; // Only 4 bits valid

            // Also write to save file if we have one open
            if let Some(ref mut file) = self.save_file {
                file.seek(SeekFrom::Start(offset as u64))?;
                file.write_all(&[self.mbc2_ram[offset]])?;
                file.flush()?; // Ensure immediate write
            }
        }
        Ok(())
    }
}

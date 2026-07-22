//! HUC1 board: register state + address->bank math.

use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct HuC1 {
    pub state: HuC1State,
}

impl Banking for HuC1 {
    fn rom_bankn(&self, g: Geom) -> usize {
        (self.state.rom_bank as usize) % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        (self.state.ram_bank as usize) % g.ram_banks.max(1)
    }
}


// --- state ---------------------------------------------------------------

/// HuC-1 state. RAM is always enabled; the 0x0000-0x1FFF register only selects
/// whether A000-BFFF accesses RAM (default) or the IR transceiver (low nibble
/// == 0xE).
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct HuC1State {
    pub(super) ir_mode: bool,
    /// 6-bit ROM bank register; bank 0 is selectable at 0x4000-0x7FFF (no
    /// MBC1-style zero remap; the largest HuC-1 cart is 1MB = 64 banks).
    pub(super) rom_bank: u8,
    /// RAM bank register, "at least 2 bits" (Pan Docs); stored raw and reduced
    /// modulo the cart's bank count like HuC-3.
    pub(super) ram_bank: u8,
    /// IR LED output latch (bit 0 of writes in IR mode). No IR transport is
    /// modeled: reads always see "no light" (0xC0), the documented idle.
    pub(super) ir_led: bool,
}

impl Default for HuC1State {
    fn default() -> Self {
        Self { ir_mode: false, rom_bank: 1, ram_bank: 0, ir_led: false }
    }
}


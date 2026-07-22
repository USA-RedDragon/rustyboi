//! Bandai TAMA5 ($FD): the licensed Tamagotchi board. Three chips — the TAMA5
//! gate array (banking + the host interface), the TAMA6 MCU (RTC and the
//! save-RAM controller) and the TAMA7 mask ROM.
//!
//! The board is unusual in two ways. Its whole register file is reached through
//! the cart-RAM window $A000-$BFFF rather than $0000-$7FFF (so the generic
//! ROM-area write dispatch never sees it), and every transfer is a NIBBLE: the
//! host writes a register index to an ODD address and a 4-bit payload to an
//! EVEN one, so a byte of save RAM is assembled from two nibble registers and
//! read back as two nibble halves.
//!
//! Protocol per mGBA `src/gb/mbc/tama5.c` + `include/mgba/internal/gb/memory.h`:
//!
//! WRITE $A000-$BFFF
//!   odd  addr -> `reg = value` (the register selector, full byte)
//!   even addr -> `value &= 0xF`; if `reg < MAX` then `registers[reg] = value`
//!                and the write is ACTED ON by `reg`:
//!                  BANK_LO/BANK_HI  latch the 8-bit ROM bank
//!                  WRITE_LO/WRITE_HI/ADDR_HI  stage data only
//!                  ADDR_LO          executes the command in `ADDR_HI >> 1`
//!                                   (0 = RAM write, 1 = arm a RAM read,
//!                                    2 = TAMA6 command)
//! READ $A000-$BFFF
//!   odd  addr -> $FF
//!   even addr -> $F1 when the selected register is ACTIVE (the readiness flag
//!                the game polls before every transfer), otherwise the low
//!                nibble (READ_LO) or high nibble (READ_HI) of the byte the
//!                armed command produced, in bits 3-0 with bits 7-4 driven high.
//!
//! The TAMA6 RTC is stubbed: its command space is modeled as a nibble-addressed
//! register file that round-trips writes back to reads, which is enough for the
//! games' boot handshake. A faithful TAMA6 (BCD clock, alarm, the leap-year
//! register) would need a hardware bench to pin down and no game behaviour we
//! can observe today distinguishes it.

use super::mapper::{Banking, Geom};
use super::*;
use serde::{Deserialize, Serialize};

// Register indices (mGBA `enum GBTAMA5Register`). $2/$3/$8/$9/$B/$E/$F are
// unused by the games and unmapped here.
const BANK_LO: u8 = 0x0;
const BANK_HI: u8 = 0x1;
const WRITE_LO: u8 = 0x4;
const WRITE_HI: u8 = 0x5;
const ADDR_HI: u8 = 0x6;
const ADDR_LO: u8 = 0x7;
/// One past the last LATCHABLE register: an even-address write with a selector
/// at or above this only sets nothing (the higher indices are read ports).
const MAX: u8 = 0x8;
const ACTIVE: u8 = 0xA;
const READ_LO: u8 = 0xC;
const READ_HI: u8 = 0xD;

/// Save RAM the board exposes: the address is assembled from `ADDR_LO` (4 bits)
/// plus bit 0 of `ADDR_HI`, so exactly 32 bytes are addressable. The header
/// declares RAM size $00, so this array is allocated from the type byte (as
/// MBC7's EEPROM is) rather than from the RAM-size byte.
pub(super) const TAMA5_RAM_SIZE: usize = 0x20;

impl Cartridge {
    /// $A000-$BFFF write: the register-file protocol above.
    pub(super) fn tama5_write(&mut self, addr: u16, value: u8) {
        let mut st = match &self.mapper {
            Mapper::Tama5(m) => m.state,
            _ => return,
        };
        // Odd addresses select the register; nothing else happens.
        if addr & 1 != 0 {
            st.reg = value;
            self.tama5_store(st);
            return;
        }
        let reg = st.reg;
        if reg >= MAX {
            return;
        }
        st.registers[reg as usize] = value & 0x0F;
        let ram_addr = st.ram_addr();
        let out = (st.registers[WRITE_HI as usize] << 4) | st.registers[WRITE_LO as usize];
        // A write to ADDR_LO completes a transfer; every other register only
        // stages state (the bank registers are read back by the Banking impl).
        if reg == ADDR_LO {
            match st.registers[ADDR_HI as usize] >> 1 {
                // RAM write. Routed through write_ram_byte so it reaches the
                // battery sidecar like any other save.
                0x0 => {
                    self.tama5_store(st);
                    if let Some(offset) = self.tama5_ram_offset(ram_addr) {
                        let _ = self.write_ram_byte(offset, out);
                    }
                    return;
                }
                // RAM read: arms the READ_LO/READ_HI ports, no side effect here.
                0x1 => {}
                // TAMA6 command space (RTC and friends), stubbed as a
                // nibble-addressed register file.
                _ => st.rtc[ram_addr as usize & (TAMA5_RTC_REGS - 1)] = out,
            }
        }
        self.tama5_store(st);
    }

    /// $A000-$BFFF read. Immutable: no TAMA5 read has a side effect.
    pub(super) fn tama5_read(&self, addr: u16, st: &Tama5State) -> u8 {
        if addr & 1 != 0 {
            return 0xFF;
        }
        if st.reg == ACTIVE {
            // Readiness flag. The MCU is modeled as always ready, so this is
            // the constant the games spin on before each transfer.
            return 0xF1;
        }
        let mut value = 0xF0;
        if st.reg == READ_LO || st.reg == READ_HI {
            match st.registers[ADDR_HI as usize] >> 1 {
                0x1 => {
                    value = self
                        .tama5_ram_offset(st.ram_addr())
                        .map_or(0xFF, |offset| self.ram_data[offset])
                }
                0x2 | 0x4 => value = st.rtc[st.ram_addr() as usize & (TAMA5_RTC_REGS - 1)],
                _ => {}
            }
            if st.reg == READ_HI {
                value >>= 4;
            }
        }
        // Only bits 3-0 are driven; the upper nibble floats high.
        value | 0xF0
    }

    /// Byte index into `ram_data` for a 5-bit TAMA5 RAM address. `None` when
    /// the cart carries no RAM array (a mis-headered $FD dump).
    #[inline]
    fn tama5_ram_offset(&self, ram_addr: u8) -> Option<usize> {
        if self.ram_data.is_empty() {
            return None;
        }
        Some(ram_addr as usize % self.ram_data.len())
    }

    #[inline]
    fn tama5_store(&mut self, st: Tama5State) {
        if let Mapper::Tama5(m) = &mut self.mapper {
            m.state = st;
        }
    }
}

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Tama5 {
    pub state: Tama5State,
}

impl Banking for Tama5 {
    fn rom_bankn(&self, g: Geom) -> usize {
        // 8-bit bank from the two nibble registers. Bank 0 is selectable at
        // $4000 (there is no MBC1-style bank-0 remap).
        let bank = self.state.registers[BANK_LO as usize] as usize
            | ((self.state.registers[BANK_HI as usize] as usize) << 4);
        bank % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        // The 32-byte save RAM is addressed by the register file, never banked.
        0
    }
}

// --- state ---------------------------------------------------------------

/// TAMA6 command-space size (stub): the same 32 nibble-addressed slots the RAM
/// path uses. A power of two so the index mask below is exact.
const TAMA5_RTC_REGS: usize = 0x20;

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct Tama5State {
    /// The register selector, latched by an odd-address write.
    pub(super) reg: u8,
    /// The eight latchable 4-bit registers (see the index constants).
    pub(super) registers: [u8; MAX as usize],
    /// TAMA6 command-space stub; writes round-trip back to reads.
    pub(super) rtc: [u8; TAMA5_RTC_REGS],
}

impl Tama5State {
    /// 5-bit save-RAM / command address: `ADDR_LO` plus bit 0 of `ADDR_HI`
    /// (whose bits 3-1 are the command instead).
    #[inline]
    fn ram_addr(&self) -> u8 {
        ((self.registers[ADDR_HI as usize] << 4) & 0x10) | self.registers[ADDR_LO as usize]
    }
}

impl Default for Tama5State {
    fn default() -> Self {
        // BANK_LO powers up at 1 so the switchable window shows bank 1 before
        // the game's first bank write, matching every other board here (the
        // real latch's power-on value is unknown and the games always program
        // it before reading $4000).
        let mut registers = [0u8; MAX as usize];
        registers[BANK_LO as usize] = 1;
        Self { reg: 0, registers, rtc: [0; TAMA5_RTC_REGS] }
    }
}

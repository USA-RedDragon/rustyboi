//! MBC6 ($20): the one-off Nintendo board behind "Net de Get - Minigame @ 100"
//! (Konami, CGB-BMVJ-JPN), the Mobile-Adapter cart that downloads minigames
//! into an on-board flash chip.
//!
//! It is the only licensed mapper whose switchable areas are not one window
//! each. Both the ROM area and the cart-RAM area are split in half, and each
//! half has its own independent bank register, so the effective bank size is
//! halved: 8 KiB ROM banks instead of 16 KiB, 4 KiB RAM banks instead of 8 KiB
//! (Pan Docs "MBC6"):
//!
//!   $0000-$3FFF  ROM bank 0 (fixed, the first 16 KiB)
//!   $4000-$5FFF  ROM/flash bank A, 8 KiB, banks $00-$7F
//!   $6000-$7FFF  ROM/flash bank B, 8 KiB, banks $00-$7F
//!   $A000-$AFFF  SRAM bank A, 4 KiB, banks $00-$07
//!   $B000-$BFFF  SRAM bank B, 4 KiB, banks $00-$07
//!
//! Either ROM half can be switched from the mask ROM to the 8 Mbit Macronix
//! MX29F008TC flash chip, which is then addressed through that same window --
//! reads AND writes, so the standard AMD-style flash command sequences are
//! issued straight into $4000-$7FFF.
//!
//! Registers are decoded at 1 KiB granularity (mGBA `_GBMBC6`, which switches
//! on `address >> 10`):
//!
//!   $0000-$03FF  RAM enable ($0A on / $00 off)
//!   $0400-$07FF  SRAM bank A number
//!   $0800-$0BFF  SRAM bank B number
//!   $0C00-$0FFF  flash enable (bit 0 -> the flash chip's /CE)
//!   $1000-$13FF  flash write enable (bit 0 -> /WP: sector 0 + hidden region)
//!   $2000-$27FF  ROM/flash bank A number
//!   $2800-$2FFF  ROM/flash bank A select ($00 = ROM, $08 = flash)
//!   $3000-$37FF  ROM/flash bank B number
//!   $3800-$3FFF  ROM/flash bank B select ($00 = ROM, $08 = flash)
//!
//! The flash array is allocated lazily: an empty `Vec` IS a fully erased chip
//! ($FF everywhere), which is the state every dump of this cart ships in, so
//! the 1 MiB never enters a savestate unless something actually programmed it.
//! Programming can only come from a Mobile Adapter download session, which
//! needs a network backend rustyboi deliberately does not have (see
//! `mobile.rs`), so in practice it stays erased. It rides in savestates but NOT
//! in the .sav sidecar, which carries the battery-backed SRAM only.

use super::mapper::{Banking, Geom};
use super::*;
use serde::{Deserialize, Serialize};

/// Both switchable ROM windows are 8 KiB.
const ROM_PAGE: usize = 0x2000;
/// Both cart-RAM windows are 4 KiB.
const RAM_HALF: usize = 0x1000;
/// MX29F008TC: 8 Mbit = 1 MiB, in eight 128 KiB sectors.
const FLASH_SIZE: usize = 0x10_0000;
const FLASH_SECTOR: usize = 0x2_0000;
/// The chip's extra 256-byte region, reachable only via the $77/$77 command.
const HIDDEN_SIZE: usize = 0x100;
/// Flash command prologue addresses. The chip decodes only A0-A14 for these,
/// so the window's bank number contributes bits 13-14 (Pan Docs writes them as
/// `2:5555` / `1:4AAA` for window A, i.e. flash offsets $5555 and $2AAA).
const CMD_MASK: usize = 0x7FFF;
const CMD_A: usize = 0x5555;
const CMD_B: usize = 0x2AAA;
/// JEDEC ID the chip reports in ID mode: Macronix, MX29F008TC.
const JEDEC_MANUFACTURER: u8 = 0xC2;
const JEDEC_DEVICE: u8 = 0x81;
/// One flash program burst is a 128-byte aligned page.
const PROGRAM_PAGE: usize = 128;

// --- board struct + banking -------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc6 {
    pub state: Mbc6State,
}

impl Banking for Mbc6 {
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    /// MBC6 has no 16 KiB switchable bank; both halves are served by
    /// `Cartridge::mbc6_rom_read`, which never consults this. It is reported
    /// as the 16 KiB bank window A currently sits in so the generic consumers
    /// (Game Genie patching, the debugger's bank readout) name a real region.
    fn rom_bankn(&self, g: Geom) -> usize {
        (self.state.rom_bank[0] as usize / 2) % g.rom_banks.max(1)
    }
    /// Likewise: the 8 KiB-bank equivalent of RAM window A. The real accesses
    /// go through `mbc6_ram_offset`.
    fn ram_bank(&self, _g: Geom) -> usize {
        self.state.ram_bank[0] as usize / 2
    }
}

/// Which half of a split window an access lands in: 0 = A ($4000/$A000),
/// 1 = B ($6000/$B000). The ROM halves are 8 KiB apart, the RAM halves 4 KiB.
#[inline]
fn half(addr: u16) -> usize {
    if addr >= EXTERNAL_RAM_START {
        usize::from(addr & 0x1000 != 0)
    } else {
        usize::from(addr & 0x2000 != 0)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc6State {
    pub ram_enabled: bool,
    /// SRAM bank per 4 KiB half.
    pub ram_bank: [u8; 2],
    /// ROM/flash bank per 8 KiB half.
    pub rom_bank: [u8; 2],
    /// Whether each half shows the flash chip instead of the mask ROM.
    pub from_flash: [bool; 2],
    /// $0C00 register: the flash chip's /CE. With it low the chip is off the
    /// bus entirely, so a flash-mapped window reads open.
    pub flash_enabled: bool,
    /// $1000 register: /WP. Sector 0 and the hidden region can only be erased
    /// or programmed while this is set; it powers up clear.
    pub flash_write_enabled: bool,
    pub flash: Flash,
}

impl Default for Mbc6State {
    fn default() -> Self {
        Self {
            ram_enabled: false,
            ram_bank: [0, 1],
            // Power-on map (mGBA `GBMBCReset`): the two halves come up on the
            // 8 KiB banks that continue the fixed area linearly, so an
            // untouched cart sees $0000-$7FFF as the first 32 KiB of ROM.
            rom_bank: [2, 3],
            from_flash: [false; 2],
            flash_enabled: false,
            flash_write_enabled: false,
            flash: Flash::default(),
        }
    }
}

// --- the cartridge-side access paths ----------------------------------------

impl Cartridge {
    /// $4000-$7FFF read: the half's own bank, from the mask ROM or the flash.
    pub(super) fn mbc6_rom_read(&self, st: &Mbc6State, addr: u16) -> u8 {
        let half = half(addr);
        let offset = (st.rom_bank[half] as usize * ROM_PAGE) + (addr as usize & (ROM_PAGE - 1));
        if st.from_flash[half] {
            if !st.flash_enabled {
                return 0xFF;
            }
            return st.flash.read(offset % FLASH_SIZE, addr);
        }
        // Out-of-range banks wrap into the image, like every other board here.
        let pages = (self.rom_data.len() / ROM_PAGE).max(1);
        self.rom_data.get(offset % (pages * ROM_PAGE)).copied().unwrap_or(0xFF)
    }

    /// $0000-$7FFF write: the register file below $4000, flash commands above.
    pub(super) fn mbc6_write(&mut self, addr: u16, value: u8) {
        let Mapper::Mbc6(m) = &mut self.mapper else { return };
        let st = &mut m.state;
        match addr >> 10 {
            0x0 => st.ram_enabled = (value & 0x0F) == 0x0A,
            0x1 => st.ram_bank[0] = value,
            0x2 => st.ram_bank[1] = value,
            0x3 => st.flash_enabled = value & 1 != 0,
            0x4 => st.flash_write_enabled = value & 1 != 0,
            0x8 | 0x9 => st.rom_bank[0] = value,
            0xA | 0xB => st.from_flash[0] = value & 0x08 != 0,
            0xC | 0xD => st.rom_bank[1] = value,
            0xE | 0xF => st.from_flash[1] = value & 0x08 != 0,
            // $4000-$7FFF: the flash chip's own command/data bus, live only
            // while the half is flash-mapped and the chip is selected.
            0x10..=0x1F => {
                let half = half(addr);
                if st.flash_enabled && st.from_flash[half] {
                    let offset = (st.rom_bank[half] as usize * ROM_PAGE)
                        + (addr as usize & (ROM_PAGE - 1));
                    let wp = st.flash_write_enabled;
                    st.flash.write(offset % FLASH_SIZE, value, wp);
                }
            }
            // $1400-$1FFF decodes to nothing on this board.
            _ => {}
        }
    }

    /// $A000-$BFFF read. Each half has its own 4 KiB bank register.
    pub(super) fn mbc6_ram_read(&self, st: &Mbc6State, addr: u16) -> u8 {
        if !st.ram_enabled {
            return 0xFF;
        }
        self.mbc6_ram_offset(st, addr).map_or(0xFF, |o| self.ram_data[o])
    }

    /// $A000-$BFFF write.
    pub(super) fn mbc6_ram_write(&mut self, addr: u16, value: u8) {
        let Mapper::Mbc6(m) = &self.mapper else { return };
        if !m.state.ram_enabled {
            return;
        }
        let Some(offset) = self.mbc6_ram_offset(&m.state, addr) else { return };
        let _ = self.write_ram_byte(offset, value);
    }

    /// Byte index into `ram_data` for a cart-RAM access. `None` when the cart
    /// carries no RAM array (a mis-headered $20 dump).
    #[inline]
    fn mbc6_ram_offset(&self, st: &Mbc6State, addr: u16) -> Option<usize> {
        if self.ram_data.is_empty() {
            return None;
        }
        let half = half(addr);
        Some(
            ((st.ram_bank[half] as usize * RAM_HALF) + (addr as usize & (RAM_HALF - 1)))
                % self.ram_data.len(),
        )
    }
}

// --- the MX29F008TC flash chip ----------------------------------------------

/// What the flash presents on a read. Every mode is left by writing $F0.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum FlashMode {
    /// Normal array read.
    #[default]
    Array,
    /// JEDEC ID at the two lowest addresses of the window.
    Id,
    /// Post-erase/post-program status byte.
    Status,
    /// The 256-byte hidden region, mirrored across the window.
    Hidden,
    /// Collecting a 128-byte program page (the array still reads back).
    Program,
    /// As `Program`, targeting the hidden region.
    ProgramHidden,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Flash {
    /// The 1 MiB array. Empty means "never programmed", which is identical to
    /// erased, so a pristine cart carries no payload in its savestates.
    data: Vec<u8>,
    /// The 256-byte hidden region, allocated on the same terms.
    hidden: Vec<u8>,
    /// How much of the AA/55 prologue has landed (0-2, then 3-5 for the second
    /// prologue of a two-cycle command).
    step: u8,
    /// First byte of a two-cycle command ($60/$77/$80).
    pending: u8,
    mode: FlashMode,
    /// Program-page buffer: (array offset, byte), flushed by the commit write.
    buffer: Vec<(u32, u8)>,
    /// Sector-0 lockout latched by the $20 command. Non-volatile on hardware,
    /// so it rides in the savestate with everything else.
    sector0_locked: bool,
}

impl Flash {
    fn read(&self, offset: usize, addr: u16) -> u8 {
        match self.mode {
            FlashMode::Id => {
                if addr & 1 == 0 {
                    JEDEC_MANUFACTURER
                } else {
                    JEDEC_DEVICE
                }
            }
            // Bit 7 set = the operation finished; bit 1 reports the sector-0
            // lockout the $20 command latched. Operations complete instantly
            // here, so the timeout bit (mask $10) never appears.
            FlashMode::Status => 0x80 | (u8::from(self.sector0_locked) << 1),
            FlashMode::Hidden => {
                self.hidden.get(offset & (HIDDEN_SIZE - 1)).copied().unwrap_or(0xFF)
            }
            _ => self.data.get(offset).copied().unwrap_or(0xFF),
        }
    }

    fn write(&mut self, offset: usize, value: u8, wp: bool) {
        // $F0 leaves every mode, including a half-collected program page.
        if value == 0xF0 {
            self.reset();
            return;
        }
        if matches!(self.mode, FlashMode::Program | FlashMode::ProgramHidden) {
            self.program(offset, value, wp);
            return;
        }
        match self.step {
            0 | 3 if offset & CMD_MASK == CMD_A && value == 0xAA => self.step += 1,
            1 | 4 if offset & CMD_MASK == CMD_B && value == 0x55 => self.step += 1,
            2 if offset & CMD_MASK == CMD_A => {
                self.step = 0;
                match value {
                    0x90 => self.mode = FlashMode::Id,
                    0xA0 => {
                        self.mode = FlashMode::Program;
                        self.buffer.clear();
                    }
                    // Two-cycle commands: a second AA/55 prologue follows.
                    0x60 | 0x77 | 0x80 => {
                        self.pending = value;
                        self.step = 3;
                    }
                    _ => {}
                }
            }
            // The closing byte of a two-cycle command. Sector erase takes its
            // sector from THIS address; the rest ignore it.
            5 => {
                self.step = 0;
                match (self.pending, value) {
                    (0x80, 0x30) => self.erase(offset & !(FLASH_SECTOR - 1), FLASH_SECTOR, wp),
                    (0x80, 0x10) => self.erase_chip(wp),
                    (0x60, 0x04) => self.erase_hidden(wp),
                    (0x60, 0xE0) => {
                        self.mode = FlashMode::ProgramHidden;
                        self.buffer.clear();
                    }
                    (0x60, 0x40) => {
                        if wp {
                            self.sector0_locked = false;
                        }
                        self.mode = FlashMode::Status;
                    }
                    (0x60, 0x20) => {
                        if wp {
                            self.sector0_locked = true;
                        }
                        self.mode = FlashMode::Status;
                    }
                    (0x77, 0x77) => self.mode = FlashMode::Hidden,
                    _ => {}
                }
            }
            _ => self.step = 0,
        }
    }

    /// Program mode collects a 128-byte page; re-writing an address already in
    /// the page is the commit cycle that burns it (Pan Docs "Programming must
    /// be done by ... writing out 128 bytes (aligned), then writing any value
    /// (except $F0) to the final address again to commit the write").
    fn program(&mut self, offset: usize, value: u8, wp: bool) {
        if self.buffer.iter().any(|&(a, _)| a as usize == offset) {
            let hidden = self.mode == FlashMode::ProgramHidden;
            // Both sector 0 and the hidden region are behind /WP.
            if if hidden { !wp } else { self.locked(offset, wp) } {
                self.mode = FlashMode::Status;
                self.buffer.clear();
                return;
            }
            let page = std::mem::take(&mut self.buffer);
            let (array, size) = if hidden {
                (&mut self.hidden, HIDDEN_SIZE)
            } else {
                (&mut self.data, FLASH_SIZE)
            };
            if array.is_empty() {
                array.resize(size, 0xFF);
            }
            for (a, v) in page {
                // Flash programming can only clear bits; setting them back
                // takes an erase.
                let slot = &mut array[a as usize & (size - 1)];
                *slot &= v;
            }
            self.mode = FlashMode::Status;
        } else if self.buffer.len() < PROGRAM_PAGE {
            self.buffer.push((offset as u32, value));
        }
    }

    /// Whether an offset is inside sector 0 while sector 0 is protected --
    /// either by /WP being low or by the $20 command's non-volatile latch.
    fn locked(&self, offset: usize, wp: bool) -> bool {
        offset < FLASH_SECTOR && (!wp || self.sector0_locked)
    }

    fn erase(&mut self, start: usize, len: usize, wp: bool) {
        self.mode = FlashMode::Status;
        if self.locked(start, wp) {
            return;
        }
        self.erase_range(start, len);
    }

    /// Chip erase clears all eight sectors, skipping sector 0 when it is
    /// protected. The hidden region is never touched by it.
    fn erase_chip(&mut self, wp: bool) {
        let start = if !wp || self.sector0_locked { FLASH_SECTOR } else { 0 };
        self.erase_range(start, FLASH_SIZE - start);
        self.mode = FlashMode::Status;
    }

    fn erase_range(&mut self, start: usize, len: usize) {
        if self.data.is_empty() {
            return;
        }
        self.data[start..start + len].fill(0xFF);
    }

    fn erase_hidden(&mut self, wp: bool) {
        self.mode = FlashMode::Status;
        if wp {
            self.hidden.clear();
        }
    }

    fn reset(&mut self) {
        self.mode = FlashMode::Array;
        self.step = 0;
        self.pending = 0;
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flash() -> Flash {
        Flash::default()
    }

    /// The AA/55/90 prologue puts the chip in ID mode; $F0 leaves it.
    #[test]
    fn jedec_id_round_trip() {
        let mut f = flash();
        assert_eq!(f.read(0, 0x4000), 0xFF, "erased flash reads $FF");
        f.write(CMD_A, 0xAA, false);
        f.write(CMD_B, 0x55, false);
        f.write(CMD_A, 0x90, false);
        assert_eq!(f.read(0, 0x4000), JEDEC_MANUFACTURER);
        assert_eq!(f.read(1, 0x4001), JEDEC_DEVICE);
        f.write(0, 0xF0, false);
        assert_eq!(f.read(0, 0x4000), 0xFF);
    }

    /// A broken prologue must not be mistaken for a command.
    #[test]
    fn bad_prologue_is_inert() {
        let mut f = flash();
        f.write(CMD_A, 0xAA, false);
        f.write(CMD_A, 0x55, false); // wrong address
        f.write(CMD_A, 0x90, false);
        assert_eq!(f.read(0, 0x4000), 0xFF, "no ID mode without the exact prologue");
    }

    /// Program mode burns the buffered page on the commit write, and only
    /// clears bits; sector 0 stays locked out while /WP is low.
    #[test]
    fn program_page_and_sector0_protection() {
        let mut f = flash();
        let addr = FLASH_SECTOR + 0x40; // sector 1, never write-protected
        f.write(CMD_A, 0xAA, false);
        f.write(CMD_B, 0x55, false);
        f.write(CMD_A, 0xA0, false);
        f.write(addr, 0x3C, false);
        f.write(addr, 0x00, false); // commit
        assert_eq!(f.read(addr, 0x4000), 0x80, "operation-complete status");
        f.write(0, 0xF0, false);
        assert_eq!(f.read(addr, 0x4000), 0x3C);

        // Same sequence into sector 0 with /WP low is refused.
        f.write(CMD_A, 0xAA, false);
        f.write(CMD_B, 0x55, false);
        f.write(CMD_A, 0xA0, false);
        f.write(0x100, 0x0F, false);
        f.write(0x100, 0x00, false);
        f.write(0, 0xF0, false);
        assert_eq!(f.read(0x100, 0x4000), 0xFF, "sector 0 is protected while /WP is low");

        // With /WP high it takes.
        f.write(CMD_A, 0xAA, true);
        f.write(CMD_B, 0x55, true);
        f.write(CMD_A, 0xA0, true);
        f.write(0x100, 0x0F, true);
        f.write(0x100, 0x00, true);
        f.write(0, 0xF0, true);
        assert_eq!(f.read(0x100, 0x4000), 0x0F);
    }

    /// Sector erase restores $FF, and only inside the addressed 128 KiB.
    #[test]
    fn sector_erase_is_scoped() {
        let mut f = flash();
        let (a, b) = (FLASH_SECTOR + 0x20, 2 * FLASH_SECTOR + 0x20);
        for addr in [a, b] {
            f.write(CMD_A, 0xAA, false);
            f.write(CMD_B, 0x55, false);
            f.write(CMD_A, 0xA0, false);
            f.write(addr, 0x5A, false);
            f.write(addr, 0x00, false);
            f.write(0, 0xF0, false);
        }
        assert_eq!(f.read(a, 0x4000), 0x5A);
        assert_eq!(f.read(b, 0x4000), 0x5A);
        // Erase the sector holding `a` only.
        f.write(CMD_A, 0xAA, false);
        f.write(CMD_B, 0x55, false);
        f.write(CMD_A, 0x80, false);
        f.write(CMD_A, 0xAA, false);
        f.write(CMD_B, 0x55, false);
        f.write(a, 0x30, false);
        f.write(0, 0xF0, false);
        assert_eq!(f.read(a, 0x4000), 0xFF);
        assert_eq!(f.read(b, 0x4000), 0x5A, "the neighbouring sector is untouched");
    }
}

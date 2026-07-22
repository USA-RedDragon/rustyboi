//! The enum-of-state mapper model. Each board keeps its own volatile registers
//! in its own struct; [`Mapper`] is the sum of them, dispatched by `match` (no
//! `dyn` — so the bincode savestate derives cleanly and the hot read/write path
//! stays monomorphized). The [`Banking`] trait is the address→bank math, moved
//! verbatim off the old `Cartridge::get_rom_bank*/get_ram_bank` match sites.
//!
//! The battery/persistent substrate (ROM/RAM buffers, RTC, save I/O, header
//! identity) stays on `Cartridge`; the peripheral engines (RTC register file,
//! camera capture, HuC-3 command MCU) are reached through the container so this
//! module carries only banking + register state.

use super::{
    CameraState, HuC1State, HuC3State, M161State, Mbc5State, Mbc7State, NtState, RocketState,
    SachenState, UnlMapper,
};
use super::{
    HUC1_RAM_BATTERY, HUC3, MBC1, MBC1_RAM, MBC1_RAM_BATTERY, MBC2, MBC2_BATTERY, MBC3, MBC3_RAM,
    MBC3_RAM_BATTERY, MBC3_TIMER_BATTERY, MBC3_TIMER_RAM_BATTERY, MBC5, MBC5_RAM, MBC5_RAM_BATTERY,
    MBC5_RUMBLE, MBC5_RUMBLE_RAM, MBC5_RUMBLE_RAM_BATTERY, MBC7_SENSOR_RUMBLE_RAM_BATTERY,
    POCKET_CAMERA, ROM_RAM, ROM_RAM_BATTERY,
};
use serde::{Deserialize, Serialize};

/// ROM/RAM geometry the bank math needs, passed by value (Copy).
#[derive(Clone, Copy)]
pub(super) struct Geom {
    pub rom_banks: usize,
    pub ram_banks: usize,
}

/// Address→bank math for a board. `rom_bank0` is the 16 KiB bank mapped at
/// $0000-$3FFF (normally 0), `rom_bankn` the switchable bank at $4000-$7FFF,
/// `ram_bank` the external-RAM bank index. Every result is already reduced
/// modulo the available bank count, exactly as the old match arms did.
pub(super) trait Banking {
    fn rom_bank0(&self, g: Geom) -> usize;
    fn rom_bankn(&self, g: Geom) -> usize;
    fn ram_bank(&self, g: Geom) -> usize;
}

// --- MBC1 ------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc1 {
    pub ram_enabled: bool,
    pub rom_bank_low: u8, // 5 bits (0x01-0x1F), zero remapped to one at write time
    pub bank2: u8,        // 2 bits (BANK2): RAM bank or ROM bank bits 5-6
    pub mode: u8,         // 0 = ROM banking, 1 = RAM banking
    pub has_ram: bool,
    pub multicart: bool,
}

impl Banking for Mbc1 {
    fn rom_bankn(&self, g: Geom) -> usize {
        // (BANK2 << shift) | BANK1, regardless of mode. BANK1's zero->one remap
        // is applied at write time, so banks 0x20/0x40/0x60 stay inaccessible.
        let bank = if self.multicart {
            ((self.bank2 as usize) << 4) | (self.rom_bank_low as usize & 0x0F)
        } else {
            ((self.bank2 as usize) << 5) | (self.rom_bank_low as usize)
        };
        bank % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        if self.mode == 1 {
            let bank = if self.multicart {
                (self.bank2 as usize) << 4
            } else {
                (self.bank2 as usize) << 5
            };
            bank % g.rom_banks
        } else {
            0
        }
    }
    fn ram_bank(&self, g: Geom) -> usize {
        if self.mode == 1 {
            (self.bank2 as usize) % g.ram_banks.max(1)
        } else {
            0
        }
    }
}

// --- MBC2 ------------------------------------------------------------------

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

// --- MBC3 ------------------------------------------------------------------

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

// --- MBC5 ------------------------------------------------------------------

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

// --- MBC7 ------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc7 {
    pub ram_enabled: bool,
    pub state: Mbc7State,
}

impl Banking for Mbc7 {
    fn rom_bankn(&self, g: Geom) -> usize {
        (self.state.rom_bank as usize) % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0 // no banked RAM (serial EEPROM)
    }
}

// --- HuC-1 -----------------------------------------------------------------

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

// --- HuC-3 -----------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct HuC3 {
    pub state: HuC3State,
}

impl Banking for HuC3 {
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

// --- Pocket Camera ---------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Camera {
    pub ram_enabled: bool,
    pub state: CameraState,
}

impl Banking for Camera {
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

// --- No MBC ----------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct NoMbc {
    pub battery: bool,
}

impl Banking for NoMbc {
    fn rom_bankn(&self, _g: Geom) -> usize {
        1 // bankless cart always maps bank 1 to the upper area
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

// --- Unlicensed: Wisdom Tree ----------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct WisdomTree {
    pub bank: u8, // 6-bit whole-32KB latch
}

impl Banking for WisdomTree {
    fn rom_bankn(&self, g: Geom) -> usize {
        (self.bank as usize * 2 + 1) % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        (self.bank as usize * 2) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

// --- Unlicensed: Rocket Games ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Rocket {
    pub state: RocketState,
}

impl Banking for Rocket {
    fn rom_bankn(&self, g: Geom) -> usize {
        (((self.state.outer as usize & 0x0F) << 4) | (self.state.rom_bank as usize)) % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        ((self.state.outer as usize & 0x0F) << 4) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

// --- Unlicensed: Sachen ----------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Sachen {
    pub state: SachenState,
    pub mmc2: bool,
}

impl Banking for Sachen {
    fn rom_bankn(&self, g: Geom) -> usize {
        (((self.state.bank & !self.state.mask) | (self.state.base & self.state.mask)) as usize)
            % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        ((self.state.base & self.state.mask) as usize) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

// --- Unlicensed: NT/Makon "old" -------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct NtOld {
    pub state: NtState,
    pub v2: bool,
    pub ram_enabled: bool, // MBC3-style RAM gate ($0A to $0000-$1FFF)
}

impl Banking for NtOld {
    fn rom_bankn(&self, g: Geom) -> usize {
        // The $5003 bit-swap is combinational on the bank lines; the $5002
        // bank-count mask and the $5001 base (32KB units) apply after it.
        let mut bank = self.state.bank;
        if self.state.swapped {
            let table = if self.v2 { &super::NT_OLD2_REORDER } else { &super::NT_OLD1_REORDER };
            bank = super::Cartridge::reorder_bits(bank, table);
        }
        if self.state.bank_mask != 0 {
            bank &= self.state.bank_mask;
        }
        (bank as usize + self.state.base as usize * 2) % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        (self.state.base as usize * 2) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

// --- Unlicensed: M161 ------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct M161 {
    pub state: M161State,
}

impl Banking for M161 {
    fn rom_bankn(&self, g: Geom) -> usize {
        ((self.state.bank as usize) | 1) & (g.rom_banks - 1)
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        (self.state.bank as usize) & (g.rom_banks - 2)
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

// --- Unlicensed: Vast Fame VF001 (electrically MBC5) ----------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Vf001 {
    pub ram_enabled: bool,
    pub regs: Mbc5State,
}

impl Banking for Vf001 {
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

// --- The sum ---------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) enum Mapper {
    NoMbc(NoMbc),
    Mbc1(Mbc1),
    Mbc2(Mbc2),
    Mbc3(Mbc3),
    Mbc5(Mbc5),
    Mbc7(Mbc7),
    HuC1(HuC1),
    HuC3(HuC3),
    Camera(Camera),
    WisdomTree(WisdomTree),
    Rocket(Rocket),
    Sachen(Sachen),
    NtOld(NtOld),
    M161(M161),
    Vf001(Vf001),
}

impl Banking for Mapper {
    fn rom_bank0(&self, g: Geom) -> usize {
        self.dispatch(|m| m.rom_bank0(g))
    }
    fn rom_bankn(&self, g: Geom) -> usize {
        self.dispatch(|m| m.rom_bankn(g))
    }
    fn ram_bank(&self, g: Geom) -> usize {
        self.dispatch(|m| m.ram_bank(g))
    }
}

impl Mapper {
    /// Build the power-on mapper from the header type byte + detected unlicensed
    /// family. Mirrors `Cartridge::decode_cartridge_type` (content-detected
    /// boards override the header byte), but yields a live board with power-on
    /// registers rather than a descriptor. Vast Fame VF001 gets its own variant
    /// (electrically MBC5) so the protection intercepts have somewhere to hang.
    pub(super) fn from_header(unl: UnlMapper, cartridge_type: u8, multicart: bool) -> Mapper {
        let mbc1 = |has_ram| {
            Mapper::Mbc1(Mbc1 {
                ram_enabled: false,
                rom_bank_low: 1,
                bank2: 0,
                mode: 0,
                has_ram,
                multicart,
            })
        };
        match unl {
            UnlMapper::None => {}
            UnlMapper::WisdomTree => return Mapper::WisdomTree(WisdomTree { bank: 0 }),
            UnlMapper::Rocket => return Mapper::Rocket(Rocket { state: RocketState::default() }),
            UnlMapper::SachenMmc1 => {
                return Mapper::Sachen(Sachen { state: SachenState::default(), mmc2: false })
            }
            UnlMapper::SachenMmc2 => {
                return Mapper::Sachen(Sachen { state: SachenState::default(), mmc2: true })
            }
            UnlMapper::NtOld1 => {
                return Mapper::NtOld(NtOld { state: NtState::default(), v2: false, ram_enabled: false })
            }
            UnlMapper::NtOld2 => {
                return Mapper::NtOld(NtOld { state: NtState::default(), v2: true, ram_enabled: false })
            }
            UnlMapper::ForceMbc1 => return mbc1(false),
            UnlMapper::M161 => return Mapper::M161(M161 { state: M161State::default() }),
            UnlMapper::Vf001(_) => {
                return Mapper::Vf001(Vf001 { ram_enabled: false, regs: Mbc5State::default() })
            }
        }
        let mbc3 = |has_ram, timer| {
            Mapper::Mbc3(Mbc3 { ram_enabled: false, rom_bank_low: 1, ram_bank: 0, has_ram, timer })
        };
        let mbc5 = |has_ram, rumble| {
            Mapper::Mbc5(Mbc5 {
                ram_enabled: false,
                regs: Mbc5State::default(),
                has_ram,
                rumble,
                rumble_motor: false,
            })
        };
        match cartridge_type {
            MBC1 => mbc1(false),
            MBC1_RAM | MBC1_RAM_BATTERY => mbc1(true),
            MBC2 | MBC2_BATTERY => Mapper::Mbc2(Mbc2 { ram_enabled: false, rom_bank_low: 1 }),
            MBC3_TIMER_BATTERY => mbc3(false, true),
            MBC3_TIMER_RAM_BATTERY => mbc3(true, true),
            MBC3 => mbc3(false, false),
            MBC3_RAM | MBC3_RAM_BATTERY => mbc3(true, false),
            MBC5 => mbc5(false, false),
            MBC5_RAM | MBC5_RAM_BATTERY => mbc5(true, false),
            MBC5_RUMBLE => mbc5(false, true),
            MBC5_RUMBLE_RAM | MBC5_RUMBLE_RAM_BATTERY => mbc5(true, true),
            MBC7_SENSOR_RUMBLE_RAM_BATTERY => {
                Mapper::Mbc7(Mbc7 { ram_enabled: false, state: Mbc7State::default() })
            }
            HUC1_RAM_BATTERY => Mapper::HuC1(HuC1 { state: HuC1State::default() }),
            HUC3 => Mapper::HuC3(HuC3 { state: HuC3State::default() }),
            POCKET_CAMERA => Mapper::Camera(Camera { ram_enabled: false, state: CameraState::default() }),
            ROM_RAM => Mapper::NoMbc(NoMbc { battery: false }),
            ROM_RAM_BATTERY => Mapper::NoMbc(NoMbc { battery: true }),
            // Unknown/unimplemented types fall through to a bankless board.
            _ => Mapper::NoMbc(NoMbc { battery: false }),
        }
    }

    /// Apply `f` to the active board's `Banking` impl. One match, reused by all
    /// three bank queries so the enum arms live in exactly one place.
    #[inline]
    fn dispatch<R>(&self, f: impl FnOnce(&dyn Banking) -> R) -> R {
        match self {
            Mapper::NoMbc(m) => f(m),
            Mapper::Mbc1(m) => f(m),
            Mapper::Mbc2(m) => f(m),
            Mapper::Mbc3(m) => f(m),
            Mapper::Mbc5(m) => f(m),
            Mapper::Mbc7(m) => f(m),
            Mapper::HuC1(m) => f(m),
            Mapper::HuC3(m) => f(m),
            Mapper::Camera(m) => f(m),
            Mapper::WisdomTree(m) => f(m),
            Mapper::Rocket(m) => f(m),
            Mapper::Sachen(m) => f(m),
            Mapper::NtOld(m) => f(m),
            Mapper::M161(m) => f(m),
            Mapper::Vf001(m) => f(m),
        }
    }
}

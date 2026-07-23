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

use super::camera::CameraState;
use super::huc1::HuC1State;
use super::huc3::HuC3State;
use super::mbc5::Mbc5State;
use super::mbc7::Mbc7State;
use super::tama5::Tama5State;
use super::unlicensed::{M161State, NtState, RocketState, SachenState};
use super::UnlMapper;
use super::header::is_documented_type;
use super::{
    HUC1_RAM_BATTERY, HUC3, MBC1, MBC1_RAM, MBC1_RAM_BATTERY, MBC2, MBC2_BATTERY, MBC3, MBC3_RAM,
    MBC3_RAM_BATTERY, MBC3_TIMER_BATTERY, MBC3_TIMER_RAM_BATTERY, MBC5, MBC5_RAM, MBC5_RAM_BATTERY,
    MBC5_RUMBLE, MBC5_RUMBLE_RAM, MBC5_RUMBLE_RAM_BATTERY, MBC7_SENSOR_RUMBLE_RAM_BATTERY,
    POCKET_CAMERA, ROM_ONLY, ROM_RAM, ROM_RAM_BATTERY, TAMA5,
};
use serde::{Deserialize, Serialize};
use super::{
    camera::Camera, huc1::HuC1, huc3::HuC3, mbc1::Mbc1, mbc2::Mbc2, mbc3::Mbc3, mbc5::Mbc5,
    mbc7::Mbc7, nombc::NoMbc, tama5::Tama5,
    unlicensed::{
        Bbd, Ggb81, Hitek, LiCheng, M161, NtOld, Rocket, Sachen, Sintax, Vf001, WisdomTree,
    },
};

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



// --- MBC2 ------------------------------------------------------------------



// --- MBC3 ------------------------------------------------------------------




// --- MBC5 ------------------------------------------------------------------



// --- MBC7 ------------------------------------------------------------------



// --- HuC-1 -----------------------------------------------------------------



// --- HuC-3 -----------------------------------------------------------------



// --- Pocket Camera ---------------------------------------------------------



// --- No MBC ----------------------------------------------------------------



// --- Unlicensed: Wisdom Tree ----------------------------------------------



// --- Unlicensed: Rocket Games ---------------------------------------------



// --- Unlicensed: Sachen ----------------------------------------------------



// --- Unlicensed: NT/Makon "old" -------------------------------------------



// --- Unlicensed: M161 ------------------------------------------------------



// --- Unlicensed: Vast Fame VF001 (electrically MBC5) ----------------------



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
    LiCheng(LiCheng),
    Bbd(Bbd),
    Ggb81(Ggb81),
    Sintax(Sintax),
    Hitek(Hitek),
    Tama5(Tama5),
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
    /// `rom_banks`/`ram_banks` are the decoded geometry, needed only to infer
    /// MBC1 for an oversized bankless header (see the `ROM_ONLY` arm below).
    pub(super) fn from_header(
        unl: &UnlMapper,
        cartridge_type: u8,
        multicart: bool,
        rom_banks: usize,
        ram_banks: usize,
    ) -> Mapper {
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
            // Header-liar re-linked to MBC5 banking (POCKETMON bootleg): plain
            // MBC5+RAM registers, no protection intercepts.
            UnlMapper::ForceMbc5 => {
                return Mapper::Mbc5(Mbc5 {
                    ram_enabled: false,
                    regs: Mbc5State::default(),
                    has_ram: true,
                    rumble: false,
                    rumble_motor: false,
                })
            }
            UnlMapper::M161 => return Mapper::M161(M161 { state: M161State::default() }),
            UnlMapper::Vf001(_) => {
                return Mapper::Vf001(Vf001 { ram_enabled: false, regs: Mbc5State::default() })
            }
            // LiCheng is electrically MBC5+RAM; the $2101-$2FFF write ignore is
            // in the write dispatch, so the board itself is plain MBC5 registers.
            UnlMapper::LiCheng => {
                return Mapper::LiCheng(LiCheng { ram_enabled: false, regs: Mbc5State::default() })
            }
            // BBD is electrically MBC5+RAM; the scramble state rides in the
            // UnlMapper::Bbd payload, so the board is plain MBC5 registers.
            UnlMapper::Bbd(_) => {
                return Mapper::Bbd(Bbd { ram_enabled: false, regs: Mbc5State::default() })
            }
            // GGB81 wears a truthful MBC5-family header, but override to its own
            // board so the read reorder / $2001 mode write have somewhere to
            // hang; the bank registers are plain MBC5.
            UnlMapper::Ggb81(_) => {
                return Mapper::Ggb81(Ggb81 { ram_enabled: false, regs: Mbc5State::default() })
            }
            // Sintax is electrically MBC5+RAM; the scramble state rides in the
            // UnlMapper::Sintax payload, so the board is plain MBC5 registers.
            UnlMapper::Sintax(_) => {
                return Mapper::Sintax(Sintax { ram_enabled: false, regs: Mbc5State::default() })
            }
            // HITEK is electrically MBC5+RAM; the scramble state rides in the
            // UnlMapper::Hitek payload, so the board is plain MBC5 registers.
            UnlMapper::Hitek(_) => {
                return Mapper::Hitek(Hitek { ram_enabled: false, regs: Mbc5State::default() })
            }
            // General VF001 is electrically MBC5; reuse the already-wired Vf001
            // board (plain MBC5 bank/RAM registers). The protection lives in the
            // UnlMapper::Vf001Gen payload's register file, applied by the
            // $6000-$7FFF write and ROM-read intercepts — the board just banks.
            UnlMapper::Vf001Gen(_) => {
                return Mapper::Vf001(Vf001 { ram_enabled: false, regs: Mbc5State::default() })
            }
            // Zook Z is electrically MBC5 too (its `rst $28` is a bare
            // `ld ($2000),a`); the challenge-response port only adds a second
            // way to drive the same bank register, so reuse the Vf001 board.
            UnlMapper::Vf001Zook(_) => {
                return Mapper::Vf001(Vf001 { ram_enabled: false, regs: Mbc5State::default() })
            }
            // The 8 KiB dual-window board keeps its two page registers in the
            // UnlMapper::Vf8k payload and serves $4000-$7FFF from the read
            // intercept, so the board here only needs the plain MBC5 RAM-enable
            // / RAM-bank / rumble registers. Falling through to the header type
            // gives exactly that (the one known cart declares $1C truthfully).
            UnlMapper::Vf8k(_) => {}
            // "New GB Color" HK PCB: a stock MBC5 board (its header type is
            // truthful) whose only addition is a read-side protection window,
            // applied in the ROM-read intercept. Fall through to the header so
            // it gets the real `Mapper::Mbc5` the intercept reads its bank
            // register from.
            UnlMapper::NewGbHk => {}
            // PKJD is electrically MBC3+TIMER+RAM+BATTERY (its header type $10 is
            // truthful); the D/E/F protection state lives in the
            // UnlMapper::PokeJadeDia payload and is applied by the $4000-$5FFF /
            // $A000-$BFFF intercepts, so the board here is a plain MBC3. Fall
            // through to the header type.
            UnlMapper::PokeJadeDia(_) => {}
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
            // A bankless header ($00/$08/$09) on a >32KB ROM is physically
            // impossible, so infer the era-standard MBC1 (see
            // `decode_cartridge_type`) with the header's RAM bit; a live board,
            // not just a relabel, so the upper banks are actually reachable.
            ROM_ONLY | ROM_RAM | ROM_RAM_BATTERY if rom_banks > 2 => mbc1(ram_banks > 0),
            MBC7_SENSOR_RUMBLE_RAM_BATTERY => {
                Mapper::Mbc7(Mbc7 { ram_enabled: false, state: Mbc7State::default() })
            }
            HUC1_RAM_BATTERY => Mapper::HuC1(HuC1 { state: HuC1State::default() }),
            HUC3 => Mapper::HuC3(HuC3 { state: HuC3State::default() }),
            TAMA5 => Mapper::Tama5(Tama5 { state: Tama5State::default() }),
            POCKET_CAMERA => Mapper::Camera(Camera { ram_enabled: false, state: CameraState::default() }),
            ROM_RAM => Mapper::NoMbc(NoMbc { battery: false }),
            ROM_RAM_BATTERY => Mapper::NoMbc(NoMbc { battery: true }),
            // Same inference for a type byte that names no board at all (see
            // `decode_cartridge_type`): >32KB rules out bankless, so give it a
            // live MBC1 instead of a board that hides three quarters of the ROM.
            t if rom_banks > 2 && !is_documented_type(t) => mbc1(ram_banks > 0),
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
            Mapper::LiCheng(m) => f(m),
            Mapper::Bbd(m) => f(m),
            Mapper::Ggb81(m) => f(m),
            Mapper::Sintax(m) => f(m),
            Mapper::Hitek(m) => f(m),
            Mapper::Tama5(m) => f(m),
        }
    }
}

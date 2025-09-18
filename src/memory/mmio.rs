use crate::cartridge;
use crate::input;
use crate::memory;
use crate::memory::Addressable;
use serde::{Deserialize, Serialize};

use std::fs;
use std::io;

const EMPTY_BYTE: u8 = 0xFF;

const BIOS_START: u16 = 0x0000;
const BIOS_SIZE: usize = 256; // 256 bytes
const BIOS_END: u16 = BIOS_START + BIOS_SIZE as u16 - 1;
pub const CARTRIDGE_START: u16 = 0x0000;
pub const CARTRIDGE_SIZE: usize = 16384; // 16KB
const CARTRIDGE_AFTER_BIOS_START: u16 = 0x0100; // After BIOS is disabled
pub const CARTRIDGE_END: u16 = CARTRIDGE_START + CARTRIDGE_SIZE as u16 - 1;
pub const CARTRIDGE_BANK_START: u16 = 0x4000;
pub const CARTRIDGE_BANK_SIZE: usize = 16384; // 16KB
pub const CARTRIDGE_BANK_END: u16 = CARTRIDGE_BANK_START + CARTRIDGE_BANK_SIZE as u16 - 1;
pub const VRAM_START: u16 = 0x8000;
const VRAM_SIZE: usize = 8192; // 8KB
const VRAM_END: u16 = VRAM_START + VRAM_SIZE as u16 - 1;
const RAM_START: u16 = 0xA000;
const RAM_SIZE: usize = 8192; // 8KB
const RAM_END: u16 = RAM_START + RAM_SIZE as u16 - 1;
const WRAM_START: u16 = 0xC000;
const WRAM_SIZE: usize = 4096; // 4KB
const WRAM_END: u16 = WRAM_START + WRAM_SIZE as u16 - 1;
const WRAM_BANK_START: u16 = 0xD000;
const WRAM_BANK_SIZE: usize = 4096; // 4KB
const WRAM_BANK_END: u16 = WRAM_BANK_START + WRAM_BANK_SIZE as u16 - 1;
const ECHO_RAM_START: u16 = 0xE000;
const ECHO_RAM_SIZE: usize = 7680; // 7.5KB
const ECHO_RAM_END: u16 = ECHO_RAM_START + ECHO_RAM_SIZE as u16 - 1;
const ECHO_RAM_MIRROR_END: u16 = 0xDDFF; // Echo RAM mirrors WRAM and most of WRAM_BANK
const OAM_START: u16 = 0xFE00;
const OAM_SIZE: usize = 160; // 160 bytes
const OAM_END: u16 = OAM_START + OAM_SIZE as u16 - 1;
const UNUSED_START: u16 = 0xFEA0;
const UNUSED_SIZE: usize = 96; // 96 bytes
const UNUSED_END: u16 = UNUSED_START + UNUSED_SIZE as u16 - 1;
const IO_REGISTERS_START: u16 = 0xFF00;
const IO_REGISTERS_START_WITHOUT_JOYP: u16 = 0xFF01; // Exclude JOYP at 0xFF00
const IO_REGISTERS_SIZE: usize = 128; // 128 bytes
const IO_REGISTERS_SIZE_WITHOUT_JOYP: usize = IO_REGISTERS_SIZE - 1; // 127 bytes excluding JOYP
const IO_REGISTERS_END: u16 = IO_REGISTERS_START + IO_REGISTERS_SIZE as u16 - 1;
const HRAM_START: u16 = 0xFF80;
const HRAM_SIZE: usize = 127; // 127 bytes
const HRAM_END: u16 = HRAM_START + HRAM_SIZE as u16 - 1;
const IE_REGISTER: u16 = 0xFFFF; // Interrupt Enable Register

pub const REG_BOOT_OFF: u16 = 0xFF50; // Boot ROM disable

#[derive(Serialize, Deserialize, Clone)]
pub struct MMIO {
    #[serde(skip, default)]
    bios: Option<memory::Memory<BIOS_START, BIOS_SIZE>>,
    #[serde(skip, default)]
    cartridge: Option<cartridge::Cartridge>,
    input: input::Input,
    vram: memory::Memory<VRAM_START, VRAM_SIZE>,
    ram: memory::Memory<RAM_START, RAM_SIZE>,
    wram: memory::Memory<WRAM_START, WRAM_SIZE>,
    wram_bank: memory::Memory<WRAM_BANK_START, WRAM_BANK_SIZE>,
    oam: memory::Memory<OAM_START, OAM_SIZE>,
    io_registers: memory::Memory<IO_REGISTERS_START_WITHOUT_JOYP, IO_REGISTERS_SIZE_WITHOUT_JOYP>,
    hram: memory::Memory<HRAM_START, HRAM_SIZE>,
    ie_register: u8,
}

impl MMIO {
    pub fn new() -> Self {
        MMIO {
            bios: None,
            cartridge: None,
            input: input::Input::new(),
            vram: memory::Memory::new(),
            ram: memory::Memory::new(),
            wram: memory::Memory::new(),
            wram_bank: memory::Memory::new(),
            oam: memory::Memory::new(),
            io_registers: memory::Memory::new(),
            hram: memory::Memory::new(),
            ie_register: 0,
        }
    }

    pub fn reset(&mut self) {
        let mut new = Self::new();
        self.bios.clone_into(&mut new.bios);
        self.cartridge.clone_into(&mut new.cartridge);
        *self = new;
    }

    pub fn insert_cartridge(&mut self, cartridge: cartridge::Cartridge) {
        self.cartridge = Some(cartridge);
    }

    pub fn load_bios(&mut self, path: &str) -> Result<(), io::Error> {
        let data = fs::read(path)?;
        let mut bios = memory::Memory::new();
        if data.len() < BIOS_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "BIOS file too small"));
        }
        if data.len() > BIOS_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "BIOS file too large"));
        }
        for (i, &byte) in data.iter().take(BIOS_SIZE).enumerate() {
            bios.write(BIOS_START + i as u16, byte);
        }
        self.bios = Some(bios);
        Ok(())
    }
}

impl memory::Addressable for MMIO {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            BIOS_START..=BIOS_END => {
                match self.read(REG_BOOT_OFF) {
                    0 => {
                        match &self.bios {
                            Some(bios) => bios.read(addr),
                            None => EMPTY_BYTE,
                        }
                    },
                    _ => {
                        match &self.cartridge {
                            Some(cart) => cart.read(addr),
                            None => EMPTY_BYTE,
                        }
                    }
                }
            },
            CARTRIDGE_AFTER_BIOS_START..=CARTRIDGE_END => {
                match &self.cartridge {
                    Some(cart) => cart.read(addr),
                    None => EMPTY_BYTE,
                }
            },
            CARTRIDGE_BANK_START..=CARTRIDGE_BANK_END => {
                match &self.cartridge {
                    Some(cart) => cart.read(addr),
                    None => EMPTY_BYTE,
                }
            },
            VRAM_START..=VRAM_END => self.vram.read(addr),
            RAM_START..=RAM_END => self.ram.read(addr),
            WRAM_START..=WRAM_END => self.wram.read(addr),
            WRAM_BANK_START..=WRAM_BANK_END => self.wram_bank.read(addr),
            ECHO_RAM_START..=ECHO_RAM_END => {
                let addr = addr - 0x2000;
                match addr {
                    0..WRAM_START => panic!("This is literally never possible"),
                    WRAM_START..=WRAM_END => self.wram.read(addr),
                    WRAM_BANK_START..=ECHO_RAM_MIRROR_END => self.wram_bank.read(addr),
                    0xDE00..=0xFFFF => panic!("This is literally never possible"),
                }
            },
            OAM_START..=OAM_END => self.oam.read(addr),
            UNUSED_START..=UNUSED_END => EMPTY_BYTE,
            input::JOYP => self.input.read(addr),
            IO_REGISTERS_START_WITHOUT_JOYP..=IO_REGISTERS_END => self.io_registers.read(addr),
            HRAM_START..=HRAM_END => self.hram.read(addr),
            IE_REGISTER => self.ie_register,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            CARTRIDGE_START..=CARTRIDGE_END => {
                match self.cartridge.as_mut() {
                    Some(cart) => cart.write(addr, value),
                    None => (),
                }
            },
            CARTRIDGE_BANK_START..=CARTRIDGE_BANK_END => {
                match self.cartridge.as_mut() {
                    Some(cart) => cart.write(addr, value),
                    None => (),
                }
            },
            VRAM_START..=VRAM_END => self.vram.write(addr, value),
            RAM_START..=RAM_END => self.ram.write(addr, value),
            WRAM_START..=WRAM_END => self.wram.write(addr, value),
            WRAM_BANK_START..=WRAM_BANK_END => self.wram_bank.write(addr, value),
            ECHO_RAM_START..=ECHO_RAM_END => {
                let addr = addr - 0x2000;
                match addr {
                    0..WRAM_START => panic!("This is literally never possible"),
                    WRAM_START..=WRAM_END => self.wram.write(addr, value),
                    WRAM_BANK_START..=ECHO_RAM_MIRROR_END => self.wram_bank.write(addr, value),
                    0xDE00..=0xFFFF => panic!("This is literally never possible"),
                }
            },
            OAM_START..=OAM_END => self.oam.write(addr, value),
            UNUSED_START..=UNUSED_END => (), // Writes to unused memory are ignored
            input::JOYP => self.input.write(addr, value),
            IO_REGISTERS_START_WITHOUT_JOYP..=IO_REGISTERS_END => self.io_registers.write(addr, value),
            HRAM_START..=HRAM_END => self.hram.write(addr, value),
            IE_REGISTER => self.ie_register = value,
        }
    }
}

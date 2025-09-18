use crate::memory;
use crate::memory::mmio;
use crate::memory::Addressable;
use serde::{Deserialize, Serialize};

use std::fs;
use std::io;

#[derive(Serialize, Deserialize, Clone)]
pub struct Cartridge {
    cartridge: memory::Memory<{mmio::CARTRIDGE_START}, {mmio::CARTRIDGE_SIZE}>,
    bank: memory::Memory<{mmio::CARTRIDGE_BANK_START}, {mmio::CARTRIDGE_BANK_SIZE}>,
}

impl Cartridge {
    pub fn load(path: &str) -> Result<Self, io::Error> {
        let data = fs::read(path)?;
        let mut cartridge = memory::Memory::new();
        let mut bank = memory::Memory::new();
        let copy_size = data.len().min(mmio::CARTRIDGE_SIZE);
        for (i, &byte) in data.iter().take(copy_size).enumerate() {
            cartridge.write(mmio::CARTRIDGE_START + i as u16, byte);
        }
        for (i, &byte) in data.iter().skip(mmio::CARTRIDGE_SIZE).take(mmio::CARTRIDGE_BANK_SIZE).enumerate() {
            bank.write(mmio::CARTRIDGE_BANK_START + i as u16, byte);
        }
        Ok(Cartridge { cartridge, bank})
    }
}

impl memory::Addressable for Cartridge {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            mmio::CARTRIDGE_START..=mmio::CARTRIDGE_END => self.cartridge.read(addr),
            mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END => self.bank.read(addr),
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            mmio::CARTRIDGE_START..=mmio::CARTRIDGE_END => self.cartridge.write(addr, value),
            mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END => self.bank.write(addr, value),
            _ => (),
        }
    }
}

use crate::memory;
use crate::memory::mmio;
use crate::memory::Addressable;
use serde::{Deserialize, Serialize};

use std::fs;
use std::io;

#[derive(Serialize, Deserialize, Clone)]
pub struct Cartridge {
    cartridge: memory::Memory<{mmio::CARTRIDGE_START}, {mmio::CARTRIDGE_SIZE}>,
}

impl Cartridge {
    pub fn load(path: &str) -> Result<Self, io::Error> {
        let data = fs::read(path)?;
        let mut cartridge = memory::Memory::new();
        let copy_size = data.len().min(mmio::CARTRIDGE_SIZE);
        for (i, &byte) in data.iter().take(copy_size).enumerate() {
            cartridge.write(mmio::CARTRIDGE_START + i as u16, byte);
        }
        Ok(Cartridge { cartridge })
    }
}

impl memory::Addressable for Cartridge {
    fn read(&self, addr: u16) -> u8 {
        self.cartridge.read(addr)
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.cartridge.write(addr, value)
    }
}

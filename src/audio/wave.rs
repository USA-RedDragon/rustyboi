use serde::{Deserialize, Serialize};
use crate::audio::{NR30, NR34};
use crate::memory::mmio;
use crate::memory::Addressable;

#[derive(Clone, Serialize, Deserialize)]
pub struct Wave {
}

impl Wave {
    pub fn new() -> Self {
        Wave {
        }
    }

    pub fn step(&mut self, mmio: &mut mmio::MMIO) {

    }
}

impl Addressable for Wave {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR30..=NR34 => 0xFF, // TODO
            _ => panic!("Invalid address for Wave: {:#X}", addr)
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR30..=NR34 => {}, // TODO
            _ => panic!("Invalid address for Wave: {:#X}", addr)
        }
    }
}

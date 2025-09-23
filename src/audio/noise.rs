use serde::{Deserialize, Serialize};
use crate::audio::{NR41, NR44};
use crate::memory::mmio;
use crate::memory::Addressable;

#[derive(Clone, Serialize, Deserialize)]
pub struct Noise {
}

impl Noise {
    pub fn new() -> Self {
        Noise {
        }
    }

    pub fn step(&mut self, mmio: &mut mmio::MMIO) {

    }
}

impl Addressable for Noise {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR41..=NR44 => 0xFF, // TODO
            _ => panic!("Invalid address for Noise: {:#X}", addr)
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR41..=NR44 => {}, // TODO
            _ => panic!("Invalid address for Noise: {:#X}", addr)
        }
    }
}

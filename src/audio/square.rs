use serde::{Deserialize, Serialize};
use crate::audio::{NR10, NR14, NR21, NR24};
use crate::memory::mmio;
use crate::memory::Addressable;

#[derive(Clone, Serialize, Deserialize)]
pub struct SquareWave {
    channel1: bool,
}

impl SquareWave {
    pub fn new(channel1: bool) -> Self {
        SquareWave {
            channel1,
        }
    }

    pub fn step(&mut self, mmio: &mut mmio::MMIO) {

    }
}

impl Addressable for SquareWave {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR10..=NR14 => {
                if self.channel1 {
                    0xFF // TODO
                } else {
                    panic!("Invalid read from Channel 2 SquareWave: {:#X}", addr);
                }
            }
            NR21..=NR24 => {
                if !self.channel1 {
                    0xFF // TODO
                } else {
                    panic!("Invalid read from Channel 1 SquareWave: {:#X}", addr);
                }
            }
            _ => panic!("Invalid address for SquareWave: {:#X}", addr)
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR10..=NR14 => {
                if self.channel1 {
                    // TODO
                } else {
                    panic!("Invalid write to Channel 2 SquareWave: {:#X}", addr);
                }
            }
            NR21..=NR24 => {
                if !self.channel1 {
                    // TODO
                } else {
                    panic!("Invalid write to Channel 1 SquareWave: {:#X}", addr);
                }
            }
            _ => panic!("Invalid address for SquareWave: {:#X}", addr)
        }
    }
}

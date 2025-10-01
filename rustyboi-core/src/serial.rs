use crate::cpu;
use crate::memory::Addressable;
use crate::memory::mmio;

use serde::{Deserialize, Serialize};

pub const SB: u16 = 0xFF01;
pub const SC: u16 = 0xFF02;

const SC_TRANSFER_START: u8 = 1 << 7;
const SC_FAST_CLOCK: u8 = 1 << 1; // CGB only
const SC_INTERNAL_CLOCK: u8 = 1 << 0;

#[derive(Serialize, Deserialize, Clone)]
pub struct Serial {
    sb: u8,
    sc: u8,
    bits_left: u8,
    // Falling edge of the selected divider bit shifts one bit out.
    last_clock_bit: bool,
    cgb: bool,
}

impl Serial {
    pub fn new() -> Self {
        Serial {
            sb: 0,
            sc: 0,
            bits_left: 0,
            last_clock_bit: false,
            cgb: false,
        }
    }

    pub fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    fn transfer_active(&self) -> bool {
        (self.sc & SC_TRANSFER_START) != 0 && (self.sc & SC_INTERNAL_CLOCK) != 0
    }

    fn clock_bit_index(&self) -> u8 {
        if self.cgb && (self.sc & SC_FAST_CLOCK) != 0 {
            3 // 262144 Hz
        } else {
            8 // 8192 Hz
        }
    }

    fn clock_bit(&self, divider: u16) -> bool {
        (divider & (1 << self.clock_bit_index())) != 0
    }

    pub fn step(&mut self, divider: u16, mmio: &mut mmio::Mmio) {
        if !self.transfer_active() {
            self.last_clock_bit = self.clock_bit(divider);
            return;
        }

        let bit = self.clock_bit(divider);
        if self.last_clock_bit && !bit {
            self.sb = (self.sb << 1) | 1; // no peer connected -> ones shifted in
            self.bits_left = self.bits_left.saturating_sub(1);
            if self.bits_left == 0 {
                self.sc &= !SC_TRANSFER_START;
                mmio.request_interrupt(cpu::registers::InterruptFlag::Serial);
            }
        }
        self.last_clock_bit = bit;
    }
}

impl Addressable for Serial {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            SB => self.sb,
            SC => {
                let unused = if self.cgb { 0x7C } else { 0x7E };
                self.sc | unused
            }
            _ => panic!("Serial: Invalid read address {:04X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            SB => self.sb = value,
            SC => {
                self.sc = value;
                if (value & SC_TRANSFER_START) != 0 {
                    self.bits_left = 8;
                }
            }
            _ => panic!("Serial: Invalid write address {:04X}", addr),
        }
    }
}

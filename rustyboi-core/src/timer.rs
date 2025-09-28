use crate::cpu;
use crate::memory::Addressable;
use crate::memory::mmio;

use serde::{Deserialize, Serialize};

pub const DIV: u16 = 0xFF04;
pub const TIMA: u16 = 0xFF05;
pub const TMA: u16 = 0xFF06;
pub const TAC: u16 = 0xFF07;

// TAC register bits
const TAC_ENABLE: u8 = 1 << 2;  // Bit 2: Timer enable
const TAC_FREQUENCY_MASK: u8 = 0b00000011;  // Bits 0-1: Timer frequency

#[derive(Serialize, Deserialize, Clone)]
pub struct Timer {
    div: u8,
    tima: u8,
    tma: u8,
    tac: u8,
    // Internal state for cycle-accurate timing
    internal_counter: u16,  // 16-bit internal counter (always running)
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            div: 0,
            tima: 0,
            tma: 0,
            tac: 0,
            internal_counter: 0,
        }
    }

    pub fn step(&mut self, cpu: &mut cpu::SM83, mmio: &mut mmio::Mmio) {
        self.internal_counter = self.internal_counter.wrapping_add(1);
        self.div = (self.internal_counter >> 8) as u8;

        if (self.tac & TAC_ENABLE) == 0 {
            return;
        }

        let frequency_bits = self.tac & TAC_FREQUENCY_MASK;
        let bit_position = match frequency_bits {
            0b00 => 9,  // 4096 Hz
            0b01 => 3,  // 262144 Hz
            0b10 => 5,  // 65536 Hz
            0b11 => 7,  // 16384 Hz
            _ => unreachable!(),
        };

        let mask = 1 << bit_position;
        let previous_counter = self.internal_counter.wrapping_sub(1);
        if (previous_counter & mask) != 0 && (self.internal_counter & mask) == 0 {
            // Increment TIMA and handle overflow
            if self.tima == 0xFF {
                self.tima = self.tma;
                cpu.set_interrupt_flag(cpu::registers::InterruptFlag::Timer, true, mmio);
            } else {
                self.tima = self.tima.wrapping_add(1);
            }
        }
    }
}

impl Addressable for Timer {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            DIV => self.div,
            TIMA => self.tima,
            TMA => self.tma,
            TAC => self.tac,
            _ => panic!("Timer: Invalid read address {:04X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            DIV => {
                self.div = 0;
                self.internal_counter = 0;
            },
            TIMA => self.tima = value,
            TMA => self.tma = value,
            TAC => self.tac = value & 0b00000111,
            _ => panic!("Timer: Invalid write address {:04X}", addr),
        }
    }
    
}

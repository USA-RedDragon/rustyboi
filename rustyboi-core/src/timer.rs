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
const RELOAD_DELAY: u8 = 4;

#[derive(Serialize, Deserialize, Clone)]
pub struct Timer {
    div: u8,
    tima: u8,
    tma: u8,
    tac: u8,
    internal_counter: u16,
    // Falling edge of (selected DIV bit AND enable) ticks TIMA.
    #[serde(default)]
    last_timer_input: bool,
    // T-cycles until a pending overflow reloads TMA + raises IRQ; TIMA reads 0 meanwhile.
    #[serde(default)]
    reload_pending: u8,
    // Previous APU-clock bit (DIV bit 12, or 13 in double speed); its falling
    // edge clocks the APU frame sequencer.
    #[serde(default)]
    last_apu_div_bit: bool,
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            div: 0,
            tima: 0,
            tma: 0,
            tac: 0,
            internal_counter: 0,
            last_timer_input: false,
            reload_pending: 0,
            last_apu_div_bit: false,
        }
    }

    fn timer_input(&self) -> bool {
        if (self.tac & TAC_ENABLE) == 0 {
            return false;
        }
        let bit_position = match self.tac & TAC_FREQUENCY_MASK {
            0b00 => 9,
            0b01 => 3,
            0b10 => 5,
            0b11 => 7,
            _ => unreachable!(),
        };
        (self.internal_counter & (1 << bit_position)) != 0
    }

    fn update_edge(&mut self) {
        let input = self.timer_input();
        if self.last_timer_input && !input {
            self.increment_tima();
        }
        self.last_timer_input = input;
    }

    /// IRQ is deferred to `step` so this is also callable from the write path.
    fn increment_tima(&mut self) {
        if self.tima == 0xFF {
            self.tima = 0x00;
            self.reload_pending = RELOAD_DELAY;
        } else {
            self.tima = self.tima.wrapping_add(1);
        }
    }

    /// Initialize the timer's internal 16-bit counter (used at boot to mirror
    /// Gambatte's post-boot `cycleCounter - divLastUpdate` low 16 bits, which
    /// determines both DIV and the TIMA pre-tick phase). Bypasses the
    /// DIV-write reset behavior intentionally; runtime DIV writes still reset.
    pub fn set_internal_counter(&mut self, value: u16) {
        self.internal_counter = value;
        self.div = (value >> 8) as u8;
        self.last_timer_input = self.timer_input();
    }

    pub fn internal_counter(&self) -> u16 {
        self.internal_counter
    }

    pub fn step(&mut self, mmio: &mut mmio::Mmio) {
        if self.reload_pending > 0 {
            self.reload_pending -= 1;
            if self.reload_pending == 0 {
                self.tima = self.tma;
                mmio.request_interrupt(cpu::registers::InterruptFlag::Timer);
            }
        }

        self.internal_counter = self.internal_counter.wrapping_add(1);
        self.div = (self.internal_counter >> 8) as u8;
        self.update_edge();

        // The APU frame sequencer is clocked by the falling edge of DIV bit 12
        // (bit 13 in double speed), so it tracks DIV writes like the timer does.
        let apu_bit_pos = if mmio.is_double_speed_mode() { 13 } else { 12 };
        let apu_bit = (self.internal_counter & (1 << apu_bit_pos)) != 0;
        if self.last_apu_div_bit && !apu_bit {
            mmio.clock_apu_frame_sequencer();
        }
        self.last_apu_div_bit = apu_bit;
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
                self.internal_counter = 0;
                self.div = 0;
                self.update_edge(); // counter reset can glitch a TIMA tick
            },
            TIMA => {
                self.reload_pending = 0; // write during reload window aborts it
                self.tima = value;
            },
            TMA => self.tma = value,
            TAC => {
                self.tac = value & 0b00000111;
                self.update_edge(); // freq/enable change can glitch a TIMA tick
            },
            _ => panic!("Timer: Invalid write address {:04X}", addr),
        }
    }
    
}

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
    tima: u8,
    tma: u8,
    tac: u8,
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
    // Double-speed state observed at the last `step`; used by `speed_change`
    // (called right after the speed flag toggles, before any further step) to
    // learn the pre-switch speed.
    #[serde(default)]
    last_double_speed: bool,
    // Absolute, never-reset T-cycle counter mirroring Gambatte's `cycleCounter_`.
    // This is the single source of time in this module: both the DIV divider and
    // the TIMA edge-detection are pure derivations of it.
    #[serde(default)]
    abs_cc: u64,
    // `abs_cc` value of the last DIV write (Gambatte `divLastUpdate`). The DIV
    // divider is `(abs_cc - div_anchor) & 0xFFFF`, so a DIV write — which must
    // zero the divider — is just `div_anchor = abs_cc`. The high byte of that
    // divider is the DIV register; the divider also drives the TIMA edge bit.
    #[serde(default)]
    div_anchor: u64,
    // Monotonic count of DIV writes (each rebases `div_anchor`). The APU master
    // clock reads this to detect a DIV reset and apply Gambatte's
    // `PSG::divReset` cycle-counter fold.
    #[serde(default)]
    div_reset_count: u64,
}

impl Timer {
    pub fn new() -> Self {
        Timer {
            tima: 0,
            tma: 0,
            tac: 0,
            last_timer_input: false,
            reload_pending: 0,
            last_apu_div_bit: false,
            last_double_speed: false,
            abs_cc: 0,
            div_anchor: 0,
            div_reset_count: 0,
        }
    }

    /// Absolute, never-reset T-cycle counter (Gambatte `cycleCounter_`). The APU
    /// master clock anchors to this so it retains the full phase a DIV write would
    /// drop.
    pub fn abs_cc(&self) -> u64 {
        self.abs_cc
    }

    /// Monotonic count of DIV writes. The APU master clock compares this against
    /// its last-seen value to detect a DIV reset (Gambatte `PSG::divReset`).
    pub fn div_reset_count(&self) -> u64 {
        self.div_reset_count
    }

    /// The 16-bit DIV divider: a pure derivation of the master counter and the
    /// last DIV-write anchor (Gambatte `(cc - divLastUpdate)`'s low 16 bits). A
    /// DIV write zeroes this by rebasing `div_anchor` to `abs_cc`.
    fn divider(&self) -> u16 {
        (self.abs_cc.wrapping_sub(self.div_anchor) & 0xFFFF) as u16
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
        (self.divider() & (1 << bit_position)) != 0
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
        // Anchor the absolute counter congruent to the divider so the APU master
        // clock's `abs_cc >> 1` reproduces the post-boot `divider >> 1` low bits
        // while carrying true high-resolution bits a DIV reset would otherwise
        // drop. With `div_anchor == 0`, `divider() == abs_cc & 0xFFFF == value`.
        self.abs_cc = value as u64;
        self.div_anchor = 0;
        self.last_timer_input = self.timer_input();
    }

    pub fn internal_counter(&self) -> u16 {
        self.divider()
    }

    /// CGB STOP speed switch. Gambatte's `Tima::speedChange` shifts the timer's
    /// `lastUpdate_` back by 4 T-cycles when the timer is enabled at one of the
    /// faster frequencies (`tac & 0x07 >= 0x05`), i.e. TIMA effectively counts 4
    /// extra cycles at the switch (potentially one extra increment) before the
    /// DIV reset that follows. We reproduce that by running 4 extra counter
    /// ticks (with edge detection) prior to the DIV reset.
    pub fn speed_change(&mut self) {
        // Fast-frequency timers get the 4-cycle catch-up Gambatte applies in
        // `Tima::speedChange`. A switch back to single speed additionally runs
        // the catch-up for any enabled timer: the double->single STOP window is
        // 4 cycles longer in TIMA's edge accounting. The DIV reset that follows
        // zeroes the divider, so this advance affects only TIMA's edge count,
        // not the post-switch DIV phase. Advancing the divider by one tick is
        // equivalent to pulling `div_anchor` back by one (`divider() += 1`).
        let fast = (self.tac & 0x07) >= 0x05;
        let single_after = self.last_double_speed && (self.tac & TAC_ENABLE) != 0;
        if fast || single_after {
            for _ in 0..4 {
                self.div_anchor = self.div_anchor.wrapping_sub(1);
                self.update_edge();
            }
        }
    }

    pub fn step(&mut self, mmio: &mut mmio::Mmio) {
        if self.reload_pending > 0 {
            self.reload_pending -= 1;
            if self.reload_pending == 0 {
                self.tima = self.tma;
                mmio.request_interrupt(cpu::registers::InterruptFlag::Timer);
            }
        }

        self.abs_cc = self.abs_cc.wrapping_add(1);
        self.update_edge();

        // The APU frame sequencer is clocked by the falling edge of DIV bit 12
        // (bit 13 in double speed), so it tracks DIV writes like the timer does.
        self.last_double_speed = mmio.is_double_speed_mode();
        let apu_bit_pos = if self.last_double_speed { 13 } else { 12 };
        let apu_bit = (self.divider() & (1 << apu_bit_pos)) != 0;
        if self.last_apu_div_bit && !apu_bit {
            mmio.clock_apu_frame_sequencer();
        }
        self.last_apu_div_bit = apu_bit;
    }
}

impl Addressable for Timer {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            DIV => (self.divider() >> 8) as u8,
            TIMA => self.tima,
            TMA => self.tma,
            TAC => self.tac,
            _ => panic!("Timer: Invalid read address {:04X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            DIV => {
                // Rebase the divider to zero (Gambatte `divLastUpdate = cc`).
                self.div_anchor = self.abs_cc;
                self.div_reset_count = self.div_reset_count.wrapping_add(1);
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

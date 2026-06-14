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
    // Absolute completion model (mirrors Gambatte's serial event time): a
    // transfer's interrupt fires at `complete_at` (a CPU T-phase), with one bit
    // shifted out every `step_t` phases. Bits already shifted are reconstructed
    // from the remaining time so SB reads stay correct mid-transfer.
    active: bool,
    complete_at: u64,
    step_t: u32,
    bits_shifted: u8,
    cgb: bool,
}

impl Serial {
    pub fn new() -> Self {
        Serial {
            sb: 0,
            sc: 0,
            active: false,
            complete_at: 0,
            step_t: 0,
            bits_shifted: 0,
            cgb: false,
        }
    }

    pub fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    /// Latch an SC (FF02) write and (re)schedule the transfer event.
    pub fn schedule_sc(&mut self, value: u8, divider: u16, phase: u64) {
        self.sc = value;
        self.schedule(divider, phase);
    }

    fn internal_start(&self) -> bool {
        (self.sc & SC_TRANSFER_START) != 0 && (self.sc & SC_INTERNAL_CLOCK) != 0
    }

    /// Schedule (or cancel) the transfer event. `divider` is the timer's
    /// internal counter and `phase` the CPU T-phase, both sampled at the SC
    /// write's resolution cc. Mirrors Gambatte memory.cpp 0x02 write:
    /// `eventTime = cc - (cc - divLastUpdate) % align + step * 8`.
    fn schedule(&mut self, divider: u16, phase: u64) {
        if !self.internal_start() {
            self.active = false;
            return;
        }
        let fast = self.cgb && (self.sc & SC_FAST_CLOCK) != 0;
        let (step, align_mask) = if fast { (16u32, 0x0Fu64) } else { (512u32, 0xFFu64) };
        self.step_t = step;
        self.bits_shifted = 0;
        // Snap `phase` down to the DIV-aligned grid (Gambatte:
        // `cc - (cc - divLastUpdate) % align`), add the 8-bit transfer span, and
        // back off one M-cycle: the SC write resolves at this M-cycle's end
        // (tick-before-write), one M-cycle past Gambatte's mid-cycle write cc.
        const WRITE_CC_OFFSET: u64 = 8;
        self.complete_at =
            phase - (divider as u64 & align_mask) + (step as u64) * 8 - WRITE_CC_OFFSET;
        self.active = true;
    }

    /// Advance bookkeeping at CPU T-phase `phase` (sampled before this dot's
    /// advance, matching the per-dot tick ordering). Shifts SB as bits clock out
    /// and raises the serial IRQ exactly when `complete_at` is reached.
    pub fn step(&mut self, phase: u64, mmio: &mut mmio::Mmio) {
        if !self.active {
            return;
        }
        // Number of bits whose clock edge has passed by `phase`.
        let target = if phase >= self.complete_at {
            8
        } else {
            let remaining_t = self.complete_at - phase;
            let remaining_bits = ((remaining_t + self.step_t as u64 - 1) / self.step_t as u64) as u8;
            8u8.saturating_sub(remaining_bits)
        };
        while self.bits_shifted < target {
            self.sb = (self.sb << 1) | 1; // no peer connected -> ones shifted in
            self.bits_shifted += 1;
        }
        if phase >= self.complete_at {
            self.active = false;
            self.sc &= !SC_TRANSFER_START;
            mmio.request_interrupt(cpu::registers::InterruptFlag::Serial);
        }
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
            SC => self.sc = value,
            _ => panic!("Serial: Invalid write address {:04X}", addr),
        }
    }
}

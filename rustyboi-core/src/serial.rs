use crate::cpu;
use crate::memory::Addressable;
use crate::memory::mmio;

use serde::{Deserialize, Serialize};

pub const SB: u16 = 0xFF01;
pub const SC: u16 = 0xFF02;

// ds-engine STAGE 3: RB_LAZYPERIPH. When set, serial anchors its completion
// event on the TRUE write cc (the raw master cc captured at the SC-write access
// start, supplied via stage-1 RB_EXACTCC `write_access_cc()` = raw abs_cc) and
// drops the per-dot phase-mapping `WRITE_CC_OFFSET=7`. The offset existed only
// to fold the legacy abs_cc-advanced-at-start-of-dot phase back to Gambatte's
// SC-write cc; with the exact write cc it becomes 0.

const SC_TRANSFER_START: u8 = 1 << 7;
const SC_FAST_CLOCK: u8 = 1 << 1; // CGB only
const SC_INTERNAL_CLOCK: u8 = 1 << 0;

#[derive(Serialize, Deserialize, Clone)]
pub struct Serial {
    sb: u8,
    sc: u8,
    // Absolute completion model (mirrors Gambatte's serial event time): a
    // transfer's interrupt fires at `complete_at` (a master-cc value), with one
    // bit shifted out every `step_t` cc. Bits already shifted are reconstructed
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
    /// internal counter and `phase` the master cc (`abs_cc`-based) at the SC
    /// write's resolution cc — serial now shares the single master clock, not a
    /// separate `cpu_t_phase` (M8 serial merge). Mirrors Gambatte memory.cpp
    /// 0x02 write: `eventTime = cc - (cc - divLastUpdate) % align + step * 8`.
    fn schedule(&mut self, divider: u16, phase: u64) {
        if !self.internal_start() {
            self.active = false;
            return;
        }
        let fast = self.cgb && (self.sc & SC_FAST_CLOCK) != 0;
        // DIV-align residue mask matches Gambatte's `% 8` (fast) / `% 0x100`
        // (slow) in memory.cpp case 0x02; the realign path already uses align=8.
        let (step, align_mask) = if fast { (16u32, 0x07u64) } else { (512u32, 0xFFu64) };
        self.step_t = step;
        self.bits_shifted = 0;
        // Snap `phase` down to the DIV-aligned grid (Gambatte:
        // `cc - (cc - divLastUpdate) % align`), add the 8-bit transfer span, then
        // subtract the master-cc write-phase offset mapping the per-dot `abs_cc`
        // (advanced at the start of each dot's tick) to Gambatte's mid-cycle SC
        // write cc. Swept against the serial cluster post-merge (minimum at 6/7).
        // Under RB_LAZYPERIPH the SC write resolves at the exact (raw) write cc,
        // so the per-dot phase-mapping offset collapses to 0.
        // STAGE 3/7: the SC write resolves at the exact (raw) write cc, so the
        // old per-dot phase-mapping WRITE_CC_OFFSET=7 is permanently 0.
        self.complete_at =
            phase - (divider as u64 & align_mask) + (step as u64) * 8;
        self.active = true;
    }

    /// Re-align the pending transfer event to a DIV reset, mirroring Gambatte
    /// memory.cpp `case 0x04`:
    /// `n = t + (cc - t) % align - 2 * ((cc - t) & half); eventTime = max(cc,n)`.
    /// A DIV write resets the internal divider that gates the serial shift clock,
    /// so the next (and only the next) shift edge snaps to the new divider phase.
    /// Only perturbs an in-flight, future-completing transfer.
    pub fn realign_to_div(&mut self, phase: u64) {
        if !self.active || self.complete_at <= phase {
            return;
        }
        let fast = self.cgb && (self.sc & SC_FAST_CLOCK) != 0;
        let (align, half) = if fast { (8u64, 4u64) } else { (0x100u64, 0x80u64) };
        // Gambatte operates on the raw serial event time `t` and write cc; our
        // `complete_at` carries the SC-write offset, so undo it for the residue
        // math (the matching write-cc offset cancels in `delta`). Must equal
        // `schedule`'s WRITE_CC_OFFSET.
        // STAGE 3/7: WRITE_CC_OFFSET is permanently 0, so `complete_at` already
        // carries the raw write cc — no offset to undo.
        let t = self.complete_at;
        let delta = phase.wrapping_sub(t); // (cc - t), wraps since t > cc
        let n = t
            .wrapping_add(delta % align)
            .wrapping_sub(2 * (delta & half));
        self.complete_at = n.max(phase);
    }

    /// Advance bookkeeping at master cc `phase` (the timer's `abs_cc`, sampled
    /// within this dot's tick). Shifts SB as bits clock out and raises the serial
    /// IRQ exactly when `complete_at` is reached.
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

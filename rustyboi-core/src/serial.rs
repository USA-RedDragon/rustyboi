use crate::cpu;
use crate::memory::Addressable;
use crate::memory::mmio;
use crate::printer;

use serde::{Deserialize, Serialize};

pub const SB: u16 = 0xFF01;
pub const SC: u16 = 0xFF02;

/// A device plugged into the link port. The serial unit latches the device's
/// preloaded response byte at transfer start (the peer shift register's
/// contents) and hands the completed outgoing byte back at transfer end, so a
/// device's reply to byte N can only depend on bytes < N — exactly the
/// simultaneous-exchange constraint of the real bus. `Disconnected` keeps the
/// no-peer behavior (0xFF shifts in) byte-identical.
#[derive(Serialize, Deserialize, Clone, Default)]
pub enum SerialDevice {
    #[default]
    Disconnected,
    Printer(printer::GbPrinter),
}

impl SerialDevice {
    /// The byte the attached device would shift out during the next transfer,
    /// or None when nothing is plugged in.
    pub fn preloaded_response(&self) -> Option<u8> {
        match self {
            SerialDevice::Disconnected => None,
            SerialDevice::Printer(p) => Some(p.preloaded_response()),
        }
    }

    /// Deliver a completed byte exchange to the device. `cc` is the master
    /// clock at completion (deterministic device timing; never wall-clock).
    pub fn receive_byte(&mut self, tx: u8, cc: u64) {
        match self {
            SerialDevice::Disconnected => {}
            SerialDevice::Printer(p) => p.receive_byte(tx, cc),
        }
    }
}

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
    // Link-peer exchange state. `rx_latch` is the peer's response byte,
    // latched at transfer start from the device's preloaded shift register
    // (0xFF when disconnected, preserving ones-shift-in). `tx_acc` collects
    // the bits actually shifted out (SB's MSB at each shift edge), delivered
    // to the device at completion.
    #[serde(default = "default_rx_latch")]
    rx_latch: u8,
    #[serde(default)]
    tx_acc: u8,
}

fn default_rx_latch() -> u8 {
    0xFF
}

impl Default for Serial {
    fn default() -> Self {
        Self::new()
    }
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
            rx_latch: 0xFF,
            tx_acc: 0,
        }
    }

    pub fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    pub fn is_cgb(&self) -> bool {
        self.cgb
    }

    /// per-access STAGE 1: true while a serial transfer is in flight (its
    /// `complete_at` event is pending). Blocks the idle bulk-skip so the transfer's
    /// bit-shift and completion IRQ land at the exact cc.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Latch an SC (FF02) write and (re)schedule the transfer event.
    /// `link_rx` is the attached serial device's preloaded response byte
    /// (None = disconnected cable), latched here — at transfer start — the
    /// way the peer's shift register contents would be.
    pub fn schedule_sc(&mut self, value: u8, divider: u16, phase: u64, link_rx: Option<u8>) {
        self.sc = value;
        self.schedule(divider, phase, link_rx);
    }

    fn internal_start(&self) -> bool {
        (self.sc & SC_TRANSFER_START) != 0 && (self.sc & SC_INTERNAL_CLOCK) != 0
    }

    /// Schedule (or cancel) the transfer event. `divider` is the timer's
    /// internal counter and `phase` the master cc (`abs_cc`-based) at the SC
    /// write's resolution cc — serial now shares the single master clock, not a
    /// separate `cpu_t_phase` (M8 serial merge). Mirrors Gambatte memory.cpp
    /// 0x02 write: `eventTime = cc - (cc - divLastUpdate) % align + step * 8`.
    fn schedule(&mut self, divider: u16, phase: u64, link_rx: Option<u8>) {
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
        self.rx_latch = link_rx.unwrap_or(0xFF);
        self.tx_acc = 0;
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
            let remaining_bits = remaining_t.div_ceil(self.step_t as u64) as u8;
            8u8.saturating_sub(remaining_bits)
        };
        while self.bits_shifted < target {
            // The outgoing bit is SB's MSB at the shift edge; the incoming bit
            // is the peer's latched response, MSB first (0xFF when no peer
            // connected -> ones shifted in, the disconnected-cable behavior).
            self.tx_acc = (self.tx_acc << 1) | (self.sb >> 7);
            let in_bit = (self.rx_latch >> (7 - self.bits_shifted)) & 1;
            self.sb = (self.sb << 1) | in_bit;
            self.bits_shifted += 1;
        }
        if phase >= self.complete_at {
            self.active = false;
            self.sc &= !SC_TRANSFER_START;
            mmio.request_interrupt(cpu::registers::InterruptFlag::Serial);
            // The device sees the byte at the transfer's true completion cc
            // (not the possibly-later observation phase after a bulk skip).
            mmio.serial_device_receive(self.tx_acc, self.complete_at);
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

#[cfg(test)]
mod tests {
    use crate::cartridge::Cartridge;
    use crate::gb::{GB, Hardware};

    /// Hand-assembled ROM: sends the 10 bytes at 0x0200 over the link port
    /// (SB write, SC=0x81, poll SC bit 7, read SB) and stores each response to
    /// 0xC000+, then spins. The table holds a printer INIT packet.
    fn link_probe_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]); // nop; jp 0150
        rom[0x150..0x16E].copy_from_slice(&[
            0x21, 0x00, 0xC0, // ld hl, C000
            0x11, 0x00, 0x02, // ld de, 0200
            0x06, 0x0A, // ld b, 10
            0x1A, // ld a, (de)
            0x13, // inc de
            0xE0, 0x01, // ldh (SB), a
            0x3E, 0x81, // ld a, 81
            0xE0, 0x02, // ldh (SC), a
            0xF0, 0x02, // ldh a, (SC)
            0xE6, 0x80, // and 80
            0x20, 0xFA, // jr nz, poll
            0xF0, 0x01, // ldh a, (SB)
            0x22, // ld (hl+), a
            0x05, // dec b
            0x20, 0xEC, // jr nz, next byte
            0x18, 0xFE, // jr $ (done)
        ]);
        // Printer INIT packet: 88 33 | cmd 01 | comp 00 | len 0000 | cksum 0100 | 00 00
        rom[0x200..0x20A].copy_from_slice(&[0x88, 0x33, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        rom
    }

    fn run_probe(attach_printer: bool) -> Vec<u8> {
        let mut gb = GB::new(Hardware::DMG);
        gb.insert(Cartridge::from_bytes(&link_probe_rom()).unwrap());
        gb.skip_bios();
        if attach_printer {
            gb.attach_printer();
        }
        // 10 bytes at 8 KHz ≈ 41k cc; a handful of frames is plenty.
        for _ in 0..20 {
            gb.run_until_frame(false);
        }
        (0..10).map(|i| gb.read_memory(0xC000 + i)).collect()
    }

    /// Disconnected cable: every transferred byte reads back 0xFF.
    #[test]
    fn disconnected_link_shifts_ones() {
        assert_eq!(run_probe(false), vec![0xFF; 10]);
    }

    /// With a printer attached the INIT packet gets 0x00 during the body and
    /// the 0x81 alive + 0x00 status pair in the trailing slots, through the
    /// real serial timing path (schedule/shift/IRQ), not a shortcut.
    #[test]
    fn printer_answers_init_packet() {
        let responses = run_probe(true);
        assert_eq!(responses[..8], [0x00; 8]);
        assert_eq!(responses[8], 0x81, "alive byte");
        assert_eq!(responses[9], 0x00, "status after INIT");
    }
}

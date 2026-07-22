//! HuC-3 RTC MCU: the mailbox command engine and its clock/event nibble helpers.

use super::*;
use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

impl Cartridge {
    /// Live HuC-3 clock (minute-of-day, day counter) read from its nibble
    /// locations 0x10-0x12 / 0x13-0x15 in the RTC MCU memory.
    pub(super) fn huc3_clock(&self) -> (u16, u16) {
        if self.huc3_rtc.mem.len() < 0x16 {
            return (0, 0);
        }
        let m = &self.huc3_rtc.mem;
        let minutes = (m[0x10] as u16 & 0xF) | ((m[0x11] as u16 & 0xF) << 4) | ((m[0x12] as u16 & 0xF) << 8);
        let days = (m[0x13] as u16 & 0xF) | ((m[0x14] as u16 & 0xF) << 4) | ((m[0x15] as u16 & 0xF) << 8);
        (minutes, days)
    }
    pub(super) fn huc3_set_clock(&mut self, minutes: u16, days: u16) {
        if self.huc3_rtc.mem.len() < 0x16 {
            return;
        }
        let m = &mut self.huc3_rtc.mem;
        m[0x10] = (minutes & 0xF) as u8;
        m[0x11] = ((minutes >> 4) & 0xF) as u8;
        m[0x12] = ((minutes >> 8) & 0xF) as u8;
        m[0x13] = (days & 0xF) as u8;
        m[0x14] = ((days >> 4) & 0xF) as u8;
        m[0x15] = ((days >> 8) & 0xF) as u8;
    }
    /// Event ("alarm") time as total minutes, from nibbles 0x58-0x5A (minutes)
    /// and 0x5B-0x5D (days).
    pub(super) fn huc3_event_total_minutes(&self) -> i64 {
        let m = &self.huc3_rtc.mem;
        let minutes =
            (m[0x58] as i64 & 0xF) | ((m[0x59] as i64 & 0xF) << 4) | ((m[0x5A] as i64 & 0xF) << 8);
        let days =
            (m[0x5B] as i64 & 0xF) | ((m[0x5C] as i64 & 0xF) << 4) | ((m[0x5D] as i64 & 0xF) << 8);
        days * 1440 + minutes
    }
    pub(super) fn huc3_set_event_total_minutes(&mut self, total: i64) {
        // 12-bit day counter x 1440 minutes wraps the representable range.
        let total = total.rem_euclid(4096 * 1440);
        let minutes = (total % 1440) as u16;
        let days = (total / 1440) as u16;
        let m = &mut self.huc3_rtc.mem;
        m[0x58] = (minutes & 0xF) as u8;
        m[0x59] = ((minutes >> 4) & 0xF) as u8;
        m[0x5A] = ((minutes >> 8) & 0xF) as u8;
        m[0x5B] = (days & 0xF) as u8;
        m[0x5C] = ((days >> 4) & 0xF) as u8;
        m[0x5D] = ((days >> 8) & 0xF) as u8;
    }
    /// Execute the pending HuC-3 RTC MCU command (mailbox command+argument,
    /// triggered by a semaphore write with bit 0 clear). The MCU is modeled as
    /// always-ready / instant execution; the semaphore therefore always reads
    /// "ready". Command set per Pan Docs "RTC Communication Protocol".
    pub(super) fn huc3_execute_command(&mut self) {
        if self.huc3_rtc.mem.len() < 0x100 {
            return;
        }
        let mut mb = match &self.mapper {
            Mapper::HuC3(m) => m.state,
            _ => return,
        };
        let addr = mb.rtc_address as usize;
        match mb.rtc_command {
            0x1 => {
                // Read value and increment access address.
                mb.rtc_result = self.huc3_rtc.mem[addr] & 0x0F;
                mb.rtc_address = mb.rtc_address.wrapping_add(1);
            }
            0x3 => {
                // Write value and increment access address.
                self.huc3_rtc.mem[addr] = mb.rtc_argument & 0x0F;
                mb.rtc_address = mb.rtc_address.wrapping_add(1);
            }
            0x4 => {
                // Set access address least significant nibble.
                mb.rtc_address = (mb.rtc_address & 0xF0) | mb.rtc_argument;
            }
            0x5 => {
                // Set access address most significant nibble.
                mb.rtc_address =
                    (mb.rtc_address & 0x0F) | (mb.rtc_argument << 4);
            }
            0x6 => {
                // Extended command in the argument nibble.
                match mb.rtc_argument {
                    0x0 => {
                        // Copy current time (0x10-0x16) to I/O space 0x00-0x06.
                        // Pan Docs specifies "locations $00-06": 7 nibbles.
                        for i in 0..7 {
                            self.huc3_rtc.mem[i] = self.huc3_rtc.mem[0x10 + i] & 0x0F;
                        }
                    }
                    0x1 => {
                        // Copy I/O space 0x00-0x06 to current time, and shift
                        // the event time by the same delta so the remaining
                        // duration until the event is preserved (Pan Docs).
                        let (old_min, old_day) = self.huc3_clock();
                        for i in 0..7 {
                            self.huc3_rtc.mem[0x10 + i] = self.huc3_rtc.mem[i] & 0x0F;
                        }
                        let (new_min, new_day) = self.huc3_clock();
                        let delta = (new_day as i64 * 1440 + new_min as i64)
                            - (old_day as i64 * 1440 + old_min as i64);
                        let event = self.huc3_event_total_minutes();
                        self.huc3_set_event_total_minutes(event + delta);
                        // Setting the time restarts the current minute.
                        self.huc3_rtc.accum = 0;
                    }
                    0x2 => {
                        // Status request issued by games on boot; they refuse
                        // to start unless the response is 1 (Pan Docs).
                    }
                    0xE => {
                        // Tone generator trigger. The piezo speaker is not
                        // modeled; accept and ignore.
                    }
                    _ => {}
                }
                // Hardware-observed: extended commands leave 1 in the response
                // nibble (this is what boot-time $62 status checks rely on).
                mb.rtc_result = 0x1;
            }
            // Commands $0, $2 and $7 are unobserved/unknown on hardware
            // (Pan Docs); treat as no-ops.
            _ => {}
        }
        if let Mapper::HuC3(m) = &mut self.mapper {
            m.state = mb;
        }
        // Commands can rewrite the clock/event nibbles; persist immediately.
        self.flush_rtc_file();
    }
}

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct HuC3 {
    pub state: HuC3State,
}

impl Banking for HuC3 {
    fn rom_bankn(&self, g: Geom) -> usize {
        (self.state.rom_bank as usize) % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        (self.state.ram_bank as usize) % g.ram_banks.max(1)
    }
}


// --- state ---------------------------------------------------------------

/// HuC-3 register state. The 0x0000-0x1FFF register selects what A000-BFFF
/// accesses: 0x0 RAM read-only, 0xA RAM read/write, 0xB RTC command mailbox
/// (write), 0xC RTC command/response (read), 0xD RTC semaphore, 0xE IR.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) struct HuC3State {
    pub(super) mode: u8,
    pub(super) rom_bank: u8, // 7-bit; bank 0 selectable like MBC5
    pub(super) ram_bank: u8,
    /// RTC MCU mailbox: command (bits 6-4 of the 0xB write) + argument (3-0),
    /// executed on a 0xD write with bit 0 clear; result readable through 0xC.
    pub(super) rtc_command: u8,
    pub(super) rtc_argument: u8,
    pub(super) rtc_result: u8,
    /// 256-nibble access pointer into the RTC MCU memory.
    pub(super) rtc_address: u8,
}

impl Default for HuC3State {
    fn default() -> Self {
        Self {
            mode: 0,
            rom_bank: 1,
            ram_bank: 0,
            rtc_command: 0,
            rtc_argument: 0,
            rtc_result: 0,
            rtc_address: 0,
        }
    }
}


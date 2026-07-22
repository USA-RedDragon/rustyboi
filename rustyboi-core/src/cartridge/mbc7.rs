//! MBC7: 2-axis accelerometer sampling + the 93LC56 serial-EEPROM state machine
//! (the EEPROM contents live in `ram_data`; this drives the Ax8x serial link).

use super::*;
use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

impl Cartridge {
    /// Feed the MBC7 accelerometer with a live tilt sample, in units of g
    /// (Earth gravity). Neutral (flat) is (0, 0); positive x tilts left,
    /// positive y tilts up, matching Pan Docs' "lower values are towards the
    /// right / bottom". The value is only observed by software when it latches
    /// a sample via the Ax0x/Ax1x erase+latch protocol. No-op storage for
    /// non-MBC7 carts.
    ///
    /// This is the sole input hook for MBC7 tilt (parallel to `set_camera_image`
    /// for the GB Camera); it is the intended path for a frontend to drive the
    /// accelerometer and is awaiting frontend wiring, so it is currently unused.
    #[allow(dead_code)]
    pub(crate) fn set_accelerometer(&mut self, x_g: f32, y_g: f32) {
        self.mbc7_sensor_x = x_g;
        self.mbc7_sensor_y = y_g;
    }
    /// Convert a sensor reading in g to the latched 16-bit accelerometer
    /// value: centered at 0x81D0, 1 g ~ 0x70 counts (Pan Docs).
    pub(super) fn mbc7_accel_counts(g: f32) -> u16 {
        let v = 0x81D0_i32 + (g * 0x70 as f32) as i32;
        v.clamp(0, 0xFFFF) as u16
    }
    /// One 16-bit word of the MBC7 EEPROM (128 little-endian words backed by
    /// `ram_data`).
    pub(super) fn mbc7_eeprom_word(&self, addr: usize) -> u16 {
        let i = (addr & 0x7F) * 2;
        (self.ram_data[i] as u16) | ((self.ram_data[i + 1] as u16) << 8)
    }
    pub(super) fn mbc7_eeprom_set_word(&mut self, addr: usize, word: u16) {
        let i = (addr & 0x7F) * 2;
        // write_ram_byte streams to the battery save file as well.
        let _ = self.write_ram_byte(i, (word & 0xFF) as u8);
        let _ = self.write_ram_byte(i + 1, (word >> 8) as u8);
    }
    /// Bit-banged 93LC56 write via the Ax8x register: bit 0 = DO (ignored on
    /// write), bit 1 = DI, bit 6 = CLK, bit 7 = CS. Commands are 1 start bit
    /// followed by 10 instruction bits, shifted MSB-first on rising CLK edges
    /// while CS is high (leading 0 bits before the start bit are ignored):
    ///
    /// ```text
    /// READ  10xAAAAAAA (then 16 bits out)   EWEN 0011xxxxxx
    /// WRITE 01xAAAAAAA (then 16 bits in)    EWDS 0000xxxxxx
    /// ERASE 11xAAAAAAA                      ERAL 0010xxxxxx
    /// WRAL  0001xxxxxx (then 16 bits in)
    /// ```
    ///
    /// Programming ops (WRITE/ERASE/WRAL/ERAL) execute on the CS falling edge
    /// that follows the last bit, require a prior EWEN, and are modeled as
    /// completing instantly: DO then reads 1 (RDY) for the software
    /// busy-poll.
    /// Bus-facing MBC7 EEPROM write: run the state machine on the live board's
    /// EEPROM, copied out of the mapper so the state machine (which also touches
    /// `ram_data`/the save file via `mbc7_eeprom_word`/`set_word`) borrows cleanly.
    pub(super) fn mbc7_eeprom_bus_write(&mut self, value: u8) {
        let mut ee = match &self.mapper {
            Mapper::Mbc7(m) => m.state.eeprom.clone(),
            _ => return,
        };
        self.mbc7_eeprom_write(&mut ee, value);
        if let Mapper::Mbc7(m) = &mut self.mapper {
            m.state.eeprom = ee;
        }
    }
    pub(super) fn mbc7_eeprom_write(&mut self, ee: &mut Mbc7Eeprom, value: u8) {
        let di = value & 0x02 != 0;
        let clk = value & 0x40 != 0;
        let cs = value & 0x80 != 0;
        let rising_clk = clk && !ee.clk;
        let falling_cs = !cs && ee.cs;

        if rising_clk && cs {
            match ee.state {
                Mbc7EepromState::Idle => {
                    if di {
                        // Start bit.
                        ee.state = Mbc7EepromState::Command;
                        ee.sr = 0;
                        ee.sr_n = 0;
                    }
                }
                Mbc7EepromState::Command => {
                    ee.sr = (ee.sr << 1) | di as u16;
                    ee.sr_n += 1;
                    if ee.sr_n == 10 {
                        self.mbc7_eeprom_decode(ee);
                    }
                }
                Mbc7EepromState::Input => {
                    ee.sr = (ee.sr << 1) | di as u16;
                    ee.sr_n += 1;
                    if ee.sr_n == 16 {
                        ee.input = ee.sr;
                        ee.state = Mbc7EepromState::Pending;
                    }
                }
                Mbc7EepromState::Output => {
                    ee.do_line = ee.out & 0x8000 != 0;
                    ee.out <<= 1;
                    ee.out_n += 1;
                    if ee.out_n == 16 {
                        ee.state = Mbc7EepromState::Done;
                    }
                }
                Mbc7EepromState::Pending | Mbc7EepromState::Done => {}
            }
        }

        if falling_cs {
            if ee.state == Mbc7EepromState::Pending {
                self.mbc7_eeprom_program(ee);
            }
            // Any in-flight instruction is aborted by deselecting the chip.
            ee.state = Mbc7EepromState::Idle;
        }

        ee.di_line = di;
        ee.clk = clk;
        ee.cs = cs;
    }
    /// Decode a completed 10-bit instruction. The top 4 bits identify the
    /// operation; the low 7 bits are the word address for READ/WRITE/ERASE.
    pub(super) fn mbc7_eeprom_decode(&mut self, ee: &mut Mbc7Eeprom) {
        let cmd = ee.sr & 0x03FF;
        ee.command = cmd;
        match (cmd >> 6) & 0xF {
            0b1000..=0b1011 => {
                // READ: present the word MSB-first on subsequent rising edges.
                // DO drops to 0 immediately (the datasheet's dummy zero bit,
                // which does not consume a clock).
                ee.out = self.mbc7_eeprom_word((cmd & 0x7F) as usize);
                ee.out_n = 0;
                ee.do_line = false;
                ee.state = Mbc7EepromState::Output;
            }
            0b0100..=0b0111 | 0b0001 => {
                // WRITE / WRAL: 16 data bits follow.
                ee.sr = 0;
                ee.sr_n = 0;
                ee.state = Mbc7EepromState::Input;
            }
            0b1100..=0b1111 | 0b0010 => {
                // ERASE / ERAL: programs on CS fall.
                ee.state = Mbc7EepromState::Pending;
            }
            0b0011 => {
                ee.write_enabled = true;
                ee.state = Mbc7EepromState::Done;
            }
            0b0000 => {
                ee.write_enabled = false;
                ee.state = Mbc7EepromState::Done;
            }
            _ => unreachable!(),
        }
    }
    /// Execute a pending programming instruction at the CS falling edge. If
    /// erase/write is not enabled (no EWEN) the operation is silently dropped
    /// and DO keeps its previous level (no programming cycle ever starts).
    pub(super) fn mbc7_eeprom_program(&mut self, ee: &mut Mbc7Eeprom) {
        if !ee.write_enabled {
            return;
        }
        let cmd = ee.command;
        let addr = (cmd & 0x7F) as usize;
        let input = ee.input;
        match (cmd >> 6) & 0xF {
            0b0100..=0b0111 => self.mbc7_eeprom_set_word(addr, input),
            0b1100..=0b1111 => self.mbc7_eeprom_set_word(addr, 0xFFFF),
            0b0001 => {
                for a in 0..128 {
                    self.mbc7_eeprom_set_word(a, input);
                }
            }
            0b0010 => {
                for a in 0..128 {
                    self.mbc7_eeprom_set_word(a, 0xFFFF);
                }
            }
            _ => {}
        }
        // Programming modeled as instant: DO = RDY as soon as CS re-rises.
        ee.do_line = true;
    }
}

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc7 {
    pub ram_enabled: bool,
    pub state: Mbc7State,
}

impl Banking for Mbc7 {
    fn rom_bankn(&self, g: Geom) -> usize {
        (self.state.rom_bank as usize) % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0 // no banked RAM (serial EEPROM)
    }
}


// --- state ---------------------------------------------------------------

/// 93LC56 serial-EEPROM interface state for MBC7 (Pan Docs "MBC7"). The
/// EEPROM contents themselves live in `Cartridge::ram_data` (256 bytes =
/// 128 little-endian 16-bit words) so the existing battery-save plumbing
/// persists them; this struct only models the bit-banged serial link
/// exposed at the Ax8x register (bit0=DO, bit1=DI, bit6=CLK, bit7=CS).
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default, Debug)]
pub(super) enum Mbc7EepromState {
    /// CS low or waiting for the start bit (first 1 on DI while CS high).
    #[default]
    Idle,
    /// Collecting the 10 instruction bits (2-bit opcode + 8 payload bits).
    Command,
    /// Collecting the 16 data bits of a WRITE/WRAL instruction.
    Input,
    /// Shifting out the 16 data bits of a READ, MSB first.
    Output,
    /// Programming instruction fully received; the actual array write
    /// happens when CS falls (93LC56 datasheet: the internal programming
    /// cycle starts on the CS falling edge after the last data bit).
    Pending,
    /// Instruction finished; further clocks are ignored until CS falls.
    Done,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub(super) struct Mbc7Eeprom {
    // Last-written pin levels (readable back through Ax8x).
    pub(super) do_line: bool,
    pub(super) di_line: bool,
    pub(super) clk: bool,
    pub(super) cs: bool,
    // Set by EWEN, cleared by EWDS. Programming ops are silently dropped
    // while disabled (the power-on state).
    pub(super) write_enabled: bool,
    pub(super) state: Mbc7EepromState,
    // Shared input shift register for the Command/Input phases.
    pub(super) sr: u16,
    pub(super) sr_n: u8,
    // Latched 10-bit instruction once the Command phase completes.
    pub(super) command: u16,
    // Latched 16-bit data word once the Input phase completes.
    pub(super) input: u16,
    // Output shift register for READ.
    pub(super) out: u16,
    pub(super) out_n: u8,
}

impl Mbc7Eeprom {
    /// Pin read-back for the Ax8x register: CS<<7 | CLK<<6 | DI<<1 | DO.
    /// Bits 2-5 are not wired to the EEPROM and read 0.
    pub(super) fn pin_state(&self) -> u8 {
        ((self.cs as u8) << 7)
            | ((self.clk as u8) << 6)
            | ((self.di_line as u8) << 1)
            | (self.do_line as u8)
    }
}

/// MBC7 state. RAM-register access needs a TWO stage unlock: 0x0A to
/// 0x0000-0x1FFF (shared `ram_enabled`) AND exactly 0x40 to 0x4000-0x5FFF.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc7State {
    pub(super) ram_enabled2: bool,
    /// 8-bit ROM bank register; like MBC5, bank 0 is selectable at 0x4000-0x7FFF.
    pub(super) rom_bank: u8,
    /// Latched accelerometer sample, 16 bits per axis. Reads 0x8000 before the
    /// first latch and after an 0x55 erase; a real sample is centered ~0x81D0.
    pub(super) accel_x: u16,
    pub(super) accel_y: u16,
    /// A new 0xAA latch is only accepted after an 0x55 erase (Pan Docs: cannot
    /// re-latch without erasing first).
    pub(super) accel_latched: bool,
    pub(super) eeprom: Mbc7Eeprom,
}

impl Default for Mbc7State {
    fn default() -> Self {
        Self {
            ram_enabled2: false,
            rom_bank: 1,
            accel_x: 0x8000,
            accel_y: 0x8000,
            accel_latched: false,
            eeprom: Mbc7Eeprom::default(),
        }
    }
}


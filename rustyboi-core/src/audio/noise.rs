use serde::{Deserialize, Serialize};
use crate::audio::{NR41, NR42, NR43, NR44};
use crate::memory::mmio;
use crate::memory::Addressable;

#[derive(Clone, Serialize, Deserialize)]
pub struct Noise {
    // Sound channel registers
    nr41: u8, // Sound length
    nr42: u8, // Volume envelope
    nr43: u8, // Frequency and randomness
    nr44: u8, // Control
    
    // Internal state
    enabled: bool,
    length_counter: u8,
    volume: u8,
    volume_direction: bool,
    volume_timer: u8,
    frequency_timer: u16,
    lfsr: u16, // Linear feedback shift register
    length_enabled: bool,
    fs_step: u8,

    // Free-running 2 MHz cycle counter (Gambatte cycleCounter_), pushed by the
    // controller; drives the cc-based length expiry.
    #[serde(default)]
    cc: u32,
    // Absolute cc of length expiry (Gambatte `LengthCounter::counter_`).
    #[serde(default = "len_disabled")]
    len_counter: u32,
    #[serde(default)]
    len_cc: u32,
}

const LEN_DISABLED: u32 = 0xFFFF_FFFF;

fn len_disabled() -> u32 {
    LEN_DISABLED
}

impl Noise {
    pub fn new() -> Self {
        Noise {
            nr41: 0,
            nr42: 0,
            nr43: 0,
            nr44: 0,
            enabled: false,
            length_counter: 0,
            volume: 0,
            volume_direction: false,
            volume_timer: 0,
            frequency_timer: 0,
            lfsr: 0x7FFF,
            length_enabled: false,
            fs_step: 0,
            cc: 0,
            len_counter: LEN_DISABLED,
            len_cc: 0,
        }
    }

    pub fn set_cc(&mut self, cc: u32) {
        self.cc = cc;
    }

    pub fn set_len_cc(&mut self, cc: u32) {
        self.len_cc = cc;
    }

    pub fn len_expired(&self) -> bool {
        self.len_cc >= self.len_counter
    }

    pub fn set_fs_step(&mut self, step: u8) {
        self.fs_step = step;
    }

    const LEN_MASK: u16 = 0x3F;

    /// Gambatte `LengthCounter::event` for channel 4.
    pub fn length_event(&mut self) {
        self.len_counter = LEN_DISABLED;
        self.length_counter = 0;
        self.enabled = false;
    }


    /// Gambatte `LengthCounter::nr1Change` (channel 4 / NR41).
    fn len_nr1_change(&mut self, value: u8) {
        self.length_counter = ((!value as u16 & Self::LEN_MASK) + 1) as u8;
        self.len_counter = if self.nr44 & 0x40 != 0 {
            (((self.len_cc >> 13) + self.length_counter as u32) << 13).min(u32::MAX)
        } else {
            LEN_DISABLED
        };
    }

    /// Gambatte `LengthCounter::nr4Change` (channel 4 / NR44) length handling.
    fn len_nr4_change(&mut self, old_nr4: u8, new_nr4: u8) {
        if self.len_counter != LEN_DISABLED {
            self.length_counter =
                (self.len_counter >> 13).wrapping_sub(self.len_cc >> 13) as u8;
        }
        let mut dec: u8 = 0;
        if new_nr4 & 0x40 != 0 {
            dec = ((!self.len_cc >> 12) & 1) as u8;
            if old_nr4 & 0x40 == 0 && self.length_counter != 0 {
                self.length_counter -= dec;
                if self.length_counter == 0 {
                    self.enabled = false;
                }
            }
        }
        if new_nr4 & 0x80 != 0 && self.length_counter == 0 {
            self.length_counter = (Self::LEN_MASK as u8) + 1 - dec;
        }
        self.len_counter = if new_nr4 & 0x40 != 0 && self.length_counter != 0 {
            (((self.len_cc >> 13) + self.length_counter as u32) << 13).min(u32::MAX)
        } else {
            LEN_DISABLED
        };
    }

    pub fn step(&mut self, _mmio: &mut mmio::Mmio) {
        if !self.enabled {
            return;
        }

        // Update frequency timer
        if self.frequency_timer > 0 {
            self.frequency_timer -= 1;
        } else {
            self.frequency_timer = self.get_frequency_timer_period();
            self.step_lfsr();
        }
    }

    pub fn step_frame_sequencer(&mut self, step: u8) {
        if !self.enabled {
            return;
        }

        // Length is now a cc-driven absolute expiry event (see `length_event`).
        // Volume envelope (step 7)
        if step == 7 {
            self.step_volume_envelope();
        }
    }

    fn step_volume_envelope(&mut self) {
        if self.volume_timer > 0 {
            self.volume_timer -= 1;
            if self.volume_timer == 0 {
                let envelope_period = self.get_envelope_period();
                if envelope_period > 0 {
                    self.volume_timer = envelope_period;
                    if self.volume_direction && self.volume < 15 {
                        self.volume += 1;
                    } else if !self.volume_direction && self.volume > 0 {
                        self.volume -= 1;
                    }
                }
            }
        }
    }

    fn step_lfsr(&mut self) {
        let bit0 = self.lfsr & 0x01;
        let bit1 = (self.lfsr >> 1) & 0x01;
        let result = bit0 ^ bit1;
        
        self.lfsr >>= 1;
        self.lfsr |= result << 14;
        
        // 7-bit mode
        if self.get_width_mode() {
            self.lfsr &= 0x7F7F;
            self.lfsr |= result << 6;
        }
    }

    fn get_frequency_timer_period(&self) -> u16 {
        let divisor_code = self.nr43 & 0x07;
        let divisor = if divisor_code == 0 { 8 } else { 16 * divisor_code as u16 };
        let shift = (self.nr43 >> 4) & 0x0F;
        divisor << shift
    }

    fn get_width_mode(&self) -> bool {
        (self.nr43 >> 3) & 0x01 != 0
    }


    fn get_envelope_initial_volume(&self) -> u8 {
        (self.nr42 >> 4) & 0x0F
    }

    fn get_envelope_direction(&self) -> bool {
        (self.nr42 >> 3) & 0x01 != 0
    }

    fn get_envelope_period(&self) -> u8 {
        self.nr42 & 0x07
    }

    fn write_nrx4(&mut self, value: u8) {
        let trigger = (value >> 7) & 0x01 != 0;
        let old_nr4 = self.nr44;

        self.len_nr4_change(old_nr4, value);
        self.length_enabled = (value >> 6) & 0x01 != 0;
        self.nr44 = value;

        if trigger {
            self.trigger();
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;

        // Length reload handled in `len_nr4_change` (Gambatte folds it in).

        // Volume envelope
        self.volume = self.get_envelope_initial_volume();
        self.volume_direction = self.get_envelope_direction();
        self.volume_timer = self.get_envelope_period();
        
        // LFSR
        self.lfsr = 0x7FFF;
        self.frequency_timer = self.get_frequency_timer_period();
        
        // If DAC is disabled, disable channel
        if self.get_envelope_initial_volume() == 0 && !self.get_envelope_direction() {
            self.enabled = false;
        }
    }

    pub fn get_output(&self) -> f32 {
        if !self.enabled || self.volume == 0 {
            return 0.0;
        }

        // Output is inverted LFSR bit 0
        let output_bit = (!self.lfsr) & 0x01;
        
        if output_bit == 1 {
            (self.volume as f32) / 15.0
        } else {
            0.0
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl Addressable for Noise {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR41..=NR44 => {
                match addr {
                    NR41 => 0xFF, // Write-only
                    NR42 => self.nr42,
                    NR43 => self.nr43,
                    NR44 => self.nr44 | 0xBF, // Only bit 6 readable
                    _ => 0xFF,
                }
            }
            _ => panic!("Invalid address for Noise: {:#X}", addr)
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR41..=NR44 => {
                match addr {
                    NR41 => {
                        self.nr41 = value;
                        self.len_nr1_change(value);
                    }
                    NR42 => {
                        self.nr42 = value;
                        if self.get_envelope_initial_volume() == 0 && !self.get_envelope_direction() {
                            self.enabled = false;
                        }
                    }
                    NR43 => {
                        self.nr43 = value;
                    }
                    NR44 => {
                        self.write_nrx4(value);
                    }
                    _ => {}
                }
            }
            _ => panic!("Invalid address for Noise: {:#X}", addr)
        }
    }
}

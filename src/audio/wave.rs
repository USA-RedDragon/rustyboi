use serde::{Deserialize, Serialize};
use crate::audio::{NR30, NR31, NR32, NR33, NR34, WAV_START, WAV_END};
use crate::memory::mmio;
use crate::memory::Addressable;

#[derive(Clone, Serialize, Deserialize)]
pub struct Wave {
    // Sound channel registers
    nr30: u8, // DAC enable
    nr31: u8, // Sound length
    nr32: u8, // Output level
    nr33: u8, // Period low
    nr34: u8, // Period high and control
    
    // Wave pattern RAM (16 bytes, 32 4-bit samples)
    wave_ram: [u8; 16],
    
    // Internal state
    enabled: bool,
    dac_enabled: bool,
    length_counter: u16,
    frequency: u16,
    frequency_timer: u16,
    position_counter: u8, // 0-31, current position in wave pattern
}

impl Wave {
    pub fn new() -> Self {
        Wave {
            nr30: 0,
            nr31: 0,
            nr32: 0,
            nr33: 0,
            nr34: 0,
            wave_ram: [0; 16],
            enabled: false,
            dac_enabled: false,
            length_counter: 0,
            frequency: 0,
            frequency_timer: 0,
            position_counter: 0,
        }
    }

    pub fn step(&mut self, _mmio: &mut mmio::MMIO) {
        if !self.enabled || !self.dac_enabled {
            return;
        }

        // Update frequency timer
        if self.frequency_timer > 0 {
            self.frequency_timer -= 1;
        } else {
            self.frequency_timer = (2048 - self.frequency) * 2;
            self.position_counter = (self.position_counter + 1) % 32;
        }
    }

    pub fn step_frame_sequencer(&mut self, step: u8) {
        if !self.enabled {
            return;
        }

        // Length counter (steps 0, 2, 4, 6)
        if step % 2 == 0 {
            self.step_length_counter();
        }
    }

    fn step_length_counter(&mut self) {
        if self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }

    fn get_length_load(&self) -> u8 {
        self.nr31
    }

    fn get_output_level(&self) -> u8 {
        (self.nr32 >> 5) & 0x03
    }

    fn is_length_enabled(&self) -> bool {
        (self.nr34 >> 6) & 0x01 != 0
    }

    fn trigger(&mut self) {
        self.enabled = true;
        
        // Length counter
        if self.length_counter == 0 {
            self.length_counter = 256;
        }
        
        // Update frequency
        self.frequency = ((self.nr34 as u16 & 0x07) << 8) | self.nr33 as u16;
        self.frequency_timer = (2048 - self.frequency) * 2;
        
        // Reset position
        self.position_counter = 0;
        
        // If DAC is disabled, disable channel
        if !self.dac_enabled {
            self.enabled = false;
        }
    }

    pub fn get_output(&self) -> f32 {
        if !self.enabled || !self.dac_enabled {
            return 0.0;
        }

        // Get the current sample from wave RAM
        let byte_index = (self.position_counter / 2) as usize;
        let sample = if self.position_counter % 2 == 0 {
            // High nibble
            (self.wave_ram[byte_index] >> 4) & 0x0F
        } else {
            // Low nibble
            self.wave_ram[byte_index] & 0x0F
        };

        // Apply output level shift
        let output_level = self.get_output_level();
        let shifted_sample = if output_level == 0 {
            0 // Mute
        } else {
            sample >> (output_level - 1)
        };

        (shifted_sample as f32) / 15.0
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn read_wave_ram(&self, addr: u16) -> u8 {
        let index = (addr - WAV_START) as usize;
        if index < 16 {
            self.wave_ram[index]
        } else {
            0xFF
        }
    }

    pub fn write_wave_ram(&mut self, addr: u16, value: u8) {
        let index = (addr - WAV_START) as usize;
        if index < 16 {
            self.wave_ram[index] = value;
        }
    }
}

impl Addressable for Wave {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR30..=NR34 => {
                match addr {
                    NR30 => self.nr30 | 0x7F, // Only bit 7 readable
                    NR31 => 0xFF, // Write-only
                    NR32 => self.nr32 | 0x9F, // Only bits 5-6 readable
                    NR33 => 0xFF, // Write-only
                    NR34 => self.nr34 | 0xBF, // Only bit 6 readable
                    _ => 0xFF,
                }
            }
            WAV_START..=WAV_END => {
                // Wave pattern RAM
                self.read_wave_ram(addr)
            }
            _ => panic!("Invalid address for Wave: {:#X}", addr)
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR30..=NR34 => {
                match addr {
                    NR30 => {
                        self.nr30 = value;
                        self.dac_enabled = (value >> 7) & 0x01 != 0;
                        if !self.dac_enabled {
                            self.enabled = false;
                        }
                    }
                    NR31 => {
                        self.nr31 = value;
                        self.length_counter = 256 - self.get_length_load() as u16;
                    }
                    NR32 => {
                        self.nr32 = value;
                    }
                    NR33 => {
                        self.nr33 = value;
                    }
                    NR34 => {
                        let trigger = (value >> 7) & 0x01 != 0;
                        self.nr34 = value;
                        
                        if trigger {
                            self.trigger();
                        }
                    }
                    _ => {}
                }
            }
            WAV_START..=WAV_END => {
                // Wave pattern RAM
                self.write_wave_ram(addr, value);
            }
            _ => panic!("Invalid address for Wave: {:#X}", addr)
        }
    }
}

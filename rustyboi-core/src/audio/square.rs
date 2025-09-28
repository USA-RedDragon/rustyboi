use serde::{Deserialize, Serialize};
use crate::audio::{NR10, NR11, NR12, NR13, NR14, NR21, NR22, NR23, NR24};
use crate::memory::mmio;
use crate::memory::Addressable;

#[derive(Clone, Serialize, Deserialize)]
pub struct SquareWave {
    channel1: bool,
    
    // Sound channel registers
    nr10: u8, // Channel 1 sweep register (frequency sweep)
    nr11: u8, // Sound length/wave pattern duty
    nr12: u8, // Volume envelope
    nr13: u8, // Period low
    nr14: u8, // Period high and control
    
    nr21: u8, // Channel 2 sound length/wave pattern duty
    nr22: u8, // Channel 2 volume envelope
    nr23: u8, // Channel 2 period low  
    nr24: u8, // Channel 2 period high and control
    
    // Internal state
    enabled: bool,
    length_counter: u8,
    volume: u8,
    volume_direction: bool, // true = increase, false = decrease
    volume_timer: u8,
    frequency: u16,
    frequency_timer: u16,
    duty_position: u8,
    
    // Frequency sweep (Channel 1 only)
    sweep_enabled: bool,
    sweep_timer: u8,
    sweep_shadow_frequency: u16,
}

impl SquareWave {
    pub fn new(channel1: bool) -> Self {
        SquareWave {
            channel1,
            nr10: 0,
            nr11: 0,
            nr12: 0,
            nr13: 0,
            nr14: 0,
            nr21: 0,
            nr22: 0,
            nr23: 0,
            nr24: 0,
            enabled: false,
            length_counter: 0,
            volume: 0,
            volume_direction: false,
            volume_timer: 0,
            frequency: 0,
            frequency_timer: 0,
            duty_position: 0,
            sweep_enabled: false,
            sweep_timer: 0,
            sweep_shadow_frequency: 0,
        }
    }

    pub fn step(&mut self, _mmio: &mut mmio::Mmio) {
        if !self.enabled {
            return;
        }

        // Update frequency timer
        if self.frequency_timer > 0 {
            self.frequency_timer -= 1;
        } else {
            self.frequency_timer = (2048 - self.frequency) * 4;
            self.duty_position = (self.duty_position + 1) % 8;
        }
    }

    pub fn step_frame_sequencer(&mut self, step: u8) {
        if !self.enabled {
            return;
        }

        // Length counter (steps 0, 2, 4, 6)
        if step.is_multiple_of(2) {
            self.step_length_counter();
        }

        // Volume envelope (step 7)
        if step == 7 {
            self.step_volume_envelope();
        }

        // Frequency sweep (steps 2, 6) - Channel 1 only
        if self.channel1 && (step == 2 || step == 6) {
            self.step_frequency_sweep();
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

    fn step_frequency_sweep(&mut self) {
        if !self.channel1 || !self.sweep_enabled {
            return;
        }

        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
            if self.sweep_timer == 0 {
                let sweep_period = self.get_sweep_period();
                if sweep_period > 0 {
                    self.sweep_timer = sweep_period;
                    let new_frequency = self.calculate_sweep_frequency();
                    if new_frequency <= 2047 && self.get_sweep_shift() > 0 {
                        self.sweep_shadow_frequency = new_frequency;
                        self.frequency = new_frequency;
                        self.update_frequency_registers();
                        
                        // Check overflow again
                        let _ = self.calculate_sweep_frequency();
                    }
                }
            }
        }
    }

    fn calculate_sweep_frequency(&mut self) -> u16 {
        let shift = self.get_sweep_shift();
        let direction = self.get_sweep_direction();
        
        let offset = self.sweep_shadow_frequency >> shift;
        if direction {
            // Decrease frequency
            self.sweep_shadow_frequency.saturating_sub(offset)
        } else {
            // Increase frequency
            let new_freq = self.sweep_shadow_frequency + offset;
            if new_freq > 2047 {
                self.enabled = false; // Disable channel on overflow
            }
            new_freq
        }
    }

    fn update_frequency_registers(&mut self) {
        if self.channel1 {
            self.nr13 = (self.frequency & 0xFF) as u8;
            self.nr14 = (self.nr14 & 0xF8) | ((self.frequency >> 8) & 0x07) as u8;
        } else {
            self.nr23 = (self.frequency & 0xFF) as u8;
            self.nr24 = (self.nr24 & 0xF8) | ((self.frequency >> 8) & 0x07) as u8;
        }
    }

    fn get_sweep_period(&self) -> u8 {
        (self.nr10 >> 4) & 0x07
    }

    fn get_sweep_direction(&self) -> bool {
        (self.nr10 >> 3) & 0x01 != 0
    }

    fn get_sweep_shift(&self) -> u8 {
        self.nr10 & 0x07
    }

    fn get_duty_cycle(&self) -> u8 {
        if self.channel1 {
            (self.nr11 >> 6) & 0x03
        } else {
            (self.nr21 >> 6) & 0x03
        }
    }

    fn get_length_load(&self) -> u8 {
        if self.channel1 {
            self.nr11 & 0x3F
        } else {
            self.nr21 & 0x3F
        }
    }

    fn get_envelope_initial_volume(&self) -> u8 {
        if self.channel1 {
            (self.nr12 >> 4) & 0x0F
        } else {
            (self.nr22 >> 4) & 0x0F
        }
    }

    fn get_envelope_direction(&self) -> bool {
        if self.channel1 {
            (self.nr12 >> 3) & 0x01 != 0
        } else {
            (self.nr22 >> 3) & 0x01 != 0
        }
    }

    fn get_envelope_period(&self) -> u8 {
        if self.channel1 {
            self.nr12 & 0x07
        } else {
            self.nr22 & 0x07
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;
        
        // Length counter
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        
        // Volume envelope
        self.volume = self.get_envelope_initial_volume();
        self.volume_direction = self.get_envelope_direction();
        self.volume_timer = self.get_envelope_period();
        
        // Update frequency
        if self.channel1 {
            self.frequency = ((self.nr14 as u16 & 0x07) << 8) | self.nr13 as u16;
        } else {
            self.frequency = ((self.nr24 as u16 & 0x07) << 8) | self.nr23 as u16;
        }
        self.frequency_timer = (2048 - self.frequency) * 4;
        
        // Frequency sweep (Channel 1 only)
        if self.channel1 {
            self.sweep_shadow_frequency = self.frequency;
            self.sweep_timer = self.get_sweep_period();
            if self.sweep_timer == 0 {
                self.sweep_timer = 8;
            }
            self.sweep_enabled = self.get_sweep_period() > 0 || self.get_sweep_shift() > 0;
            
            if self.get_sweep_shift() > 0 {
                let _ = self.calculate_sweep_frequency();
            }
        }
        
        // If DAC is disabled, disable channel
        if self.get_envelope_initial_volume() == 0 && !self.get_envelope_direction() {
            self.enabled = false;
        }
    }

    pub fn get_output(&self) -> f32 {
        if !self.enabled || self.volume == 0 {
            return 0.0;
        }

        // Duty cycle patterns
        const DUTY_PATTERNS: [[u8; 8]; 4] = [
            [0, 0, 0, 0, 0, 0, 0, 1], // 12.5%
            [1, 0, 0, 0, 0, 0, 0, 1], // 25%
            [1, 0, 0, 0, 0, 1, 1, 1], // 50%
            [0, 1, 1, 1, 1, 1, 1, 0], // 75%
        ];

        let duty_cycle = self.get_duty_cycle() as usize;
        let output_bit = DUTY_PATTERNS[duty_cycle][self.duty_position as usize];
        
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

impl Addressable for SquareWave {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR10..=NR14 => {
                if self.channel1 {
                    match addr {
                        NR10 => self.nr10 | 0x80, // Top bit always set
                        NR11 => self.nr11 | 0x3F, // Bottom 6 bits always set
                        NR12 => self.nr12,
                        NR13 => 0xFF, // Write-only
                        NR14 => self.nr14 | 0xBF, // Only bit 6 readable
                        _ => 0xFF,
                    }
                } else {
                    panic!("Invalid read from Channel 2 SquareWave: {:#X}", addr);
                }
            }
            NR21..=NR24 => {
                if !self.channel1 {
                    match addr {
                        NR21 => self.nr21 | 0x3F, // Bottom 6 bits always set
                        NR22 => self.nr22,
                        NR23 => 0xFF, // Write-only
                        NR24 => self.nr24 | 0xBF, // Only bit 6 readable
                        _ => 0xFF,
                    }
                } else {
                    panic!("Invalid read from Channel 1 SquareWave: {:#X}", addr);
                }
            }
            _ => panic!("Invalid address for SquareWave: {:#X}", addr)
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR10..=NR14 => {
                if self.channel1 {
                    match addr {
                        NR10 => {
                            self.nr10 = value;
                        }
                        NR11 => {
                            self.nr11 = value;
                            self.length_counter = 64 - self.get_length_load();
                        }
                        NR12 => {
                            self.nr12 = value;
                            // If DAC is disabled, disable channel
                            if self.get_envelope_initial_volume() == 0 && !self.get_envelope_direction() {
                                self.enabled = false;
                            }
                        }
                        NR13 => {
                            self.nr13 = value;
                        }
                        NR14 => {
                            let trigger = (value >> 7) & 0x01 != 0;
                            self.nr14 = value;
                            
                            if trigger {
                                self.trigger();
                            }
                        }
                        _ => {}
                    }
                } else {
                    panic!("Invalid write to Channel 2 SquareWave: {:#X}", addr);
                }
            }
            NR21..=NR24 => {
                if !self.channel1 {
                    match addr {
                        NR21 => {
                            self.nr21 = value;
                            self.length_counter = 64 - self.get_length_load();
                        }
                        NR22 => {
                            self.nr22 = value;
                            // If DAC is disabled, disable channel
                            if self.get_envelope_initial_volume() == 0 && !self.get_envelope_direction() {
                                self.enabled = false;
                            }
                        }
                        NR23 => {
                            self.nr23 = value;
                        }
                        NR24 => {
                            let trigger = (value >> 7) & 0x01 != 0;
                            self.nr24 = value;
                            
                            if trigger {
                                self.trigger();
                            }
                        }
                        _ => {}
                    }
                } else {
                    panic!("Invalid write to Channel 1 SquareWave: {:#X}", addr);
                }
            }
            _ => panic!("Invalid address for SquareWave: {:#X}", addr)
        }
    }
}

use serde::{Deserialize, Serialize};
use crate::audio::{wave, square, noise};
use crate::memory::mmio;
use crate::memory::Addressable;

pub const NR10: u16 = 0xFF10; // Channel 1 sweep register
pub const NR11: u16 = 0xFF11; // Channel 1 sound length/wave pattern duty
pub const NR12: u16 = 0xFF12; // Channel 1 volume and envelope
pub const NR13: u16 = 0xFF13; // Channel 1 period low
pub const NR14: u16 = 0xFF14; // Channel 1 period high and control

pub const NR21: u16 = 0xFF16; // Channel 2 sound length/wave pattern duty
pub const NR22: u16 = 0xFF17; // Channel 2 volume and envelope
pub const NR23: u16 = 0xFF18; // Channel 2 period low
pub const NR24: u16 = 0xFF19; // Channel 2 period high and control

pub const NR30: u16 = 0xFF1A; // Channel 3 dac enable
pub const NR31: u16 = 0xFF1B; // Channel 3 sound length
pub const NR32: u16 = 0xFF1C; // Channel 3 output level
pub const NR33: u16 = 0xFF1D; // Channel 3 period low
pub const NR34: u16 = 0xFF1E; // Channel 3 period high and control

pub const NR41: u16 = 0xFF20; // Channel 4 sound length
pub const NR42: u16 = 0xFF21; // Channel 4 volume and envelope
pub const NR43: u16 = 0xFF22; // Channel 4 frequency and randomness
pub const NR44: u16 = 0xFF23; // Channel 4 control

pub const NR50: u16 = 0xFF24; // master volume, VIN panning
pub const NR51: u16 = 0xFF25; // Sound panning
pub const NR52: u16 = 0xFF26; // Audio master control

pub const WAV_START: u16 = 0xFF30; // Channel 3 wave pattern RAM start
pub const WAV_LENGTH: usize = 16; // Channel 3 wave pattern RAM length
pub const WAV_END: u16 = WAV_START + WAV_LENGTH as u16 - 1; // Channel 3 wave pattern RAM end

#[derive(Clone, Serialize, Deserialize)]
pub struct Audio {
    channel1: square::SquareWave,
    channel2: square::SquareWave,
    channel3: wave::Wave,
    channel4: noise::Noise,
    
    // Master control registers
    nr50: u8, // Master volume and VIN panning
    nr51: u8, // Sound panning
    nr52: u8, // Master control/status
    
    // Frame sequencer
    frame_sequencer_step: u8,
    frame_sequencer_timer: u16,
    
    // Audio enabled flag
    audio_enabled: bool,
    
    // Sample generation timing
    fractional_cycles: f32,
    
    // Reusable sample buffer to avoid allocations
    sample_buffer: Vec<(f32, f32)>,
}

impl Audio {
    pub fn new() -> Self {
        Audio {
            channel1: square::SquareWave::new(true),
            channel2: square::SquareWave::new(false),
            channel3: wave::Wave::new(),
            channel4: noise::Noise::new(),
            nr50: 0,
            nr51: 0,
            nr52: 0,
            frame_sequencer_step: 0,
            frame_sequencer_timer: 8192,
            audio_enabled: false,
            fractional_cycles: 0.0,
            sample_buffer: Vec::with_capacity(2048), // Pre-allocate for common frame sizes
        }
    }

    pub fn step(&mut self) {
        if !self.audio_enabled {
            return;
        }

        // Step individual channels without mmio (they don't use it anyway)
        self.channel1.step();
        self.channel2.step();
        self.channel3.step();
        self.channel4.step();

        // Step frame sequencer
        self.frame_sequencer_timer -= 1;
        if self.frame_sequencer_timer == 0 {
            self.frame_sequencer_timer = 8192; // 512 Hz
            self.step_frame_sequencer();
        }
    }

    fn step_frame_sequencer(&mut self) {
        self.channel1.step_frame_sequencer(self.frame_sequencer_step);
        self.channel2.step_frame_sequencer(self.frame_sequencer_step);
        self.channel3.step_frame_sequencer(self.frame_sequencer_step);
        self.channel4.step_frame_sequencer(self.frame_sequencer_step);
        
        self.frame_sequencer_step = (self.frame_sequencer_step + 1) % 8;
    }

    pub fn get_master_volume_left(&self) -> u8 {
        (self.nr50 >> 4) & 0x07
    }

    pub fn get_master_volume_right(&self) -> u8 {
        self.nr50 & 0x07
    }

    pub fn is_channel_left_enabled(&self, channel: u8) -> bool {
        match channel {
            1 => (self.nr51 >> 4) & 0x01 != 0,
            2 => (self.nr51 >> 5) & 0x01 != 0,
            3 => (self.nr51 >> 6) & 0x01 != 0,
            4 => (self.nr51 >> 7) & 0x01 != 0,
            _ => false,
        }
    }

    pub fn is_channel_right_enabled(&self, channel: u8) -> bool {
        match channel {
            1 => self.nr51 & 0x01 != 0,
            2 => (self.nr51 >> 1) & 0x01 != 0,
            3 => (self.nr51 >> 2) & 0x01 != 0,
            4 => (self.nr51 >> 3) & 0x01 != 0,
            _ => false,
        }
    }

    pub fn get_mixed_output(&self) -> (f32, f32) {
        if !self.audio_enabled {
            return (0.0, 0.0);
        }

        let ch1_output = self.channel1.get_output();
        let ch2_output = self.channel2.get_output();
        let ch3_output = self.channel3.get_output();
        let ch4_output = self.channel4.get_output();

        let mut left_mix = 0.0;
        let mut right_mix = 0.0;

        if self.is_channel_left_enabled(1) {
            left_mix += ch1_output;
        }
        if self.is_channel_left_enabled(2) {
            left_mix += ch2_output;
        }
        if self.is_channel_left_enabled(3) {
            left_mix += ch3_output;
        }
        if self.is_channel_left_enabled(4) {
            left_mix += ch4_output;
        }

        if self.is_channel_right_enabled(1) {
            right_mix += ch1_output;
        }
        if self.is_channel_right_enabled(2) {
            right_mix += ch2_output;
        }
        if self.is_channel_right_enabled(3) {
            right_mix += ch3_output;
        }
        if self.is_channel_right_enabled(4) {
            right_mix += ch4_output;
        }

        // Apply master volume
        left_mix *= (self.get_master_volume_left() as f32 + 1.0) / 8.0;
        right_mix *= (self.get_master_volume_right() as f32 + 1.0) / 8.0;

        // Divide by 4 to normalize since we're summing 4 channels
        (left_mix / 4.0, right_mix / 4.0)
    }

    pub fn generate_samples(&mut self, cpu_cycles: u32) -> Vec<(f32, f32)> {
        self.sample_buffer.clear();
        
        // Game Boy audio runs at ~4.194 MHz (same as CPU)
        // We want to output at 44.1 kHz, so we need to downsample
        // 4194304 / 44100 ≈ 95.1 cycles per sample
        const CYCLES_PER_SAMPLE: f32 = 4194304.0 / 44100.0;
        
        self.fractional_cycles += cpu_cycles as f32;
        
        while self.fractional_cycles >= CYCLES_PER_SAMPLE {
            self.step();
            
            let sample = self.get_mixed_output();
            self.sample_buffer.push(sample);
            
            self.fractional_cycles -= CYCLES_PER_SAMPLE;
        }
        
        self.sample_buffer.clone()
    }
}

impl Addressable for Audio {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR10..=NR14 => self.channel1.read(addr),
            NR21..=NR24 => self.channel2.read(addr),
            NR30..=NR34 => self.channel3.read(addr),
            NR41..=NR44 => self.channel4.read(addr),
            NR50 => self.nr50,
            NR51 => self.nr51,
            NR52 => {
                let mut value = self.nr52 & 0x80; // Preserve audio enabled bit
                
                // Set channel status bits (read-only)
                if self.channel1.is_enabled() { value |= 0x01; }
                if self.channel2.is_enabled() { value |= 0x02; }
                if self.channel3.is_enabled() { value |= 0x04; }
                if self.channel4.is_enabled() { value |= 0x08; }
                
                value | 0x70 // Bits 4-6 always read as 1
            }
            WAV_START..=WAV_END => self.channel3.read(addr),
            _ => panic!("Invalid address for Audio: {:#X}", addr)
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR10..=NR14 => {
                if self.audio_enabled {
                    self.channel1.write(addr, value)
                }
            },
            NR21..=NR24 => {
                if self.audio_enabled {
                    self.channel2.write(addr, value)
                }
            },
            NR30..=NR34 => {
                if self.audio_enabled {
                    self.channel3.write(addr, value)
                }
            },
            NR41..=NR44 => {
                if self.audio_enabled {
                    self.channel4.write(addr, value)
                }
            },
            NR50 => {
                if self.audio_enabled {
                    self.nr50 = value;
                }
            },
            NR51 => {
                if self.audio_enabled {
                    self.nr51 = value;
                }
            },
            NR52 => {
                let was_enabled = self.audio_enabled;
                self.audio_enabled = (value >> 7) & 0x01 != 0;
                self.nr52 = value;
                
                // If audio was disabled, reset all registers
                if was_enabled && !self.audio_enabled {
                    *self = Audio::new();
                    self.nr52 = value; // Preserve the written value
                }
            },
            WAV_START..=WAV_END => {
                // Wave RAM can be accessed even when audio is disabled
                self.channel3.write(addr, value);
            },
            _ => panic!("Invalid address for Audio: {:#X}", addr)
        }
    }
}

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
}

impl Audio {
    pub fn new() -> Self {
        Audio {
            channel1: square::SquareWave::new(true),
            channel2: square::SquareWave::new(false),
            channel3: wave::Wave::new(),
            channel4: noise::Noise::new(),
        }
    }

    pub fn step(&mut self, mmio: &mut mmio::MMIO) {
        self.channel1.step(mmio);
        self.channel2.step(mmio);
        self.channel3.step(mmio);
        self.channel4.step(mmio);
    }
}

impl Addressable for Audio {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR10..=NR14 => self.channel1.read(addr),
            NR21..=NR24 => self.channel2.read(addr),
            NR30..=NR34 => self.channel3.read(addr),
            NR41..=NR44 => self.channel4.read(addr),
            NR50..=NR52 => 0xFF, // TODO
            WAV_START..=WAV_END => 0xFF, // TODO
            _ => panic!("Invalid address for Audio: {:#X}", addr)
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR10..=NR14 => self.channel1.write(addr, value),
            NR21..=NR24 => self.channel2.write(addr, value),
            NR30..=NR34 => self.channel3.write(addr, value),
            NR41..=NR44 => self.channel4.write(addr, value),
            NR50..=NR52 => {}, // TODO
            WAV_START..=WAV_END => {}, // TODO
            _ => panic!("Invalid address for Audio: {:#X}", addr)
        }
    }
}

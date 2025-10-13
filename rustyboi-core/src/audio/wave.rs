use serde::{Deserialize, Serialize};
use crate::audio::{NR30, NR31, NR32, NR33, NR34, WAV_START, WAV_END};
use crate::memory::mmio;
use crate::memory::Addressable;

const COUNTER_DISABLED: u32 = 0xFFFF_FFFF;

fn to_period(nr3: u8, nr4: u8) -> u32 {
    0x800 - (((nr4 as u32) << 8 & 0x700) | nr3 as u32)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Wave {
    nr30: u8, // DAC enable
    nr31: u8, // Sound length
    nr32: u8, // Output level
    nr33: u8, // Period low
    nr34: u8, // Period high and control

    wave_ram: [u8; 16],

    // Length counter (FS-step model, DIV-locked via clock_frame_sequencer).
    enabled: bool,
    length_counter: u16,
    length_enabled: bool,
    fs_step: u8,

    // Free-running 2 MHz cycle counter (Gambatte cycleCounter_), pushed by the
    // controller. Channel 3's wave fetch is modelled cc-based per channel3.cpp.
    #[serde(default)]
    cc: u32,

    // Wave fetch timing (channel3.cpp waveCounter_/lastReadTime_/wavePos_).
    #[serde(default = "disabled")]
    wave_counter: u32,
    #[serde(default)]
    last_read_time: u32,
    #[serde(default)]
    wave_pos: u8, // 0..63 (2 * 16 nibbles)
    #[serde(default)]
    sample_buf: u8,

    // master_: DAC on and channel triggered (drives the wave fetch / read gate).
    #[serde(default)]
    master: bool,
    #[serde(default)]
    dac_enabled: bool,

    cgb: bool,
    #[serde(default)]
    ds: bool,
}

fn disabled() -> u32 {
    COUNTER_DISABLED
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
            length_counter: 0,
            length_enabled: false,
            fs_step: 0,
            cc: 0,
            wave_counter: COUNTER_DISABLED,
            last_read_time: 0,
            wave_pos: 0,
            sample_buf: 0,
            master: false,
            dac_enabled: false,
            cgb: false,
            ds: false,
        }
    }

    pub fn set_cc(&mut self, cc: u32) {
        self.cc = cc;
    }

    /// Gambatte Channel3::resetCc: shift lastReadTime_ and waveCounter_ back by
    /// the cc delta caused by a DIV write.
    pub fn reset_cc(&mut self, delta: u32) {
        self.last_read_time = self.last_read_time.wrapping_sub(delta);
        if self.wave_counter != COUNTER_DISABLED {
            self.wave_counter = self.wave_counter.wrapping_sub(delta);
        }
    }

    pub fn set_fs_step(&mut self, step: u8) {
        self.fs_step = step;
    }

    fn length_extra_clock_due(&self) -> bool {
        (self.fs_step % 2) == 1
    }

    fn period(&self) -> u32 {
        // Our cycle counter advances at the single-speed rate per CPU M-cycle
        // even in double speed, so the wave fetch period (in those cc) doubles
        // to keep the fetch cadence aligned to the CPU.
        to_period(self.nr33, self.nr34) << (self.ds as u32)
    }

    /// channel3.cpp updateWaveCounter.
    fn update_wave_counter(&mut self) {
        let cc = self.cc;
        if self.wave_counter != COUNTER_DISABLED && cc >= self.wave_counter {
            let period = self.period();
            let periods = (cc - self.wave_counter) / period;
            self.last_read_time = self.wave_counter + periods * period;
            self.wave_counter = self.last_read_time + period;
            self.wave_pos = ((self.wave_pos as u32 + periods + 1) % 32) as u8;
            self.sample_buf = self.wave_ram[(self.wave_pos / 2) as usize];
        }
    }

    pub fn step(&mut self, _mmio: &mut mmio::Mmio) {
        self.cgb = _mmio.is_cgb_features_enabled();
        self.ds = _mmio.is_double_speed_mode();
        if self.master {
            self.update_wave_counter();
        }
    }

    /// Advance the wave fetch counter to the current cc for the CPU read path.
    pub fn sync_for_read(&mut self) {
        if self.master {
            self.update_wave_counter();
        }
    }

    pub fn step_frame_sequencer(&mut self, step: u8) {
        if !self.enabled {
            return;
        }
        if step.is_multiple_of(2) && self.length_enabled {
            self.step_length_counter();
        }
    }

    fn step_length_counter(&mut self) {
        if self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
                self.master = false;
                self.wave_counter = COUNTER_DISABLED;
            }
        }
    }

    fn get_output_level(&self) -> u8 {
        (self.nr32 >> 5) & 0x03
    }

    fn write_nrx4(&mut self, value: u8) {
        let trigger = (value >> 7) & 0x01 != 0;
        let new_length_enabled = (value >> 6) & 0x01 != 0;
        let was_length_enabled = self.length_enabled;

        if new_length_enabled
            && !was_length_enabled
            && self.length_extra_clock_due()
            && self.length_counter > 0
        {
            self.length_counter -= 1;
            if self.length_counter == 0 && !trigger {
                self.enabled = false;
                self.master = false;
                self.wave_counter = COUNTER_DISABLED;
            }
        }

        self.length_enabled = new_length_enabled;
        self.nr34 = value & !0x80;

        if trigger {
            self.trigger();
        }
    }

    /// channel3.cpp setNr4 trigger (DAC-gated) plus the FS-step length reload.
    fn trigger(&mut self) {
        self.enabled = true;

        if self.length_counter == 0 {
            self.length_counter = 256;
            if self.length_enabled && self.length_extra_clock_due() {
                self.length_counter -= 1;
            }
        }

        if self.dac_enabled {
            // DMG wave-RAM corruption when triggering during an active fetch.
            if self.wave_counter == self.cc.wrapping_add(1) {
                self.sample_buf = self.wave_ram[0];
                if !self.cgb {
                    let pos = ((self.wave_pos as usize + 1) / 2) % 16;
                    if pos < 4 {
                        self.wave_ram[0] = self.wave_ram[pos];
                    } else {
                        let base = pos & !3;
                        let copy = [
                            self.wave_ram[base],
                            self.wave_ram[base + 1],
                            self.wave_ram[base + 2],
                            self.wave_ram[base + 3],
                        ];
                        self.wave_ram[0..4].copy_from_slice(&copy);
                    }
                }
            }
            self.master = true;
            self.wave_pos = 0;
            self.wave_counter = self.cc + self.period() + 3 + 2 * self.ds as u32;
            self.last_read_time = self.wave_counter;
        } else {
            self.enabled = false;
            self.master = false;
            self.wave_counter = COUNTER_DISABLED;
        }
    }

    /// channel3.cpp setNr0 (DAC enable/disable with sample-buffer latch).
    fn write_nr0(&mut self, value: u8) {
        let new_nr0 = value & 0x80;
        self.nr30 = new_nr0;
        self.dac_enabled = new_nr0 != 0;
        if new_nr0 == 0 {
            if self.master {
                if self.wave_counter == self.cc.wrapping_add(1) {
                    self.sample_buf = self.wave_ram[0];
                } else if !self.cgb && self.last_read_time == self.cc {
                    self.sample_buf = self.wave_ram[0xA];
                }
            }
            self.enabled = false;
            self.master = false;
            self.wave_counter = COUNTER_DISABLED;
        }
    }

    pub fn get_output(&self) -> f32 {
        if !self.master || !self.dac_enabled {
            return 0.0;
        }
        let byte_index = (self.wave_pos / 2) as usize;
        let sample = if self.wave_pos.is_multiple_of(2) {
            (self.wave_ram[byte_index] >> 4) & 0x0F
        } else {
            self.wave_ram[byte_index] & 0x0F
        };
        let output_level = self.get_output_level();
        let shifted = if output_level == 0 { 0 } else { sample >> (output_level - 1) };
        (shifted as f32) / 15.0
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// channel3.h waveRamRead, evaluated at the exact read cc.
    pub fn read_wave_ram(&self, addr: u16) -> u8 {
        let mut index = (addr - WAV_START) as usize;
        if index >= 16 {
            return 0xFF;
        }
        if self.master {
            if !self.cgb && self.cc != self.last_read_time {
                return 0xFF;
            }
            index = (self.wave_pos / 2) as usize;
        }
        self.wave_ram[index]
    }

    /// channel3.h waveRamWrite.
    pub fn write_wave_ram(&mut self, addr: u16, value: u8) {
        let mut index = (addr - WAV_START) as usize;
        if index >= 16 {
            return;
        }
        if self.master {
            if !self.cgb && self.cc != self.last_read_time {
                return;
            }
            index = (self.wave_pos / 2) as usize;
        }
        self.wave_ram[index] = value;
    }
}

impl Addressable for Wave {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR30..=NR34 => match addr {
                NR30 => self.nr30 | 0x7F,
                NR31 => 0xFF,
                NR32 => self.nr32 | 0x9F,
                NR33 => 0xFF,
                NR34 => self.nr34 | 0xBF,
                _ => 0xFF,
            },
            WAV_START..=WAV_END => self.read_wave_ram(addr),
            _ => panic!("Invalid address for Wave: {:#X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR30..=NR34 => match addr {
                NR30 => self.write_nr0(value),
                NR31 => {
                    self.nr31 = value;
                    self.length_counter = 256 - value as u16;
                }
                NR32 => self.nr32 = value,
                NR33 => self.nr33 = value,
                NR34 => self.write_nrx4(value),
                _ => {}
            },
            WAV_START..=WAV_END => self.write_wave_ram(addr, value),
            _ => panic!("Invalid address for Wave: {:#X}", addr),
        }
    }
}

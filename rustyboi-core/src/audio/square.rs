use serde::{Deserialize, Serialize};
use crate::audio::{NR10, NR11, NR12, NR13, NR14, NR21, NR22, NR23, NR24};
use crate::memory::mmio;
use crate::memory::Addressable;

// Gambatte's sound cycle counter is a free-running 2 MHz value; the frame
// sequencer position is `(cc >> 12) & 7`. Our FS step (the index about to be
// clocked) is offset from that by +3 (measured empirically against the boot
// DIV phase): `fs_step == ((cc >> 12) + 3) & 7`. Equivalently, length clocks
// when `(cc >> 12) & 7` is in {5,7,1,3} and envelope at {2}.
//
// Duty timing uses absolute event counters (`next_pos_update`) exactly like
// Gambatte's duty_unit.cpp; envelope and length use absolute `cc`-based
// counters mirroring envelope_unit.cpp / length_counter.cpp.

const COUNTER_DISABLED: u32 = 0xFFFF_FFFF;

// 0x7EE18180: duty/position output table, same packing as Gambatte.
fn to_out_state(duty: u8, pos: u8) -> bool {
    (0x7EE1_8180u32 >> (duty * 8 + pos)) & 1 != 0
}

fn to_period(freq: u16) -> u32 {
    (2048 - freq as u32) * 2
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SquareWave {
    channel1: bool,

    nr10: u8,
    nr11: u8,
    nr12: u8,
    nr13: u8,
    nr14: u8,
    nr21: u8,
    nr22: u8,
    nr23: u8,
    nr24: u8,

    enabled: bool,

    // Free-running 2 MHz cycle counter, kept in sync by the controller.
    #[serde(default)]
    cc: u32,

    // --- Duty unit (absolute event-counter model) ---
    #[serde(default = "disabled")]
    next_pos_update: u32,
    #[serde(default)]
    period: u32,
    #[serde(default)]
    pos: u8,
    #[serde(default)]
    high: bool,

    // --- Envelope unit ---
    #[serde(default = "disabled")]
    env_counter: u32,
    #[serde(default)]
    volume: u8,
    // The DAC/master enable: false once the DAC is off (NRx2 high nibble 0 and
    // not increasing). Mirrors Gambatte's `master_`.
    #[serde(default)]
    master: bool,

    // --- Length counter ---
    #[serde(default)]
    length_counter: u16,
    // Absolute-cc expiry counter (Gambatte LengthCounter::counter_).
    #[serde(default = "disabled")]
    length_abs: u32,
    #[serde(default)]
    length_enabled: bool,

    // --- Frequency sweep (Channel 1 only) ---
    #[serde(default)]
    sweep_enabled: bool,
    #[serde(default)]
    sweep_timer: u8,
    #[serde(default)]
    sweep_shadow_frequency: u16,
    #[serde(default)]
    fs_step: u8,
}

fn disabled() -> u32 {
    COUNTER_DISABLED
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
            cc: 0,
            next_pos_update: COUNTER_DISABLED,
            period: 4096,
            pos: 0,
            high: false,
            env_counter: COUNTER_DISABLED,
            volume: 0,
            master: false,
            length_counter: 0,
            length_abs: COUNTER_DISABLED,
            length_enabled: false,
            sweep_enabled: false,
            sweep_timer: 0,
            sweep_shadow_frequency: 0,
            fs_step: 0,
        }
    }

    pub fn set_cc(&mut self, cc: u32) {
        self.cc = cc;
    }

    /// Shift all absolute event counters backward by `delta` (Gambatte
    /// `resetCc`). Called when the underlying cycle counter is reset (DIV write)
    /// so the absolute schedules stay relative to the new counter origin.
    pub fn reset_cc(&mut self, delta: u32) {
        self.update_pos();
        if self.next_pos_update != COUNTER_DISABLED {
            self.next_pos_update = self.next_pos_update.wrapping_sub(delta);
        }
        if self.env_counter != COUNTER_DISABLED {
            self.env_counter = self.env_counter.wrapping_sub(delta);
        }
        if self.length_abs != COUNTER_DISABLED {
            self.length_abs = self.length_abs.wrapping_sub(delta);
        }
    }

    pub fn set_fs_step(&mut self, step: u8) {
        self.fs_step = step;
    }

    fn freq(&self) -> u16 {
        if self.channel1 {
            ((self.nr14 as u16 & 0x07) << 8) | self.nr13 as u16
        } else {
            ((self.nr24 as u16 & 0x07) << 8) | self.nr23 as u16
        }
    }

    fn duty(&self) -> u8 {
        if self.channel1 {
            self.nr11 >> 6
        } else {
            self.nr21 >> 6
        }
    }

    fn nr2(&self) -> u8 {
        if self.channel1 { self.nr12 } else { self.nr22 }
    }

    // --- Duty unit ---

    fn update_pos(&mut self) {
        let cc = self.cc;
        if self.next_pos_update != COUNTER_DISABLED && cc >= self.next_pos_update {
            let inc = (cc - self.next_pos_update) / self.period + 1;
            self.next_pos_update = self.next_pos_update.wrapping_add(self.period * inc);
            self.pos = ((self.pos as u32 + inc) % 8) as u8;
            self.high = to_out_state(self.duty(), self.pos);
        }
    }

    fn set_freq(&mut self, new_freq: u16) {
        self.update_pos();
        self.period = to_period(new_freq);
    }

    // --- Envelope unit ---

    fn env_event(&mut self) {
        let period = (self.nr2() & 0x07) as u32;
        if period != 0 {
            let inc = (self.nr2() & 0x08) != 0;
            let new_vol = if inc { self.volume as i16 + 1 } else { self.volume as i16 - 1 };
            if (0..0x10).contains(&new_vol) {
                self.volume = new_vol as u8;
                self.env_counter = self.env_counter.wrapping_add(period << 15);
            } else {
                self.env_counter = COUNTER_DISABLED;
            }
        } else {
            self.env_counter = self.env_counter.wrapping_add(8u32 << 15);
        }
    }

    /// Gambatte `EnvelopeUnit::nr4Init`. Returns true if the DAC is off.
    fn env_nr4_init(&mut self) -> bool {
        let nr2 = self.nr2();
        let mut period = if nr2 & 0x07 != 0 { (nr2 & 0x07) as u32 } else { 8 };
        if (self.cc.wrapping_add(2) & 0x7000) == 0x0000 {
            period += 1;
        }
        self.env_counter = self.cc
            .wrapping_sub(self.cc.wrapping_sub(0x1000) & 0x7FFF)
            .wrapping_add(period * 0x8000);
        self.volume = nr2 >> 4;
        (nr2 & 0xF8) == 0
    }

    fn write_nrx2(&mut self, value: u8) {
        // Gambatte `EnvelopeUnit::nr2Change` (DMG zombie mode), only when master.
        let old = self.nr2();
        if self.master {
            let will_clock = self.env_will_clock();
            if will_clock {
                let period = (old & 0x07) as u32;
                self.env_counter = self.cc
                    .wrapping_sub(self.cc.wrapping_sub(0x1000) & 0x7FFF)
                    .wrapping_add(period * 0x8000);
            }

            let mut tick = (value & 0x07) != 0
                && (old & 0x07) == 0
                && self.env_counter != COUNTER_DISABLED;
            let invert = ((value & 0x08) ^ (old & 0x08)) != 0;

            if (value & 0x0F) == 0x08
                && (old & 0x0F) == 0x08
                && self.env_counter != COUNTER_DISABLED
            {
                tick = true;
            }

            if invert {
                if value & 0x08 != 0 {
                    if (old & 0x07) == 0 && self.env_counter != COUNTER_DISABLED {
                        self.volume ^= 0xF;
                    } else {
                        self.volume = (0xE_i16 - self.volume as i16) as u8 & 0xF;
                    }
                    tick = false;
                } else {
                    self.volume = (0x10_i16 - self.volume as i16) as u8 & 0xF;
                }
            }

            if tick {
                if value & 0x08 != 0 {
                    self.volume = self.volume.wrapping_add(1);
                } else {
                    self.volume = self.volume.wrapping_sub(1);
                }
                self.volume &= 0xF;
            } else if (value & 0x07) == 0 && will_clock {
                if invert {
                    if self.volume == (if value & 0x08 != 0 { 0xE } else { 0x1 }) {
                        self.env_counter = COUNTER_DISABLED;
                    }
                } else if self.volume == (if value & 0x08 != 0 { 0xF } else { 0x0 }) {
                    self.env_counter = COUNTER_DISABLED;
                }
            }
        }

        if self.channel1 {
            self.nr12 = value;
        } else {
            self.nr22 = value;
        }

        // DAC off disables the channel (master).
        if (value & 0xF8) == 0 {
            self.master = false;
            self.enabled = false;
        }
    }

    /// Will the envelope clock on the FS step that NRx2 writes coincide with?
    /// In Gambatte this is `EnvelopeUnit::clock(cc)`; the envelope event fires on
    /// FS step 7, i.e. when `(cc >> 12) & 7` rounds into that frame region.
    fn env_will_clock(&self) -> bool {
        // Gambatte's clock_ flag is set when the unit is in the active phase. We
        // approximate via the counter being live; the precise zombie sub-cases
        // are handled by the volume math above.
        self.env_counter != COUNTER_DISABLED
    }

    // --- Length counter ---

    fn length_mask(&self) -> u16 {
        0x3F
    }

    fn write_nrx1(&mut self, value: u8) {
        if self.channel1 {
            self.nr11 = value;
        } else {
            self.nr21 = value;
        }
        // length_counter.cpp nr1Change.
        self.length_counter = (!(value as u16) & self.length_mask()) + 1;
        let nr4 = if self.channel1 { self.nr14 } else { self.nr24 };
        self.length_abs = if nr4 & 0x40 != 0 {
            ((self.cc >> 13) + self.length_counter as u32) << 13
        } else {
            COUNTER_DISABLED
        };
        self.duty_nr1_change();
    }

    /// Clock the length counter if cc has reached the absolute expiry.
    fn length_check(&mut self) {
        if self.length_abs != COUNTER_DISABLED && self.cc >= self.length_abs {
            self.length_abs = COUNTER_DISABLED;
            self.length_counter = 0;
            self.enabled = false;
        }
    }

    fn duty_nr1_change(&mut self) {
        self.update_pos();
    }

    pub fn step(&mut self, _mmio: &mut mmio::Mmio) {
        // Length expiry is checked even when the DAC is off.
        self.length_check();

        if !self.master {
            return;
        }
        // Advance the duty position up to the current cc.
        self.update_pos();

        // Envelope event(s).
        while self.env_counter != COUNTER_DISABLED && self.cc >= self.env_counter {
            self.env_event();
        }
    }

    pub fn step_frame_sequencer(&mut self, step: u8) {
        if !self.enabled {
            return;
        }

        // Frequency sweep (steps 2, 6) - Channel 1 only.
        if self.channel1 && (step == 2 || step == 6) {
            self.step_frequency_sweep();
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
                        self.set_freq(new_frequency);
                        self.update_frequency_registers();
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
            self.sweep_shadow_frequency.saturating_sub(offset)
        } else {
            let new_freq = self.sweep_shadow_frequency + offset;
            if new_freq > 2047 {
                self.enabled = false;
                self.master = false;
            }
            new_freq
        }
    }

    fn update_frequency_registers(&mut self) {
        // Reflect the swept frequency back into the period registers.
        if self.channel1 {
            self.nr13 = (self.sweep_shadow_frequency & 0xFF) as u8;
            self.nr14 = (self.nr14 & 0xF8) | ((self.sweep_shadow_frequency >> 8) & 0x07) as u8;
        } else {
            self.nr23 = (self.sweep_shadow_frequency & 0xFF) as u8;
            self.nr24 = (self.nr24 & 0xF8) | ((self.sweep_shadow_frequency >> 8) & 0x07) as u8;
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

    // --- NRx4 / trigger ---

    fn write_nrx4(&mut self, value: u8) {
        let old_nr4 = if self.channel1 { self.nr14 } else { self.nr24 };
        let trigger = value & 0x80 != 0;
        let new_length_enabled = value & 0x40 != 0;

        // length_counter.cpp nr4Change (absolute-cc model).
        if self.length_abs != COUNTER_DISABLED {
            self.length_counter = ((self.length_abs >> 13) - (self.cc >> 13)) as u16;
        }

        let mut dec: u16 = 0;
        if new_length_enabled {
            dec = ((!self.cc >> 12) & 1) as u16;
            if (old_nr4 & 0x40) == 0 && self.length_counter > 0 {
                self.length_counter -= dec;
                if self.length_counter == 0 {
                    self.enabled = false;
                }
            }
        }

        if trigger && self.length_counter == 0 {
            self.length_counter = self.length_mask() + 1 - dec;
        }

        self.length_enabled = new_length_enabled;

        if self.channel1 {
            self.nr14 = value;
        } else {
            self.nr24 = value;
        }

        self.length_abs = if new_length_enabled && self.length_counter > 0 {
            ((self.cc >> 13) + self.length_counter as u32) << 13
        } else {
            COUNTER_DISABLED
        };

        // dutyUnit/envelope nr4 handling happens on trigger.
        if trigger {
            self.trigger();
        } else {
            // Frequency-high write still updates the duty period.
            self.set_freq(self.freq());
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;

        // Envelope: nr4Init sets volume + counter; master = DAC on.
        let dac_off = self.env_nr4_init();
        self.master = !dac_off;

        // Duty: set frequency/period, then place the absolute next-pos update.
        self.set_freq(self.freq());
        // ref = 1 in single speed (lastUpdate_ always 4-aligned); master bool
        // toggles the +4 vs +2 offset.
        let m = if self.master { 1 } else { 0 };
        self.next_pos_update = self.cc
            .wrapping_sub((self.cc.wrapping_sub(1)) & 1)
            .wrapping_add(self.period)
            .wrapping_add(4 - 2 * m);

        // Frequency sweep (Channel 1 only).
        if self.channel1 {
            self.sweep_shadow_frequency = self.freq();
            self.sweep_timer = self.get_sweep_period();
            if self.sweep_timer == 0 {
                self.sweep_timer = 8;
            }
            self.sweep_enabled = self.get_sweep_period() > 0 || self.get_sweep_shift() > 0;
            if self.get_sweep_shift() > 0 {
                let _ = self.calculate_sweep_frequency();
            }
        }

        if dac_off {
            self.enabled = false;
        }
    }

    pub fn get_output(&self) -> f32 {
        if !self.enabled || !self.master || self.volume == 0 {
            return 0.0;
        }
        if self.high {
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
                        NR10 => self.nr10 | 0x80,
                        NR11 => self.nr11 | 0x3F,
                        NR12 => self.nr12,
                        NR13 => 0xFF,
                        NR14 => self.nr14 | 0xBF,
                        _ => 0xFF,
                    }
                } else {
                    panic!("Invalid read from Channel 2 SquareWave: {:#X}", addr);
                }
            }
            NR21..=NR24 => {
                if !self.channel1 {
                    match addr {
                        NR21 => self.nr21 | 0x3F,
                        NR22 => self.nr22,
                        NR23 => 0xFF,
                        NR24 => self.nr24 | 0xBF,
                        _ => 0xFF,
                    }
                } else {
                    panic!("Invalid read from Channel 1 SquareWave: {:#X}", addr);
                }
            }
            _ => panic!("Invalid address for SquareWave: {:#X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR10..=NR14 => {
                if self.channel1 {
                    match addr {
                        NR10 => self.nr10 = value,
                        NR11 => self.write_nrx1(value),
                        NR12 => self.write_nrx2(value),
                        NR13 => {
                            self.nr13 = value;
                            self.set_freq(self.freq());
                        }
                        NR14 => self.write_nrx4(value),
                        _ => {}
                    }
                } else {
                    panic!("Invalid write to Channel 2 SquareWave: {:#X}", addr);
                }
            }
            NR21..=NR24 => {
                if !self.channel1 {
                    match addr {
                        NR21 => self.write_nrx1(value),
                        NR22 => self.write_nrx2(value),
                        NR23 => {
                            self.nr23 = value;
                            self.set_freq(self.freq());
                        }
                        NR24 => self.write_nrx4(value),
                        _ => {}
                    }
                } else {
                    panic!("Invalid write to Channel 1 SquareWave: {:#X}", addr);
                }
            }
            _ => panic!("Invalid address for SquareWave: {:#X}", addr),
        }
    }
}

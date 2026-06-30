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
    lfsr: u16, // Linear feedback shift register (Gambatte Lfsr::reg_)
    length_enabled: bool,
    fs_step: u8,

    // Gambatte `Channel4::Lfsr` cc-based model (channel4.cpp). The LFSR register
    // is advanced lazily to the access cc via `update_backup_counter`, so a
    // PCM34 read resolves the exact sub-counter phase rather than a per-clock
    // countdown.
    #[serde(default = "counter_disabled")]
    lfsr_backup_counter: u32,
    #[serde(default = "counter_disabled")]
    lfsr_counter: u32,
    #[serde(default)]
    lfsr_master: bool,

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
const COUNTER_DISABLED: u32 = 0xFFFF_FFFF;

fn len_disabled() -> u32 {
    LEN_DISABLED
}

fn counter_disabled() -> u32 {
    COUNTER_DISABLED
}

/// Gambatte channel4.cpp `toPeriod`: the LFSR step period in `cycleCounter_`
/// units. `s = nr3>>4 + 3`, `r = nr3 & 7` (r==0 acts as r=1 with s-1).
fn lfsr_to_period(nr3: u8) -> u32 {
    let mut s = (nr3 >> 4) as u32 + 3;
    let mut r = (nr3 & 0x07) as u32;
    if r == 0 {
        r = 1;
        s -= 1;
    }
    r << s
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
            lfsr: 0x7FFF,
            length_enabled: false,
            fs_step: 0,
            lfsr_backup_counter: COUNTER_DISABLED,
            lfsr_counter: COUNTER_DISABLED,
            lfsr_master: false,
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

    /// Gambatte `Channel4::reset` (from `PSG::reset` on the NR52 0→1 enable):
    /// resets the LFSR + envelope sub-state and disables the channel. The length
    /// counter is preserved.
    pub fn psg_reset(&mut self) {
        self.lfsr = 0x7FFF;
        self.volume = 0;
        self.volume_timer = 0;
        self.enabled = false;
        // Gambatte `Channel4::reset` -> `Lfsr::reset(cc)`: nr3_=0, master off,
        // backupCounter_ = cc + toPeriod(0); the step counter stays disabled.
        self.lfsr_master = false;
        self.lfsr_backup_counter = self.cc.wrapping_add(lfsr_to_period(0));
        self.lfsr_counter = COUNTER_DISABLED;
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
        self.lfsr_master = false;
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
                    self.lfsr_master = false;
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
        // The LFSR register is advanced LAZILY at the access cc via
        // `update_backup_counter` (Gambatte `Lfsr::updateBackupCounter`), so no
        // per-clock stepping is needed here — `event()`/`lfsr_counter` is only
        // the audio-mixer's scheduling bookkeeping, kept in sync inside the
        // bulk update. Advancing it here too would double-count the shifts.
    }

    /// Resolve the LFSR register to the current cc for the CPU read path
    /// (PCM34), mirroring `Channel3::sync_for_read`.
    pub fn sync_for_read(&mut self) {
        if self.lfsr_master {
            self.lfsr_update_backup_counter(self.cc);
        }
    }

    /// Resolve the LFSR register at a specific (precise per-access) cc, the
    /// length subsystem's overlaid read cc rather than the per-dot `self.cc`.
    pub fn sync_lfsr_at(&mut self, at_cc: u32) {
        if self.lfsr_master {
            self.lfsr_update_backup_counter(at_cc);
        }
    }

    /// Gambatte `Channel4::Lfsr::resetCc` hook for a DIV write.
    pub fn reset_cc(&mut self, cc: u32, delta: u32) {
        self.lfsr_reset_cc(cc, delta);
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

    // --- Gambatte `Channel4::Lfsr` (channel4.cpp) cc-based register model ---

    /// channel4.cpp `Lfsr::updateBackupCounter`: bulk-advance the LFSR register
    /// to `cc` (lazy catch-up). Only steps while the shift code is < 14
    /// (`nr3_ < 0xE0`); a code >= 14 freezes the register (Gambatte quirk).
    fn lfsr_update_backup_counter(&mut self, cc: u32) {
        if self.lfsr_backup_counter <= cc {
            let period = lfsr_to_period(self.nr43);
            let mut periods = (cc - self.lfsr_backup_counter) / period + 1;
            self.lfsr_backup_counter = self
                .lfsr_backup_counter
                .wrapping_add(periods * period);

            if self.lfsr_master && self.nr43 < 0xE0 {
                let mut reg = self.lfsr as u32;
                if self.nr43 & 0x08 != 0 {
                    // 7-bit width.
                    while periods > 6 {
                        let xored = (reg << 1 ^ reg) & 0x7E;
                        reg = (reg >> 6 & !0x7E) | xored | xored << 8;
                        periods -= 6;
                    }
                    let xored = ((reg ^ reg >> 1) << (7 - periods)) & 0x7F;
                    reg = (reg >> periods & !(0x80u32.wrapping_sub(0x80 >> periods)))
                        | xored
                        | xored << 8;
                } else {
                    // 15-bit width.
                    while periods > 15 {
                        reg = reg ^ reg >> 1;
                        periods -= 15;
                    }
                    reg = reg >> periods | (((reg ^ reg >> 1) << (15 - periods)) & 0x7FFF);
                }
                self.lfsr = reg as u16;
            }
        }
    }

    /// channel4.cpp `Lfsr::nr3Change`: re-anchor the step counter (`counter_=cc`,
    /// the GSR "Mickey fix") after catching the register up.
    fn lfsr_nr3_change(&mut self, cc: u32) {
        self.lfsr_update_backup_counter(cc);
        self.lfsr_counter = cc;
    }

    /// channel4.cpp `Lfsr::nr4Init`: enable + schedule the first step at `cc+4`.
    fn lfsr_nr4_init(&mut self, cc: u32) {
        self.lfsr_master = false;
        self.lfsr_update_backup_counter(cc);
        self.lfsr_master = true;
        self.lfsr_backup_counter = self.lfsr_backup_counter.wrapping_add(4);
        self.lfsr_counter = self.lfsr_backup_counter;
    }

    /// channel4.cpp `Lfsr::resetCc`: shift the cc anchors back on a DIV write.
    fn lfsr_reset_cc(&mut self, cc: u32, delta: u32) {
        self.lfsr_update_backup_counter(cc);
        self.lfsr_backup_counter = self.lfsr_backup_counter.wrapping_sub(delta);
        if self.lfsr_counter != COUNTER_DISABLED {
            self.lfsr_counter = self.lfsr_counter.wrapping_sub(delta);
        }
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

        // LFSR: Gambatte channel4.cpp `setNr4` triggers `lfsr_.nr4Init(cc)` only
        // when the DAC stays on (`master_ = !envelope.nr4Init`). The register is
        // NOT reloaded to 0x7FFF on trigger (only on `Lfsr::reset` at power-on).
        let dac_off =
            self.get_envelope_initial_volume() == 0 && !self.get_envelope_direction();
        if dac_off {
            self.enabled = false;
            self.lfsr_master = false;
        } else {
            self.lfsr_nr4_init(self.cc);
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

    /// CGB PCM34 high nibble for the noise channel (Gambatte `channel4.cpp`):
    /// `isActive()` is `master_` (here our `enabled`, which the DAC-off path
    /// clears) and `vol_ = lfsr.isHighState(cc) ? envelope.volume : 0`. The LFSR
    /// high state is the inverted bit-0 (Gambatte `Lfsr::isHighState`).
    pub fn pcm_nibble(&self) -> u8 {
        // Gambatte channel4.cpp `update`: `vol_ = isHighState(cc) ? volume : 0`,
        // gated by `isActive()` == `master_` (the DAC/trigger gate). The register
        // is already advanced to the read cc by `sync_for_read`.
        if !self.lfsr_master {
            return 0;
        }
        if (!self.lfsr) & 0x01 == 1 {
            self.volume & 0x0F
        } else {
            0
        }
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
                        // channel4.cpp setNr2: a DAC-off envelope disables master.
                        if self.get_envelope_initial_volume() == 0 && !self.get_envelope_direction() {
                            self.enabled = false;
                            self.lfsr_master = false;
                        }
                    }
                    NR43 => {
                        // channel4.cpp setNr3 -> lfsr_.nr3Change(value, cc): catch
                        // the register up at the OLD period, then re-anchor.
                        self.lfsr_nr3_change(self.cc);
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

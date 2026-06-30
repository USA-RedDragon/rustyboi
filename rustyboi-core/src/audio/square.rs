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

    // --- Length counter (Gambatte length_counter.cpp, cc-event model) ---
    #[serde(default)]
    length_counter: u16,
    #[serde(default)]
    length_enabled: bool,
    // Absolute cc of length expiry (Gambatte `LengthCounter::counter_`):
    // `((cc>>13)+lengthCounter)<<13` when enabled, else `LEN_DISABLED`.
    #[serde(default = "len_disabled")]
    len_counter: u32,
    // Length-subsystem cc (duty/envelope use `cc`; length uses this phased cc).
    #[serde(default)]
    len_cc: u32,

    // --- Frequency sweep (Channel 1 only) ---
    #[serde(default)]
    sweep_shadow_frequency: u16,
    #[serde(default)]
    fs_step: u8,

    // Gambatte cc-driven sweep (channel1.cpp SweepUnit). Absolute-cc event
    // counter, `neg_` latch, and the cgb flag the nr4Init phase needs.
    #[serde(default = "disabled")]
    sweep_counter: u32,
    #[serde(default)]
    sweep_neg: bool,
    #[serde(default)]
    cgb: bool,

    // The duty-trigger reference parity (`ref` in Gambatte's `setNr4`):
    // `!(lastUpdate_ & ds)`. In single speed this is always 1, but at double
    // speed it tracks the CPU `lastUpdate_` parity, which shifts the
    // `nextPosUpdate_ = cc - (cc - ref) % 2 + ...` duty-trigger placement by one
    // cc. Pushed each sync by the controller.
    #[serde(default = "default_nr4_ref")]
    nr4_ref: u32,
}

fn default_nr4_ref() -> u32 {
    1
}

fn disabled() -> u32 {
    COUNTER_DISABLED
}

const LEN_DISABLED: u32 = COUNTER_DISABLED;

fn len_disabled() -> u32 {
    LEN_DISABLED
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
            length_enabled: false,
            len_counter: LEN_DISABLED,
            len_cc: 0,
            sweep_shadow_frequency: 0,
            fs_step: 0,
            sweep_counter: COUNTER_DISABLED,
            sweep_neg: false,
            cgb: false,
            nr4_ref: 1,
        }
    }

    pub fn set_nr4_ref(&mut self, r: u32) {
        self.nr4_ref = r;
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

    /// Post-boot channel-1 mid-tone state (Gambatte `setPostBiosState`). The boot
    /// ROM leaves ch1 playing the startup tone: master/enabled with duty pos/phase
    /// mid-cycle. `pos_offset` is Gambatte's duty.nextPosUpdate offset (in 2 MHz
    /// units) added to the current cc; `pos`/`high` are the duty-unit phase.
    pub fn set_post_bios_ch1(&mut self, pos_offset: u32, pos: u8, high: bool) {
        self.nr11 = 0xBF;
        self.nr12 = 0xF3;
        self.nr13 = 0xC1;
        self.nr14 = 0x07;
        self.master = true;
        self.enabled = true;
        // Post-boot the startup-ding envelope has already decayed to 0 (Gambatte
        // initstate.cpp `env.volume = 0`). The channel's length counter is still
        // running (NR52 bit 0 / `enabled` set), but its digital DAC output is 0 —
        // matching the real-cgb04c `fexx_ffxx_dumper` capture where FF76 (PCM12)
        // reads 0x00 while NR52 reads 0xF1.
        self.volume = 0x00;
        self.period = to_period(self.freq());
        self.pos = pos;
        self.high = high;
        self.next_pos_update = (self.cc & !1u32).wrapping_add(pos_offset);
        self.length_counter = 0x40;
    }

    pub fn set_length_counter(&mut self, value: u16) {
        self.length_counter = value;
    }

    /// Shift the duty event counter backward by `delta` (Gambatte
    /// `Channel::resetCc`, which only resets the duty unit). Called when the
    /// underlying cycle counter is reset by a DIV write. The envelope and length
    /// counters are intentionally left alone — they key on absolute `cc>>13` /
    /// `cc>>15` boundaries that survive the reset.
    pub fn reset_cc(&mut self, delta: u32) {
        self.update_pos();
        if self.next_pos_update != COUNTER_DISABLED {
            self.next_pos_update = self.next_pos_update.wrapping_sub(delta);
        }
    }

    pub fn set_fs_step(&mut self, step: u8) {
        self.fs_step = step;
    }

    /// Gambatte `Channel1::reset`/`Channel2::reset` (called from `PSG::reset` on
    /// the NR52 0→1 enable). Re-initializes the duty + envelope sub-counters at
    /// the freshly-folded cc. The length counter is intentionally preserved
    /// (Gambatte's `lengthCounter_` survives `PSG::reset`).
    pub fn psg_reset(&mut self) {
        // DutyUnit::reset
        self.pos = 0;
        self.high = false;
        self.next_pos_update = COUNTER_DISABLED;
        // EnvelopeUnit::reset
        self.env_counter = COUNTER_DISABLED;
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

    // --- Length counter (Gambatte length_counter.cpp, cc-driven) ---

    fn length_mask(&self) -> u16 {
        0x3F
    }

    fn nr4(&self) -> u8 {
        if self.channel1 { self.nr14 } else { self.nr24 }
    }

    /// Gambatte `LengthCounter::nr1Change`. The NRx1 write reloads the length
    /// load and (re)schedules the absolute expiry cc from the current NRx4 lcen.
    fn write_nrx1(&mut self, value: u8) {
        if self.channel1 {
            self.nr11 = value;
        } else {
            self.nr21 = value;
        }
        let mask = self.length_mask();
        self.length_counter = (!value as u16 & mask) + 1;
        self.len_counter = if self.nr4() & 0x40 != 0 {
            (((self.len_cc >> 13) + self.length_counter as u32) << 13).min(u32::MAX)
        } else {
            LEN_DISABLED
        };
        self.duty_nr1_change();
    }

    /// Gambatte `LengthCounter::event`: expiry disables the channel.
    pub fn length_event(&mut self) {
        self.len_counter = LEN_DISABLED;
        self.length_counter = 0;
        self.enabled = false;
    }

    fn duty_nr1_change(&mut self) {
        self.update_pos();
    }

    pub fn step(&mut self, _mmio: &mut mmio::Mmio) {
        if self.channel1 {
            self.cgb = _mmio.is_cgb_features_enabled();
        }
        if !self.master {
            return;
        }
        // Advance the duty position up to the current cc.
        self.update_pos();

        // Envelope event(s).
        while self.env_counter != COUNTER_DISABLED && self.cc >= self.env_counter {
            self.env_event();
        }

        // Frequency sweep event(s) (Channel 1 only) — cc-driven, like Gambatte's
        // SweepUnit (channel1.cpp). Polled here, not FS-clocked.
        while self.channel1
            && self.sweep_counter != COUNTER_DISABLED
            && self.cc >= self.sweep_counter
        {
            self.sweep_event();
        }
    }

    pub fn step_frame_sequencer(&mut self, _step: u8) {
        // Length is a cc-driven absolute expiry event (see `length_event`) and
        // the frequency sweep is now a cc-driven event polled in `step`, so
        // nothing is FS-clocked here.
    }

    /// Gambatte `Channel1::SweepUnit::calcFreq`. Uses NR10 directly, latches
    /// `neg_`, and disables master on an overflow (freq & 2048).
    fn sweep_calc_freq(&mut self) -> u16 {
        let nr0 = self.nr10;
        let shift = (nr0 & 0x07) as u16;
        let freq = if nr0 & 0x08 != 0 {
            self.sweep_shadow_frequency.wrapping_sub(self.sweep_shadow_frequency >> shift)
        } else {
            self.sweep_shadow_frequency.wrapping_add(self.sweep_shadow_frequency >> shift)
        };
        if nr0 & 0x08 != 0 {
            self.sweep_neg = true;
        }
        if freq & 2048 != 0 {
            self.enabled = false;
            self.master = false;
        }
        freq
    }

    /// Gambatte `Channel1::SweepUnit::event`. Dispatched when `cc >= counter_`.
    fn sweep_event(&mut self) {
        let period = ((self.nr10 & 0x70) >> 4) as u32;
        if period != 0 {
            let freq = self.sweep_calc_freq();
            if freq & 2048 == 0 && (self.nr10 & 0x07) != 0 {
                self.sweep_shadow_frequency = freq;
                self.set_freq_at(freq, self.sweep_counter);
                self.sweep_calc_freq();
            }
            self.sweep_counter = self.sweep_counter.wrapping_add(period << 14);
        } else {
            self.sweep_counter = self.sweep_counter.wrapping_add(8u32 << 14);
        }
    }

    /// Gambatte `Channel1::SweepUnit::nr0Change`: a neg→non-neg transition after
    /// a negative calc disables master.
    fn sweep_nr0_change(&mut self, new_nr0: u8) {
        if self.sweep_neg && (new_nr0 & 0x08) == 0 {
            self.enabled = false;
            self.master = false;
        }
    }

    /// Gambatte `Channel1::SweepUnit::nr4Init`. Schedules the absolute-cc sweep
    /// event counter at the trigger cc.
    fn sweep_nr4_init(&mut self) {
        self.sweep_neg = false;
        self.sweep_shadow_frequency = self.freq();
        let period = ((self.nr10 & 0x70) >> 4) as u32;
        let rsh = (self.nr10 & 0x07) as u32;
        if period | rsh != 0 {
            let cgb2 = if self.cgb { 2 } else { 0 };
            self.sweep_counter = ((((self.cc.wrapping_add(2).wrapping_add(cgb2)) >> 14)
                + if period != 0 { period } else { 8 })
                << 14)
                .wrapping_add(2);
        } else {
            self.sweep_counter = COUNTER_DISABLED;
        }
        if rsh != 0 {
            self.sweep_calc_freq();
        }
    }

    /// Like `set_freq`, but advances the duty position to a specified cc (the
    /// sweep event's `counter_`) rather than the live `cc` (Gambatte calls
    /// `dutyUnit_.setFreq(freq, counter_)`).
    fn set_freq_at(&mut self, new_freq: u16, at_cc: u32) {
        let saved = self.cc;
        self.cc = at_cc;
        self.update_pos();
        self.cc = saved;
        self.period = to_period(new_freq);
        // Reflect the swept frequency back into the period registers.
        self.nr13 = (new_freq & 0xFF) as u8;
        self.nr14 = (self.nr14 & 0xF8) | ((new_freq >> 8) & 0x07) as u8;
    }

    // --- NRx4 / trigger ---

    /// Gambatte `LengthCounter::nr4Change` length-unit handling, folded into the
    /// NRx4 write. Re-derives `lengthCounter_` from the absolute expiry cc, then
    /// applies the lcen-enable `dec = ~cc>>12 & 1` extra-clock quirk and the
    /// trigger reload, finally rescheduling the absolute expiry.
    fn length_nr4_change(&mut self, old_nr4: u8, new_nr4: u8, trigger: bool) {
        let mask = self.length_mask();
        if self.len_counter != LEN_DISABLED {
            self.length_counter =
                ((self.len_counter >> 13).wrapping_sub(self.len_cc >> 13)) as u16;
        }

        let mut dec: u16 = 0;
        if new_nr4 & 0x40 != 0 {
            dec = ((!self.len_cc >> 12) & 1) as u16;
            if old_nr4 & 0x40 == 0 && self.length_counter != 0 {
                self.length_counter -= dec;
                if self.length_counter == 0 {
                    self.enabled = false;
                }
            }
        }

        if new_nr4 & 0x80 != 0 && self.length_counter == 0 {
            self.length_counter = mask + 1 - dec;
        }

        let _ = trigger;
        self.len_counter = if new_nr4 & 0x40 != 0 && self.length_counter != 0 {
            (((self.len_cc >> 13) + self.length_counter as u32) << 13).min(u32::MAX)
        } else {
            LEN_DISABLED
        };
    }

    fn write_nrx4(&mut self, value: u8) {
        let trigger = value & 0x80 != 0;
        let old_nr4 = self.nr4();

        self.length_nr4_change(old_nr4, value, trigger);
        self.length_enabled = value & 0x40 != 0;

        if self.channel1 {
            self.nr14 = value;
        } else {
            self.nr24 = value;
        }

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

        // Length-counter reload + reschedule is handled in `length_nr4_change`
        // (Gambatte folds the trigger reload into the length unit's nr4Change).

        // Channel 1 runs dutyUnit_.nr4Change BEFORE updating master_, so its
        // nextPosUpdate uses the OLD master; channel 2 updates master_ first and
        // uses the NEW master (Gambatte channel1.cpp vs channel2.cpp ordering).
        let old_master = self.master;

        // Envelope: nr4Init sets volume + counter; master = DAC on.
        let dac_off = self.env_nr4_init();
        self.master = !dac_off;

        // Duty: set frequency/period, then place the absolute next-pos update.
        self.set_freq(self.freq());
        // ref = 1 in single speed (lastUpdate_ always 4-aligned); master bool
        // toggles the +4 vs +2 offset.
        let duty_master = if self.channel1 { old_master } else { self.master };
        let m = if duty_master { 1 } else { 0 };
        // Gambatte DutyUnit::nr4Change: `nextPosUpdate_ =
        //   cc - (cc - ref) % 2 + period_ + 4 - (master << 1)`. `ref` is
        // `!(lastUpdate_ & ds)` (always 1 in single speed; tracks the CPU
        // `lastUpdate_` parity at double speed). Pushed in `nr4_ref`.
        self.next_pos_update = self.cc
            .wrapping_sub(self.cc.wrapping_sub(self.nr4_ref) & 1)
            .wrapping_add(self.period)
            .wrapping_add(4 - 2 * m);

        // Frequency sweep (Channel 1 only) — Gambatte cc-driven SweepUnit.
        if self.channel1 {
            self.sweep_nr4_init();
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

    /// CGB PCM12 nibble for this square channel (Gambatte `channel{1,2}.cpp`):
    /// `isActive()` is `master_` (the DAC/trigger gate) and the reported digital
    /// amplitude is `vol_ = dutyUnit_.isHighState(cc) ? envelope.volume : 0`.
    pub fn pcm_nibble(&self) -> u8 {
        if !self.master {
            return 0;
        }
        if self.high {
            self.volume & 0x0F
        } else {
            0
        }
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
                        NR10 => {
                            self.sweep_nr0_change(value);
                            self.nr10 = value;
                        }
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

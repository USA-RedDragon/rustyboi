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

// SameBoy `duties[]` (Core/apu.c): the digital output for a given
// (current_sample_index + duty*8). `current_sample_index` INCREMENTS each duty
// tick (SameBoy runs the phase forward), unlike Gambatte's decrementing table.
// This is the hardware-accurate model the SameSuite channel_*_align/duty/delay
// tests are validated against on cgb04c.
const DUTIES: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 1,
    1, 0, 0, 0, 0, 0, 0, 1,
    1, 0, 0, 0, 0, 1, 1, 1,
    0, 1, 1, 1, 1, 1, 1, 0,
];

fn duty_out(duty: u8, index: u8) -> bool {
    DUTIES[(index as usize & 7) + (duty as usize) * 8] != 0
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

    // --- Duty unit (SameBoy countdown model, Core/apu.c) ---
    // `period` = (2048-freq)*2, the steady-state duty tick interval in 2 MHz
    // cycles. Kept for the freq-write path.
    #[serde(default)]
    period: u32,
    // SameBoy `current_sample_index`: the duty phase (0..7), INCREMENTING. NOT
    // reset on trigger — only APU-off resets it.
    #[serde(default)]
    pos: u8,
    // Cached digital-high state for the current `pos`/`duty` (SameBoy computes it
    // via `duties[]` at each tick).
    #[serde(default)]
    high: bool,
    // SameBoy `sample_countdown`: 2 MHz cycles until the next duty tick. The tick
    // consumes `sample_countdown + 1` cycles (SameBoy `cycles_left -= countdown+1`),
    // reloading to `(2047-freq)*2 + 1`. `-1` (u32::MAX) means "not yet reloaded"
    // (SameBoy inits to -1). `sample_length` here == freq.
    #[serde(default = "disabled")]
    sample_countdown: u32,
    // SameBoy `delay`: extra 2 MHz cycles added to the first countdown at trigger
    // so the first duty edge lands at the hardware-accurate phase.
    #[serde(default)]
    delay: u32,
    // SameBoy `sample_surpressed`: true after a fresh trigger until the first duty
    // tick clears it; while set the channel's digital output reads 0 (this is the
    // "(sample length + 2) ticks until PCM12 is affected" delay the channel_*_delay
    // tests measure).
    #[serde(default)]
    sample_surpressed: bool,
    // SameBoy `did_tick` / `just_reloaded`: NRx3/NRx4 edge-case flags.
    #[serde(default)]
    did_tick: bool,
    #[serde(default)]
    just_reloaded: bool,
    // The 2 MHz `cc` up to which the duty countdown has been advanced.
    #[serde(default)]
    last_pos_cc: u32,
    // SameBoy `lf_div`: the 2 MHz sub-phase (0/1) used by the trigger delay
    // formula. Derived from the free-running `cc` parity; pushed by the controller.
    #[serde(default = "default_lf_div")]
    lf_div: u32,
    // CGB double-speed flag (SameBoy `cgb_double_speed`), pushed by the controller.
    #[serde(default)]
    ds: bool,

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

fn default_lf_div() -> u32 {
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
            period: 4096,
            pos: 0,
            high: false,
            sample_countdown: COUNTER_DISABLED,
            delay: 0,
            sample_surpressed: false,
            did_tick: false,
            just_reloaded: false,
            last_pos_cc: 0,
            lf_div: 1,
            ds: false,
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

    /// SameBoy `lf_div` (2 MHz sub-phase) used by the trigger delay formula.
    pub fn set_lf_div(&mut self, lf_div: u32) {
        self.lf_div = lf_div;
    }

    /// SameBoy `cgb_double_speed`.
    pub fn set_ds(&mut self, ds: bool) {
        self.ds = ds;
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
        // Seed the SameBoy countdown from the post-boot phase offset: the next
        // duty tick is `pos_offset` 2 MHz cycles out. `last_pos_cc` anchors the
        // countdown to the current cc so `update_pos` deltas are correct.
        self.last_pos_cc = self.cc;
        self.sample_countdown = pos_offset.wrapping_sub(1);
        self.delay = 0;
        self.sample_surpressed = false;
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
        // Advance the duty countdown to the current (pre-fold) cc, then shift the
        // countdown anchor by the same delta the controller applies to `cc`, so the
        // subsequent `set_cc(folded)` sees a zero delta and the countdown/index are
        // preserved across the DIV-reset fold (SameBoy keeps `sample_countdown`).
        self.update_pos();
        self.last_pos_cc = self.last_pos_cc.wrapping_sub(delta);
    }

    pub fn set_fs_step(&mut self, step: u8) {
        self.fs_step = step;
    }

    /// Gambatte `Channel1::reset`/`Channel2::reset` (called from `PSG::reset` on
    /// the NR52 0→1 enable). Re-initializes the duty + envelope sub-counters at
    /// the freshly-folded cc. The length counter is intentionally preserved
    /// (Gambatte's `lengthCounter_` survives `PSG::reset`).
    pub fn psg_reset(&mut self) {
        // DutyUnit::reset. SameBoy resets the duty phase to 0 only on APU-off; the
        // NR52 0→1 enable path (PSG::reset) re-anchors the countdown but keeps the
        // sub-counter idle until a trigger. Index resets to 0 here (APU was off).
        self.pos = 0;
        self.high = false;
        self.sample_countdown = COUNTER_DISABLED;
        self.delay = 0;
        self.sample_surpressed = false;
        self.did_tick = false;
        self.just_reloaded = false;
        self.last_pos_cc = self.cc;
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

    /// SameBoy `GB_apu_run` square tick loop (Core/apu.c ~959). Advances the duty
    /// countdown by the 2 MHz cycles elapsed since `last_pos_cc`. On each underflow
    /// the sample index increments and the countdown reloads to `(2047-freq)*2+1`;
    /// the `delay` set at trigger is consumed first (before the countdown loop),
    /// which is what phases the trigger→first-edge to hardware.
    fn update_pos(&mut self) {
        let cc = self.cc;
        // How many 2 MHz cycles have elapsed since we last advanced the duty.
        let mut cycles_left = cc.wrapping_sub(self.last_pos_cc);
        self.last_pos_cc = cc;
        // SameBoy only ticks the duty while the channel is active (is_active[i]).
        // While inactive the index/countdown freeze; we keep `last_pos_cc` current
        // (done above) so a later trigger doesn't replay the idle span.
        if !self.master || self.sample_countdown == COUNTER_DISABLED {
            return;
        }
        // A zero-cycle advance (a write landing on the same cc a prior dot already
        // resolved) neither ticks nor changes the reload phase, so preserve the
        // existing `just_reloaded` rather than spuriously asserting it. In SameBoy
        // `just_reloaded` reflects the last non-empty batch.
        if cycles_left == 0 {
            return;
        }
        // SameBoy: `delay` (trigger phase offset) is consumed off the front.
        if self.delay != 0 {
            if self.delay < cycles_left {
                self.delay = 0;
            } else {
                self.delay -= cycles_left;
            }
        }
        // SameBoy: `while (cycles_left > sample_countdown) { ... }`.
        while cycles_left > self.sample_countdown {
            cycles_left -= self.sample_countdown + 1;
            self.sample_countdown = (self.sample_length() ^ 0x7FF) * 2 + 1;
            self.pos = (self.pos + 1) & 7;
            self.sample_surpressed = false;
            self.did_tick = true;
            self.high = duty_out(self.duty(), self.pos);
        }
        self.just_reloaded = cycles_left == 0;
        self.sample_countdown -= cycles_left;
    }

    /// The 11-bit sample length == the raw frequency (SameBoy `sample_length`).
    fn sample_length(&self) -> u32 {
        self.freq() as u32
    }

    fn set_freq(&mut self, new_freq: u16) {
        self.update_pos();
        self.period = to_period(new_freq);
    }

    /// SameBoy NR13/NR23 write (Core/apu.c ~1796): update the sample length low
    /// byte (the register is already stored) and, if the countdown JUST reloaded
    /// this cycle, re-derive it from the new length so the running tone tracks the
    /// freq change immediately. Otherwise the new length takes effect on the next
    /// reload (the countdown keeps running).
    fn write_nrx3(&mut self) {
        self.update_pos();
        self.period = to_period(self.freq());
        if self.just_reloaded {
            self.sample_countdown = (self.sample_length() ^ 0x7FF) * 2 + 1;
        }
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
        // Both channels need the CGB-features flag (the trigger pre-increment
        // quirk is CGB-D/E only); ch1 also uses it for the sweep nr4Init phase.
        self.cgb = _mmio.is_cgb_features_enabled();
        // Always keep the duty's `last_pos_cc` current (update_pos advances the
        // index only while active, but must track cc even when idle so a later
        // trigger doesn't replay the idle span).
        self.update_pos();
        if !self.master {
            return;
        }

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

        // Catch the duty unit up to the write cc before touching the frequency
        // (SameBoy runs GB_apu_run before the register write).
        self.update_pos();

        // SameBoy NRx4 step-back quirk (Core/apu.c ~1814): when the sample length
        // changes from ≥$700 to <$700 on a NON-trigger write of an active channel,
        // the index steps back one (compensating a same-cycle would-be tick). CGB-D/E
        // apply it unconditionally; older revs only when countdown bit 0 is set.
        if !trigger && self.master && (old_nr4 & 0x7) == 7 && (value & 7) != 7 {
            // CGB-D/E: unconditional (older revs gate on `sample_countdown & 1`).
            if self.did_tick
                && self.sample_countdown >> 1 == (self.sample_length() ^ 0x7FF)
            {
                self.pos = (self.pos.wrapping_sub(1)) & 7;
                self.sample_surpressed = false;
                self.high = duty_out(self.duty(), self.pos);
            }
        }

        self.length_nr4_change(old_nr4, value, trigger);
        self.length_enabled = value & 0x40 != 0;

        if self.channel1 {
            self.nr14 = value;
        } else {
            self.nr24 = value;
        }
        self.period = to_period(self.freq());

        // SameBoy: `just_reloaded` reload from the new sample length.
        if self.just_reloaded {
            self.sample_countdown = (self.sample_length() ^ 0x7FF) * 2 + 1;
        }

        // dutyUnit/envelope nr4 handling happens on trigger.
        if trigger {
            self.trigger();
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;

        // Length-counter reload + reschedule is handled in `length_nr4_change`.

        // SameBoy `is_active[index]` before the trigger = the channel was already
        // playing (DAC on + previously triggered). `master` carries that here.
        let was_active = self.master;

        // Catch the duty unit up to the trigger cc (SameBoy runs GB_apu_run before
        // the register write) so the countdown/index reflect the exact trigger cc.
        self.update_pos();

        // Envelope: nr4Init sets volume + counter; master = DAC on.
        let dac_off = self.env_nr4_init();

        // Duty period from the (possibly just-written) frequency.
        self.period = to_period(self.freq());

        // SameBoy NRx4 trigger (Core/apu.c ~1833): the duty countdown/delay place
        // the first edge at the hardware-accurate phase. `sample_length` == freq.
        // `current_sample_index` (pos) is NOT reset — it persists across triggers.
        // The reload base `(sl^0x7FF)*2` plus `delay` (6-lf_div fresh / 4-lf_div
        // when the channel was already active — "sound starts 2 ticks earlier")
        // is the SameBoy trigger→first-edge model the SameSuite align/delay/duty
        // tests validate on cgb04c.
        //
        // SameBoy additionally models a CGB-D/E trigger pre-increment quirk (steps
        // the index forward on trigger when NRx4 bit 2 is clear and a countdown bit
        // is unset). Enabling it gives ZERO SameSuite gain yet regresses 16 gambatte
        // cgb04c/dmg08 duty-pos-pattern tests (also real-hardware oracles) — on the
        // cases rustyboi exercises, cgb04c shows no pre-increment — so it is omitted.
        let sl = self.sample_length();
        self.did_tick = false;
        self.delay = if was_active {
            4u32.wrapping_sub(self.lf_div)
        } else {
            6u32.wrapping_sub(self.lf_div)
        };
        self.sample_countdown = (sl ^ 0x7FF) * 2 + self.delay;

        self.master = !dac_off;
        // Recompute the cached high state for the current index (SameBoy calls
        // update_square_sample; while surpressed the digital output reads 0).
        self.high = duty_out(self.duty(), self.pos);

        // Fresh trigger with the DAC on surpresses the first output until the first
        // duty tick clears it (SameBoy `sample_surpressed`).
        if !dac_off && !was_active {
            self.sample_surpressed = true;
        }

        // Frequency sweep (Channel 1 only) — Gambatte cc-driven SweepUnit.
        if self.channel1 {
            self.sweep_nr4_init();
        }

        if dac_off {
            self.enabled = false;
        }
    }

    pub fn get_output(&self) -> f32 {
        if !self.enabled || !self.master || self.volume == 0 || self.sample_surpressed {
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

    /// CGB PCM12 nibble for this square channel. Reports SameBoy's `samples[index]`
    /// digital amplitude: 0 while the DAC is off (`!master`) or the fresh-trigger
    /// output is still surpressed (SameBoy `sample_surpressed`); otherwise the
    /// current duty high-state times the envelope volume.
    pub fn pcm_nibble(&self) -> u8 {
        if !self.master || self.sample_surpressed {
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
                            self.write_nrx3();
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
                            self.write_nrx3();
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

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
    enabled: bool, // SameBoy `is_active[GB_NOISE]` (NR52 status bit)
    length_counter: u8,
    volume: u8,
    volume_direction: bool,
    volume_timer: u8,
    length_enabled: bool,
    fs_step: u8,

    // --- Envelope (SameBoy div-anchored model, see square.rs) ---
    #[serde(default)]
    volume_countdown: u8,
    #[serde(default)]
    env_clock: bool,
    #[serde(default)]
    env_should_lock: bool,
    #[serde(default)]
    env_locked: bool,
    // cc of the last NR44 trigger (frame-boundary race window, see square.rs).
    #[serde(default = "len_disabled")]
    env_trigger_cc: u32,

    // Free-running 2 MHz cycle counter (controller cc), pushed each sync;
    // drives the cc-based length expiry and the ripple-counter advance.
    #[serde(default)]
    cc: u32,
    // Absolute cc of length expiry (Gambatte `LengthCounter::counter_`).
    #[serde(default = "len_disabled")]
    len_counter: u32,
    #[serde(default)]
    len_cc: u32,

    // --- SameBoy ripple-counter noise model (Core/apu.c noise_channel) ---
    // 14-bit ripple counter incremented every `divisor` 2 MHz cycles; the
    // LFSR steps on the RISING edge of counter bit (nr43 >> 4). This is what
    // makes frequency changes glitch-accurate (the counter keeps running
    // through NR43 writes) and is the model SameSuite's channel_4 tests are
    // validated against.
    #[serde(default)]
    counter: u16,
    // 2 MHz cycles until the next counter increment.
    #[serde(default)]
    counter_countdown: u32,
    // Free-running 2 MHz cycle count since APU power-on (SameBoy
    // `noise_channel.alignment`): accumulates in `run_batch` from elapsed
    // cycles only, so the DIV-reset/speed-switch cc folds never touch it.
    #[serde(default)]
    alignment: u32,
    // 7-bit LFSR width (NR43 bit 3).
    #[serde(default)]
    narrow: bool,
    // SameBoy-convention 15-bit LFSR: starts at 0, feedback
    // `(lfsr ^ (lfsr>>1) ^ 1) & 1` into the high bit(s); output = bit 0.
    #[serde(default)]
    lfsr: u16,
    // Latched output bit (SameBoy `current_lfsr_sample`).
    #[serde(default)]
    current_sample: bool,
    // SameBoy `noise_counter_active` / `noise_background_counter_active`:
    // the ripple counter runs while either is set (background counting keeps
    // the counter alive after DAC-off, with trigger-time glitches).
    #[serde(default)]
    counter_active: bool,
    #[serde(default)]
    background_active: bool,
    // SameBoy `countdown_reloaded` / `did_step_counter` write-quirk flags.
    #[serde(default)]
    countdown_reloaded: bool,
    #[serde(default)]
    did_step_counter: bool,
    // SameBoy `lfsr_stepped_in_narrow` / `lfsr_bit_7_before_step`: LFSR-width
    // switch history for the CGB-E NR43 category-1 glitch (see nr43_glitch_write).
    #[serde(default)]
    lfsr_stepped_in_narrow: bool,
    #[serde(default)]
    lfsr_bit7_before_step: bool,
    // DMG-only delayed channel start (SameBoy `dmg_delayed_start`).
    #[serde(default)]
    dmg_delayed_start: u32,
    // SameBoy `noise_started_with_dac_disabled`.
    #[serde(default)]
    started_with_dac_off: bool,
    // The 2 MHz cc up to which the ripple counter has been advanced.
    #[serde(default)]
    last_run_cc: u32,
    // CGB (vs DMG) and double-speed flags, pushed by the controller.
    #[serde(default)]
    cgb: bool,
    #[serde(default)]
    ds: bool,
    // CGB-D/E APU revision gate (SameBoy `model > GB_MODEL_CGB_C`): drops the
    // <=CGB-C divisor-0 even-alignment DS trigger-countdown +2 (see nr44).
    #[serde(default)]
    cgb_de: bool,
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
            length_enabled: false,
            fs_step: 0,
            volume_countdown: 0,
            env_clock: false,
            env_should_lock: false,
            env_locked: false,
            env_trigger_cc: LEN_DISABLED,
            cc: 0,
            len_counter: LEN_DISABLED,
            len_cc: 0,
            counter: 0,
            counter_countdown: 0,
            alignment: 0,
            narrow: false,
            lfsr: 0,
            current_sample: false,
            counter_active: false,
            background_active: false,
            countdown_reloaded: false,
            did_step_counter: false,
            lfsr_stepped_in_narrow: false,
            lfsr_bit7_before_step: false,
            dmg_delayed_start: 0,
            started_with_dac_off: false,
            last_run_cc: 0,
            cgb: false,
            ds: false,
            cgb_de: false,
        }
    }

    pub fn set_cc(&mut self, cc: u32) {
        self.cc = cc;
    }

    pub fn set_len_cc(&mut self, cc: u32) {
        self.len_cc = cc;
    }

    pub fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    pub fn set_ds(&mut self, ds: bool) {
        self.ds = ds;
    }

    /// CGB-D/E APU revision gate (SameBoy `model > GB_MODEL_CGB_C`).
    pub fn set_cgb_de(&mut self, de: bool) {
        self.cgb_de = de;
    }

    pub fn len_expired(&self) -> bool {
        self.len_cc >= self.len_counter
    }

    /// SameBoy `GB_apu_init` noise state (the APU struct memset at the NR52
    /// 0->1 power-on): the ripple counter, its alignment and the LFSR restart
    /// from zero. The length counter is preserved (handled by the caller's
    /// register-zeroing path).
    pub fn psg_reset(&mut self) {
        self.lfsr = 0;
        self.current_sample = false;
        self.counter = 0;
        self.counter_countdown = 0;
        self.alignment = 0;
        self.narrow = false;
        self.counter_active = false;
        self.background_active = false;
        self.countdown_reloaded = false;
        self.did_step_counter = false;
        self.lfsr_stepped_in_narrow = false;
        self.lfsr_bit7_before_step = false;
        self.dmg_delayed_start = 0;
        self.started_with_dac_off = false;
        self.last_run_cc = self.cc;
        self.volume = 0;
        self.volume_timer = 0;
        self.volume_countdown = 0;
        self.env_clock = false;
        self.env_should_lock = false;
        self.env_locked = false;
        self.enabled = false;
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
        self.advance();
    }

    /// Resolve the ripple counter/LFSR to the current cc for the CPU read
    /// path.
    pub fn sync_for_read(&mut self) {
        self.advance();
    }

    /// SameBoy `resetCc` equivalent for a DIV write: the controller folds the
    /// master cc; shift the run anchor so the elapsed-cycle stream (and thus
    /// `alignment`) is unaffected.
    pub fn reset_cc(&mut self, _cc: u32, delta: u32) {
        self.advance();
        self.last_run_cc = self.last_run_cc.wrapping_sub(delta);
    }

    pub fn step_frame_sequencer(&mut self, _step: u8) {
        // Length is a cc-driven absolute expiry event (see `length_event`);
        // the envelope is DIV-APU-event driven (env_div_tick /
        // env_secondary_reload, dispatched by the controller).
    }

    // --- Envelope (SameBoy div-anchored model; see square.rs for the
    // sibling implementation and event cadence) ---

    fn is_active(&self) -> bool {
        self.enabled
    }

    fn set_env_clock(&mut self, value: bool, direction: bool, volume: u8) {
        if self.env_clock == value {
            return;
        }
        if value {
            self.env_clock = true;
            self.env_should_lock =
                (volume == 0xF && direction) || (volume == 0x0 && !direction);
        } else {
            self.env_clock = false;
            self.env_locked |= self.env_should_lock;
        }
    }

    /// SameBoy `_nrx2_glitch` ("zombie mode") for NR42.
    fn nrx2_glitch_step(&mut self, value: u8, old: u8) {
        if self.env_clock {
            self.volume_countdown = value & 7;
        }
        let mut should_tick = (value & 7) != 0 && (old & 7) == 0 && !self.env_locked;
        let should_invert = ((value & 8) ^ (old & 8)) != 0;

        if (value & 0xF) == 8 && (old & 0xF) == 8 && !self.env_locked {
            should_tick = true;
        }

        if should_invert {
            if value & 8 != 0 {
                if (old & 7) == 0 && !self.env_locked {
                    self.volume ^= 0xF;
                } else {
                    self.volume = 0xEu8.wrapping_sub(self.volume) & 0xF;
                }
                should_tick = false;
            } else {
                self.volume = 0x10u8.wrapping_sub(self.volume) & 0xF;
            }
        }
        if should_tick {
            if value & 8 != 0 {
                self.volume = self.volume.wrapping_add(1) & 0xF;
            } else {
                self.volume = self.volume.wrapping_sub(1) & 0xF;
            }
        } else if (value & 7) == 0 && self.env_clock {
            self.set_env_clock(false, false, 0);
        }
    }

    fn nrx2_glitch(&mut self, value: u8, old: u8) {
        if self.cgb {
            self.nrx2_glitch_step(value, old);
        } else {
            self.nrx2_glitch_step(0xFF, old);
            self.nrx2_glitch_step(value, 0xFF);
        }
    }

    /// DIV-APU event leg (div_divider & 7 == 7, 64 Hz). A trigger 2 cc or
    /// less before the boundary escapes this decrement (see square.rs).
    pub fn env_frame_countdown(&mut self, event_cc: u32) {
        if event_cc.wrapping_sub(self.env_trigger_cc) <= 2 {
            return;
        }
        if !self.env_clock {
            self.volume_countdown = self.volume_countdown.wrapping_sub(1) & 7;
        }
    }

    /// DIV-APU secondary event (rising edge, 512 Hz).
    pub fn env_secondary_reload(&mut self) {
        if self.is_active() && self.volume_countdown == 0 {
            let nr2 = self.nr42;
            self.volume_countdown = nr2 & 7;
            let vol = self.volume;
            self.set_env_clock(self.volume_countdown != 0, nr2 & 8 != 0, vol);
        }
    }

    /// DIV-APU event: consume an armed tick (SameBoy `tick_noise_envelope`).
    pub fn env_div_tick(&mut self) {
        if !self.env_clock {
            return;
        }
        self.set_env_clock(false, false, 0);
        if self.env_locked {
            return;
        }
        let nr2 = self.nr42;
        if nr2 & 7 == 0 {
            return;
        }
        if nr2 & 8 != 0 {
            self.volume = self.volume.wrapping_add(1) & 0xF;
        } else {
            self.volume = self.volume.wrapping_sub(1) & 0xF;
        }
    }

    // --- SameBoy ripple-counter machinery (Core/apu.c GB_apu_run noise) ---

    /// Advance the ripple counter/LFSR to the current `cc`.
    fn advance(&mut self) {
        let cc = self.cc;
        let cycles = cc.wrapping_sub(self.last_run_cc);
        if cycles == 0 || cycles >= 0x8000_0000 {
            return;
        }
        self.last_run_cc = cc;
        self.run_cycles(cycles);
    }

    fn run_cycles(&mut self, mut cycles: u32) {
        // DMG delayed channel start (SameBoy `dmg_delayed_start` handling in
        // GB_apu_run): the NR44 trigger takes effect 6 cycles later; the
        // batch is split at the crossing and NR44 is re-applied there.
        if self.dmg_delayed_start > 0 {
            if self.dmg_delayed_start > cycles {
                self.dmg_delayed_start -= cycles;
            } else {
                let head = self.dmg_delayed_start;
                self.dmg_delayed_start = 0;
                cycles -= head;
                self.run_batch(head);
                let nr44 = self.nr44 | 0x80;
                self.write_nrx4(nr44);
                if cycles == 0 {
                    return;
                }
            }
        }
        self.run_batch(cycles);
    }

    fn run_batch(&mut self, cycles: u32) {
        self.alignment = self.alignment.wrapping_add(cycles);
        if !(self.counter_active || self.background_active) {
            return;
        }
        let mut divisor = ((self.nr43 & 0x07) as u32) << 2;
        if divisor == 0 {
            divisor = 2;
        }
        if self.counter_countdown == 0 {
            self.counter_countdown = divisor;
        }
        let mut cycles_left = cycles;
        while cycles_left >= self.counter_countdown {
            cycles_left -= self.counter_countdown;
            self.counter_countdown = divisor;
            let mask = 1u16 << (self.nr43 >> 4);
            let old_bit = self.counter & mask != 0;
            self.counter = self.counter.wrapping_add(1) & 0x3FFF;
            self.did_step_counter = true;
            let new_bit = self.counter & mask != 0;
            if new_bit && !old_bit && self.enabled {
                self.step_lfsr();
            }
        }
        if cycles_left > 0 {
            self.counter_countdown -= cycles_left;
            self.countdown_reloaded = false;
        } else {
            self.countdown_reloaded = true;
        }
    }

    /// SameBoy `update_lfsr`: refresh the latched output bit from LFSR bit 0
    /// (used by the NR43 glitch paths that mutate the LFSR without stepping).
    fn update_lfsr(&mut self) {
        self.current_sample = self.lfsr & 1 != 0;
    }

    /// SameBoy `step_lfsr`: feedback `(lfsr ^ lfsr>>1 ^ 1) & 1` into bit 14
    /// (and bit 6 in narrow mode); output = bit 0.
    fn step_lfsr(&mut self) {
        self.lfsr_bit7_before_step = self.lfsr & 0x80 != 0;
        let high_mask: u16 = if self.narrow { 0x4040 } else { 0x4000 };
        let new_high = (self.lfsr ^ (self.lfsr >> 1) ^ 1) & 1 != 0;
        self.lfsr >>= 1;
        if new_high {
            self.lfsr |= high_mask;
        } else {
            // Relevant when switching LFSR widths.
            self.lfsr &= !high_mask;
        }
        self.update_lfsr();
        self.lfsr_stepped_in_narrow = self.narrow;
    }

    /// SameBoy `prepare_noise_start` (Core/apu.c), CGB-C-and-older + DMG
    /// paths (cgb04c/dmg08 are the hardware oracles; AGB and CGB-D/E-specific
    /// branches omitted). Reloads the counter countdown with the
    /// alignment-dependent trigger phase and reseeds the LFSR.
    fn prepare_noise_start(&mut self) {
        self.counter_active = self.nr42 & 0xF8 != 0;
        let was_started_with_dac_off = self.started_with_dac_off;
        self.started_with_dac_off = !self.counter_active;
        let mut divisor = (self.nr43 & 0x07) as u32;
        let was_background = self.background_active;
        self.background_active = true;
        let mut instant_step = false;
        let mut div_1_glitch = false;

        if divisor > 1 && self.counter_countdown == 1 {
            self.counter = self.counter.wrapping_add(1) & 0x3FFF;
        } else if divisor > 1 && self.counter_countdown == 2 && self.enabled && self.ds {
            // model <= CGB_C in double speed
            self.counter = self.counter.wrapping_add(1) & 0x3FFF;
        } else if self.counter_countdown == 2 && (self.alignment & 3) == 0 && self.enabled {
            if divisor == 0 {
                divisor = 8;
            } else if divisor == 1 {
                if !self.did_step_counter {
                    div_1_glitch = true;
                }
                let mask = 1u16 << (self.nr43 >> 4);
                let old_bit = self.counter & mask != 0;
                self.counter = self.counter.wrapping_add(1) & 0x3FFF;
                let new_bit = self.counter & mask != 0;
                if new_bit && !old_bit {
                    instant_step = true;
                }
            }
        }
        let mut countdown: i32 = if divisor == 0 { 6 } else { divisor as i32 * 4 + 6 };
        if self.alignment & 1 != 0 {
            if divisor == 0 {
                // model <= CGB_C (DMG uses the was_background variants, but
                // <=C is the cgb04c behavior; DMG08's r=0 path matches +1)
                countdown += 1;
            } else if self.alignment & 2 != 0 {
                if divisor == 1 && !self.enabled {
                    countdown += 1;
                } else {
                    countdown -= 3;
                }
            } else {
                countdown -= 1;
                if divisor == 1 && self.enabled {
                    countdown -= 4;
                }
            }
        } else if divisor != 0 {
            if self.alignment & 2 != 0 {
                if self.ds && divisor == 1 {
                    countdown += 2; // <= CGB_C in double speed
                } else {
                    countdown -= 2;
                }
            } else if divisor > 1 && !self.ds {
                countdown -= 4;
            } else if divisor == 1 && self.enabled && (self.nr43 & 0xF0) == 0 {
                countdown -= 4;
            }
        } else if self.ds && !self.cgb_de {
            // divisor 0, even alignment, double speed: SameBoy adds 2 here on
            // model <= CGB_C; CGB-D/E hardware does not (SameSuite
            // channel_4_align \2-even rows pin the shorter countdown on its
            // CPU-CGB-E silicon — that suite runs under Hardware::CGBE. No
            // gambatte cgb04c capture reaches this path, so the C-side +2 is
            // pinned by SameBoy's revision gate alone).
            countdown += 2;
        }

        // Background counting glitches.
        if divisor > 1 {
            if !self.counter_active && (self.alignment & 3) == 0 {
                countdown += 4;
            }
        } else if was_background && !self.enabled && (self.alignment & 3) == 0 {
            if divisor == 0 {
                if was_started_with_dac_off {
                    countdown += 28;
                }
            } else {
                countdown -= 4;
            }
        }
        if divisor == 0 && was_background && !self.enabled && self.ds {
            countdown -= 1; // <= CGB_C
        }
        if div_1_glitch {
            countdown -= 4;
        }
        self.counter_countdown = countdown.max(1) as u32;

        if divisor == 0 && self.enabled && (self.alignment & 3) == 3 {
            // Confirmed-but-unexplained constant on real hardware (SameBoy).
            self.lfsr = 0x0055;
        } else {
            self.lfsr = 0;
        }
        if instant_step {
            self.step_lfsr();
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
        self.advance();
        let dac_off =
            self.get_envelope_initial_volume() == 0 && !self.get_envelope_direction();

        // SameBoy NR44 trigger: the envelope clock unlock/disarm happens
        // unconditionally (before the DMG delayed-start deferral).
        self.env_locked = false;
        self.env_clock = false;

        // DMG-only: an unaligned trigger is deferred 6 cycles (SameBoy
        // `dmg_delayed_start`); the whole start runs at the crossing.
        if !self.cgb && (self.alignment & 3) != 0 && self.dmg_delayed_start == 0 {
            self.dmg_delayed_start = 6;
            return;
        }

        self.lfsr = 0;
        self.prepare_noise_start();
        self.current_sample = false;

        // Volume envelope init (SameBoy: reload volume + countdown from NR42).
        self.volume = self.get_envelope_initial_volume();
        self.volume_direction = self.get_envelope_direction();
        self.volume_timer = self.get_envelope_period();
        self.volume_countdown = self.nr42 & 7;
        self.env_trigger_cc = self.cc;

        self.did_step_counter = (self.alignment & 3) == 2;

        if dac_off {
            self.enabled = false;
        } else {
            self.enabled = true;
        }
    }

    /// SameBoy master `nr43_write` (Core/apu.c, the issue-#397 rework),
    /// CGB-E model: an NR43 write whose frequency nibble changes sends glitch
    /// signals to the LFSR, categorized by the (old_bit, new_bit, glitch_bit)
    /// triple read from the ripple counter. `glitch_bit` reads the counter at
    /// an intermediate register value mixing the old low bits with the new
    /// top bit. CGB-D maps, the AGB approximation and the <=CGB-C FF-
    /// intermediate leg are omitted (see `write_nr43` — only `cgb_de` routes
    /// here; SameSuite channel_4_freq_change is the rev=cgbe pin).
    fn nr43_glitch_write(&mut self, new: u8) {
        let old_narrow = self.narrow;
        self.narrow = new & 8 != 0;
        let old = self.nr43;
        self.nr43 = new;

        if (old & 0xF0) == (new & 0xF0) {
            return;
        }

        let effective_counter = self.counter;
        let old_bit = (effective_counter >> (old >> 4)) & 1 != 0;
        let glitch_value = (old & 0x7F) | (new & 0x80);
        let glitch_bit = (effective_counter >> (glitch_value >> 4)) & 1 != 0;
        let new_bit = (effective_counter >> (new >> 4)) & 1 != 0;

        if old_bit == new_bit && new_bit != glitch_bit {
            // Glitching write. Two categories; these are SameBoy's
            // deterministic common variants.
            if new_bit {
                // Category 1.
                if new & 0x80 == 0 {
                    self.step_lfsr();
                } else {
                    // Only happens under this odd condition (SameBoy).
                    let t1 = (old >> 4) & 7;
                    let t2 = (new >> 4) & 7;
                    if (t1 ^ 7) + t2 > 7 || ((t1 ^ 7) & t2) != 0 {
                        // Copy bit 8 to bit 7.
                        self.lfsr = (self.lfsr & !0x80) | ((self.lfsr >> 1) & 0x80);
                        // The specific cases below have non-deterministic
                        // variants; these are SameBoy's deterministic picks.
                        if (t1 == 0 || t1 == 4) && t2 == 3 {
                            self.lfsr &= (self.lfsr >> 1) | 0x545;
                            self.update_lfsr();
                        } else if t1 == 2 && t2 == 3 {
                            let mut mask: u16 = 0x555;
                            if self.lfsr & 0xC == 0xC {
                                mask |= 8;
                            }
                            if self.lfsr & 0xC00 == 0xC00 {
                                mask |= 0x800;
                            }
                            self.lfsr &= (self.lfsr >> 1) | mask;
                            self.update_lfsr();
                        }
                        if !self.narrow && old_narrow && self.lfsr_stepped_in_narrow {
                            if self.lfsr_bit7_before_step {
                                self.lfsr |= 0x40;
                            } else {
                                self.lfsr &= !0x40;
                            }
                        }
                        self.lfsr |= if self.narrow { 0x4040 } else { 0x4000 };
                        self.lfsr_stepped_in_narrow = self.narrow;
                    }
                }
            } else {
                // Category 2: transition-indexed glitch map,
                // glitch_map[old_high*8 + new_high], only for NR43 bit 7 set.
                const GLITCH_MAP: [u8; 64] = [
                    0, 0, 4, 2, 2, 2, 0, 0, // old high 0, new high 0..7
                    0, 0, 2, 4, 2, 2, 0, 0, // 1
                    1, 2, 0, 1, 5, 3, 0, 0, // 2
                    0, 0, 0, 0, 2, 2, 0, 0, // 3
                    0, 2, 2, 2, 0, 0, 0, 0, // 4
                    6, 0, 2, 2, 0, 0, 0, 0, // 5
                    0, 0, 0, 0, 0, 0, 0, 0, // 6
                    0, 0, 0, 0, 0, 0, 0, 0, // 7
                ];
                let glitch = if new & 0x80 != 0 {
                    GLITCH_MAP[(((old & 0x70) >> 1) | ((new & 0x70) >> 4)) as usize]
                } else {
                    0
                };
                match glitch {
                    1 | 6 => {
                        // Step, followed by bit1 &= bit0 (glitch 6 first
                        // unsets a mid bit under specific patterns).
                        self.step_lfsr();
                        if glitch == 6 {
                            if (self.narrow && (self.lfsr & 0x71) == 0x20)
                                || (self.lfsr & 0x71) == 0x61
                            {
                                self.lfsr &= !0x20;
                            }
                            if (self.lfsr & 0x7001) == 0x2000
                                || (self.lfsr & 0x7001) == 0x6001
                            {
                                self.lfsr &= !0x2000;
                            }
                        }
                        if (self.lfsr & 0x3) == 2 {
                            self.lfsr &= !2;
                        }
                    }
                    2 => {
                        // Step, bitwise AND with previous, except bit 0.
                        let prev = self.lfsr;
                        self.step_lfsr();
                        self.lfsr &= prev | 1;
                    }
                    3 | 5 => {
                        // No step; bit0 = bit1 (glitch 5 first applies the
                        // non-deterministic high-bit/bit-3 variants).
                        if glitch == 5 {
                            if (self.lfsr & 0x3) == 2 {
                                self.lfsr &=
                                    if self.narrow { !0x4040 } else { !0x4000 };
                            }
                            if (self.lfsr & 0x19) == 8 {
                                self.lfsr &= !8;
                            }
                        }
                        self.lfsr &= !1;
                        self.lfsr |= (self.lfsr >> 1) & 1;
                        self.update_lfsr();
                        self.lfsr_stepped_in_narrow = self.narrow;
                    }
                    4 => {
                        // Step, bit1 &= bit0, LFSR bit-1 &= LFSR bit.
                        let prev = self.lfsr;
                        self.step_lfsr();
                        self.lfsr &= prev | if self.narrow { !0x2022 } else { !0x2002 };
                    }
                    _ => {
                        self.step_lfsr();
                    }
                }
            }
        } else if !old_bit && new_bit {
            self.step_lfsr();
        }
    }

    /// SameBoy NR43 write (`GB_apu_write` case GB_IO_NR43): if the counter
    /// countdown just reloaded, re-derive it from the new divisor with the
    /// alignment-phase table ({2,1,4,3} on <=CGB-C, {2,1,0,3} on D/E), then
    /// run the revision write model. The ripple counter itself keeps running
    /// (frequency changes are inherently glitch-accurate).
    ///
    /// CGB-E (`cgb_de`, SameSuite's validation silicon) takes the master-
    /// SameBoy `nr43_write` glitch categories — SameSuite channel_4_freq_-
    /// change (rev=cgbe) pins all 64 subtests of it.
    ///
    /// <=CGB-C keeps only the reload-coincident LFSR step glitch. SameBoy
    /// master additionally routes every <=CGB-C write through an $FF
    /// intermediate value ("all writes go through 3 intermediate values" on
    /// pre-CGB-D silicon), but its own comments call the pre-D behavior
    /// non-deterministic and instance-specific ("only 100% accurate in CGB-E
    /// mode"), and porting that leg regresses the C-model SameSuite rows
    /// channel_4_lfsr_15_7 / channel_4_lfsr_7_15 (measured: the $FF
    /// intermediate makes every width-only NR43 write glitch, since the $F
    /// top nibble always differs). Our C-side oracles (gambatte cgb04c
    /// captures + those rows) pin the glitch-free behavior, so the FF leg is
    /// deliberately not applied.
    fn write_nr43(&mut self, value: u8) {
        self.advance();
        if self.countdown_reloaded {
            let mut divisor = ((value & 0x07) as u32) << 2;
            if divisor == 0 {
                divisor = 2;
            }
            let table: [u32; 4] = if self.cgb_de { [2, 1, 0, 3] } else { [2, 1, 4, 3] };
            let adj = if divisor == 2 {
                0
            } else {
                table[(self.alignment & 3) as usize]
            };
            self.counter_countdown = divisor + adj;
        }
        if self.cgb_de {
            self.nr43_glitch_write(value);
            return;
        }
        if self.countdown_reloaded {
            // <= CGB_C: reload-coincident bit-transition glitch (RAW counter,
            // not the effective OR-decrement view).
            let old_bit = (self.counter >> (self.nr43 >> 4)) & 1 != 0;
            let glitch_bit = (self.counter >> 7) & 1 != 0;
            let new_bit = (self.counter >> (value >> 4)) & 1 != 0;
            if !old_bit && new_bit && glitch_bit {
                let prev = self.counter.wrapping_sub(1) & 0x3FFF;
                let p_old = (prev >> (self.nr43 >> 4)) & 1 != 0;
                let p_glitch = (prev >> 7) & 1 != 0;
                let p_new = (prev >> (value >> 4)) & 1 != 0;
                if p_old && !p_new && p_glitch {
                    self.step_lfsr();
                }
            }
        }
        self.narrow = value & 0x08 != 0;
        self.nr43 = value;
    }

    pub fn get_output(&self) -> f32 {
        if !self.enabled || self.volume == 0 {
            return 0.0;
        }
        if self.current_sample {
            (self.volume as f32) / 15.0
        } else {
            0.0
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// CGB PCM34 high nibble for the noise channel: the latched LFSR output
    /// bit times the envelope volume while the channel is active.
    pub fn pcm_nibble(&self) -> u8 {
        if !self.enabled {
            return 0;
        }
        if self.current_sample {
            self.volume & 0x0F
        } else {
            0
        }
    }

    /// PCM34 nibble resolved at the canonical CPU READ access cc (M7) via a
    /// non-mutating shadow advance of the ripple counter, mirroring the
    /// squares' `pcm_nibble_at`.
    pub fn pcm_nibble_at(&self, read_cc: u32) -> u8 {
        if !self.enabled {
            return 0;
        }
        let mut sample = self.current_sample;
        let cycles = read_cc.wrapping_sub(self.last_run_cc);
        if cycles > 0
            && cycles < 0x8000_0000
            && (self.counter_active || self.background_active)
            && self.dmg_delayed_start == 0
        {
            let mut divisor = ((self.nr43 & 0x07) as u32) << 2;
            if divisor == 0 {
                divisor = 2;
            }
            let mut countdown = self.counter_countdown;
            if countdown == 0 {
                countdown = divisor;
            }
            let mut counter = self.counter;
            let mut lfsr = self.lfsr;
            let mut cycles_left = cycles;
            let mask = 1u16 << (self.nr43 >> 4);
            let high_mask: u16 = if self.narrow { 0x4040 } else { 0x4000 };
            while cycles_left >= countdown {
                cycles_left -= countdown;
                countdown = divisor;
                let old_bit = counter & mask != 0;
                counter = counter.wrapping_add(1) & 0x3FFF;
                let new_bit = counter & mask != 0;
                if new_bit && !old_bit {
                    let new_high = (lfsr ^ (lfsr >> 1) ^ 1) & 1 != 0;
                    lfsr >>= 1;
                    if new_high {
                        lfsr |= high_mask;
                    } else {
                        lfsr &= !high_mask;
                    }
                    sample = lfsr & 1 != 0;
                }
            }
        }
        if sample {
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
                        self.advance();
                        if value & 0xF8 == 0 {
                            // DAC off (SameBoy NR42 case): a running channel
                            // with a nonzero divisor gets a counter nudge and
                            // stops the background counting.
                            if self.enabled && self.nr43 & 0x07 != 0 {
                                if self.counter_countdown <= 2 {
                                    self.counter = self.counter.wrapping_add(1) & 0x3FFF;
                                }
                                self.background_active = false;
                            }
                            self.enabled = false;
                            self.counter_active = false;
                        } else if self.is_active() {
                            // SameBoy: NR42 write on a playing channel applies
                            // the zombie-mode volume transform.
                            let old = self.nr42;
                            self.nrx2_glitch(value, old);
                        }
                        self.nr42 = value;
                    }
                    NR43 => {
                        self.write_nr43(value);
                    }
                    NR44 => {
                        self.advance();
                        self.write_nrx4(value);
                    }
                    _ => {}
                }
            }
            _ => panic!("Invalid address for Noise: {:#X}", addr)
        }
    }
}

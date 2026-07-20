use serde::{Deserialize, Serialize};
use crate::audio::{NR41, NR42, NR43, NR44};
use crate::memory::Addressable;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Noise {
    // Sound channel registers
    nr41: u8, // Sound length
    nr42: u8, // Volume envelope
    nr43: u8, // Frequency and randomness
    nr44: u8, // Control

    // Internal state
    enabled: bool, // NR52 status bit (channel active)
    length_counter: u8,
    volume: u8,
    volume_direction: bool,
    volume_timer: u8,
    length_enabled: bool,
    fs_step: u8,

    // --- Envelope (DIV-anchored model, see square.rs) ---
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
    // Absolute cc of length expiry.
    #[serde(default = "len_disabled")]
    len_counter: u32,
    #[serde(default)]
    len_cc: u32,

    // --- Ripple-counter noise model ---
    // 14-bit ripple counter incremented every `divisor` 2 MHz cycles; the
    // LFSR steps on the RISING edge of counter bit (nr43 >> 4). This is what
    // makes frequency changes glitch-accurate (the counter keeps running
    // through NR43 writes) and is the model SameSuite's channel_4 tests are
    // validated against.
    #[serde(default)]
    counter: u16,
    // 2 MHz cycles until the next counter increment.
    #[serde(default)]
    ripple_countdown: u32,
    // Free-running 2 MHz cycle count since APU power-on (the LFSR
    // alignment): accumulates in `run_batch` from elapsed
    // cycles only, so the DIV-reset/speed-switch cc folds never touch it.
    #[serde(default)]
    alignment: u32,
    // 7-bit LFSR width (NR43 bit 3).
    #[serde(default)]
    narrow: bool,
    // 15-bit LFSR: starts at 0, feedback
    // `(lfsr ^ (lfsr>>1) ^ 1) & 1` into the high bit(s); output = bit 0.
    #[serde(default)]
    lfsr: u16,
    // Latched output bit.
    #[serde(default)]
    current_sample: bool,
    // Counter-active / background-counter-active:
    // the ripple counter runs while either is set (background counting keeps
    // the counter alive after DAC-off, with trigger-time glitches).
    #[serde(default)]
    ripple_active: bool,
    #[serde(default)]
    background_active: bool,
    // Countdown-reloaded / did-step-counter write-quirk flags.
    #[serde(default)]
    countdown_reloaded: bool,
    #[serde(default)]
    did_step_counter: bool,
    // LFSR-width switch history for the CGB-E NR43 category-1 glitch (see
    // nr43_glitch_write).
    #[serde(default)]
    lfsr_stepped_in_narrow: bool,
    #[serde(default)]
    lfsr_bit7_before_step: bool,
    // DMG-only delayed channel start.
    #[serde(default)]
    dmg_delayed_start: u32,
    // True only while re-applying the deferred NR44 trigger from `run_cycles`,
    // so `trigger` starts the channel instead of re-arming another deferral
    // (the delayed start is a one-shot). Not serialized: it never spans
    // an instruction boundary.
    #[serde(skip)]
    in_deferred_reapply: bool,
    // Whether the noise channel started with its DAC disabled.
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
    // CGB-D/E APU revision gate (model newer than CGB-C): drops the
    // <=CGB-C divisor-0 even-alignment DS trigger-countdown +2 (see nr44).
    #[serde(default)]
    cgb_de: bool,
    // CGB-B-or-earlier APU revision gate (see `len_nr4_change`).
    #[serde(default)]
    cgb_le_b: bool,
}

const LEN_DISABLED: u32 = 0xFFFF_FFFF;

fn len_disabled() -> u32 {
    LEN_DISABLED
}

impl Noise {
    pub(super) fn new() -> Self {
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
            ripple_countdown: 0,
            alignment: 0,
            narrow: false,
            lfsr: 0,
            current_sample: false,
            ripple_active: false,
            background_active: false,
            countdown_reloaded: false,
            did_step_counter: false,
            lfsr_stepped_in_narrow: false,
            lfsr_bit7_before_step: false,
            dmg_delayed_start: 0,
            in_deferred_reapply: false,
            started_with_dac_off: false,
            last_run_cc: 0,
            cgb: false,
            ds: false,
            cgb_de: false,
            cgb_le_b: false,
        }
    }

    pub(super) fn set_cc(&mut self, cc: u32) {
        self.cc = cc;
    }

    pub(super) fn set_len_cc(&mut self, cc: u32) {
        self.len_cc = cc;
    }

    pub(super) fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    pub(super) fn set_ds(&mut self, ds: bool) {
        self.ds = ds;
    }

    /// CGB-D/E APU revision gate (model newer than CGB-C).
    pub(super) fn set_cgb_de(&mut self, de: bool) {
        self.cgb_de = de;
    }

    /// CGB-B-or-earlier APU revision gate (CGB with model <= CGB-B).
    pub(super) fn set_cgb_le_b(&mut self, le_b: bool) {
        self.cgb_le_b = le_b;
    }

    pub(super) fn len_expired(&self) -> bool {
        self.len_cc >= self.len_counter
    }

    /// Directly seed the hidden length counter (post-boot state seeding).
    pub fn set_length_counter(&mut self, value: u8) {
        self.length_counter = value;
    }

    /// APU-init noise state (the APU struct reset at the NR52
    /// 0->1 power-on): the ripple counter, its alignment and the LFSR restart
    /// from zero. The length counter is preserved (handled by the caller's
    /// register-zeroing path).
    pub(super) fn psg_reset(&mut self) {
        self.lfsr = 0;
        self.current_sample = false;
        self.counter = 0;
        self.ripple_countdown = 0;
        self.alignment = 0;
        self.narrow = false;
        self.ripple_active = false;
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

    pub(super) fn set_fs_step(&mut self, step: u8) {
        self.fs_step = step;
    }

    /// Master-clock epoch rebase: shift every absolute-cc anchor down by
    /// `delta`. `alignment` is an elapsed-cycle accumulator (wrap-safe by
    /// design) and the counter/LFSR state is cc-free, so only the cc anchors
    /// move.
    pub fn epoch_fold(&mut self, delta: u32) {
        self.cc = self.cc.wrapping_sub(delta);
        self.len_cc = self.len_cc.wrapping_sub(delta);
        self.last_run_cc = self.last_run_cc.wrapping_sub(delta);
        if self.env_trigger_cc != LEN_DISABLED {
            self.env_trigger_cc = self.env_trigger_cc.wrapping_sub(delta);
        }
        if self.len_counter != LEN_DISABLED {
            self.len_counter = self.len_counter.wrapping_sub(delta);
        }
    }

    const LEN_MASK: u16 = 0x3F;

    /// Length-counter expiry for channel 4.
    pub(super) fn length_event(&mut self) {
        self.len_counter = LEN_DISABLED;
        self.length_counter = 0;
        self.enabled = false;
    }

    /// Length-counter NR41 write handling (channel 4).
    fn len_nr1_change(&mut self, value: u8) {
        self.length_counter = ((!value as u16 & Self::LEN_MASK) + 1) as u8;
        self.len_counter = if self.nr44 & 0x40 != 0 {
            ((self.len_cc >> 13) + self.length_counter as u32) << 13
        } else {
            LEN_DISABLED
        };
    }

    /// Length-counter NR44 write handling (channel 4).
    fn len_nr4_change(&mut self, old_nr4: u8, new_nr4: u8) {
        if self.len_counter != LEN_DISABLED {
            self.length_counter =
                (self.len_counter >> 13).wrapping_sub(self.len_cc >> 13) as u8;
        }
        let mut dec: u8 = 0;
        // CGB-B and older: extra length clock regardless of the written bit-6
        // value (CGB-B-or-earlier revision; SameSuite
        // channel_4_extra_length_clocking-cgb0B).
        if new_nr4 & 0x40 != 0 || self.cgb_le_b {
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
            ((self.len_cc >> 13) + self.length_counter as u32) << 13
        } else {
            LEN_DISABLED
        };
    }

    pub(super) fn step(&mut self) {
        self.advance();
    }

    /// Resolve the ripple counter/LFSR to the current cc for the CPU read
    /// path.
    pub(super) fn sync_for_read(&mut self) {
        self.advance();
    }

    /// DIV-write cc reset: the controller folds the
    /// master cc; shift the run anchor so the elapsed-cycle stream (and thus
    /// `alignment`) is unaffected.
    pub(super) fn reset_cc(&mut self, _cc: u32, delta: u32) {
        self.advance();
        self.last_run_cc = self.last_run_cc.wrapping_sub(delta);
    }

    // --- Envelope (DIV-anchored model; see square.rs for the
    // sibling implementation and event cadence) ---

    fn is_active(&self) -> bool {
        self.enabled
    }

    /// NR42 (the envelope's NRx2 register), named uniformly for the shared
    /// envelope helpers.
    fn nr2(&self) -> u8 {
        self.nr42
    }

    // The six NRx2 envelope helpers are shared byte-for-byte with the square
    // channel; see audio/envelope.rs.
    crate::audio::envelope::impl_envelope_unit!();

    // --- Ripple-counter machinery ---

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
        // DMG delayed channel start: the NR44 trigger takes effect 6 cycles
        // later; the
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
                // The deferred re-application is the trigger *taking effect* — it
                // must actually start the channel, never re-arm another 6-cycle
                // deferral. rustyboi re-applies at the exact 6-cc crossing (so the
                // ripple counter/LFSR start at the right cc), where `alignment`
                // has advanced by only the 6 deferred cycles; `6 & 3 == 2` flips
                // only bit 1, so an odd `alignment & 3` stays odd and the re-apply
                // would re-defer forever. (The hardware oracle avoids this
                // because its start-ch4 re-write fires at the end of the run,
                // after `alignment += cycles` over the whole scheduler quantum,
                // landing on a varied alignment.) A game that re-triggers ch4
                // every frame
                // at such a sub-alignment — Pokémon R/B/Y's GameFreak-intro
                // drumroll — otherwise has every trigger's `env_clock = false`
                // clear the envelope every 6 cc, so the volume never steps and the
                // noise channel latches into a continuous buzz instead of a
                // decaying drum hit. `in_deferred_reapply` makes the crossing a
                // one-shot start (never a re-defer), matching hardware/CGB and the
                // SameSuite channel_4 delay/alignment tests.
                self.in_deferred_reapply = true;
                self.write_nrx4(nr44);
                self.in_deferred_reapply = false;
                if cycles == 0 {
                    return;
                }
            }
        }
        self.run_batch(cycles);
    }

    fn run_batch(&mut self, cycles: u32) {
        self.alignment = self.alignment.wrapping_add(cycles);
        if !(self.ripple_active || self.background_active) {
            return;
        }
        let mut divisor = ((self.nr43 & 0x07) as u32) << 2;
        if divisor == 0 {
            divisor = 2;
        }
        if self.ripple_countdown == 0 {
            self.ripple_countdown = divisor;
        }
        let mut cycles_left = cycles;
        while cycles_left >= self.ripple_countdown {
            cycles_left -= self.ripple_countdown;
            self.ripple_countdown = divisor;
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
            self.ripple_countdown -= cycles_left;
            self.countdown_reloaded = false;
        } else {
            self.countdown_reloaded = true;
        }
    }

    /// Refresh the latched output bit from LFSR bit 0
    /// (used by the NR43 glitch paths that mutate the LFSR without stepping).
    fn update_lfsr(&mut self) {
        self.current_sample = self.lfsr & 1 != 0;
    }

    /// Step the LFSR: feedback `(lfsr ^ lfsr>>1 ^ 1) & 1` into bit 14
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

    /// Prepare noise start, CGB-C-and-older + DMG
    /// paths (cgb04c/dmg08 are the hardware oracles; AGB and CGB-D/E-specific
    /// branches omitted). Reloads the counter countdown with the
    /// alignment-dependent trigger phase and reseeds the LFSR.
    fn prepare_noise_start(&mut self) {
        self.ripple_active = self.nr42 & 0xF8 != 0;
        let was_started_with_dac_off = self.started_with_dac_off;
        self.started_with_dac_off = !self.ripple_active;
        let mut divisor = (self.nr43 & 0x07) as u32;
        let was_background = self.background_active;
        self.background_active = true;
        let mut instant_step = false;
        let mut div_1_glitch = false;

        if divisor > 1 && self.ripple_countdown == 1 {
            self.counter = self.counter.wrapping_add(1) & 0x3FFF;
        } else if divisor > 1 && self.ripple_countdown == 2 && self.enabled && self.ds {
            // <=CGB_C double-speed behavior, deliberately ungated (applied to
            // all revisions): no D/E oracle reaches this path, so the D/E fork
            // is omitted pending hardware captures — not a missing `!cgb_de`.
            self.counter = self.counter.wrapping_add(1) & 0x3FFF;
        } else if self.ripple_countdown == 2 && (self.alignment & 3) == 0 && self.enabled {
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
                // <=CGB_C-derived (cgb04c) behavior, deliberately ungated:
                // DMG08's r=0 path matches the +1 too, and no D/E oracle
                // discriminates this path — the D/E fork is omitted pending
                // hardware captures, not a missing `!cgb_de`.
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
                    // <=CGB_C double-speed behavior, deliberately ungated (all
                    // revisions): D/E fork omitted pending oracles.
                    countdown += 2;
                } else {
                    countdown -= 2;
                }
            } else if (divisor > 1 && !self.ds) || (divisor == 1 && self.enabled && (self.nr43 & 0xF0) == 0) {
                countdown -= 4;
            }
        } else if self.ds && !self.cgb_de {
            // divisor 0, even alignment, double speed: the hardware oracle adds
            // 2 here on model <= CGB_C; CGB-D/E hardware does not (SameSuite
            // channel_4_align \2-even rows pin the shorter countdown on its
            // CPU-CGB-E silicon — that suite runs under Hardware::CGBE. No
            // cgb04c capture reaches this path, so the C-side +2 is
            // pinned by the revision gate alone).
            countdown += 2;
        }

        // Background counting glitches.
        if divisor > 1 {
            if !self.ripple_active && (self.alignment & 3) == 0 {
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
            // <=CGB_C behavior, deliberately ungated (all revisions): D/E fork
            // omitted pending oracles.
            countdown -= 1;
        }
        if div_1_glitch {
            countdown -= 4;
        }
        self.ripple_countdown = countdown.max(1) as u32;

        if divisor == 0 && self.enabled && (self.alignment & 3) == 3 {
            // Confirmed-but-unexplained constant on real hardware.
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

        // NR44 trigger: the envelope clock unlock/disarm happens
        // unconditionally (before the DMG delayed-start deferral).
        self.env_locked = false;
        self.env_clock = false;

        // DMG-only: an unaligned trigger is deferred 6 cycles; the whole
        // start runs at the crossing. The
        // deferred re-application (`in_deferred_reapply`) is that crossing: it
        // must start the channel, not re-arm the deferral (see `run_cycles`).
        if !self.cgb
            && (self.alignment & 3) != 0
            && self.dmg_delayed_start == 0
            && !self.in_deferred_reapply
        {
            self.dmg_delayed_start = 6;
            return;
        }

        self.lfsr = 0;
        self.prepare_noise_start();
        self.current_sample = false;

        // Volume envelope init (reload volume + countdown from NR42).
        self.volume = self.get_envelope_initial_volume();
        self.volume_direction = self.get_envelope_direction();
        self.volume_timer = self.get_envelope_period();
        self.volume_countdown = self.nr42 & 7;
        self.env_trigger_cc = self.cc;

        self.did_step_counter = (self.alignment & 3) == 2;

        self.enabled = !dac_off;
    }

    /// NR43-write glitch model,
    /// CGB-E: an NR43 write whose frequency nibble changes sends glitch
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
            // Glitching write. Two categories; these are the oracle's
            // deterministic common variants.
            if new_bit {
                // Category 1.
                if new & 0x80 == 0 {
                    self.step_lfsr();
                } else {
                    // Only happens under this odd condition.
                    let t1 = (old >> 4) & 7;
                    let t2 = (new >> 4) & 7;
                    if (t1 ^ 7) + t2 > 7 || ((t1 ^ 7) & t2) != 0 {
                        // Copy bit 8 to bit 7.
                        self.lfsr = (self.lfsr & !0x80) | ((self.lfsr >> 1) & 0x80);
                        // The specific cases below have non-deterministic
                        // variants; these are the oracle's deterministic picks.
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

    /// NR43 write: if the counter
    /// countdown just reloaded, re-derive it from the new divisor with the
    /// alignment-phase table ({2,1,4,3} on <=CGB-C, {2,1,0,3} on D/E), then
    /// run the revision write model. The ripple counter itself keeps running
    /// (frequency changes are inherently glitch-accurate).
    ///
    /// CGB-E (`cgb_de`, SameSuite's validation silicon) takes the full
    /// `nr43_glitch_write` glitch categories — SameSuite channel_4_freq_-
    /// change (rev=cgbe) pins all 64 subtests of it.
    ///
    /// <=CGB-C keeps only the reload-coincident LFSR step glitch. The reference
    /// model additionally routes every <=CGB-C write through an $FF
    /// intermediate value ("all writes go through 3 intermediate values" on
    /// pre-CGB-D silicon), but the pre-D behavior is
    /// non-deterministic and instance-specific ("only 100% accurate in CGB-E
    /// mode"), and porting that leg regresses the C-model SameSuite rows
    /// channel_4_lfsr_15_7 / channel_4_lfsr_7_15 (measured: the $FF
    /// intermediate makes every width-only NR43 write glitch, since the $F
    /// top nibble always differs). Our C-side oracles (cgb04c
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
            self.ripple_countdown = divisor + adj;
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

    pub(super) fn get_output(&self) -> f32 {
        if !self.enabled || self.volume == 0 {
            return 0.0;
        }
        if self.current_sample {
            (self.volume as f32) / 15.0
        } else {
            0.0
        }
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// CGB PCM34 high nibble for the noise channel: the latched LFSR output
    /// bit times the envelope volume while the channel is active.
    pub(super) fn pcm_nibble(&self) -> u8 {
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
    pub(super) fn pcm_nibble_at(&self, read_cc: u32) -> u8 {
        if !self.enabled {
            return 0;
        }
        let mut sample = self.current_sample;
        let cycles = read_cc.wrapping_sub(self.last_run_cc);
        if cycles > 0
            && cycles < 0x8000_0000
            && (self.ripple_active || self.background_active)
            && self.dmg_delayed_start == 0
        {
            let mut divisor = ((self.nr43 & 0x07) as u32) << 2;
            if divisor == 0 {
                divisor = 2;
            }
            let mut countdown = self.ripple_countdown;
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
                            // DAC off (NR42 case): a running channel
                            // with a nonzero divisor gets a counter nudge and
                            // stops the background counting.
                            if self.enabled && self.nr43 & 0x07 != 0 {
                                if self.ripple_countdown <= 2 {
                                    self.counter = self.counter.wrapping_add(1) & 0x3FFF;
                                }
                                self.background_active = false;
                            }
                            self.enabled = false;
                            self.ripple_active = false;
                        } else if self.is_active() {
                            // NR42 write on a playing channel applies
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

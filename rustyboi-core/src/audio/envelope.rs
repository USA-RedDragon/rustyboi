//! Shared NRx2 volume-envelope unit (SameBoy div-anchored model, Core/apu.c).
//!
//! The square and noise channels drive byte-identical envelope logic over the
//! same five fields (`env_clock`, `env_should_lock`, `env_locked`, `volume`,
//! `volume_countdown`) plus `env_trigger_cc` and the `cgb` flag. Rather than
//! move those fields into a sub-struct (which would renest every savestate JSON
//! key and touch ~100 mixing/trigger/DAC access sites), this macro emits the
//! six shared helpers directly into each channel `impl`. Both expansions are
//! the same tokens, so the two channels stay bit-for-bit identical by
//! construction. Each host `impl` must provide `nr2(&self) -> u8` and
//! `is_active(&self) -> bool` and carry the six envelope fields plus `cgb`.

macro_rules! impl_envelope_unit {
    () => {
        /// SameBoy `set_envelope_clock`.
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

        /// SameBoy `_nrx2_glitch` ("zombie mode"): the volume transform an NRx2
        /// write applies to a playing channel.
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

        /// SameBoy `nrx2_glitch`: CGB-D/E apply the transform once; older
        /// revisions (and DMG) pass through an FF intermediate value, applying
        /// it twice. rustyboi's CGB target follows the SameSuite-calibrated D/E
        /// behavior; DMG takes the pre-CGB double application.
        fn nrx2_glitch(&mut self, value: u8, old: u8) {
            if self.cgb {
                self.nrx2_glitch_step(value, old);
            } else {
                self.nrx2_glitch_step(0xFF, old);
                self.nrx2_glitch_step(value, 0xFF);
            }
        }

        /// DIV-APU event leg (div_divider & 7 == 7, 64 Hz): the envelope frame
        /// countdown decrements (mod 8) while no tick is armed. A trigger 2 cc
        /// or less before the boundary shares the event's hardware M-cycle: the
        /// fresh countdown escapes this decrement (see fs_div_event_at).
        pub fn env_frame_countdown(&mut self, event_cc: u32) {
            if event_cc.wrapping_sub(self.env_trigger_cc) <= 2 {
                return;
            }
            if !self.env_clock {
                self.volume_countdown = self.volume_countdown.wrapping_sub(1) & 7;
            }
        }

        /// DIV-APU secondary event (rising edge, 512 Hz): a zero countdown on an
        /// active channel reloads from NRx2 and arms the tick for the next event.
        pub fn env_secondary_reload(&mut self) {
            if self.is_active() && self.volume_countdown == 0 {
                let nr2 = self.nr2();
                self.volume_countdown = nr2 & 7;
                let vol = self.volume;
                self.set_env_clock(self.volume_countdown != 0, nr2 & 8 != 0, vol);
            }
        }

        /// DIV-APU event: consume an armed tick (SameBoy `tick_*_envelope`).
        pub fn env_div_tick(&mut self) {
            if !self.env_clock {
                return;
            }
            self.set_env_clock(false, false, 0);
            if self.env_locked {
                return;
            }
            let nr2 = self.nr2();
            if nr2 & 7 == 0 {
                return;
            }
            if nr2 & 8 != 0 {
                self.volume = self.volume.wrapping_add(1) & 0xF;
            } else {
                self.volume = self.volume.wrapping_sub(1) & 0xF;
            }
        }
    };
}

pub(crate) use impl_envelope_unit;

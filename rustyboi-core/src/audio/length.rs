//! Shared NRx1/NRx4 length-counter unit (cc-event hardware model).
//!
//! All four channels run the same length unit: NRx1 loads `mask + 1 - value`,
//! NRx4 re-derives the counter from the scheduled expiry, applies the
//! length-enable extra-clock glitch, applies the trigger reload, and
//! reschedules. Only three things actually differ between channels — the NRx1
//! mask (`0x3F` for square/noise, `0xFF` for wave), the counter width (`u16`
//! for square/wave, `u8` for noise, which is what the savestate pins), and two
//! per-channel hooks — so the ~90 lines were written out three times and any
//! length fix had to be applied three times.
//!
//! Following the [`super::envelope`] pattern, this macro emits the six shared
//! helpers directly into each channel `impl` rather than moving the fields into
//! a sub-struct: the fields stay where they are, so the bincode layout is
//! untouched. Each host `impl` must carry `length_counter: $counter`,
//! `len_counter: u32`, `len_cc: u32` and `cgb_le_b: bool`, and provide:
//!
//! - `fn nr4(&self) -> u8` — the channel's NRx4 register byte.
//! - the `$on_disable` hook — channel teardown when the length counter hits
//!   zero. Square/noise clear `enabled`; wave additionally drops `master` and
//!   disarms its fetch counter.
//! - the `$pre_dec` hook — a chance to modify the extra-clock `dec` before it
//!   is applied. Wave uses it for the CPU-CGB-A/B first-glitch-write swallow;
//!   square and noise return `dec` unchanged.

/// Disarmed sentinel for every absolute-cc event counter in the APU. `u32::MAX`
/// is unreachable as a real cc because the controller folds the clock epoch
/// long before the counter can approach it.
pub(crate) const COUNTER_DISABLED: u32 = 0xFFFF_FFFF;

/// serde default for a disarmed counter. Default-fn paths are not part of the
/// wire format, so the three channels sharing one is layout-neutral.
pub(crate) fn counter_disabled() -> u32 {
    COUNTER_DISABLED
}

macro_rules! impl_length_unit {
    (mask: $mask:expr, counter: $counter:ty, on_disable: $on_disable:ident, pre_dec: $pre_dec:ident $(,)?) => {
        /// Advance the length subsystem's phased cc (length keys on `cc >> 13`
        /// boundaries, which are phased differently from the duty/envelope cc).
        pub(super) fn set_len_cc(&mut self, cc: u32) {
            self.len_cc = cc;
        }

        /// Whether the scheduled expiry cc has been reached.
        pub(super) fn len_expired(&self) -> bool {
            self.len_cc >= self.len_counter
        }

        /// Directly seed the hidden length counter (post-boot state seeding).
        pub(super) fn set_length_counter(&mut self, value: $counter) {
            self.length_counter = value;
        }

        /// Length-counter expiry: disarms the schedule and tears the channel
        /// down through the per-channel hook.
        pub(super) fn length_event(&mut self) {
            self.len_counter = crate::audio::length::COUNTER_DISABLED;
            self.length_counter = 0;
            self.$on_disable();
        }

        /// NRx1 write: reload the length counter and (re)schedule the absolute
        /// expiry cc from the current NRx4 length-enable bit.
        fn len_nr1_change(&mut self, value: u8) {
            self.length_counter = (!value as $counter & $mask) + 1;
            self.len_counter = if self.nr4() & 0x40 != 0 {
                ((self.len_cc >> 13) + self.length_counter as u32) << 13
            } else {
                crate::audio::length::COUNTER_DISABLED
            };
        }

        /// NRx4 write: re-derive the length counter from the absolute expiry
        /// cc, apply the length-enable `dec = ~cc>>12 & 1` extra-clock quirk
        /// and the trigger reload, then reschedule the absolute expiry.
        fn len_nr4_change(&mut self, old_nr4: u8, new_nr4: u8) {
            if self.len_counter != crate::audio::length::COUNTER_DISABLED {
                self.length_counter =
                    ((self.len_counter >> 13).wrapping_sub(self.len_cc >> 13)) as $counter;
            }

            let mut dec: $counter = 0;
            // CGB-B and older: the length-enable extra-clock glitch fires
            // regardless of the written bit-6 value (`(value & 0x40) ||
            // (cgb && model <= CGB_B)` — "current value is irrelevant";
            // SameSuite channel_*_extra_length_clocking-cgb0B).
            if new_nr4 & 0x40 != 0 || self.cgb_le_b {
                dec = ((!self.len_cc >> 12) & 1) as $counter;
                dec = self.$pre_dec(new_nr4, dec);
                if old_nr4 & 0x40 == 0 && self.length_counter != 0 {
                    self.length_counter -= dec;
                    if self.length_counter == 0 {
                        self.$on_disable();
                    }
                }
            }

            if new_nr4 & 0x80 != 0 && self.length_counter == 0 {
                self.length_counter = $mask + 1 - dec;
            }

            self.len_counter = if new_nr4 & 0x40 != 0 && self.length_counter != 0 {
                ((self.len_cc >> 13) + self.length_counter as u32) << 13
            } else {
                crate::audio::length::COUNTER_DISABLED
            };
        }
    };
}

pub(crate) use impl_length_unit;

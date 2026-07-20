//! The APU's analog stage: the per-channel DACs, the DAC-off fade, and the
//! output high-pass filter — everything between the digital generators and the
//! host's speaker.
//!
//! Pan Docs (Audio details): each generator emits a digital `$0..$F` that its
//! DAC "linearly translate[s] to the analog range -1 to 1 … the slope is
//! negative: 'digital 0' maps to 'analog 1'". A *deactivated channel* still
//! feeds its still-enabled DAC a digital 0, so it sits at analog +1 rather than
//! at silence; a *disabled DAC* instead "fades to an analog value of 0, which
//! corresponds to 'digital 7.5'", and "the nature of this fade is not entirely
//! deterministic and varies between models". Both stereo outputs then pass a
//! high-pass filter, which is what keeps those DC offsets from parking the
//! speaker off-centre.
//!
//! The charge factors come from blargg's "Game Boy Sound Hardware" (gbdev wiki):
//! the per-4194304 Hz-cycle factor is 0.999958 on DMG and 0.998943 on MGB & CGB,
//! and "the charge factor can be calculated for any output sampling rate as
//! 0.999958^(4194304/rate)".

use crate::audio::controller::HOST_SAMPLE_RATE;
use crate::gb::DMG_CPU_HZ;

/// Digital `0..=15` to the DAC's analog output. Negative slope: digital 0 is
/// analog +1, digital 15 is analog -1, and the (unreachable) digital 7.5 is 0.
pub(super) fn dac_analog(digital: u8) -> f32 {
    (7.5 - digital as f32) / 7.5
}

/// Per-cycle charge factor on DMG-family silicon (blargg: DMG-03/05/06).
const DMG_CHARGE_PER_CYCLE: f32 = 0.999958;
/// Per-cycle charge factor on MGB & CGB silicon (blargg: MGB-01, CGB-02/04/05).
const CGB_CHARGE_PER_CYCLE: f32 = 0.998943;

/// Which high-pass the machine wires to its output. Pan Docs only orders the
/// three families ("more aggressive on GBA than on GBC, which itself is more
/// aggressive than on DMG"); blargg publishes the constants for the first two.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum AnalogModel {
    #[default]
    Dmg,
    CgbMgb,
    Agb,
}

impl AnalogModel {
    fn charge_per_cycle(self) -> f32 {
        match self {
            AnalogModel::Dmg => DMG_CHARGE_PER_CYCLE,
            // No published AGB constant exists, only Pan Docs' ordering
            // (AGB more aggressive than CGB). Squaring the CGB factor doubles
            // the per-cycle decay rate in the log domain, which satisfies the
            // documented ordering without inventing false precision.
            AnalogModel::CgbMgb => CGB_CHARGE_PER_CYCLE,
            AnalogModel::Agb => CGB_CHARGE_PER_CYCLE * CGB_CHARGE_PER_CYCLE,
        }
    }

    /// The per-host-sample factor: `base^(cycles per output sample)`. The RC is
    /// a real-time network and the host rate is fixed, so the exponent is
    /// always the DMG cycle rate over [`HOST_SAMPLE_RATE`] — an SGB1's slower
    /// crystal repitches the machine, not the filter.
    fn charge_per_sample(self) -> f32 {
        self.charge_per_cycle()
            .powf(DMG_CPU_HZ as f32 / HOST_SAMPLE_RATE)
    }
}

/// Below this the fade / capacitor is snapped to exactly zero: it keeps a long
/// silent tail bit-identical (rather than an endless denormal ramp) and costs
/// nothing audible at -180 dBFS.
const FLUSH: f32 = 1e-9;

/// The analog stage's continuous state. Deliberately NOT serialized: this is
/// the charge on a physical RC network, not machine state, and every field
/// re-settles within milliseconds. Keeping it out of the savestate also keeps
/// the wire format untouched — the cost is a small transient right after a
/// load, which is inaudible next to the load itself.
#[derive(Clone)]
pub(super) struct AnalogStage {
    /// Per-host-sample charge factor for this model, shared by the fade and
    /// the high-pass (one RC family per machine).
    charge: f32,
    /// Per-channel analog level, which the DAC drives while it is on and which
    /// decays from wherever it was left once the DAC goes off.
    fade: [f32; 4],
    /// High-pass capacitors, one per stereo side.
    cap_l: f32,
    cap_r: f32,
}

impl Default for AnalogStage {
    fn default() -> Self {
        AnalogStage {
            charge: AnalogModel::Dmg.charge_per_sample(),
            fade: [0.0; 4],
            cap_l: 0.0,
            cap_r: 0.0,
        }
    }
}

impl AnalogStage {
    pub(super) fn set_model(&mut self, model: AnalogModel) {
        self.charge = model.charge_per_sample();
    }

    /// Apply the DAC-off fade. `raw` is each channel's post-DAC analog level
    /// (0.0 where the DAC is off — the endpoint, not the instantaneous value);
    /// a live DAC drives its node directly, a dead one coasts toward 0 instead
    /// of stepping there.
    pub(super) fn fade(&mut self, raw: [f32; 4], dac_on: [bool; 4]) -> [f32; 4] {
        for (i, level) in self.fade.iter_mut().enumerate() {
            if dac_on[i] {
                *level = raw[i];
            } else {
                *level *= self.charge;
                if level.abs() < FLUSH {
                    *level = 0.0;
                }
            }
        }
        self.fade
    }

    /// blargg's high_pass, one instance per stereo side: the output is the
    /// input less the capacitor, and the capacitor charges toward the input
    /// through their difference. His `dacs_enabled` hard-gate is deliberately
    /// absent — it exists to snap the output to 0 once every DAC is off, which
    /// is exactly the endpoint [`AnalogStage::fade`] already coasts to, so
    /// gating here would reintroduce the very step the fade removes.
    pub(super) fn high_pass(&mut self, left: f32, right: f32) -> (f32, f32) {
        let out_l = left - self.cap_l;
        self.cap_l = left - out_l * self.charge;
        if self.cap_l.abs() < FLUSH {
            self.cap_l = 0.0;
        }
        let out_r = right - self.cap_r;
        self.cap_r = right - out_r * self.charge;
        if self.cap_r.abs() < FLUSH {
            self.cap_r = 0.0;
        }
        (out_l, out_r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DAC's negative slope, at both rails and around the midpoint.
    #[test]
    fn dac_maps_digital_zero_to_analog_one_on_a_negative_slope() {
        assert_eq!(dac_analog(0), 1.0);
        assert_eq!(dac_analog(15), -1.0);
        assert!(dac_analog(7) > 0.0 && dac_analog(8) < 0.0, "7.5 is the zero crossing");
        for d in 1..=15u8 {
            assert!(dac_analog(d) < dac_analog(d - 1), "slope is negative at {d}");
        }
    }

    /// blargg's worked example: "if you were applying high_pass() at 44100 Hz,
    /// you'd use a charge factor of 0.996".
    #[test]
    fn dmg_charge_factor_matches_the_published_44100_hz_value() {
        let f = AnalogModel::Dmg.charge_per_sample();
        assert!((f - 0.996).abs() < 5e-4, "DMG charge at 44.1 kHz was {f}");
    }

    /// Pan Docs orders the three families by aggressiveness; a more aggressive
    /// filter pulls harder, i.e. has the SMALLER charge factor.
    #[test]
    fn charge_factors_follow_the_documented_model_ordering() {
        let dmg = AnalogModel::Dmg.charge_per_sample();
        let cgb = AnalogModel::CgbMgb.charge_per_sample();
        let agb = AnalogModel::Agb.charge_per_sample();
        assert!(agb < cgb, "AGB ({agb}) is more aggressive than CGB ({cgb})");
        assert!(cgb < dmg, "CGB ({cgb}) is more aggressive than DMG ({dmg})");
    }

    /// A constant input is a pure DC bias, which the high-pass must remove.
    #[test]
    fn high_pass_removes_a_constant_bias() {
        let mut stage = AnalogStage::default();
        let mut last = f32::INFINITY;
        for _ in 0..20_000 {
            let (l, r) = stage.high_pass(0.5, -0.25);
            assert!(l.abs() <= last, "the DC response must not grow");
            last = l.abs();
            let _ = r;
        }
        let (l, r) = stage.high_pass(0.5, -0.25);
        assert!(l.abs() < 1e-3, "left bias survived: {l}");
        assert!(r.abs() < 1e-3, "right bias survived: {r}");
    }

    /// A dead DAC coasts to 0 from where it was left; it never steps there.
    #[test]
    fn dac_off_fade_decays_monotonically_without_a_jump() {
        let mut stage = AnalogStage::default();
        // Charge channel 1 to the positive rail with its DAC on.
        let live = stage.fade([1.0, 0.0, 0.0, 0.0], [true; 4]);
        assert_eq!(live[0], 1.0);

        // Now kill it: the first sample must still be near the rail.
        let mut prev = 1.0f32;
        let mut steps = 0;
        loop {
            let out = stage.fade([0.0; 4], [false, true, true, true])[0];
            assert!(out <= prev, "fade rose ({prev} -> {out})");
            assert!(out >= 0.0, "fade overshot below 0 ({out})");
            if steps == 0 {
                assert!(out > 0.9, "fade jumped instead of coasting ({out})");
            }
            prev = out;
            steps += 1;
            if out == 0.0 {
                break;
            }
            assert!(steps < 1_000_000, "fade never reached 0");
        }
        assert!(steps > 100, "DMG fade was too abrupt ({steps} samples)");
    }
}

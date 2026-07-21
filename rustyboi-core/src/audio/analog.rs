//! The APU's analog stage: the DAC-off fade and the output high-pass filter —
//! the continuous, stateful half of what sits between the digital generators
//! and the host's speaker.
//!
//! The discrete half — the DAC transfer function itself and the stereo mixer —
//! lives in [`rustyboi_mix`], because the `.rba` replay decoder has to
//! reproduce it bit-for-bit from a recorded tap and cannot depend on this
//! crate. Everything in this file is deliberately downstream of that boundary:
//! a tap value is always one of the 16 DAC levels or `0.0`, whereas a fade ramp
//! and a filter state are not representable in the recording's palette at all.
//!
//! Pan Docs (Audio details): a *deactivated channel* still
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
use serde::{Deserialize, Serialize};

/// The DAC transfer function, re-exported at its historical path so the
/// generators keep calling `analog::dac_analog`. Defined in [`rustyboi_mix`].
pub(super) use rustyboi_mix::dac_analog;

/// Per-cycle charge factor on DMG-family silicon (blargg: DMG-03/05/06).
const DMG_CHARGE_PER_CYCLE: f32 = 0.999958;
/// Per-cycle charge factor on MGB & CGB silicon (blargg: MGB-01, CGB-02/04/05).
const CGB_CHARGE_PER_CYCLE: f32 = 0.998943;

/// Which high-pass the machine wires to its output. Pan Docs only orders the
/// three families ("more aggressive on GBA than on GBC, which itself is more
/// aggressive than on DMG"); blargg publishes the constants for the first two.
///
/// What [`AnalogModel::Agb`] deliberately does NOT model, so the next reader
/// does not mistake absence for oversight:
///
///   * **SOUNDBIAS.** Pan Docs: on GBA "sound is converted to an analog signal
///     and an offset is added (see SOUNDBIAS in GBATEK)". That offset is a
///     GBA-side register a CGB program cannot reach, and our output is already
///     centred, so adding it would only shift a DC level the high-pass removes.
///   * **SameBoy's `agb_bias_for_channel`.** It adds each channel's envelope
///     `current_volume` to that channel's sample on AGB. It carries no comment,
///     no citation, and no Pan Docs counterpart, and it is not derivable from
///     "mixing is digital" the way the modelled items are. Unverified, so
///     omitted — a bench item, not a defect.
///   * **The NR43 intermediate-value glitch.** SameBoy: "AGB behavior is very
///     glitchy and incosistent … This is a *very* rough approximation of the
///     behavior." A rough approximation of chaotic silicon is worse than a
///     clean omission; bench item.
///   * **The high-pass constant.** Pan Docs orders GBA as more aggressive than
///     GBC; pandocs issue #390 claims the GBA has no internal high-pass at all.
///     Flatly contradictory and unresolved, so the derived constant below is
///     left exactly as it was rather than guessed in either direction.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum AnalogModel {
    #[default]
    Dmg,
    CgbMgb,
    Agb,
}

impl AnalogModel {
    /// Whether this machine mixes digitally rather than through per-channel
    /// DACs. Pan Docs (Game Boy Advance audio): "Instead of mixing being done
    /// by analog circuitry, it's instead done digitally … This also means that
    /// the GBA APU has no DACs."
    pub(super) fn is_agb(self) -> bool {
        matches!(self, AnalogModel::Agb)
    }

    fn charge_per_cycle(self) -> f32 {
        match self {
            AnalogModel::Dmg => DMG_CHARGE_PER_CYCLE,
            AnalogModel::CgbMgb => CGB_CHARGE_PER_CYCLE,
            // No published AGB constant exists, only Pan Docs' ordering
            // (AGB more aggressive than CGB). Squaring the CGB factor doubles
            // the per-cycle decay rate in the log domain, which satisfies the
            // documented ordering without inventing false precision.
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

/// The analog stage's continuous state.
///
/// The charge on the RC network IS serialized. It is not "machine state" in the
/// register sense, but restoring it to a default restarts the high-pass from
/// zero, which rings out as an audible transient (~6 ms on DMG at the charge
/// factor above) on every load — and RetroArch's rewind unserializes once per
/// frame, so the default would put that transient on every rewound frame.
/// Carrying it makes a load analog-continuous.
///
/// `charge` is the exception: it is a pure function of the machine model, and
/// `Mmio::reseed_hardware_flags` re-applies it from the serialized `hardware`
/// identity on every load (via `set_analog_model`), so it is derived rather
/// than stored. It still defaults to the DMG factor rather than `0.0`, so a
/// stage that somehow escapes reseeding filters rather than collapsing.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AnalogStage {
    /// Per-host-sample charge factor for this model, shared by the fade and
    /// the high-pass (one RC family per machine).
    #[serde(skip, default = "dmg_charge_per_sample")]
    charge: f32,
    /// Which family this stage is modelling. Derived from the machine's
    /// serialized `hardware` identity by `set_model`, exactly like `charge`,
    /// so it is skipped rather than stored.
    #[serde(skip)]
    model: AnalogModel,
    /// Per-channel analog level, which the DAC drives while it is on and which
    /// decays from wherever it was left once the DAC goes off.
    #[serde(default)]
    fade: [f32; 4],
    /// High-pass capacitors, one per stereo side.
    #[serde(default)]
    cap_l: f32,
    #[serde(default)]
    cap_r: f32,
}

/// The `charge` stand-in for a stage deserialized before `set_analog_model`
/// re-derives it. Matches [`AnalogStage::default`].
fn dmg_charge_per_sample() -> f32 {
    AnalogModel::Dmg.charge_per_sample()
}

impl Default for AnalogStage {
    fn default() -> Self {
        AnalogStage {
            charge: AnalogModel::Dmg.charge_per_sample(),
            model: AnalogModel::Dmg,
            fade: [0.0; 4],
            cap_l: 0.0,
            cap_r: 0.0,
        }
    }
}

impl AnalogStage {
    pub(super) fn set_model(&mut self, model: AnalogModel) {
        self.charge = model.charge_per_sample();
        self.model = model;
    }

    pub(super) fn model(&self) -> AnalogModel {
        self.model
    }

    /// Apply the DAC-off fade. `raw` is each channel's post-DAC analog level
    /// (0.0 where the DAC is off — the endpoint, not the instantaneous value);
    /// a live DAC drives its node directly, a dead one coasts toward 0 instead
    /// of stepping there.
    pub(super) fn fade(&mut self, raw: [f32; 4], dac_on: [bool; 4]) -> [f32; 4] {
        // There is nothing to fade on AGB: the fade is the discharge of a real
        // per-channel DAC coupling capacitor, and AGB has no per-channel DACs
        // to discharge. SameBoy gates its equivalent on `<= GB_MODEL_CGB_E` for
        // the same reason. A dead "DAC" there steps straight to digital 0's
        // level, which is what makes CH3's disable a spike rather than a slew.
        if self.model.is_agb() {
            return raw;
        }
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

    // The DAC's own transfer function is tested in `rustyboi-mix`, which owns
    // it; what remains here is the stage built on top of it.

    /// blargg's worked example: "if you were applying high_pass() at 44100 Hz,
    /// you'd use a charge factor of 0.996".
    #[test]
    fn dmg_charge_factor_matches_the_published_44100_hz_value() {
        let f = AnalogModel::Dmg.charge_per_sample();
        assert!((f - 0.996).abs() < 5e-4, "DMG charge at 44.1 kHz was {f}");
    }

    /// The per-cycle factors against blargg's published figures.
    ///
    /// This replaces an ordering test (AGB < CGB < DMG) that could not fail:
    /// AGB's factor is *defined* as CGB's squared, and squaring any factor in
    /// (0, 1) shrinks it, so the ordering held for every possible value of the
    /// two constants — the CGB constant could be set to 0.5 with the whole
    /// suite still green. Pinning the published values instead is strictly
    /// stronger, since the documented ordering follows from them.
    ///
    /// AGB is the exception and is asserted only against its own definition:
    /// no measured AGB constant is published (Pan Docs gives only "more
    /// aggressive than GBC"), so there is nothing to independently assert it
    /// against. That assertion pins the documented derivation, not silicon.
    #[test]
    fn charge_factors_match_their_published_per_cycle_values() {
        assert_eq!(
            AnalogModel::Dmg.charge_per_cycle(),
            0.999958,
            "blargg, Game Boy Sound Hardware: DMG-03/05/06"
        );
        assert_eq!(
            AnalogModel::CgbMgb.charge_per_cycle(),
            0.998943,
            "blargg, Game Boy Sound Hardware: MGB-01, CGB-02/04/05"
        );
        let cgb = AnalogModel::CgbMgb.charge_per_cycle();
        assert_eq!(
            AnalogModel::Agb.charge_per_cycle(),
            cgb * cgb,
            "AGB is defined as the CGB factor squared"
        );
    }

    /// Which filter each machine wires up — asserted by nothing until now.
    ///
    /// The entry worth pinning is MGB: it is DMG-family silicon and takes the
    /// DMG side of every other model split in the codebase, but here it takes
    /// the CGB factor, because blargg measured MGB-01 with the CGB constant
    /// (see the citation on `Hardware::analog_model`). The SGBs run the other
    /// way — they feed the SNES's audio path but their Game-Boy-side APU is DMG
    /// silicon, so they keep the DMG filter.
    ///
    /// The table is written out independently rather than re-deriving from the
    /// mapping under test, and the coverage check below makes a newly added
    /// `Hardware` variant fail here until it is deliberately classified.
    #[test]
    fn every_hardware_model_maps_to_its_analog_stage() {
        use crate::gb::Hardware;
        use clap::ValueEnum;

        const EXPECTED: &[(Hardware, AnalogModel)] = &[
            (Hardware::DMG, AnalogModel::Dmg),
            (Hardware::DMG0, AnalogModel::Dmg),
            (Hardware::SGB, AnalogModel::Dmg),
            (Hardware::SGB2, AnalogModel::Dmg),
            (Hardware::MGB, AnalogModel::CgbMgb),
            (Hardware::CGB0, AnalogModel::CgbMgb),
            (Hardware::CGBB, AnalogModel::CgbMgb),
            (Hardware::CGB, AnalogModel::CgbMgb),
            (Hardware::CGBE, AnalogModel::CgbMgb),
            (Hardware::AGB, AnalogModel::Agb),
        ];

        for &(hw, want) in EXPECTED {
            assert_eq!(hw.analog_model(), want, "{hw:?} wired up the wrong analog model");
        }
        for hw in Hardware::value_variants() {
            assert!(
                EXPECTED.iter().any(|&(h, _)| h == *hw),
                "{hw:?} is unclassified here -- a new Hardware variant must \
                 pick an analog model explicitly"
            );
        }
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

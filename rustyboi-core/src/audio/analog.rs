//! Thin shim: the analog stage (DAC-off fade + output high-pass) moved to
//! [`rustyboi_mix`], because the `.rba` replay decoder must run the identical
//! fade and high-pass to reproduce the core's output byte-for-byte. The types
//! are re-exported at their historical path so the audio module compiles
//! unchanged; every citation and constant lives with the implementation in
//! `rustyboi-mix/src/analog.rs`.
//!
//! The `Hardware`-coverage test below stays here because it enumerates the
//! core's `Hardware` enum, which the leaf crate cannot see.

pub(crate) use rustyboi_mix::{AnalogModel, AnalogStage, dac_analog};

#[cfg(test)]
mod tests {
    use super::*;

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
}

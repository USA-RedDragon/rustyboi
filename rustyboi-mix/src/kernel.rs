//! Clean-room band-limited step (BLEP) kernel.
//!
//! Derived from the published math only, never from any implementation:
//!
//!   * Stilson & Smith, "Alias-Free Digital Synthesis of Classic Analog
//!     Waveforms" (ICMC 1996) — replacing a discontinuity with a band-limited
//!     step at its exact transition time removes the aliasing that
//!     point-sampling folds into the audible band.
//!   * Brandt, "Hard Sync Without Aliasing" (ICMC 2001) — precompute the step
//!     as a phase-indexed residual table (step minus its settled value), so a
//!     synthesizer adds a short correction and carries the settled level
//!     exactly.
//!   * Oppenheim & Schafer, "Discrete-Time Signal Processing" — FIR design by
//!     the window method: a windowed ideal-lowpass impulse, integrated to a
//!     step.
//!
//! Construction, all in f64 via `libm` (platform transcendentals are not
//! bit-identical across targets — same determinism rationale as `analog.rs`'s
//! `libm::powf`), with `fc = 0.42 × 44100` and `w` the 4-term Blackman-Harris
//! window (−92 dB sidelobes; coefficients from Harris 1978):
//!
//! ```text
//! h[j] = w(j / (N·P)) · (2fc/fs) · sinc((2fc/fs) · (j − N·P/2) / P),  j = 0..=N·P
//! ```
//!
//! cumulative-summed into a step, sliced per phase, normalized by the step's
//! asymptote, and stored as `residual[p][k] = S_p[k] − 1.0` in f32.
//!
//! # Phase indexing
//!
//! Row `p` is a transition at `p / PHASES` through a sample window; slot `k`
//! is the k-th output sample whose end boundary follows the transition, so it
//! reads the step at elapsed `(k·P + P − p) / P` output samples:
//!
//! ```text
//! residual[p][k] = S[k·P + P − p] / S[N·P] − 1.0
//! ```
//!
//! The step therefore begins at the transition and settles [`TAPS`] samples
//! later. The residual form is what makes steady state bit-exact on the
//! discrete DAC alphabet: the renderer assigns the new level outright and
//! scatters only `delta · residual`, which decays to exactly `0.0` once the
//! table is exhausted — no accumulated rounding survives a step.

/// Output-rate span of the band-limited step, in samples.
pub const TAPS: usize = 32;
/// Sub-sample transition resolution (P): 1/64ths of an output sample.
pub const PHASES: usize = 64;

/// The precomputed phase-indexed step-residual table. Deterministic: the same
/// bytes on every platform (pinned by the golden-hash test below).
pub struct BlepKernel {
    pub(crate) residual: [[f32; TAPS]; PHASES],
}

/// `2fc/fs` with `fc = 0.42 × 44100` and `fs = 44100`: the kernel is designed
/// in units of output samples, so only the ratio enters.
const CUTOFF_RATIO: f64 = 0.84;

/// 4-term Blackman-Harris coefficients (Harris 1978, the −92 dB window).
const BH: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let px = core::f64::consts::PI * x;
        libm::sin(px) / px
    }
}

impl BlepKernel {
    pub fn build() -> BlepKernel {
        const NP: usize = TAPS * PHASES;
        let mut step = [0.0f64; NP + 1];
        let mut acc = 0.0f64;
        for (j, s) in step.iter_mut().enumerate() {
            let x = j as f64 / NP as f64;
            let tau_x = core::f64::consts::TAU * x;
            let w = BH[0] - BH[1] * libm::cos(tau_x) + BH[2] * libm::cos(2.0 * tau_x)
                - BH[3] * libm::cos(3.0 * tau_x);
            let t = (j as f64 - (NP / 2) as f64) / PHASES as f64;
            acc += w * CUTOFF_RATIO * sinc(CUTOFF_RATIO * t);
            *s = acc;
        }
        let asymptote = step[NP];
        let mut residual = [[0.0f32; TAPS]; PHASES];
        for (p, row) in residual.iter_mut().enumerate() {
            for (k, r) in row.iter_mut().enumerate() {
                *r = (step[k * PHASES + PHASES - p] / asymptote - 1.0) as f32;
            }
        }
        BlepKernel { residual }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analog::AnalogModel;
    use crate::dac_analog;
    use crate::render::{NO_TRANSITION, Renderer, SampleRecord};

    fn fnv1a64(hash: &mut u64, byte: u8) {
        *hash ^= byte as u64;
        *hash = hash.wrapping_mul(0x100_0000_01b3);
    }

    /// The table's exact bytes, pinned. This is the cross-platform determinism
    /// gate: any toolchain, target, or `libm` drift that changes a single bit
    /// of the kernel — and with it the audio hash of every recording — fails
    /// here instead of surfacing as a baseline mismatch.
    #[test]
    fn the_built_table_matches_its_pinned_golden_hash() {
        let kernel = BlepKernel::build();
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for row in &kernel.residual {
            for value in row {
                for byte in value.to_le_bytes() {
                    fnv1a64(&mut hash, byte);
                }
            }
        }
        assert_eq!(hash, 0xafb2_d7ed_6f0a_7e27, "BLEP kernel bytes drifted");
    }

    /// Every phase row must have settled by its last tap: what remains beyond
    /// the table is carried by the renderer's `level` alone.
    #[test]
    fn every_phase_row_settles_by_its_last_tap() {
        let kernel = BlepKernel::build();
        for (p, row) in kernel.residual.iter().enumerate() {
            let tail = row[TAPS - 1].abs();
            assert!(tail < 1e-4, "phase {p} still {tail} from its asymptote");
        }
    }

    /// One step through the renderer settles bit-exactly on the target level:
    /// the residual form's whole point (see the module doc).
    #[test]
    fn a_step_settles_bit_exactly_through_the_renderer() {
        let kernel = BlepKernel::build();
        for phase in [0u8, 17, 37, 63] {
            let mut r = Renderer::new(AnalogModel::Dmg);
            let target = dac_analog(9);
            let mut rec = SampleRecord {
                levels: [target, 0.0, 0.0, 0.0],
                phases: [phase, NO_TRANSITION, NO_TRANSITION, NO_TRANSITION],
                nr50: 0x77,
                nr51: 0x11,
                enabled: true,
            };
            r.render(&kernel, &rec);
            rec.phases[0] = NO_TRANSITION;
            for _ in 0..TAPS + 1 {
                r.render(&kernel, &rec);
            }
            assert_eq!(
                r.chans[0].level.to_bits(),
                target.to_bits(),
                "phase {phase}: settled level is not the target"
            );
            for (slot, v) in r.chans[0].ring.iter().enumerate() {
                assert_eq!(
                    v.to_bits(),
                    0.0f32.to_bits(),
                    "phase {phase}: ring slot {slot} kept residue"
                );
            }
        }
    }

    /// Stopband of the reconstructed step's first difference (the band-limited
    /// impulse), via an in-test DFT at the oversampled rate `PHASES × 44100`.
    ///
    /// The aliasing-relevant stopband is asserted at −90 dB: every component
    /// at or above `fs − 20 kHz = 24.1 kHz` — the entire region that can fold
    /// onto the audible 0–20 kHz band from any image, measured across the full
    /// oversampled spectrum (≈ −107 dB here). At exactly `0.5·fs` the kernel
    /// is still on its transition-band skirt (≈ −43 dB, asserted at −40): a
    /// 32-tap Blackman-Harris design has a mainlobe half-width of
    /// `4/TAPS = 0.125·fs`, so with `fc = 0.42·fs` the −90 dB floor is
    /// physically reachable only above ≈ `0.545·fs` — energy in that skirt
    /// folds onto 20–22.05 kHz, outside audibility.
    #[test]
    fn the_reconstructed_step_is_quiet_in_the_folding_stopband() {
        const NP: usize = TAPS * PHASES;
        let kernel = BlepKernel::build();

        let mut step = [0.0f64; NP + 1];
        for (j, s) in step.iter_mut().enumerate().skip(1) {
            let o = (j - 1) % PHASES + 1;
            let k = (j - o) / PHASES;
            *s = 1.0 + kernel.residual[PHASES - o][k] as f64;
        }
        let mut diff = [0.0f64; NP + 1];
        diff[0] = step[0];
        for j in 1..=NP {
            diff[j] = step[j] - step[j - 1];
        }

        const DFT_LEN: usize = 4096;
        let fs = crate::HOST_SAMPLE_RATE as f64;
        let bin_hz = PHASES as f64 * fs / DFT_LEN as f64;
        let dc: f64 = diff.iter().sum();
        let mut worst_fold = f64::MIN;
        let mut worst_half_fs = f64::MIN;
        for b in 1..=DFT_LEN / 2 {
            let freq = b as f64 * bin_hz;
            if freq < 0.5 * fs {
                continue;
            }
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, d) in diff.iter().enumerate() {
                let angle = -core::f64::consts::TAU * b as f64 * n as f64 / DFT_LEN as f64;
                re += d * libm::cos(angle);
                im += d * libm::sin(angle);
            }
            let db = 10.0 * libm::log10((re * re + im * im) / (dc * dc));
            if freq >= fs - 20_000.0 {
                worst_fold = worst_fold.max(db);
            } else {
                worst_half_fs = worst_half_fs.max(db);
            }
        }
        assert!(worst_fold < -90.0, "audible-folding stopband floor was {worst_fold} dB");
        assert!(worst_half_fs < -40.0, "transition skirt above 0.5*fs was {worst_half_fs} dB");
    }
}

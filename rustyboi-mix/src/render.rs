//! The shared per-sample renderer: band-limited step synthesis feeding the
//! analog chain. This is the ONLY implementation of the output stage — the
//! core runs it live and the `.rba` replay decoder runs it client-side — so
//! byte-exactness of live output vs decoded replay is structural rather than
//! tested-into-existence (but tested anyway).
//!
//! # The collapse contract
//!
//! [`SampleRecord`] is the single per-sample record both producers emit and
//! this renderer consumes: per channel, the settled post-DAC level at the
//! sample window's end boundary plus the sub-sample phase of the LAST
//! level-changing edge inside the window (`P = 64` positions,
//! [`NO_TRANSITION`] when the level is unchanged). Intermediate intra-sample
//! edges deliberately collapse away. The rationale is the sampling theorem:
//! hardware's line-out carries ultrasonic harmonics a 44.1 kHz stream cannot
//! represent, and the faithful rendering of those components is band-limiting
//! — attenuating them to their in-band residue — not folding them into
//! audible aliases the way point-sampling does. Net-zero intra-sample flips
//! (ultrasonic channel parking) therefore become correctly silent.
//!
//! # Per-channel step semantics
//!
//! The level alphabet is the 16 DAC levels plus `0.0`, and on non-AGB
//! machines `0.0` IS the DAC-off marker — [`crate::dac_analog`] never returns
//! `0.0` for a digital input, so no separate DAC bit exists anywhere.
//!
//!   * `phases[i] == NO_TRANSITION` — no scatter; the channel holds.
//!   * Non-AGB, new level `0.0` (DAC off) — NO scatter and `level` keeps its
//!     stale pre-off value: the analog fade coasts the audible node toward 0,
//!     and the next DAC-on transition steps from the stale level.
//!   * Non-AGB, new level nonzero — scatter `delta = new − level` at the
//!     phase, then assign `level = new` (this covers both an ordinary
//!     transition and a DAC-on from a stale level).
//!   * AGB — always scatter and assign, `0.0` included: there are no
//!     per-channel DACs so nothing fades (see [`AnalogStage::fade`]'s gate),
//!     and a dead "DAC" steps like any other transition.
//!
//! A scatter adds `delta · residual[phase][k]` into the k-th upcoming ring
//! slot; the settled part of the step is carried by `level` itself, which is
//! what makes steady state bit-exact on the discrete alphabet (see
//! [`crate::kernel`]).
//!
//! # f32 operation order is load-bearing
//!
//! Same discipline as the crate root: every producer and consumer of this
//! chain must agree bit-for-bit on real PCM, so do not reassociate the
//! scatter / level+ring / fade / mix / high-pass sequence, and do not fuse
//! multiply-adds.

use crate::analog::{AnalogModel, AnalogStage};
use crate::kernel::{BlepKernel, TAPS};
use crate::mix_stereo;

/// Phase sentinel: the channel's level is unchanged this sample — no scatter.
pub const NO_TRANSITION: u8 = 0xFF;

/// Ring capacity: the next power of two above [`TAPS`], so slot indexing is a
/// mask and a scatter never wraps onto a slot it also reads this sample.
const RING_LEN: usize = 2 * TAPS;

/// One output sample of the shared contract (see the module doc): per-channel
/// settled levels and transition phases at the window's end boundary, plus the
/// mix registers and NR52 master enable as of that boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleRecord {
    pub levels: [f32; 4],
    pub phases: [u8; 4],
    pub nr50: u8,
    pub nr51: u8,
    pub enabled: bool,
}

/// One channel's synthesis state: the settled level and the pending
/// band-limited correction ring.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChannelSynth {
    pub(crate) level: f32,
    #[cfg_attr(feature = "serde", serde(with = "ring"))]
    pub(crate) ring: [f32; RING_LEN],
}

impl Default for ChannelSynth {
    fn default() -> Self {
        ChannelSynth { level: 0.0, ring: [0.0; RING_LEN] }
    }
}

/// serde's built-in array impls stop at 32 elements, so the ring crosses the
/// wire as its two 32-slot halves.
#[cfg(feature = "serde")]
mod ring {
    use super::RING_LEN;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        ring: &[f32; RING_LEN],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let lo: &[f32; RING_LEN / 2] = ring[..RING_LEN / 2].try_into().expect("half ring");
        let hi: &[f32; RING_LEN / 2] = ring[RING_LEN / 2..].try_into().expect("half ring");
        (lo, hi).serialize(s)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<[f32; RING_LEN], D::Error> {
        let (lo, hi): ([f32; RING_LEN / 2], [f32; RING_LEN / 2]) =
            Deserialize::deserialize(d)?;
        let mut ring = [0.0f32; RING_LEN];
        ring[..RING_LEN / 2].copy_from_slice(&lo);
        ring[RING_LEN / 2..].copy_from_slice(&hi);
        Ok(ring)
    }
}

/// The whole output stage: four channel synthesizers, one shared ring cursor,
/// and the analog chain. Continuous state (rings, fade, capacitors) is
/// serialized behind the `serde` feature so a savestate load resumes
/// mid-transition without a pop; the model-derived parts (`agb`, the stage's
/// charge factor) are reseeded via [`Renderer::set_model`] instead, exactly
/// like the core's other hardware-identity reseeds.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Renderer {
    pub(crate) chans: [ChannelSynth; 4],
    pub(crate) cursor: usize,
    pub(crate) analog: AnalogStage,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) agb: bool,
}

impl Renderer {
    pub fn new(model: AnalogModel) -> Renderer {
        let mut r = Renderer {
            chans: Default::default(),
            cursor: 0,
            analog: AnalogStage::default(),
            agb: false,
        };
        r.set_model(model);
        r
    }

    /// Re-derive the model-dependent state (charge factor + AGB gate). Applied
    /// by [`Renderer::new`] and re-applied after deserialization.
    pub fn set_model(&mut self, model: AnalogModel) {
        self.analog.set_model(model);
        self.agb = model.is_agb();
    }

    pub fn model(&self) -> AnalogModel {
        self.analog.model()
    }

    /// Render one output sample: scatter this record's transitions, read the
    /// four channel levels plus their pending corrections, then run the analog
    /// chain — fade, [`mix_stereo`], high-pass — in that order.
    ///
    /// `phases[i]` must be `0..PHASES` or [`NO_TRANSITION`]; anything else is
    /// a producer bug and panics on the table index.
    pub fn render(&mut self, kernel: &BlepKernel, rec: &SampleRecord) -> (f32, f32) {
        for (i, ch) in self.chans.iter_mut().enumerate() {
            let phase = rec.phases[i];
            if phase == NO_TRANSITION {
                continue;
            }
            let new = rec.levels[i];
            if !self.agb && new == 0.0 {
                continue;
            }
            let delta = new - ch.level;
            for (k, r) in kernel.residual[phase as usize].iter().enumerate() {
                ch.ring[(self.cursor + k) & (RING_LEN - 1)] += delta * r;
            }
            ch.level = new;
        }

        let mut raw = [0.0f32; 4];
        for (i, ch) in self.chans.iter_mut().enumerate() {
            raw[i] = ch.level + ch.ring[self.cursor];
            ch.ring[self.cursor] = 0.0;
        }
        self.cursor = (self.cursor + 1) & (RING_LEN - 1);

        let dac_on: [bool; 4] = core::array::from_fn(|i| rec.levels[i] != 0.0);
        let faded = self.analog.fade(raw, dac_on);
        let (left, right) = mix_stereo(faded, rec.nr50, rec.nr51, rec.enabled, self.agb);
        self.analog.high_pass(left, right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dac_analog;

    fn rec(levels: [f32; 4], phases: [u8; 4]) -> SampleRecord {
        SampleRecord { levels, phases, nr50: 0x77, nr51: 0xFF, enabled: true }
    }

    const HOLD: [u8; 4] = [NO_TRANSITION; 4];

    /// DC exactness: a constant stream settles the synth state onto the exact
    /// alphabet levels (bit-equal, empty rings), and from there one rendered
    /// sample equals fade → mix_stereo → high_pass applied to those exact
    /// levels — pinning both the settled values and the chain's f32 op order.
    #[test]
    fn a_constant_stream_settles_to_the_exact_alphabet_mix() {
        let kernel = BlepKernel::build();
        let mut r = Renderer::new(AnalogModel::Dmg);
        let levels = [dac_analog(0), dac_analog(15), dac_analog(3), 0.0];
        r.render(&kernel, &rec(levels, [0, 0, 0, NO_TRANSITION]));
        let hold = rec(levels, HOLD);
        for _ in 0..TAPS + 2 {
            r.render(&kernel, &hold);
        }
        for (i, level) in levels.iter().enumerate() {
            assert_eq!(r.chans[i].level.to_bits(), level.to_bits(), "channel {i} level");
            for (slot, v) in r.chans[i].ring.iter().enumerate() {
                assert_eq!(v.to_bits(), 0.0f32.to_bits(), "channel {i} ring slot {slot}");
            }
        }

        let mut analog = r.analog.clone();
        let faded = analog.fade(levels, [true, true, true, false]);
        let (l, rr) = mix_stereo(faded, 0x77, 0xFF, true, false);
        let expect = analog.high_pass(l, rr);
        let got = r.render(&kernel, &hold);
        assert_eq!(got.0.to_bits(), expect.0.to_bits(), "left chain order");
        assert_eq!(got.1.to_bits(), expect.1.to_bits(), "right chain order");
    }

    /// A square-wave record stream (flips faster than the step settles, phase
    /// lane wandering) stays inside PCM bounds and actually produces signal.
    #[test]
    fn a_square_record_stream_stays_inside_pcm_bounds() {
        let kernel = BlepKernel::build();
        let mut r = Renderer::new(AnalogModel::CgbMgb);
        let (hi, lo) = (dac_analog(0), dac_analog(15));
        let mut level = lo;
        let mut phase = 0u8;
        let mut worst = 0.0f32;
        for n in 0..4096 {
            let flip = n % 8 == 0;
            let mut phases = HOLD;
            if flip {
                level = if level == hi { lo } else { hi };
                phase = (phase + 13) % 64;
                phases[0] = phase;
            }
            let sample = SampleRecord {
                levels: [level, 0.0, 0.0, 0.0],
                phases,
                nr50: 0x77,
                nr51: 0x11,
                enabled: true,
            };
            let (l, rr) = r.render(&kernel, &sample);
            assert!(l.is_finite() && rr.is_finite(), "non-finite output at sample {n}");
            worst = worst.max(l.abs()).max(rr.abs());
        }
        assert!(worst <= 1.0, "square stream escaped PCM bounds: {worst}");
        assert!(worst > 0.05, "square stream produced no signal: {worst}");
    }

    /// Once settled, a sentinel stream is a bit-stable fixed point — no drift,
    /// no denormal churn — and a redundant same-level record with a phase
    /// (delta 0) is the same fixed point.
    #[test]
    fn a_sentinel_stream_is_bit_stable_once_settled() {
        let kernel = BlepKernel::build();
        let mut r = Renderer::new(AnalogModel::Dmg);
        let levels = [dac_analog(4), 0.0, 0.0, 0.0];
        r.render(&kernel, &rec(levels, [32, NO_TRANSITION, NO_TRANSITION, NO_TRANSITION]));
        let hold = rec(levels, HOLD);
        for _ in 0..40_000 {
            r.render(&kernel, &hold);
        }
        let settled = r.render(&kernel, &hold);
        for _ in 0..64 {
            let again = r.render(&kernel, &hold);
            assert_eq!(again.0.to_bits(), settled.0.to_bits());
            assert_eq!(again.1.to_bits(), settled.1.to_bits());
        }
        let redundant = rec(levels, [17, NO_TRANSITION, NO_TRANSITION, NO_TRANSITION]);
        for _ in 0..64 {
            let again = r.render(&kernel, &redundant);
            assert_eq!(again.0.to_bits(), settled.0.to_bits());
            assert_eq!(again.1.to_bits(), settled.1.to_bits());
        }
    }

    /// The mirror of `analog::tests::dac_off_fade_decays_monotonically_without
    /// _a_jump`, but through `render()`: a DAC-off record starts a monotonic
    /// coast (never a step), while the synth level holds the stale pre-off
    /// value the next DAC-on transition must step from.
    #[test]
    fn a_dac_off_record_fades_monotonically_through_render() {
        let kernel = BlepKernel::build();
        let mut r = Renderer::new(AnalogModel::Dmg);
        let on = [dac_analog(0), 0.0, 0.0, 0.0];
        r.render(&kernel, &rec(on, [0, NO_TRANSITION, NO_TRANSITION, NO_TRANSITION]));
        let hold_on = rec(on, HOLD);
        for _ in 0..TAPS + 2 {
            r.render(&kernel, &hold_on);
        }
        assert_eq!(r.analog.fade_state()[0].to_bits(), dac_analog(0).to_bits());

        let off = [0.0f32; 4];
        r.render(&kernel, &rec(off, [40, NO_TRANSITION, NO_TRANSITION, NO_TRANSITION]));
        let mut prev = r.analog.fade_state()[0];
        assert!(prev > 0.9, "fade jumped instead of coasting ({prev})");
        let hold_off = rec(off, HOLD);
        let mut steps = 0u32;
        loop {
            r.render(&kernel, &hold_off);
            let f = r.analog.fade_state()[0];
            assert!(f <= prev, "fade rose ({prev} -> {f})");
            assert!(f >= 0.0, "fade overshot below 0 ({f})");
            prev = f;
            steps += 1;
            if f == 0.0 {
                break;
            }
            assert!(steps < 1_000_000, "fade never reached 0");
        }
        assert!(steps > 100, "DMG fade was too abrupt ({steps} samples)");
        assert_eq!(
            r.chans[0].level.to_bits(),
            dac_analog(0).to_bits(),
            "the stale level must survive the whole off period"
        );
    }

    /// The AGB side of the DAC-off split: no fade exists, so a transition to
    /// `0.0` scatters and assigns like any other step, where a non-AGB
    /// renderer fed the same records keeps its stale level.
    #[test]
    fn agb_scatters_even_a_transition_to_level_zero() {
        let kernel = BlepKernel::build();
        let start = [dac_analog(2), 0.0, 0.0, 0.0];
        let off = rec([0.0; 4], [5, NO_TRANSITION, NO_TRANSITION, NO_TRANSITION]);

        let mut agb = Renderer::new(AnalogModel::Agb);
        agb.render(&kernel, &rec(start, [0, NO_TRANSITION, NO_TRANSITION, NO_TRANSITION]));
        agb.render(&kernel, &off);
        assert_eq!(agb.chans[0].level.to_bits(), 0.0f32.to_bits(), "a dead AGB 'DAC' steps");

        let mut dmg = Renderer::new(AnalogModel::Dmg);
        dmg.render(&kernel, &rec(start, [0, NO_TRANSITION, NO_TRANSITION, NO_TRANSITION]));
        dmg.render(&kernel, &off);
        assert_eq!(
            dmg.chans[0].level.to_bits(),
            dac_analog(2).to_bits(),
            "a non-AGB DAC-off keeps the stale level"
        );
    }
}

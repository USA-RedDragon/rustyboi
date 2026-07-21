//! The APU's stereo mixer and DAC transfer function — the single definition
//! shared by the emulator core and the `.rba` replay decoder.
//!
//! This crate exists because two consumers need byte-identical audio from
//! opposite sides of a recording. `rustyboi-core` mixes live; `rustyboi-replay`
//! rebuilds the same mix from a recorded channel tap, client-side, in the
//! compat gallery's wasm player. Those were once two hand-maintained clones of
//! the same arithmetic, pinned against each other by a test; they are now one
//! function, and there is nothing left to drift.
//!
//! Replay cannot simply depend on the core: the core pulls `clap`, `zip`
//! (deflate64 + lzma), `bincode`, and `serde` with no feature gates, and all of
//! that would land in the wasm bundle. Hence a leaf crate with no dependencies
//! at all, which is also what makes `no_std` free here — the whole crate is f32
//! arithmetic over integer register values, so nothing wants `libm`.
//!
//! # The boundary
//!
//! What lives here is the mixer and the DACs, and nothing downstream of them.
//! The core applies two further stages after this one — the per-channel DAC-off
//! fade and the model-gated output high-pass — and both deliberately stay in
//! the core:
//!
//!   * They are continuous, and the tap that feeds a recording is upstream of
//!     them by construction. The `.rba` per-plane encoder builds a `u16`
//!     palette of distinct values, which a fade ramp would both overflow and
//!     make quadratic to encode. Every value crossing this boundary is one of
//!     the 16 DAC levels or `0.0`.
//!   * The high-pass is stateful, and the replay decoder seeks freely. Filter
//!     state at an arbitrary seek target is not reconstructible without
//!     decoding everything before it.
//!
//! So this crate is the pre-analog-stage boundary exactly, which is also the
//! whole of what an `.rba` encodes.
//!
//! # f32 operation order is load-bearing
//!
//! Both consumers must agree bit-for-bit on real PCM, so the arithmetic below
//! is written for reproducibility rather than for brevity. Do not reassociate
//! the accumulate/scale/normalize sequence, and do not reduce [`dac_analog`]'s
//! calls in [`agb_unrouted_levels`] to literals — writing them as the formula
//! is what makes the levels correct by construction rather than by
//! coincidence.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// Digital `0..=15` to the DAC's analog output. Negative slope: digital 0 is
/// analog +1, digital 15 is analog -1, and the (unreachable) digital 7.5 is 0.
///
/// Pan Docs (Audio details): each generator emits a digital `$0..$F` that its
/// DAC "linearly translate[s] to the analog range -1 to 1 … the slope is
/// negative: 'digital 0' maps to 'analog 1'".
pub fn dac_analog(digital: u8) -> f32 {
    (7.5 - digital as f32) / 7.5
}

/// What a channel contributes to a stereo side that NR51 does NOT route it to,
/// on AGB only, in channel order.
///
/// With analog mixing an unrouted channel is simply not summed. AGB sums
/// digitally instead, so there is no "not summed" — an unrouted channel
/// contributes the same fixed level a routed channel emitting digital 0 would.
/// SameBoy `Core/apu.c`, `update_sample`: "On the AGB, because no analog mixing
/// is done, the behavior of NR51 is a bit different. A channel that is not
/// connected to a terminal is idenitcal to a connected channel playing PCM
/// sample 0."
///
/// Its `int8_t silence = 0` is [`dac_analog`]`(0)` in our units (SameBoy's
/// `(0xF - value * 2)` over its 15-unit rail is exactly `(7.5 - d) / 7.5`).
/// CH3 is the exception and gets `silence = 7 * 2`, i.e. digital 7 — taken
/// BEFORE the CH3 inversion, which SameBoy applies only on the routed path.
pub fn agb_unrouted_levels() -> [f32; 4] {
    [dac_analog(0), dac_analog(0), dac_analog(7), dac_analog(0)]
}

/// The mixer proper: NR51 routing, NR50 master volume, and the 4-channel
/// normalize. Pure — it is the whole of what a tap sample reconstructs to.
///
/// `ch` is the four post-DAC analog channel levels in channel order, `agb`
/// selects digital-mixing NR51 semantics, and `enabled` is NR52's master bit.
/// The result is PRE-analog-stage: it carries neither the DAC-off fade nor the
/// output high-pass (see the crate docs).
pub fn mix_stereo(ch: [f32; 4], nr50: u8, nr51: u8, enabled: bool, agb: bool) -> (f32, f32) {
    if !enabled {
        return (0.0, 0.0);
    }

    let mut left = 0.0f32;
    let mut right = 0.0f32;

    // On AGB every channel contributes to both sides unconditionally; NR51
    // selects between the channel's own level and a fixed unrouted level rather
    // than between summing and not summing. The `else` arms are what make this
    // differ from an analog mixer, where an unrouted contribution is
    // structurally zero.
    let unrouted = agb_unrouted_levels();

    for (i, &out) in ch.iter().enumerate() {
        if nr51 & (1 << (i + 4)) != 0 {
            left += out;
        } else if agb {
            left += unrouted[i];
        }
        if nr51 & (1 << i) != 0 {
            right += out;
        } else if agb {
            right += unrouted[i];
        }
    }

    // Master volume, `(vol + 1) / 8`, then the normalize by the channel count.
    left *= ((nr50 >> 4) & 7) as f32 + 1.0;
    left /= 8.0;
    right *= (nr50 & 7) as f32 + 1.0;
    right /= 8.0;
    (left / 4.0, right / 4.0)
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

    /// CH3's unrouted level is the one that is not digital 0, and it is taken
    /// before that channel's AGB output inversion — so it is `dac_analog(7)`,
    /// positive, not the negated level the routed CH3 path emits.
    #[test]
    fn agb_unrouted_levels_are_digital_zero_except_ch3() {
        let u = agb_unrouted_levels();
        assert_eq!(u, [dac_analog(0), dac_analog(0), dac_analog(7), dac_analog(0)]);
        assert_eq!(u[2], (7.5f32 - 7.0) / 7.5, "CH3 is digital 7, pre-inversion");
    }

    /// NR52 clear is silence regardless of everything else.
    #[test]
    fn a_disabled_apu_emits_silence() {
        let ch = [1.0, -1.0, 0.5, -0.25];
        assert_eq!(mix_stereo(ch, 0x77, 0xFF, false, false), (0.0, 0.0));
        assert_eq!(mix_stereo(ch, 0x77, 0xFF, false, true), (0.0, 0.0));
    }

    /// An unrouted channel is structurally absent from an analog mix, and
    /// present at its fixed level in a digital one. This is the whole of the
    /// AGB/analog split.
    #[test]
    fn nr51_drops_a_channel_on_analog_but_substitutes_a_level_on_agb() {
        let ch = [1.0, 0.0, 0.0, 0.0];
        // NR51 routes nothing anywhere, NR50 at full volume.
        let (l, _) = mix_stereo(ch, 0x77, 0x00, true, false);
        assert_eq!(l, 0.0, "analog mixing simply does not sum an unrouted channel");

        let (l, r) = mix_stereo(ch, 0x77, 0x00, true, true);
        let u = agb_unrouted_levels();
        let want = (u[0] + u[1] + u[2] + u[3]) / 4.0;
        assert_eq!((l, r), (want, want), "AGB substitutes the unrouted levels");
    }

    /// NR50's per-side volume is `(vol + 1) / 8`, read from separate nibbles.
    #[test]
    fn nr50_scales_each_side_independently() {
        let ch = [1.0, 1.0, 1.0, 1.0];
        // Left volume 7 (=> 8/8), right volume 0 (=> 1/8).
        let (l, r) = mix_stereo(ch, 0x70, 0xFF, true, false);
        assert_eq!(l, 1.0, "4 channels at 1.0, full volume, /4");
        assert_eq!(r, 1.0 / 8.0);
    }

    /// The mixer's output alphabet must stay discrete: the tap upstream of it
    /// carries at most the 16 DAC levels plus 0.0, and the `.rba` encoder's
    /// palette is a `u16`. Nothing here may introduce a continuous value.
    #[test]
    fn the_channel_level_alphabet_stays_discrete() {
        let mut levels = [0.0f32; 17];
        for d in 0..=15u8 {
            levels[d as usize] = dac_analog(d);
        }
        levels[16] = 0.0; // an unpowered DAC
        for (i, a) in levels.iter().enumerate() {
            for b in levels.iter().skip(i + 1) {
                assert!(a != b || *a == 0.0, "the DAC level alphabet collapsed");
            }
        }
    }
}

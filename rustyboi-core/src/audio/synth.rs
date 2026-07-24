//! The band-limited output stage's core-side driver: the absolute-cc sample
//! grid, the per-sample transition collapse, and the shared [`Renderer`].
//!
//! The digital PSG is untouched; this is faithful *rendering* of the analog
//! signal it already produces. Every DAC-level transition is timestamped at its
//! exact APU cycle by the channels (into their `pending` buffers) and drained
//! here into per-sample slots keyed on a monotonic, never-folded `synth_cc`.
//! Host samples are then pulled off the grid and rendered through the one
//! [`Renderer`] the `.rba` replay decoder also runs, so live output and decoded
//! replay are byte-identical by construction.
//!
//! # The grid
//!
//! `synth_cc` counts 2 MHz APU cycles and is anchored at `(a_cc, a_sample)`.
//! With `cpu_hz` the machine's real clock and the host rate fixed at 44100:
//!
//! ```text
//! d      = event_cc - a_cc                     (APU cycles since the anchor)
//! sample = a_sample + floor(d * 88200 / cpu_hz)
//! phase  = floor((d * 88200 - dsample * cpu_hz) * 64 / cpu_hz)   (0..63)
//! ```
//!
//! (`88200 = 2 * 44100`, one APU cycle spanning `2 * 44100 / cpu_hz` samples.)
//! `u128` intermediates, half-open windows: an edge exactly on a sample
//! boundary belongs to the next sample at phase 0. The SAME mapping serves both
//! the event collapse and the sample pull, so the two can never disagree.

use rustyboi_mix::{dac_analog, BlepKernel, Renderer, SampleRecord, AnalogModel, NO_TRANSITION, PHASES};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// `2 * HOST_SAMPLE_RATE`. `synth_cc` is in APU cycles (host clock / 2), so one
/// APU cycle is `2 * 44100 / cpu_hz` output samples. Pinned to the fixed host
/// rate; a change to [`rustyboi_mix::HOST_SAMPLE_RATE`] must change this too.
const SAMPLE_RATE_X2: u128 = 88_200;

const _: () = assert!(SAMPLE_RATE_X2 == 2 * 44_100);

/// Hard cap on how far past the pull frontier [`SynthBox::slot_mut`] will open
/// a slot — ~4.5 s of host samples (~270 frames). The pull finalizes every
/// closed sample after each instruction, so the real open window is one or two
/// samples; nothing legitimate reaches even one frame ahead. This is purely a
/// backstop against a mis-mapped (underflowed) transition timestamp, capping
/// the slot `VecDeque` at ~11 MB instead of an unbounded (OOM) allocation.
pub(crate) const MAX_SLOT_HORIZON: usize = 200_000;

/// A channel's discrete output level as a small code the channel emits and the
/// controller resolves: `0..=15` is a live DAC nibble, `16` is the DAC-off
/// marker (only produced by non-AGB machines, where it renders to `0.0`).
pub(crate) const DAC_OFF_CODE: u8 = 16;

/// Resolve an output [code](DAC_OFF_CODE) to its analog level, matching
/// `Audio::channel_outputs` exactly: the negative-slope DAC transfer, the
/// non-AGB DAC-off `0.0`, and the AGB CH3 output inversion.
pub(crate) fn resolve_level(code: u8, ch: usize, agb: bool) -> f32 {
    if code == DAC_OFF_CODE {
        // Only non-AGB channels ever emit this; on AGB a "dead DAC" converts
        // digital 0, so the channel emits code 0 instead and never reaches here.
        return 0.0;
    }
    let l = dac_analog(code);
    if agb && ch == 2 {
        -l
    } else {
        l
    }
}

/// The nearest of the 16 live-DAC levels [`dac_analog`]`(0..=15)` to `avg`,
/// the quantization target for the above-Nyquist box-filter collapse.
///
/// NEVER returns `0.0`: [`Renderer`] derives a channel's DAC-on state from
/// `level != 0.0`, and every `dac_analog(d)` for integer `d` is nonzero (the
/// transfer crosses 0 only at the unreachable `d = 7.5`), so a DAC-on channel's
/// box-filtered average always resolves to a DAC-on level and is never misread
/// as DAC-off. Ties break to the lower `d` (the higher level), deterministically,
/// because the scan keeps a strictly-closer candidate only.
fn nearest_dac_alphabet(avg: f32) -> f32 {
    let mut best = dac_analog(0);
    let mut best_dist = (best - avg).abs();
    for d in 1..=15u8 {
        let l = dac_analog(d);
        let dist = (l - avg).abs();
        if dist < best_dist {
            best = l;
            best_dist = dist;
        }
    }
    best
}

/// Whether a tonal channel whose full waveform period is `full_period_cc` APU
/// cycles has a fundamental strictly ABOVE Nyquist at clock `cpu_hz` — i.e.
/// fewer than 2 output samples span one period.
///
/// `samples_per_period = full_period_cc * SAMPLE_RATE_X2 / cpu_hz`; a period
/// under 2 samples means a fundamental over `HOST_SAMPLE_RATE / 2` = 22.05 kHz.
/// The comparison is strict, so a fundamental EXACTLY at Nyquist (2 samples per
/// period) is not gated — nothing at or below the audible range is ever caught.
/// `cpu_hz`-relative, so it tracks SGB1's slower grid without a separate rule.
pub(crate) fn is_ultrasonic(full_period_cc: u32, cpu_hz: u32) -> bool {
    full_period_cc as u128 * SAMPLE_RATE_X2 < 2 * cpu_hz as u128
}

/// One output sample under construction: a fixed-size box-filter accumulator per
/// channel plus an optional mix-register change effective from this sample's
/// boundary onward.
///
/// A channel toggling at most once in the window (`edge_count <= 1`) takes the
/// exact band-limited BLEP path — the single edge's `(first_phase, last_level)`.
/// A channel toggling faster than the sample grid (`edge_count >= 2`, i.e. an
/// EDGE rate above Nyquist — only reachable ultrasonically, e.g. a square whose
/// fundamental exceeds 22 kHz) would ALIAS under a last-edge collapse, so
/// `finalize` instead emits the window's time-weighted average level.
/// `area_internal` accumulates the `cur_level`-INDEPENDENT part of that area at
/// record time — the segments strictly between the first and last edge — because
/// the leading (carry-in from the running `cur_level`) and trailing segments are
/// only knowable at `finalize`, once this slot's `cur_level` is settled.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Slot {
    first_phase: [u8; 4],
    last_phase: [u8; 4],
    last_level: [f32; 4],
    area_internal: [f32; 4],
    edge_count: [u16; 4],
    mix: Option<(u8, u8, bool)>,
}

impl Default for Slot {
    fn default() -> Self {
        Slot {
            first_phase: [0; 4],
            last_phase: [0; 4],
            last_level: [0.0; 4],
            area_internal: [0.0; 4],
            edge_count: [0; 4],
            mix: None,
        }
    }
}

/// The whole synth. The renderer (its continuous analog state) AND the grid /
/// collapse state are serialized, so a savestate load resumes the exact sample
/// stream — same phase alignment, same running levels, same open slot — with no
/// pop and no restart transient. `observing` rides along so a machine saved
/// mid-observe resumes observing without re-anchoring the grid (the channels'
/// own `#[serde(skip)]` observing flags are re-synced from it on load). Only the
/// measurement tap is skipped.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SynthBox {
    /// Continuous renderer state (rings, fade, capacitors); its model-derived
    /// parts are re-seeded via [`SynthBox::set_model`].
    renderer: Renderer,
    #[serde(default)]
    synth_cc: u64,
    #[serde(default)]
    a_cc: u64,
    #[serde(default)]
    a_sample: u64,
    #[serde(default)]
    next_sample: u64,
    #[serde(default)]
    slots: VecDeque<Slot>,
    #[serde(default)]
    cur_level: [f32; 4],
    #[serde(default)]
    cur_mix: (u8, u8, bool),
    #[serde(default)]
    noted_mix: (u8, u8, bool),
    #[serde(default)]
    pub(crate) observing: bool,
    #[serde(skip)]
    tap: Option<Vec<SampleRecord>>,
}

impl Default for SynthBox {
    fn default() -> Self {
        SynthBox {
            renderer: Renderer::new(AnalogModel::Dmg),
            synth_cc: 0,
            a_cc: 0,
            a_sample: 0,
            next_sample: 0,
            slots: VecDeque::new(),
            cur_level: [0.0; 4],
            cur_mix: (0, 0, false),
            noted_mix: (0, 0, false),
            observing: false,
            tap: None,
        }
    }
}

impl SynthBox {
    /// Re-derive the renderer's model-dependent state (charge factor + AGB
    /// gate). Applied after a savestate load, like the core's other
    /// hardware-identity reseeds.
    pub(crate) fn set_model(&mut self, model: AnalogModel) {
        self.renderer.set_model(model);
    }

    pub(crate) fn model(&self) -> AnalogModel {
        self.renderer.model()
    }

    /// Advance the monotonic grid clock. NEVER folded — the epoch / DIV-reset /
    /// speed / boot / load rebases touch the channels' `cc`, not this.
    pub(crate) fn advance(&mut self, cycles: u64) {
        self.synth_cc = self.synth_cc.wrapping_add(cycles);
    }

    pub(crate) fn synth_cc(&self) -> u64 {
        self.synth_cc
    }

    /// The sample index an APU cycle falls in (floor).
    fn sample_of(&self, e: u64, cpu_hz: u32) -> u64 {
        let d = e.wrapping_sub(self.a_cc) as u128;
        self.a_sample + (d * SAMPLE_RATE_X2 / cpu_hz as u128) as u64
    }

    /// The sample index AND sub-sample phase (0..63) of an APU cycle.
    fn sample_and_phase(&self, e: u64, cpu_hz: u32) -> (u64, u8) {
        let d = e.wrapping_sub(self.a_cc) as u128;
        let num = d * SAMPLE_RATE_X2;
        let hz = cpu_hz as u128;
        let dsample = num / hz;
        let phase = ((num - dsample * hz) * 64 / hz) as u8;
        (self.a_sample + dsample as u64, phase)
    }

    /// Re-anchor the grid at the current clock, keeping `sample_of` continuous
    /// across a `cpu_hz` slope change.
    pub(crate) fn reanchor_for_cpu_hz(&mut self, old_hz: u32) {
        let s = self.sample_of(self.synth_cc, old_hz);
        self.a_cc = self.synth_cc;
        self.a_sample = s;
    }

    fn slot_mut(&mut self, sample: u64) -> &mut Slot {
        let sample = sample.max(self.next_sample);
        let idx = (sample - self.next_sample) as usize;
        // Defensive bound. The pull finalizes every closed sample after each
        // instruction, so a live transition sits at most one or two samples
        // past `next_sample`; a target this far ahead is never real audio but a
        // cc-mapping underflow (an `e` that wrapped below `a_cc` maps to a
        // ~1e17 index — the pre-fix multi-GB OOM). The root cause is clamped at
        // the drain site; this is the last-resort cap so the slot buffer can
        // never balloon. Loud under audit/debug, silently clamped in release.
        if idx > MAX_SLOT_HORIZON {
            #[cfg(any(debug_assertions, feature = "synth-audit"))]
            panic!(
                "synth slot index {idx} beyond horizon {MAX_SLOT_HORIZON} \
                 (sample {sample}, next_sample {}, synth_cc {}, a_cc {})",
                self.next_sample, self.synth_cc, self.a_cc
            );
        }
        let idx = idx.min(MAX_SLOT_HORIZON);
        while self.slots.len() <= idx {
            self.slots.push_back(Slot::default());
        }
        &mut self.slots[idx]
    }

    /// Record one drained channel transition into its target sample slot,
    /// accumulating the box-filter state. Transitions arrive in cc order, so
    /// within a slot each edge's `phase` is >= the previous edge's, and the
    /// segment between them (`last_level` held over `phase - last_phase`) folds
    /// into `area_internal`. The first edge only seeds the anchors — its leading
    /// and trailing segments depend on the (still-unsettled) `cur_level` and the
    /// window end, and are added in `finalize`.
    pub(crate) fn record_transition(&mut self, e: u64, ch: usize, cpu_hz: u32, level: f32) {
        let (sample, phase) = self.sample_and_phase(e, cpu_hz);
        let slot = self.slot_mut(sample);
        if slot.edge_count[ch] == 0 {
            slot.first_phase[ch] = phase;
            slot.last_phase[ch] = phase;
            slot.last_level[ch] = level;
            slot.edge_count[ch] = 1;
        } else {
            slot.area_internal[ch] +=
                slot.last_level[ch] * (phase as f32 - slot.last_phase[ch] as f32);
            slot.last_phase[ch] = phase;
            slot.last_level[ch] = level;
            slot.edge_count[ch] = slot.edge_count[ch].saturating_add(1);
        }
    }

    /// Record a mix-register change effective from the sample containing `e`.
    pub(crate) fn record_mix(&mut self, e: u64, cpu_hz: u32, mix: (u8, u8, bool)) {
        let sample = self.sample_of(e, cpu_hz);
        self.slot_mut(sample).mix = Some(mix);
    }

    /// Whether a mix write actually changed the mix as this synth last saw it
    /// (dedup so a redundant NR50 write records nothing). Updates the shadow.
    pub(crate) fn mix_changed(&mut self, mix: (u8, u8, bool)) -> bool {
        if mix != self.noted_mix {
            self.noted_mix = mix;
            true
        } else {
            false
        }
    }

    /// The next sample whose end boundary the grid has already passed, i.e. how
    /// far the pull may finalize.
    pub(crate) fn pull_target(&self, cpu_hz: u32) -> u64 {
        self.sample_of(self.synth_cc, cpu_hz)
    }

    pub(crate) fn next_sample(&self) -> u64 {
        self.next_sample
    }

    /// Current depth of the open-sample slot buffer. Bounded by
    /// [`MAX_SLOT_HORIZON`] in [`SynthBox::slot_mut`]; read only by the
    /// pull-time audit tripwire and the cc-mapping regression test, so it is
    /// compiled only when one of those exists.
    #[cfg(any(test, debug_assertions, feature = "synth-audit"))]
    pub(crate) fn slots_len(&self) -> usize {
        self.slots.len()
    }

    /// Finalize and render the next open sample: apply its mix change, collapse
    /// its per-channel transitions, push the record to the tap if engaged, and
    /// step the renderer.
    ///
    /// `ultrasonic[ch]` is `Some(dc)` when that channel is a running, DAC-on
    /// TONE whose fundamental is above Nyquist (a period-based test on the
    /// channel's CURRENT register state — see [`is_ultrasonic`] and the channels'
    /// `ultrasonic_dc`). Such a channel's audible-band output IS its DC average:
    /// this branch emits that DC (quantized to the DAC alphabet), SUPERSEDING
    /// both the box filter and the BLEP path, because at ~1 edge per window the
    /// box filter attenuates only ~7 dB and the BLEP path aliases the last edge
    /// into an audible tone. The DC is constant while duty/volume hold (even mid
    /// frequency-sweep), so consecutive samples collapse to the sentinel → held
    /// constant DC → HPF-silenced. The step into the DC is band-limited (at
    /// `first_phase`, or 0 for a no-edge window), so crossing Nyquist never
    /// clicks. CH4 noise is never flagged (no single fundamental).
    ///
    /// When a channel is NOT flagged the collapse forks on the window's edge
    /// count. `<= 1` edge is at most a sub-Nyquist transition and takes the exact
    /// BLEP path (transition to the edge's level at its phase iff it changes the
    /// running level). `>= 2` edges is an above-Nyquist edge rate a last-edge
    /// collapse would ALIAS, so it emits the window's time-weighted average level
    /// (box filter); this remains the path for CH4 and for the transient window
    /// where the period test and the recorded edges briefly disagree.
    pub(crate) fn finalize(
        &mut self,
        kernel: &BlepKernel,
        ultrasonic: &[Option<f32>; 4],
    ) -> (f32, f32) {
        let slot = self.slots.pop_front().unwrap_or_default();
        if let Some(m) = slot.mix {
            self.cur_mix = m;
        }
        let mut phases = [NO_TRANSITION; 4];
        for (ch, phase) in phases.iter_mut().enumerate() {
            // Period-based ultrasonic gate: fundamental above Nyquist → emit the
            // channel's DC, ignoring the window's (aliasing) edge structure.
            if let Some(dc) = ultrasonic[ch] {
                let q = nearest_dac_alphabet(dc);
                if q != self.cur_level[ch] {
                    *phase = if slot.edge_count[ch] > 0 { slot.first_phase[ch] } else { 0 };
                    self.cur_level[ch] = q;
                }
                continue;
            }
            match slot.edge_count[ch] {
                // Held sample: no edge, keep the sentinel and the previous level.
                0 => {}
                // Exact BLEP path (unchanged): one edge, step to its level iff it
                // actually changes the running level (a net-zero flip is held).
                1 => {
                    if slot.last_level[ch] != self.cur_level[ch] {
                        *phase = slot.first_phase[ch];
                        self.cur_level[ch] = slot.last_level[ch];
                    }
                }
                // Above-Nyquist edge rate: box-filter the window to its
                // time-weighted average and quantize to the DAC alphabet. The
                // area is the carry-in leading segment (`cur_level` over
                // `[0, first_phase)`), the internal segments accumulated at
                // record time, and the trailing segment (`last_level` over
                // `[last_phase, PHASES)`), all over the `PHASES`-wide window. A
                // subsequent equal-average sample resolves to the same `q` and
                // collapses to the sentinel → held constant DC → HPF-silenced.
                _ => {
                    let w = PHASES as f32;
                    let leading = self.cur_level[ch] * slot.first_phase[ch] as f32;
                    let trailing = slot.last_level[ch] * (w - slot.last_phase[ch] as f32);
                    let avg = (leading + slot.area_internal[ch] + trailing) / w;
                    let q = nearest_dac_alphabet(avg);
                    if q != self.cur_level[ch] {
                        *phase = slot.first_phase[ch];
                        self.cur_level[ch] = q;
                    }
                }
            }
        }
        let rec = SampleRecord {
            levels: self.cur_level,
            phases,
            nr50: self.cur_mix.0,
            nr51: self.cur_mix.1,
            enabled: self.cur_mix.2,
        };
        if let Some(tap) = &mut self.tap {
            tap.push(rec);
        }
        self.next_sample += 1;
        self.renderer.render(kernel, &rec)
    }

    /// Begin observing. Returns `true` when this is a FRESH start (the grid was
    /// not carried in from a savestate), in which case the controller must
    /// reseed the channel levels; the pull begins at the current sample so no
    /// backlog is emitted. Returns `false` when resuming a serialized observing
    /// session (a savestate load), where the grid / running levels / open slot
    /// all continue from their restored values and nothing is re-seeded.
    ///
    /// The grid mapping (`a_cc`, `a_sample`) is never re-anchored here — it holds
    /// from boot (or a `cpu_hz` change) — so a transition's phase is stable
    /// across a save/load.
    pub(crate) fn start_observing(&mut self, mix: (u8, u8, bool), cpu_hz: u32) -> bool {
        if self.observing {
            return false; // already observing (resumed from a serialized state)
        }
        self.observing = true;
        self.next_sample = self.sample_of(self.synth_cc, cpu_hz);
        self.slots.clear();
        self.cur_mix = mix;
        self.noted_mix = mix;
        // cur_level stays at the renderer's implied level; the reseed
        // transitions step it to the channels' real levels.
        true
    }

    pub(crate) fn stop_observing(&mut self) {
        self.observing = false;
    }

    /// The serialized observing flag, so the channels' skipped flags can be
    /// re-synced from it on load.
    pub(crate) fn observing(&self) -> bool {
        self.observing
    }

    pub(crate) fn set_tap(&mut self, on: bool) {
        self.tap = on.then(Vec::new);
    }

    pub(crate) fn drain_tap(&mut self) -> Vec<SampleRecord> {
        self.tap.as_mut().map(std::mem::take).unwrap_or_default()
    }
}

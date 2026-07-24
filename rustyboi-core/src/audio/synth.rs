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

use rustyboi_mix::{dac_analog, BlepKernel, Renderer, SampleRecord, AnalogModel, NO_TRANSITION};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// `2 * HOST_SAMPLE_RATE`. `synth_cc` is in APU cycles (host clock / 2), so one
/// APU cycle is `2 * 44100 / cpu_hz` output samples. Pinned to the fixed host
/// rate; a change to [`rustyboi_mix::HOST_SAMPLE_RATE`] must change this too.
const SAMPLE_RATE_X2: u128 = 88_200;

const _: () = assert!(SAMPLE_RATE_X2 == 2 * 44_100);

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

/// One output sample under construction: the LAST transition per channel
/// (last-wins collapse of every intra-sample edge) plus an optional
/// mix-register change effective from this sample's boundary onward.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Slot {
    phase: [u8; 4],
    level: [f32; 4],
    mix: Option<(u8, u8, bool)>,
}

impl Default for Slot {
    fn default() -> Self {
        Slot { phase: [NO_TRANSITION; 4], level: [0.0; 4], mix: None }
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
        while self.slots.len() <= idx {
            self.slots.push_back(Slot::default());
        }
        &mut self.slots[idx]
    }

    /// Record one drained channel transition into its target sample slot
    /// (last-wins: transitions arrive in cc order, so a later one overwrites).
    pub(crate) fn record_transition(&mut self, e: u64, ch: usize, cpu_hz: u32, level: f32) {
        let (sample, phase) = self.sample_and_phase(e, cpu_hz);
        let slot = self.slot_mut(sample);
        slot.phase[ch] = phase;
        slot.level[ch] = level;
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

    /// Finalize and render the next open sample: apply its mix change, collapse
    /// its per-channel transitions (net-zero flips → sentinel), push the record
    /// to the tap if engaged, and step the renderer.
    pub(crate) fn finalize(&mut self, kernel: &BlepKernel) -> (f32, f32) {
        let slot = self.slots.pop_front().unwrap_or_default();
        if let Some(m) = slot.mix {
            self.cur_mix = m;
        }
        let mut phases = [NO_TRANSITION; 4];
        for (ch, phase) in phases.iter_mut().enumerate() {
            // A held sample (no edge) or a net-zero intra-sample flip keeps the
            // sentinel and the previous level; a real change updates both.
            if slot.phase[ch] != NO_TRANSITION && slot.level[ch] != self.cur_level[ch] {
                *phase = slot.phase[ch];
                self.cur_level[ch] = slot.level[ch];
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

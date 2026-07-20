use serde::{Deserialize, Serialize};
use crate::audio::{analog, wave, square, noise};
use crate::memory::Addressable;

pub(crate) const NR10: u16 = 0xFF10; // Channel 1 sweep register
pub(crate) const NR11: u16 = 0xFF11; // Channel 1 sound length/wave pattern duty
pub(crate) const NR12: u16 = 0xFF12; // Channel 1 volume and envelope
pub(crate) const NR13: u16 = 0xFF13; // Channel 1 period low
pub(crate) const NR14: u16 = 0xFF14; // Channel 1 period high and control

pub(crate) const NR21: u16 = 0xFF16; // Channel 2 sound length/wave pattern duty
pub(crate) const NR22: u16 = 0xFF17; // Channel 2 volume and envelope
pub(crate) const NR23: u16 = 0xFF18; // Channel 2 period low
pub(crate) const NR24: u16 = 0xFF19; // Channel 2 period high and control

pub(crate) const NR30: u16 = 0xFF1A; // Channel 3 dac enable
pub(crate) const NR31: u16 = 0xFF1B; // Channel 3 sound length
pub(crate) const NR32: u16 = 0xFF1C; // Channel 3 output level
pub(crate) const NR33: u16 = 0xFF1D; // Channel 3 period low
pub(crate) const NR34: u16 = 0xFF1E; // Channel 3 period high and control

pub(crate) const NR41: u16 = 0xFF20; // Channel 4 sound length
pub(crate) const NR42: u16 = 0xFF21; // Channel 4 volume and envelope
pub(crate) const NR43: u16 = 0xFF22; // Channel 4 frequency and randomness
pub(crate) const NR44: u16 = 0xFF23; // Channel 4 control

pub(crate) const NR50: u16 = 0xFF24; // master volume, VIN panning
pub(crate) const NR51: u16 = 0xFF25; // Sound panning
pub const NR52: u16 = 0xFF26; // Audio master control

pub(crate) const WAV_START: u16 = 0xFF30; // Channel 3 wave pattern RAM start
pub(crate) const WAV_LENGTH: usize = 16; // Channel 3 wave pattern RAM length
pub(crate) const WAV_END: u16 = WAV_START + WAV_LENGTH as u16 - 1; // Channel 3 wave pattern RAM end

#[derive(Clone, Serialize, Deserialize)]
pub struct Audio {
    channel1: square::SquareWave,
    channel2: square::SquareWave,
    channel3: wave::Wave,
    channel4: noise::Noise,

    // Master control registers
    nr50: u8, // Master volume and VIN panning
    nr51: u8, // Sound panning
    nr52: u8, // Master control/status

    // Audio enabled flag
    audio_enabled: bool,

    // Sample generation timing
    fractional_cycles: f32,

    // Dots per 44.1 kHz host sample = cpu_hz / 44100. Host resampling only, not
    // machine state (an SGB1's dot timeline is identical to a DMG's — only the
    // wall-clock rate those dots are played back at differs), so it is skipped
    // in the savestate and re-seeded from the model by `GB::set_region`.
    #[serde(skip, default = "default_cycles_per_sample")]
    cycles_per_sample: f32,

    // APU master clock — an absolute 2 MHz counter (mod 0x8000_0000) anchored
    // at boot. Driven from the timer's absolute `abs_cc`: each `sync_cc`
    // advances by `(abs_cc - last_update) >> (1 + ds)`.
    // Carries the full phase a DIV reset would otherwise drop, which the
    // cc-driven length counter needs across the power-on fold.
    #[serde(default)]
    cc: u32,
    // Length-subsystem clock. Mirrors `cc` exactly; kept as a separate field
    // only so the read/write access-cc overlays can temporarily push a
    // different length cc to the channels and restore it afterwards.
    #[serde(default)]
    len_cc: u32,
    // Absolute CPU cc at the last clock advance; its bit-0 parity feeds the
    // duty / DIV-reset / speed-change folds.
    #[serde(default)]
    last_update: u64,
    // Last-seen timer DIV-write count; a change triggers the DIV-reset fold.
    #[serde(default)]
    last_div_resets: u64,
    #[serde(default)]
    clock_anchored: bool,
    // Double-speed flag from the last `sync_cc`, so the APU-enable fold (which
    // runs on the write path, without `ds`) uses the right speed.
    #[serde(default)]
    cached_ds: bool,
    // The `ds` value last broadcast to the channels via push_cc; lets sync_cc
    // skip the redundant per-dot push when neither cc nor ds moved.
    #[serde(default)]
    last_pushed_ds: bool,
    // CGB vs DMG flag for the post-boot APU clock anchor (high-bit constant
    // differs: 0x1E00 CGB / 0x2400 DMG).
    // Not in Pan Docs, TCAGBD, or GBCTR (no audio chapter); post-boot APU
    // clock phase reverse-engineered from test-ROM refs.
    #[serde(default)]
    boot_cgb: bool,
    // Free-running 1 MHz sub-phase of the 2 MHz APU clock (`lf_div ^= cycles &
    // 1`). This parity never folds: a DIV reset only steps the DIV-APU phase
    // and a speed switch leaves it alone, so it toggles ONLY in `advance_to`
    // and is deliberately outside the DIV-reset / speed-change cc re-anchoring.
    // Reset to 1 on APU power-on, seeded at boot to the post-boot phase.
    // Not in Pan Docs, TCAGBD, or GBCTR; free-running 1 MHz APU sub-phase from
    // reverse-engineered from test-ROM refs.
    #[serde(default = "default_ctl_lf_div")]
    lf_div: u32,
    // Per-access read cc (2 MHz units) for the current APU register read, set
    // by `set_read_len_cc`; a PCM12 read resolves the square duty at this
    // access cc. Not serialized (transient per-access).
    #[serde(skip)]
    pcm_read_cc: Option<u32>,
    // Counts DIV-APU events (the 0x1000-cc master-clock boundaries = DIV bit
    // 12/13 falling edges, including the forced edge of a DIV write). The 64 Hz
    // envelope frame runs when (div_divider & 7) == 7.
    // Pan Docs: Audio details — https://gbdev.io/pandocs/Audio_details.html
    #[serde(default)]
    div_divider: u16,
    // Powering the APU on while the DIV-APU bit is high skips the first
    // (truncated) DIV-APU event, with div_divider pre-set to 1.
    // 0=inactive, 1=skipped, 2=skip. The DIV-APU counter itself is in Pan Docs
    // (Audio_details), but this power-on skip is not in Pan Docs, TCAGBD, or
    // GBCTR; reverse-engineered from test-ROM refs.
    #[serde(default)]
    skip_div_event: u8,
    // CGB-D/E APU revision gate; false = the default CGB-C model.
    #[serde(default)]
    cgb_de: bool,
    // Optional per-sample tap of the pre-mix channel outputs + mix registers,
    // filled by `generate_samples` when engaged. Recording/measurement only;
    // never serialized
    #[serde(skip)]
    channel_tap: Option<Vec<ChannelSample>>,
    // The analog output stage (DAC-off fade + output high-pass). Serialized so
    // a load / rewind step resumes the filter where it was instead of ringing
    // out a restart transient; its model-derived charge factor is the one part
    // that is re-seeded by `set_analog_model` rather than stored.
    #[serde(default)]
    analog: analog::AnalogStage,
}

/// One tapped sample: pre-mix channel outputs [ch1..ch4] + the mix registers
/// (nr50, nr51) + the master enable — everything [`Audio::mix_tap_sample`]
/// consumes, so the stereo mix is exactly reconstructible from the tap alone.
///
/// The channel outputs are post-DAC but PRE-analog-stage: the DAC-off fade and
/// the output high-pass are continuous and sit downstream of the tap, so a tap
/// value is always one of the 16 DAC levels or 0.0 for an unpowered DAC.
pub type ChannelSample = ([f32; 4], u8, u8, bool);

fn default_ctl_lf_div() -> u32 {
    1
}

/// The host output rate every backend consumes. Fixed: the machine's clock
/// changes how many dots fill one sample, never the samples-per-second.
pub const HOST_SAMPLE_RATE: f32 = 44100.0;

fn default_cycles_per_sample() -> f32 {
    crate::gb::DMG_CPU_HZ as f32 / HOST_SAMPLE_RATE
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

impl Audio {
    pub fn new() -> Self {
        Audio {
            channel1: square::SquareWave::new(true),
            channel2: square::SquareWave::new(false),
            channel3: wave::Wave::new(),
            channel4: noise::Noise::new(),
            nr50: 0,
            nr51: 0,
            nr52: 0,
            len_cc: 0,
            audio_enabled: false,
            fractional_cycles: 0.0,
            cycles_per_sample: default_cycles_per_sample(),
            cc: 0,
            last_update: 0,
            last_div_resets: 0,
            clock_anchored: false,
            cached_ds: false,
            last_pushed_ds: false,
            boot_cgb: false,
            lf_div: 1,
            pcm_read_cc: None,
            div_divider: 0,
            skip_div_event: 0,
            cgb_de: false,
            channel_tap: None,
            analog: analog::AnalogStage::default(),
        }
    }

    /// Select the analog stage's model family (the DAC-off fade and output
    /// high-pass share one RC per machine). Called from `GB::new` and re-applied
    /// after a savestate load, exactly like the other hardware-identity setters.
    pub(crate) fn set_analog_model(&mut self, model: analog::AnalogModel) {
        self.analog.set_model(model);
    }

    /// Engage/disengage the per-sample channel tap (recording/measurement).
    pub fn set_channel_tap(&mut self, on: bool) {
        self.channel_tap = on.then(Vec::new);
    }

    /// Take the tapped samples accumulated since the last drain.
    pub fn drain_channel_tap(&mut self) -> Vec<ChannelSample> {
        self.channel_tap.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// DIV-APU event (a DIV-APU falling edge, the master clock
    /// crossing a 0x1000-cc boundary): advance `div_divider` (unless the
    /// power-on skip glitch eats this event), run the 64 Hz envelope frame
    /// countdown, and consume any armed envelope ticks. Length and sweep stay
    /// on their cc-event models.
    fn fs_div_event(&mut self) {
        let cc = self.cc;
        self.fs_div_event_at(cc);
    }

    /// The DIV-APU event with its exact boundary cc. `event_cc` feeds the
    /// envelope frame's trigger-race window: an NRx4 trigger 2 cc (one CPU
    /// write M-cycle) or less before the frame boundary shares the M-cycle with
    /// the event, and its freshly-reloaded countdown escapes that frame's
    /// decrement (the envelope keys its frame quantum on `(cc + 2) & 0x7000`).
    /// Pan Docs: Audio details, Obscure Behavior (envelope timer reload on
    /// trigger) — https://gbdev.io/pandocs/Audio_details.html
    fn fs_div_event_at(&mut self, event_cc: u32) {
        match self.skip_div_event {
            2 => {
                self.skip_div_event = 1;
                return;
            }
            1 => self.skip_div_event = 0,
            _ => self.div_divider = self.div_divider.wrapping_add(1),
        }
        if self.div_divider & 7 == 7 {
            self.channel1.env_frame_countdown(event_cc);
            self.channel2.env_frame_countdown(event_cc);
            self.channel4.env_frame_countdown(event_cc);
        }
        self.channel1.env_div_tick();
        self.channel2.env_div_tick();
        self.channel4.env_div_tick();
    }

    /// DIV-APU secondary event (the rising edge, cc crossing a
    /// 0x800-offset boundary): reload zero envelope countdowns and arm the
    /// tick for the next DIV-APU event.
    fn fs_secondary_event(&mut self) {
        self.channel1.env_secondary_reload();
        self.channel2.env_secondary_reload();
        self.channel4.env_secondary_reload();
    }

    /// Walk the DIV-APU half-period (0x800-cc) boundaries crossed by a forward
    /// clock advance from `pre_cc` over `cycles`, dispatching falling
    /// (div event) and rising (secondary) edges in order.
    ///
    /// The hardware edge grid sits at cc ≡ -2 (mod 0x800): the envelope unit
    /// keys the frame quantum on `cc + 2` (the `(cc + 2) & 0x7000` period
    /// bump), so a trigger 1-2 cc before a raw 0x1000 multiple misses that
    /// frame's countdown decrement. cc + 2 ≡ 0 (mod 0x1000) is the falling edge
    /// (DIV-APU event); cc + 2 ≡ 0x800 is the rising edge (secondary).
    /// Base in Pan Docs (Audio_details: DIV-APU event grid + the envelope
    /// trigger-reload race); the -2 sub-cycle edge phase is a novel refinement,
    /// not in Pan Docs, TCAGBD, or GBCTR.
    fn fs_walk(&mut self, pre_cc: u32, cycles: u64) {
        if !self.audio_enabled {
            return;
        }
        let pre = pre_cc.wrapping_add(0) % Self::CC_MAX;
        let crossed = (((pre & 0x7FF) as u64) + cycles) >> 11;
        if crossed == 0 {
            return;
        }
        // First boundary index is (pre >> 11) + 1; even index = 0x1000
        // multiple = falling edge.
        let mut falling = (pre >> 11) & 1 == 1;
        let mut event_cc = (pre & !0x7FF).wrapping_add(0x800) % Self::CC_MAX;
        for _ in 0..crossed {
            if falling {
                self.fs_div_event_at(event_cc);
            } else {
                self.fs_secondary_event();
            }
            falling = !falling;
            event_cc = event_cc.wrapping_add(0x800) % Self::CC_MAX;
        }
    }

    const CC_MAX: u32 = 0x8000_0000;

    // Epoch fold: the master clock is kept well below the CC_MAX wrap by
    // rebasing cc (and every channel anchor derived from it) down by a fixed
    // delta whenever a clock advance leaves cc at or above the threshold.
    // Without it the `% CC_MAX` wrap (~17 emulated minutes) strands every
    // absolute-cc anchor ~2^31 in the future and all four channels freeze
    // until an NR52 power cycle.
    //
    // The delta is a multiple of every consumed grid — length 0x2000, sweep
    // 0x4000, DIV-APU 0x800/0x1000, envelope frame 0x8000 — and even, so every
    // relative phase (including the `lf_div == (cc&1)^1` parity relation)
    // survives. The threshold exceeds the maximum scheduled-ahead distance
    // (length <= ~0x20_2000, sweep <= 0x2_0000, wave <= period+3), so at fold
    // time every armed target is > cc >= threshold > delta and the plain
    // ordering of armed comparisons is preserved; stale/disarmed anchors are
    // shifted with wrapping_sub. While the APU is powered, chunks are grid-
    // bounded (<= 0x800 cc) so the fold keeps cc < threshold + 0x800 and the
    // CC_MAX modulo is unreachable; the modulo stays for the powered-off
    // giant chunk, where every foldable anchor is dormant (the power-off
    // register-zeroing pass disarms all length targets, and the surviving
    // sweep/env anchors are rescheduled on trigger before any poll).
    const EPOCH_FOLD_THRESHOLD: u32 = 0x6000_0000;
    const EPOCH_FOLD_DELTA: u32 = 0x4000_0000;

    /// Rebase the clock epoch when a clock advance leaves `cc` at or above
    /// the fold threshold: shift `cc`/`len_cc` and every channel anchor down
    /// by `EPOCH_FOLD_DELTA`. Total and phase-preserving, unlike the DIV-reset
    /// / speed-change / power-on folds (which deliberately drop phase).
    fn epoch_fold(&mut self) {
        if self.cc < Self::EPOCH_FOLD_THRESHOLD {
            return;
        }
        let delta = Self::EPOCH_FOLD_DELTA;
        self.cc -= delta;
        self.len_cc = self.cc;
        self.channel1.epoch_fold(delta);
        self.channel2.epoch_fold(delta);
        self.channel3.epoch_fold(delta);
        self.channel4.epoch_fold(delta);
    }

    /// Lazily catch the whole APU (clock AND channels) up to the timer's
    /// absolute cc. This is the ONLY driver of APU time: the per-dot crank no
    /// longer steps audio, so every observer path (APU register reads/writes,
    /// DIV writes, speed switches, sample generation) funnels through here
    /// first. Byte-identical to the retired per-dot `step_audio` crank by
    /// construction: the clock advances in chunks that stop at every cc where
    /// the per-dot interleave could act — each DIV-APU half-period grid cell
    /// (so envelope events fire between the duty ticks exactly as before) and
    /// each armed ch1 sweep counter (so the `cc >= counter` polls fire at the
    /// same cc) — and the per-dot postlude (push, length poll, channel step)
    /// runs at each stop. In between, the channels' own advance routines are
    /// span-exact (duty/wave/ripple are delta-driven catch-ups).
    ///
    /// A DIV write resets the timer's internal counter, dropping the sub-step
    /// part of cc. The DIV-reset fold preserves the upper cc bits (the length
    /// `cc>>13` / frame-sequencer boundaries) and shifts only the duty unit by
    /// the resulting delta.
    pub(crate) fn sync_cc(
        &mut self,
        abs_cc: u64,
        div_resets: u64,
        div_anchor: u64,
        ds: bool,
        cgb: bool,
        agb: bool,
    ) {
        // The per-access PCM read cc only lives for the access that set it
        // (`set_read_len_cc` runs after this sync, `pcm12`/`pcm34` consume it
        // within the same access). Any later sync means that access is over;
        // out-of-band reads then resolve at the current cc.
        self.pcm_read_cc = None;
        self.cached_ds = ds;
        if !self.clock_anchored {
            // Defer the boot anchor past the abs_cc==0 pre-boot sync: the
            // post-boot anchor sets `last_update = abs_cc - 1`, which would
            // underflow and freeze `advance_to` at abs_cc==0.
            if abs_cc == 0 {
                self.last_div_resets = div_resets;
                self.push_cc();
                return;
            }
            // Post-boot APU cycle counter: a fixed per-mode high constant
            // (0x1E00 CGB / 0x2400 DMG) OR the low 9 bits of `abs_cc>>1`.
            // Not in Pan Docs, TCAGBD (§10 audio is one sentence), or GBCTR;
            // post-boot APU clock anchor reverse-engineered from
            // test-ROM refs.
            let high = if self.boot_cgb { 0x1E00u32 } else { 0x2400u32 };
            self.cc = (high | ((abs_cc >> 1) as u32 & 0x1FF)) & (Self::CC_MAX - 1);
            self.len_cc = self.cc;
            // The floored boundary sits one cc below the current cpu cc so the
            // first sample generation picks up the right parity remainder.
            self.last_update = abs_cc - 1;
            self.last_div_resets = div_resets;
            self.clock_anchored = true;
            // Seed div_divider to the post-boot phase so the first crossing
            // lands the envelope frame ((div_divider & 7) == 7) on the absolute
            // cc grid: frames at (cc+2)>>12 ≡ 0 (mod 8), ticks at ≡ 1 (CGB
            // anchor 0x1E00 -> 0; DMG anchor 0x2400 -> 1).
            self.div_divider = ((self.cc.wrapping_add(0) >> 12).wrapping_sub(1) & 7) as u16;
            self.skip_div_event = 0;
            // Seed the free-running lf_div to the post-boot phase (invariant
            // `lf_div == (cc&1)^1` at the anchor); from here it free-runs on
            // elapsed 2 MHz cycles only.
            self.lf_div = (self.cc & 1) ^ 1;
            self.push_cc();
            return;
        }

        // A DIV write resets the divider. Sample generation then the divider
        // reset both run AT the DIV-write cc: advance to `div_anchor` (the
        // timer's access cc for the FF04 write), fire any length events
        // strictly before the fold, then fold there — not at the later dot.
        if div_resets != self.last_div_resets {
            // Run the fold AT the FF04 write's access cc (`div_anchor`), not the
            // later current dot, so the length-expiry boundary
            // `((cc>>13)+len)<<13` is anchored to the same per-access cc the
            // subsequent NR52 read resolves on.
            self.advance_chunked(div_anchor, ds, cgb, agb);
            self.push_cc();
            self.fire_length_events(self.cc);
            self.div_reset_fold(ds);
            self.last_div_resets = div_resets;
        }

        // Steady state: chunked catch-up to the current cc. When the clock is
        // already current but the channels' ds flag changed, still re-broadcast
        // and re-poll (mirrors the old per-dot ds-change push).
        let advanced = self.advance_chunked(abs_cc, ds, cgb, agb);
        if !advanced && ds != self.last_pushed_ds {
            self.push_cc();
            self.fire_length_events(self.cc);
            self.step_channels(cgb, agb);
        }
    }

    /// Advance the APU clock from `last_update` to `abs_cc` in event-bounded
    /// chunks, running the per-dot postlude (cc broadcast, length poll,
    /// channel catch-up/polls) at every chunk end. Chunk stops (see `sync_cc`
    /// doc): one cc before each DIV-APU half-period boundary AND the boundary
    /// itself (duty ticks strictly before an envelope event must resolve
    /// against the pre-event volume, the boundary tick against the post-event
    /// volume — the per-dot interleave), plus every armed future ch1 sweep
    /// counter. Returns whether the clock advanced at all.
    fn advance_chunked(&mut self, abs_cc: u64, ds: bool, cgb: bool, agb: bool) -> bool {
        let shift = 1 + ds as u32;
        let mut any = false;
        // Guard against a non-monotonic target (a DIV-write access cc that
        // resolves just before the current dot): never run backward.
        while abs_cc > self.last_update {
            let cycles = (abs_cc - self.last_update) >> shift;
            if cycles == 0 {
                break;
            }
            let chunk = if self.audio_enabled {
                let cur = self.cc;
                // Next DIV-APU half-period boundary strictly above `cur`
                // (b <= CC_MAX; the wrap is applied after the add below).
                let b = ((cur >> 11) + 1) << 11;
                let grid_stop = if cur + 1 < b { (b - 1 - cur) as u64 } else { (b - cur) as u64 };
                let stop = match self.channel1.next_sweep_stop(cur) {
                    Some(d) => grid_stop.min(d as u64),
                    None => grid_stop,
                };
                stop.min(cycles)
            } else {
                // APU off: no channel work, no envelope grid (fs_walk bails on
                // !audio_enabled), so the whole span is one chunk.
                cycles
            };
            self.last_update = self.last_update.wrapping_add(chunk << shift);
            let pre_cc = self.cc;
            self.cc = ((self.cc as u64 + chunk) % Self::CC_MAX as u64) as u32;
            self.len_cc = self.cc;
            // The 1 MHz sub-phase free-runs on elapsed 2 MHz cycle parity.
            self.lf_div ^= (chunk & 1) as u32;
            // Dispatch the DIV-APU (envelope) events crossed by this advance
            // (at most one — the chunker never crosses two boundaries).
            self.fs_walk(pre_cc, chunk);
            any = true;
            // Rebase the epoch before the postlude broadcasts cc.
            self.epoch_fold();
            // Per-dot postlude at the chunk end.
            self.push_cc();
            self.fire_length_events(self.cc);
            self.step_channels(cgb, agb);
        }
        any
    }

    /// The retired per-dot `Audio::step` body: channel catch-up + cc-event
    /// polls (ch1 sweep triple), run at every catch-up chunk end.
    fn step_channels(&mut self, cgb: bool, agb: bool) {
        if !self.audio_enabled {
            return;
        }
        self.channel1.step(cgb);
        self.channel2.step(cgb);
        self.channel3.step(cgb, agb);
        self.channel4.step();
    }

    /// Convert CPU cycles since `last_update` to 2 MHz APU cycles and advance
    /// `cc`. Audio isn't buffered here (the live mixer is sampled elsewhere),
    /// so this only moves the clock.
    fn advance_to(&mut self, abs_cc: u64, ds: bool) -> bool {
        // `step_audio` is gated to half-rate in double speed, so `abs_cc`
        // advances at the physical APU rate the duty/envelope tuning is
        // anchored to: shift by 1 in both speeds. At double speed the divider
        // runs twice as fast in CPU-cc terms, so the APU clock advances at half
        // the rate — the duty period `(2048-freq)*2` is in the same 2 MHz units
        // regardless of speed. Count whole APU cycles on absolute even
        // boundaries so the floored phase aligns to absolute parity.
        let shift = 1 + ds as u32;
        // `cycles` is the cc delta taken BEFORE the shift; `last_update`
        // advances by whole APU cycles (`cycles << shift`), staying a floored
        // boundary that preserves the sub-quantum remainder/parity. Guard
        // against a non-monotonic target (a DIV-write access cc that resolves
        // just before the current dot): never run backward.
        if abs_cc <= self.last_update {
            return false;
        }
        let cycles = (abs_cc - self.last_update) >> shift;
        if cycles == 0 {
            return false;
        }
        self.last_update = self.last_update.wrapping_add(cycles << shift);
        let pre_cc = self.cc;
        self.cc = ((self.cc as u64 + cycles) % Self::CC_MAX as u64) as u32;
        self.len_cc = self.cc;
        // The 1 MHz sub-phase free-runs on elapsed 2 MHz cycle parity.
        self.lf_div ^= (cycles & 1) as u32;
        // Dispatch the DIV-APU (envelope) events crossed by this advance.
        self.fs_walk(pre_cc, cycles);
        // Rebase the epoch here too: this path (the speed-change flush) does
        // not run the chunked postlude, but one sync can advance far.
        self.epoch_fold();
        true
    }

    /// Re-fold the APU cycle counter so the DIV-relative phase
    /// resets while the length `cc>>13` boundaries are preserved. The duty unit is
    /// shifted by the resulting delta.
    fn div_reset_fold(&mut self, ds: bool) {
        let div_offset = (self.last_update as u32) & (ds as u32);
        let cc = self.cc.wrapping_add(div_offset);
        let folded = (cc & 0xFFFF_F000)
            .wrapping_add(2 * (cc & 0x800))
            .wrapping_sub(div_offset)
            % Self::CC_MAX;
        // Resetting DIV while the DIV-APU bit is high is a falling edge — the
        // DIV-APU event fires AT the write. (The fold expresses this by jumping
        // cc forward across the 0x1000 boundary; the low-12-bit reset itself
        // never crosses one.) The bit is read in the -2 event-grid frame (see
        // fs_walk): a cc in the last 2 cells of the high half already fired its
        // falling edge in advance_to and must not double-fire here.
        // Pan Docs: Timer obscure behaviour (DIV write fires DIV-APU event) —
        // https://gbdev.io/pandocs/Timer_Obscure_Behaviour.html
        if self.audio_enabled && cc.wrapping_add(0) & 0x800 != 0 {
            self.fs_div_event();
        }
        let old = cc.wrapping_sub(div_offset);
        let delta = old.wrapping_sub(folded);
        self.cc = folded;
        self.channel1.reset_cc(delta);
        self.channel2.reset_cc(delta);
        self.channel3.reset_cc(delta);
        self.channel4.reset_cc(old, delta);
        // `len_cc` mirrors `cc`; the channels' length boundaries survive
        // because the fold preserves `cc & -0x1000`.
        self.len_cc = self.cc;
    }

    fn push_cc(&mut self) {
        self.last_pushed_ds = self.cached_ds;
        let cc = self.cc;
        self.channel1.set_cc(cc);
        self.channel2.set_cc(cc);
        self.channel3.set_cc(cc);
        self.channel4.set_cc(cc);
        // `lf_div`: the 1 MHz sub-phase for the trigger delay, immune to the
        // DIV-reset and speed-switch cc folds.
        self.channel1.set_lf_div(self.lf_div);
        self.channel2.set_lf_div(self.lf_div);
        self.channel1.set_ds(self.cached_ds);
        self.channel2.set_ds(self.cached_ds);
        self.channel4.set_ds(self.cached_ds);
        // Length cc is `cc` itself.
        let lcc = cc;
        self.channel1.set_len_cc(lcc);
        self.channel2.set_len_cc(lcc);
        self.channel3.set_len_cc(lcc);
        self.channel4.set_len_cc(lcc);
    }

    /// The length unit is a scheduled absolute-cc event: when the master
    /// clock reaches a channel's scheduled expiry cc (`((cc>>13)+len)<<13`), the
    /// channel's length expires (disables it). We poll it each clock advance.
    fn fire_length_events(&mut self, _cc: u32) {
        if self.channel1.len_expired() {
            self.channel1.length_event();
        }
        if self.channel2.len_expired() {
            self.channel2.length_event();
        }
        if self.channel3.len_expired() {
            self.channel3.length_event();
        }
        if self.channel4.len_expired() {
            self.channel4.length_event();
        }
    }

    /// APU-enable reset, fired on the NR52 0→1 (APU enable) transition. Folds
    /// the master clock from its large `abs_cc>>1`-anchored value down to the small
    /// FS-anchored value the cycle counter carries, then re-initializes
    /// every channel's duty/envelope/LFSR sub-counter at the folded cc. The length
    /// counters survive (they're re-derived against the new small `cc>>13`).
    ///
    /// Folding the whole APU clock here re-anchors the length-expiry boundary
    /// `((cc>>13)+len)<<13`, which would otherwise be computed against the
    /// un-folded large anchor and land one 0x1000 quantum off after a DIV write.
    /// The observable effects (registers clear, length counters survive — which
    /// after the power-off zeroing means 0, or a DMG while-off NRx1 load) are in
    /// Pan Docs (Audio_Registers); this cc-fold mechanism is a model construct,
    /// not in Pan Docs, TCAGBD, or GBCTR.
    fn psg_reset(&mut self, ds: bool) {
        // Skip the fold before the APU master clock is anchored (boot instant,
        // `cc`/`last_update` still 0): there's no accumulated phase to fold, and
        // the fold formula would inject a spurious +0x1000 that offsets `cc>>13`
        // (the length quantum) for the rest of the run. The channel sub-counters
        // are still reset.
        if !self.clock_anchored {
            // APU power-on resets the 1 MHz sub-phase.
            self.lf_div = 1;
            self.div_divider = 0;
            self.skip_div_event = 0;
            self.push_cc();
            self.channel1.psg_reset();
            self.channel2.psg_reset();
            self.channel3.psg_reset();
            self.channel4.psg_reset();
            return;
        }
        // APU power-on (NR52 0->1) re-seeds lf_div at the power-on write cc —
        // the only event besides boot that re-seeds the free-running sub-phase.
        // The seed is speed-dependent: a DS M-cycle covers one 2 MHz cycle, an
        // SS M-cycle two, so the 1 MHz phase the duty unit latches differs by
        // one (1 in single speed, 0 in double speed). This forks by revision:
        // the DS seed 0 is the CGB-C placement (square DS delay 5-2a-lf);
        // CGB-D/E takes seed 1 always with DS delay 6-2a-lf
        // (`6 + lf * (model < CGB_D && ds ? 1 : -1)`). `cgb_de` selects the
        // D/E side.
        // Pan Docs notes a first-trigger duty quirk (Audio_details) and that
        // APU revisions differ (CGB-02 vs 04/05 length glitch), but this
        // per-revision DS/SS duty-trigger sub-phase is not in Pan Docs, TCAGBD,
        // or GBCTR; reverse-engineered from test-ROM refs.
        self.lf_div = if self.cached_ds && !self.cgb_de { 0 } else { 1 };
        // APU power-on DIV-APU glitch: enabling the APU while the DIV-APU bit
        // (the half-period phase, read in the -2 event-grid frame of fs_walk)
        // is high skips the first (truncated) DIV-APU event, with div_divider
        // pre-set to 1.
        self.div_divider = 0;
        self.skip_div_event = 0;
        if self
            .cc
            .wrapping_add((self.last_update as u32) & (ds as u32))
            .wrapping_add(2u32)
            & 0x800
            != 0
        {
            self.skip_div_event = 2;
            self.div_divider = 1;
        }
        // APU-enable reset. `last_update` is the exact floored boundary, so
        // `cc` already equals the cycle counter.
        let div_offset = (self.last_update as u32) & (ds as u32);
        let cc = self.cc.wrapping_add(div_offset);
        let not_ds = (!ds) as u32;
        let folded = ((cc & 0xFFF)
            .wrapping_add(2 * (!(cc.wrapping_add(1).wrapping_add(not_ds)) & 0x800)))
            % Self::CC_MAX;
        self.cc = folded;
        self.len_cc = folded;
        // Re-anchor last_update parity: ((last_update + 3) & -4) - !ds.
        self.last_update = (self.last_update.wrapping_add(3) & !3u64)
            .wrapping_sub(not_ds as u64);
        // Push the folded cc into the channels first so ch4's LFSR reset
        // anchors its backup counter at the folded cc.
        self.push_cc();
        self.channel1.psg_reset();
        self.channel2.psg_reset();
        self.channel3.psg_reset();
        self.channel4.psg_reset();
    }

    /// APU clock fold for the CGB STOP speed switch. `stop_cc` is the master cc
    /// the STOP resolves at; the flush target is `cc_ = stop_cc + 8 * !old_ds`.
    /// The counter is flushed to `cc_` at the OLD speed, then `last_update -=
    /// old_ds`, then the single→double fold.
    /// Base in TCAGBD (§5.1.1 double-speed uses the next DIV bit; §10 sound
    /// frequency is unaffected by double speed); this STOP speed-switch cc-fold
    /// is a novel refinement, not in Pan Docs, TCAGBD, or GBCTR.
    pub(crate) fn psg_speed_change_at(&mut self, old_ds: bool, stop_cc: u64) {
        if !self.clock_anchored {
            return;
        }
        let cc_ = stop_cc + 8 * (!old_ds as u64);
        // Flush the counter to the switch cc at the old speed.
        self.advance_to(cc_, old_ds);
        // The DS->SS STOP bridge advances the 1 MHz sub-phase by an odd number
        // of 2 MHz cycles, flipping lf_div; the SS->DS direction is parity-even
        // and leaves it alone.
        if old_ds {
            self.lf_div ^= 1;
        }
        if old_ds {
            self.last_update = self.last_update.wrapping_sub(1);
        }
        // Only the single->double transition re-folds the counter.
        if !old_ds {
            let cc = self.cc;
            let div_cycles = cc & 0xFFF;
            let folded = cc
                .wrapping_sub(div_cycles / 2)
                .wrapping_sub((self.last_update % 2) as u32)
                % Self::CC_MAX;
            let delta = cc.wrapping_sub(folded);
            self.cc = folded;
            self.len_cc = folded;
            self.channel1.reset_cc(delta);
            self.channel2.reset_cc(delta);
            self.channel3.reset_cc(delta);
            self.channel4.reset_cc(cc, delta);
            self.push_cc();
            self.fire_length_events(self.cc);
        }
    }

    /// Advance only the wave channel's fetch counter to the current cc, for the
    /// CPU read path. Does not run square envelope/length events.
    pub(crate) fn sync_wave_for_read(&mut self) {
        if self.audio_enabled {
            self.channel3.sync_for_read();
            self.channel4.sync_for_read();
        }
    }

    /// Resolve the length subsystem at the CPU-access cc on an APU register
    /// read. `read_abs_cc` is the master cc at the exact access point (the same
    /// cc the timer register access resolves on); it may run a few dots ahead
    /// of the per-dot `self.last_update` the duty/envelope sub-counters use.
    ///
    /// Overlays each channel's length-comparison cc (`len_cc`) at the access cc
    /// and fires any length expiry there, WITHOUT disturbing
    /// `self.cc`/`last_update`/duty, so the NR52 length-expiry boundary
    /// (`((cc>>13)+len)<<13` vs the read cc) resolves at the same access cc as
    /// the timer.
    pub(crate) fn set_read_len_cc(&mut self, read_abs_cc: u64) {
        if !self.clock_anchored {
            return;
        }
        let shift = 1 + self.cached_ds as u32;
        // The delta is taken BEFORE the shift: flooring `read_abs_cc` and
        // `last_update` independently would over-count by one length-cc when
        // they straddle a `>>shift` boundary, pushing the read one cc past the
        // expiry boundary.
        let delta = (read_abs_cc.wrapping_sub(self.last_update) >> shift) as u32;
        let lcc = self.len_cc.wrapping_add(delta);
        // Record the per-access read cc so a PCM12 read in this access resolves
        // the square duty at the same access clock, not the earlier per-dot sync.
        self.pcm_read_cc = Some(self.cc.wrapping_add(delta));
        self.channel1.set_len_cc(lcc);
        self.channel2.set_len_cc(lcc);
        self.channel3.set_len_cc(lcc);
        self.channel4.set_len_cc(lcc);
        self.fire_length_events(lcc);
        // Restore the steady-state length cc so the next per-dot `push_cc`
        // doesn't see a stale ahead value.
        let base = self.len_cc;
        self.channel1.set_len_cc(base);
        self.channel2.set_len_cc(base);
        self.channel3.set_len_cc(base);
        self.channel4.set_len_cc(base);
    }

    /// Overlay the length subsystem cc (`len_cc`) at the CPU WRITE access cc,
    /// so the NRx1/NRx4 length-counter math (trigger reload + expiry
    /// scheduling, `((len_cc>>13)+len)<<13`) is anchored to the same per-access
    /// clock the subsequent NR52 read resolves on. The write side is a separate
    /// phase term from the read (its trigger-boundary rounding differs from the
    /// read's event rounding), so `write_abs_cc` carries the write access cc.
    /// Unlike the read overlay this LEAVES `len_cc` set: the immediately-
    /// following `audio.write` consumes it, and the next per-dot `push_cc`
    /// restores the steady-state base. Duty/envelope (`self.cc`) are untouched.
    pub(crate) fn set_write_len_cc(&mut self, write_abs_cc: u64) {
        if !self.clock_anchored {
            return;
        }
        let shift = 1 + self.cached_ds as u32;
        // Before-shift delta, matching the read path.
        let delta = (write_abs_cc.wrapping_sub(self.last_update) >> shift) as u32;
        let lcc = self.len_cc.wrapping_add(delta);
        self.channel1.set_len_cc(lcc);
        self.channel2.set_len_cc(lcc);
        self.channel3.set_len_cc(lcc);
        self.channel4.set_len_cc(lcc);
        // Fire expiries at the overlay cc, mirroring the read side. Today the
        // delta is always 0 (timer WRITE_CC_OFF = 0) so this is a no-op; it is
        // here so a retuned write-cc offset can't silently skip an expiry the
        // read overlay would have fired.
        self.fire_length_events(lcc);
    }

    /// Restore the steady-state length cc after a write overlay, so a later
    /// per-dot poll doesn't see a stale ahead value before the next `push_cc`.
    pub(crate) fn restore_len_cc(&mut self) {
        if !self.clock_anchored {
            return;
        }
        let base = self.len_cc;
        self.channel1.set_len_cc(base);
        self.channel2.set_len_cc(base);
        self.channel3.set_len_cc(base);
        self.channel4.set_len_cc(base);
    }

    /// Record the CGB/DMG flag (and seed it into channel 4). Called from
    /// `GB::new`, before any audio write can anchor the SPU clock, so the
    /// post-boot clock high-bit constant, the DMG-only NRx1-writable-while-off
    /// exception, and channel 4's DMG deferred-trigger fork are right on
    /// every boot path (skip_bios AND the real boot ROM).
    pub(crate) fn set_boot_cgb(&mut self, cgb: bool) {
        self.boot_cgb = cgb;
        self.channel4.set_cgb(cgb);
    }

    /// Seed the CGB-D/E APU revision gate (model newer than CGB-C)
    /// into the revision-forked units: the square duty-trigger DS delay pair
    /// (psg_reset lf seed + DS delay formula) and the ch4 divisor-0 DS
    /// countdown. Called once from `GB::new` for Hardware::CGBE.
    /// Set the machine's real-time CPU clock, which fixes how many dots make one
    /// 44.1 kHz host sample. Affects only the downsample ratio in
    /// `generate_samples` — no channel timer, length counter, or frame-sequencer
    /// step reads it, so the dot-domain APU state stays byte-identical.
    pub fn set_cpu_hz(&mut self, hz: u32) {
        self.cycles_per_sample = hz as f32 / HOST_SAMPLE_RATE;
    }

    /// Dots per host sample (`cpu_hz / 44100`).
    pub fn cycles_per_sample(&self) -> f32 {
        self.cycles_per_sample
    }

    pub(crate) fn set_cgb_de(&mut self, de: bool) {
        self.cgb_de = de;
        self.channel1.set_cgb_de(de);
        self.channel2.set_cgb_de(de);
        self.channel4.set_cgb_de(de);
    }

    /// Seed the CGB-B-or-earlier APU revision gate (CGB with model <= CGB-B)
    /// into all four channels' NRx4 length-glitch
    /// fork. Called once from `GB::new` for Hardware::CGB0/CGBB.
    pub(crate) fn set_cgb_le_b(&mut self, le_b: bool) {
        self.channel1.set_cgb_le_b(le_b);
        self.channel2.set_cgb_le_b(le_b);
        self.channel3.set_cgb_le_b(le_b);
        self.channel4.set_cgb_le_b(le_b);
    }

    /// CPU-CGB-A/B (Hardware::CGBB) wave first-glitch-write swallow.
    pub(crate) fn set_cgb_b(&mut self, b: bool) {
        self.channel3.set_cgb_b(b);
    }

    /// CGB-C-and-older PCM read glitch (the pcm_mask applied for
    /// model <= CGB-C; excludes AGB and CGB-D/E).
    pub(crate) fn set_pcm_c_glitch(&mut self, on: bool) {
        self.channel1.set_pcm_c_glitch(on);
        self.channel2.set_pcm_c_glitch(on);
    }

    /// NRx4 sample-index step-back parity gate for the two square channels
    /// (true for CGB0/CGBB/AGB; the step-back is gated on
    /// `sample_countdown & 1` for those, unconditional on CGB-D/E).
    pub(crate) fn set_step_back_parity(&mut self, on: bool) {
        self.channel1.set_step_back_parity(on);
        self.channel2.set_step_back_parity(on);
    }

    /// Seed the AGB flag into the wave channel (ch3 wave-RAM behavior) and into
    /// the three envelope channels, where AGB takes the CGB-D/E side of the
    /// NRx2 zombie transform (see `nrx2_glitch`).
    pub(crate) fn set_agb(&mut self, agb: bool) {
        self.channel1.set_agb(agb);
        self.channel2.set_agb(agb);
        self.channel3.set_agb(agb);
        self.channel4.set_agb(agb);
    }

    /// Seed the post-boot APU state. `cgb` selects the CGB vs DMG channel-1
    /// startup-tone phase; `ch1_active` is the NR52 bit-0 (channel-1 running)
    /// state at hand-off. The DMG/MGB/CGB boot ROMs play the startup "ding" and
    /// hand off with channel 1 still running (bit 0 = 1); the SGB boot ROM
    /// plays no chime on the Game Boy side, so it hands off with channel 1
    /// already disabled (NR52 reads 0xF0, not 0xF1).
    pub(crate) fn set_post_bios_state(&mut self, cgb: bool, ch1_active: bool) {
        self.audio_enabled = true;
        self.nr50 = 0x77;
        self.nr51 = 0xF3;
        self.nr52 = 0xF1;

        // Channel 1 startup-tone phase: CGB pos=6/high, offset 37*2; DMG pos=3,
        // offset 69*2.
        if cgb {
            self.channel1.set_post_bios_ch1(37 * 2, 6, true);
        } else {
            self.channel1.set_post_bios_ch1(69 * 2, 3, false);
        }
        // SGB: same register bytes as DMG, but channel 1 is not left running.
        if !ch1_active {
            self.channel1.set_enabled(false);
        }

        // Hardware post-boot length counters: the boot ROMs write no NRx1 but
        // NR11 (=0x80, loading CH1's 64 — seeded in set_post_bios_ch1 above),
        // so CH2/CH3/CH4 hold the power-on value 0. A later trigger with
        // length enabled reloads a 0 counter to its max (64/256/64). Seeded
        // explicitly so no skip_bios boot-table write can leak a DMG
        // while-off length load into the hidden counters.
        self.channel2.set_length_counter(0);
        self.channel3.set_length_counter(0);
        self.channel4.set_length_counter(0);
    }

    /// True while the APU is powered (NR52 bit 7). The min-event idle fast path
    /// only bulk-skips dots when audio is OFF, because a powered APU steps its
    /// channel duty/freq counters per dot (`step`), which is not
    /// span-collapsible like the frame sequencer.
    pub(crate) fn is_powered(&self) -> bool {
        self.audio_enabled
    }

    /// CGB PCM12 register (0xFF76): low nibble = channel 1 digital output, high
    /// nibble = channel 2. Returns 0 when the APU is powered off; the CGB-only
    /// / power gating is applied by the caller in `mmio.rs`.
    /// Pan Docs: Audio details, PCM registers —
    /// https://gbdev.io/pandocs/Audio_details.html
    pub(crate) fn pcm12(&self) -> u8 {
        if !self.audio_enabled {
            return 0;
        }
        // Resolve the duty at the per-access read cc when the read path
        // recorded one; fall back to the per-dot state (mixer path).
        match self.pcm_read_cc {
            Some(rcc) => {
                self.channel1.pcm_nibble_at(rcc) | (self.channel2.pcm_nibble_at(rcc) << 4)
            }
            _ => self.channel1.pcm_nibble() | (self.channel2.pcm_nibble() << 4),
        }
    }

    /// CGB PCM34 register (0xFF77): low nibble = channel 3, high nibble =
    /// channel 4.
    pub(crate) fn pcm34(&self) -> u8 {
        if !self.audio_enabled {
            return 0;
        }
        // Channel 4 resolves at the per-access read cc like PCM12; channel 3's
        // fetch counter was already advanced on the read path
        // (`sync_wave_for_read`).
        let ch4 = match self.pcm_read_cc {
            Some(rcc) => self.channel4.pcm_nibble_at(rcc),
            _ => self.channel4.pcm_nibble(),
        };
        self.channel3.pcm_nibble() | (ch4 << 4)
    }

    /// The four post-DAC analog channel levels, in channel order. Each is
    /// either one of the 16 DAC levels or 0.0 for an unpowered DAC — a small
    /// discrete alphabet, which is what the tap (and the `.rba` per-plane
    /// palette encoder behind it) requires. The DAC-off fade and the output
    /// high-pass are continuous and therefore live strictly downstream.
    fn channel_outputs(&self) -> [f32; 4] {
        [
            self.channel1.get_output(),
            self.channel2.get_output(),
            self.channel3.get_output(),
            self.channel4.get_output(),
        ]
    }

    /// Which channels currently have a powered DAC, in channel order.
    fn channel_dacs_on(&self) -> [bool; 4] {
        [
            self.channel1.dac_on(),
            self.channel2.dac_on(),
            self.channel3.dac_on(),
            self.channel4.dac_on(),
        ]
    }

    /// The mixer proper: NR51 routing, NR50 master volume, and the 4-channel
    /// normalize. Pure — it is the whole of what a tap sample reconstructs to.
    ///
    /// `rustyboi_replay::mix` is a bit-for-bit clone of this function and the
    /// f32 operation order is load-bearing (the compat gallery rebuilds audio
    /// through it); the two must change together.
    fn mix_stereo(ch: [f32; 4], nr50: u8, nr51: u8, enabled: bool) -> (f32, f32) {
        if !enabled {
            return (0.0, 0.0);
        }

        let mut left_mix = 0.0;
        let mut right_mix = 0.0;

        for (i, &out) in ch.iter().enumerate() {
            if nr51 & (1 << (i + 4)) != 0 {
                left_mix += out;
            }
        }
        for (i, &out) in ch.iter().enumerate() {
            if nr51 & (1 << i) != 0 {
                right_mix += out;
            }
        }

        // Apply master volume
        left_mix *= (((nr50 >> 4) & 0x07) as f32 + 1.0) / 8.0;
        right_mix *= ((nr50 & 0x07) as f32 + 1.0) / 8.0;

        // Divide by 4 to normalize since we're summing 4 channels
        (left_mix / 4.0, right_mix / 4.0)
    }

    /// Reconstruct the stereo mix of one tapped sample. This is the canonical
    /// definition the `.rba` decoder reproduces; exposed so a consumer holding
    /// tap data can check its own reconstruction against the core's.
    ///
    /// The result is PRE-analog-stage: it carries neither the DAC-off fade nor
    /// the output high-pass, both of which are continuous, stateful, and
    /// downstream of the tap.
    pub fn mix_tap_sample(sample: ChannelSample) -> (f32, f32) {
        let (ch, nr50, nr51, enabled) = sample;
        Self::mix_stereo(ch, nr50, nr51, enabled)
    }


    pub(crate) fn generate_samples(&mut self, cpu_cycles: u32) -> Vec<(f32, f32)> {
        let mut samples = Vec::new();

        // Channels are caught up lazily via `sync_cc` (the caller syncs the
        // APU to the current cc first), so here we only down-sample the live
        // mixer output. Re-advancing here would double-advance the channel
        // timers and corrupt their phase.
        // The divisor is the machine's own clock (an SGB1's is the host SNES's
        // / 5), so a fixed 70224-dot frame yields fewer host samples and every
        // tone comes out at `cpu_hz / period` — pitched up 2.4% on an NTSC SGB1.
        let cycles_per_sample = self.cycles_per_sample;

        self.fractional_cycles += cpu_cycles as f32;

        while self.fractional_cycles >= cycles_per_sample {
            samples.push(self.analog_sample());
            self.fractional_cycles -= cycles_per_sample;
        }

        samples
    }

    /// One host sample, taken all the way through the analog stage: the DACs'
    /// discrete levels are tapped, then faded (for any DAC that has gone
    /// unpowered), mixed, and high-passed.
    ///
    /// The tap is deliberately taken BEFORE the fade and the high-pass. Both
    /// are continuous, so tapping downstream of them would hand the `.rba`
    /// per-plane encoder an unbounded value alphabet — its palette is a `u16`,
    /// and building one over a fade ramp would both overflow it and make
    /// encoding quadratic.
    ///
    /// On an SGB this is still the whole output: the SGB's own effects come
    /// from the SNES APU, which is decoded but not synthesised
    /// ([`crate::sgb::SgbSound`]); adding them later means summing into this
    /// stream here or at a downstream sink, with no change to the channels.
    fn analog_sample(&mut self) -> (f32, f32) {
        let raw = self.channel_outputs();
        if let Some(tap) = &mut self.channel_tap {
            tap.push((raw, self.nr50, self.nr51, self.audio_enabled));
        }
        let faded = self.analog.fade(raw, self.channel_dacs_on());
        let (left, right) = Self::mix_stereo(faded, self.nr50, self.nr51, self.audio_enabled);
        self.analog.high_pass(left, right)
    }
}

impl Addressable for Audio {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR10..=NR14 => self.channel1.read(addr),
            NR21..=NR24 => self.channel2.read(addr),
            NR30..=NR34 => self.channel3.read(addr),
            NR41..=NR44 => self.channel4.read(addr),
            NR50 => self.nr50,
            NR51 => self.nr51,
            NR52 => {
                let mut value = self.nr52 & 0x80; // Preserve audio enabled bit

                // Set channel status bits (read-only)
                if self.channel1.is_enabled() { value |= 0x01; }
                if self.channel2.is_enabled() { value |= 0x02; }
                if self.channel3.is_enabled() { value |= 0x04; }
                if self.channel4.is_enabled() { value |= 0x08; }

                value | 0x70 // Bits 4-6 always read as 1
            }
            WAV_START..=WAV_END => self.channel3.read(addr),
            // Unused gaps in the APU register block (0xFF15, 0xFF1F,
            // 0xFF27-0xFF2F) read back as open bus.
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            // NRx1 length-load registers stay writable while the APU is off on
            // DMG (monochrome models); on CGB they are ignored like every other
            // register. While off, NR11/NR21 apply only the length bits
            // (`& 0x3F`), not the duty bits.
            // Pan Docs: Audio Registers (APU off makes registers read-only
            // except NR52, and length timers on monochrome) —
            // https://gbdev.io/pandocs/Audio_Registers.html
            NR11 => {
                if self.audio_enabled {
                    self.channel1.write(addr, value);
                } else if !self.boot_cgb {
                    self.channel1.write(addr, value & 0x3F);
                }
            },
            NR21 => {
                if self.audio_enabled {
                    self.channel2.write(addr, value);
                } else if !self.boot_cgb {
                    self.channel2.write(addr, value & 0x3F);
                }
            },
            NR31 => {
                if self.audio_enabled || !self.boot_cgb {
                    self.channel3.write(addr, value);
                }
            },
            NR41 => {
                if self.audio_enabled || !self.boot_cgb {
                    self.channel4.write(addr, value);
                }
            },
            NR10..=NR14 => {
                if self.audio_enabled {
                    self.channel1.write(addr, value)
                }
            },
            NR22..=NR24 => {
                if self.audio_enabled {
                    self.channel2.write(addr, value)
                }
            },
            NR30..=NR34 => {
                if self.audio_enabled {
                    self.channel3.write(addr, value)
                }
            },
            NR42..=NR44 => {
                if self.audio_enabled {
                    self.channel4.write(addr, value)
                }
            },
            NR50 => {
                if self.audio_enabled {
                    self.nr50 = value;
                }
            },
            NR51 => {
                if self.audio_enabled {
                    self.nr51 = value;
                }
            },
            NR52 => {
                let was_enabled = self.audio_enabled;
                let now_enabled = (value >> 7) & 0x01 != 0;

                if was_enabled && !now_enabled {
                    // APU power-off: while still enabled, write 0 to every sound
                    // register 0x10-0x25, THEN disable. The per-register writes
                    // take the normal (enabled) path, so the NRx4 zero-writes
                    // disarm every length expiry. The free-running master clock
                    // (cc/last_update) and wave RAM are left untouched.
                    // Pan Docs: Audio Registers (off clears registers but not
                    // wave RAM or the DIV-APU counter) —
                    // https://gbdev.io/pandocs/Audio_Registers.html
                    for reg in NR10..=NR51 {
                        // Skip the unused gaps (FF15, FF1F) — open bus, no effect.
                        if reg == 0xFF15 || reg == 0xFF1F {
                            continue;
                        }
                        self.write(reg, 0);
                    }
                    // The cascade's NRx1=0 writes reload each length counter to
                    // its max; hardware instead leaves the counters at zero
                    // (gbdev wiki "Power Control": "always zero at power on
                    // (CGB-02, CGB-04, CGB-05)"; SameBoy Core/apu.c memsets the
                    // APU state in its NR52 power-off handler). Undo the
                    // reloads. `psg_reset` (power-on) preserves counters, so
                    // what survives a power cycle is 0 — or, on DMG only, the
                    // value a while-off NRx1 write loaded afterwards, matching
                    // SameBoy's `!GB_is_cgb(gb) && (value & 0x80)` restore. The
                    // expiry targets stay disarmed: the NRx4 zero-writes above
                    // already set them to COUNTER_DISABLED, and none of these
                    // setters rearm one.
                    self.channel1.set_length_counter(0);
                    self.channel2.set_length_counter(0);
                    self.channel3.set_length_counter(0);
                    self.channel4.set_length_counter(0);
                    // `audio_enabled` stays true through the loop above (so the
                    // zero-writes take the enabled path) and is cleared by the
                    // `audio_enabled = now_enabled` at the end of this branch.
                } else if !was_enabled && now_enabled {
                    // APU power-on (NR52 0→1): apply the APU-enable reset fold.
                    self.psg_reset(self.cached_ds);
                }
                self.audio_enabled = now_enabled;
                self.nr52 = value;
            },
            WAV_START..=WAV_END => {
                // Wave RAM can be accessed even when audio is disabled
                self.channel3.write(addr, value);
            },
            // Unused gaps in the APU register block (0xFF15, 0xFF1F,
            // 0xFF27-0xFF2F): writes are ignored.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync(audio: &mut Audio, abs: u64) {
        audio.sync_cc(abs, 0, 0, false, true, false);
    }

    /// Sync a DMG APU (`cgb = false`), so channel 4 takes the DMG-only
    /// deferred-trigger fork.
    fn dmg_sync(audio: &mut Audio, abs: u64) {
        audio.sync_cc(abs, 0, 0, false, false, false);
    }

    /// A powered-on DMG APU with channel 4 armed (volume 15, envelope period
    /// `period`, fastest LFSR) but not yet triggered, plus the abs CPU cc the
    /// clock is parked at.
    ///
    /// `phase` shifts the boot anchor by 0-3 APU cc. The noise `alignment`
    /// counts 2 MHz cycles since the NR52 0->1 write, and the DMG deferral only
    /// engages when `alignment & 3 != 0`, so callers sweep `phase` to land the
    /// ripple phase they need at their trigger cc.
    fn dmg_noise_apu(phase: u64, period: u8) -> (Audio, u64) {
        let mut audio = Audio::new();
        audio.set_boot_cgb(false);
        dmg_sync(&mut audio, 0);
        let abs = 0x400 + phase * 2;
        dmg_sync(&mut audio, abs);
        audio.write(NR52, 0x80);
        audio.write(NR42, 0xF0 | period); // volume 15, decreasing
        audio.write(NR43, 0x00); // divisor 0 / shift 0: fastest LFSR
        (audio, abs)
    }

    /// Advance the APU clock to exactly `target_cc`: coarse syncs (the
    /// controller chunks them internally) until the last few cc, then one cc
    /// per sync so we land on the exact cc. The power-on `last_update`
    /// re-anchor can eat one cc, hence the creep tail.
    fn advance_to_cc(audio: &mut Audio, abs: &mut u64, target_cc: u32) {
        while audio.cc + 8 < target_cc {
            *abs += 2 * (target_cc - audio.cc - 8) as u64;
            dmg_sync(audio, *abs);
        }
        while audio.cc != target_cc {
            *abs += 2;
            dmg_sync(audio, *abs);
        }
    }

    /// The channel-4 envelope volume, read back through PCM34's high nibble.
    /// That nibble is `volume` only while the LFSR output bit is high, so take
    /// the maximum over a window; the fastest NR43 setting steps the LFSR every
    /// 4 cc, so a 0x100-cc window always catches a high bit. PCM34 is a
    /// CGB-only register on the bus, but `Audio::pcm34` is just live channel
    /// state (the CGB gate lives in the MMIO read path), so it reads fine here.
    fn probe_ch4_volume(audio: &mut Audio, abs: &mut u64, at_cc: u32) -> u8 {
        let mut v = 0;
        for i in 0..0x100 {
            advance_to_cc(audio, abs, at_cc + i);
            v = v.max(audio.pcm34() >> 4);
        }
        assert_ne!(v, 0, "PCM34 probe never saw a high LFSR bit at cc {at_cc:#x}");
        v
    }

    /// The cc of the next 64 Hz envelope frame. The frame runs on a DIV-APU
    /// falling edge (cc a multiple of 0x1000) whose post-bump `div_divider & 7`
    /// is 7; `div_divider` bumps once per falling edge.
    fn next_env_frame_cc(audio: &Audio) -> u32 {
        let mut k = 7u32.wrapping_sub(audio.div_divider as u32) & 7;
        if k == 0 {
            k = 8;
        }
        (((audio.cc >> 12) + 1) << 12) + (k - 1) * 0x1000
    }

    fn distinct(values: &[u8]) -> usize {
        let mut seen = [false; 256];
        let mut n = 0;
        for &v in values {
            if !seen[v as usize] {
                seen[v as usize] = true;
                n += 1;
            }
        }
        n
    }

    /// The APU master clock `cc` is kept mod 2^31 and wraps every ~17 emulated
    /// minutes. Channel state anchored on absolute cc (duty countdown anchor,
    /// wave fetch counter, noise ripple anchor, scheduled length expiries) must
    /// be rebased at that wrap; without the epoch fold the wrap strands every
    /// anchor ~2^31 in the future and all four channels freeze until an NR52
    /// power cycle. This drives `Audio` directly across the wrap (abs CPU cc
    /// 2^32 = cc 2^31 in single speed) and asserts each channel still moves.
    #[test]
    fn channels_survive_master_clock_epoch_wrap() {
        // abs CPU cc where the APU cc (abs >> 1, plus the small boot anchor)
        // crosses 2^31.
        const WRAP: u64 = 1 << 32;
        const SLICE: u64 = 1 << 17;

        let mut audio = Audio::new();
        audio.set_boot_cgb(true);
        sync(&mut audio, 0);
        sync(&mut audio, 100);

        audio.write(NR52, 0x80);
        audio.write(NR51, 0xFF);
        audio.write(NR50, 0x77);
        for (i, b) in (0..16u16).map(|i| (i, 0x01u8.wrapping_add(0x22u8.wrapping_mul(i as u8)))) {
            audio.write(WAV_START + i, b);
        }

        // Quiet-advance most of the epoch in one sync.
        sync(&mut audio, WRAP - (1 << 23));

        // Arm all four channels. Periods are chosen to not divide the 2^16-cc
        // sample slice, so a live channel shows a different phase each slice
        // (freq 0x000/period 4096 would be stroboscopic at this slice size).
        audio.write(NR12, 0xF0);
        audio.write(NR13, 0x00);
        audio.write(NR14, 0x83);
        audio.write(NR22, 0xF0);
        audio.write(NR23, 0x00);
        audio.write(NR24, 0x83);
        audio.write(NR30, 0x80);
        audio.write(NR32, 0x20);
        audio.write(NR33, 0x00);
        audio.write(NR34, 0x81);
        audio.write(NR42, 0xF0);
        audio.write(NR43, 0x77);
        audio.write(NR44, 0x80);

        let mut pre12 = Vec::new();
        let mut pre3 = Vec::new();
        let mut pre4 = Vec::new();
        let mut post12 = Vec::new();
        let mut post3 = Vec::new();
        let mut post4 = Vec::new();
        let mut record = |audio: &mut Audio, abs: u64| {
            sync(audio, abs);
            let (p12, p34) = (audio.pcm12(), audio.pcm34());
            // Keep a wrap-point margin: the APU cc crosses 2^31 slightly
            // before abs == WRAP (the boot anchor offsets it by a few kcc).
            if abs <= WRAP - (1 << 18) {
                pre12.push(p12);
                pre3.push(p34 & 0x0F);
                pre4.push(p34 >> 4);
            } else if abs >= WRAP + (1 << 18) {
                post12.push(p12);
                post3.push(p34 & 0x0F);
                post4.push(p34 >> 4);
            }
        };

        let mut abs = WRAP - (1 << 21);
        while abs < WRAP - 0x8_0000 {
            record(&mut audio, abs);
            abs += SLICE;
        }

        // Length expiry scheduled across the epoch boundary: the target
        // `((len_cc>>13)+64)<<13` lands >= 2^31 and can never fire once the
        // clock wraps back below it.
        sync(&mut audio, WRAP - 0x8_0000);
        audio.write(NR21, 0x00);
        audio.write(NR24, 0xC0);

        while abs <= WRAP + (1 << 23) {
            record(&mut audio, abs);
            abs += SLICE;
        }

        // More than one emulated second past the wrap, then check NR52.
        sync(&mut audio, WRAP + (1 << 23) + (1 << 22));
        let nr52 = audio.read(NR52);

        // Pre-wrap canary: the harness itself must observe live channels.
        assert!(distinct(&pre12) >= 2, "pre-wrap canary: pcm12 static ({pre12:02x?})");
        assert!(distinct(&pre3) >= 2, "pre-wrap canary: wave nibble static ({pre3:02x?})");
        assert!(distinct(&pre4) >= 2, "pre-wrap canary: noise nibble static ({pre4:02x?})");
        // Post-wrap: every channel must still be moving. Accumulate so one
        // failure report names every frozen unit.
        let mut failures = Vec::new();
        if distinct(&post12) < 2 {
            failures.push(format!("square duty frozen (pcm12 all {:#04x})", post12[0]));
        }
        if distinct(&post3) < 2 {
            failures.push(format!("wave position frozen (pcm34 low all {:#x})", post3[0]));
        }
        if distinct(&post4) < 2 {
            failures.push(format!("noise LFSR frozen (pcm34 high all {:#x})", post4[0]));
        }
        if nr52 & 0x02 != 0 {
            failures.push(format!(
                "ch2 length expiry scheduled across the epoch boundary never fired (NR52={nr52:#04x})"
            ));
        }
        assert!(failures.is_empty(), "after the 2^31 epoch wrap: {}", failures.join("; "));
    }

    /// A DMG NR44 trigger landing on an unaligned ripple phase is deferred 6 cc
    /// (`dmg_delayed_start`), and the crossing re-applies it as the real start.
    /// A SECOND trigger arriving while that deferral is still in flight must
    /// RESTART the one delayed-start pipeline, not run a start of its own: the
    /// deferral bailout used to require `dmg_delayed_start == 0`, so the
    /// retrigger fell through to a full immediate start (LFSR reseed,
    /// `prepare_noise_start`, envelope reload) and the still-armed crossing then
    /// fired a SECOND complete start a couple of cc later, at a different
    /// `alignment & 3`. Hardware has one pipeline; a retrigger inside it
    /// restarts that pipeline.
    ///
    /// The deferral is a sub-10-cc phenomenon, so this drives `Audio` directly.
    /// Observables (both DMG-legal as live channel state): NR52 bit 3 for the
    /// cc the channel actually starts on, and PCM34's high nibble for the LFSR
    /// output. Two starts cannot be faked by one, because they reseed the LFSR
    /// and the ripple countdown twice at different cc — so the whole
    /// post-trigger trace is compared against a reference run that performs a
    /// SINGLE deferred trigger at the same cc.
    #[test]
    fn dmg_noise_retrigger_inside_deferral_starts_once() {
        // APU cc of the first trigger, and cc sampled after the second.
        const T1: u32 = 0x4000;
        const WINDOW: u32 = 256;

        // Pick a power-on phase whose `alignment & 3` at T1 is unaligned, i.e.
        // one the DMG deferral engages on. A deferred trigger leaves NR52 bit 3
        // clear at the write cc; an immediate start sets it.
        let phase = (0..4u64)
            .find(|&phase| {
                let (mut audio, mut abs) = dmg_noise_apu(phase, 0);
                advance_to_cc(&mut audio, &mut abs, T1);
                audio.write(NR44, 0x80);
                audio.read(NR52) & 0x08 == 0
            })
            .expect("no power-on phase puts the ch4 trigger on an unaligned ripple phase");

        // T1+4 is 4 cc into the 6-cc deferral. `4 % 4 == 0` keeps
        // `alignment & 3` unaligned there too, so the reference run's lone
        // trigger defers exactly like the retrigger restarts.
        let trace = |trigger_at_t1: bool| -> Vec<(bool, u8)> {
            let (mut audio, mut abs) = dmg_noise_apu(phase, 0);
            advance_to_cc(&mut audio, &mut abs, T1);
            if trigger_at_t1 {
                audio.write(NR44, 0x80);
            }
            advance_to_cc(&mut audio, &mut abs, T1 + 4);
            audio.write(NR44, 0x80);
            (0..WINDOW)
                .map(|i| {
                    advance_to_cc(&mut audio, &mut abs, T1 + 4 + i);
                    (audio.read(NR52) & 0x08 != 0, audio.pcm34() >> 4)
                })
                .collect()
        };
        let retriggered = trace(true);
        let single = trace(false);

        // One start, at the RESTARTED crossing: 6 cc after the SECOND write.
        // Offset 0 is the bug's immediate start.
        assert_eq!(
            retriggered.iter().position(|&(on, _)| on),
            Some(6),
            "channel 4 started at the wrong offset from the retrigger that \
             landed inside the DMG deferral (0 = the buggy immediate start, \
             6 = the restarted delayed-start crossing)"
        );
        // ...and it is ONE start, not two 2 cc apart.
        assert_eq!(
            retriggered, single,
            "retriggering inside the DMG noise deferral did not collapse to a \
             single start: the (NR52 ch4 bit, PCM34 high nibble) trace diverges \
             from a single deferred trigger at the same cc"
        );
    }

    /// A deferred DMG noise trigger re-applies from inside `advance`, where
    /// `self.cc` is the CHUNK END — up to a whole DIV-APU grid cell past the
    /// actual +6 crossing. `trigger()` anchoring `env_trigger_cc` there inflates
    /// the envelope's 2-cc frame-escape window (`envelope.rs`
    /// `env_frame_countdown`), so a trigger that should sit inside the 64 Hz
    /// frame escapes its decrement and the whole envelope steps one frame late.
    ///
    /// Both legs write NR44 just below a 64 Hz frame boundary and then take ONE
    /// sync across it; the controller's chunker stops one cc before the
    /// boundary, so the chunk end is always `event_cc - 1` while the crossing
    /// lands where the write placed it. The legs pin the boundary from both
    /// sides: a crossing 3 cc before the frame must NOT escape, one 2 cc before
    /// must. On the unfixed anchor both read `event_cc - 1` and both escape.
    ///
    /// Read out through the envelope: NR42 period 1 steps the volume 15 -> 14
    /// one frame after the trigger, so an escaped frame leaves 15 where 14 is
    /// due, sampled well clear of the next frame boundary.
    #[test]
    fn dmg_deferred_noise_trigger_anchors_env_race_at_the_crossing() {
        // `lead` = cc from the NR44 write to the frame boundary; the deferral
        // crossing lands 6 cc after the write, i.e. at `event_cc - (lead - 6)`.
        let run = |phase: u64, lead: u32| -> Option<u8> {
            let (mut audio, mut abs) = dmg_noise_apu(phase, 1);
            // Clear of the power-on DIV-APU skip glitch before reading
            // `div_divider` to locate the frame.
            advance_to_cc(&mut audio, &mut abs, 0x8000);
            let event_cc = next_env_frame_cc(&audio);
            advance_to_cc(&mut audio, &mut abs, event_cc - lead);
            audio.write(NR44, 0x80);
            if audio.read(NR52) & 0x08 != 0 {
                return None; // aligned ripple phase: no deferral at this phase
            }
            // One sync across the crossing AND the frame boundary. The chunker
            // caps the first chunk at `event_cc - 1`, putting the crossing
            // strictly inside it — the gap this test is about.
            abs += 2 * 32;
            dmg_sync(&mut audio, abs);
            assert!(audio.cc > event_cc, "sync did not clear the frame boundary");
            assert!(
                audio.read(NR52) & 0x08 != 0,
                "the deferred trigger never started the channel"
            );
            Some(probe_ch4_volume(&mut audio, &mut abs, event_cc + 0x4000))
        };
        let sweep = |lead: u32| {
            (0..4u64).find_map(|phase| run(phase, lead)).unwrap_or_else(|| {
                panic!("no power-on phase defers a trigger {lead} cc before the frame")
            })
        };

        let crossing_3_cc_before = sweep(9);
        let crossing_2_cc_before = sweep(8);

        assert_eq!(
            crossing_3_cc_before, 14,
            "a deferred DMG trigger whose crossing lands 3 cc before the 64 Hz \
             frame must NOT escape that frame's envelope decrement -- got the \
             escaped (one frame late) volume, so `env_trigger_cc` was anchored \
             at the chunk end instead of the crossing"
        );
        assert_eq!(
            crossing_2_cc_before, 15,
            "a deferred trigger whose crossing lands 2 cc before the frame is \
             inside the escape window and must still skip the decrement"
        );
    }

    /// A powered-on CGB APU with everything routed to both sides at full master
    /// volume, parked at the returned abs CPU cc.
    fn powered_apu() -> (Audio, u64) {
        let mut audio = Audio::new();
        audio.set_boot_cgb(true);
        sync(&mut audio, 0);
        sync(&mut audio, 0x400);
        audio.write(NR52, 0x80);
        audio.write(NR51, 0xFF);
        audio.write(NR50, 0x77);
        (audio, 0x400)
    }

    /// The DAC's polarity, observed through the real channel path rather than
    /// through `dac_analog` alone. Pan Docs: "the digital range $0 to $F is
    /// linearly translated to the analog range -1 to 1 … the slope is negative:
    /// 'digital 0' maps to 'analog 1'". A 50 %-duty square at volume 15 spends
    /// half its period at digital 15 and half at digital 0, so its pre-mix
    /// output must visit exactly the two rails — and never 0.0, which under the
    /// old unipolar convention was where digital 0 sat.
    #[test]
    fn dac_maps_digital_zero_to_plus_one_and_digital_fifteen_to_minus_one() {
        let (mut audio, mut abs) = powered_apu();
        audio.write(NR21, 0x80); // duty 2 (50%)
        audio.write(NR22, 0xF0); // volume 15, no envelope: DAC on
        audio.write(NR23, 0x00);
        audio.write(NR24, 0x83); // trigger

        let mut saw_zero = false;
        let mut saw_fifteen = false;
        for _ in 0..4000 {
            abs += 8;
            sync(&mut audio, abs);
            let digital = audio.channel2.pcm_nibble();
            let analog = audio.channel_outputs()[1];
            match digital {
                0 => {
                    assert_eq!(analog, 1.0, "digital 0 must be analog +1");
                    saw_zero = true;
                }
                15 => {
                    assert_eq!(analog, -1.0, "digital 15 must be analog -1");
                    saw_fifteen = true;
                }
                d => panic!("a 50% duty at volume 15 has no digital {d}"),
            }
        }
        assert!(saw_zero && saw_fifteen, "the square never swung between rails");
    }

    /// An unpowered DAC contributes analog 0 to the tap — the endpoint the fade
    /// coasts to. Pan Docs' recommended pop-free silencing (write $08 to NRx2)
    /// is the discriminating case: it zeroes the digital output while KEEPING
    /// the DAC powered, so the channel must sit at analog +1, not at silence.
    #[test]
    fn a_silenced_channel_holds_analog_one_while_only_a_dead_dac_reads_zero() {
        let (mut audio, mut abs) = powered_apu();
        audio.write(NR21, 0x80);
        audio.write(NR22, 0xF0);
        audio.write(NR23, 0x00);
        audio.write(NR24, 0x83);
        abs += 64;
        sync(&mut audio, abs);

        // $08: volume 0, envelope increasing -> digital 0 with the DAC still on.
        audio.write(NR22, 0x08);
        audio.write(NR24, 0x83);
        abs += 64;
        sync(&mut audio, abs);
        assert!(audio.channel2.dac_on(), "$08 must keep the DAC powered");
        assert_eq!(audio.channel2.pcm_nibble(), 0);
        assert_eq!(
            audio.channel_outputs()[1],
            1.0,
            "a silenced-but-powered channel sits at analog +1, not at 0"
        );

        // $00: the DAC goes down, and with it the channel's contribution.
        audio.write(NR22, 0x00);
        abs += 64;
        sync(&mut audio, abs);
        assert!(!audio.channel2.dac_on(), "$00 must unpower the DAC");
        assert_eq!(
            audio.channel_outputs()[1],
            0.0,
            "an unpowered DAC contributes analog 0"
        );
    }

    /// CH3 emits its LATCHED sample buffer, not a live wave-RAM read. Pan Docs:
    /// "CH3 does not emit samples directly, but stores every sample read into a
    /// buffer, and emits that continuously; (re)triggering the channel does not
    /// clear nor refresh this buffer". The buffer is cleared when the APU is
    /// powered on, so a channel triggered over an all-$FF wave RAM must still
    /// emit digital 0 until its first fetch lands — a live read would give
    /// digital 15 immediately. The audible path and PCM34 must agree throughout.
    #[test]
    fn wave_emits_the_latched_sample_buffer_not_a_live_wave_ram_read() {
        let (mut audio, mut abs) = powered_apu();
        for i in 0..16u16 {
            audio.write(WAV_START + i, 0xFF);
        }
        audio.write(NR30, 0x80); // DAC on
        audio.write(NR32, 0x20); // output level 1: no shift
        audio.write(NR33, 0x00);
        audio.write(NR34, 0x87); // trigger, slow period so the first fetch is far off

        assert!(audio.channel3.dac_on());
        assert_eq!(
            audio.channel3.pcm_nibble(),
            0,
            "the power-on-cleared buffer still reads 0 right after the trigger"
        );
        assert_eq!(
            audio.channel_outputs()[2],
            1.0,
            "a live wave-RAM read would have emitted digital 15 (analog -1) here"
        );

        // Run until the first fetch replaces the buffer; the audible path must
        // track PCM34 exactly, sample for sample, the whole way.
        let mut reached_fifteen = false;
        for _ in 0..4000 {
            abs += 8;
            sync(&mut audio, abs);
            let digital = audio.channel3.pcm_nibble();
            assert_eq!(
                audio.channel_outputs()[2],
                super::analog::dac_analog(digital),
                "the audible path diverged from PCM34 at digital {digital}"
            );
            reached_fifteen |= digital == 15;
        }
        assert!(
            reached_fifteen,
            "the fetch never latched the $FF wave RAM into the buffer"
        );
    }

    /// The high-pass and the DAC-off fade are the analog stage's continuous
    /// state, and both are deliberately absent from the tap: the `.rba`
    /// per-plane encoder builds a `u16` palette of DISTINCT values, so a
    /// continuous ramp there would blow past 65,535 uniques. Whatever the APU
    /// is doing, a tap sample may only ever carry one of the 16 DAC levels or
    /// the unpowered-DAC 0.0.
    #[test]
    fn tapped_channel_values_stay_a_small_discrete_alphabet() {
        let (mut audio, mut abs) = powered_apu();
        audio.set_channel_tap(true);
        for i in 0..16u16 {
            audio.write(WAV_START + i, 0x1Fu8.wrapping_mul(i as u8 + 1));
        }
        // All four channels live, then torn down one DAC at a time so the fade
        // is running underneath while the tap is sampled.
        audio.write(NR12, 0xF3);
        audio.write(NR13, 0x00);
        audio.write(NR14, 0x83);
        audio.write(NR22, 0xA2);
        audio.write(NR23, 0x40);
        audio.write(NR24, 0x84);
        audio.write(NR30, 0x80);
        audio.write(NR32, 0x40);
        audio.write(NR33, 0x00);
        audio.write(NR34, 0x82);
        audio.write(NR42, 0xF2);
        audio.write(NR43, 0x37);
        audio.write(NR44, 0x80);

        let mut allowed: Vec<f32> = (0..=15).map(super::analog::dac_analog).collect();
        allowed.push(0.0);

        for step in 0..600 {
            abs += 128;
            sync(&mut audio, abs);
            audio.generate_samples(64);
            match step {
                200 => audio.write(NR12, 0x00),
                300 => audio.write(NR30, 0x00),
                400 => audio.write(NR42, 0x00),
                500 => audio.write(NR22, 0x00),
                _ => {}
            }
        }

        let tap = audio.drain_channel_tap();
        // 600 * 64 cycles / (4194304/44100) cycles per sample.
        assert!(tap.len() > 350, "tap collected only {} samples", tap.len());
        let mut distinct: Vec<f32> = Vec::new();
        for (chs, ..) in &tap {
            for &v in chs {
                assert!(
                    allowed.contains(&v),
                    "tapped {v} is not a DAC level -- the fade or the high-pass \
                     leaked upstream of the tap"
                );
                if !distinct.contains(&v) {
                    distinct.push(v);
                }
            }
        }
        assert!(
            distinct.len() <= 17,
            "tap alphabet grew to {} values",
            distinct.len()
        );
    }

    /// An APU whose mixed output is a pure DC bias of +0.25 per side.
    ///
    /// Channel 1's DAC is powered (NR12 volume 15) but the channel is never
    /// triggered, so it feeds its live DAC a digital 0 and parks at analog +1
    /// (Pan Docs, Audio details: a deactivated channel with a live DAC sits at
    /// the positive rail, not at silence). NR51 routes it to both sides and
    /// NR50 is at full master volume, so `mix_stereo` emits a constant
    /// `1.0 * 8/8 / 4` = +0.25 — exactly the offset the output high-pass exists
    /// to remove.
    ///
    /// The analog model is set directly rather than via `Hardware`, to isolate
    /// the filter from boot behaviour; `analog::tests::
    /// every_hardware_model_maps_to_its_analog_stage` pins the other half of
    /// that chain.
    fn dc_biased_apu(model: analog::AnalogModel) -> (Audio, u64) {
        let mut audio = Audio::new();
        audio.set_boot_cgb(true);
        audio.set_analog_model(model);
        sync(&mut audio, 0);
        sync(&mut audio, 100);
        audio.write(NR52, 0x80);
        audio.write(NR51, 0xFF);
        audio.write(NR50, 0x77);
        audio.write(NR12, 0xF0);
        (audio, 100)
    }

    /// Exactly `n` host samples (left side) pulled through the real output
    /// path, advancing the APU clock in step with the sample generator.
    fn emit_samples(audio: &mut Audio, abs: &mut u64, n: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            *abs += 128;
            sync(audio, *abs);
            out.extend(audio.generate_samples(128).into_iter().map(|(l, _)| l));
        }
        out.truncate(n);
        out
    }

    /// The output high-pass must be wired INTO `analog_sample`, with the right
    /// model's charge factor.
    ///
    /// `AnalogStage::high_pass` has its own unit test, but that test exercises
    /// the function in isolation and passes with the filter unplugged from the
    /// sample path. The only test that caught the disconnection was the
    /// ROM-gated Pokémon drumroll regression, which skips silently wherever the
    /// ROM is absent — so on most machines the entire filter could be removed
    /// from the output path with the suite still green. This drives the real
    /// path instead, with a DC bias the filter is obliged to remove.
    ///
    /// The bias decays as `charge^n`, and the two published factors are far
    /// enough apart to identify WHICH one is applied: 0.996 per sample on DMG
    /// against 0.9043 on CGB (blargg's per-cycle values raised to
    /// 4194304/44100). The mean of `0.25 * charge^n` over the first 400 samples
    /// is therefore 0.125 on DMG but 0.0065 on CGB, a 19x gap — so swapping the
    /// two factors fails this test rather than merely re-tuning it. DMG's own
    /// convergence is then asserted over a longer window, so "DMG is slow"
    /// cannot be satisfied by the filter not running at all.
    #[test]
    fn output_high_pass_is_applied_in_the_sample_path() {
        let (mut cgb, mut cgb_abs) = dc_biased_apu(analog::AnalogModel::CgbMgb);
        let cgb_out = emit_samples(&mut cgb, &mut cgb_abs, 400);
        let (mut dmg, mut dmg_abs) = dc_biased_apu(analog::AnalogModel::Dmg);
        let dmg_out = emit_samples(&mut dmg, &mut dmg_abs, 4000);

        // The bias really is the +0.25 the rest of the test reasons about: the
        // first sample is taken before the capacitor has charged at all.
        assert!((cgb_out[0] - 0.25).abs() < 1e-6, "DC bias was {}", cgb_out[0]);
        assert!((dmg_out[0] - 0.25).abs() < 1e-6, "DC bias was {}", dmg_out[0]);

        let mean = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
        let cgb_mean = mean(&cgb_out);
        let dmg_mean = mean(&dmg_out[..400]);

        // Measured 0.0065. An unplugged high-pass parks this at the full 0.25;
        // the DMG factor applied here leaves 0.125.
        assert!(
            cgb_mean < 0.02,
            "the CGB high-pass did not remove the DC bias (mean over 400 = \
             {cgb_mean}) -- either it is not in the output path, or the DMG \
             factor is being applied to a CGB"
        );
        // Measured 0.125: DMG must NOT have converged on CGB's timescale.
        assert!(
            dmg_mean > 0.08,
            "the DMG high-pass converged like a CGB (mean over 400 = \
             {dmg_mean}) -- the two charge factors look swapped"
        );
        // Measured 1.9e-6: but it must still converge on its own timescale.
        assert!(
            dmg_out[3999].abs() < 1e-4,
            "the DMG high-pass never removed the bias ({}) -- a 'slow' filter \
             must still be a filter",
            dmg_out[3999]
        );
    }

    /// The DAC-off fade must be wired INTO `analog_sample`.
    ///
    /// Same failure mode as the high-pass above, and worse: with
    /// `AnalogStage::fade` unplugged at its call site the entire core suite
    /// stays green, the ROM-gated tests included, so nothing at all noticed.
    ///
    /// The observable is continuity. A DAC that loses power coasts from
    /// wherever its node was left toward 0 (Pan Docs: it "fades to an analog
    /// value of 0"); it does not step there. With the fade in the path the
    /// mixer input creeps down by `1 - charge` per sample and the emitted
    /// stream moves at most ~0.0014 per sample. Without it the input drops the
    /// whole 0.25 bias in one sample, and the high-pass — which passes exactly
    /// that kind of fast transient — hands it to the speaker as a 0.25
    /// discontinuity, 179x larger. DMG is used because its gentle filter leaves
    /// a real pre-off level to fade down FROM.
    #[test]
    fn dac_off_fade_is_applied_in_the_sample_path() {
        let (mut audio, mut abs) = dc_biased_apu(analog::AnalogModel::Dmg);
        let pre = emit_samples(&mut audio, &mut abs, 400);
        let pre_off = pre[399];
        assert!(pre_off > 0.02, "nothing left to fade from ({pre_off})");

        audio.write(NR12, 0x00); // unpower channel 1's DAC mid-tone
        let post = emit_samples(&mut audio, &mut abs, 4000);

        let mut prev = pre_off;
        let mut max_step = 0.0f32;
        for &s in &post {
            max_step = max_step.max((s - prev).abs());
            prev = s;
        }
        // Measured 0.0014 with the fade, 0.2504 without it.
        assert!(
            max_step < 0.01,
            "the output stepped by {max_step} at DAC-off -- the fade is not in \
             the output path (a bypassed fade steps by the full 0.25 bias)"
        );
        assert!(
            post[3999].abs() < 1e-4,
            "the unpowered DAC never coasted to 0 ({})",
            post[3999]
        );
    }
}

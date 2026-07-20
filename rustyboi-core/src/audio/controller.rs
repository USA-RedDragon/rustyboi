use serde::{Deserialize, Serialize};
use crate::audio::{wave, square, noise};
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

    // Frame sequencer
    frame_sequencer_step: u8,
    frame_sequencer_timer: u16,

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
}

/// One tapped sample: pre-mix channel outputs [ch1..ch4] + the mix registers
/// (nr50, nr51) + the master enable — everything `get_mixed_output` consumes,
/// so the stereo mix is exactly reconstructible from the tap alone.
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
            frame_sequencer_step: 0,
            frame_sequencer_timer: 8192,
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
        }
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
            self.step_channels(cgb, agb, ds);
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
            self.step_channels(cgb, agb, ds);
        }
        any
    }

    /// The retired per-dot `Audio::step` body: channel catch-up + cc-event
    /// polls (ch1 sweep triple), run at every catch-up chunk end.
    fn step_channels(&mut self, cgb: bool, agb: bool, ds: bool) {
        if !self.audio_enabled {
            return;
        }
        self.channel1.step(cgb);
        self.channel2.step(cgb);
        self.channel3.step(cgb, agb, ds);
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

    /// Clock the frame sequencer one step. Called by the timer at the exact dot
    /// of each DIV-bit-12 (bit-13 in double speed) falling edge, so the sequencer
    /// stays phase-locked to DIV (and reacts to DIV writes).
    pub(crate) fn clock_frame_sequencer(&mut self) {
        if self.audio_enabled {
            // Length/envelope/sweep are cc-event driven; the step counter and the
            // channels' fs_step mirrors are serialized savestate fields, so they
            // keep ticking to preserve the wire format.
            let step = self.frame_sequencer_step;
            self.channel1.set_fs_step(step);
            self.channel2.set_fs_step(step);
            self.channel3.set_fs_step(step);
            self.channel4.set_fs_step(step);
            self.frame_sequencer_step = (self.frame_sequencer_step + 1) % 8;
        }
    }

    /// True while the APU is powered (NR52 bit 7). The min-event idle fast path
    /// only bulk-skips dots when audio is OFF, because a powered APU steps its
    /// channel duty/freq counters per dot (`step`), which is not
    /// span-collapsible like the frame sequencer.
    pub(crate) fn is_powered(&self) -> bool {
        self.audio_enabled
    }

    pub(crate) fn get_master_volume_left(&self) -> u8 {
        (self.nr50 >> 4) & 0x07
    }

    pub(crate) fn get_master_volume_right(&self) -> u8 {
        self.nr50 & 0x07
    }

    pub(crate) fn is_channel_left_enabled(&self, channel: u8) -> bool {
        match channel {
            1 => (self.nr51 >> 4) & 0x01 != 0,
            2 => (self.nr51 >> 5) & 0x01 != 0,
            3 => (self.nr51 >> 6) & 0x01 != 0,
            4 => (self.nr51 >> 7) & 0x01 != 0,
            _ => false,
        }
    }

    pub(crate) fn is_channel_right_enabled(&self, channel: u8) -> bool {
        match channel {
            1 => self.nr51 & 0x01 != 0,
            2 => (self.nr51 >> 1) & 0x01 != 0,
            3 => (self.nr51 >> 2) & 0x01 != 0,
            4 => (self.nr51 >> 3) & 0x01 != 0,
            _ => false,
        }
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

    /// The four GB channels mixed to stereo. On an SGB this is still the whole
    /// output: the SGB's own effects come from the SNES APU, which is decoded
    /// but not synthesised ([`crate::sgb::SgbSound`]); adding them later means
    /// summing into this stream here or at a downstream sink, with no change to
    /// the channels below.
    pub(crate) fn get_mixed_output(&self) -> (f32, f32) {
        if !self.audio_enabled {
            return (0.0, 0.0);
        }

        let ch1_output = self.channel1.get_output();
        let ch2_output = self.channel2.get_output();
        let ch3_output = self.channel3.get_output();
        let ch4_output = self.channel4.get_output();

        let mut left_mix = 0.0;
        let mut right_mix = 0.0;

        if self.is_channel_left_enabled(1) {
            left_mix += ch1_output;
        }
        if self.is_channel_left_enabled(2) {
            left_mix += ch2_output;
        }
        if self.is_channel_left_enabled(3) {
            left_mix += ch3_output;
        }
        if self.is_channel_left_enabled(4) {
            left_mix += ch4_output;
        }

        if self.is_channel_right_enabled(1) {
            right_mix += ch1_output;
        }
        if self.is_channel_right_enabled(2) {
            right_mix += ch2_output;
        }
        if self.is_channel_right_enabled(3) {
            right_mix += ch3_output;
        }
        if self.is_channel_right_enabled(4) {
            right_mix += ch4_output;
        }

        // Apply master volume
        left_mix *= (self.get_master_volume_left() as f32 + 1.0) / 8.0;
        right_mix *= (self.get_master_volume_right() as f32 + 1.0) / 8.0;

        // Divide by 4 to normalize since we're summing 4 channels
        (left_mix / 4.0, right_mix / 4.0)
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
            samples.push(self.get_mixed_output());
            if let Some(tap) = &mut self.channel_tap {
                tap.push((
                    [
                        self.channel1.get_output(),
                        self.channel2.get_output(),
                        self.channel3.get_output(),
                        self.channel4.get_output(),
                    ],
                    self.nr50,
                    self.nr51,
                    self.audio_enabled,
                ));
            }
            self.fractional_cycles -= cycles_per_sample;
        }

        samples
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
                    // already set them to LEN_DISABLED, and none of these
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
}

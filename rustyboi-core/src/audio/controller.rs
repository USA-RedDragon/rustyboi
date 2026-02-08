use serde::{Deserialize, Serialize};
use crate::audio::{wave, square, noise};
use crate::memory::mmio;
use crate::memory::Addressable;

pub const NR10: u16 = 0xFF10; // Channel 1 sweep register
pub const NR11: u16 = 0xFF11; // Channel 1 sound length/wave pattern duty
pub const NR12: u16 = 0xFF12; // Channel 1 volume and envelope
pub(crate) const NR13: u16 = 0xFF13; // Channel 1 period low
pub const NR14: u16 = 0xFF14; // Channel 1 period high and control

pub const NR21: u16 = 0xFF16; // Channel 2 sound length/wave pattern duty
pub const NR22: u16 = 0xFF17; // Channel 2 volume and envelope
pub(crate) const NR23: u16 = 0xFF18; // Channel 2 period low
pub const NR24: u16 = 0xFF19; // Channel 2 period high and control

pub const NR30: u16 = 0xFF1A; // Channel 3 dac enable
pub const NR31: u16 = 0xFF1B; // Channel 3 sound length
pub const NR32: u16 = 0xFF1C; // Channel 3 output level
pub const NR33: u16 = 0xFF1D; // Channel 3 period low
pub const NR34: u16 = 0xFF1E; // Channel 3 period high and control

pub const NR41: u16 = 0xFF20; // Channel 4 sound length
pub const NR42: u16 = 0xFF21; // Channel 4 volume and envelope
pub const NR43: u16 = 0xFF22; // Channel 4 frequency and randomness
pub const NR44: u16 = 0xFF23; // Channel 4 control

pub const NR50: u16 = 0xFF24; // master volume, VIN panning
pub const NR51: u16 = 0xFF25; // Sound panning
pub const NR52: u16 = 0xFF26; // Audio master control

pub const WAV_START: u16 = 0xFF30; // Channel 3 wave pattern RAM start
pub const WAV_LENGTH: usize = 16; // Channel 3 wave pattern RAM length
pub const WAV_END: u16 = WAV_START + WAV_LENGTH as u16 - 1; // Channel 3 wave pattern RAM end

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

    // APU master clock mirroring Gambatte's `PSG::cycleCounter_` — an absolute
    // 2 MHz counter (mod 0x8000_0000) anchored at boot. Driven from the timer's
    // absolute `abs_cc` (Gambatte `cpuCc`): each `sync_cc` advances by
    // `(abs_cc - last_update) >> (1 + ds)`, exactly like `PSG::generateSamples`.
    // Carries the full phase a DIV reset would otherwise drop, which the
    // cc-driven length counter needs across the power-on fold.
    #[serde(default)]
    cc: u32,
    // Length-subsystem clock, mirroring Gambatte's `cycleCounter_` at the TRUE
    // `generateSamples` rate `(cpuCc - lastUpdate) >> (1 + ds)`. The duty/envelope
    // `cc` above advances at `>>1` in both speeds (its tuning is anchored there
    // via the half-rate `step_audio` gating); but the length-expiry boundary
    // `((cc>>13)+len)<<13` must advance at HALF that rate at double speed to land
    // on Gambatte's boundary (the NR52 ch2 a/b straddle). This parallel clock
    // carries the Gambatte length rate, folded identically to `cc` on DIV-reset /
    // PSG::reset / speedChange.
    #[serde(default)]
    len_cc: u32,
    // Absolute CPU cc (Gambatte `lastUpdate_`) at the last clock advance; its
    // bit-0 parity matters for the duty/divReset/speedchange folds.
    #[serde(default)]
    last_update: u64,
    // Last-seen timer DIV-write count; a change triggers the `PSG::divReset` fold.
    #[serde(default)]
    last_div_resets: u64,
    #[serde(default)]
    clock_anchored: bool,
    // Double-speed flag from the last `sync_cc`, so the NR52-enable `PSG::reset`
    // fold (which happens on the write path, without `ds`) uses the right speed.
    #[serde(default)]
    cached_ds: bool,
    // CGB vs DMG flag for the post-BIOS SPU `cycleCounter_` boot anchor (the
    // high-bit constant differs: 0x1E00 CGB / 0x2400 DMG, initstate.cpp).
    #[serde(default)]
    boot_cgb: bool,
    // SameBoy `lf_div` (Core/apu.c GB_apu_run: `lf_div ^= cycles & 1`): the
    // free-running 1 MHz sub-phase of the 2 MHz APU clock. On hardware this
    // parity NEVER folds — a DIV reset only steps the frame-sequencer phase
    // (GB_apu_div_event) and a speed switch doesn't touch it — so it toggles
    // ONLY in `advance_to` (the one place real 2 MHz cycles elapse) and is
    // deliberately NOT part of the `div_reset_fold`/`speedChange` cc
    // re-anchoring. Reset to 1 on APU power-on (SameBoy GB_apu_init) and
    // seeded at the boot anchor to the post-BIOS hardware phase.
    #[serde(default = "default_ctl_lf_div")]
    lf_div: u32,
    // Per-access read cc (channel 2 MHz units) for the current APU register
    // read, set by `set_read_len_cc`; a PCM12 read resolves the square duty at
    // this canonical access cc (M7). Not serialized (transient per-access).
    #[serde(skip)]
    pcm_read_cc: Option<u32>,
    // SameBoy `div_divider`: counts DIV-APU events (the 0x1000-cc boundaries
    // of the master clock = DIV bit 12/13 falling edges, including the forced
    // edge of a DIV write). The envelope frame runs at (div_divider & 7) == 7;
    // its parity mirrors (cc >> 12) except for the power-on skip glitch below.
    #[serde(default)]
    div_divider: u16,
    // SameBoy `skip_div_event` (GB_apu_init): powering the APU on while the
    // DIV-APU bit is high skips the first (truncated) DIV-APU event entirely,
    // with div_divider pre-set to 1. 0=inactive, 1=skipped, 2=skip.
    #[serde(default)]
    skip_div_event: u8,
    // CGB-D/E APU revision gate (SameBoy `model > GB_MODEL_CGB_C`), set once
    // from Hardware::CGBE. false = the default cgb04c (CPU-CGB-C) model.
    #[serde(default)]
    cgb_de: bool,
}

fn default_ctl_lf_div() -> u32 {
    1
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
            cc: 0,
            last_update: 0,
            last_div_resets: 0,
            clock_anchored: false,
            cached_ds: false,
            boot_cgb: false,
            lf_div: 1,
            pcm_read_cc: None,
            div_divider: 0,
            skip_div_event: 0,
            cgb_de: false,
        }
    }

    /// SameBoy `GB_apu_div_event` (a DIV-APU falling edge, the master clock
    /// crossing a 0x1000-cc boundary): advance `div_divider` (unless the
    /// power-on skip glitch eats this event), run the 64 Hz envelope frame
    /// countdown, and consume any armed envelope ticks. Length and sweep stay
    /// on their Gambatte cc-event models.
    fn fs_div_event(&mut self) {
        let cc = self.cc;
        self.fs_div_event_at(cc);
    }

    /// The DIV-APU event with its exact boundary cc. `event_cc` feeds the
    /// envelope frame's trigger-race window: an NRx4 trigger 2 cc (one CPU
    /// write M-cycle) or less before the frame boundary shares the hardware
    /// M-cycle with the event, and its freshly-reloaded countdown escapes that
    /// frame's decrement (Gambatte `nr4Init`'s `(cc + 2) & 0x7000` period
    /// bump; pinned by the dmg08/cgb04c ch2_init_reset_env_counter brackets).
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

    /// SameBoy `GB_apu_div_secondary_event` (the rising edge, cc crossing a
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
    /// The hardware edge grid sits at cc ≡ -2 (mod 0x800): Gambatte's envelope
    /// unit keys the frame quantum on `cc + 2` (`nr4Init`'s
    /// `(cc + 2) & 0x7000` period bump), and the dmg08/cgb04c
    /// ch2_init_env_counter_timing 1-cc brackets pin exactly that -2 offset
    /// (a trigger 1-2 cc before a raw 0x1000 multiple must MISS that frame's
    /// countdown decrement). cc + 2 ≡ 0 (mod 0x1000) is the falling edge
    /// (DIV-APU event); cc + 2 ≡ 0x800 is the rising edge (secondary).
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

    /// Reconstruct Gambatte's free-running 2 MHz `cycleCounter_` from the
    /// timer's internal counter (`ic >> 1`, the low 15 bits) and push it to the
    /// square channels. Measured against the boot DIV phase, the frame-sequencer
    /// position satisfies `cc>>12&7 == ((our fs_step) + 5) & 7`.
    ///
    /// A DIV write resets the timer's internal counter, which drops the sub-step
    /// part of cc. We mirror Gambatte's `divReset`/`Channel::resetCc`: preserve
    /// the upper cc bits (the length `cc>>13` / FS-phase boundaries) and shift
    /// only the duty unit by the resulting delta.
    pub fn sync_cc(&mut self, abs_cc: u64, div_resets: u64, div_anchor: u64, ds: bool) {
        self.cached_ds = ds;
        if !self.clock_anchored {
            // Defer the boot anchor past the abs_cc==0 pre-boot sync (Gambatte
            // `loadState` sets `lastUpdate_ = cpuCc - 1` at the post-BIOS load,
            // with cpuCc already advanced). Anchoring at abs_cc==0 with `-1` would
            // underflow and freeze `advance_to`.
            if abs_cc == 0 {
                self.last_div_resets = div_resets;
                self.push_cc();
                return;
            }
            // Faithful post-BIOS SPU `cycleCounter_` (Gambatte initstate.cpp
            // setPostBiosState): `(cgb ? 0x1E00 : 0x2400) | (cpuCc>>1 & 0x1FF)`.
            // The high bits are a fixed per-mode constant; only the low 9 bits
            // come from `cpuCc>>1`. rustyboi's `abs_cc` boot origin is the DIV
            // boot counter, whose low 9 bits of `>>1` match Gambatte's
            // `cpuCc>>1 & 0x1FF` (verified CGB 0x150 / DMG 0x1E6), so we take the
            // mode constant for the high bits and `abs_cc>>1` for the low 9.
            let high = if self.boot_cgb { 0x1E00u32 } else { 0x2400u32 };
            self.cc = (high | ((abs_cc >> 1) as u32 & 0x1FF)) & (Self::CC_MAX - 1);
            self.len_cc = self.cc;
            // Faithful boot anchor (Gambatte loadState `lastUpdate_ = cpuCc - 1`):
            // the floored boundary sits one cc below the current cpu cc so the
            // first generateSamples picks up the right parity remainder.
            self.last_update = abs_cc - 1;
            self.last_div_resets = div_resets;
            self.clock_anchored = true;
            // Seed div_divider to the post-BIOS phase: the boot ROM powered
            // NR52 on (no psg_reset runs at the boot handoff), and the real
            // hardware's event count since that power-on leaves the envelope
            // frame (div_divider & 7 == 7) on the absolute cc grid — frames at
            // (cc+2) >> 12 ≡ 0 (mod 8), ticks at ≡ 1, exactly Gambatte's
            // `nr4Init` quantum. Seed `div_divider = m - 1` for the current
            // region m so the first crossing lands on that grid (CGB anchor
            // 0x1E00 -> 0; DMG anchor 0x2400 -> 1 — the dmg08
            // ch2_init_env_counter_timing brackets pin the DMG offset).
            self.div_divider = ((self.cc.wrapping_add(0) >> 12).wrapping_sub(1) & 7) as u16;
            self.skip_div_event = 0;
            // Seed the free-running lf_div to the post-BIOS hardware phase.
            // Calibrated: at single-speed instruction boundaries the passing
            // SameSuite phase is lf_div=1 (trigger delay 5 fresh / 3 active),
            // i.e. the invariant `lf_div == (cc&1)^1` at the anchor; from here
            // it free-runs on elapsed 2 MHz cycles only.
            self.lf_div = (self.cc & 1) ^ 1;
            self.push_cc();
            return;
        }

        // A DIV write resets the divider; mirror `PSG::divReset`. Gambatte runs
        // `generateSamples(writeCc)` then `divReset` AT the DIV-write cc, so we
        // advance to the DIV-write cc (`div_anchor`, the timer's access-cc for
        // the FF04 write), fire any length events strictly before the fold, then
        // fold `cycleCounter_` there — not at the (later) current dot.
        if div_resets != self.last_div_resets {
            // Run the fold AT the FF04 write's canonical access cc (`div_anchor`,
            // the timer's `access_cc()`), not the later current dot — so the
            // length-expiry boundary `((cc>>13)+len)<<13` is anchored to the SAME
            // per-access cc the subsequent NR52 read resolves on (M7). This is
            // the mixed-anchor the prior `advance_to(abs_cc)` left open.
            self.advance_to(div_anchor, ds);
            self.push_cc();
            self.fire_length_events(self.cc);
            self.div_reset_fold(ds);
            self.last_div_resets = div_resets;
        }

        self.advance_to(abs_cc, ds);
        self.push_cc();
        self.fire_length_events(self.cc);
    }

    /// Gambatte `PSG::generateSamples`: convert CPU cycles since `last_update` to
    /// 2 MHz APU cycles and advance `cc`. We don't buffer audio here (the live
    /// mixer is sampled elsewhere), so this only moves the clock.
    fn advance_to(&mut self, abs_cc: u64, ds: bool) {
        // rustyboi gates `step_audio` to half-rate in double speed, so the timer
        // divider (`abs_cc`) already advances at the physical APU rate that the
        // duty/envelope tuning was anchored to: shift by 1 in both speeds, i.e.
        // `cc == abs_cc >> 1` at steady state (matching the prior `ic >> 1`).
        // Count whole APU cycles using absolute even boundaries (floor(abs/2) -
        // floor(last/2)), matching the prior direct `ic >> 1` so the floored
        // phase aligns to absolute parity rather than the anchor's parity.
        // Guard against a non-monotonic target (e.g. a DIV-write access cc that
        // resolves slightly before the current dot anchor): never run backward.
        // The duty/envelope/sweep `cc` AND the length `len_cc` both mirror
        // Gambatte's single `cycleCounter_`, which advances at
        // `(cpuCc - lastUpdate) >> (1 + ds)` (PSG::generateSamples). At double
        // speed the divider runs twice as fast in CPU-cc terms, so the APU clock
        // advances at HALF the rate — the duty period `(2048-freq)*2` is in the
        // same 2 MHz units regardless of speed. (The earlier `>>1` duty rate ran
        // the duty 2x too fast in DS, off the pos6→pos7 boundary.)
        let shift = 1 + ds as u32;
        // Faithful `PSG::generateSamples`. `cycles` is the cc delta taken BEFORE
        // the shift (Gambatte sound.cpp:142), and `last_update` advances by whole
        // APU cycles (`cycles << shift`), staying a FLOORED boundary that preserves
        // the sub-quantum remainder/parity. `cc` is the single real counter;
        // `len_cc` mirrors it exactly (no dual clock).
        if abs_cc <= self.last_update {
            return;
        }
        let cycles = (abs_cc - self.last_update) >> shift;
        if cycles == 0 {
            return;
        }
        self.last_update = self.last_update.wrapping_add(cycles << shift);
        let pre_cc = self.cc;
        self.cc = ((self.cc as u64 + cycles) % Self::CC_MAX as u64) as u32;
        self.len_cc = self.cc;
        // SameBoy GB_apu_run: the 1 MHz sub-phase free-runs on elapsed 2 MHz
        // cycle parity (never folded).
        self.lf_div ^= (cycles & 1) as u32;
        // Dispatch the DIV-APU (envelope) events crossed by this advance.
        self.fs_walk(pre_cc, cycles);
    }

    /// Gambatte `PSG::divReset`: re-fold `cycleCounter_` so the DIV-relative phase
    /// resets while the length `cc>>13` boundaries are preserved. The duty unit is
    /// shifted by the resulting delta (`Channel::resetCc`).
    fn div_reset_fold(&mut self, ds: bool) {
        let div_offset = (self.last_update as u32) & (ds as u32);
        let cc = self.cc.wrapping_add(div_offset);
        let folded = (cc & 0xFFFF_F000)
            .wrapping_add(2 * (cc & 0x800))
            .wrapping_sub(div_offset)
            % Self::CC_MAX;
        // Hardware DIV-write trigger: resetting DIV while the DIV-APU bit is
        // high is a falling edge — the DIV-APU event fires AT the write. (The
        // fold expresses the same thing by jumping cc forward across the
        // 0x1000 boundary; the low-12-bit reset itself never crosses one.)
        // The bit is read in the -2 event-grid frame (see fs_walk): a cc in
        // the last 2 cells of the high half already fired its falling edge in
        // advance_to and must not double-fire here.
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
        // Single counter — `len_cc` mirrors `cc` (Gambatte folds the one
        // `cycleCounter_`; the channels' `len_counter` boundaries survive because
        // the fold preserves `cc & -0x1000`).
        self.len_cc = self.cc;
    }

    fn push_cc(&mut self) {
        let cc = self.cc;
        self.channel1.set_cc(cc);
        self.channel2.set_cc(cc);
        self.channel3.set_cc(cc);
        self.channel4.set_cc(cc);
        // SameBoy `lf_div` (the 1 MHz sub-phase for the trigger delay): the
        // controller's free-running parity bit (toggled in `advance_to`,
        // seeded at boot / APU power-on, immune to the DIV-reset and
        // speed-switch cc folds — hardware never folds this phase).
        self.channel1.set_lf_div(self.lf_div);
        self.channel2.set_lf_div(self.lf_div);
        self.channel1.set_ds(self.cached_ds);
        self.channel2.set_ds(self.cached_ds);
        self.channel4.set_ds(self.cached_ds);
        // The single counter — length cc is `cc` itself.
        let lcc = cc;
        self.channel1.set_len_cc(lcc);
        self.channel2.set_len_cc(lcc);
        self.channel3.set_len_cc(lcc);
        self.channel4.set_len_cc(lcc);
    }

    /// Gambatte's length unit is a scheduled absolute-cc event: when the master
    /// clock reaches a channel's `counter_` (`((cc>>13)+len)<<13`), the channel's
    /// length expires (disables it). We poll it each clock advance.
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

    /// Gambatte `PSG::reset`, fired on the NR52 0→1 (APU enable) transition. Folds
    /// the master clock from its large `abs_cc>>1`-anchored value down to the small
    /// FS-anchored value Gambatte's `cycleCounter_` carries, then re-initializes
    /// every channel's duty/envelope/LFSR sub-counter at the folded cc. The length
    /// counters survive (they're re-derived against the new small `cc>>13`).
    ///
    /// This is the M2b fix: the length-expiry boundary `((cc>>13)+len)<<13` was
    /// being computed against the un-folded large anchor, landing one 0x1000
    /// quantum off Gambatte after a DIV write. Folding the whole APU clock here
    /// re-anchors that boundary exactly like Gambatte.
    fn psg_reset(&mut self, ds: bool) {
        // Skip the fold before the APU master clock is anchored (boot instant,
        // `cc`/`last_update` still 0): there's no accumulated phase to fold, and
        // the fold formula would inject a spurious +0x1000 that offsets `cc>>13`
        // (the length quantum) for the rest of the run. The channel sub-counters
        // are still reset.
        if !self.clock_anchored {
            // SameBoy GB_apu_init: APU power-on resets the 1 MHz sub-phase.
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
        // SameBoy GB_apu_init (NR52 0->1): lf_div re-seeds at the power-on
        // write cc. This is the ONLY event besides boot that re-seeds the
        // free-running sub-phase. The seed is SPEED-DEPENDENT: the NR52
        // write-effect instant sits at a different sub-position of the 2 MHz
        // cell in double speed (a DS M-cycle covers one 2 MHz cycle, an SS
        // M-cycle two), so the 1 MHz phase the duty unit latches differs by
        // one: 1 in single speed (SameBoy's GB_apu_init frame), 0 in double
        // speed. Measured: the SS seed carries the SameSuite channel_*_align/
        // duty/delay set; the DS seed is pinned by the gambatte cgb04c
        // ch1_duty0_pos6_to_pos7_timing_ds ladder (write-capture brackets).
        // REVISION FORK: the DS seed 0 is the cgb04c (CPU-CGB-C) placement,
        // one half of the C pair (with the square DS delay 5-2a-lf). CPU-CGB-
        // D/E silicon (SameSuite's DS align tests) takes the SameBoy-literal
        // pair instead: seed 1 always, DS delay 6-2a-lf (SameBoy's square
        // delay is `6 + lf * (model < CGB_D && ds ? 1 : -1)`). Both are real
        // hardware; `cgb_de` (Hardware::CGBE) selects the D/E side.
        self.lf_div = if self.cached_ds && !self.cgb_de { 0 } else { 1 };
        // SameBoy GB_apu_init power-on DIV-APU glitch: enabling the APU while
        // the DIV-APU bit (the half-period phase, read in the -2 event-grid
        // frame of fs_walk) is high skips the first (truncated) DIV-APU
        // event, with div_divider pre-set to 1.
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
        // Faithful `PSG::reset` (sound.cpp:67). Single counter, no reconstruction-
        // drift bias (`last_update` is the exact floored boundary, so `cc` already
        // equals Gambatte's `cycleCounter_`).
        let div_offset = (self.last_update as u32) & (ds as u32);
        let cc = self.cc.wrapping_add(div_offset);
        let not_ds = (!ds) as u32;
        let folded = ((cc & 0xFFF)
            .wrapping_add(2 * (!(cc.wrapping_add(1).wrapping_add(not_ds)) & 0x800)))
            % Self::CC_MAX;
        self.cc = folded;
        self.len_cc = folded;
        // lastUpdate_ = ((lastUpdate_ + 3) & -4) - !ds  (parity re-anchor).
        self.last_update = (self.last_update.wrapping_add(3) & !3u64)
            .wrapping_sub(not_ds as u64);
        // Push the folded cc into the channels first so ch4's `Lfsr::reset(cc)`
        // anchors `backupCounter_` at the folded cc (Gambatte `reset(cc)`).
        self.push_cc();
        self.channel1.psg_reset();
        self.channel2.psg_reset();
        self.channel3.psg_reset();
        self.channel4.psg_reset();
    }

    /// Faithful `PSG::speedChange` (sound.cpp:106) for the
    /// CGB STOP speed switch. `stop_cc` is the master cc the STOP resolves at
    /// (Gambatte's `cc`); the flush target is `cc_ = stop_cc + 8 * !old_ds`. The
    /// single counter is flushed to `cc_` at the OLD speed (`generateSamples`),
    /// then `lastUpdate_ -= old_ds`, then the single→double divCycles/2 fold.
    pub fn psg_speed_change_at(&mut self, old_ds: bool, stop_cc: u64) {
        if !self.clock_anchored {
            return;
        }
        let cc_ = stop_cc + 8 * (!old_ds as u64);
        // generateSamples(cc_, old_ds) — flush the single counter to the switch cc.
        self.advance_to(cc_, old_ds);
        // The real DS->SS STOP bridge advances the 1 MHz sub-phase by an ODD
        // number of 2 MHz cycles relative to Gambatte's flush model, flipping
        // the free-running lf_div (the SS->DS direction is parity-even —
        // flipping it breaks the nr4init fresh-DS bracket). The duty GRIDS
        // carried across the switch stay Gambatte-exact (an odd whole-clock
        // advance breaks the carried-grid brackets, measured +6).
        if old_ds {
            self.lf_div ^= 1;
        }
        // lastUpdate_ -= ds
        if old_ds {
            self.last_update = self.last_update.wrapping_sub(1);
        }
        // Only the single->double transition re-folds cycleCounter_.
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
    pub fn sync_wave_for_read(&mut self) {
        if self.audio_enabled {
            self.channel3.sync_for_read();
            self.channel4.sync_for_read();
        }
    }

    /// Resolve the length subsystem at the canonical CPU-access cc on an APU
    /// register read (M7). `read_abs_cc` is the master cc at the exact access
    /// point (the same canonical cc the timer register access resolves on); it
    /// may run a few dots ahead of the per-dot `self.last_update` that the
    /// duty/envelope sub-counters are anchored to.
    ///
    /// We overlay each channel's length-comparison cc (`len_cc`) at the access
    /// cc — `self.cc + ((read_abs_cc>>1) - (last_update>>1))` — and fire any
    /// length expiry there, WITHOUT disturbing `self.cc`/`last_update`/duty. This
    /// makes the cycle-exact NR52 length-expiry boundary (`((cc>>13)+len)<<13`
    /// vs the read cc) resolve at the same canonical access cc as the timer,
    /// dissolving the per-peripheral phase constant.
    pub fn set_read_len_cc(&mut self, read_abs_cc: u64) {
        if !self.clock_anchored {
            return;
        }
        let shift = 1 + self.cached_ds as u32;
        // Gambatte `generateSamples` advances by `(cpuCc - lastUpdate) >> (1+ds)`
        // — the difference is taken BEFORE the shift. Flooring `read_abs_cc` and
        // `last_update` independently (each `>>shift`) over-counts by one length-cc
        // when they straddle a `>>shift` boundary, pushing the read one cc past the
        // expiry boundary (the ch2 nr52 `_1a` off-by-one). Match Gambatte exactly.
        let delta = (read_abs_cc.wrapping_sub(self.last_update) >> shift) as u32;
        let lcc = self.len_cc.wrapping_add(delta);
        // Record the per-access read cc (channel 2 MHz units) so a PCM12 read
        // in this access resolves the square duty at the SAME canonical access
        // clock (M7), not the earlier per-dot sync (`pcm_nibble_at`).
        self.pcm_read_cc = Some(self.cc.wrapping_add(delta));
        self.channel1.set_len_cc(lcc);
        self.channel2.set_len_cc(lcc);
        self.channel3.set_len_cc(lcc);
        self.channel4.set_len_cc(lcc);
        self.fire_length_events(lcc);
        // Channel 4's PCM34 read resolves at the same per-access cc via the
        // non-mutating shadow advance (`pcm_nibble_at`, keyed off
        // `pcm_read_cc`).
        // Restore the steady-state length cc so the next per-dot `push_cc`
        // (which uses the un-overlaid `len_cc`) doesn't see a stale ahead value.
        let base = self.len_cc;
        self.channel1.set_len_cc(base);
        self.channel2.set_len_cc(base);
        self.channel3.set_len_cc(base);
        self.channel4.set_len_cc(base);
    }

    /// Overlay the length subsystem cc (`len_cc`) at the canonical CPU WRITE
    /// access cc, so the NRx1/NRx4 length-counter math (trigger reload + expiry
    /// scheduling, `((len_cc>>13)+len)<<13`) is anchored to the SAME per-access
    /// clock the subsequent NR52 read resolves on (M7 read side: `set_read_len_cc`).
    /// The write side is a SEPARATE phase term from the read (the trigger's
    /// `nr4Change` boundary rounding differs from the read's `event` rounding), so
    /// `write_abs_cc` carries the write access cc (`abs_cc + APU_WRITE_CC_OFF`).
    /// Unlike the read overlay we LEAVE `len_cc` set: the immediately-following
    /// `audio.write` consumes it, and the next per-dot `push_cc` restores the
    /// steady-state base. Duty/envelope (`self.cc`) are untouched.
    pub fn set_write_len_cc(&mut self, write_abs_cc: u64) {
        if !self.clock_anchored {
            return;
        }
        let shift = 1 + self.cached_ds as u32;
        // The BEFORE-shift delta (Gambatte generateSamples) over the single
        // counter, matching the read path.
        let delta = (write_abs_cc.wrapping_sub(self.last_update) >> shift) as u32;
        let lcc = self.len_cc.wrapping_add(delta);
        self.channel1.set_len_cc(lcc);
        self.channel2.set_len_cc(lcc);
        self.channel3.set_len_cc(lcc);
        self.channel4.set_len_cc(lcc);
    }

    /// Restore the steady-state length cc after a write overlay, so a later
    /// per-dot poll doesn't see a stale ahead value before the next `push_cc`.
    pub fn restore_len_cc(&mut self) {
        if !self.clock_anchored {
            return;
        }
        let base = self.len_cc;
        self.channel1.set_len_cc(base);
        self.channel2.set_len_cc(base);
        self.channel3.set_len_cc(base);
        self.channel4.set_len_cc(base);
    }

    /// Apply Gambatte's post-`skip_bios` APU state. The boot ROM enables the APU
    /// and leaves channel 1 mid-tone (the startup "ding"). `sync_cc` must run
    /// first so the channels' duty event counter has the correct cc base.
    /// `cgb` selects the CGB vs DMG duty phase (Gambatte `setPostBiosState`).
    /// Record the CGB/DMG flag before the boot `sync_cc` anchors the SPU clock,
    /// so the post-BIOS `cycleCounter_` high-bit constant is correct.
    pub fn set_boot_cgb(&mut self, cgb: bool) {
        self.boot_cgb = cgb;
        self.channel4.set_cgb(cgb);
    }

    /// Seed the CGB-D/E APU revision gate (SameBoy `model > GB_MODEL_CGB_C`)
    /// into the revision-forked units: the square duty-trigger DS delay pair
    /// (psg_reset lf seed + DS delay formula) and the ch4 divisor-0 DS
    /// countdown. Called once from `GB::new` for Hardware::CGBE.
    pub fn set_cgb_de(&mut self, de: bool) {
        self.cgb_de = de;
        self.channel1.set_cgb_de(de);
        self.channel2.set_cgb_de(de);
        self.channel4.set_cgb_de(de);
    }

    /// Seed the CGB-B-or-earlier APU revision gate (SameBoy `GB_is_cgb &&
    /// model <= GB_MODEL_CGB_B`) into all four channels' NRx4 length-glitch
    /// fork. Called once from `GB::new` for Hardware::CGB0/CGBB.
    pub fn set_cgb_le_b(&mut self, le_b: bool) {
        self.channel1.set_cgb_le_b(le_b);
        self.channel2.set_cgb_le_b(le_b);
        self.channel3.set_cgb_le_b(le_b);
        self.channel4.set_cgb_le_b(le_b);
    }

    /// CPU-CGB-A/B (Hardware::CGBB) wave first-glitch-write swallow.
    pub fn set_cgb_b(&mut self, b: bool) {
        self.channel3.set_cgb_b(b);
    }

    /// CGB-C-and-older PCM read glitch (SameBoy `pcm_mask` applied for
    /// `model <= GB_MODEL_CGB_C`; excludes AGB and CGB-D/E).
    pub fn set_pcm_c_glitch(&mut self, on: bool) {
        self.channel1.set_pcm_c_glitch(on);
        self.channel2.set_pcm_c_glitch(on);
    }

    /// NRx4 sample-index step-back parity gate for the two square channels
    /// (true for CGB0/CGBB/AGB; SameBoy gates the step-back on
    /// `sample_countdown & 1` for those, unconditional on CGB-D/E).
    pub fn set_step_back_parity(&mut self, on: bool) {
        self.channel1.set_step_back_parity(on);
        self.channel2.set_step_back_parity(on);
    }

    /// Seed the AGB flag into the wave channel (Gambatte channel3 agb_).
    pub fn set_agb(&mut self, agb: bool) {
        self.channel3.set_agb(agb);
    }

    /// Seed the post-boot APU state. `cgb` selects the CGB vs DMG channel-1
    /// startup-tone phase; `ch1_active` is the NR52 bit-0 (channel-1 running)
    /// state at hand-off. The DMG/MGB/CGB boot ROMs play the startup "ding" and
    /// hand off with channel 1 still running (bit 0 = 1); the SGB boot ROM plays
    /// no chime on the Game Boy side, so it hands off with channel 1 already
    /// disabled (NR52 reads 0xF0, not 0xF1 — mooneye boot_hwio-S).
    pub fn set_post_bios_state(&mut self, cgb: bool, ch1_active: bool) {
        self.audio_enabled = true;
        self.nr50 = 0x77;
        self.nr51 = 0xF3;
        self.nr52 = 0xF1;

        // Channel 1 startup-tone phase: CGB pos=6/high, offset 37*2; DMG pos=3,
        // offset 69*2 (Gambatte initstate.cpp).
        if cgb {
            self.channel1.set_post_bios_ch1(37 * 2, 6, true);
        } else {
            self.channel1.set_post_bios_ch1(69 * 2, 3, false);
        }
        // SGB: same register bytes as DMG, but channel 1 is not left running.
        if !ch1_active {
            self.channel1.set_enabled(false);
        }

        self.channel2.set_length_counter(0x40);
    }

    pub fn step(&mut self, mmio: &mut mmio::Mmio) {
        if !self.audio_enabled {
            return;
        }

        // Step individual channels
        self.channel1.step(mmio);
        self.channel2.step(mmio);
        self.channel3.step(mmio);
        self.channel4.step(mmio);

        // The frame sequencer is clocked directly by the timer on each DIV-bit-12
        // falling edge (see `clock_frame_sequencer`), so nothing to do here.
    }

    /// Clock the frame sequencer one step. Called by the timer at the exact dot
    /// of each DIV-bit-12 (bit-13 in double speed) falling edge, so the sequencer
    /// stays phase-locked to DIV (and reacts to DIV writes).
    pub fn clock_frame_sequencer(&mut self) {
        if self.audio_enabled {
            self.step_frame_sequencer();
        }
    }

    fn step_frame_sequencer(&mut self) {
        let step = self.frame_sequencer_step;
        self.channel1.step_frame_sequencer(step);
        self.channel2.step_frame_sequencer(step);
        self.channel3.step_frame_sequencer(step);
        self.channel4.step_frame_sequencer(step);

        // Channels need to know which step was just clocked so their NRx4 write
        // handlers can model the length-counter "extra clock" quirk.
        self.channel1.set_fs_step(step);
        self.channel2.set_fs_step(step);
        self.channel3.set_fs_step(step);
        self.channel4.set_fs_step(step);

        self.frame_sequencer_step = (self.frame_sequencer_step + 1) % 8;
    }

    /// per-access STAGE 1: true while the APU is powered (NR52 bit 7). The
    /// min-event idle fast path only bulk-skips dots when audio is OFF, because a
    /// powered APU steps its channel duty/freq counters per dot (`step`), which is
    /// not span-collapsible like the frame sequencer.
    pub fn is_powered(&self) -> bool {
        self.audio_enabled
    }

    pub fn get_master_volume_left(&self) -> u8 {
        (self.nr50 >> 4) & 0x07
    }

    pub fn get_master_volume_right(&self) -> u8 {
        self.nr50 & 0x07
    }

    pub fn is_channel_left_enabled(&self, channel: u8) -> bool {
        match channel {
            1 => (self.nr51 >> 4) & 0x01 != 0,
            2 => (self.nr51 >> 5) & 0x01 != 0,
            3 => (self.nr51 >> 6) & 0x01 != 0,
            4 => (self.nr51 >> 7) & 0x01 != 0,
            _ => false,
        }
    }

    pub fn is_channel_right_enabled(&self, channel: u8) -> bool {
        match channel {
            1 => self.nr51 & 0x01 != 0,
            2 => (self.nr51 >> 1) & 0x01 != 0,
            3 => (self.nr51 >> 2) & 0x01 != 0,
            4 => (self.nr51 >> 3) & 0x01 != 0,
            _ => false,
        }
    }

    /// CGB PCM12 register (0xFF76): low nibble = channel 1 digital amplitude,
    /// high nibble = channel 2 (Gambatte `memory.cpp` case 0x76 ->
    /// `PSG::pcm12Read`). Returns 0 when the APU is powered off; the
    /// CGB-only / power gating is applied by the caller in `mmio.rs`.
    pub fn pcm12(&self) -> u8 {
        if !self.audio_enabled {
            return 0;
        }
        // Resolve the duty at the canonical per-access read cc (M7) when the
        // read path recorded one; fall back to the per-dot state (mixer path).
        match self.pcm_read_cc {
            Some(rcc) => {
                self.channel1.pcm_nibble_at(rcc) | (self.channel2.pcm_nibble_at(rcc) << 4)
            }
            _ => self.channel1.pcm_nibble() | (self.channel2.pcm_nibble() << 4),
        }
    }

    /// CGB PCM34 register (0xFF77): low nibble = channel 3, high nibble =
    /// channel 4 (Gambatte `PSG::pcm34Read`).
    pub fn pcm34(&self) -> u8 {
        if !self.audio_enabled {
            return 0;
        }
        // Channel 4 resolves at the canonical per-access read cc (M7) like
        // PCM12; channel 3's fetch counter was already advanced on the read
        // path (`sync_wave_for_read`).
        let ch4 = match self.pcm_read_cc {
            Some(rcc) => self.channel4.pcm_nibble_at(rcc),
            _ => self.channel4.pcm_nibble(),
        };
        self.channel3.pcm_nibble() | (ch4 << 4)
    }

    pub fn get_mixed_output(&self) -> (f32, f32) {
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

    pub fn generate_samples(&mut self, _mmio: &mut mmio::Mmio, cpu_cycles: u32) -> Vec<(f32, f32)> {
        let mut samples = Vec::new();

        // Channels are advanced per-dot via `step` (called from the Bus tick),
        // so here we only down-sample the live mixer output. Re-stepping here
        // would double-advance the channel timers and corrupt their phase.
        const CYCLES_PER_SAMPLE: f32 = 4194304.0 / 44100.0;

        self.fractional_cycles += cpu_cycles as f32;

        while self.fractional_cycles >= CYCLES_PER_SAMPLE {
            samples.push(self.get_mixed_output());
            self.fractional_cycles -= CYCLES_PER_SAMPLE;
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
            // NRx1 (length-load) registers stay writable while the APU is powered
            // off on DMG; on CGB they are ignored like every other reg (Gambatte
            // memory.cpp cases 0x11/0x16/0x1B/0x20: `if (!isEnabled() && isCgb())
            // return;`, with NR11/NR21 additionally masked to `data & 0x3F` —
            // only the length bits, not the duty bits, are applied while off).
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
                    // APU power-off (Gambatte memory.cpp case 0x26): while still
                    // enabled, write 0 to every sound register 0x10-0x25, THEN
                    // disable. The per-register writes go through the normal
                    // (enabled) path, so each NRx1 length-load reloads its length
                    // counter to its max (e.g. NR41=0 -> lengthCounter=64). This
                    // is what makes the length counters survive on DMG the way the
                    // blargg `08-len ctr during power` / `11-regs after power`
                    // tests expect — a flat struct reset would zero them instead.
                    // The free-running master clock (cc/lastUpdate) and wave RAM
                    // are left untouched (Gambatte keeps `cycleCounter_` running
                    // and `PSG::reset` never clears `waveRam_`).
                    for reg in NR10..=NR51 {
                        // Skip the unused gaps (FF15, FF1F) — open bus, no effect.
                        if reg == 0xFF15 || reg == 0xFF1F {
                            continue;
                        }
                        self.write(reg, 0);
                    }
                    // `audio_enabled` stays true through the loop above (so the
                    // zero-writes take the enabled path) and is cleared by the
                    // `audio_enabled = now_enabled` at the end of this branch.
                } else if !was_enabled && now_enabled {
                    // APU power-on (NR52 0→1): apply Gambatte's `PSG::reset` fold.
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

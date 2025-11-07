use serde::{Deserialize, Serialize};
use crate::audio::{wave, square, noise};
use crate::memory::mmio;
use crate::memory::Addressable;

pub const NR10: u16 = 0xFF10; // Channel 1 sweep register
pub const NR11: u16 = 0xFF11; // Channel 1 sound length/wave pattern duty
pub const NR12: u16 = 0xFF12; // Channel 1 volume and envelope
pub const NR13: u16 = 0xFF13; // Channel 1 period low
pub const NR14: u16 = 0xFF14; // Channel 1 period high and control

pub const NR21: u16 = 0xFF16; // Channel 2 sound length/wave pattern duty
pub const NR22: u16 = 0xFF17; // Channel 2 volume and envelope
pub const NR23: u16 = 0xFF18; // Channel 2 period low
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
            // Anchor `cc` so `abs_cc >> 1` reproduces the post-boot duty phase
            // base the channels were tuned against (the old `ic >> 1`).
            self.cc = ((abs_cc >> 1) as u32) & (Self::CC_MAX - 1);
            self.len_cc = self.cc;
            self.last_update = abs_cc;
            self.last_div_resets = div_resets;
            self.clock_anchored = true;
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
        if (abs_cc >> 1) <= (self.last_update >> 1) {
            return;
        }
        let cycles = (abs_cc >> 1) - (self.last_update >> 1);
        // Length clock advances at Gambatte's `generateSamples` rate
        // `(cpuCc - lastUpdate) >> (1 + ds)` — HALF of `cc`'s `>>1` rate at double
        // speed. Compute from absolute floored boundaries (like `cc`) so the
        // floored phase aligns to absolute parity across calls.
        let shift = 1 + ds as u32;
        let len_cycles = (abs_cc >> shift) - (self.last_update >> shift);
        self.last_update = abs_cc;
        self.cc = ((self.cc as u64 + cycles) % Self::CC_MAX as u64) as u32;
        self.len_cc = ((self.len_cc as u64 + len_cycles) % Self::CC_MAX as u64) as u32;
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
        let old = cc.wrapping_sub(div_offset);
        let delta = old.wrapping_sub(folded);
        self.cc = folded;
        self.channel1.reset_cc(delta);
        self.channel2.reset_cc(delta);
        self.channel3.reset_cc(delta);
        // Fold the length clock with the same DIV-reset transform (it preserves
        // the `cc>>13` length boundaries the channels' `len_counter` are pinned to).
        let lcc = self.len_cc.wrapping_add(div_offset);
        self.len_cc = (lcc & 0xFFFF_F000)
            .wrapping_add(2 * (lcc & 0x800))
            .wrapping_sub(div_offset)
            % Self::CC_MAX;
    }

    // APU-cc offset applied to the length subsystem only. rustyboi's duty/
    // envelope units are anchored to the raw `abs_cc>>1` phase, but the length
    // counter's DIV-phase reference (Gambatte's folded `cc>>13` boundary) is the
    // access-cc phase the timer's DIV write resolves on. This constant carries
    // that fixed phase difference into the length `cc` without disturbing duty.
    const LEN_CC_OFF: u32 = 0;

    fn push_cc(&mut self) {
        let cc = self.cc;
        self.channel1.set_cc(cc);
        self.channel2.set_cc(cc);
        self.channel3.set_cc(cc);
        self.channel4.set_cc(cc);
        let lcc = self.len_cc.wrapping_add(Self::LEN_CC_OFF);
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
        if !self.clock_anchored || self.last_update == 0 {
            self.channel1.psg_reset();
            self.channel2.psg_reset();
            self.channel3.psg_reset();
            self.channel4.psg_reset();
            self.push_cc();
            return;
        }
        // PSG::reset cycleCounter_ fold (sound.cpp:67). Fold the master
        // accumulator `self.cc` (it carries the DIV-reset fold phase the div_write
        // cases need); the boot +0x1000 artifact that would otherwise offset the
        // length quantum is suppressed by the boot-instant guard above.
        let div_offset = (self.last_update as u32) & (ds as u32);
        let cc = self.cc.wrapping_add(div_offset);
        // (cc & 0xFFF) + 2 * (~(cc + 1 + !ds) & 0x800)
        let not_ds = (!ds) as u32;
        let folded = (cc & 0xFFF)
            .wrapping_add(2 * (!(cc.wrapping_add(1).wrapping_add(not_ds)) & 0x800))
            % Self::CC_MAX;
        self.cc = folded;
        // Gambatte's `PSG::reset` folds `cycleCounter_` (the length clock) with
        // this same formula; apply it to `len_cc` so the length-expiry boundary is
        // re-anchored exactly like Gambatte after the NR52-enable.
        let lcc = self.len_cc.wrapping_add(div_offset);
        self.len_cc = (lcc & 0xFFF)
            .wrapping_add(2 * (!(lcc.wrapping_add(1).wrapping_add(not_ds)) & 0x800))
            % Self::CC_MAX;
        // Gambatte adjusts `lastUpdate_ = ((lastUpdate_+3)&-4) - !ds` here to set
        // the sub-cycle parity for subsequent generateSamples/divReset shifts.
        // rustyboi's `advance_to` re-derives whole APU cycles via `floor(abs/2)`
        // each sync, so it only needs `last_update` to stay anchored to the CPU
        // clock — mutating its low bits (and the `-!ds` underflow at last_update=0)
        // freezes `advance_to`. Leave it anchored to the current CPU cc.

        self.channel1.psg_reset();
        self.channel2.psg_reset();
        self.channel3.psg_reset();
        self.channel4.psg_reset();

        self.push_cc();
    }

    /// Gambatte `PSG::speedChange` (sound.cpp:89), fired on the CGB STOP speed
    /// switch. Flushes the APU to the switch cc (handled by the caller via
    /// `sync_cc` before this runs), then re-folds `cycleCounter_` for the
    /// single→double transition so the DIV-relative phase halves. `old_ds` is the
    /// speed being LEFT (Gambatte passes `isDoubleSpeed()` before the KEY1 toggle).
    ///
    /// Gambatte: `lastUpdate_ -= ds; if (!ds) { cc = cycleCounter_;
    /// divCycles = cc & 0xFFF; cycleCounter_ = cc - divCycles/2 - lastUpdate_%2;
    /// chN.resetCc(cc, cycleCounter_); }`. The `if(!ds)` correction only applies
    /// going single→double (the DIV runs twice as fast in cc terms afterward, so
    /// the accumulated sub-quantum phase is halved).
    pub fn psg_speed_change(&mut self, old_ds: bool) {
        if !self.clock_anchored {
            return;
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
            self.channel1.reset_cc(delta);
            self.channel2.reset_cc(delta);
            self.channel3.reset_cc(delta);
            // Gambatte's `speedChange` folds `cycleCounter_` (the length clock);
            // apply the same single->double correction to `len_cc`.
            let lc = self.len_cc;
            let lc_div = lc & 0xFFF;
            self.len_cc = lc
                .wrapping_sub(lc_div / 2)
                .wrapping_sub((self.last_update % 2) as u32)
                % Self::CC_MAX;
            self.push_cc();
            self.fire_length_events(self.cc);
        }
    }

    /// Advance only the wave channel's fetch counter to the current cc, for the
    /// CPU read path. Does not run square envelope/length events.
    pub fn sync_wave_for_read(&mut self) {
        if self.audio_enabled {
            self.channel3.sync_for_read();
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
        let lcc = self.len_cc.wrapping_add(delta).wrapping_add(Self::LEN_CC_OFF);
        self.channel1.set_len_cc(lcc);
        self.channel2.set_len_cc(lcc);
        self.channel3.set_len_cc(lcc);
        self.channel4.set_len_cc(lcc);
        self.fire_length_events(lcc);
        // Restore the steady-state length cc so the next per-dot `push_cc`
        // (which uses the un-overlaid `len_cc`) doesn't see a stale ahead value.
        let base = self.len_cc.wrapping_add(Self::LEN_CC_OFF);
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
        let delta = (write_abs_cc >> shift).wrapping_sub(self.last_update >> shift) as u32;
        let lcc = self.len_cc.wrapping_add(delta).wrapping_add(Self::LEN_CC_OFF);
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
        let base = self.len_cc.wrapping_add(Self::LEN_CC_OFF);
        self.channel1.set_len_cc(base);
        self.channel2.set_len_cc(base);
        self.channel3.set_len_cc(base);
        self.channel4.set_len_cc(base);
    }

    /// Apply Gambatte's post-`skip_bios` APU state. The boot ROM enables the APU
    /// and leaves channel 1 mid-tone (the startup "ding"). `sync_cc` must run
    /// first so the channels' duty event counter has the correct cc base.
    /// `cgb` selects the CGB vs DMG duty phase (Gambatte `setPostBiosState`).
    pub fn set_post_bios_state(&mut self, cgb: bool) {
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
            NR10..=NR14 => {
                if self.audio_enabled {
                    self.channel1.write(addr, value)
                }
            },
            NR21..=NR24 => {
                if self.audio_enabled {
                    self.channel2.write(addr, value)
                }
            },
            NR30..=NR34 => {
                if self.audio_enabled {
                    self.channel3.write(addr, value)
                }
            },
            NR41..=NR44 => {
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
                    // APU power-off: clear all channel + control registers, but
                    // preserve the free-running master clock state (Gambatte's
                    // `cycleCounter_`/`lastUpdate_` keep running while disabled).
                    // The clock continuity is what lets the next enable's
                    // `PSG::reset` fold from the correct large anchor.
                    let cc = self.cc;
                    let len_cc = self.len_cc;
                    let last_update = self.last_update;
                    let last_div_resets = self.last_div_resets;
                    let clock_anchored = self.clock_anchored;
                    // Wave pattern RAM survives APU power-off (Gambatte's
                    // `PSG::reset` leaves `waveRam_` untouched).
                    let wave_ram = self.channel3.wave_ram();
                    *self = Audio::new();
                    self.cc = cc;
                    self.len_cc = len_cc;
                    self.last_update = last_update;
                    self.last_div_resets = last_div_resets;
                    self.clock_anchored = clock_anchored;
                    self.channel3.set_wave_ram(wave_ram);
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

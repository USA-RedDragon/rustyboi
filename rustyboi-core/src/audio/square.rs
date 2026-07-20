use serde::{Deserialize, Serialize};
use crate::audio::{NR10, NR11, NR12, NR13, NR14, NR21, NR22, NR23, NR24};
use crate::memory::Addressable;

// The APU sound cycle counter is a free-running 2 MHz value; the frame
// sequencer position is `(cc >> 12) & 7`. Our FS step (the index about to be
// clocked) is offset from that by +3 (measured empirically against the boot
// DIV phase): `fs_step == ((cc >> 12) + 3) & 7`. Equivalently, length clocks
// when `(cc >> 12) & 7` is in {5,7,1,3} and envelope at {2}.
//
// Duty timing uses absolute event counters (`next_pos_update`); envelope and
// length use absolute `cc`-based counters.

const COUNTER_DISABLED: u32 = 0xFFFF_FFFF;

// Duty table: the digital output for a given
// (current_sample_index + duty*8). `current_sample_index` INCREMENTS each duty
// tick (the phase runs forward). This is the hardware-accurate model the
// SameSuite channel_*_align/duty/delay tests are validated against on cgb04c.
const DUTIES: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 1,
    1, 0, 0, 0, 0, 0, 0, 1,
    1, 0, 0, 0, 0, 1, 1, 1,
    0, 1, 1, 1, 1, 1, 1, 0,
];

fn duty_out(duty: u8, index: u8) -> bool {
    DUTIES[(index as usize & 7) + (duty as usize) * 8] != 0
}

fn to_period(freq: u16) -> u32 {
    (2048 - freq as u32) * 2
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SquareWave {
    channel1: bool,

    nr10: u8,
    nr11: u8,
    nr12: u8,
    nr13: u8,
    nr14: u8,
    nr21: u8,
    nr22: u8,
    nr23: u8,
    nr24: u8,

    enabled: bool,

    // Free-running 2 MHz cycle counter, kept in sync by the controller.
    #[serde(default)]
    cc: u32,

    // --- Duty unit (countdown model) ---
    // `period` = (2048-freq)*2, the steady-state duty tick interval in 2 MHz
    // cycles. Kept for the freq-write path.
    #[serde(default)]
    period: u32,
    // `current_sample_index`: the duty phase (0..7), INCREMENTING. NOT
    // reset on trigger — only APU-off resets it.
    #[serde(default)]
    pos: u8,
    // Cached digital-high state for the current `pos`/`duty` (computed via the
    // duty table at each tick).
    #[serde(default)]
    high: bool,
    // `sample_countdown`: 2 MHz cycles until the next duty tick. The tick
    // consumes `sample_countdown + 1` cycles (`cycles_left -= countdown+1`),
    // reloading to `(2047-freq)*2 + 1`. `-1` (u32::MAX) means "not yet reloaded"
    // (inits to -1). `sample_length` here == freq.
    #[serde(default = "disabled")]
    sample_countdown: u32,
    // `delay`: extra 2 MHz cycles added to the first countdown at trigger
    // so the first duty edge lands at the hardware-accurate phase.
    #[serde(default)]
    delay: u32,
    // `sample_surpressed`: true after a fresh trigger until the first duty
    // tick clears it; while set the channel's digital output reads 0 (this is the
    // "(sample length + 2) ticks until PCM12 is affected" delay the channel_*_delay
    // tests measure).
    #[serde(default)]
    sample_surpressed: bool,
    // `did_tick` / `just_reloaded`: NRx3/NRx4 edge-case flags.
    #[serde(default)]
    did_tick: bool,
    #[serde(default)]
    just_reloaded: bool,
    // The 2 MHz `cc` up to which the duty countdown has been advanced.
    #[serde(default)]
    last_pos_cc: u32,
    // `lf_div`: the 2 MHz sub-phase (0/1) used by the trigger delay
    // formula. Derived from the free-running `cc` parity; pushed by the controller.
    #[serde(default = "default_lf_div")]
    lf_div: u32,
    // CGB double-speed flag, pushed by the controller.
    #[serde(default)]
    ds: bool,
    // CGB-D/E APU revision gate (model newer than CGB-C): selects the
    // D/E double-speed trigger-delay placement (see the nr4 trigger below).
    #[serde(default)]
    cgb_de: bool,
    // CGB-B-or-earlier APU revision gate (CGB with model <= CGB-B): the NRx4
    // length-glitch extra clock fires regardless of
    // the written bit-6 value (see `length_nr4_change`).
    #[serde(default)]
    cgb_le_b: bool,
    // CGB-C-and-older PCM read glitch (the pcm_mask, applied for
    // model <= CGB-C; excludes AGB): see `pcm_nibble_at`.
    #[serde(default)]
    pcm_c_glitch: bool,
    // NRx4 sample-index step-back parity gate: the step-back is
    // unconditional on CGB-D/E but gated on `sample_countdown & 1` for CGB-C-
    // and-earlier AND AGB. true for {CGB0, CGBB, AGB}; the default CGB keeps
    // the unconditional (cgb04c-captured) placement. See write_nrx4.
    #[serde(default)]
    step_back_parity: bool,
    // cc of the most recent duty tick and whether the pre-tick digital sample
    // was 0 (the `cycles_left == 0 && samples[i] == 0` mask condition,
    // re-derivable against any access cc).
    #[serde(default = "disabled")]
    last_tick_cc: u32,
    #[serde(default)]
    last_tick_pre_zero: bool,
    // --- Envelope unit (DIV-anchored model) ---
    // `volume_countdown`: decremented (mod 8) on every 8th DIV-APU event
    // (div_divider & 7 == 7) while no tick is pending; reloaded from NRx2 & 7
    // at trigger and at the secondary (rising-edge) event when it hits 0.
    #[serde(default)]
    volume_countdown: u8,
    // Envelope clock: `clock` = a tick is armed (set at the
    // secondary event, consumed at the next DIV-APU event); `locked` = the
    // envelope hit its rail (0/F) and stays frozen until the next trigger.
    #[serde(default)]
    env_clock: bool,
    #[serde(default)]
    env_should_lock: bool,
    #[serde(default)]
    env_locked: bool,
    // cc of the last NRx4 trigger (for the frame-boundary race window).
    #[serde(default = "disabled")]
    env_trigger_cc: u32,
    #[serde(default)]
    volume: u8,
    // The DAC/master enable: false once the DAC is off (NRx2 high nibble 0 and
    // not increasing). The channel master-enable.
    #[serde(default)]
    master: bool,

    // --- Length counter (cc-event model) ---
    #[serde(default)]
    length_counter: u16,
    #[serde(default)]
    length_enabled: bool,
    // Absolute cc of length expiry:
    // `((cc>>13)+length_counter)<<13` when enabled, else `LEN_DISABLED`.
    #[serde(default = "len_disabled")]
    len_counter: u32,
    // Length-subsystem cc (duty/envelope use `cc`; length uses this phased cc).
    #[serde(default)]
    len_cc: u32,

    // --- Frequency sweep (Channel 1 only) ---
    #[serde(default)]
    sweep_shadow_frequency: u16,
    #[serde(default)]
    fs_step: u8,

    // cc-driven sweep (Channel 1). Absolute-cc event
    // counter, negate latch, and the CGB flag the sweep trigger init needs.
    #[serde(default = "disabled")]
    sweep_counter: u32,
    // The swept frequency APPLICATION instant: the 128 Hz DIV-APU edge,
    // 4 cc before the event grid (the sweep-calculation trigger
    // updates sample_length AT the div event; the recalculation/overflow-kill
    // side effects land later). SameSuite channel_1_sweep_restart round 1
    // pins this: the period-2 duty must NOT tick at event-3 with the old
    // period (oracle duty index 3-not-4 at the retrigger).
    #[serde(default = "disabled")]
    sweep_apply_counter: u32,
    // Deferred sweep overflow disable:
    // the post-event "second calculation" overflow check takes (NR10 & 7)
    // 1 MHz cycles on hardware; the channel stays audible until it lands
    // (SameSuite channel_1_sweep rows around the 128 Hz event pin the
    // 2*(NR10&7) cc gap). Absolute-cc event; COUNTER_DISABLED when idle.
    #[serde(default = "disabled")]
    sweep_kill_counter: u32,
    #[serde(default)]
    sweep_neg: bool,
    #[serde(default)]
    cgb: bool,

}

fn default_lf_div() -> u32 {
    1
}

fn disabled() -> u32 {
    COUNTER_DISABLED
}

const LEN_DISABLED: u32 = COUNTER_DISABLED;

fn len_disabled() -> u32 {
    LEN_DISABLED
}

impl SquareWave {
    pub(super) fn new(channel1: bool) -> Self {
        SquareWave {
            channel1,
            nr10: 0,
            nr11: 0,
            nr12: 0,
            nr13: 0,
            nr14: 0,
            nr21: 0,
            nr22: 0,
            nr23: 0,
            nr24: 0,
            enabled: false,
            cc: 0,
            period: 4096,
            pos: 0,
            high: false,
            sample_countdown: COUNTER_DISABLED,
            delay: 0,
            sample_surpressed: false,
            did_tick: false,
            just_reloaded: false,
            last_pos_cc: 0,
            lf_div: 1,
            ds: false,
            cgb_de: false,
            cgb_le_b: false,
            pcm_c_glitch: false,
            step_back_parity: false,
            last_tick_cc: COUNTER_DISABLED,
            last_tick_pre_zero: false,
            volume_countdown: 0,
            env_clock: false,
            env_should_lock: false,
            env_locked: false,
            env_trigger_cc: COUNTER_DISABLED,
            volume: 0,
            master: false,
            length_counter: 0,
            length_enabled: false,
            len_counter: LEN_DISABLED,
            len_cc: 0,
            sweep_shadow_frequency: 0,
            fs_step: 0,
            sweep_counter: COUNTER_DISABLED,
            sweep_apply_counter: COUNTER_DISABLED,
            sweep_kill_counter: COUNTER_DISABLED,
            sweep_neg: false,
            cgb: false,
        }
    }

    pub(super) fn set_cc(&mut self, cc: u32) {
        self.cc = cc;
    }

    /// The `lf_div` (2 MHz sub-phase) used by the trigger delay formula.
    pub(super) fn set_lf_div(&mut self, lf_div: u32) {
        self.lf_div = lf_div;
    }

    /// CGB double-speed flag.
    pub(super) fn set_ds(&mut self, ds: bool) {
        self.ds = ds;
    }

    /// CGB-D/E APU revision gate (model newer than CGB-C).
    pub(super) fn set_cgb_de(&mut self, de: bool) {
        self.cgb_de = de;
    }

    /// CGB-B-or-earlier APU revision gate (CGB with model <= CGB-B).
    pub(super) fn set_cgb_le_b(&mut self, le_b: bool) {
        self.cgb_le_b = le_b;
    }

    /// CGB-C-and-older PCM read glitch (the pcm_mask, model <= CGB-C).
    pub(super) fn set_pcm_c_glitch(&mut self, on: bool) {
        self.pcm_c_glitch = on;
    }

    /// NRx4 step-back parity gate (true for CGB0/CGBB/AGB; the step-back is
    /// gated on `sample_countdown & 1` for those, unconditional on D/E).
    pub(super) fn set_step_back_parity(&mut self, on: bool) {
        self.step_back_parity = on;
    }

    pub(super) fn set_len_cc(&mut self, cc: u32) {
        self.len_cc = cc;
    }

    pub(super) fn len_expired(&self) -> bool {
        self.len_cc >= self.len_counter
    }

    /// Post-boot channel-1 mid-tone state. The boot
    /// ROM leaves ch1 playing the startup tone: master/enabled with duty pos/phase
    /// mid-cycle. `pos_offset` is the duty next-pos-update offset (in 2 MHz
    /// units) added to the current cc; `pos`/`high` are the duty-unit phase.
    pub(super) fn set_post_bios_ch1(&mut self, pos_offset: u32, pos: u8, high: bool) {
        self.nr11 = 0xBF;
        self.nr12 = 0xF3;
        self.nr13 = 0xC1;
        self.nr14 = 0x07;
        self.master = true;
        self.enabled = true;
        // Post-boot the startup-ding envelope has already decayed to 0
        // (`env.volume = 0`). The channel's length counter is still
        // running (NR52 bit 0 / `enabled` set), but its digital DAC output is 0 —
        // matching the real-cgb04c `fexx_ffxx_dumper` capture where FF76 (PCM12)
        // reads 0x00 while NR52 reads 0xF1.
        self.volume = 0x00;
        self.period = to_period(self.freq());
        self.pos = pos;
        self.high = high;
        // Seed the countdown from the post-boot phase offset: the next
        // duty tick is `pos_offset` 2 MHz cycles out. `last_pos_cc` anchors the
        // countdown to the current cc so `update_pos` deltas are correct.
        self.last_pos_cc = self.cc;
        self.sample_countdown = pos_offset.wrapping_sub(1);
        self.delay = 0;
        self.sample_surpressed = false;
        self.length_counter = 0x40;
    }

    pub(super) fn set_length_counter(&mut self, value: u16) {
        self.length_counter = value;
    }

    /// Shift the duty event counter backward by `delta` (the DIV-reset fold
    /// only resets the duty unit). Called when the
    /// underlying cycle counter is reset by a DIV write. The envelope and length
    /// counters are intentionally left alone — they key on absolute `cc>>13` /
    /// `cc>>15` boundaries that survive the reset.
    pub(super) fn reset_cc(&mut self, delta: u32) {
        // Advance the duty countdown to the current (pre-fold) cc, then shift the
        // countdown anchor by the same delta the controller applies to `cc`, so the
        // subsequent `set_cc(folded)` sees a zero delta and the countdown/index are
        // preserved across the DIV-reset fold (the countdown is kept).
        self.update_pos();
        self.last_pos_cc = self.last_pos_cc.wrapping_sub(delta);
    }

    pub(super) fn set_fs_step(&mut self, step: u8) {
        self.fs_step = step;
    }

    /// Master-clock epoch rebase: shift every absolute-cc anchor down by
    /// `delta` (total and phase-preserving, unlike `reset_cc`). Sentinel-
    /// guarded counters are skipped when disarmed so a sentinel can never
    /// become a reachable value.
    pub fn epoch_fold(&mut self, delta: u32) {
        self.cc = self.cc.wrapping_sub(delta);
        self.len_cc = self.len_cc.wrapping_sub(delta);
        self.last_pos_cc = self.last_pos_cc.wrapping_sub(delta);
        for counter in [
            &mut self.last_tick_cc,
            &mut self.env_trigger_cc,
            &mut self.sweep_counter,
            &mut self.sweep_apply_counter,
            &mut self.sweep_kill_counter,
        ] {
            if *counter != COUNTER_DISABLED {
                *counter = counter.wrapping_sub(delta);
            }
        }
        if self.len_counter != LEN_DISABLED {
            self.len_counter = self.len_counter.wrapping_sub(delta);
        }
    }

    /// Channel reset (called from the APU-enable reset on
    /// the NR52 0→1 enable). Re-initializes the duty + envelope sub-counters at
    /// the freshly-folded cc. The length counter is intentionally preserved
    /// (the length counter survives the APU-enable reset).
    pub(super) fn psg_reset(&mut self) {
        // Duty-unit reset. The duty phase resets to 0 only on APU-off; the
        // NR52 0→1 enable path re-anchors the countdown but keeps the
        // sub-counter idle until a trigger. Index resets to 0 here (APU was off).
        self.pos = 0;
        self.high = false;
        self.sample_countdown = COUNTER_DISABLED;
        self.delay = 0;
        self.sample_surpressed = false;
        self.did_tick = false;
        self.just_reloaded = false;
        self.last_pos_cc = self.cc;
        self.last_tick_cc = COUNTER_DISABLED;
        self.last_tick_pre_zero = false;
        self.sweep_kill_counter = COUNTER_DISABLED;
        self.sweep_apply_counter = COUNTER_DISABLED;
        // Envelope reset (APU-init clear).
        self.volume_countdown = 0;
        self.env_clock = false;
        self.env_should_lock = false;
        self.env_locked = false;
    }

    fn freq(&self) -> u16 {
        if self.channel1 {
            ((self.nr14 as u16 & 0x07) << 8) | self.nr13 as u16
        } else {
            ((self.nr24 as u16 & 0x07) << 8) | self.nr23 as u16
        }
    }

    fn duty(&self) -> u8 {
        if self.channel1 {
            self.nr11 >> 6
        } else {
            self.nr21 >> 6
        }
    }

    fn nr2(&self) -> u8 {
        if self.channel1 { self.nr12 } else { self.nr22 }
    }

    // --- Duty unit ---

    /// Square tick loop. Advances the duty
    /// countdown by the 2 MHz cycles elapsed since `last_pos_cc`. On each underflow
    /// the sample index increments and the countdown reloads to `(2047-freq)*2+1`;
    /// the `delay` set at trigger is consumed first (before the countdown loop),
    /// which is what phases the trigger→first-edge to hardware.
    fn update_pos(&mut self) {
        let cc = self.cc;
        // How many 2 MHz cycles have elapsed since we last advanced the duty.
        let mut cycles_left = cc.wrapping_sub(self.last_pos_cc);
        // A backward overlay (per-dot cc behind a just-resolved per-access
        // write cc) must not replay as a huge wrapped span; keep the
        // further-ahead anchor and wait for the dot stream to catch up.
        if cycles_left >= 0x8000_0000 {
            return;
        }
        self.last_pos_cc = cc;
        // The duty only ticks while the channel is active (is_active).
        // While inactive the index/countdown freeze; we keep `last_pos_cc` current
        // (done above) so a later trigger doesn't replay the idle span.
        if !self.master || self.sample_countdown == COUNTER_DISABLED {
            return;
        }
        // A zero-cycle advance (a write landing on the same cc a prior dot already
        // resolved) neither ticks nor changes the reload phase, so preserve the
        // existing `just_reloaded` rather than spuriously asserting it —
        // `just_reloaded` reflects the last non-empty batch.
        if cycles_left == 0 {
            return;
        }
        // `delay` (trigger phase offset) is consumed off the front.
        if self.delay != 0 {
            if self.delay < cycles_left {
                self.delay = 0;
            } else {
                self.delay -= cycles_left;
            }
        }
        // `while (cycles_left > sample_countdown) { ... }`.
        while cycles_left > self.sample_countdown {
            // Pre-tick digital sample (the `samples[i]` value before the tick's
            // update), recorded per tick for the CGB<=C PCM read glitch.
            let pre_zero =
                self.sample_surpressed || !self.high || self.volume & 0x0F == 0;
            cycles_left -= self.sample_countdown + 1;
            self.sample_countdown = self.duty_tick_reload();
            self.pos = (self.pos + 1) & 7;
            self.sample_surpressed = false;
            self.did_tick = true;
            self.high = duty_out(self.duty(), self.pos);
            // cc this tick landed on (batch span minus what remains).
            self.last_tick_cc = cc.wrapping_sub(cycles_left);
            self.last_tick_pre_zero = pre_zero;
        }
        self.just_reloaded = cycles_left == 0;
        self.sample_countdown -= cycles_left;
    }

    /// Duty countdown reload after a tick. The duty tick interval is the full
    /// period `(2048 - freq) * 2`, and a tick consumes `countdown + 1` cycles,
    /// so the reload is exactly one less than that period. Expressed as period
    /// arithmetic rather than a packed complement-and-shift.
    fn duty_tick_reload(&self) -> u32 {
        to_period(self.freq()) - 1
    }

    /// NR13/NR23 write: update the sample length low
    /// byte (the register is already stored) and, if the countdown JUST reloaded
    /// this cycle, re-derive it from the new length so the running tone tracks the
    /// freq change immediately. Otherwise the new length takes effect on the next
    /// reload (the countdown keeps running).
    fn write_nrx3(&mut self) {
        self.update_pos();
        self.period = to_period(self.freq());
        if self.just_reloaded {
            self.sample_countdown = self.duty_tick_reload();
        }
    }

    // --- Envelope unit (DIV-anchored model) ---

    /// `is_active`: the channel is playing (triggered with the DAC
    /// on, not yet stopped by length expiry or DAC-off).
    fn is_active(&self) -> bool {
        self.enabled && self.master
    }

    // The six NRx2 envelope helpers (set_env_clock, nrx2_glitch_step,
    // nrx2_glitch, env_frame_countdown, env_secondary_reload, env_div_tick)
    // are shared byte-for-byte with the noise channel; see audio/envelope.rs.
    crate::audio::envelope::impl_envelope_unit!();

    fn write_nrx2(&mut self, value: u8) {
        let old = self.nr2();
        if (value & 0xF8) == 0 {
            // DAC off disables the channel.
            if self.channel1 {
                self.nr12 = value;
            } else {
                self.nr22 = value;
            }
            self.master = false;
            self.enabled = false;
            return;
        }
        if self.is_active() {
            // NRx2 write on a playing channel: the zombie-mode volume
            // transform. The envelope countdown itself continues undisturbed
            // (no reschedule) — ticks stay anchored to the DIV-APU events.
            self.nrx2_glitch(value, old);
        }
        if self.channel1 {
            self.nr12 = value;
        } else {
            self.nr22 = value;
        }
    }

    // --- Length counter (cc-driven) ---

    fn length_mask(&self) -> u16 {
        0x3F
    }

    fn nr4(&self) -> u8 {
        if self.channel1 { self.nr14 } else { self.nr24 }
    }

    /// NRx1 write handling. The NRx1 write reloads the length
    /// load and (re)schedules the absolute expiry cc from the current NRx4
    /// length-enable bit.
    fn write_nrx1(&mut self, value: u8) {
        if self.channel1 {
            self.nr11 = value;
        } else {
            self.nr21 = value;
        }
        let mask = self.length_mask();
        self.length_counter = (!value as u16 & mask) + 1;
        self.len_counter = if self.nr4() & 0x40 != 0 {
            ((self.len_cc >> 13) + self.length_counter as u32) << 13
        } else {
            LEN_DISABLED
        };
        self.duty_nr1_change();
    }

    /// Length-counter expiry: disables the channel.
    pub(super) fn length_event(&mut self) {
        self.len_counter = LEN_DISABLED;
        self.length_counter = 0;
        self.enabled = false;
    }

    fn duty_nr1_change(&mut self) {
        // The duty change LATCHES until the next duty tick (samples
        // are only recomputed at ticks). Recomputing `high` immediately here
        // breaks SameSuite channel_*_duty_delay + channel_*_align_cpu
        // (measured -4) and does not fix the duty0_to_duty3 captures.
        self.update_pos();
    }

    pub(super) fn step(&mut self, cgb: bool) {
        // Both channels need the CGB-features flag (the trigger pre-increment
        // quirk is CGB-D/E only); ch1 also uses it for the sweep trigger init.
        self.cgb = cgb;
        // Always keep the duty's `last_pos_cc` current (update_pos advances the
        // index only while active, but must track cc even when idle so a later
        // trigger doesn't replay the idle span).
        self.update_pos();
        if !self.master {
            return;
        }

        // The envelope is DIV-APU-event driven (see env_div_tick /
        // env_secondary_reload, dispatched by the controller).

        // Swept-frequency application at the 128 Hz DIV-APU edge (4 cc before
        // the event grid; see `sweep_apply_freq`). Polled before the event.
        if self.channel1
            && self.sweep_apply_counter != COUNTER_DISABLED
            && self.cc >= self.sweep_apply_counter
        {
            self.sweep_apply_counter = COUNTER_DISABLED;
            self.sweep_apply_freq();
        }
        // Frequency sweep event(s) (Channel 1 only) — cc-driven. Polled here,
        // not FS-clocked.
        while self.channel1
            && self.sweep_counter != COUNTER_DISABLED
            && self.cc >= self.sweep_counter
        {
            self.sweep_event();
        }
        // Deferred sweep overflow disable.
        if self.channel1
            && self.sweep_kill_counter != COUNTER_DISABLED
            && self.cc >= self.sweep_kill_counter
        {
            self.sweep_kill_counter = COUNTER_DISABLED;
            self.enabled = false;
            self.master = false;
        }
    }

    /// Distance (in 2 MHz APU cycles) from `cur` to the nearest armed,
    /// not-yet-reached ch1 sweep counter (event / apply / kill), for the lazy
    /// catch-up chunker: the controller must stop a batched clock advance
    /// exactly at each counter so the `cc >= counter` polls in `step` fire at
    /// the same cc the per-dot crank fired them at. `None` when no future
    /// counter is armed (counters already <= `cur` fire at the next poll in
    /// both models; comparisons are deliberately linear, mirroring the polls).
    pub(crate) fn next_sweep_stop(&self, cur: u32) -> Option<u32> {
        if !self.channel1 || !self.master {
            return None;
        }
        let mut best: Option<u32> = None;
        for c in [
            self.sweep_apply_counter,
            self.sweep_counter,
            self.sweep_kill_counter,
        ] {
            if c != COUNTER_DISABLED && c > cur {
                let d = c - cur;
                best = Some(best.map_or(d, |b| b.min(d)));
            }
        }
        best
    }

    /// The next swept frequency from the shadow register: `shadow ± (shadow >>
    /// shift)`, with NR10 bit 3 selecting the decreasing (subtract) direction.
    /// Pure arithmetic — the caller applies the negate latch / overflow-kill
    /// side effects.
    fn sweep_next_freq(&self) -> u16 {
        let nr0 = self.nr10;
        let step = self.sweep_shadow_frequency >> (nr0 & 0x07) as u16;
        if nr0 & 0x08 != 0 {
            self.sweep_shadow_frequency.wrapping_sub(step)
        } else {
            self.sweep_shadow_frequency.wrapping_add(step)
        }
    }

    /// Sweep calculation without the overflow disable: latches the negate flag
    /// (set once a decreasing sweep is calculated) only. Used by the deferred
    /// second calculation (the disable lands later).
    fn sweep_calc_freq_raw(&mut self) -> u16 {
        if self.nr10 & 0x08 != 0 {
            self.sweep_neg = true;
        }
        self.sweep_next_freq()
    }

    /// Sweep calculation with the overflow disable: as `sweep_calc_freq_raw`,
    /// but a result that overflows 11 bits (bit 11 set) silences the channel.
    fn sweep_calc_freq(&mut self) -> u16 {
        let freq = self.sweep_calc_freq_raw();
        if freq & 2048 != 0 {
            self.enabled = false;
            self.master = false;
        }
        freq
    }

    /// The swept-frequency APPLICATION leg (the sweep-calculation
    /// `sample_length` update): the new frequency reaches the duty unit AT the
    /// 128 Hz DIV-APU edge, 4 cc before the event grid where the
    /// calculation side effects (neg latch, overflow kill, shadow update) stay.
    /// A period-2 duty therefore reloads with the NEW period for the tick at
    /// edge+1 — one fewer old-period tick than an at-event application
    /// (SameSuite channel_1_sweep_restart round 1, oracle-verified).
    /// Pure arithmetic only: no neg latch, no kill, no shadow update — the
    /// event 4 cc later recomputes the identical value from the unchanged
    /// shadow and applies the pinned side effects.
    fn sweep_apply_freq(&mut self) {
        let at_cc = self.sweep_counter.wrapping_sub(4);
        let nr0 = self.nr10;
        if nr0 & 0x70 == 0 || nr0 & 0x07 == 0 {
            return;
        }
        let freq = self.sweep_next_freq();
        if freq & 2048 == 0 {
            self.set_freq_at(freq, at_cc);
        }
    }

    /// Sweep event. Dispatched when the master clock reaches the scheduled
    /// sweep-event cc.
    fn sweep_event(&mut self) {
        let event_cc = self.sweep_counter;
        let period = ((self.nr10 & 0x70) >> 4) as u32;
        if period != 0 {
            let freq = self.sweep_calc_freq();
            if freq & 2048 == 0 && (self.nr10 & 0x07) != 0 {
                self.sweep_shadow_frequency = freq;
                self.set_freq_at(freq, self.sweep_counter);
                // The second calculation ("overflow is checked after adding
                // the sweep delta twice"): on hardware the check takes
                // (NR10 & 7) 1 MHz cycles; defer the disable (the deferred
                // second-calculation overflow check).
                let freq2 = self.sweep_calc_freq_raw();
                if freq2 & 2048 != 0 {
                    self.sweep_kill_counter =
                        event_cc.wrapping_add(2 * (self.nr10 & 0x07) as u32);
                }
            }
            self.sweep_counter = self.sweep_counter.wrapping_add(period << 14);
        } else {
            self.sweep_counter = self.sweep_counter.wrapping_add(8u32 << 14);
        }
        self.sweep_apply_counter = self.sweep_counter.wrapping_sub(4);
    }

    /// NR10 write handling: a neg→non-neg transition after
    /// a negative calc disables master. Writing a zero sweep shift pauses the
    /// in-flight overflow calculation ("calculation is paused if the
    /// lower bits are 0" — SameSuite channel_1_sweep_restart rounds 3/7: the
    /// pending disable never lands).
    fn sweep_nr0_change(&mut self, new_nr0: u8) {
        if self.sweep_neg && (new_nr0 & 0x08) == 0 {
            self.enabled = false;
            self.master = false;
        }
        if new_nr0 & 0x07 == 0 {
            self.sweep_kill_counter = COUNTER_DISABLED;
        }
    }

    /// Sweep trigger init. Schedules the absolute-cc sweep
    /// event counter at the trigger cc.
    fn sweep_nr4_init(&mut self) {
        self.sweep_kill_counter = COUNTER_DISABLED;
        self.sweep_neg = false;
        self.sweep_shadow_frequency = self.freq();
        let period = ((self.nr10 & 0x70) >> 4) as u32;
        let rsh = (self.nr10 & 0x07) as u32;
        if period | rsh != 0 {
            let cgb2 = if self.cgb { 2 } else { 0 };
            self.sweep_counter = ((((self.cc.wrapping_add(2).wrapping_add(cgb2)) >> 14)
                + if period != 0 { period } else { 8 })
                << 14)
                .wrapping_add(2);
            self.sweep_apply_counter = self.sweep_counter.wrapping_sub(4);
        } else {
            self.sweep_counter = COUNTER_DISABLED;
            self.sweep_apply_counter = COUNTER_DISABLED;
        }
        if rsh != 0 {
            // Trigger-time overflow check ("if shift is nonzero, the check
            // also occurs on trigger"): on hardware the calculation takes
            // (NR10 & 7) 1 MHz cycles — the channel stays alive until it
            // lands (SameSuite channel_1_sweep_restart rounds 2/6: NR52
            // reads 1 for ~2*(NR10&7) cc after a restart into overflow).
            let freq = self.sweep_calc_freq_raw();
            if freq & 2048 != 0 {
                // 2*(NR10&7) for the calculation plus 4 cc: the
                // trigger-path reload timer of 2 (1 MHz) cycles before the
                // countdown starts.
                self.sweep_kill_counter = self.cc.wrapping_add(2 * rsh + 4);
            }
        }
    }

    /// Applies a new frequency but advances the duty position to a specified cc
    /// (the sweep event's scheduled cc) rather than the live `cc` — the duty
    /// unit's frequency update is applied at that event cc.
    fn set_freq_at(&mut self, new_freq: u16, at_cc: u32) {
        let saved = self.cc;
        self.cc = at_cc;
        self.update_pos();
        self.cc = saved;
        self.period = to_period(new_freq);
        // Reflect the swept frequency back into the period registers.
        self.nr13 = (new_freq & 0xFF) as u8;
        self.nr14 = (self.nr14 & 0xF8) | ((new_freq >> 8) & 0x07) as u8;
    }

    // --- NRx4 / trigger ---

    /// NR4 write length-unit handling, folded into the
    /// NRx4 write. Re-derives the length counter from the absolute expiry cc,
    /// then applies the length-enable `dec = ~cc>>12 & 1` extra-clock quirk and the
    /// trigger reload, finally rescheduling the absolute expiry.
    fn length_nr4_change(&mut self, old_nr4: u8, new_nr4: u8) {
        let mask = self.length_mask();
        if self.len_counter != LEN_DISABLED {
            self.length_counter =
                ((self.len_counter >> 13).wrapping_sub(self.len_cc >> 13)) as u16;
        }

        let mut dec: u16 = 0;
        // CGB-B and older: the length-enable extra-clock glitch fires
        // regardless of the written bit-6 value (`(value & 0x40) ||
        // (cgb && model <= CGB_B)` — "current value is
        // irrelevant"; SameSuite channel_*_extra_length_clocking-cgb0B).
        if new_nr4 & 0x40 != 0 || self.cgb_le_b {
            dec = ((!self.len_cc >> 12) & 1) as u16;
            if old_nr4 & 0x40 == 0 && self.length_counter != 0 {
                self.length_counter -= dec;
                if self.length_counter == 0 {
                    self.enabled = false;
                }
            }
        }

        if new_nr4 & 0x80 != 0 && self.length_counter == 0 {
            self.length_counter = mask + 1 - dec;
        }

        self.len_counter = if new_nr4 & 0x40 != 0 && self.length_counter != 0 {
            ((self.len_cc >> 13) + self.length_counter as u32) << 13
        } else {
            LEN_DISABLED
        };
    }

    fn write_nrx4(&mut self, value: u8) {
        let trigger = value & 0x80 != 0;
        let old_nr4 = self.nr4();

        // Catch the duty unit up to the write cc before touching the frequency
        // (the APU is run before the register write).
        self.update_pos();

        // NRx4 step-back quirk: when the sample length
        // changes from ≥$700 to <$700 on a NON-trigger write of an active channel,
        // the index steps back one (compensating a same-cycle would-be tick).
        // `if (model == CGB_E || model == CGB_D || (sample_countdown & 1))`
        // — CGB-D/E apply it unconditionally; CGB-C-and-earlier AND AGB gate it
        // on the countdown parity ("behaves slightly different on double speed").
        // In DOUBLE SPEED the pre-D/AGB parity term resolves to "never step
        // back" against rustyboi's dot-sync grid (its `sample_countdown` sits
        // one cc off the oracle's, so the low bit is 0 across the DS freq-change
        // race cells; SameSuite freq_change_timing-cgb0BC/-A pin this). The
        // default CGB keeps the unconditional (cgb04c-captured)
        // placement; `step_back_parity` selects the {CGB0,CGBB,AGB} fork.
        if !trigger
            && self.master
            && (old_nr4 & 0x7) == 7
            && (value & 7) != 7
            && !(self.step_back_parity && self.ds)
            && self.did_tick
            && self.sample_countdown >> 1 == 2047 - self.freq() as u32
        {
            self.pos = (self.pos.wrapping_sub(1)) & 7;
            self.sample_surpressed = false;
            // The output LATCH is NOT recomputed: hardware keeps emitting the
            // pre-step-back sample until the next duty tick (no sample update
            // here; SameSuite channel_1_freq_change_timing-cgbDE
            // nops-28 pins the stale-high read).
        }

        self.length_nr4_change(old_nr4, value);
        self.length_enabled = value & 0x40 != 0;

        if self.channel1 {
            self.nr14 = value;
        } else {
            self.nr24 = value;
        }
        self.period = to_period(self.freq());

        // `just_reloaded` reload from the new sample length.
        if self.just_reloaded {
            self.sample_countdown = self.duty_tick_reload();
        }

        // Duty/envelope NRx4 handling happens on trigger.
        if trigger {
            self.trigger();
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;

        // Length-counter reload + reschedule is handled in `length_nr4_change`.

        // `is_active` before the trigger = the channel was already
        // playing (DAC on + previously triggered). `master` carries that here.
        let was_active = self.master;

        // Catch the duty unit up to the trigger cc (the APU is run before
        // the register write) so the countdown/index reflect the exact trigger cc.
        self.update_pos();

        // Envelope trigger init (NRx4 bit 7): unlock, disarm, reload
        // volume + countdown from NRx2. master = DAC on.
        let nr2 = self.nr2();
        self.env_locked = false;
        self.env_clock = false;
        self.volume = nr2 >> 4;
        self.volume_countdown = nr2 & 7;
        self.env_trigger_cc = self.cc;
        let dac_off = (nr2 & 0xF8) == 0;

        // Duty period from the (possibly just-written) frequency.
        self.period = to_period(self.freq());

        // NRx4 trigger: the duty countdown/delay place
        // the first edge at the hardware-accurate phase. `sample_length` == freq.
        // `current_sample_index` (pos) is NOT reset — it persists across triggers.
        // The reload base `(sl^0x7FF)*2` plus `delay` (6-lf_div fresh / 4-lf_div
        // when the channel was already active — "sound starts 2 ticks earlier")
        // is the trigger→first-edge model the SameSuite align/delay/duty
        // tests validate on cgb04c.
        //
        // The reference additionally models a CGB-D/E trigger pre-increment quirk
        // (steps the index forward on trigger when NRx4 bit 2 is clear and a
        // countdown bit is unset). Enabling it gives ZERO SameSuite gain yet
        // regresses 16 cgb04c/dmg08 duty-pos-pattern tests (also real-hardware
        // oracles) — on the cases rustyboi exercises, cgb04c shows no
        // pre-increment — so it is omitted.
        self.did_tick = false;
        // The cgb04c/dmg08-validated cycle-exact placement, expressed
        // in the sample-countdown convention (first tick lands at
        // `cc + sample_countdown + 1`):
        //   delay = 5 - 2*was_active - lf_div
        // `was_active` (the OLD master, the is_active state) is the master
        // term for BOTH channels: the ch2 new-master variant breaks 6
        // SameSuite ch2 fresh-trigger tests (measured) and no oracle
        // test needs it.
        //
        // This is exactly 1 2MHz-cycle EARLIER than the reference's literal
        // 6-lf_div/4-lf_div: the reference write/probe grid sits 1 cycle after
        // rustyboi's dot-sync grid, so its +1 is a frame-of-reference
        // constant, not a hardware phase. Single-speed PCM12 probes land on
        // even cc and the fresh-trigger grid is odd, so SameSuite cannot
        // distinguish the two (measured: 0 samesuite delta from the -1);
        // the double-speed / post-speed-switch cgb04c brackets probe odd
        // cc and require this placement (16 speedchange + ds_6 measured).
        //
        // The phase term is the free-running POWER-ON-ANCHORED lf_div
        // (the 1 MHz sub-phase): SameSuite channel_*_align_cpu and
        // channel_*_duty sweep the APU-enable alignment and show the
        // trigger grid anchors to the enable instant, not absolute cc
        // parity (an absolute `(cc-ref)&1` phase breaks exactly those).
        //
        // REVISION FORK (`6 + lf * (model < CGB_D && ds ? 1 : -1)`):
        // the default `5 - 2a - lf_div` is the cgb04c (CPU-CGB-C) placement,
        // pinned by the DS/speedchange pos6->pos7 brackets
        // together with the controller's DS power-on lf seed 0. CPU-CGB-
        // D/E silicon (SameSuite channel_*_align/align_cpu, DS power-on
        // sweeps) takes the other literal pair instead: seed 1 always with
        // DS `6 - 2*was_active - lf` — the two demands differ by 2 cells
        // at odd power-on->trigger parity, a real revision divergence with
        // no shared value. `cgb_de` (Hardware::CGBE) selects the D/E side;
        // single speed is revision-independent.
        self.delay = if self.cgb_de && self.ds {
            6 - 2 * (was_active as u32) - self.lf_div
        } else if self.step_back_parity && self.ds && !was_active {
            // Pre-D / AGB DOUBLE-SPEED fresh trigger: the lf term flips sign
            // (`6 + lf_div` for the `< CGB_D && double_speed` fork,
            // here in the -1 frame: `5 + lf_div`). The active-trigger path
            // (`4 - lf`) has no model fork. Applied to the {CGB0,CGBB,AGB}
            // parity set (the SameSuite -cgb0BC and -A DS rows both need it —
            // the real pre-04 / AGB silicon these tests target diverges from
            // the cgb04c default `5 - lf` here), leaving the default
            // CGB unchanged so the cgb04c DS pos6->pos7 brackets hold.
            5 + self.lf_div
        } else {
            5 - 2 * (was_active as u32) - self.lf_div
        };
        // Trigger base is `(2047 - freq) * 2` (the duty period less two, the
        // half-tick trigger convention) plus the phase `delay` computed above.
        self.sample_countdown = (2047 - self.freq() as u32) * 2 + self.delay;

        self.master = !dac_off;
        // The duty output latch is NOT recomputed at trigger: it keeps the
        // last tick's value until the next duty tick. Recomputing it here with
        // the current duty flips the ch1_duty0_to_duty3_pos3 cgb04c/dmg08
        // captures (duty changed mid-position, then re-triggered: hardware
        // keeps outputting the OLD duty's level at the frozen position).
        // Volume changes still take effect instantly (`get_output` reads the
        // live `volume` field).

        // Fresh trigger with the DAC on surpresses the first output until the first
        // duty tick clears it (the `sample_surpressed` flag).
        if !dac_off && !was_active {
            self.sample_surpressed = true;
        }

        // Frequency sweep (Channel 1 only) — cc-driven.
        if self.channel1 {
            self.sweep_nr4_init();
        }

        if dac_off {
            self.enabled = false;
        }
    }

    pub(super) fn get_output(&self) -> f32 {
        if !self.enabled || !self.master || self.volume == 0 || self.sample_surpressed {
            return 0.0;
        }
        if self.high {
            (self.volume as f32) / 15.0
        } else {
            0.0
        }
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Force the channel's active/length-running flag (NR52 status bit). Used by
    /// the SGB post-boot seed to hand off with channel 1 already stopped.
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// CGB PCM12 nibble for this square channel. Reports the `samples[index]`
    /// digital amplitude: 0 while the DAC is off (`!master`) or the fresh-trigger
    /// output is still surpressed (the `sample_surpressed` flag); otherwise the
    /// current duty high-state times the envelope volume.
    ///
    pub(super) fn pcm_nibble(&self) -> u8 {
        // A length-expired channel (enabled=false, is_active=false)
        // reports 0 — the digital sample stops with the channel, even though
        // the DAC (master) state survives (SameSuite channel_*_stop_div).
        if !self.is_active() || self.sample_surpressed {
            return 0;
        }
        if self.high {
            self.volume & 0x0F
        } else {
            0
        }
    }

    /// PCM12 nibble resolved at the canonical CPU READ access cc (`read_cc`,
    /// in the channel's 2 MHz cc units) — the same per-access clock the length
    /// subsystem resolves on (M7). Advances a SHADOW copy of the duty
    /// countdown from the per-dot state to `read_cc` without mutating the
    /// channel (the read must not disturb the real per-dot stream).
    pub(super) fn pcm_nibble_at(&self, read_cc: u32) -> u8 {
        if !self.is_active() {
            return 0;
        }
        let mut high = self.high;
        let mut surpressed = self.sample_surpressed;
        // Most recent tick at-or-before the read (already-consumed ticks come
        // from `last_tick_cc`; not-yet-consumed ones from the shadow advance).
        let mut tick_cc = self.last_tick_cc;
        let mut tick_pre_zero = self.last_tick_pre_zero;
        if self.sample_countdown != COUNTER_DISABLED {
            let mut cycles_left = read_cc.wrapping_sub(self.last_pos_cc);
            // Guard against a non-monotonic overlay (access cc behind the dot
            // stream): treat as zero elapsed.
            if cycles_left < 0x8000_0000 {
                let mut countdown = self.sample_countdown;
                let mut pos = self.pos;
                while cycles_left > countdown {
                    // Pre-tick digital sample (the `samples[i]` value before the
                    // tick's update) for the CGB<=C PCM read glitch below.
                    let pre_zero = surpressed || !high || self.volume & 0x0F == 0;
                    cycles_left -= countdown + 1;
                    countdown = self.duty_tick_reload();
                    pos = (pos + 1) & 7;
                    surpressed = false;
                    high = duty_out(self.duty(), pos);
                    tick_cc = read_cc.wrapping_sub(cycles_left);
                    tick_pre_zero = pre_zero;
                }
            }
        }
        if surpressed {
            return 0;
        }
        // CGB-C and older (NOT AGB, NOT D/E): a duty tick landing in the read
        // access M-cycle (0 or 1 cc before the read resolution point) with a
        // 0 pre-tick sample masks this channel's nibble to 0 (the
        // pcm_mask, consumed for `model <= CGB_C`; the {0,1} window
        // is the read grid sitting 1 cc after rustyboi's, cell-pinned by
        // SameSuite channel_1_freq_change_timing-cgb0BC first digits vs -A).
        if self.pcm_c_glitch && tick_pre_zero && read_cc.wrapping_sub(tick_cc) <= 1 {
            return 0;
        }
        if high {
            self.volume & 0x0F
        } else {
            0
        }
    }
}

impl Addressable for SquareWave {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR10..=NR14 => {
                if self.channel1 {
                    match addr {
                        NR10 => self.nr10 | 0x80,
                        NR11 => self.nr11 | 0x3F,
                        NR12 => self.nr12,
                        NR13 => 0xFF,
                        NR14 => self.nr14 | 0xBF,
                        _ => 0xFF,
                    }
                } else {
                    panic!("Invalid read from Channel 2 SquareWave: {:#X}", addr);
                }
            }
            NR21..=NR24 => {
                if !self.channel1 {
                    match addr {
                        NR21 => self.nr21 | 0x3F,
                        NR22 => self.nr22,
                        NR23 => 0xFF,
                        NR24 => self.nr24 | 0xBF,
                        _ => 0xFF,
                    }
                } else {
                    panic!("Invalid read from Channel 1 SquareWave: {:#X}", addr);
                }
            }
            _ => panic!("Invalid address for SquareWave: {:#X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR10..=NR14 => {
                if self.channel1 {
                    match addr {
                        NR10 => {
                            self.sweep_nr0_change(value);
                            self.nr10 = value;
                        }
                        NR11 => self.write_nrx1(value),
                        NR12 => self.write_nrx2(value),
                        NR13 => {
                            self.nr13 = value;
                            self.write_nrx3();
                        }
                        NR14 => self.write_nrx4(value),
                        _ => {}
                    }
                } else {
                    panic!("Invalid write to Channel 2 SquareWave: {:#X}", addr);
                }
            }
            NR21..=NR24 => {
                if !self.channel1 {
                    match addr {
                        NR21 => self.write_nrx1(value),
                        NR22 => self.write_nrx2(value),
                        NR23 => {
                            self.nr23 = value;
                            self.write_nrx3();
                        }
                        NR24 => self.write_nrx4(value),
                        _ => {}
                    }
                } else {
                    panic!("Invalid write to Channel 1 SquareWave: {:#X}", addr);
                }
            }
            _ => panic!("Invalid address for SquareWave: {:#X}", addr),
        }
    }
}

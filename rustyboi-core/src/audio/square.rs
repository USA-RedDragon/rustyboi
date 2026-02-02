use serde::{Deserialize, Serialize};
use crate::audio::{NR10, NR11, NR12, NR13, NR14, NR21, NR22, NR23, NR24};
use crate::memory::mmio;
use crate::memory::Addressable;

// Gambatte's sound cycle counter is a free-running 2 MHz value; the frame
// sequencer position is `(cc >> 12) & 7`. Our FS step (the index about to be
// clocked) is offset from that by +3 (measured empirically against the boot
// DIV phase): `fs_step == ((cc >> 12) + 3) & 7`. Equivalently, length clocks
// when `(cc >> 12) & 7` is in {5,7,1,3} and envelope at {2}.
//
// Duty timing uses absolute event counters (`next_pos_update`) exactly like
// Gambatte's duty_unit.cpp; envelope and length use absolute `cc`-based
// counters mirroring envelope_unit.cpp / length_counter.cpp.

const COUNTER_DISABLED: u32 = 0xFFFF_FFFF;

// SameBoy `duties[]` (Core/apu.c): the digital output for a given
// (current_sample_index + duty*8). `current_sample_index` INCREMENTS each duty
// tick (SameBoy runs the phase forward), unlike Gambatte's decrementing table.
// This is the hardware-accurate model the SameSuite channel_*_align/duty/delay
// tests are validated against on cgb04c.
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
pub struct SquareWave {
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

    // --- Duty unit (SameBoy countdown model, Core/apu.c) ---
    // `period` = (2048-freq)*2, the steady-state duty tick interval in 2 MHz
    // cycles. Kept for the freq-write path.
    #[serde(default)]
    period: u32,
    // SameBoy `current_sample_index`: the duty phase (0..7), INCREMENTING. NOT
    // reset on trigger — only APU-off resets it.
    #[serde(default)]
    pos: u8,
    // Cached digital-high state for the current `pos`/`duty` (SameBoy computes it
    // via `duties[]` at each tick).
    #[serde(default)]
    high: bool,
    // SameBoy `sample_countdown`: 2 MHz cycles until the next duty tick. The tick
    // consumes `sample_countdown + 1` cycles (SameBoy `cycles_left -= countdown+1`),
    // reloading to `(2047-freq)*2 + 1`. `-1` (u32::MAX) means "not yet reloaded"
    // (SameBoy inits to -1). `sample_length` here == freq.
    #[serde(default = "disabled")]
    sample_countdown: u32,
    // SameBoy `delay`: extra 2 MHz cycles added to the first countdown at trigger
    // so the first duty edge lands at the hardware-accurate phase.
    #[serde(default)]
    delay: u32,
    // SameBoy `sample_surpressed`: true after a fresh trigger until the first duty
    // tick clears it; while set the channel's digital output reads 0 (this is the
    // "(sample length + 2) ticks until PCM12 is affected" delay the channel_*_delay
    // tests measure).
    #[serde(default)]
    sample_surpressed: bool,
    // SameBoy `did_tick` / `just_reloaded`: NRx3/NRx4 edge-case flags.
    #[serde(default)]
    did_tick: bool,
    #[serde(default)]
    just_reloaded: bool,
    // The 2 MHz `cc` up to which the duty countdown has been advanced.
    #[serde(default)]
    last_pos_cc: u32,
    // SameBoy `lf_div`: the 2 MHz sub-phase (0/1) used by the trigger delay
    // formula. Derived from the free-running `cc` parity; pushed by the controller.
    #[serde(default = "default_lf_div")]
    lf_div: u32,
    // CGB double-speed flag (SameBoy `cgb_double_speed`), pushed by the controller.
    #[serde(default)]
    ds: bool,
    // CGB-D/E APU revision gate (SameBoy `model > GB_MODEL_CGB_C`): selects the
    // D/E double-speed trigger-delay placement (see the nr4 trigger below).
    #[serde(default)]
    cgb_de: bool,
    // --- Envelope unit (SameBoy div-anchored model, Core/apu.c) ---
    // `volume_countdown`: decremented (mod 8) on every 8th DIV-APU event
    // (div_divider & 7 == 7) while no tick is pending; reloaded from NRx2 & 7
    // at trigger and at the secondary (rising-edge) event when it hits 0.
    #[serde(default)]
    volume_countdown: u8,
    // SameBoy `GB_envelope_clock_t`: `clock` = a tick is armed (set at the
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
    // not increasing). Mirrors Gambatte's `master_`.
    #[serde(default)]
    master: bool,

    // --- Length counter (Gambatte length_counter.cpp, cc-event model) ---
    #[serde(default)]
    length_counter: u16,
    #[serde(default)]
    length_enabled: bool,
    // Absolute cc of length expiry (Gambatte `LengthCounter::counter_`):
    // `((cc>>13)+lengthCounter)<<13` when enabled, else `LEN_DISABLED`.
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

    // Gambatte cc-driven sweep (channel1.cpp SweepUnit). Absolute-cc event
    // counter, `neg_` latch, and the cgb flag the nr4Init phase needs.
    #[serde(default = "disabled")]
    sweep_counter: u32,
    // The swept frequency APPLICATION instant: the 128 Hz DIV-APU edge,
    // 4 cc before the Gambatte event grid (SameBoy trigger_sweep_calculation
    // updates sample_length AT the div event; the recalculation/overflow-kill
    // side effects land later). SameSuite channel_1_sweep_restart round 1
    // pins this: the period-2 duty must NOT tick at event-3 with the old
    // period (SameBoy-oracle duty index 3-not-4 at the retrigger).
    #[serde(default = "disabled")]
    sweep_apply_counter: u32,
    // Deferred sweep overflow disable (SameBoy square_sweep_calculate_countdown):
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
    pub fn new(channel1: bool) -> Self {
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

    pub fn set_cc(&mut self, cc: u32) {
        self.cc = cc;
    }

    /// SameBoy `lf_div` (2 MHz sub-phase) used by the trigger delay formula.
    pub fn set_lf_div(&mut self, lf_div: u32) {
        self.lf_div = lf_div;
    }

    /// SameBoy `cgb_double_speed`.
    pub fn set_ds(&mut self, ds: bool) {
        self.ds = ds;
    }

    /// CGB-D/E APU revision gate (SameBoy `model > GB_MODEL_CGB_C`).
    pub fn set_cgb_de(&mut self, de: bool) {
        self.cgb_de = de;
    }


    pub fn set_len_cc(&mut self, cc: u32) {
        self.len_cc = cc;
    }

    pub fn len_expired(&self) -> bool {
        self.len_cc >= self.len_counter
    }

    /// Post-boot channel-1 mid-tone state (Gambatte `setPostBiosState`). The boot
    /// ROM leaves ch1 playing the startup tone: master/enabled with duty pos/phase
    /// mid-cycle. `pos_offset` is Gambatte's duty.nextPosUpdate offset (in 2 MHz
    /// units) added to the current cc; `pos`/`high` are the duty-unit phase.
    pub fn set_post_bios_ch1(&mut self, pos_offset: u32, pos: u8, high: bool) {
        self.nr11 = 0xBF;
        self.nr12 = 0xF3;
        self.nr13 = 0xC1;
        self.nr14 = 0x07;
        self.master = true;
        self.enabled = true;
        // Post-boot the startup-ding envelope has already decayed to 0 (Gambatte
        // initstate.cpp `env.volume = 0`). The channel's length counter is still
        // running (NR52 bit 0 / `enabled` set), but its digital DAC output is 0 —
        // matching the real-cgb04c `fexx_ffxx_dumper` capture where FF76 (PCM12)
        // reads 0x00 while NR52 reads 0xF1.
        self.volume = 0x00;
        self.period = to_period(self.freq());
        self.pos = pos;
        self.high = high;
        // Seed the SameBoy countdown from the post-boot phase offset: the next
        // duty tick is `pos_offset` 2 MHz cycles out. `last_pos_cc` anchors the
        // countdown to the current cc so `update_pos` deltas are correct.
        self.last_pos_cc = self.cc;
        self.sample_countdown = pos_offset.wrapping_sub(1);
        self.delay = 0;
        self.sample_surpressed = false;
        self.length_counter = 0x40;
    }

    pub fn set_length_counter(&mut self, value: u16) {
        self.length_counter = value;
    }

    /// Shift the duty event counter backward by `delta` (Gambatte
    /// `Channel::resetCc`, which only resets the duty unit). Called when the
    /// underlying cycle counter is reset by a DIV write. The envelope and length
    /// counters are intentionally left alone — they key on absolute `cc>>13` /
    /// `cc>>15` boundaries that survive the reset.
    pub fn reset_cc(&mut self, delta: u32) {
        // Advance the duty countdown to the current (pre-fold) cc, then shift the
        // countdown anchor by the same delta the controller applies to `cc`, so the
        // subsequent `set_cc(folded)` sees a zero delta and the countdown/index are
        // preserved across the DIV-reset fold (SameBoy keeps `sample_countdown`).
        self.update_pos();
        self.last_pos_cc = self.last_pos_cc.wrapping_sub(delta);
    }

    pub fn set_fs_step(&mut self, step: u8) {
        self.fs_step = step;
    }

    /// Gambatte `Channel1::reset`/`Channel2::reset` (called from `PSG::reset` on
    /// the NR52 0→1 enable). Re-initializes the duty + envelope sub-counters at
    /// the freshly-folded cc. The length counter is intentionally preserved
    /// (Gambatte's `lengthCounter_` survives `PSG::reset`).
    pub fn psg_reset(&mut self) {
        // DutyUnit::reset. SameBoy resets the duty phase to 0 only on APU-off; the
        // NR52 0→1 enable path (PSG::reset) re-anchors the countdown but keeps the
        // sub-counter idle until a trigger. Index resets to 0 here (APU was off).
        self.pos = 0;
        self.high = false;
        self.sample_countdown = COUNTER_DISABLED;
        self.delay = 0;
        self.sample_surpressed = false;
        self.did_tick = false;
        self.just_reloaded = false;
        self.last_pos_cc = self.cc;
        self.sweep_kill_counter = COUNTER_DISABLED;
        self.sweep_apply_counter = COUNTER_DISABLED;
        // Envelope reset (SameBoy GB_apu_init memset).
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

    /// SameBoy `GB_apu_run` square tick loop (Core/apu.c ~959). Advances the duty
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
        // SameBoy only ticks the duty while the channel is active (is_active[i]).
        // While inactive the index/countdown freeze; we keep `last_pos_cc` current
        // (done above) so a later trigger doesn't replay the idle span.
        if !self.master || self.sample_countdown == COUNTER_DISABLED {
            return;
        }
        // A zero-cycle advance (a write landing on the same cc a prior dot already
        // resolved) neither ticks nor changes the reload phase, so preserve the
        // existing `just_reloaded` rather than spuriously asserting it. In SameBoy
        // `just_reloaded` reflects the last non-empty batch.
        if cycles_left == 0 {
            return;
        }
        // SameBoy: `delay` (trigger phase offset) is consumed off the front.
        if self.delay != 0 {
            if self.delay < cycles_left {
                self.delay = 0;
            } else {
                self.delay -= cycles_left;
            }
        }
        // SameBoy: `while (cycles_left > sample_countdown) { ... }`.
        while cycles_left > self.sample_countdown {
            cycles_left -= self.sample_countdown + 1;
            self.sample_countdown = (self.sample_length() ^ 0x7FF) * 2 + 1;
            self.pos = (self.pos + 1) & 7;
            self.sample_surpressed = false;
            self.did_tick = true;
            self.high = duty_out(self.duty(), self.pos);
        }
        self.just_reloaded = cycles_left == 0;
        self.sample_countdown -= cycles_left;
    }

    /// The 11-bit sample length == the raw frequency (SameBoy `sample_length`).
    fn sample_length(&self) -> u32 {
        self.freq() as u32
    }

    fn set_freq(&mut self, new_freq: u16) {
        self.update_pos();
        self.period = to_period(new_freq);
    }

    /// SameBoy NR13/NR23 write (Core/apu.c ~1796): update the sample length low
    /// byte (the register is already stored) and, if the countdown JUST reloaded
    /// this cycle, re-derive it from the new length so the running tone tracks the
    /// freq change immediately. Otherwise the new length takes effect on the next
    /// reload (the countdown keeps running).
    fn write_nrx3(&mut self) {
        self.update_pos();
        self.period = to_period(self.freq());
        if self.just_reloaded {
            self.sample_countdown = (self.sample_length() ^ 0x7FF) * 2 + 1;
        }
    }

    // --- Envelope unit (SameBoy div-anchored model) ---

    /// SameBoy `is_active[i]`: the channel is playing (triggered with the DAC
    /// on, not yet stopped by length expiry or DAC-off).
    fn is_active(&self) -> bool {
        self.enabled && self.master
    }

    /// SameBoy `set_envelope_clock`.
    fn set_env_clock(&mut self, value: bool, direction: bool, volume: u8) {
        if self.env_clock == value {
            return;
        }
        if value {
            self.env_clock = true;
            self.env_should_lock =
                (volume == 0xF && direction) || (volume == 0x0 && !direction);
        } else {
            self.env_clock = false;
            self.env_locked |= self.env_should_lock;
        }
    }

    /// SameBoy `_nrx2_glitch` ("zombie mode"): the volume transform an NRx2
    /// write applies to a playing channel.
    fn nrx2_glitch_step(&mut self, value: u8, old: u8) {
        if self.env_clock {
            self.volume_countdown = value & 7;
        }
        let mut should_tick = (value & 7) != 0 && (old & 7) == 0 && !self.env_locked;
        let should_invert = ((value & 8) ^ (old & 8)) != 0;

        if (value & 0xF) == 8 && (old & 0xF) == 8 && !self.env_locked {
            should_tick = true;
        }

        if should_invert {
            if value & 8 != 0 {
                if (old & 7) == 0 && !self.env_locked {
                    self.volume ^= 0xF;
                } else {
                    self.volume = 0xEu8.wrapping_sub(self.volume) & 0xF;
                }
                should_tick = false;
            } else {
                self.volume = 0x10u8.wrapping_sub(self.volume) & 0xF;
            }
        }
        if should_tick {
            if value & 8 != 0 {
                self.volume = self.volume.wrapping_add(1) & 0xF;
            } else {
                self.volume = self.volume.wrapping_sub(1) & 0xF;
            }
        } else if (value & 7) == 0 && self.env_clock {
            self.set_env_clock(false, false, 0);
        }
    }

    /// SameBoy `nrx2_glitch`: CGB-D/E apply the transform once; older
    /// revisions (and DMG) pass through an FF intermediate value, applying it
    /// twice. rustyboi's CGB target follows the SameSuite-calibrated D/E
    /// behavior; DMG takes the pre-CGB double application.
    fn nrx2_glitch(&mut self, value: u8, old: u8) {
        if self.cgb {
            self.nrx2_glitch_step(value, old);
        } else {
            self.nrx2_glitch_step(0xFF, old);
            self.nrx2_glitch_step(value, 0xFF);
        }
    }

    /// DIV-APU event leg (div_divider & 7 == 7, 64 Hz): the envelope frame
    /// countdown decrements (mod 8) while no tick is armed. A trigger 2 cc or
    /// less before the boundary shares the event's hardware M-cycle: the
    /// fresh countdown escapes this decrement (see controller fs_div_event_at).
    pub fn env_frame_countdown(&mut self, event_cc: u32) {
        if event_cc.wrapping_sub(self.env_trigger_cc) <= 2 {
            return;
        }
        if !self.env_clock {
            self.volume_countdown = self.volume_countdown.wrapping_sub(1) & 7;
        }
    }

    /// DIV-APU secondary event (rising edge, 512 Hz): a zero countdown on an
    /// active channel reloads from NRx2 and arms the tick for the next event.
    pub fn env_secondary_reload(&mut self) {
        if self.is_active() && self.volume_countdown == 0 {
            let nr2 = self.nr2();
            self.volume_countdown = nr2 & 7;
            let vol = self.volume;
            self.set_env_clock(self.volume_countdown != 0, nr2 & 8 != 0, vol);
        }
    }

    /// DIV-APU event: consume an armed tick (SameBoy `tick_square_envelope`).
    pub fn env_div_tick(&mut self) {
        if !self.env_clock {
            return;
        }
        self.set_env_clock(false, false, 0);
        if self.env_locked {
            return;
        }
        let nr2 = self.nr2();
        if nr2 & 7 == 0 {
            return;
        }
        if nr2 & 8 != 0 {
            self.volume = self.volume.wrapping_add(1) & 0xF;
        } else {
            self.volume = self.volume.wrapping_sub(1) & 0xF;
        }
    }

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
            // SameBoy NRx2 write on a playing channel: the zombie-mode volume
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

    // --- Length counter (Gambatte length_counter.cpp, cc-driven) ---

    fn length_mask(&self) -> u16 {
        0x3F
    }

    fn nr4(&self) -> u8 {
        if self.channel1 { self.nr14 } else { self.nr24 }
    }

    /// Gambatte `LengthCounter::nr1Change`. The NRx1 write reloads the length
    /// load and (re)schedules the absolute expiry cc from the current NRx4 lcen.
    fn write_nrx1(&mut self, value: u8) {
        if self.channel1 {
            self.nr11 = value;
        } else {
            self.nr21 = value;
        }
        let mask = self.length_mask();
        self.length_counter = (!value as u16 & mask) + 1;
        self.len_counter = if self.nr4() & 0x40 != 0 {
            (((self.len_cc >> 13) + self.length_counter as u32) << 13).min(u32::MAX)
        } else {
            LEN_DISABLED
        };
        self.duty_nr1_change();
    }

    /// Gambatte `LengthCounter::event`: expiry disables the channel.
    pub fn length_event(&mut self) {
        self.len_counter = LEN_DISABLED;
        self.length_counter = 0;
        self.enabled = false;
    }

    fn duty_nr1_change(&mut self) {
        // The duty change LATCHES until the next duty tick (SameBoy: samples
        // are only recomputed at ticks). Recomputing `high` immediately here
        // breaks SameSuite channel_*_duty_delay + channel_*_align_cpu
        // (measured -4) and does not fix the gambatte duty0_to_duty3 captures.
        self.update_pos();
    }

    pub fn step(&mut self, _mmio: &mut mmio::Mmio) {
        // Both channels need the CGB-features flag (the trigger pre-increment
        // quirk is CGB-D/E only); ch1 also uses it for the sweep nr4Init phase.
        self.cgb = _mmio.is_cgb_features_enabled();
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
        // Frequency sweep event(s) (Channel 1 only) — cc-driven, like Gambatte's
        // SweepUnit (channel1.cpp). Polled here, not FS-clocked.
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

    pub fn step_frame_sequencer(&mut self, _step: u8) {
        // Length is a cc-driven absolute expiry event (see `length_event`) and
        // the frequency sweep is now a cc-driven event polled in `step`, so
        // nothing is FS-clocked here.
    }

    /// `calcFreq` without the overflow disable: latches `neg_` only. Used by
    /// the deferred second calculation (the disable lands later).
    fn sweep_calc_freq_raw(&mut self) -> u16 {
        let nr0 = self.nr10;
        let shift = (nr0 & 0x07) as u16;
        let freq = if nr0 & 0x08 != 0 {
            self.sweep_shadow_frequency.wrapping_sub(self.sweep_shadow_frequency >> shift)
        } else {
            self.sweep_shadow_frequency.wrapping_add(self.sweep_shadow_frequency >> shift)
        };
        if nr0 & 0x08 != 0 {
            self.sweep_neg = true;
        }
        freq
    }

    /// Gambatte `Channel1::SweepUnit::calcFreq`. Uses NR10 directly, latches
    /// `neg_`, and disables master on an overflow (freq & 2048).
    fn sweep_calc_freq(&mut self) -> u16 {
        let nr0 = self.nr10;
        let shift = (nr0 & 0x07) as u16;
        let freq = if nr0 & 0x08 != 0 {
            self.sweep_shadow_frequency.wrapping_sub(self.sweep_shadow_frequency >> shift)
        } else {
            self.sweep_shadow_frequency.wrapping_add(self.sweep_shadow_frequency >> shift)
        };
        if nr0 & 0x08 != 0 {
            self.sweep_neg = true;
        }
        if freq & 2048 != 0 {
            self.enabled = false;
            self.master = false;
        }
        freq
    }

    /// The swept-frequency APPLICATION leg (SameBoy `trigger_sweep_calculation`
    /// `sample_length` update): the new frequency reaches the duty unit AT the
    /// 128 Hz DIV-APU edge, 4 cc before the Gambatte event grid where the
    /// calculation side effects (neg latch, overflow kill, shadow update) stay.
    /// A period-2 duty therefore reloads with the NEW period for the tick at
    /// edge+1 — one fewer old-period tick than an at-event application
    /// (SameSuite channel_1_sweep_restart round 1, SameBoy-oracle verified).
    /// Pure arithmetic only: no neg latch, no kill, no shadow update — the
    /// event 4 cc later recomputes the identical value from the unchanged
    /// shadow and applies the pinned side effects.
    fn sweep_apply_freq(&mut self) {
        let at_cc = self.sweep_counter.wrapping_sub(4);
        let nr0 = self.nr10;
        if nr0 & 0x70 == 0 || nr0 & 0x07 == 0 {
            return;
        }
        let shift = (nr0 & 0x07) as u16;
        let freq = if nr0 & 0x08 != 0 {
            self.sweep_shadow_frequency.wrapping_sub(self.sweep_shadow_frequency >> shift)
        } else {
            self.sweep_shadow_frequency.wrapping_add(self.sweep_shadow_frequency >> shift)
        };
        if freq & 2048 == 0 {
            self.set_freq_at(freq, at_cc);
        }
    }

    /// Gambatte `Channel1::SweepUnit::event`. Dispatched when `cc >= counter_`.
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
                // (NR10 & 7) 1 MHz cycles; defer the disable (SameBoy
                // sweep_calculation_done via square_sweep_calculate_countdown).
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

    /// Gambatte `Channel1::SweepUnit::nr0Change`: a neg→non-neg transition after
    /// a negative calc disables master. Writing a zero sweep shift pauses the
    /// in-flight overflow calculation (SameBoy: "calculation is paused if the
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

    /// Gambatte `Channel1::SweepUnit::nr4Init`. Schedules the absolute-cc sweep
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
                // 2*(NR10&7) for the calculation plus 4 cc: SameBoy's
                // trigger-path square_sweep_calculate_countdown_reload_timer
                // of 2 (1 MHz) cycles before the countdown starts.
                self.sweep_kill_counter = self.cc.wrapping_add(2 * rsh + 4);
            }
        }
    }

    /// Like `set_freq`, but advances the duty position to a specified cc (the
    /// sweep event's `counter_`) rather than the live `cc` (Gambatte calls
    /// `dutyUnit_.setFreq(freq, counter_)`).
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

    /// Gambatte `LengthCounter::nr4Change` length-unit handling, folded into the
    /// NRx4 write. Re-derives `lengthCounter_` from the absolute expiry cc, then
    /// applies the lcen-enable `dec = ~cc>>12 & 1` extra-clock quirk and the
    /// trigger reload, finally rescheduling the absolute expiry.
    fn length_nr4_change(&mut self, old_nr4: u8, new_nr4: u8, trigger: bool) {
        let mask = self.length_mask();
        if self.len_counter != LEN_DISABLED {
            self.length_counter =
                ((self.len_counter >> 13).wrapping_sub(self.len_cc >> 13)) as u16;
        }

        let mut dec: u16 = 0;
        if new_nr4 & 0x40 != 0 {
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

        let _ = trigger;
        self.len_counter = if new_nr4 & 0x40 != 0 && self.length_counter != 0 {
            (((self.len_cc >> 13) + self.length_counter as u32) << 13).min(u32::MAX)
        } else {
            LEN_DISABLED
        };
    }

    fn write_nrx4(&mut self, value: u8) {
        let trigger = value & 0x80 != 0;
        let old_nr4 = self.nr4();

        // Catch the duty unit up to the write cc before touching the frequency
        // (SameBoy runs GB_apu_run before the register write).
        self.update_pos();

        // SameBoy NRx4 step-back quirk (Core/apu.c ~1814): when the sample length
        // changes from ≥$700 to <$700 on a NON-trigger write of an active channel,
        // the index steps back one (compensating a same-cycle would-be tick). CGB-D/E
        // apply it unconditionally; older revs only when countdown bit 0 is set.
        if !trigger && self.master && (old_nr4 & 0x7) == 7 && (value & 7) != 7 {
            // CGB-D/E: unconditional (older revs gate on `sample_countdown & 1`).
            if self.did_tick
                && self.sample_countdown >> 1 == (self.sample_length() ^ 0x7FF)
            {
                self.pos = (self.pos.wrapping_sub(1)) & 7;
                self.sample_surpressed = false;
                // The output LATCH is NOT recomputed: hardware keeps emitting
                // the pre-step-back sample until the next duty tick (SameBoy
                // has no update_sample here; SameSuite channel_1_freq_change_-
                // timing-cgbDE nops-28 pins the stale-high read).
            }
        }

        self.length_nr4_change(old_nr4, value, trigger);
        self.length_enabled = value & 0x40 != 0;

        if self.channel1 {
            self.nr14 = value;
        } else {
            self.nr24 = value;
        }
        self.period = to_period(self.freq());

        // SameBoy: `just_reloaded` reload from the new sample length.
        if self.just_reloaded {
            self.sample_countdown = (self.sample_length() ^ 0x7FF) * 2 + 1;
        }

        // dutyUnit/envelope nr4 handling happens on trigger.
        if trigger {
            self.trigger();
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;

        // Length-counter reload + reschedule is handled in `length_nr4_change`.

        // SameBoy `is_active[index]` before the trigger = the channel was already
        // playing (DAC on + previously triggered). `master` carries that here.
        let was_active = self.master;

        // Catch the duty unit up to the trigger cc (SameBoy runs GB_apu_run before
        // the register write) so the countdown/index reflect the exact trigger cc.
        self.update_pos();

        // Envelope trigger init (SameBoy NRx4 bit 7): unlock, disarm, reload
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

        // SameBoy NRx4 trigger (Core/apu.c ~1833): the duty countdown/delay place
        // the first edge at the hardware-accurate phase. `sample_length` == freq.
        // `current_sample_index` (pos) is NOT reset — it persists across triggers.
        // The reload base `(sl^0x7FF)*2` plus `delay` (6-lf_div fresh / 4-lf_div
        // when the channel was already active — "sound starts 2 ticks earlier")
        // is the SameBoy trigger→first-edge model the SameSuite align/delay/duty
        // tests validate on cgb04c.
        //
        // SameBoy additionally models a CGB-D/E trigger pre-increment quirk (steps
        // the index forward on trigger when NRx4 bit 2 is clear and a countdown bit
        // is unset). Enabling it gives ZERO SameSuite gain yet regresses 16 gambatte
        // cgb04c/dmg08 duty-pos-pattern tests (also real-hardware oracles) — on the
        // cases rustyboi exercises, cgb04c shows no pre-increment — so it is omitted.
        let sl = self.sample_length();
        self.did_tick = false;
        // The cgb04c/dmg08-validated cycle-exact placement, re-expressed
        // in the SameBoy countdown convention (first tick lands at
        // `cc + sample_countdown + 1`):
        //   delay = 5 - 2*was_active - lf_div
        // `was_active` (the OLD master, SameBoy's is_active) is the master
        // term for BOTH channels: the ch2 new-master variant breaks 6
        // SameSuite ch2 fresh-trigger tests (measured) and no gambatte
        // test needs it.
        //
        // This is exactly 1 2MHz-cycle EARLIER than SameBoy's literal
        // 6-lf_div/4-lf_div: SameBoy's write/probe grid sits 1 cycle after
        // rustyboi's dot-sync grid, so its +1 is a frame-of-reference
        // constant, not a hardware phase. Single-speed PCM12 probes land on
        // even cc and the fresh-trigger grid is odd, so SameSuite cannot
        // distinguish the two (measured: 0 samesuite delta from the -1);
        // the double-speed / post-speed-switch gambatte brackets probe odd
        // cc and require this placement (16 speedchange + ds_6 measured).
        //
        // The phase term is the free-running POWER-ON-ANCHORED lf_div
        // (SameBoy's 1 MHz sub-phase): SameSuite channel_*_align_cpu and
        // channel_*_duty sweep the APU-enable alignment and show the
        // trigger grid anchors to the enable instant, not absolute cc
        // parity (Gambatte's `(cc-ref)&1` breaks exactly those).
        //
        // REVISION FORK (SameBoy `6 + lf * (model < CGB_D && ds ? 1 : -1)`):
        // the default `5 - 2a - lf_div` is the cgb04c (CPU-CGB-C) placement,
        // pinned by the gambatte DS/speedchange pos6->pos7 brackets
        // together with the controller's DS power-on lf seed 0. CPU-CGB-
        // D/E silicon (SameSuite channel_*_align/align_cpu, DS power-on
        // sweeps) takes SameBoy's literal pair instead: seed 1 always with
        // DS `6 - 2*was_active - lf` — the two demands differ by 2 cells
        // at odd power-on->trigger parity, a real revision divergence with
        // no shared value. `cgb_de` (Hardware::CGBE) selects the D/E side;
        // single speed is revision-independent.
        self.delay = if self.cgb_de && self.ds {
            6 - 2 * (was_active as u32) - self.lf_div
        } else {
            5 - 2 * (was_active as u32) - self.lf_div
        };
        self.sample_countdown = (sl ^ 0x7FF) * 2 + self.delay;

        self.master = !dac_off;
        // The duty output latch is NOT recomputed at trigger: it keeps the
        // last tick's value until the next duty tick. Recomputing it here with
        // the current duty flips the ch1_duty0_to_duty3_pos3 cgb04c/dmg08
        // captures (duty changed mid-position, then re-triggered: hardware
        // keeps outputting the OLD duty's level at the frozen position).
        // Volume changes still take effect instantly (`get_output` reads the
        // live `volume` field).

        // Fresh trigger with the DAC on surpresses the first output until the first
        // duty tick clears it (SameBoy `sample_surpressed`).
        if !dac_off && !was_active {
            self.sample_surpressed = true;
        }

        // Frequency sweep (Channel 1 only) — Gambatte cc-driven SweepUnit.
        if self.channel1 {
            self.sweep_nr4_init();
        }

        if dac_off {
            self.enabled = false;
        }
    }

    pub fn get_output(&self) -> f32 {
        if !self.enabled || !self.master || self.volume == 0 || self.sample_surpressed {
            return 0.0;
        }
        if self.high {
            (self.volume as f32) / 15.0
        } else {
            0.0
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// CGB PCM12 nibble for this square channel. Reports SameBoy's `samples[index]`
    /// digital amplitude: 0 while the DAC is off (`!master`) or the fresh-trigger
    /// output is still surpressed (SameBoy `sample_surpressed`); otherwise the
    /// current duty high-state times the envelope volume.
    ///
    pub fn pcm_nibble(&self) -> u8 {
        // A length-expired channel (enabled=false, SameBoy is_active=false)
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
    pub fn pcm_nibble_at(&self, read_cc: u32) -> u8 {
        if !self.is_active() {
            return 0;
        }
        let mut high = self.high;
        let mut surpressed = self.sample_surpressed;
        if self.sample_countdown != COUNTER_DISABLED {
            let mut cycles_left = read_cc.wrapping_sub(self.last_pos_cc);
            // Guard against a non-monotonic overlay (access cc behind the dot
            // stream): treat as zero elapsed.
            if cycles_left < 0x8000_0000 {
                let mut countdown = self.sample_countdown;
                let mut pos = self.pos;
                while cycles_left > countdown {
                    cycles_left -= countdown + 1;
                    countdown = (self.sample_length() ^ 0x7FF) * 2 + 1;
                    pos = (pos + 1) & 7;
                    surpressed = false;
                    high = duty_out(self.duty(), pos);
                }
            }
        }
        if surpressed {
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

//! Frame pacing: one regulator for every frontend.
//!
//! Three clocks disagree by design — the emulated console (exactly
//! [`NOMINAL_FPS`]), the host display's vsync, and the host audio DAC (its own
//! crystal). An emulator can lock to exactly one. Accuracy-first means the
//! game must run at the console's rate, so the wall clock is the ONLY master
//! of the timeline, and the audio bridges to the DAC's clock instead:
//!
//! - Tokens accrue at exactly `NOMINAL_FPS` per elapsed wall second; each
//!   display tick emulates `floor(tokens)` frames. Reading a clock is precise
//!   on every host; only *sleeping* is not (macOS timer coalescing overshoots
//!   1-7ms), and this design never sleeps on the emulating path — tick
//!   cadence is the platform's concern (a blocking vsync present, a throttle,
//!   a worker timer) and cannot affect game speed. **The audio clock has ZERO
//!   authority over the timeline**: an Android device whose DAC ran −0.4%
//!   slow measurably dragged the game to 59.5 fps under an earlier ±0.5%
//!   rate-trim design — under the GB's true rate, unacceptable.
//! - The backlog instead steers an audio **stretch ratio** ([`Stretcher`]):
//!   the pushed samples are micro-resampled to the device's actual
//!   consumption rate. A clock offset of ±0.4% is ±7 cents of pitch —
//!   imperceptible — while the game timeline stays exact. Beyond ±1% the
//!   host is broken and the ring drops/zero-fills instead (audible, its
//!   fault, and the diagnostics show it).
//!
//! The regulator is pure (time is passed in, wasm-clean) so desktop, iOS,
//! Android, and the web worker share this one implementation, and its
//! behavior is provable in ordinary unit tests with no display or audio.

/// Dots in one emulated frame (154 scanlines × 456 dots). Fixed on every model
/// — a machine's clock changes how fast these dots are played back in real
/// time, never how many there are.
pub const DOTS_PER_FRAME: f64 = 70_224.0;

/// The host output rate every audio backend consumes.
pub const HOST_SAMPLE_RATE: f64 = 44_100.0;

/// Exact emulated frame rate for DMG-rate hardware: 70224 dots at 4.194304 MHz.
/// Every model but the SGB1 runs at this rate; see [`nominal_fps`] for the
/// general case.
pub const NOMINAL_FPS: f64 = 4_194_304.0 / DOTS_PER_FRAME;

/// Exact stereo sample pairs per emulated frame at DMG rate.
pub const SAMPLES_PER_FRAME_F64: f64 = HOST_SAMPLE_RATE / NOMINAL_FPS;

/// Emulated frames per real second for a machine clocked at `cpu_hz`. An NTSC
/// SGB1 (4 295 454 Hz — the host SNES's clock / 5) presents ~61.17 fps, which
/// is exactly where its characteristic stutter on a 60 Hz display comes from.
pub fn nominal_fps(cpu_hz: u32) -> f64 {
    f64::from(cpu_hz) / DOTS_PER_FRAME
}

/// Stereo sample pairs per emulated frame for a machine clocked at `cpu_hz`.
///
/// This is the counterpart of the core's `cpu_hz / 44100` downsample ratio, and
/// the pair must stay consistent: the core emits `DOTS_PER_FRAME / (cpu_hz /
/// 44100)` pairs per frame and this rate presents `cpu_hz / DOTS_PER_FRAME`
/// frames per second, whose product is exactly 44 100 pairs/second on **every**
/// model. Change one without the other and the host output rate drifts off
/// 44.1 kHz.
pub fn samples_per_frame(cpu_hz: u32) -> f64 {
    HOST_SAMPLE_RATE / nominal_fps(cpu_hz)
}

/// Ring-depth FLOOR (in frames) the stretch steers toward — the control
/// variable is the windowed *minimum* backlog, not the mean. Device pulls
/// arrive in chunks (a PipeWire quantum ~1.4 frames; a macOS App-Nap'd
/// session gulps ~300ms ≈ 18 frames at a time), so the ring rides a sawtooth
/// whose mean is elevated by half the chunk size — a mean-targeting
/// controller misreads that as surplus and pegs low, while an instantaneous
/// controller rectifies the oscillation into a bias (both measured).
/// Steering the sawtooth's trough to a small margin above empty is
/// chunk-size-agnostic: no underruns once settled, no bias, and the mean
/// (latency) stays as low as the host's own cadence permits.
const TARGET_MIN_BACKLOG_FRAMES: f64 = 2.0;
/// The stretch ratio's authority: covers real audio-clock offsets (crystal
/// drift ~±100 ppm; sloppy phone audio PLLs a few tenths of a percent) with
/// margin. ±1% is ±17 cents of pitch — still imperceptible in game audio —
/// and anything needing more is a broken host the ring handles by
/// dropping/zero-filling instead.
const STRETCH_MAX: f64 = 0.01;
/// Proportional gain, per frame of backlog-floor error.
const STRETCH_GAIN: f64 = 0.0025;
/// Width of the rolling window the backlog minimum is taken over. Must span
/// at least one full pull cycle of the burstiest expected host cadence.
const STRETCH_WINDOW_SECS: f64 = 2.0;
/// Token bank ceiling: bounds the burst after a stall to ~100ms of game time.
const BUCKET_CAP: f64 = 6.0;
/// Per-tick emulation ceiling (bounds tick CPU; the bank carries the rest).
const MAX_PER_TICK: u32 = 4;

/// The wall-clock frame regulator (+ audio-stretch controller). One per
/// running emulator; call [`Regulator::frames_to_run`] once per platform tick.
#[derive(Debug)]
pub struct Regulator {
    /// Banked frame credit (fractional).
    tokens: f64,
    /// `now` of the previous call, in seconds. `None` until the first call.
    last: Option<f64>,
    /// Two-bucket rolling minimum of the backlog (frames): the current
    /// window's min, the previous full window's min, and when the current
    /// window began. `min(win_min, prev_win_min)` is always a full-window
    /// statistic — see [`STRETCH_WINDOW_SECS`].
    win_start: f64,
    win_min: f64,
    prev_win_min: f64,
    /// Latest audio stretch ratio (output/input pairs) — see
    /// [`Regulator::audio_stretch`].
    stretch: f64,
    /// The machine's frame rate this regulator paces to, and the matching
    /// sample pairs per frame. Both derive from the model's CPU clock, so an
    /// SGB1 paces at its true ~61.17 fps instead of a DMG's 59.73.
    fps: f64,
    samples_per_frame: f64,
}

impl Default for Regulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Regulator {
    /// A regulator at the DMG rate. Platforms that know their model should use
    /// [`Regulator::for_cpu_hz`] (or call [`Regulator::set_cpu_hz`] per tick, so
    /// a hardware change mid-session retunes the pacing).
    pub fn new() -> Self {
        Self::for_cpu_hz(4_194_304)
    }

    /// A regulator pacing a machine clocked at `cpu_hz`.
    pub fn for_cpu_hz(cpu_hz: u32) -> Self {
        // Seed one token so the very first tick shows a frame immediately.
        // The min-window buckets start at infinity: an empty window must not
        // read as "backlog was zero".
        Regulator {
            tokens: 1.0,
            last: None,
            win_start: 0.0,
            win_min: f64::INFINITY,
            prev_win_min: f64::INFINITY,
            stretch: 1.0,
            fps: nominal_fps(cpu_hz),
            samples_per_frame: samples_per_frame(cpu_hz),
        }
    }

    /// Retune to a machine clocked at `cpu_hz`. Cheap and idempotent, so the
    /// platform tick loop can call it unconditionally; switching hardware model
    /// or TV region mid-session then takes effect on the next frame.
    pub fn set_cpu_hz(&mut self, cpu_hz: u32) {
        let fps = nominal_fps(cpu_hz);
        if fps != self.fps {
            self.fps = fps;
            self.samples_per_frame = samples_per_frame(cpu_hz);
        }
    }

    /// The frame rate this regulator is pacing to.
    pub fn nominal_fps(&self) -> f64 {
        self.fps
    }

    /// How many frames to emulate this tick.
    ///
    /// `now` is monotonic seconds from any origin (`Instant`-derived on
    /// native, `performance.now()/1000` on web). `backlog_pairs` is the audio
    /// ring depth in stereo sample pairs (`None` = no audio device → pure
    /// wall-clock pacing). `fast_forward` bypasses regulation (the session
    /// batches frames itself and its audio is decimated in-core); `paused`
    /// ticks bank nothing, so resuming never bursts.
    pub fn frames_to_run(
        &mut self,
        now: f64,
        backlog_pairs: Option<usize>,
        fast_forward: bool,
        paused: bool,
    ) -> u32 {
        let dt = match self.last {
            // Clamp: a huge gap (suspend, debugger, first tick after pause) is
            // not owed frames beyond the bucket; a negative dt (clock hiccup,
            // wasm timer quantization) banks nothing.
            Some(last) => (now - last).clamp(0.0, 1.0),
            None => 0.0,
        };
        self.last = Some(now);

        if paused {
            self.tokens = self.tokens.min(1.0);
            return 0;
        }
        if fast_forward {
            // The session runs `factor` frames (or an uncapped batch) per
            // call; regulation is intentionally out of the loop. Leave a
            // single banked token so dropping out of FF resumes instantly
            // without a burst.
            self.tokens = 1.0;
            return 1;
        }

        // Audio stretch: proportional to the error in the backlog's
        // rolling-window MINIMUM (the sawtooth trough), clamped. The trough is
        // chunk-size-agnostic — see TARGET_MIN_BACKLOG_FRAMES for why neither
        // the mean nor the instantaneous value can be trusted here. This
        // steers the RESAMPLE RATIO only; the game rate below is pure wall
        // clock, so no audio clock can bend the timeline.
        if let Some(p) = backlog_pairs {
            let backlog = p as f64 / self.samples_per_frame;
            if now - self.win_start >= STRETCH_WINDOW_SECS {
                self.prev_win_min = self.win_min;
                self.win_start = now;
                self.win_min = backlog;
            } else {
                self.win_min = self.win_min.min(backlog);
            }
            let floor = self.win_min.min(self.prev_win_min);
            if floor.is_finite() {
                self.stretch = 1.0
                    + (STRETCH_GAIN * (TARGET_MIN_BACKLOG_FRAMES - floor))
                        .clamp(-STRETCH_MAX, STRETCH_MAX);
            }
        } else {
            self.stretch = 1.0;
        }

        self.tokens = (self.tokens + dt * self.fps).min(BUCKET_CAP);

        // Deliberately NO backlog ceiling on production: a host consuming
        // slower than nominal must not be able to command the game to skip
        // frames any more than a fast one may command a fast-forward. The
        // stretch is the only audio-side accommodation; if the device's clock
        // is off by more than its ±1% authority, the ring drops/zero-fills —
        // the audio degrades (the host's fault, and the diagnostics show it),
        // the game's timeline never does.
        let n = (self.tokens.floor() as u32).min(MAX_PER_TICK);
        self.tokens -= f64::from(n);
        n
    }

    /// The audio stretch ratio (output pairs per input pair) that keeps the
    /// device fed at ITS clock while the game runs at exactly the wall
    /// clock's [`NOMINAL_FPS`]. Apply to each frame's samples with a
    /// [`Stretcher`] before pushing them to the sink.
    pub fn audio_stretch(&self) -> f64 {
        self.stretch
    }

    /// Banked frame credit (diagnostics — the `RB_LOG_FPS` line).
    pub fn tokens(&self) -> f64 {
        self.tokens
    }

    /// Seconds until the bank matures its next whole token at the nominal
    /// rate — the ideal tick-timer delay for platforms that schedule their own
    /// ticks (the web worker). Sleeping exactly this long keeps wakeups at
    /// ~one per frame instead of oversampling; a late wake is harmless (the
    /// bucket banks the elapsed time).
    pub fn seconds_until_next_frame(&self) -> f64 {
        ((1.0 - self.tokens) / self.fps).max(0.0)
    }
}

/// Micro-resampler applying the regulator's [`Regulator::audio_stretch`]
/// ratio to each frame's samples before they reach the sink: linear
/// interpolation, with fractional position and the previous sample pair
/// carried across calls so arbitrary push sizes stay artifact-free. At the
/// ≤±1% ratios the regulator produces, linear interpolation of 44.1kHz game
/// audio is transparent; the pitch shift equals the host clock's own offset
/// (≤17 cents at the clamp — imperceptible).
#[derive(Debug, Default)]
pub struct Stretcher {
    /// Fractional read position within the input stream, relative to `prev`.
    pos: f64,
    /// The sample pair just before the current input slice.
    prev: Option<(f32, f32)>,
    out: Vec<(f32, f32)>,
}

impl Stretcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resample `input` by `ratio` (output pairs per input pair), returning
    /// the stretched samples. A ratio of exactly 1.0 is passed through
    /// untouched (the common case on healthy hosts once settled).
    pub fn process<'a>(&'a mut self, input: &'a [(f32, f32)], ratio: f64) -> &'a [(f32, f32)] {
        if input.is_empty() {
            return input;
        }
        if ratio == 1.0 && self.prev.is_none() {
            return input;
        }
        self.out.clear();
        let step = 1.0 / ratio.max(0.5);
        // Virtual input stream: prev (at index 0) followed by `input` (from 1).
        let first = self.prev.unwrap_or(input[0]);
        let at = |i: usize| -> (f32, f32) {
            if i == 0 { first } else { input[(i - 1).min(input.len() - 1)] }
        };
        let end = input.len() as f64; // last real index in the virtual stream
        while self.pos < end {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let (a0, b0) = at(i);
            let (a1, b1) = at(i + 1);
            self.out.push((a0 + (a1 - a0) * frac, b0 + (b1 - b0) * frac));
            self.pos += step;
        }
        self.pos -= end;
        self.prev = Some(input[input.len() - 1]);
        &self.out
    }
}

/// Frame-rate meter. The FPS readout is **game speed** — emulated frames per
/// second — the number "59.7" means on an emulator, and the one the whole
/// pacing architecture locks. It is NOT distinct-frames-presented: a late
/// tick that carries two frames coalesced into one present is a presentation
/// blemish, not a speed change, and letting it dent the readout made a
/// provably-locked game read 59.3-59.6 on hosts with tick jitter (measured on
/// Android/macOS). Honesty about speed is enforced by [`RateMeter::drift_frames`]
/// (cumulative frames vs a perfect [`NOMINAL_FPS`] timeline — it cannot
/// flatter).
///
/// Readout: rate = Δemulated/Δtime over a trailing window of half-second
/// samples, low-passed with an EMA. Emission is tick-quantized, so a naive
/// per-frame-timestamp window wobbles ±1 tick at the endpoints even when the
/// long-run rate is exact; sampling counts on a coarser grid removes that.
#[derive(Debug)]
pub struct RateMeter {
    /// (now_seconds, cumulative emulated count) samples, ~500ms apart.
    samples: std::collections::VecDeque<(f64, u64)>,
    emulated: u64,
    ema: Option<f64>,
    /// First recorded-emulation timestamp, for the drift counter.
    origin: Option<f64>,
    /// Cumulative idle time (pause/menu gaps) excluded from the drift
    /// baseline: the game legitimately does not emulate while paused, and
    /// counting that time made drift walk hundreds of frames negative over a
    /// session, burying the signal the counter exists for.
    idle_secs: f64,
    /// Timestamp of the last emulated frame: idle gaps (pre-ROM, pause)
    /// longer than [`METER_GAP_RESET_SECS`] reset the measurement epoch, so
    /// stale zero-rate history never dilutes the reading once frames flow —
    /// the meter reads the true rate within one sample period (~0.5s) of
    /// frames starting, instead of climbing from 0 over several seconds.
    last_active_at: Option<f64>,
    /// The machine's frame rate the drift counter measures against — an SGB1
    /// legitimately runs ~61.17 fps, and grading it against a DMG's 59.73 would
    /// make a perfectly-locked session look like it was racing.
    fps: f64,
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// Half-second sampling grid.
const METER_SAMPLE_SECS: f64 = 0.5;
/// Trailing window the raw rate is measured over. Generous — smooths the
/// inherent vsync-beat skip (one dup tick per ~3.7s on a 60Hz panel) and the
/// DAC trim's slow wobble into a steady readout. Long smoothing costs no
/// responsiveness because discontinuities reset the epoch (below).
const METER_WINDOW_SECS: f64 = 4.0;
/// EMA time constant applied to the windowed rate.
const METER_EMA_TAU_SECS: f64 = 3.0;
/// An emulation gap longer than this ends the measurement epoch. Normal play
/// never pauses emission for more than a tick or two (hitch recovery is
/// ≤100ms), so anything longer is a discontinuity — a menu pause, pre-ROM
/// idle. Measuring the *current* rate through it would under-read for
/// seconds after resume (user-reported: 5-8s recovery after a brief menu,
/// while the screen was visibly fine).
const METER_GAP_RESET_SECS: f64 = 0.25;

impl RateMeter {
    /// A meter at the DMG rate; see [`RateMeter::set_cpu_hz`].
    pub fn new() -> Self {
        Self::for_cpu_hz(4_194_304)
    }

    /// A meter grading a machine clocked at `cpu_hz`.
    pub fn for_cpu_hz(cpu_hz: u32) -> Self {
        RateMeter {
            samples: std::collections::VecDeque::new(),
            emulated: 0,
            ema: None,
            origin: None,
            idle_secs: 0.0,
            last_active_at: None,
            fps: nominal_fps(cpu_hz),
        }
    }

    /// Retune to a machine clocked at `cpu_hz` — cheap and idempotent, so the
    /// tick loop can call it unconditionally.
    pub fn set_cpu_hz(&mut self, cpu_hz: u32) {
        self.fps = nominal_fps(cpu_hz);
    }

    /// The frame rate this meter grades against.
    pub fn nominal_fps(&self) -> f64 {
        self.fps
    }

    /// Record one tick at time `now` (seconds, same clock as the regulator)
    /// on which `emulated` frames advanced. Call every tick, including idle
    /// ones.
    pub fn record(&mut self, now: f64, emulated: u32) {
        self.emulated += u64::from(emulated);
        if self.origin.is_none() && emulated > 0 {
            self.origin = Some(now);
        }
        if emulated > 0 {
            // A discontinuity (pause, pre-ROM idle) starts a fresh
            // measurement epoch: measuring the *current* rate through a window
            // containing idle time would under-read it — see
            // METER_GAP_RESET_SECS.
            if self
                .last_active_at
                .is_none_or(|t| now - t > METER_GAP_RESET_SECS)
            {
                self.samples.clear();
                self.ema = None;
                if let Some(t) = self.last_active_at {
                    self.idle_secs += now - t;
                }
            }
            self.last_active_at = Some(now);
        } else if self.last_active_at.is_none() {
            // Nothing has ever run: nothing to measure yet.
            return;
        }
        match self.samples.back() {
            Some(&(t, _)) if now - t < METER_SAMPLE_SECS => {}
            _ => {
                self.samples.push_back((now, self.emulated));
                // Keep one sample beyond the window so Δtime spans ≥ the window.
                while self
                    .samples
                    .front()
                    .is_some_and(|&(t, _)| now - t > METER_WINDOW_SECS + METER_SAMPLE_SECS)
                {
                    self.samples.pop_front();
                }
                if let (Some(&(t0, f0)), Some(&(t1, f1))) =
                    (self.samples.front(), self.samples.back())
                    && t1 > t0
                {
                    let rate = (f1 - f0) as f64 / (t1 - t0);
                    let alpha = (METER_SAMPLE_SECS / METER_EMA_TAU_SECS).min(1.0);
                    self.ema = Some(match self.ema {
                        Some(prev) => prev + alpha * (rate - prev),
                        None => rate,
                    });
                }
            }
        }
    }

    /// The smoothed game-speed readout in frames per second (0.0 until enough
    /// samples). See the type doc for why this is emulated rate, not
    /// distinct-frames-presented.
    pub fn fps(&self) -> f64 {
        self.ema.unwrap_or(0.0)
    }

    /// Cumulative *game-speed* drift in frames versus a perfect
    /// [`NOMINAL_FPS`] timeline since the first emulated frame. In lock this
    /// oscillates within a couple of frames and mean-reverts; a walk means
    /// the rate is wrong — the fps readout can flatter, this counter cannot.
    pub fn drift_frames(&self, now: f64) -> f64 {
        match self.origin {
            Some(t0) => {
                // Exclude pause/menu gaps (and the current one, if inside it)
                // from the baseline: paused time is not owed frames.
                let idle = self.idle_secs
                    + self
                        .last_active_at
                        .map_or(0.0, |t| (now - t - METER_GAP_RESET_SECS).max(0.0));
                self.emulated as f64 - (now - t0 - idle) * self.fps
            }
            None => 0.0,
        }
    }

    /// Total emulated frames recorded (diagnostics).
    pub fn total_frames(&self) -> u64 {
        self.emulated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the regulator over simulated ticks. `tick_hz` is the platform
    /// cadence; `jitter` adds a deterministic ±bound wobble to each tick;
    /// `backlog` models the audio ring from actual production/consumption.
    /// The DMG-rate clock most of these tests run at.
    const DMG_HZ: u32 = 4_194_304;
    /// The NTSC SGB1: the host SNES's 21.477270 MHz / 5, ~2.4% fast.
    const SGB_NTSC_HZ: u32 = 4_295_454;
    /// The PAL SGB1: the host SNES's 21.281370 MHz / 5, ~1.5% fast.
    const SGB_PAL_HZ: u32 = 4_256_274;

    struct Sim {
        reg: Regulator,
        now: f64,
        /// Ring depth in pairs, or None for no audio.
        backlog_pairs: Option<f64>,
        /// Device consumption in pairs/second (44100 nominal).
        consume_rate: f64,
        frames: u64,
        /// The machine being paced — every rate assertion is relative to this,
        /// so the same simulation grades a DMG and an SGB1 correctly.
        cpu_hz: u32,
    }

    impl Sim {
        fn new(audio: bool) -> Self {
            Self::for_cpu_hz(audio, DMG_HZ)
        }

        fn for_cpu_hz(audio: bool, cpu_hz: u32) -> Self {
            Sim {
                reg: Regulator::for_cpu_hz(cpu_hz),
                now: 0.0,
                // Primed ring.
                backlog_pairs: audio.then_some(8.0 * samples_per_frame(cpu_hz)),
                consume_rate: 44_100.0,
                frames: 0,
                cpu_hz,
            }
        }

        /// The rate this sim's machine should lock to.
        fn target_fps(&self) -> f64 {
            nominal_fps(self.cpu_hz)
        }

        fn samples_per_frame(&self) -> f64 {
            samples_per_frame(self.cpu_hz)
        }

        /// One tick after `dt` seconds; returns frames run.
        fn tick(&mut self, dt: f64) -> u32 {
            self.now += dt;
            // Device consumes continuously.
            if let Some(b) = self.backlog_pairs.as_mut() {
                *b = (*b - self.consume_rate * dt).max(0.0);
            }
            let n = self.reg.frames_to_run(
                self.now,
                self.backlog_pairs.map(|b| b as usize),
                false,
                false,
            );
            if let Some(b) = self.backlog_pairs.as_mut() {
                // Production reaches the ring through the stretcher.
                *b += f64::from(n) * samples_per_frame(self.cpu_hz) * self.reg.audio_stretch();
            }
            self.frames += u64::from(n);
            n
        }

        fn run(&mut self, seconds: f64, tick_hz: f64, jitter: f64) {
            let period = 1.0 / tick_hz;
            let end = self.now + seconds;
            let mut phase = 0u32;
            while self.now < end {
                // Deterministic triangle-wave jitter in ±jitter.
                let j = jitter * (f64::from(phase % 7) / 3.0 - 1.0);
                phase = phase.wrapping_add(1);
                self.tick((period + j).max(0.0005));
            }
        }
    }

    /// Long-run rate locks to NOMINAL_FPS across tick cadences and jitter.
    /// Measured over the steady-state window: the first seconds legitimately
    /// run a few frames short while the regulator drains the ring's silent
    /// startup prime down to its target — a one-time offset, not a rate error
    /// (the drift test pins that separately).
    #[test]
    fn locks_at_any_tick_cadence() {
        for &(hz, jitter) in &[(60.0, 0.0), (60.0, 0.003), (120.0, 0.002), (144.0, 0.001), (30.0, 0.003)] {
            let mut sim = Sim::new(true);
            sim.run(15.0, hz, jitter); // warm-up: prime drain + trim settle
            let (t0, f0) = (sim.now, sim.frames);
            sim.run(60.0, hz, jitter);
            let rate = (sim.frames - f0) as f64 / (sim.now - t0);
            let err = (rate - sim.target_fps()).abs() / sim.target_fps();
            assert!(
                err < 0.0005,
                "cadence {hz}Hz jitter {jitter}: rate {rate:.4} vs {:.4} (err {err:.5})", sim.target_fps()
            );
        }
    }

    /// No audio device: pure wall clock still locks (the original macOS bug).
    #[test]
    fn locks_without_audio() {
        let mut sim = Sim::new(false);
        // Simulate macOS-style overshooting ticks: nominal 60Hz but each tick
        // late by 1-3ms (the sleep-overshoot pattern that caused 56.7fps).
        sim.run(60.0, 60.0, 0.0);
        let mut late = Sim::new(false);
        let mut phase = 0u32;
        while late.now < 60.0 {
            let overshoot = 0.001 + 0.002 * f64::from(phase % 3) / 2.0;
            phase += 1;
            late.tick(1.0 / 60.0 + overshoot);
        }
        for s in [&sim, &late] {
            let rate = s.frames as f64 / s.now;
            let err = (rate - s.target_fps()).abs() / s.target_fps();
            assert!(err < 0.0005, "no-audio rate {rate:.4} (err {err:.5})");
        }
    }

    /// A host consumption surge (startup pipeline fill / re-quantum) may trim
    /// the rate up slightly but can never command a fast-forward: bounded by
    /// the emergency trim + one bucket, never above 66 frames in any second.
    #[test]
    fn consumption_surge_cannot_fast_forward_the_game() {
        let mut sim = Sim::new(true);
        sim.run(5.0, 60.0, 0.0);
        // Surge: device consumes 3x for 2 seconds (far beyond any real DAC).
        sim.consume_rate = 3.0 * 44_100.0;
        let mut per_second_max = 0u64;
        for _ in 0..2 {
            let before = sim.frames;
            sim.run(1.0, 60.0, 0.0);
            per_second_max = per_second_max.max(sim.frames - before);
        }
        assert!(
            per_second_max <= 66,
            "surge second ran {per_second_max} frames — the game fast-forwarded"
        );
    }

    /// Startup: the first seconds never run visibly fast (old failure: ~+60
    /// frames in 3s while the pipeline filled).
    #[test]
    fn startup_is_not_fast() {
        let mut sim = Sim::new(true);
        // Model the pipeline fill: 2x consumption for the first 0.5s.
        sim.consume_rate = 2.0 * 44_100.0;
        sim.run(0.5, 60.0, 0.0);
        sim.consume_rate = 44_100.0;
        sim.run(4.5, 60.0, 0.0);
        let budget = (5.0 * sim.target_fps() + BUCKET_CAP + 2.0) as u64;
        assert!(
            sim.frames <= budget,
            "first 5s ran {} frames (budget {budget})",
            sim.frames
        );
    }

    /// A stall (dropped ticks) is repaid from the bank — bounded — and the
    /// long-run rate still locks.
    #[test]
    fn stall_recovery_is_bounded() {
        let mut sim = Sim::new(true);
        sim.run(5.0, 60.0, 0.0);
        // 100ms stall: no ticks, device keeps consuming.
        sim.now += 0.1;
        if let Some(b) = sim.backlog_pairs.as_mut() {
            *b = (*b - 44_100.0 * 0.1).max(0.0);
        }
        let n = sim.tick(1.0 / 60.0);
        assert!(n <= 4, "single tick ran {n} > MAX_PER_TICK");
        // Recovery refills the ring above target; the trim then drains it back
        // at ≤0.5% slow (intentional, imperceptible) — the trough statistic
        // lags two windows, so give the drain ~25s. Measure steady rate after.
        sim.run(25.0, 60.0, 0.0);
        let (t0, f0) = (sim.now, sim.frames);
        sim.run(15.0, 60.0, 0.0);
        let rate = (sim.frames - f0) as f64 / (sim.now - t0);
        let err = (rate - sim.target_fps()).abs() / sim.target_fps();
        assert!(err < 0.001, "post-stall rate {rate:.4}");
    }

    /// Pausing banks nothing: resume never bursts.
    #[test]
    fn unpause_does_not_burst() {
        let mut reg = Regulator::new();
        let mut now = 0.0;
        for _ in 0..60 {
            now += 1.0 / 60.0;
            reg.frames_to_run(now, None, false, false);
        }
        // 10 seconds paused.
        for _ in 0..600 {
            now += 1.0 / 60.0;
            assert_eq!(reg.frames_to_run(now, None, false, true), 0);
        }
        now += 1.0 / 60.0;
        let n = reg.frames_to_run(now, None, false, false);
        assert!(n <= 2, "unpause burst of {n} frames");
    }

    /// Fast-forward always reports one batch and leaves no banked burst.
    #[test]
    fn fast_forward_bypasses_and_exits_clean() {
        let mut reg = Regulator::new();
        let mut now = 0.0;
        for _ in 0..300 {
            now += 1.0 / 60.0;
            assert_eq!(reg.frames_to_run(now, Some(44_100), true, false), 1);
        }
        now += 1.0 / 60.0;
        let n = reg.frames_to_run(now, Some(44_100), false, false);
        assert!(n <= 2, "FF exit burst of {n} frames");
    }

    /// A bursty pull cadence with a HEALTHY average (macOS App-Nap gulps
    /// ~300ms at a time; total consumption still exactly 44100/s) must not
    /// bias game speed: an unfiltered trim rectified this oscillation into a
    /// measured +0.64% real-world drift walk.
    #[test]
    fn bursty_consumption_does_not_bias_speed() {
        let mut sim = Sim::new(true);
        sim.run(15.0, 60.0, 0.0); // settle
        let (t0, f0) = (sim.now, sim.frames);
        // 45 seconds of gulped consumption: nothing for 300ms, then the whole
        // 300ms worth at once (device average stays exactly 44100/s).
        let period = 1.0 / 60.0;
        let mut since_gulp = 0.0;
        while sim.now < t0 + 45.0 {
            // Manually advance without continuous consumption:
            sim.now += period;
            since_gulp += period;
            if since_gulp >= 0.3 {
                if let Some(b) = sim.backlog_pairs.as_mut() {
                    *b = (*b - 44_100.0 * since_gulp).max(0.0);
                }
                since_gulp = 0.0;
            }
            let n = sim.reg.frames_to_run(
                sim.now,
                sim.backlog_pairs.map(|b| b as usize),
                false,
                false,
            );
            let per_frame = sim.samples_per_frame();
            let stretch = sim.reg.audio_stretch();
            if let Some(b) = sim.backlog_pairs.as_mut() {
                // Ring cap: pushes beyond 32 frames drop (as the backend does).
                *b = (*b + f64::from(n) * per_frame * stretch).min(32.0 * per_frame);
            }
            sim.frames += u64::from(n);
        }
        let rate = (sim.frames - f0) as f64 / (sim.now - t0);
        let err = (rate - sim.target_fps()).abs() / sim.target_fps();
        assert!(
            err < 0.002,
            "bursty consumption biased the rate to {rate:.4} (err {err:.5})"
        );
    }

    /// A slow/overfull host must not slow the game AT ALL: even with a full
    /// ring, production continues at exactly the nominal rate — the stretch
    /// squeezes audio (and beyond its authority the ring drops), the game
    /// keeps its timeline.
    #[test]
    fn full_ring_never_slows_production() {
        let mut reg = Regulator::new();
        let full = (32.0 * SAMPLES_PER_FRAME_F64) as usize;
        let mut now = 0.0;
        let mut frames = 0u64;
        for _ in 0..600 {
            now += 1.0 / 60.0;
            frames += u64::from(reg.frames_to_run(now, Some(full), false, false));
        }
        let rate = frames as f64 / 10.0;
        assert!(
            (rate - NOMINAL_FPS).abs() < 0.2,
            "overfull-ring rate {rate:.2}, expected ~{NOMINAL_FPS:.2}"
        );
        assert!(reg.audio_stretch() < 1.0, "stretch should squeeze when overfull");
    }

    /// The meter must read the true rate almost immediately — idle time before
    /// the first frame (app open, no ROM) must not dilute the window into a
    /// seconds-long climb from 0 (user-reported: "starts at 0 and makes its
    /// way up near 60 over several seconds — that obscures honesty").
    #[test]
    fn meter_reads_true_rate_promptly_after_idle_start() {
        let mut meter = RateMeter::new();
        let period = 1.0 / 60.0;
        let mut now = 0.0;
        // 5 seconds idle at the menu: ticks with no frames.
        while now < 5.0 {
            now += period;
            meter.record(now, 0);
        }
        // ROM loads; frames flow at the nominal cadence.
        let start = now;
        while now < start + 1.2 {
            now += period;
            meter.record(now, 1);
        }
        let fps = meter.fps();
        assert!(
            (fps - NOMINAL_FPS).abs() < 1.0,
            "meter read {fps:.2} at 1.2s after frames started (expected ~{NOMINAL_FPS:.1})"
        );
    }

    /// Same after a pause — including one SHORTER than the window (a quick
    /// menu visit): the gap resets the measurement epoch instead of dragging
    /// the post-resume reading down for seconds (user-reported: 5-8s recovery
    /// after a brief menu while the screen was visibly at speed immediately).
    /// The vsync beat's single skipped tick must NOT reset (it's normal play).
    #[test]
    fn meter_recovers_promptly_after_pause() {
        for pause_secs in [1.0, 10.0] {
            let mut meter = RateMeter::new();
            let period = 1.0 / 60.0;
            let mut now = 0.0;
            while now < 5.0 {
                now += period;
                meter.record(now, 1);
            }
            let pause_end = now + pause_secs;
            while now < pause_end {
                now += period;
                meter.record(now, 0);
            }
            let resume = now;
            while now < resume + 1.2 {
                now += period;
                meter.record(now, 1);
            }
            let fps = meter.fps();
            assert!(
                (fps - NOMINAL_FPS).abs() < 1.0,
                "meter read {fps:.2} at 1.2s after a {pause_secs}s pause (expected ~{NOMINAL_FPS:.1})"
            );
        }
    }

    /// THE Android regression: a device whose audio clock runs −0.4% slow
    /// must NOT drag the game under the GB's true rate (an earlier rate-trim
    /// design measurably locked such a device at 59.5 fps). The stretch
    /// converges to the device clock while the game stays at NOMINAL_FPS.
    #[test]
    fn offset_device_clock_bends_pitch_not_time() {
        let mut sim = Sim::new(true);
        sim.consume_rate = 44_100.0 * 0.996; // −0.4% device clock
        sim.run(20.0, 60.0, 0.0); // settle
        let (t0, f0) = (sim.now, sim.frames);
        sim.run(60.0, 60.0, 0.0);
        let rate = (sim.frames - f0) as f64 / (sim.now - t0);
        let err = (rate - sim.target_fps()).abs() / sim.target_fps();
        assert!(
            err < 0.0005,
            "game rate {rate:.4} bent by the device clock (err {err:.5})"
        );
        let stretch = sim.reg.audio_stretch();
        assert!(
            (stretch - 0.996).abs() < 0.002,
            "stretch {stretch:.4} should track the device clock (~0.996)"
        );
    }

    /// The stretcher preserves duration-at-ratio and stays continuous across
    /// arbitrary push boundaries.
    #[test]
    fn stretcher_output_counts_and_continuity() {
        let mut st = Stretcher::new();
        // A slow ramp so interpolation discontinuities would be visible.
        let input: Vec<(f32, f32)> = (0..2000).map(|i| (i as f32, -(i as f32))).collect();
        let ratio = 0.996;
        let mut out: Vec<(f32, f32)> = Vec::new();
        for chunk in input.chunks(738) {
            out.extend_from_slice(st.process(chunk, ratio));
        }
        let expected = (input.len() as f64 * ratio) as isize;
        assert!(
            (out.len() as isize - expected).abs() <= 2,
            "output {} pairs, expected ~{expected}",
            out.len()
        );
        // Continuity: the resampled ramp must still be monotonic with ~unit steps.
        for w in out.windows(2).skip(1) {
            let d = w[1].0 - w[0].0;
            assert!(
                (0.0..=2.0).contains(&d),
                "discontinuity in stretched output: step {d}"
            );
        }
        // Ratio 1.0 with no prior state is a pure pass-through.
        let mut clean = Stretcher::new();
        let same = clean.process(&input, 1.0);
        assert_eq!(same.len(), input.len());
    }

    /// Pause/menu time is not owed frames: drift must NOT walk negative
    /// across idle gaps (a real session accumulated −847 "drift" that was
    /// just menu time, burying the lock signal).
    #[test]
    fn drift_baseline_excludes_pauses() {
        let mut meter = RateMeter::new();
        let period = 1.0 / 60.0;
        let mut now = 0.0;
        for _ in 0..3 {
            let seg_end = now + 10.0;
            while now < seg_end {
                now += period;
                meter.record(now, 1);
            }
            let pause_end = now + 20.0;
            while now < pause_end {
                now += period;
                meter.record(now, 0);
            }
        }
        let drift = meter.drift_frames(now);
        assert!(
            drift.abs() < 4.0,
            "drift walked to {drift:.1} across pauses (pause time counted as owed frames)"
        );
    }

    /// The inherent one-tick skip (60Hz vsync beat, every ~3.7s) must neither
    /// reset the epoch nor visibly dent the smoothed readout.
    #[test]
    fn meter_rides_through_the_vsync_beat() {
        let mut meter = RateMeter::new();
        let period = 1.0 / 60.0;
        let mut now = 0.0;
        let mut tick = 0u64;
        while now < 30.0 {
            now += period;
            tick += 1;
            let skip = tick.is_multiple_of(220); // one dup tick per ~3.67s
            meter.record(now, u32::from(!skip));
        }
        let fps = meter.fps();
        assert!(
            (fps - NOMINAL_FPS).abs() < 0.4,
            "beat-skips dented the readout to {fps:.2}"
        );
    }

    /// The meter reads a flat NOMINAL_FPS under lock and its drift counter
    /// mean-reverts instead of walking.
    #[test]
    fn meter_reads_flat_and_drift_is_bounded() {
        let mut sim = Sim::new(true);
        let mut meter = RateMeter::new();
        let period = 1.0 / 60.0;
        while sim.now < 30.0 {
            let n = sim.tick(period);
            // Each emulating tick presents one new frame regardless of n.
            meter.record(sim.now, n);
        }
        let fps = meter.fps();
        assert!(
            (fps - NOMINAL_FPS).abs() < 0.15,
            "meter reads {fps:.2}, expected ~{NOMINAL_FPS:.2}"
        );
        let drift = meter.drift_frames(sim.now);
        assert!(drift.abs() < 4.0, "drift walked to {drift:.2} frames");
    }

    // --- the SGB1 clock model ------------------------------------------------

    /// **THE identity that keeps the two coupled sites in balance.** The core
    /// emits `70224 / (cpu_hz/44100)` sample pairs per frame and the regulator
    /// presents `cpu_hz / 70224` frames per second. Their product must be
    /// exactly 44 100 pairs/second on EVERY model — that is what lets an SGB1
    /// be pitched up 2.4% while the host DAC still receives its true rate.
    /// Change the pitch site without the cadence site (or vice versa) and this
    /// fails.
    #[test]
    fn host_sample_rate_is_44100_on_every_model() {
        for (name, hz) in [
            ("DMG", DMG_HZ),
            ("SGB1 NTSC", SGB_NTSC_HZ),
            ("SGB1 PAL", SGB_PAL_HZ),
            ("SGB2", DMG_HZ),
            ("CGB", DMG_HZ),
        ] {
            // Exactly the core's `generate_samples` ratio.
            let cycles_per_sample = f64::from(hz) / HOST_SAMPLE_RATE;
            let pairs_per_frame = DOTS_PER_FRAME / cycles_per_sample;
            let pairs_per_second = pairs_per_frame * nominal_fps(hz);
            assert!(
                (pairs_per_second - HOST_SAMPLE_RATE).abs() < 1e-6,
                "{name}: host output {pairs_per_second:.4} Hz, must be 44100"
            );
            // And the session's own helper must agree with that pairs/frame.
            assert!((samples_per_frame(hz) - pairs_per_frame).abs() < 1e-9, "{name}");
        }
    }

    /// The frame cadence each model presents at. An SGB1 on a 60 Hz display
    /// runs ~61.17 fps — more frames than the panel can show — which is exactly
    /// where its characteristic periodic stutter comes from.
    #[test]
    fn nominal_fps_per_model() {
        let fps = |hz| (nominal_fps(hz) * 100.0).round() / 100.0;
        assert_eq!(fps(SGB_NTSC_HZ), 61.17);
        assert_eq!(fps(SGB_PAL_HZ), 60.61);
        // SGB2 and DMG are the same machine rate — the SGB2's own crystal is
        // the entire reason it exists.
        assert_eq!(fps(DMG_HZ), 59.73);
        assert!((nominal_fps(DMG_HZ) - NOMINAL_FPS).abs() < 1e-12);
        // Ordering: NTSC SGB1 > PAL SGB1 > DMG-rate.
        assert!(nominal_fps(SGB_NTSC_HZ) > nominal_fps(SGB_PAL_HZ));
        assert!(nominal_fps(SGB_PAL_HZ) > nominal_fps(DMG_HZ));
    }

    /// The regulator actually paces to the model's rate, not a hardcoded 59.73
    /// — the whole point of Phase 5. Same lock quality as the DMG path.
    #[test]
    fn regulator_locks_to_each_models_rate() {
        for (name, hz) in [("DMG", DMG_HZ), ("SGB1 NTSC", SGB_NTSC_HZ), ("SGB1 PAL", SGB_PAL_HZ)] {
            let mut sim = Sim::for_cpu_hz(true, hz);
            sim.run(15.0, 60.0, 0.0); // prime drain + trim settle
            let (t0, f0) = (sim.now, sim.frames);
            sim.run(60.0, 60.0, 0.0);
            let rate = (sim.frames - f0) as f64 / (sim.now - t0);
            let err = (rate - sim.target_fps()).abs() / sim.target_fps();
            assert!(
                err < 0.0005,
                "{name}: paced {rate:.4} fps, expected {:.4} (err {err:.5})",
                sim.target_fps()
            );
        }
    }

    /// An SGB1 must genuinely outrun a DMG in wall-clock frames — a regression
    /// here (e.g. the regulator quietly falling back to the const) would leave
    /// the SGB1 running at DMG speed with no other symptom.
    #[test]
    fn sgb1_outruns_a_dmg_over_the_same_wall_clock() {
        let frames_in_60s = |hz| {
            let mut sim = Sim::for_cpu_hz(true, hz);
            sim.run(15.0, 60.0, 0.0);
            let f0 = sim.frames;
            sim.run(60.0, 60.0, 0.0);
            sim.frames - f0
        };
        let dmg = frames_in_60s(DMG_HZ);
        let sgb = frames_in_60s(SGB_NTSC_HZ);
        assert!(sgb > dmg, "SGB1 ran {sgb} frames vs DMG {dmg} in the same minute");
        let pct = (sgb as f64 / dmg as f64 - 1.0) * 100.0;
        assert!((pct - 2.4).abs() < 0.2, "SGB1 ran {pct:.2}% fast, expected ~2.4%");
    }

    /// `set_cpu_hz` retunes a live regulator (the platform tick calls it every
    /// frame, so switching model or region mid-session must take effect).
    #[test]
    fn set_cpu_hz_retunes_a_live_regulator() {
        let mut reg = Regulator::new();
        assert!((reg.nominal_fps() - NOMINAL_FPS).abs() < 1e-12);
        reg.set_cpu_hz(SGB_NTSC_HZ);
        assert!((reg.nominal_fps() - nominal_fps(SGB_NTSC_HZ)).abs() < 1e-12);
        // Idempotent: re-setting the same rate changes nothing.
        reg.set_cpu_hz(SGB_NTSC_HZ);
        assert!((reg.nominal_fps() - nominal_fps(SGB_NTSC_HZ)).abs() < 1e-12);
        reg.set_cpu_hz(DMG_HZ);
        assert!((reg.nominal_fps() - NOMINAL_FPS).abs() < 1e-12);
    }

    /// The drift counter must grade against the machine's own rate: an SGB1
    /// locked at its true 61.17 fps is NOT drifting, and reporting it as such
    /// would bury the signal the counter exists for.
    #[test]
    fn meter_grades_an_sgb1_against_its_own_rate() {
        let mut meter = RateMeter::for_cpu_hz(SGB_NTSC_HZ);
        let period = 1.0 / nominal_fps(SGB_NTSC_HZ);
        let mut now = 0.0;
        for _ in 0..(nominal_fps(SGB_NTSC_HZ) * 30.0) as u32 {
            now += period;
            meter.record(now, 1);
        }
        let fps = meter.fps();
        assert!(
            (fps - nominal_fps(SGB_NTSC_HZ)).abs() < 0.2,
            "meter read {fps:.2} for an SGB1, expected ~61.17"
        );
        assert!(meter.drift_frames(now).abs() < 4.0, "a locked SGB1 read as drifting");

        // The same stream graded as a DMG would look badly fast — proof the
        // rate-awareness is load-bearing and not cosmetic.
        let mut dmg_meter = RateMeter::new();
        let mut now = 0.0;
        for _ in 0..(nominal_fps(SGB_NTSC_HZ) * 30.0) as u32 {
            now += period;
            dmg_meter.record(now, 1);
        }
        assert!(
            dmg_meter.drift_frames(now) > 30.0,
            "DMG-graded SGB1 stream should show a large positive drift"
        );
    }
}

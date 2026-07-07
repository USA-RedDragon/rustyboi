//! WebAudio output: a scheduled `AudioBufferSourceNode` ring.
//!
//! The core emits stereo `f32` samples at a fixed 44100 Hz (see
//! `audio/controller.rs` `CYCLES_PER_SAMPLE`). WebAudio's `AudioContext`
//! typically runs at 48000 Hz, but an `AudioBuffer` may declare *any* sample
//! rate and the graph resamples it on playback — so we build 44100 Hz buffers
//! directly and let the browser resample. No manual rate conversion.
//!
//! # Scheduling (glitch-tolerant queued buffers)
//!
//! Per presented frame we get ~735 stereo samples. We copy them into a fresh
//! two-channel `AudioBuffer`, wrap it in a one-shot `AudioBufferSourceNode`,
//! and `start()` it at a running `next_time` cursor. Each buffer's duration is
//! added to the cursor so consecutive frames play gaplessly.
//!
//! To stay robust to `requestAnimationFrame` jitter (tab throttling, GC
//! pauses) we keep a small latency cushion ahead of `currentTime`. If the
//! cursor ever falls behind the clock (we starved), we resync it to
//! `currentTime + cushion` — a brief silence instead of a hard glitch. This is
//! an AudioWorklet-free path: simpler, universally supported in Firefox, and
//! good enough at GB frame cadence. An AudioWorklet with a shared ring is the
//! natural future upgrade for lower, steadier latency.

use wasm_bindgen::prelude::*;
use web_sys::{AudioContext, AudioContextState, GainNode};

/// Native sample rate of the core's audio output.
const CORE_SAMPLE_RATE: f32 = 44100.0;

/// Seconds of scheduling cushion kept ahead of the audio clock. ~2 frames of
/// GB audio; trades a little latency for jitter tolerance.
const CUSHION_SECS: f64 = 0.033;

/// If the scheduling cursor drifts more than this far ahead of the clock we're
/// buffering too much (e.g. after fast-forward) — clamp back down.
const MAX_AHEAD_SECS: f64 = 0.20;

pub struct AudioPlayer {
    ctx: AudioContext,
    gain: GainNode,
    /// Absolute `AudioContext` time at which the next buffer should start.
    next_time: f64,
}

impl AudioPlayer {
    /// Create the context and a master gain node. The context often starts
    /// `suspended` until a user gesture; [`AudioPlayer::resume`] is called from
    /// the ROM-load click to unlock it.
    pub fn new() -> Result<AudioPlayer, JsValue> {
        let ctx = AudioContext::new()?;
        let gain = ctx.create_gain()?;
        gain.gain().set_value(0.35);
        gain.connect_with_audio_node(&ctx.destination())?;
        Ok(AudioPlayer { ctx, gain, next_time: 0.0 })
    }

    /// Resume the context (must run inside a user-gesture handler in Firefox).
    pub fn resume(&self) {
        if self.ctx.state() == AudioContextState::Suspended {
            let _ = self.ctx.resume();
        }
    }

    /// Queue one frame's worth of interleaved stereo samples for playback.
    pub fn queue(&mut self, samples: &[(f32, f32)]) {
        if samples.is_empty() {
            return;
        }
        let n = samples.len() as u32;
        let buffer = match self
            .ctx
            .create_buffer(2, n, CORE_SAMPLE_RATE)
        {
            Ok(b) => b,
            Err(_) => return,
        };

        let mut left = Vec::with_capacity(samples.len());
        let mut right = Vec::with_capacity(samples.len());
        for &(l, r) in samples {
            left.push(l);
            right.push(r);
        }
        if buffer.copy_to_channel(&mut left, 0).is_err() {
            return;
        }
        if buffer.copy_to_channel(&mut right, 1).is_err() {
            return;
        }

        let src = match self.ctx.create_buffer_source() {
            Ok(s) => s,
            Err(_) => return,
        };
        src.set_buffer(Some(&buffer));
        if src.connect_with_audio_node(&self.gain).is_err() {
            return;
        }

        let now = self.ctx.current_time();
        // Resync if we've starved (cursor behind the clock) or drifted too far
        // ahead (over-buffered, e.g. leaving fast-forward).
        if self.next_time < now + 0.001 || self.next_time > now + MAX_AHEAD_SECS {
            self.next_time = now + CUSHION_SECS;
        }

        let _ = src.start_with_when(self.next_time);
        self.next_time += n as f64 / CORE_SAMPLE_RATE as f64;
    }
}

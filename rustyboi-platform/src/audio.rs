//! Platform audio output.
//!
//! Two backends behind one `Output` API (`start_device` / `push_samples` /
//! `queued_pairs` / `consumed_pairs`), both built on the same lock-free SPSC
//! ring so the backlog signal is sample-accurate — it is the DAC-trim input to
//! the shared pacing regulator (`rustyboi_session::pacing`):
//!
//! - **Android** → oboe, requesting AAudio's LOW_LATENCY / MMAP path (which cpal
//!   never asks for, pinning it to the higher-latency Legacy path); its
//!   real-time callback drains the ring.
//! - **Everything else** (desktop native + iOS) → cpal directly, stream opened
//!   at the core's 44100Hz (the host resamples to the device rate); its
//!   callback drains the ring.

#[cfg(not(target_os = "android"))]
pub(crate) use cpal_backend::Output;

#[cfg(target_os = "android")]
pub use oboe_backend::Output;

/// Stereo sample rate the core emits at (see `audio/controller.rs`).
const SAMPLE_RATE: u32 = 44100;

#[cfg(not(target_os = "android"))]
mod cpal_backend {
    use super::SAMPLE_RATE;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use ringbuf::{
        traits::{Consumer, Observer, Producer, Split},
        HeapProd, HeapRb,
    };
    use rustyboi_core_lib::audio::AudioOutput;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// Stereo sample *pairs* per emulated frame (44100 / 59.7275 fps), rounded.
    /// Only sizes the ring and the startup pre-fill — the pacing regulator does
    /// its frame math with the exact fractional constant (`pacing.rs`).
    const SAMPLES_PER_FRAME: usize = 738;
    /// Playback gain, applied on push (same as the Android backend's VOLUME).
    const VOLUME: f32 = 0.3;
    /// Ring capacity in frames. Generous — the pacing regulator holds the fill
    /// near its target, so this is just a ceiling that bounds latency (the
    /// excess is dropped on push).
    const RING_FRAMES: usize = 32;
    /// Frames of silence pre-pushed at device start. The host audio pipeline
    /// fills its buffers by consuming fast for its first ~100-200ms; pre-filled
    /// silence absorbs that one-time surge so it drains the ring instead of
    /// demanding emulated frames, and whatever the pipeline doesn't eat plays
    /// out inaudibly while the regulator's backlog trim settles the ring.
    const PRIME_SILENCE_FRAMES: usize = 12;

    /// Desktop/iOS output: a cpal stream opened AT THE CORE'S 44100Hz whose
    /// callback drains the SPSC ring 1:1, zero-filling on underrun — exactly
    /// the Android oboe model. The host does any device-rate conversion
    /// (PipeWire/CoreAudio natively, WASAPI via AUTOCONVERTPCM), so ring
    /// consumption is precisely 44100 pairs/s by the host audio clock: the
    /// honest, sample-accurate signal the pacing regulator trims against.
    /// (The previous rodio sink's mixer/converter stack consumed the ring at
    /// the device rate when its span bootstrap mis-captured the source rate,
    /// and leaked samples on every span re-bootstrap — 10-20%% zero-fill.)
    pub(crate) struct Output {
        stream: Option<cpal::Stream>,
        prod: Option<HeapProd<f32>>,
        scratch: Vec<f32>,
        /// Raw f32 samples the callback zero-filled because the ring was empty.
        underrun_samples: Arc<AtomicU64>,
    }

    impl Output {
        pub(crate) fn new() -> Result<Self, Box<dyn std::error::Error>> {
            Ok(Output {
                stream: None,
                prod: None,
                scratch: Vec::new(),
                underrun_samples: Arc::new(AtomicU64::new(0)),
            })
        }

        pub(crate) fn start_device(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            <Self as AudioOutput>::start(self)
        }

        pub(crate) fn push_samples(&mut self, samples: &[(f32, f32)]) {
            <Self as AudioOutput>::add_samples(self, samples)
        }

        /// Backlog in stereo sample pairs — the sample-accurate signal the
        /// pacing regulator trims against.
        pub(crate) fn queued_pairs(&self) -> usize {
            self.prod.as_ref().map_or(0, |p| p.occupied_len() / 2)
        }
    }

    impl AudioOutput for Output {
        fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            let (mut prod, mut cons) = HeapRb::<f32>::new(RING_FRAMES * SAMPLES_PER_FRAME * 2).split();
            // Absorb the host pipeline's one-time startup fill with silence
            // (see PRIME_SILENCE_FRAMES) so it can't fast-forward the game.
            let silence = vec![0.0f32; PRIME_SILENCE_FRAMES * SAMPLES_PER_FRAME * 2];
            prod.push_slice(&silence);

            let host = cpal::default_host();
            let device = host
                .default_output_device()
                .ok_or("no default audio output device")?;
            let config = cpal::StreamConfig {
                channels: 2,
                sample_rate: SAMPLE_RATE,
                buffer_size: cpal::BufferSize::Default,
            };
            let underrun_samples = self.underrun_samples.clone();
            let stream = device.build_output_stream(
                config,
                move |data: &mut [f32], _| {
                    // Real-time callback: drain the ring, zero-fill the rest.
                    let got = cons.pop_slice(data);
                    if got < data.len() {
                        data[got..].fill(0.0);
                        underrun_samples.fetch_add((data.len() - got) as u64, Ordering::Relaxed);
                    }
                },
                |e| log::warn!("audio stream error: {e}"),
                None,
            )?;
            stream.play()?;
            log::info!("cpal stream opened at {SAMPLE_RATE}Hz stereo (host converts to device rate)");

            self.stream = Some(stream);
            self.prod = Some(prod);
            Ok(())
        }

        fn add_samples(&mut self, samples: &[(f32, f32)]) {
            let Some(prod) = self.prod.as_mut() else { return };
            if samples.is_empty() {
                return;
            }
            self.scratch.clear();
            self.scratch.reserve(samples.len() * 2);
            for &(left, right) in samples {
                self.scratch.push(left * VOLUME);
                self.scratch.push(right * VOLUME);
            }
            // A full ring (production outran the device) drops the excess,
            // bounding latency.
            prod.push_slice(&self.scratch);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ringbuf::{
            traits::{Consumer, Split},
            HeapCons, HeapRb,
        };

        /// Install a ring producer directly, bypassing `start()` (which opens a
        /// real cpal device). Returns the consumer end so tests can inspect what
        /// the callback would drain.
        fn wire_ring(out: &mut Output) -> HeapCons<f32> {
            let (prod, cons) = HeapRb::<f32>::new(RING_FRAMES * SAMPLES_PER_FRAME * 2).split();
            out.prod = Some(prod);
            cons
        }

        /// The regression guard: `add_samples` MUST enqueue into the ring. When
        /// the `push_slice` was trimmed, `scratch` filled but the ring stayed
        /// empty and the callback zero-filled forever (silent desktop audio).
        #[test]
        fn add_samples_feeds_the_ring() {
            let mut out = Output::new().unwrap();
            let mut cons = wire_ring(&mut out);

            out.push_samples(&[(1.0, -1.0), (0.5, -0.5)]);

            assert_eq!(out.queued_pairs(), 2, "samples must reach the ring");

            let mut drained = [0.0f32; 4];
            assert_eq!(cons.pop_slice(&mut drained), 4);
            // Interleaved L/R, scaled by VOLUME.
            assert_eq!(drained, [VOLUME, -VOLUME, 0.5 * VOLUME, -0.5 * VOLUME]);
        }

        #[test]
        fn add_samples_is_a_noop_before_the_device_starts() {
            // No ring wired yet: must not panic, must enqueue nothing.
            let mut out = Output::new().unwrap();
            out.push_samples(&[(1.0, -1.0)]);
            assert_eq!(out.queued_pairs(), 0);
        }

        #[test]
        fn empty_push_is_a_noop() {
            let mut out = Output::new().unwrap();
            let _cons = wire_ring(&mut out);
            out.push_samples(&[]);
            assert_eq!(out.queued_pairs(), 0);
        }
    }
}

#[cfg(target_os = "android")]
mod oboe_backend {
    use super::SAMPLE_RATE;
    use oboe::{
        AudioOutputCallback, AudioOutputStreamSafe, AudioStream, AudioStreamAsync,
        AudioStreamBase, AudioStreamBuilder, AudioStreamSafe, DataCallbackResult,
        Output as OboeOutput, PerformanceMode, SampleRateConversionQuality, SharingMode,
        Stereo, Usage,
    };
    use ringbuf::{
        traits::{Consumer, Observer, Producer, Split},
        HeapCons, HeapProd, HeapRb,
    };
    use rustyboi_core_lib::audio::AudioOutput;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;

    /// Stereo sample *pairs* per emulated frame (44100 / 59.7275 fps), rounded.
    /// Only sizes the ring and the startup pre-fill — the pacing regulator does
    /// its frame math with the exact fractional constant (`pacing.rs`).
    const SAMPLES_PER_FRAME: usize = 738;
    /// Playback gain, matching the desktop sink's `set_volume(0.3)`.
    const VOLUME: f32 = 0.3;
    /// Ring capacity in frames. Generous — the pacing regulator holds the fill
    /// near a small target, so this is just a ceiling that bounds latency.
    const RING_FRAMES: usize = 32;
    /// Frames of silence pre-pushed at stream start — same rationale as the
    /// desktop backend's constant of the same name: the host pipeline's one-time
    /// startup fill drains silence instead of demanding emulated frames.
    const PRIME_SILENCE_FRAMES: usize = 12;
    /// Log a backlog summary this often (~1s of audio).
    const REPORT_EVERY_FRAMES: u32 = 60;

    /// Real-time callback: drains the ring into oboe's output buffer, zero-filling
    /// on underrun. Must not allocate, lock, or block.
    struct Callback {
        cons: HeapCons<f32>,
        underruns: Arc<AtomicU32>,
        underrun_samples: Arc<AtomicU64>,
    }

    impl AudioOutputCallback for Callback {
        type FrameType = (f32, Stereo);

        fn on_audio_ready(
            &mut self,
            _stream: &mut dyn AudioOutputStreamSafe,
            frames: &mut [(f32, f32)],
        ) -> DataCallbackResult {
            let mut zero_filled: u64 = 0;
            for frame in frames.iter_mut() {
                let mut lr = [0.0f32; 2];
                if self.cons.pop_slice(&mut lr) == 2 {
                    *frame = (lr[0], lr[1]);
                } else {
                    *frame = (0.0, 0.0);
                    zero_filled += 2;
                }
            }
            if zero_filled > 0 {
                self.underruns.fetch_add(1, Ordering::Relaxed);
                self.underrun_samples.fetch_add(zero_filled, Ordering::Relaxed);
            }
            DataCallbackResult::Continue
        }
    }

    pub struct Output {
        stream: Option<AudioStreamAsync<OboeOutput, Callback>>,
        prod: Option<HeapProd<f32>>,
        underruns: Arc<AtomicU32>,
        underrun_samples: Arc<AtomicU64>,
        scratch: Vec<f32>,
        /// Counts appended frames to throttle the underrun check to ~once/second.
        report_frames: u32,
        /// Cumulative stereo pairs pushed (including the startup silence), for
        /// consumption diagnostics — same contract as the desktop backend.
        pushed_pairs: u64,
    }

    impl Output {
        pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
            Ok(Output {
                stream: None,
                prod: None,
                underruns: Arc::new(AtomicU32::new(0)),
                underrun_samples: Arc::new(AtomicU64::new(0)),
                scratch: Vec::new(),
                report_frames: 0,
                pushed_pairs: 0,
            })
        }

        pub fn start_device(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            <Self as AudioOutput>::start(self)
        }

        pub fn push_samples(&mut self, samples: &[(f32, f32)]) {
            <Self as AudioOutput>::add_samples(self, samples)
        }

        /// Backlog in stereo sample pairs — the sample-accurate signal the
        /// pacing regulator trims against.
        pub fn queued_pairs(&self) -> usize {
            self.prod.as_ref().map_or(0, |p| p.occupied_len() / 2)
        }

        /// Cumulative stereo pairs the device has *consumed* (pushed minus
        /// still-queued) — same contract as the desktop backend.
        pub fn consumed_pairs(&self) -> u64 {
            self.pushed_pairs.saturating_sub(self.queued_pairs() as u64)
        }

        /// Cumulative raw samples zero-filled on ring underrun — same contract
        /// as the desktop backend.
        pub fn underrun_samples(&self) -> u64 {
            self.underrun_samples.load(Ordering::Relaxed)
        }
    }

    impl AudioOutput for Output {
        fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            let (mut prod, cons) = HeapRb::<f32>::new(RING_FRAMES * SAMPLES_PER_FRAME * 2).split();
            let underruns = Arc::new(AtomicU32::new(0));
            // Absorb the host pipeline's one-time startup fill with silence
            // (see PRIME_SILENCE_FRAMES). The callback zero-fills on an empty
            // ring, so the stream can start immediately — no deferred-start
            // priming latch needed (same model as the desktop backend).
            let silence = vec![0.0f32; PRIME_SILENCE_FRAMES * SAMPLES_PER_FRAME * 2];
            prod.push_slice(&silence);
            self.pushed_pairs = (silence.len() / 2) as u64;

            // Request the AAudio low-latency (MMAP) path. We feed 44.1kHz and let
            // oboe resample to the device rate so the fast path is preserved.
            let mut stream = AudioStreamBuilder::default()
                .set_performance_mode(PerformanceMode::LowLatency)
                .set_sharing_mode(SharingMode::Shared)
                .set_usage(Usage::Game)
                .set_format::<f32>()
                .set_channel_count::<Stereo>()
                .set_sample_rate(SAMPLE_RATE as i32)
                .set_sample_rate_conversion_quality(SampleRateConversionQuality::Medium)
                .set_callback(Callback {
                    cons,
                    underruns: underruns.clone(),
                    underrun_samples: self.underrun_samples.clone(),
                })
                .open_stream()
                .map_err(|e| format!("oboe open_stream failed: {e:?}"))?;

            log::info!(
                "oboe stream opened: rate={}Hz burst={} frames, perf={:?}",
                stream.get_sample_rate(),
                stream.get_frames_per_burst(),
                stream.get_performance_mode(),
            );
            stream.start().map_err(|e| format!("oboe start failed: {e:?}"))?;

            self.stream = Some(stream);
            self.prod = Some(prod);
            self.underruns = underruns;
            Ok(())
        }

        fn add_samples(&mut self, samples: &[(f32, f32)]) {
            let Some(prod) = self.prod.as_mut() else { return };
            if samples.is_empty() {
                return;
            }
            self.scratch.clear();
            self.scratch.reserve(samples.len() * 2);
            for &(left, right) in samples {
                self.scratch.push(left * VOLUME);
                self.scratch.push(right * VOLUME);
            }
            // A full ring (production outran the device) drops the excess,
            // bounding latency.
            let pushed = prod.push_slice(&self.scratch);
            self.pushed_pairs += (pushed / 2) as u64;

            // Stay silent when healthy; surface underruns only if they happen
            // (e.g. a device that can't hold the MMAP low-latency path).
            self.report_frames += 1;
            if self.report_frames >= REPORT_EVERY_FRAMES {
                let underruns = self.underruns.swap(0, Ordering::Relaxed);
                if underruns > 0 {
                    log::warn!("audio: {underruns} underruns in last {} frames", self.report_frames);
                }
                self.report_frames = 0;
            }
        }
    }
}

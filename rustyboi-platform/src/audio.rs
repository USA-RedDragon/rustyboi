//! Platform audio output.
//!
//! Two backends behind one `Output` API (`start_device` / `push_samples` /
//! `queued_frames`):
//!
//! - **Android** → oboe, requesting AAudio's LOW_LATENCY / MMAP path (which cpal
//!   never asks for, pinning it to the higher-latency Legacy path). The emulator
//!   pushes samples into a lock-free SPSC ring that oboe's real-time callback
//!   drains. `queued_frames` reports the ring depth so the frame loop can pace
//!   off it (audio-clocked pacing).
//! - **Everything else** (desktop native) → rodio/cpal, which is already
//!   low-latency there.

#[cfg(not(target_os = "android"))]
pub use rodio_backend::Output;

#[cfg(target_os = "android")]
pub use oboe_backend::Output;

/// Stereo sample rate the core emits at (see `audio/controller.rs`).
const SAMPLE_RATE: u32 = 44100;

#[cfg(not(target_os = "android"))]
mod rodio_backend {
    use super::SAMPLE_RATE;
    use rodio::buffer::SamplesBuffer;
    use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};
    use rustyboi_core_lib::audio::AudioOutput;
    use std::num::NonZero;

    // One `SamplesBuffer` is appended per presented frame, so `Player::len()` is
    // the backlog in frames. Prime a small cushion before the first sound; the
    // frame loop's pacing keeps it healthy after that.
    const CUSHION_PRIME_FRAMES: usize = 3;
    const CUSHION_MAX_FRAMES: usize = 6;

    pub struct Output {
        _stream: Option<MixerDeviceSink>,
        sink: Option<Player>,
        playing: bool,
    }

    impl Output {
        pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
            Ok(Output { _stream: None, sink: None, playing: false })
        }

        pub fn start_device(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            <Self as AudioOutput>::start(self)
        }

        pub fn push_samples(&mut self, samples: &[(f32, f32)]) {
            <Self as AudioOutput>::add_samples(self, samples)
        }

        /// Backlog in frames; the frame loop paces off this.
        pub fn queued_frames(&self) -> usize {
            self.sink.as_ref().map_or(0, |s| s.len())
        }
    }

    impl AudioOutput for Output {
        fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            let stream = DeviceSinkBuilder::open_default_sink()?;
            let sink = Player::connect_new(stream.mixer());
            sink.set_volume(0.3);
            sink.pause();
            self._stream = Some(stream);
            self.sink = Some(sink);
            self.playing = false;
            Ok(())
        }

        fn add_samples(&mut self, samples: &[(f32, f32)]) {
            let Some(sink) = self.sink.as_ref() else { return };
            if samples.is_empty() {
                return;
            }
            let mut interleaved = Vec::with_capacity(samples.len() * 2);
            for &(left, right) in samples {
                interleaved.push(left);
                interleaved.push(right);
            }
            let channels = NonZero::new(2u16).unwrap();
            let sample_rate = NonZero::new(SAMPLE_RATE).unwrap();
            sink.append(SamplesBuffer::new(channels, sample_rate, interleaved));

            let queued = sink.len();
            if !self.playing {
                if queued >= CUSHION_PRIME_FRAMES {
                    sink.play();
                    self.playing = true;
                }
            } else if queued > CUSHION_MAX_FRAMES {
                while sink.len() > CUSHION_PRIME_FRAMES {
                    sink.skip_one();
                }
            }
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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// Stereo sample *pairs* per emulated frame (~44100 / 59.7 fps).
    const SAMPLES_PER_FRAME: usize = 735;
    /// Playback gain, matching the desktop sink's `set_volume(0.3)`.
    const VOLUME: f32 = 0.3;
    /// Ring capacity in frames. Generous — audio-clocked pacing holds the fill
    /// near a small target, so this is just a ceiling that bounds latency.
    const RING_FRAMES: usize = 32;
    /// Frames to buffer before starting the stream, so its first callback drains
    /// a primed ring instead of underrunning into startup silence.
    const PRIME_FRAMES: usize = 2;
    /// Log a backlog summary this often (~1s of audio).
    const REPORT_EVERY_FRAMES: u32 = 60;

    /// Real-time callback: drains the ring into oboe's output buffer, zero-filling
    /// on underrun. Must not allocate, lock, or block.
    struct Callback {
        cons: HeapCons<f32>,
        underruns: Arc<AtomicU32>,
    }

    impl AudioOutputCallback for Callback {
        type FrameType = (f32, Stereo);

        fn on_audio_ready(
            &mut self,
            _stream: &mut dyn AudioOutputStreamSafe,
            frames: &mut [(f32, f32)],
        ) -> DataCallbackResult {
            let mut underran = false;
            for frame in frames.iter_mut() {
                let mut lr = [0.0f32; 2];
                if self.cons.pop_slice(&mut lr) == 2 {
                    *frame = (lr[0], lr[1]);
                } else {
                    *frame = (0.0, 0.0);
                    underran = true;
                }
            }
            if underran {
                self.underruns.fetch_add(1, Ordering::Relaxed);
            }
            DataCallbackResult::Continue
        }
    }

    pub struct Output {
        stream: Option<AudioStreamAsync<OboeOutput, Callback>>,
        prod: Option<HeapProd<f32>>,
        underruns: Arc<AtomicU32>,
        /// `false` until the ring is primed and the stream is started.
        started: bool,
        scratch: Vec<f32>,
        /// Counts appended frames to throttle the underrun check to ~once/second.
        report_frames: u32,
    }

    impl Output {
        pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
            Ok(Output {
                stream: None,
                prod: None,
                underruns: Arc::new(AtomicU32::new(0)),
                started: false,
                scratch: Vec::new(),
                report_frames: 0,
            })
        }

        pub fn start_device(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            <Self as AudioOutput>::start(self)
        }

        pub fn push_samples(&mut self, samples: &[(f32, f32)]) {
            <Self as AudioOutput>::add_samples(self, samples)
        }

        /// Backlog in frames (ring depth); the frame loop paces off this.
        pub fn queued_frames(&self) -> usize {
            self.prod
                .as_ref()
                .map_or(0, |p| p.occupied_len() / 2 / SAMPLES_PER_FRAME)
        }
    }

    impl AudioOutput for Output {
        fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            let (prod, cons) = HeapRb::<f32>::new(RING_FRAMES * SAMPLES_PER_FRAME * 2).split();
            let underruns = Arc::new(AtomicU32::new(0));

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
                .set_callback(Callback { cons, underruns: underruns.clone() })
                .open_stream()
                .map_err(|e| format!("oboe open_stream failed: {e:?}"))?;

            // Opened but not started: `add_samples` starts it once the ring is
            // primed, so the first callback drains real samples, not silence.
            log::info!(
                "oboe stream opened: rate={}Hz burst={} frames, perf={:?}",
                stream.get_sample_rate(),
                stream.get_frames_per_burst(),
                stream.get_performance_mode(),
            );

            self.stream = Some(stream);
            self.prod = Some(prod);
            self.underruns = underruns;
            self.started = false;
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
            // If the ring is full (producer outran the device — shouldn't happen
            // under audio-clocked pacing) the excess is dropped, bounding latency.
            prod.push_slice(&self.scratch);

            // Start the stream once the ring holds a cushion (avoids startup
            // underruns draining an empty ring).
            if !self.started
                && prod.occupied_len() / 2 / SAMPLES_PER_FRAME >= PRIME_FRAMES
            {
                if let Some(stream) = self.stream.as_mut() {
                    let _ = stream.start();
                    self.started = true;
                }
            }

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

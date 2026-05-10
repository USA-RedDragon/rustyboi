use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};
use rodio::buffer::SamplesBuffer;
use std::num::NonZero;

use rustyboi_core_lib::audio::AudioOutput;

pub struct Output {
    _stream: Option<MixerDeviceSink>,
    sink: Option<Player>,
    buffer: Vec<(f32, f32)>,
}

const SAMPLE_RATE: u32 = 44100;
const BUFFER_SIZE: usize = (SAMPLE_RATE as f32 * 0.001) as usize; // 1ms worth of samples

impl Output {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Output {
            _stream: None,
            sink: None,
            buffer: Vec::new(),
        })
    }

    /// Start the audio device. Inherent wrapper over the `AudioOutput::start`
    /// impl so callers that feed samples directly (the session path, where the
    /// core's sink lives in `Session`, not the `GB`) don't need the trait in
    /// scope.
    pub fn start_device(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        <Self as AudioOutput>::start(self)
    }

    /// Push stereo samples produced by a `Session::run_frame` call into the
    /// output. Inherent wrapper over `AudioOutput::add_samples`.
    pub fn push_samples(&mut self, samples: &[(f32, f32)]) {
        <Self as AudioOutput>::add_samples(self, samples)
    }

    fn flush_buffer(&mut self) {
        if let Some(sink) = &self.sink
            && !self.buffer.is_empty() {
                let mut mono_samples = Vec::with_capacity(self.buffer.len() * 2);
                for &(left, right) in &self.buffer {
                    mono_samples.push(left);
                    mono_samples.push(right);
                }

                let channels = NonZero::new(2u16).unwrap();
                let sample_rate = NonZero::new(SAMPLE_RATE).unwrap();
                let audio_buffer = SamplesBuffer::new(channels, sample_rate, mono_samples);
                sink.append(audio_buffer);

                self.buffer.clear();
            }
    }
}

impl AudioOutput for Output {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let stream = DeviceSinkBuilder::open_default_sink()?;
        let sink = Player::connect_new(stream.mixer());

        sink.set_volume(0.3);
        sink.play();

        self._stream = Some(stream);
        self.sink = Some(sink);

        Ok(())
    }

    fn add_samples(&mut self, samples: &[(f32, f32)]) {
        if self.sink.is_some()
            && !samples.is_empty() {
                self.buffer.extend_from_slice(samples);
                if self.buffer.len() >= BUFFER_SIZE {
                    self.flush_buffer();
                }
        }
    }
}

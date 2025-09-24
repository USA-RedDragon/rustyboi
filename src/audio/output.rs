use rodio::{OutputStream, OutputStreamHandle, Sink};
use rodio::buffer::SamplesBuffer;
use std::time::Duration;

pub struct AudioOutput {
    _stream: OutputStream,
    _stream_handle: OutputStreamHandle,
    sink: Option<Sink>,
    sample_rate: u32,
}

impl AudioOutput {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (stream, stream_handle) = OutputStream::try_default()?;
        
        Ok(AudioOutput {
            _stream: stream,
            _stream_handle: stream_handle,
            sink: None,
            sample_rate: 44100, // Standard sample rate
        })
    }

    pub fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let sink = Sink::try_new(&self._stream_handle)?;
        sink.set_volume(0.3); // Increase volume for better audibility
        sink.play(); // Ensure the sink is playing
        self.sink = Some(sink);
        Ok(())
    }

    pub fn add_samples(&self, samples: &[(f32, f32)]) {
        if let Some(sink) = &self.sink {
            // Convert stereo samples to interleaved mono samples for rodio
            let mut mono_samples = Vec::with_capacity(samples.len() * 2);
            for &(left, right) in samples {
                mono_samples.push(left);
                mono_samples.push(right);
            }
            
            // Create a SamplesBuffer and append it directly to the sink
            let buffer = SamplesBuffer::new(2, self.sample_rate, mono_samples);
            sink.append(buffer);
        }
    }

    pub fn set_volume(&self, volume: f32) {
        if let Some(sink) = &self.sink {
            sink.set_volume(volume.clamp(0.0, 1.0));
        }
    }

    pub fn is_playing(&self) -> bool {
        self.sink.as_ref().map_or(false, |sink| !sink.empty())
    }
}
use rodio::{OutputStream, Sink};
use rodio::buffer::SamplesBuffer;
use std::time::Instant;

pub struct AudioOutput {
    _stream: Option<OutputStream>,
    sink: Option<Sink>,
    buffer: Vec<(f32, f32)>,
}

const SAMPLE_RATE: u32 = 44100;
const BUFFER_SIZE: usize = (SAMPLE_RATE as f32 * 0.001) as usize; // 1ms worth of samples

impl AudioOutput {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(AudioOutput {
            _stream: None,
            sink: None,
            buffer: Vec::new(),
        })
    }

    pub fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (_stream, stream_handle) = OutputStream::try_default()?;
        let sink = Sink::try_new(&stream_handle)?;
        
        sink.set_volume(0.3);
        sink.play();
        
        self._stream = Some(_stream);
        self.sink = Some(sink);
        
        Ok(())
    }

    pub fn add_samples(&mut self, samples: &[(f32, f32)]) {
        if self.sink.is_some() {
            if !samples.is_empty() {
                self.buffer.extend_from_slice(samples);
                if self.buffer.len() >= BUFFER_SIZE {
                    self.flush_buffer();
                }
            }
        }
    }

    fn flush_buffer(&mut self) {
        if let Some(sink) = &self.sink {
            if !self.buffer.is_empty() {
                let mut mono_samples = Vec::with_capacity(self.buffer.len() * 2);
                for &(left, right) in &self.buffer {
                    mono_samples.push(left);
                    mono_samples.push(right);
                }
                
                let audio_buffer = SamplesBuffer::new(2, SAMPLE_RATE, mono_samples);
                sink.append(audio_buffer);
                
                self.buffer.clear();
            }
        }
    }
}
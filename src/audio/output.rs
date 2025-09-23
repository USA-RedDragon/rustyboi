use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct AudioOutput {
    _stream: OutputStream,
    _stream_handle: OutputStreamHandle,
    sink: Option<Sink>,
    sample_buffer: Arc<Mutex<Vec<(f32, f32)>>>,
    sample_rate: u32,
}

impl AudioOutput {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (stream, stream_handle) = OutputStream::try_default()?;
        
        Ok(AudioOutput {
            _stream: stream,
            _stream_handle: stream_handle,
            sink: None,
            sample_buffer: Arc::new(Mutex::new(Vec::new())),
            sample_rate: 44100, // Standard sample rate
        })
    }

    pub fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let sink = Sink::try_new(&self._stream_handle)?;
        
        // Create a continuous audio source that reads from our buffer
        let buffer_clone = Arc::clone(&self.sample_buffer);
        let source = GameBoyAudioSource::new(buffer_clone, self.sample_rate);
        
        sink.append(source);
        sink.set_volume(0.1); // Start with lower volume
        sink.play(); // Ensure the sink is playing
        self.sink = Some(sink);
        
        Ok(())
    }

    pub fn add_samples(&self, samples: &[(f32, f32)]) {
        if let Ok(mut buffer) = self.sample_buffer.lock() {
            buffer.extend_from_slice(samples);
            
            // Prevent buffer from growing too large
            let buffer_len = buffer.len();
            let max_size = self.sample_rate as usize * 2;
            if buffer_len > max_size {
                buffer.drain(0..buffer_len - self.sample_rate as usize);
            }
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

// Custom audio source that reads from our Game Boy audio buffer
struct GameBoyAudioSource {
    buffer: Arc<Mutex<Vec<(f32, f32)>>>,
    sample_rate: u32,
    current_sample: Option<(f32, f32)>,
    channel_position: usize, // 0 for left, 1 for right
}

impl GameBoyAudioSource {
    fn new(buffer: Arc<Mutex<Vec<(f32, f32)>>>, sample_rate: u32) -> Self {
        Self {
            buffer,
            sample_rate,
            current_sample: None,
            channel_position: 0,
        }
    }
}

impl Iterator for GameBoyAudioSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        // If we don't have a current sample, try to get one from the buffer
        if self.current_sample.is_none() {
            if let Ok(mut buffer) = self.buffer.lock() {
                if !buffer.is_empty() {
                    self.current_sample = Some(buffer.remove(0));
                    self.channel_position = 0;
                }
            }
        }
        
        // Return the appropriate channel from current sample
        if let Some(sample) = self.current_sample {
            let result = if self.channel_position == 0 {
                sample.0 // Left channel
            } else {
                sample.1 // Right channel
            };
            
            self.channel_position += 1;
            
            // If we've returned both channels, clear current sample
            if self.channel_position >= 2 {
                self.current_sample = None;
                self.channel_position = 0;
            }
            
            Some(result)
        } else {
            // No samples available, output silence
            Some(0.0)
        }
    }
}

impl Source for GameBoyAudioSource {
    fn current_frame_len(&self) -> Option<usize> {
        None // Infinite source
    }

    fn channels(&self) -> u16 {
        2 // Stereo
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None // Infinite source
    }
}
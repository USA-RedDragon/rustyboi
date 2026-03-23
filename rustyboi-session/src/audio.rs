//! Audio as an OUTPUT, not a port.
//!
//! The core delivers audio by pushing `(f32, f32)` stereo samples into a
//! `Box<dyn AudioOutput>` sink installed via `GB::enable_audio`. To surface
//! those samples as a return value (so the adapter presents them however it
//! likes, at whatever cadence it drives), the session installs a capturing
//! sink here that just accumulates into a shared buffer; `run_frame` drains it
//! into the returned `FrameOutput`. No wall clock, no device — purely a
//! collector, so it stays WASM-clean.

use rustyboi_core_lib::audio::AudioOutput;
use std::cell::RefCell;
use std::rc::Rc;

/// Shared, drainable buffer of stereo samples produced since the last drain.
pub(crate) type SampleBuf = Rc<RefCell<Vec<(f32, f32)>>>;

/// The `AudioOutput` the session installs into the `GB`. Holds a clone of the
/// shared buffer; every `add_samples` appends to it. The session owns the other
/// clone and drains it each frame.
pub(crate) struct CaptureSink {
    buf: SampleBuf,
}

impl CaptureSink {
    pub(crate) fn new(buf: SampleBuf) -> Self {
        CaptureSink { buf }
    }
}

impl AudioOutput for CaptureSink {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    fn add_samples(&mut self, samples: &[(f32, f32)]) {
        self.buf.borrow_mut().extend_from_slice(samples);
    }
}

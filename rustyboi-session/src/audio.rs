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
use std::sync::{Arc, Mutex};

/// Shared, drainable buffer of stereo samples produced since the last drain.
/// `Arc<Mutex>` (not `Rc<RefCell>`) so the installed sink — and therefore the
/// whole `GB` — is `Send`, letting the host serialize a cloned `GB` on a worker
/// thread with no `unsafe`. The lock is uncontended (once per frame) and works
/// fine on single-threaded wasm.
pub(crate) type SampleBuf = Arc<Mutex<Vec<(f32, f32)>>>;

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
    // Poison-recovering per the tree-wide convention (see `rustyboi_core_lib::ir`):
    // the buffer is plain accumulated samples, so the worst a panic mid-append
    // can leave is a partial frame's audio that the next drain clears. Bricking
    // every later frame's audio on an unrelated panic would be far worse.
    fn add_samples(&mut self, samples: &[(f32, f32)]) {
        self.buf.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(samples);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic elsewhere must not permanently silence audio capture.
    #[test]
    fn poisoned_buffer_still_captures() {
        let buf: SampleBuf = Arc::new(Mutex::new(Vec::new()));
        let poisoner = Arc::clone(&buf);
        let joined = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("unrelated failure while holding the sample buffer");
        })
        .join();
        assert!(joined.is_err(), "the helper thread must actually have panicked");
        assert!(buf.is_poisoned(), "the buffer must actually be poisoned");

        // Pre-fix this panicked, permanently killing audio for the session.
        let mut sink = CaptureSink::new(Arc::clone(&buf));
        sink.add_samples(&[(0.25, -0.25), (0.5, -0.5)]);
        assert_eq!(
            *buf.lock().unwrap_or_else(|e| e.into_inner()),
            vec![(0.25, -0.25), (0.5, -0.5)]
        );
    }
}

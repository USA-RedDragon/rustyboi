//! Tiny wasm decoder for `.rbr` recordings. Parses a recording and decodes its
//! frames to RGBA for a 2D `<canvas>` (`putImageData`). Pulls in only the codec
//! + wasm-bindgen glue — no core/wgpu/egui — so the module is a few tens of KB.
//!
//! One `Decoder` per live canvas keeps its own running frame; `next_frame()`
//! advances (looping at the end), returning the RGBA bytes for the new frame.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Decoder {
    inner: rustyboi_replay::Decoder,
    rgba: Vec<u8>,
}

#[wasm_bindgen]
impl Decoder {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>) -> Result<Decoder, JsError> {
        let inner = rustyboi_replay::Decoder::new(bytes).map_err(|e| JsError::new(&e.to_string()))?;
        let rgba = vec![0u8; inner.width() as usize * inner.height() as usize * 4];
        Ok(Decoder { inner, rgba })
    }

    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u16 {
        self.inner.width()
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u16 {
        self.inner.height()
    }

    #[wasm_bindgen(getter)]
    pub fn frames(&self) -> u32 {
        self.inner.frame_count()
    }

    /// Frame duration in milliseconds, from the recording's fps rational.
    #[wasm_bindgen(getter, js_name = frameMs)]
    pub fn frame_ms(&self) -> f64 {
        1000.0 * self.inner.fps_den() as f64 / self.inner.fps_num() as f64
    }

    /// Decode the next frame (wrapping after the last) and return its RGBA bytes,
    /// ready for `new ImageData(new Uint8ClampedArray(bytes), width, height)`.
    #[wasm_bindgen(js_name = nextFrame)]
    pub fn next_frame(&mut self) -> Result<Vec<u8>, JsError> {
        self.inner
            .next_into(&mut self.rgba)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(self.rgba.clone())
    }

    /// Restart from frame 0.
    pub fn reset(&mut self) {
        self.inner.reset();
    }
}

/// Decodes an `.rba` audio recording (4-channel decomposition) to interleaved
/// stereo f32, one video frame's span at a time — the page schedules the
/// samples via Web Audio; no APU and no web-sys in here.
#[wasm_bindgen]
pub struct AudioDecoder {
    inner: rustyboi_replay::AudioDecoder,
    buf: Vec<f32>,
    frame: u32,
}

#[wasm_bindgen]
impl AudioDecoder {
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: Vec<u8>) -> Result<AudioDecoder, JsError> {
        let inner =
            rustyboi_replay::AudioDecoder::new(bytes).map_err(|e| JsError::new(&e.to_string()))?;
        Ok(AudioDecoder { inner, buf: Vec::new(), frame: 0 })
    }

    #[wasm_bindgen(getter, js_name = sampleRate)]
    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    #[wasm_bindgen(getter, js_name = sampleCount)]
    pub fn sample_count(&self) -> u32 {
        self.inner.sample_count()
    }

    /// Position playback at the start of video frame `i`.
    #[wasm_bindgen(js_name = seekFrame)]
    pub fn seek_frame(&mut self, i: u32) {
        self.inner.seek_frame(i);
        self.frame = i;
    }

    /// Interleaved stereo samples for the next video frame (empty at stream
    /// end — callers seek back to 0 when the video loops).
    #[wasm_bindgen(js_name = nextFrame)]
    pub fn next_frame(&mut self) -> Result<Vec<f32>, JsError> {
        self.inner
            .frame_into(self.frame, &mut self.buf)
            .map_err(|e| JsError::new(&e.to_string()))?;
        self.frame += 1;
        Ok(self.buf.clone())
    }
}

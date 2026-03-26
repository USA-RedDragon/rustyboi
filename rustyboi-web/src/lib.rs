//! `rustyboi-web` — a WASM/Firefox frontend for the rustyboi Game Boy / Color
//! emulator, built on the shared `rustyboi-session` crate.
//!
//! # Why these choices are Firefox-safe
//!
//! - **Rendering: 2D canvas `putImageData`.** No `wgpu`/WebGPU (not stable in
//!   Firefox), no `pixels`. At 160x144 an `ImageData` blit per frame is
//!   trivially fast; CSS `image-rendering: pixelated` handles upscaling.
//! - **Audio: WebAudio queued `AudioBufferSourceNode`** (see [`audio`]).
//! - **Input: DOM keyboard events → [`AbstractInput`]**, resolved by the
//!   session's own remap. No host key codes leak into the session.
//! - **Storage: IndexedDB** (see [`storage`]). The File System Access API is
//!   Chrome-only; ROM bytes arrive via `<input type=file>` → `ArrayBuffer`.
//!
//! The JS shell drives a `requestAnimationFrame` loop that calls
//! [`Emulator::run_frame`] to keep pace, then presents + pumps audio.

mod audio;
mod storage;

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_session::config::DmgPalette;
use rustyboi_session::ports::{Rumble, Webcam};
use rustyboi_session::{
    movie, AbstractInput, Config, Frame, GbButton, Hardware, Ports, Session, SlotMeta, GB,
};

use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};

use audio::AudioPlayer;
use storage::IdbStore;

const GB_WIDTH: u32 = 160;
const GB_HEIGHT: u32 = 144;
const RGBA_LEN: usize = (GB_WIDTH * GB_HEIGHT * 4) as usize;

/// No-op rumble: browsers have no cartridge motor. (Gamepad haptics could hook
/// here later.)
struct NullRumble;
impl Rumble for NullRumble {
    fn set(&mut self, _on: bool) {}
}

/// No-op webcam: Game Boy Camera support would wire `getUserMedia` here.
struct NullWebcam;
impl Webcam for NullWebcam {
    fn grab(&mut self) -> Option<Vec<u8>> {
        None
    }
}

/// The emulator handle exposed to JavaScript. Owns the session, the render
/// target, the audio player, and the live keyboard-derived input.
#[wasm_bindgen]
pub struct Emulator {
    session: Session,
    storage: IdbStore,
    ctx: CanvasRenderingContext2d,
    audio: AudioPlayer,
    input: AbstractInput,
    /// Reusable RGBA scratch buffer (avoids a per-frame allocation).
    rgba: Vec<u8>,
    dmg_palette: DmgPalette,
    has_rom: bool,
}

#[wasm_bindgen]
impl Emulator {
    /// Construct the emulator bound to a canvas (looked up by element id).
    /// Async because it must open + hydrate IndexedDB before building the
    /// session (so persisted config/saves are visible to the first sync read).
    /// A static factory rather than a `constructor` — wasm-bindgen can't emit a
    /// valid async constructor.
    pub async fn create(canvas_id: &str) -> Result<Emulator, JsValue> {
        console_error_panic_hook::set_once();

        let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
        let document = window.document().ok_or_else(|| JsValue::from_str("no document"))?;
        let canvas: HtmlCanvasElement = document
            .get_element_by_id(canvas_id)
            .ok_or_else(|| JsValue::from_str("canvas element not found"))?
            .dyn_into()?;
        canvas.set_width(GB_WIDTH);
        canvas.set_height(GB_HEIGHT);
        let ctx: CanvasRenderingContext2d = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("no 2d context"))?
            .dyn_into()?;

        let storage = IdbStore::open_and_hydrate().await?;
        let config = Config::load(&storage);
        let dmg_palette = config.dmg_palette;

        // Start with an empty (no-cartridge) session; a ROM is inserted later
        // via `load_rom`. Cheap, and keeps the JS bootstrap a single await.
        let ports = Ports {
            storage: Box::new(storage.clone()),
            rumble: Box::new(NullRumble),
            webcam: Box::new(NullWebcam),
        };
        let session = Session::new(config, ports, [0u8; 32]);
        let audio = AudioPlayer::new()?;

        Ok(Emulator {
            session,
            storage,
            ctx,
            audio,
            input: AbstractInput::none(),
            rgba: vec![0u8; RGBA_LEN],
            dmg_palette,
            has_rom: false,
        })
    }

    /// Load a ROM from raw bytes (an `ArrayBuffer` from `<input type=file>`).
    /// Builds a fresh booted `GB`, re-binds the session to the new ROM identity,
    /// and unlocks the audio context (this runs from a user gesture).
    pub fn load_rom(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let cart = Cartridge::from_bytes(bytes)
            .map_err(|e| JsValue::from_str(&format!("cartridge load failed: {e}")))?;
        let rom_id = movie::sha256(bytes);

        let mut gb = GB::new(self.session.hardware());
        gb.insert(cart);
        gb.skip_bios();
        self.session.replace_machine(gb, rom_id);

        self.audio.resume();
        self.has_rom = true;
        Ok(())
    }

    /// Advance one presented frame per the session's run mode, blit it to the
    /// canvas, and queue its audio. Called from the JS `requestAnimationFrame`
    /// loop (which decides how many times to call it to keep 59.7275 fps pace).
    pub fn run_frame(&mut self) -> Result<(), JsValue> {
        if !self.has_rom {
            return Ok(());
        }
        let out = self.session.run_frame(self.input);
        self.present(&out.frame)?;
        self.audio.queue(&out.audio);
        Ok(())
    }

    /// Convert the core `Frame` to RGBA and `putImageData` it. `Monochrome`
    /// frames map shade 0-3 through the DMG palette; `Color` frames are already
    /// RGB and just gain an opaque alpha.
    fn present(&mut self, frame: &Frame) -> Result<(), JsValue> {
        match frame {
            Frame::Monochrome(shades) => {
                let colors = &self.dmg_palette.shades;
                for (i, &s) in shades.iter().enumerate() {
                    let c = colors.get(s as usize).unwrap_or(&colors[3]);
                    let o = i * 4;
                    self.rgba[o..o + 4].copy_from_slice(c);
                }
            }
            Frame::Color(rgb) => {
                for (i, chunk) in rgb.chunks_exact(3).enumerate() {
                    let o = i * 4;
                    self.rgba[o] = chunk[0];
                    self.rgba[o + 1] = chunk[1];
                    self.rgba[o + 2] = chunk[2];
                    self.rgba[o + 3] = 0xFF;
                }
            }
        }
        let image = ImageData::new_with_u8_clamped_array_and_sh(
            Clamped(&self.rgba),
            GB_WIDTH,
            GB_HEIGHT,
        )?;
        self.ctx.put_image_data(&image, 0.0, 0.0)
    }

    /// Set/clear an abstract GB button from a JS key handler. `code` is a
    /// `KeyboardEvent.code` string; unknown codes are ignored. This is the
    /// host→abstract classification the session docs put on the adapter.
    pub fn set_button(&mut self, code: &str, pressed: bool) {
        if let Some(btn) = classify_key(code) {
            self.input.set(btn, pressed);
        }
    }

    /// Clear all pressed buttons (e.g. on focus loss / blur).
    pub fn clear_input(&mut self) {
        self.input = AbstractInput::none();
    }

    /// Toggle pause.
    pub fn toggle_pause(&mut self) {
        self.session.toggle_pause();
    }

    /// Hold fast-forward while pressed; release returns to normal speed.
    pub fn set_fast_forward(&mut self, on: bool) {
        if on {
            self.session.fast_forward();
        } else {
            self.session.set_mode(rustyboi_session::RunMode::Normal);
        }
    }

    /// Save the current machine state to a numbered slot. `timestamp` is
    /// caller-supplied wall-clock millis (the session never reads a clock).
    pub fn save_slot(&mut self, slot: u32, timestamp: f64) -> Result<(), JsValue> {
        self.session
            .save_slot(slot, timestamp as u64)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Load a numbered slot, replacing the current machine. Returns the loaded
    /// frame count.
    pub fn load_slot(&mut self, slot: u32) -> Result<f64, JsValue> {
        let SlotMeta { frame_count, .. } = self
            .session
            .load_slot(slot)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(frame_count as f64)
    }

    /// The slot numbers with a saved state for the current ROM.
    pub fn list_slots(&self) -> Vec<u32> {
        self.session.list_slots()
    }

    /// Switch the emulated hardware model ("dmg" or "cgb"); persists config.
    /// Takes effect on the next ROM load.
    pub fn set_hardware(&mut self, model: &str) -> Result<(), JsValue> {
        let hw = match model {
            "dmg" | "DMG" => Hardware::DMG,
            "cgb" | "CGB" => Hardware::CGB,
            other => return Err(JsValue::from_str(&format!("unknown hardware: {other}"))),
        };
        let mut cfg = self.session.config().clone();
        cfg.hardware = hw;
        self.session.set_config(cfg);
        self.session
            .save_config()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Whether a ROM is currently loaded.
    pub fn has_rom(&self) -> bool {
        self.has_rom
    }

    /// Number of persisted IndexedDB keys hydrated at startup (diagnostic).
    pub fn stored_key_count(&self) -> usize {
        self.storage.len()
    }
}

/// Map a browser `KeyboardEvent.code` to an abstract GB button. The default
/// layout: arrows = d-pad, Z = B, X = A, Enter = Start, Shift = Select.
fn classify_key(code: &str) -> Option<GbButton> {
    Some(match code {
        "ArrowUp" => GbButton::Up,
        "ArrowDown" => GbButton::Down,
        "ArrowLeft" => GbButton::Left,
        "ArrowRight" => GbButton::Right,
        "KeyX" => GbButton::A,
        "KeyZ" => GbButton::B,
        "Enter" => GbButton::Start,
        "ShiftRight" | "ShiftLeft" => GbButton::Select,
        _ => return None,
    })
}

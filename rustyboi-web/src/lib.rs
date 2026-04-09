//! `rustyboi-web` — a WASM web frontend for the rustyboi Game Boy / Color
//! emulator, built on the shared `rustyboi-session` crate.
//!
//! # Worker-threaded architecture
//!
//! The emulator runs entirely in a **Web Worker** (`www/worker.js`), decoupled
//! from the display refresh rate. This fixes compositor jank on high-refresh
//! displays (e.g. 175 Hz): a `requestAnimationFrame` loop fires at the monitor
//! rate, but one GB frame can take ~10 ms, blowing a ~5.7 ms rAF budget. The
//! worker self-paces at 59.7275 fps with a `performance.now()` accumulator, so
//! emulation cadence is independent of how fast the screen refreshes.
//!
//! Thread split:
//! - **Worker** owns [`Emulator`]: the session + wasm core, IndexedDB storage
//!   (IndexedDB works in workers), and rendering to a transferred
//!   [`web_sys::OffscreenCanvas`] via its 2D context — so video never crosses a
//!   `postMessage` boundary. Each frame it returns interleaved audio to JS.
//! - **Main thread** is a thin UI shell (`www/index.html`): DOM controls,
//!   keyboard input, and the [`WebAudio`] sink (WebAudio must be created on the
//!   main thread). It forwards input/control messages to the worker and queues
//!   the audio batches the worker posts back.
//!
//! # Why these host choices are Firefox-safe
//!
//! - **Rendering: OffscreenCanvas 2D `putImageData`.** No `wgpu`/WebGPU (not
//!   stable in Firefox), no `pixels`. At 160x144 an `ImageData` blit per frame
//!   is trivially fast; CSS `image-rendering: pixelated` handles upscaling.
//! - **Audio: WebAudio queued `AudioBufferSourceNode`** (see [`audio`]).
//! - **Input: DOM keyboard events → [`AbstractInput`]**, resolved by the
//!   session's own remap. No host key codes leak into the session.
//! - **Storage: IndexedDB** (see [`storage`]). The File System Access API is
//!   Chrome-only; ROM bytes arrive via `<input type=file>` → `ArrayBuffer`.

mod audio;
mod overlay;
mod storage;

use rustyboi_session::config::DmgPalette;
use rustyboi_session::ports::{Rumble, Webcam};
use rustyboi_session::{AbstractInput, Config, Frame, GbButton, Hardware, Ports, Session};

use js_sys::Float32Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::{Clamped, JsCast};
use web_sys::{ImageData, OffscreenCanvas, OffscreenCanvasRenderingContext2d};

use rustyboi_session::{
    FileData, HardwareChoice, PaletteChoice, PlatformRequest, SessionUiState, UiAction,
};

use js_sys::Array;

use storage::IdbStore;

// The main-thread audio sink; re-exported so JS can `new WebAudio()`.
pub use audio::WebAudio;
// The shared on-screen joypad overlay bridge (main-thread usable).
pub use overlay::TouchOverlay;

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

/// The worker-side emulator handle exposed to JavaScript. Owns the session, the
/// OffscreenCanvas render target, storage, and the live keyboard-derived input.
///
/// Runs ONLY inside the Web Worker. It never touches `window`, `document`,
/// `requestAnimationFrame`, or `AudioContext` — those are main-thread-only and
/// live in the JS shell. Video is drawn straight to the OffscreenCanvas the
/// worker owns; audio is returned to JS to be posted to the main thread.
#[wasm_bindgen]
pub struct Emulator {
    session: Session,
    storage: IdbStore,
    ctx: OffscreenCanvasRenderingContext2d,
    /// Buttons held via the keyboard (main-thread key events).
    input: AbstractInput,
    /// Buttons held via the on-screen touch overlay (multi-touch). Kept separate
    /// from `input` so touch and keyboard never clobber each other; the machine
    /// sees the union each frame.
    touch: AbstractInput,
    /// Reusable RGBA scratch buffer (avoids a per-frame allocation).
    rgba: Vec<u8>,
    /// Reusable interleaved-audio scratch buffer (`[l0,r0,l1,r1,...]`).
    audio_scratch: Vec<f32>,
    dmg_palette: DmgPalette,
    has_rom: bool,
}

#[wasm_bindgen]
impl Emulator {
    /// Construct the emulator bound to a transferred [`OffscreenCanvas`].
    /// Async because it must open + hydrate IndexedDB before building the
    /// session (so persisted config/saves are visible to the first sync read).
    /// A static factory rather than a `constructor` — wasm-bindgen can't emit a
    /// valid async constructor.
    pub async fn create(canvas: OffscreenCanvas) -> Result<Emulator, JsValue> {
        console_error_panic_hook::set_once();

        canvas.set_width(GB_WIDTH);
        canvas.set_height(GB_HEIGHT);
        let ctx: OffscreenCanvasRenderingContext2d = canvas
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

        Ok(Emulator {
            session,
            storage,
            ctx,
            input: AbstractInput::none(),
            touch: AbstractInput::none(),
            rgba: vec![0u8; RGBA_LEN],
            audio_scratch: Vec::new(),
            dmg_palette,
            has_rom: false,
        })
    }

    /// Load a ROM from raw bytes (an `ArrayBuffer` transferred from the main
    /// thread), routed through the shared contract.
    ///
    /// `session.apply(LoadRom)` returns a [`PlatformRequest::LoadFile`]; the web
    /// frontend already holds the bytes, so we service that request here by
    /// feeding them to `finish_load_rom`. Returns the resulting requests
    /// (Status/Error) for the worker to surface. `name` is only for messages.
    pub fn load_rom(&mut self, name: &str, bytes: &[u8]) -> Array {
        let outcome = self.session.apply(UiAction::LoadRom(rom_file(name, bytes)), 0);
        let mut extra: Vec<PlatformRequest> = Vec::new();
        for req in outcome.requests {
            // The only request LoadRom produces is a LoadFile; the web frontend
            // already holds the bytes, so service it here. Any other request is
            // forwarded verbatim.
            if matches!(req, PlatformRequest::LoadFile(_)) {
                match self.session.finish_load_rom(bytes) {
                    Ok(_) => {
                        self.has_rom = true;
                        extra.push(PlatformRequest::ClearError);
                    }
                    Err(e) => {
                        extra.push(PlatformRequest::Error(format!("cartridge load failed: {e}")))
                    }
                }
            } else {
                extra.push(req);
            }
        }
        requests_to_js(&extra)
    }

    /// Advance one presented frame per the session's run mode, blit it to the
    /// OffscreenCanvas, and return this frame's interleaved stereo audio
    /// (`[l0,r0,l1,r1,...]`) as a fresh `Float32Array` for the worker to
    /// `postMessage` (transferring its buffer) to the main-thread audio sink.
    /// Returns an empty array when no ROM is loaded or the frame produced no
    /// audio.
    pub fn run_frame(&mut self) -> Float32Array {
        if !self.has_rom {
            return Float32Array::new_with_length(0);
        }
        // The machine sees the union of keyboard + touch input.
        let mut combined = self.input;
        for b in GbButton::ALL {
            if self.touch.is_pressed(b) {
                combined.set(b, true);
            }
        }
        let out = self.session.run_frame(combined);
        if let Err(e) = self.present(&out.frame) {
            web_sys::console::warn_1(&e);
        }

        self.audio_scratch.clear();
        self.audio_scratch.reserve(out.audio.len() * 2);
        for &(l, r) in &out.audio {
            self.audio_scratch.push(l);
            self.audio_scratch.push(r);
        }
        // Copy into a JS-owned Float32Array; the worker transfers its buffer.
        Float32Array::from(self.audio_scratch.as_slice())
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

    /// Clear all keyboard-held buttons (e.g. on focus loss / blur). Touch state
    /// is untouched (a blur can't strand a finger the way it can a key).
    pub fn clear_input(&mut self) {
        self.input = AbstractInput::none();
    }

    /// Set the on-screen overlay's held buttons from a multi-touch bitmask (see
    /// [`overlay::TouchOverlay::button_mask`]). Replaces the whole touch layer
    /// each call, so lifting a finger releases exactly its buttons.
    pub fn set_touch_mask(&mut self, mask: u8) {
        let mut touch = AbstractInput::none();
        for b in overlay::buttons_from_mask(mask) {
            touch.set(b, true);
        }
        self.touch = touch;
    }

    /// Toggle pause. (Pause is run-loop state; `apply` only signals a re-sync, so
    /// the flip stays here.)
    pub fn toggle_pause(&mut self) {
        self.session.toggle_pause();
    }

    /// Toggle the on-screen touch overlay via the shared contract and report the
    /// new state so the shell can show/hide its DOM overlay.
    pub fn toggle_touch_controls(&mut self) -> bool {
        self.session.apply(UiAction::ToggleTouchControls, 0);
        self.session.touch_controls()
    }

    /// Whether the on-screen touch overlay is currently enabled (session state).
    pub fn touch_controls(&self) -> bool {
        self.session.touch_controls()
    }

    /// Hold fast-forward while pressed; release returns to normal speed. Uses the
    /// `ToggleFastForward` contract action, guarded so a held key doesn't flip it
    /// back and forth.
    pub fn set_fast_forward(&mut self, on: bool) {
        if on != self.session.is_fast_forward() {
            self.session.apply(UiAction::ToggleFastForward, 0);
        }
    }

    /// Save the current machine state to a numbered slot via the shared contract.
    /// The `SaveSlot` action persists through the storage port (IndexedDB) inside
    /// the session; we return the resulting Status/Error requests. `timestamp` is
    /// caller-supplied wall-clock millis (the session never reads a clock).
    pub fn save_slot(&mut self, slot: u32, timestamp: f64) -> Array {
        let outcome = self.session.apply(UiAction::SaveSlot(slot), timestamp as u64);
        requests_to_js(&outcome.requests)
    }

    /// Load a numbered slot via the shared contract, replacing the current
    /// machine (persisted state read through the storage port). Returns the
    /// resulting Status/Error requests.
    pub fn load_slot(&mut self, slot: u32) -> Array {
        let outcome = self.session.apply(UiAction::LoadSlot(slot), 0);
        requests_to_js(&outcome.requests)
    }

    /// Quicksave to the reserved quick slot (shared contract).
    pub fn quicksave(&mut self, timestamp: f64) -> Array {
        let outcome = self.session.apply(UiAction::Quicksave, timestamp as u64);
        requests_to_js(&outcome.requests)
    }

    /// Quickload from the reserved quick slot (shared contract).
    pub fn quickload(&mut self) -> Array {
        let outcome = self.session.apply(UiAction::Quickload, 0);
        requests_to_js(&outcome.requests)
    }

    /// The slot numbers with a saved state for the current ROM.
    pub fn list_slots(&self) -> Vec<u32> {
        self.session.list_slots()
    }

    /// Switch the emulated hardware model ("dmg" or "cgb") via the shared
    /// contract (rebuilds + persists). Returns the resulting requests
    /// (ClearError/ResizeContent/Status).
    pub fn set_hardware(&mut self, model: &str) -> Result<Array, JsValue> {
        let choice = match model {
            "dmg" | "DMG" => HardwareChoice::Dmg,
            "cgb" | "CGB" => HardwareChoice::Cgb,
            other => return Err(JsValue::from_str(&format!("unknown hardware: {other}"))),
        };
        let outcome = self.session.apply(UiAction::SetHardware(choice), 0);
        Ok(requests_to_js(&outcome.requests))
    }

    /// Set the four-shade DMG palette (lightest→darkest, RGBA8 per shade, 16
    /// bytes total). Presentation-only. Config persistence is done through the
    /// `SetPalette` contract action; the local blit cache is refreshed so the
    /// worker's `present` uses the new shades immediately.
    pub fn set_palette(&mut self, shades: &[u8]) -> Result<(), JsValue> {
        if shades.len() != 16 {
            return Err(JsValue::from_str("palette must be 16 bytes (4 RGBA shades)"));
        }
        let mut palette = [[0u8; 4]; 4];
        for (i, chunk) in shades.chunks_exact(4).enumerate() {
            palette[i].copy_from_slice(chunk);
        }
        let choice = PaletteChoice::from_shades(palette);
        self.session.apply(UiAction::SetPalette(choice), 0);
        // Keep the raw shades for the worker's blit even for a custom palette
        // (from_shades falls back to Grayscale for an unknown set).
        self.dmg_palette = DmgPalette { shades: palette };
        Ok(())
    }

    /// The current hardware model as a lowercase string ("dmg" / "cgb").
    pub fn hardware(&self) -> String {
        match self.session.hardware() {
            Hardware::DMG => "dmg".into(),
            _ => "cgb".into(),
        }
    }

    /// A JSON-serializable snapshot of session UI state the shell reflects
    /// (hardware, palette, fast-forward, touch-controls, slot list). Returned as
    /// a plain JS object.
    pub fn ui_state(&self) -> js_sys::Object {
        let s = self.session_ui_state();
        let o = js_sys::Object::new();
        let set = |k: &str, v: JsValue| {
            let _ = js_sys::Reflect::set(&o, &k.into(), &v);
        };
        set("fastForward", s.fast_forward.into());
        set("touchControls", s.touch_controls.into());
        let hw = match s.hardware {
            HardwareChoice::Dmg => "dmg",
            HardwareChoice::Sgb => "sgb",
            HardwareChoice::Cgb => "cgb",
        };
        set("hardware", hw.into());
        o
    }

    /// Assemble the shared [`SessionUiState`] view from the session. Isolated so
    /// the new `touch_controls` field (and any future one) is set in one place.
    fn session_ui_state(&self) -> SessionUiState {
        SessionUiState {
            hardware: self.session.hardware_choice(),
            palette: self.session.palette(),
            rewind_enabled: self.session.config().rewind.enabled,
            rewind_interval_frames: self.session.config().rewind.interval_frames,
            rewind_depth: self.session.config().rewind.depth,
            sgb_border: self.session.sgb_border(),
            fast_forward: self.session.is_fast_forward(),
            touch_controls: self.session.touch_controls(),
            slots: self.session.list_slots(),
        }
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

/// Build the [`FileData`] the web frontend passes to `LoadRom` — always the
/// byte-carrying `Contents` variant on wasm. The `not(wasm32)` arm exists only so
/// `cargo build --workspace` typechecks this cdylib against the host `FileData`
/// (whose only variant is `Path`); the web crate never actually runs natively.
#[cfg(target_arch = "wasm32")]
fn rom_file(name: &str, bytes: &[u8]) -> FileData {
    FileData::Contents { name: name.into(), data: bytes.to_vec() }
}
#[cfg(not(target_arch = "wasm32"))]
fn rom_file(name: &str, _bytes: &[u8]) -> FileData {
    FileData::Path(name.into())
}

/// Translate the [`PlatformRequest`]s an `apply` produced into a JS array of
/// plain `{ type, ... }` objects the worker forwards to the main thread. `Exit`
/// is a no-op on web (nothing to quit) and is dropped; `SaveStateBytes`/
/// `LoadFile` are serviced in-worker and never appear here for the actions the
/// web frontend issues.
fn requests_to_js(requests: &[PlatformRequest]) -> Array {
    let out = Array::new();
    for req in requests {
        let o = js_sys::Object::new();
        let set = |k: &str, v: JsValue| {
            let _ = js_sys::Reflect::set(&o, &k.into(), &v);
        };
        match req {
            PlatformRequest::Exit => continue, // no-op on web
            PlatformRequest::Status(msg) => {
                set("type", "Status".into());
                set("msg", msg.as_str().into());
            }
            PlatformRequest::Error(msg) => {
                set("type", "Error".into());
                set("msg", msg.as_str().into());
            }
            PlatformRequest::ClearError => {
                set("type", "ClearError".into());
            }
            PlatformRequest::ResizeContent { width, height } => {
                set("type", "ResizeContent".into());
                set("width", (*width).into());
                set("height", (*height).into());
            }
            // These are serviced inside the worker for the web frontend and are
            // not expected from the actions it issues; surface as a status so
            // nothing is silently lost.
            PlatformRequest::SaveStateBytes { .. } | PlatformRequest::LoadFile(_) => {
                set("type", "Status".into());
                set("msg", "unhandled platform request".into());
            }
        }
        out.push(&o);
    }
    out
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

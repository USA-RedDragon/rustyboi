//! `rustyboi-web` — a WASM web frontend for the rustyboi Game Boy / Color
//! emulator, built on the shared `rustyboi-session` crate.
//!
//! # Two halves, one wasm module
//!
//! `wasm-pack --target web` emits a single module used by BOTH the worker and
//! the main thread. Which half runs depends on which entry point the JS calls:
//!
//! - **Worker** ([`Emulator`], driven by `www/worker.js`) owns the [`Session`] +
//!   wasm core and IndexedDB storage. It self-paces at 59.7275 fps, decoupled
//!   from the display refresh — the whole reason emulation lives off the main
//!   thread (a 175 Hz `requestAnimationFrame` loop would otherwise blow a
//!   per-frame budget and jank). Each frame it produces the RGBA framebuffer,
//!   interleaved audio, and a UI-state snapshot, and posts them to the main
//!   thread. Control commands arrive as [`web_action::WebAction`] JSON and are
//!   applied through the shared `Session::apply` contract.
//! - **Main thread** ([`WebApp`], driven by `www/index.html`) renders the
//!   **egui** UI (menus, cheats, keybinds, settings) over the game with wgpu's
//!   WebGL2 backend + the portable `rustyboi-frontend` `Renderer`/`UiHost`. It
//!   owns the `AudioContext` (WebAudio is main-thread only), the winit canvas,
//!   and keyboard/touch input, which it forwards to the worker.
//!
//! Video crosses the `postMessage` boundary as a transferred `ArrayBuffer`
//! (zero-copy). The main thread uploads it into the renderer's game texture;
//! emulation never runs on the main thread.

mod audio;
mod storage;
mod web_action;
mod webapp;

use rustyboi_session::config::DmgPalette;
use rustyboi_session::ports::{Rumble, Webcam};
use rustyboi_session::{
    AbstractInput, Config, DebugDetail, Frame, GbButton, Hardware, Ports, Session,
};

use js_sys::Float32Array;
use wasm_bindgen::prelude::*;

use rustyboi_session::{FileData, PlatformRequest, SessionUiState, UiAction};

use js_sys::Array;

use storage::IdbStore;
use web_action::{WebAction, WebUiState};

// The main-thread audio sink; re-exported so JS can `new WebAudio()`.
pub use audio::WebAudio;
// The main-thread egui driver (canvas + wgpu-WebGL2 + egui). JS `WebApp.start()`.
pub use webapp::WebApp;

const GB_WIDTH: u32 = 160;
const GB_HEIGHT: u32 = 144;
const SGB_WIDTH: u32 = 256;
const SGB_HEIGHT: u32 = 224;
/// RGBA scratch large enough for the SGB composite (the biggest source).
const RGBA_LEN: usize = (SGB_WIDTH * SGB_HEIGHT * 4) as usize;

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

/// The worker-side emulator handle exposed to JavaScript. Owns the session,
/// storage, and the live keyboard/touch-derived input.
///
/// Runs ONLY inside the Web Worker. It never touches `window`, `document`,
/// `requestAnimationFrame`, `AudioContext`, or any GPU — those are main-thread
/// concerns handled by [`WebApp`]. Each frame it fills an RGBA buffer the worker
/// transfers to the main thread for rendering; audio is returned to JS to be
/// posted to the main-thread audio sink.
#[wasm_bindgen]
pub struct Emulator {
    session: Session,
    storage: IdbStore,
    /// GB buttons currently held (union of keyboard + on-screen touch), latched
    /// from the main thread each time the set changes.
    input: AbstractInput,
    /// Reusable RGBA scratch (avoids a per-frame allocation). Sized for SGB.
    rgba: Vec<u8>,
    /// Width/height of the RGBA currently in `rgba` (GB 160×144 or SGB 256×224).
    frame_w: u32,
    frame_h: u32,
    /// Reusable interleaved-audio scratch (`[l0,r0,l1,r1,...]`).
    audio_scratch: Vec<f32>,
    dmg_palette: DmgPalette,
    /// Last UI-state snapshot posted, so the worker only re-posts on change.
    last_ui_state: Option<SessionUiState>,
    has_rom: bool,
    /// Whether any main-thread debug panel is open. While false the worker builds
    /// and posts NO debug snapshot (the common case — zero cost).
    debug_active: bool,
    /// Which heavy debug-snapshot sections the open panels want (only meaningful
    /// when `debug_active`).
    debug_detail: DebugDetail,
}

#[wasm_bindgen]
impl Emulator {
    /// Construct the emulator. Async because it must open + hydrate IndexedDB
    /// before building the session (so persisted config/saves are visible to the
    /// first sync read). A static factory rather than a `constructor` —
    /// wasm-bindgen can't emit a valid async constructor.
    pub async fn create() -> Result<Emulator, JsValue> {
        console_error_panic_hook::set_once();

        let storage = IdbStore::open_and_hydrate().await?;
        let config = Config::load(&storage);
        let dmg_palette = config.dmg_palette;

        // Start with an empty (no-cartridge) session; a ROM is inserted later
        // via `load_rom`.
        let ports = Ports {
            storage: Box::new(storage.clone()),
            rumble: Box::new(NullRumble),
            webcam: Box::new(NullWebcam),
        };
        let session = Session::new(config, ports, [0u8; 32]);

        Ok(Emulator {
            session,
            storage,
            input: AbstractInput::none(),
            rgba: vec![0u8; RGBA_LEN],
            frame_w: GB_WIDTH,
            frame_h: GB_HEIGHT,
            audio_scratch: Vec::new(),
            dmg_palette,
            last_ui_state: None,
            has_rom: false,
            debug_active: false,
            debug_detail: DebugDetail::default(),
        })
    }

    /// Load a ROM from raw bytes (an `ArrayBuffer` transferred from the main
    /// thread). `session.apply(LoadRom)` returns a [`PlatformRequest::LoadFile`];
    /// the web frontend already holds the bytes, so we service that request here
    /// by feeding them to `finish_load_rom`. Returns Status/Error requests.
    pub fn load_rom(&mut self, name: &str, bytes: &[u8]) -> Array {
        let outcome = self.session.apply(UiAction::LoadRom(rom_file(name, bytes)), 0);
        let mut extra: Vec<PlatformRequest> = Vec::new();
        for req in outcome.requests {
            if matches!(req, PlatformRequest::LoadFile { .. }) {
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

    /// Load a savestate from raw bytes (a `.rustyboisave` file the main thread
    /// picked and transferred). Re-attaches the currently-loaded ROM. Returns
    /// Status/Error requests.
    pub fn load_state(&mut self, bytes: &[u8]) -> Array {
        let rom_id = self.session.rom_id();
        let mut reqs: Vec<PlatformRequest> = Vec::new();
        match self.session.finish_load_state(bytes, None, rom_id) {
            Ok(()) => {
                reqs.push(PlatformRequest::ClearError);
                reqs.push(PlatformRequest::Status("State loaded".into()));
            }
            Err(e) => reqs.push(PlatformRequest::Error(format!("Failed to load state: {e}"))),
        }
        requests_to_js(&reqs)
    }

    /// Import a battery `.sav` image (bytes the main thread read from a picked
    /// file) into the current cartridge. Persists to IndexedDB (via the session's
    /// storage port) so it survives a reload. Returns Status/Error requests.
    pub fn import_battery(&mut self, bytes: &[u8]) -> Array {
        let reqs = match self.session.finish_import_battery(bytes) {
            Ok(()) => vec![PlatformRequest::Status("Battery save imported".into())],
            Err(e) => vec![PlatformRequest::Error(format!("Failed to import battery save: {e}"))],
        };
        requests_to_js(&reqs)
    }

    /// Import an `.rtc` clock blob into the current cartridge. Returns
    /// Status/Error requests.
    pub fn import_rtc(&mut self, bytes: &[u8]) -> Array {
        let reqs = match self.session.finish_import_rtc(bytes) {
            Ok(()) => vec![PlatformRequest::Status("RTC imported".into())],
            Err(e) => vec![PlatformRequest::Error(format!("Failed to import RTC: {e}"))],
        };
        requests_to_js(&reqs)
    }

    /// Apply an IPS/UPS/BPS ROM patch (bytes the main thread read from a picked
    /// file) to the loaded ROM and re-load the patched cartridge. Returns
    /// Status/Error requests.
    pub fn apply_patch(&mut self, bytes: &[u8]) -> Array {
        let reqs = match self.session.apply_rom_patch(bytes) {
            Ok(_) => vec![
                PlatformRequest::ClearError,
                PlatformRequest::Status("Patch applied".into()),
            ],
            Err(e) => vec![PlatformRequest::Error(format!("Failed to apply patch: {e}"))],
        };
        requests_to_js(&reqs)
    }

    /// Export the current cartridge's battery SRAM, or an empty array when the
    /// cart has no battery. The worker posts these bytes to the main thread,
    /// which triggers a browser download.
    pub fn export_battery(&self) -> js_sys::Uint8Array {
        match self.session.export_battery() {
            Some(bytes) => js_sys::Uint8Array::from(bytes.as_slice()),
            None => js_sys::Uint8Array::new_with_length(0),
        }
    }

    /// Export the current cartridge's RTC blob, or an empty array when there is
    /// no RTC.
    pub fn export_rtc(&self) -> js_sys::Uint8Array {
        match self.session.export_rtc() {
            Some(bytes) => js_sys::Uint8Array::from(bytes.as_slice()),
            None => js_sys::Uint8Array::new_with_length(0),
        }
    }

    /// Export the full machine state (`.rustyboisave`), or an empty array when
    /// serialization fails / no ROM is loaded.
    pub fn export_state(&self) -> js_sys::Uint8Array {
        match self.session.gb().to_state_bytes() {
            Ok(bytes) => js_sys::Uint8Array::from(bytes.as_slice()),
            Err(_) => js_sys::Uint8Array::new_with_length(0),
        }
    }

    /// Advance one presented frame, fill the RGBA framebuffer, and return this
    /// frame's interleaved stereo audio (`[l0,r0,l1,r1,...]`) as a fresh
    /// `Float32Array` for the worker to transfer to the main-thread audio sink.
    /// Empty when no ROM is loaded or the frame produced no audio. After this
    /// returns, [`Emulator::frame`] holds the RGBA and [`Emulator::frame_width`]/
    /// [`Emulator::frame_height`] its size.
    pub fn run_frame(&mut self) -> Float32Array {
        if !self.has_rom {
            return Float32Array::new_with_length(0);
        }
        let out = self.session.run_frame(self.input);
        self.present(&out.frame);

        self.audio_scratch.clear();
        self.audio_scratch.reserve(out.audio.len() * 2);
        for &(l, r) in &out.audio {
            self.audio_scratch.push(l);
            self.audio_scratch.push(r);
        }
        Float32Array::from(self.audio_scratch.as_slice())
    }

    /// Convert the latest presented frame to RGBA into `self.rgba`, preferring
    /// the SGB composite when the border toggle is on and the machine offers one
    /// (mirrors the desktop `App::present`). Sets `frame_w`/`frame_h`.
    fn present(&mut self, frame: &Frame) {
        let sgb = if self.session.sgb_border() {
            self.session.gb().sgb_composited_frame()
        } else {
            None
        };
        if let Some(rgb) = sgb {
            let n = rgb.len() / 3;
            for i in 0..n {
                let s = i * 3;
                let o = i * 4;
                self.rgba[o] = rgb[s];
                self.rgba[o + 1] = rgb[s + 1];
                self.rgba[o + 2] = rgb[s + 2];
                self.rgba[o + 3] = 0xFF;
            }
            self.frame_w = SGB_WIDTH;
            self.frame_h = SGB_HEIGHT;
            return;
        }

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
        self.frame_w = GB_WIDTH;
        self.frame_h = GB_HEIGHT;
    }

    /// Copy the most recent frame's RGBA (`frame_width * frame_height * 4` bytes)
    /// into `out`. The worker passes a pooled `Uint8Array` (recycled from the
    /// main thread after each upload) so no framebuffer is allocated per frame —
    /// that per-frame allocation was the main-thread GC sawtooth.
    pub fn frame_into(&self, out: &js_sys::Uint8Array) {
        let len = (self.frame_w * self.frame_h * 4) as usize;
        out.copy_from(&self.rgba[..len]);
    }

    /// Step back one rewind snapshot (hold-to-rewind, driven by the worker while
    /// Backspace is held). Returns false when the buffer is exhausted. Re-presents
    /// the restored frame into the RGBA buffer for the next `frame_into`.
    pub fn rewind_step(&mut self) -> bool {
        if !self.has_rom || self.session.rewind().is_none() {
            return false;
        }
        let frame = self.session.gb_mut().get_current_frame();
        self.present(&frame);
        true
    }

    /// Width of the RGBA in [`Emulator::frame`] (160 normal, 256 for SGB border).
    pub fn frame_width(&self) -> u32 {
        self.frame_w
    }

    /// Height of the RGBA in [`Emulator::frame`] (144 normal, 224 for SGB border).
    pub fn frame_height(&self) -> u32 {
        self.frame_h
    }

    /// Set the full GB button state from a bitmask (bits 0..7 =
    /// A,B,Start,Select,Up,Down,Left,Right). The main thread sends the union of
    /// keyboard + on-screen egui-touch each time it changes, so lifting a
    /// key/finger releases exactly its buttons.
    pub fn set_input_mask(&mut self, mask: u8) {
        let mut input = AbstractInput::none();
        for b in GbButton::ALL {
            if mask & (1u8 << button_bit(b)) != 0 {
                input.set(b, true);
            }
        }
        self.input = input;
    }

    /// Set which debug snapshot the worker should build each frame. `active` is
    /// whether ANY debug panel is open on the main thread; `bits` is the packed
    /// [`DebugDetail`] (see `DebugDetail::to_bits`). While `active` is false the
    /// worker builds/posts nothing (the common no-panel case), so there is zero
    /// per-frame debug cost until a panel is opened.
    pub fn set_debug_detail(&mut self, active: bool, bits: u8) {
        self.debug_active = active;
        self.debug_detail = DebugDetail::from_bits(bits);
    }

    /// Build the debug snapshot for the current frame and return it bincode-
    /// serialized (the worker transfers the bytes to the main thread, which
    /// deserializes into a `DebugSnapshot` for the egui panels). Returns an empty
    /// array when no panel is open — the worker then posts nothing.
    pub fn take_debug_snapshot(&self) -> js_sys::Uint8Array {
        if !self.debug_active {
            return js_sys::Uint8Array::new_with_length(0);
        }
        let snap = self.session.debug_snapshot(self.debug_detail);
        js_sys::Uint8Array::from(snap.to_bytes().as_slice())
    }

    /// Apply a control action (JSON-encoded [`WebAction`]) through the shared
    /// `Session::apply` contract. Returns the resulting Status/Error/etc.
    /// requests for the worker to forward to the main thread. A palette change
    /// also refreshes the worker's local blit shades so the next frame uses it.
    pub fn apply_action(&mut self, json: &str) -> Result<Array, JsValue> {
        let action: WebAction = serde_json::from_str(json)
            .map_err(|e| JsValue::from_str(&format!("bad action json: {e}")))?;
        let palette_before = self.session.palette();
        let ui_action = action.into_ui_action();
        let outcome = self.session.apply(ui_action, 0);
        if self.session.palette() != palette_before {
            self.dmg_palette = self.session.config().dmg_palette;
        }
        Ok(requests_to_js(&outcome.requests))
    }

    /// The current UI-state snapshot as a JSON string, IF it changed since the
    /// last call (else `None`). The worker posts it to the main thread only on
    /// change, so the egui UI always reflects live session state (slots, cheats,
    /// hardware, palette, fast-forward, touch-controls, rewind config).
    pub fn take_ui_state(&mut self) -> Option<String> {
        let state = self.session_ui_state();
        if self.last_ui_state.as_ref() == Some(&state) {
            return None;
        }
        let json = serde_json::to_string(&WebUiState::from_session(&state)).ok()?;
        self.last_ui_state = Some(state);
        Some(json)
    }

    /// Assemble the shared [`SessionUiState`] view from the session.
    fn session_ui_state(&self) -> SessionUiState {
        SessionUiState {
            hardware: self.session.hardware_choice(),
            palette: self.session.palette(),
            rewind_enabled: self.session.config().rewind.enabled,
            rewind_interval_frames: self.session.config().rewind.interval_frames,
            rewind_depth: self.session.config().rewind.depth,
            volume: self.session.volume(),
            scaling: self.session.scaling_mode(),
            sgb_border: self.session.sgb_border(),
            fast_forward: self.session.is_fast_forward(),
            touch_controls: self.session.touch_controls(),
            slots: self.session.list_slots(),
            cheats: self.session.cheats().map(str::to_owned).collect(),
            has_battery: self.session.has_battery(),
            has_rtc: self.session.has_rtc(),
            has_rom: self.has_rom,
            input: self.session.input_config().clone(),
        }
    }

    /// The current hardware model as a lowercase string ("dmg" / "cgb").
    pub fn hardware(&self) -> String {
        match self.session.hardware() {
            Hardware::DMG => "dmg".into(),
            _ => "cgb".into(),
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

/// The bit position for a GB button in the input bitmask exchanged between the
/// main thread and the worker. Mirrors `overlay::button_bit` so the touch
/// overlay and this input path agree.
fn button_bit(b: GbButton) -> u8 {
    match b {
        GbButton::A => 0,
        GbButton::B => 1,
        GbButton::Start => 2,
        GbButton::Select => 3,
        GbButton::Up => 4,
        GbButton::Down => 5,
        GbButton::Left => 6,
        GbButton::Right => 7,
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
/// is a no-op on web and is dropped; `SaveStateBytes`/`LoadFile` are serviced
/// in-worker and never appear here for the actions the web frontend issues.
fn requests_to_js(requests: &[PlatformRequest]) -> Array {
    let out = Array::new();
    for req in requests {
        let o = js_sys::Object::new();
        let set = |k: &str, v: JsValue| {
            let _ = js_sys::Reflect::set(&o, &k.into(), &v);
        };
        match req {
            PlatformRequest::Exit => continue, // no-op on web
            // Fullscreen is handled on the main thread (canvas Fullscreen API); a
            // web `WebAction` never lowers it to the worker, so it never reaches
            // here — drop it defensively.
            PlatformRequest::ToggleFullscreen => continue,
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
            // Serviced inside the worker for the web frontend and not expected
            // from the actions it issues; surface as a status so nothing is lost.
            // (Export SaveBytes is serviced by the dedicated `export_*` methods,
            // not through `apply`, so it never reaches here.)
            PlatformRequest::SaveStateBytes { .. }
            | PlatformRequest::SaveBytes { .. }
            | PlatformRequest::LoadFile { .. } => {
                set("type", "Status".into());
                set("msg", "unhandled platform request".into());
            }
        }
        out.push(&o);
    }
    out
}

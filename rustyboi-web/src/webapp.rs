//! The main-thread egui driver.
//!
//! Renders the SAME egui UI as desktop/Android on the web: it owns a winit
//! canvas, a wgpu **WebGL2** surface, and the portable `rustyboi-frontend`
//! [`Renderer`] + [`UiHost`]. Each animation frame it draws the worker's latest
//! framebuffer as the game texture and composites the egui `Gui` (menus, cheats,
//! keybind remap, palette, hardware, slots, settings) on top. The `UiAction`s
//! egui emits are posted to the worker (which owns the `Session`); GB input
//! (keyboard + the egui on-screen touch overlay) is posted as a button bitmask.
//!
//! Emulation NEVER runs here — it stays in the worker so a 175 Hz display can't
//! jank it. This thread only renders + routes input/actions.
//!
//! ## Bridge
//! [`WebApp`] is the wasm-bindgen handle the JS shell keeps. JS owns the worker
//! and, as worker messages arrive, pushes into `WebApp` (`on_frame`,
//! `on_ui_state`, `on_status`, `on_error`, `clear_error`). Those mutate a shared
//! [`Shared`] cell the spawned winit loop reads each redraw. Outbound, the loop
//! invokes JS callbacks (supplied at construction) to post actions/input/loads
//! to the worker.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys};
use winit::window::{Window, WindowId};

use rustyboi_frontend_lib::renderer::{GameFrame, Renderer, SourceSize};
use rustyboi_frontend_lib::ui_host::UiHost;
use rustyboi_session::input_config::{
    FiredHotkey, HeldInputs, HotkeyAction, InputTrigger, KeyName, PadButton, ResolveState,
};
use rustyboi_session::{DebugSnapshot, GbButton, SessionUiState, UiAction};

use crate::web_action::WebAction;

/// State shared between the JS-facing [`WebApp`] handle and the spawned winit
/// event loop. The JS shell writes the worker's frames/UI-state/status in; the
/// loop reads them each redraw and calls back out through the JS closures.
struct Shared {
    /// Latest framebuffer from the worker (RGBA), and its source size.
    frame_rgba: Vec<u8>,
    frame_size: SourceSize,
    /// Set when a new frame arrived; the loop uploads + clears it.
    frame_dirty: bool,
    /// Latest session UI-state snapshot (what the egui `Gui` renders from).
    ui_state: SessionUiState,
    /// A pending error to surface in the egui overlay.
    error: Option<String>,
    /// A pending status line to surface.
    status: Option<String>,
    /// Whether a `clear_error` was requested.
    clear_error: bool,
    /// Something changed the UI (new session snapshot / status / error) since the
    /// last render, so egui must re-run — see the repaint gating in `draw`. Frame
    /// arrivals do NOT set this (the game texture is uploaded separately).
    ui_dirty: bool,

    /// Last GB button bitmask posted to the worker (dedupe: only post changes).
    last_input_mask: u8,

    /// Latest debug read-model from the worker (deserialized), rendered by the
    /// egui debug panels. `None` until the first snapshot arrives / while no
    /// panel is open.
    debug_snapshot: Option<DebugSnapshot>,
    /// Last `(active, bits)` debug-detail posted to the worker, so we only re-post
    /// when the set of open panels changes.
    last_debug_detail: Option<(bool, u8)>,

    // Outbound JS callbacks to the worker (installed by JS at construction):
    /// `(jsonAction: string) => void` — post a `WebAction` to the worker.
    post_action: js_sys::Function,
    /// `(mask: number) => void` — post a GB button bitmask to the worker.
    post_input: js_sys::Function,
    /// `(name: string, bytes: Uint8Array) => void` — worker `load_rom`.
    load_rom: js_sys::Function,
    /// `(bytes: Uint8Array) => void` — worker `load_state`.
    load_state: js_sys::Function,
    /// `(on: boolean) => void` — set the worker's hold-to-rewind state.
    set_rewind: js_sys::Function,
    /// `(purpose: string, name: string, bytes: Uint8Array) => void` — post a
    /// picked import file to the worker (purpose ∈ state|battery|rtc|patch).
    import_file: js_sys::Function,
    /// `(kind: string) => void` — ask the worker to produce export bytes
    /// (kind ∈ state|battery|rtc); the worker posts them back for JS to download.
    request_export: js_sys::Function,
    /// `(active: boolean, bits: number) => void` — tell the worker which debug
    /// snapshot to build (which panels are open). Posted only on change.
    post_debug_detail: js_sys::Function,
    /// `() => void` — toggle canvas fullscreen (main-thread DOM; the worker is
    /// not involved). Mirrors `request_export` as an outbound JS bridge.
    toggle_fullscreen: js_sys::Function,
}

impl Shared {
    #[allow(clippy::too_many_arguments)]
    fn new(
        post_action: js_sys::Function,
        post_input: js_sys::Function,
        load_rom: js_sys::Function,
        load_state: js_sys::Function,
        set_rewind: js_sys::Function,
        import_file: js_sys::Function,
        request_export: js_sys::Function,
        post_debug_detail: js_sys::Function,
        toggle_fullscreen: js_sys::Function,
    ) -> Self {
        Shared {
            frame_rgba: Vec::new(),
            frame_size: SourceSize::Gb,
            frame_dirty: false,
            ui_state: SessionUiState::default(),
            error: None,
            status: None,
            clear_error: false,
            ui_dirty: true,
            last_input_mask: 0,
            debug_snapshot: None,
            last_debug_detail: None,
            post_action,
            post_input,
            load_rom,
            load_state,
            set_rewind,
            import_file,
            request_export,
            post_debug_detail,
            toggle_fullscreen,
        }
    }
}

/// The main-thread egui driver handle exposed to JavaScript.
#[wasm_bindgen]
pub struct WebApp {
    shared: Rc<RefCell<Shared>>,
    started: bool,
}

#[wasm_bindgen]
impl WebApp {
    /// Build the driver. The callbacks bridge OUT to the worker (the JS shell
    /// wires them to `worker.postMessage`): `post_action(json)`,
    /// `post_input(mask)`, `load_rom(name, bytes)`, `load_state(bytes)`,
    /// `set_rewind(on)`.
    #[allow(clippy::too_many_arguments)]
    #[wasm_bindgen(constructor)]
    pub fn new(
        post_action: js_sys::Function,
        post_input: js_sys::Function,
        load_rom: js_sys::Function,
        load_state: js_sys::Function,
        set_rewind: js_sys::Function,
        import_file: js_sys::Function,
        request_export: js_sys::Function,
        post_debug_detail: js_sys::Function,
        toggle_fullscreen: js_sys::Function,
    ) -> WebApp {
        console_error_panic_hook::set_once();
        WebApp {
            shared: Rc::new(RefCell::new(Shared::new(
                post_action,
                post_input,
                load_rom,
                load_state,
                set_rewind,
                import_file,
                request_export,
                post_debug_detail,
                toggle_fullscreen,
            ))),
            started: false,
        }
    }

    /// Push the worker's latest framebuffer (RGBA bytes + pixel size). The next
    /// redraw uploads it as the game texture. `width`/`height` are 160×144 for a
    /// normal frame or 256×224 for the SGB border composite.
    pub fn on_frame(&self, rgba: &[u8], width: u32, _height: u32) {
        let mut s = self.shared.borrow_mut();
        s.frame_size = if width == 256 { SourceSize::Sgb } else { SourceSize::Gb };
        s.frame_rgba.clear();
        s.frame_rgba.extend_from_slice(rgba);
        s.frame_dirty = true;
    }

    /// Push a new UI-state snapshot (JSON [`crate::web_action::WebUiState`]) so
    /// the egui menus/cheats/settings reflect live session state.
    pub fn on_ui_state(&self, json: &str) {
        if let Ok(state) = serde_json::from_str::<crate::web_action::WebUiState>(json) {
            let title = match state.game_name.as_deref() {
                Some(g) => format!("{g} — RustyBoi"),
                None => "RustyBoi".to_string(),
            };
            {
                let mut s = self.shared.borrow_mut();
                s.ui_state = state.into_session();
                s.ui_dirty = true;
            }
            // Reflect the identified game in the browser tab title. Snapshots
            // only arrive on change, so this runs rarely.
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                doc.set_title(&title);
            }
        }
    }

    /// Push a fresh debug snapshot (bincode bytes the worker built + transferred).
    /// Deserialized into a [`DebugSnapshot`] the egui debug panels render from.
    pub fn on_debug_snapshot(&self, bytes: &[u8]) {
        if let Some(snap) = DebugSnapshot::from_bytes(bytes) {
            let mut s = self.shared.borrow_mut();
            s.debug_snapshot = Some(snap);
            s.ui_dirty = true;
        }
    }

    /// Surface a status line in the UI.
    pub fn on_status(&self, msg: String) {
        let mut s = self.shared.borrow_mut();
        s.status = Some(msg);
        s.ui_dirty = true;
    }

    /// Surface an error overlay in the UI.
    pub fn on_error(&self, msg: String) {
        let mut s = self.shared.borrow_mut();
        s.error = Some(msg);
        s.ui_dirty = true;
    }

    /// Clear the error overlay (a load succeeded / the error was dismissed).
    pub fn clear_error(&self) {
        let mut s = self.shared.borrow_mut();
        s.clear_error = true;
        s.ui_dirty = true;
    }

    /// Create the canvas + wgpu WebGL2 surface, build the renderer/UI, and spawn
    /// the winit render loop. `canvas` is the `<canvas>` egui draws into (the JS
    /// shell created + sized it). Idempotent-guarded: only starts once.
    pub async fn start(&mut self, canvas: HtmlCanvasElement) -> Result<(), JsValue> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        run_loop(self.shared.clone(), canvas).map_err(|e| JsValue::from_str(&e))
    }
}

/// The window + GPU renderer + egui host, built asynchronously once the winit
/// window exists (wgpu adapter/device requests are async on the web).
struct WebRender {
    window: Arc<Window>,
    renderer: Renderer,
    ui: UiHost,
}

/// The winit 0.30 `ApplicationHandler` for the web driver. Creates the window in
/// `resumed` (winit 0.30 requires an `ActiveEventLoop`) then kicks off the async
/// wgpu init; the finished `WebRender` lands in `pending_render` and is adopted
/// on the next event tick.
struct WebGuiApp {
    shared: Rc<RefCell<Shared>>,
    /// The JS-created canvas, moved into the window on first `resumed`.
    canvas: Option<HtmlCanvasElement>,
    /// Filled by the async wgpu-init task, then moved into `render`.
    pending_render: Rc<RefCell<Option<WebRender>>>,
    render: Option<WebRender>,
    started_init: bool,
    /// Raw keyboard keys held this frame (host-agnostic names); resolved against
    /// the session's InputConfig in `draw`.
    held_keys: HashSet<KeyName>,
    resolve_state: ResolveState,
    rewind_held: bool,
}

impl WebGuiApp {
    /// Move a completed async-built `WebRender` out of the shared cell.
    fn adopt_pending_render(&mut self) {
        if self.render.is_none() {
            if let Some(r) = self.pending_render.borrow_mut().take() {
                r.window.request_redraw();
                self.render = Some(r);
            }
        }
    }
}

impl ApplicationHandler for WebGuiApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.started_init {
            return;
        }
        self.started_init = true;
        // Wait, NOT Poll. On web, Poll makes winit reschedule an immediate wakeup
        // via Scheduler.postTask every iteration, and dropping the previous
        // schedule calls AbortController.abort() — which Firefox implements by
        // walking + saving a full stack, costing ~40% of the main thread. Our
        // render loop is driven purely by request_redraw() (mapped to
        // requestAnimationFrame) in about_to_wait, so Wait gives the same
        // continuous rAF cadence with none of the postTask/abort churn.
        event_loop.set_control_flow(ControlFlow::Wait);

        let Some(canvas) = self.canvas.take() else { return };
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        // winit adopts the JS-created canvas (don't append a second one). Prevent
        // default so arrow keys/space don't scroll the page while playing.
        let attrs = Window::default_attributes()
            .with_canvas(Some(canvas))
            .with_prevent_default(true)
            .with_append(false);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                web_sys::console::error_1(&format!("Window build: {e}").into());
                return;
            }
        };

        // wgpu adapter/device requests are async on the web; build off-thread and
        // drop the result into `pending_render` for the next event tick to adopt.
        let pending = self.pending_render.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match build_web_render(window, width, height).await {
                Ok(r) => *pending.borrow_mut() = Some(r),
                Err(e) => web_sys::console::error_1(&e.into()),
            }
        });
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        self.adopt_pending_render();
        let Some(render) = self.render.as_mut() else { return };

        // Feed egui (mouse/keyboard/touch/IME + text entry for the cheat and
        // keybind panels). GB input is derived separately below. But keep keys the
        // config binds to a hotkey (e.g. Tab = fast-forward) AWAY from egui while
        // playing — otherwise egui uses Tab for focus traversal (cursor jumps into
        // the menu bar) and eats Backspace. When a text field is focused they
        // belong to the UI, so forward.
        let hotkey_for_game = !render.ui.wants_keyboard_input()
            && matches!(&event,
                WindowEvent::KeyboardInput { event: k, .. }
                    if is_hotkey_key(&self.shared, k));
        if !hotkey_for_game {
            render.ui.handle_event(&render.window, &event);
        }
        match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                // While the user is typing in an egui text field (cheat / keybind
                // entry) the keyboard belongs to the UI: no GB input, no feature
                // hotkeys, and release any held rewind.
                if render.ui.wants_keyboard_input() {
                    self.held_keys.clear();
                    set_rewind(&self.shared, &mut self.rewind_held, false);
                } else {
                    update_held_keys(&mut self.held_keys, &key);
                }
            }
            WindowEvent::Focused(false) => {
                self.held_keys.clear();
                set_rewind(&self.shared, &mut self.rewind_held, false);
            }
            WindowEvent::Resized(size) => {
                render.renderer.resize(size.width.max(1), size.height.max(1));
                render.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                render.ui.set_pixels_per_point(scale_factor as f32);
                render.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                draw(
                    &self.shared,
                    &render.window,
                    &mut render.ui,
                    &mut render.renderer,
                    &self.held_keys,
                    &mut self.resolve_state,
                    &mut self.rewind_held,
                );
            }
            WindowEvent::CloseRequested => {}
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.adopt_pending_render();
        // Drive a continuous redraw so the UI + latest worker frame keep updating
        // (the browser paces this via requestAnimationFrame).
        if let Some(render) = self.render.as_ref() {
            render.window.request_redraw();
        }
    }
}

/// Build the wgpu WebGL2 surface/device and the frontend `Renderer` + `UiHost`
/// for `window`. Async because the browser resolves adapter/device requests via
/// promises.
async fn build_web_render(
    window: Arc<Window>,
    width: u32,
    height: u32,
) -> Result<WebRender, String> {
    // wgpu on wasm MUST use the GL backend (WebGL2) — WebGPU is not Firefox
    // stable. The `webgl` feature (rustyboi-frontend Cargo.toml) provides it.
    // wgpu 29's `InstanceDescriptor` holds a non-Default `display` field, so set
    // the fields explicitly rather than spreading `..Default::default()`.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::GL,
        flags: wgpu::InstanceFlags::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| format!("create_surface: {e}"))?;

    // wgpu 29: `request_adapter` returns a `Result` (was `Option`).
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        })
        .await
        .map_err(|e| format!("no compatible WebGL2 adapter: {e}"))?;

    // WebGL2 caps: request the downlevel-webgl2 limit set so the device request
    // succeeds on browsers (full desktop limits would be rejected). wgpu 29
    // dropped the trailing trace arg and added memory_hints/trace/experimental.
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("rustyboi_web_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                .using_resolution(adapter.limits()),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("request_device: {e}"))?;

    let caps = surface.get_capabilities(&adapter);
    // Prefer a non-sRGB surface format (mirrors desktop). `Renderer::new` derives
    // the game texture's sRGB-ness FROM this surface format, so the GB frame's
    // already-sRGB-encoded RGB is presented pass-through (unorm texture ↔ unorm
    // surface here), not double-linearized/too-dark. Fall back to the first
    // advertised format if the browser offers only sRGB.
    let surface_format = caps
        .formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .unwrap_or(caps.formats[0]);

    let max_texture_size = device.limits().max_texture_dimension_2d as usize;
    let scale_factor = window.scale_factor() as f32;

    // Web: the browser drives presentation via requestAnimationFrame; Fifo
    // matches that and is universally supported.
    let renderer = Renderer::new(
        surface, device, queue, surface_format, width, height, wgpu::PresentMode::Fifo,
    );
    let ui = UiHost::new(&window, scale_factor, max_texture_size, None);
    Ok(WebRender { window, renderer, ui })
}

/// Build the winit event loop, then hand it a [`WebGuiApp`] to drive. Returns as
/// soon as the loop is spawned (it runs via the browser event loop from then on).
fn run_loop(shared: Rc<RefCell<Shared>>, canvas: HtmlCanvasElement) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| format!("EventLoop::new: {e}"))?;
    let app = WebGuiApp {
        shared,
        canvas: Some(canvas),
        pending_render: Rc::new(RefCell::new(None)),
        render: None,
        started_init: false,
        held_keys: HashSet::new(),
        resolve_state: ResolveState::new(),
        rewind_held: false,
    };
    event_loop.spawn_app(app);
    Ok(())
}

/// Whether `key` is bound (as a single-key chord) to a hotkey in the current
/// config. Such keys are held out of egui while playing so egui doesn't use Tab
/// for focus traversal or consume Backspace as an edit.
fn is_hotkey_key(shared: &Rc<RefCell<Shared>>, key: &KeyEvent) -> bool {
    let Some(name) = key_name(key) else { return false };
    let s = shared.borrow();
    s.ui_state.input.hotkeys.iter().any(|h| {
        h.chord
            .iter()
            .any(|t| matches!(t, InputTrigger::Key(k) if *k == name))
    })
}

/// Map a winit `KeyCode` (browser physical key) to the host-agnostic [`KeyName`].
fn key_name(key: &KeyEvent) -> Option<KeyName> {
    let PhysicalKey::Code(code) = key.physical_key else { return None };
    rustyboi_frontend_lib::keymap::key_name(code)
}

/// Update the held-keys set from a key event (host-agnostic names). Keys with
/// no [`KeyName`] mapping are ignored.
fn update_held_keys(held: &mut HashSet<KeyName>, key: &KeyEvent) {
    let Some(name) = key_name(key) else { return };
    match key.state {
        ElementState::Pressed => {
            held.insert(name);
        }
        ElementState::Released => {
            held.remove(&name);
        }
    }
}

/// Post the hold-to-rewind state to the worker, but only on a change (the worker
/// steps back through its rewind buffer while this is on).
fn set_rewind(shared: &Rc<RefCell<Shared>>, held: &mut bool, on: bool) {
    if *held == on {
        return;
    }
    *held = on;
    let s = shared.borrow();
    let f = s.set_rewind.clone();
    drop(s);
    let _ = f.call1(&JsValue::NULL, &JsValue::from_bool(on));
}

/// Run one egui frame + composite: apply pending status/error, run the UI,
/// dispatch the action it emits (to the worker), forward GB input, and render.
fn draw(
    shared: &Rc<RefCell<Shared>>,
    window: &Window,
    ui: &mut UiHost,
    renderer: &mut Renderer,
    held_keys: &HashSet<KeyName>,
    resolve_state: &mut ResolveState,
    rewind_held: &mut bool,
) {
    // Pull the shared inputs for this frame, releasing the borrow before running
    // egui (the rfd file-picker callback can re-enter `shared` via JS).
    let (ui_state, error, status, clear_err, force_repaint, debug_snapshot): (
        SessionUiState,
        Option<String>,
        Option<String>,
        bool,
        bool,
        Option<DebugSnapshot>,
    ) = {
        let mut s = shared.borrow_mut();
        // Upload the game texture straight from the shared buffer while borrowed —
        // no clone to carry ownership past the borrow. render() below draws the
        // retained texture (has_game) with game: None.
        if s.frame_dirty {
            s.frame_dirty = false;
            renderer.upload_game(&GameFrame { size: s.frame_size, rgba: &s.frame_rgba });
        }
        (
            s.ui_state.clone(),
            s.error.take(),
            s.status.take(),
            std::mem::take(&mut s.clear_error),
            std::mem::take(&mut s.ui_dirty),
            s.debug_snapshot.clone(),
        )
    };

    if clear_err {
        ui.clear_error();
    }
    if let Some(e) = error {
        ui.set_error(e);
    }
    if let Some(msg) = status {
        ui.set_status(msg);
    }

    // Tell the worker which debug snapshot to build from the panels open THIS
    // frame (the Gui lives here on the main thread). Posted only on change; when
    // no panel is open we post (false, 0) once and the worker then builds/posts
    // nothing — zero per-frame debug cost in the common case.
    let debug_open = ui.any_debug_panel_open();
    let detail_bits = ui.wanted_debug_detail().to_bits();
    {
        let mut s = shared.borrow_mut();
        if s.last_debug_detail != Some((debug_open, detail_bits)) {
            s.last_debug_detail = Some((debug_open, detail_bits));
            // Dropping the stale snapshot when panels close stops the panels from
            // rendering old bytes the instant one is reopened.
            if !debug_open {
                s.debug_snapshot = None;
            }
            let f = s.post_debug_detail.clone();
            drop(s);
            let _ = f.call2(
                &JsValue::NULL,
                &JsValue::from_bool(debug_open),
                &JsValue::from_f64(detail_bits as f64),
            );
        }
    }

    // Pass the worker's latest debug snapshot to the panels only while a panel is
    // open (Phase C — web debug views). The paused state (below) comes from the
    // worker snapshot, since pause lives in the session's run mode on web.
    let debug_ref = if debug_open { debug_snapshot.as_ref() } else { None };
    // Debug panels are live views fed by the worker each frame — force a repaint
    // while any is open so a freshly-arrived snapshot always re-renders (repaint
    // gating would otherwise reuse the cached frame and the panel would never show).
    // Held gamepad buttons, computed before the UI so the keybind editor can
    // capture controller presses (egui never sees pad input); reused for the GB
    // input resolve below.
    let pad = gamepad_pad_held();
    // The menu bar auto-hides while the canvas is in the Fullscreen API.
    let fullscreen = web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| d.fullscreen_element().is_some());
    let (paint, ui_frame) = ui.run(
        window,
        rustyboi_frontend_lib::ui_host::UiRunInputs {
            paused: ui_state.paused,
            debug: debug_ref,
            fullscreen,
            session: &ui_state,
            extra_events: Vec::new(),
            held_pad: &pad,
            force_repaint: force_repaint || debug_open,
        },
    );

    // Dispatch the action egui emitted.
    if let Some(action) = ui_frame.action {
        dispatch_action(shared, action);
    }

    // Resolve GB input + hotkeys through the shared config on the main thread
    // (the worker only sees the resulting button mask). Raw held inputs = the
    // physical keyboard + the Gamepad-API buttons; the resolver applies the
    // GB-button bindings and chord hotkeys from `ui_state.input`.
    let held = HeldInputs {
        keys: held_keys.clone(),
        pad,
    };
    let (mut button_state, fired) = ui_state.input.resolve(&held, resolve_state);
    // Union the egui on-screen touch overlay on top of the resolved buttons.
    let touch = ui.touch_button_state();
    button_state.a |= touch.a;
    button_state.b |= touch.b;
    button_state.start |= touch.start;
    button_state.select |= touch.select;
    button_state.up |= touch.up;
    button_state.down |= touch.down;
    button_state.left |= touch.left;
    button_state.right |= touch.right;

    dispatch_hotkeys(shared, &fired, rewind_held);

    // Forward the resolved GB button mask to the worker. Only post on change.
    let mask = input_mask(button_state);
    {
        let mut s = shared.borrow_mut();
        if mask != s.last_input_mask {
            s.last_input_mask = mask;
            let f = s.post_input.clone();
            drop(s);
            let _ = f.call1(&JsValue::NULL, &JsValue::from_f64(mask as f64));
        }
    }

    // Push the current presentation policy from the session UI snapshot before
    // render (mirroring the desktop App::draw): letterboxing, texture filter and
    // the LCD post-process effect. On web the renderer lives on the main thread
    // (the session is in the worker), so these must be pushed here too.
    renderer.set_scaling_mode(ui_state.scaling);
    renderer.set_texture_filter(ui_state.texture_filter);
    renderer.set_lcd_effect(ui_state.lcd_effect);

    // Render: the game texture (uploaded above) letterboxed into the central
    // region, egui on top. game: None — the retained texture is drawn via has_game.
    if let Err(status) = renderer.render(None, ui_frame.region, paint) {
        // wgpu 29: reconfigure + retry next frame on Lost/Outdated. Timeout /
        // Occluded are handled inside render() (mapped to Ok); Validation is
        // surfaced via the device error scope, so skip it here.
        match status {
            wgpu::SurfaceStatus::Lost | wgpu::SurfaceStatus::Outdated => {
                let (w, h) = renderer.surface_size();
                renderer.resize(w, h);
            }
            _ => {}
        }
    }
}

/// Route the `UiAction` egui produced. Loads resolve on the main thread (the
/// rfd picker already read the bytes into `FileData::Contents`); everything else
/// the worker can service is lowered to a [`WebAction`] and posted as JSON.
/// Debug/OS-only actions are dropped (Phase B).
fn dispatch_action(shared: &Rc<RefCell<Shared>>, action: UiAction) {
    match action {
        UiAction::LoadRom(file) => {
            // On wasm the picked file always arrives as bytes (`Contents`).
            if let Some((name, data)) = file_contents(file) {
                let s = shared.borrow();
                let cb = s.load_rom.clone();
                drop(s);
                let bytes = js_sys::Uint8Array::from(data.as_slice());
                let _ = cb.call2(&JsValue::NULL, &JsValue::from_str(&name), &bytes);
            }
        }
        UiAction::LoadState(file) => {
            if let Some((_, data)) = file_contents(file) {
                let s = shared.borrow();
                let cb = s.load_state.clone();
                drop(s);
                let bytes = js_sys::Uint8Array::from(data.as_slice());
                let _ = cb.call1(&JsValue::NULL, &bytes);
            }
        }
        // Imports: the rfd picker already read the file into `Contents`; post the
        // bytes + purpose to the worker, which feeds the right `finish_import_*`.
        UiAction::ImportState(file) => post_import(shared, "state", file),
        UiAction::ImportBatterySave(file) => post_import(shared, "battery", file),
        UiAction::ImportRtc(file) => post_import(shared, "rtc", file),
        UiAction::ApplyPatch(file) => post_import(shared, "patch", file),
        UiAction::LoadMovie(file) => post_import(shared, "movie", file),
        // Exports: the worker owns the session bytes, so ask it to produce them;
        // it posts them back and the JS shell triggers the browser download.
        UiAction::ExportState => request_export(shared, "state"),
        UiAction::ExportBatterySave => request_export(shared, "battery"),
        UiAction::ExportRtc => request_export(shared, "rtc"),
        // Fullscreen is a main-thread DOM op (canvas Fullscreen API); the worker
        // is not involved, so call the bridge here rather than posting a WebAction.
        UiAction::ToggleFullscreen => {
            let s = shared.borrow();
            let cb = s.toggle_fullscreen.clone();
            drop(s);
            let _ = cb.call0(&JsValue::NULL);
        }
        other => {
            if let Some(web_action) = WebAction::from_ui_action(&other) {
                if let Ok(json) = serde_json::to_string(&web_action) {
                    let s = shared.borrow();
                    let cb = s.post_action.clone();
                    drop(s);
                    let _ = cb.call1(&JsValue::NULL, &JsValue::from_str(&json));
                }
            }
            // Dropped: SaveState-to-path (web uses ExportState / slots), Exit,
            // and the debug actions (breakpoints/stepping) that need a Phase-B
            // &GB snapshot.
        }
    }
}

/// Post a picked import file to the worker with its `purpose` (state|battery|
/// rtc|patch). The rfd picker already read the bytes into `Contents`.
fn post_import(shared: &Rc<RefCell<Shared>>, purpose: &str, file: rustyboi_session::FileData) {
    let Some((name, data)) = file_contents(file) else { return };
    let s = shared.borrow();
    let cb = s.import_file.clone();
    drop(s);
    let bytes = js_sys::Uint8Array::from(data.as_slice());
    let _ = cb.call3(
        &JsValue::NULL,
        &JsValue::from_str(purpose),
        &JsValue::from_str(&name),
        &bytes,
    );
}

/// Ask the worker to produce export bytes for `kind` (state|battery|rtc); it
/// posts them back for the JS shell to download.
fn request_export(shared: &Rc<RefCell<Shared>>, kind: &str) {
    let s = shared.borrow();
    let cb = s.request_export.clone();
    drop(s);
    let _ = cb.call1(&JsValue::NULL, &JsValue::from_str(kind));
}

/// Pack a core `ButtonState` into the A,B,Start,Select,Up,Down,Left,Right
/// bitmask the worker's `set_input_mask` expects (bit layout mirrors
/// `overlay::button_bit`).
fn input_mask(state: rustyboi_session::ButtonState) -> u8 {
    let mut mask = 0u8;
    let mut set = |b: GbButton, on: bool| {
        if on {
            mask |= 1u8 << button_bit(b);
        }
    };
    set(GbButton::A, state.a);
    set(GbButton::B, state.b);
    set(GbButton::Start, state.start);
    set(GbButton::Select, state.select);
    set(GbButton::Up, state.up);
    set(GbButton::Down, state.down);
    set(GbButton::Left, state.left);
    set(GbButton::Right, state.right);
    mask
}

/// Dispatch the hotkeys the resolver fired this frame. Fast-forward and rewind
/// have worker paths (reuse `set_rewind` / the `ToggleFastForward` action);
/// quicksave/quickload/pause route through the worker via a `WebAction`; exit and
/// fullscreen are main-thread DOM ops (exit closes nothing on web, so it's a
/// no-op; fullscreen calls the canvas bridge). Turbo is baked into the button
/// state by the resolver, so it needs no dispatch here.
fn dispatch_hotkeys(shared: &Rc<RefCell<Shared>>, fired: &[FiredHotkey], rewind_held: &mut bool) {
    // Rewind is a hold action: engage while active, release when it stops firing.
    let want_rewind = fired.iter().any(|f| matches!(f.action, HotkeyAction::Rewind));
    set_rewind(shared, rewind_held, want_rewind);

    for f in fired {
        match f.action {
            HotkeyAction::FastForward if f.rising => {
                dispatch_action(shared, UiAction::ToggleFastForward);
            }
            HotkeyAction::Quicksave if f.rising => dispatch_action(shared, UiAction::Quicksave),
            HotkeyAction::Quickload if f.rising => dispatch_action(shared, UiAction::Quickload),
            HotkeyAction::FrameAdvance if f.rising => dispatch_action(shared, UiAction::FrameAdvance),
            HotkeyAction::TogglePause if f.rising => dispatch_action(shared, UiAction::TogglePause),
            HotkeyAction::ToggleFullscreen if f.rising => {
                dispatch_action(shared, UiAction::ToggleFullscreen);
            }
            _ => {}
        }
    }
}

/// Poll connected gamepads (the Gamepad API) and collect their held buttons as
/// abstract [`PadButton`]s. Standard mapping: 0=South,1=East,2=West,3=North,
/// 8=Select,9=Start,12..15=D-pad,4/5=L1/R1,6/7=L2/R2; D-pad also honors the left
/// stick (axes 0/1, web Y is +down). Empty with no gamepad; re-polled each frame.
fn gamepad_pad_held() -> HashSet<PadButton> {
    let mut held = HashSet::new();
    let Some(win) = web_sys::window() else { return held };
    let Ok(pads) = win.navigator().get_gamepads() else { return held };
    for i in 0..pads.length() {
        let Ok(pad) = pads.get(i).dyn_into::<web_sys::Gamepad>() else { continue };
        let buttons = pad.buttons();
        let pressed = |idx: u32| {
            buttons
                .get(idx)
                .dyn_into::<web_sys::GamepadButton>()
                .map(|b| b.pressed())
                .unwrap_or(false)
        };
        let axes = pad.axes();
        let axis = |idx: u32| axes.get(idx).as_f64().unwrap_or(0.0);
        let mut hold = |cond: bool, b: PadButton| {
            if cond {
                held.insert(b);
            }
        };
        hold(pressed(0), PadButton::South);
        hold(pressed(1), PadButton::East);
        hold(pressed(2), PadButton::West);
        hold(pressed(3), PadButton::North);
        hold(pressed(8), PadButton::Select);
        hold(pressed(9), PadButton::Start);
        hold(pressed(4), PadButton::LeftShoulder);
        hold(pressed(5), PadButton::RightShoulder);
        hold(pressed(6), PadButton::LeftTrigger);
        hold(pressed(7), PadButton::RightTrigger);
        hold(pressed(12), PadButton::DpadUp);
        hold(pressed(13), PadButton::DpadDown);
        hold(pressed(14), PadButton::DpadLeft);
        hold(pressed(15), PadButton::DpadRight);
        // Analog sticks as discrete directions (web axes: 0/1 = left X/Y, 2/3 =
        // right X/Y, +Y is down). Bound with the d-pad by default, separately
        // mappable — sticks and d-pad interchangeable.
        hold(axis(1) < -0.5, PadButton::LStickUp);
        hold(axis(1) > 0.5, PadButton::LStickDown);
        hold(axis(0) < -0.5, PadButton::LStickLeft);
        hold(axis(0) > 0.5, PadButton::LStickRight);
        hold(axis(3) < -0.5, PadButton::RStickUp);
        hold(axis(3) > 0.5, PadButton::RStickDown);
        hold(axis(2) < -0.5, PadButton::RStickLeft);
        hold(axis(2) > 0.5, PadButton::RStickRight);
    }
    held
}

/// Extract `(name, bytes)` from a picked file. On wasm the rfd picker always
/// yields the byte-carrying `Contents` variant; the `Path` arm exists only so
/// this typechecks against the host `FileData` in a `cargo build --workspace`.
fn file_contents(file: rustyboi_session::FileData) -> Option<(String, Vec<u8>)> {
    match file {
        rustyboi_session::FileData::Contents { name, data } => Some((name, data)),
        #[cfg(not(target_arch = "wasm32"))]
        rustyboi_session::FileData::Path(_) => None,
    }
}

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

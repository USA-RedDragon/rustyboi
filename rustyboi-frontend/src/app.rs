//! The portable application: the old platform `World` plus the GuiAction
//! dispatch from the event loop, with every winit/window/OS specific moved out.
//!
//! `App` owns the `Session` (emulator + feature logic), the egui `UiHost`, the
//! presentation palette, and the run/pause/error bookkeeping. The platform crate
//! drives it: it pumps input, calls [`App::run_frame`] to advance emulation, and
//! [`App::draw`] to run the UI + render. OS-only work the UI asks for (exit,
//! resize, file reads/writes, printer sinks, rewind serialization) is surfaced
//! as [`PlatformRequest`]s the platform performs, or fed in through explicit
//! byte-level methods — so the app itself does no filesystem I/O, spawns no
//! threads, and reads no clock beyond frame pacing.

use std::time::{Duration, Instant};

use rustyboi_core_lib::{gb, input, ppu};
use rustyboi_session::action::FileData;
use rustyboi_session::{AbstractInput, GbButton, RunMode, Session, SessionUiState};
#[cfg(target_os = "android")]
use rustyboi_session::UiAction;

use rustyboi_egui_lib::actions::GuiAction;

use crate::contract::{drive_action, Frontend, PauseHint};
use crate::palette::ColorPalette;
use crate::renderer::{GameFrame, Renderer, SourceSize};
use crate::ui_host::{ExtraEvents, UiHost};

/// Something only the platform (OS/window/fs) can do, surfaced by the app for
/// the platform to perform after a `draw`.
#[derive(Debug)]
pub enum PlatformRequest {
    /// The user asked to quit.
    Exit,
    /// The window should be resized to fit the given content aspect at the
    /// current scale (used when the SGB border toggles the presented size).
    /// Dimensions are the un-scaled content size in pixels; the platform
    /// multiplies by its scale factor.
    ResizeContent { width: u32, height: u32 },
    /// The UI requested a state save to an arbitrary path (File → Save State).
    /// The platform serializes and writes; the app hands over the bytes.
    SaveStateBytes { path: std::path::PathBuf, bytes: Vec<u8> },
    /// The UI picked a file to load (ROM or savestate). The platform reads the
    /// bytes (path on desktop, content on web/Android) and feeds them back via
    /// [`App::finish_load_rom`] / [`App::finish_load_state`].
    LoadFile(FileData),
    /// A status line to show the user.
    Status(String),
    /// An error to show the user.
    Error(String),
    /// Clear the UI error overlay (a load succeeded / the error was dismissed).
    ClearError,
    /// An Android ROM-library / SAF action the app can't service itself (it
    /// needs the JNI bridge + the library panel, both platform-owned). The
    /// platform matches these and drives `android_bridge` / `LibraryState`.
    #[cfg(target_os = "android")]
    AndroidLibrary(GuiAction),
}

/// Frame pacing target (~59.7 fps), matching the original World loop.
const TARGET_FRAME_TIME: Duration = Duration::from_micros(16750);

/// The portable app.
///
/// It deliberately does NOT own the [`UiHost`] or [`Renderer`]: those are
/// GPU/window-bound and are recreated when the surface (re)appears (desktop
/// startup, Android foreground). The `App` — Session, palette, run/pause/error
/// bookkeeping — persists across a surface teardown. The platform passes the
/// live `UiHost`/`Renderer` into [`App::draw`] each frame.
pub struct App {
    session: Session,

    /// Latest presented frame (or a debug step's frame).
    frame: Option<gb::Frame>,
    error_state: Option<String>,
    is_paused: bool,

    // Debug single-step requests, consumed by `run_frame`. (Multi-step requests
    // are session-owned now — set by `Session::apply`, drained in `run_frame`.)
    step_single_frame: bool,
    step_single_cycle: bool,

    current_rom_path: Option<String>,
    current_bios_path: Option<String>,
    /// Raw ROM bytes, kept so a slot/state load can re-derive the ROM id and
    /// reinsert the cartridge.
    rom_bytes: Option<Vec<u8>>,

    input: AbstractInput,

    /// Host requests accumulated while applying a UI action (drained by `draw`
    /// and returned to the platform). The `Frontend` capability methods push
    /// here; the platform performs them.
    pending_requests: Vec<PlatformRequest>,

    // Pause bookkeeping (moved from the event loop): the user's explicit pause
    // vs. the transient menu-open pause.
    user_paused: bool,
    manually_paused: bool,
    auto_paused_no_content: bool,
    breakpoint_hit: bool,

    // Perf / pacing.
    frame_times: Vec<Instant>,
    last_frame_time: Instant,
    last_title_update: Instant,
    fps: f64,

    /// The most recent chrome inset in *logical points*: how much wider/taller
    /// the window is than the egui central region (menu bar + any status
    /// panel). Measured each `draw` so the platform can size the window to
    /// `content*scale + inset`, letting the game fill the central rect at full
    /// integer scale with no letterbox bars. Dynamic — never a static offset.
    content_inset: (f32, f32),
}

impl App {
    /// Build the app around a prepared `Session`. `should_pause` is the initial
    /// pause state (true when neither ROM nor BIOS is loaded). The `UiHost` and
    /// `Renderer` are created separately by the platform and passed into
    /// [`App::draw`].
    pub fn new(
        mut session: Session,
        palette: ColorPalette,
        rom_path: Option<String>,
        bios_path: Option<String>,
        rom_bytes: Option<Vec<u8>>,
        should_pause: bool,
    ) -> Self {
        // Seed the session's presentation palette from the CLI/config choice so
        // the shared `apply`/`ui_state` path renders from one source (no persist
        // at startup — only a user SetPalette writes config).
        session.init_palette_choice(palette.to_choice());
        let now = Instant::now();
        App {
            session,
            frame: None,
            error_state: None,
            is_paused: should_pause,
            step_single_frame: false,
            step_single_cycle: false,
            current_rom_path: rom_path,
            current_bios_path: bios_path,
            rom_bytes,
            input: AbstractInput::none(),
            pending_requests: Vec::new(),
            user_paused: should_pause,
            manually_paused: should_pause,
            auto_paused_no_content: should_pause,
            breakpoint_hit: false,
            frame_times: Vec::with_capacity(60),
            last_frame_time: now,
            last_title_update: now,
            fps: 0.0,
            content_inset: (0.0, 0.0),
        }
    }

    // --- access for the platform -------------------------------------------

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn gb(&self) -> &gb::GB {
        self.session.gb()
    }

    pub fn gb_mut(&mut self) -> &mut gb::GB {
        self.session.gb_mut()
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused
    }

    pub fn error_state(&self) -> Option<&str> {
        self.error_state.as_deref()
    }

    pub fn current_rom_path(&self) -> Option<&str> {
        self.current_rom_path.as_deref()
    }

    pub fn fps(&self) -> f64 {
        self.fps
    }

    /// The content size (pre-scale) that should drive the window: the SGB
    /// composite size only when the border is actually being shown, else the
    /// plain Game Boy screen. Delegates to the session (shared source of truth).
    pub fn content_size(&self) -> (u32, u32) {
        self.session.content_size()
    }

    /// The chrome inset (menu bar + status panel) in logical points measured on
    /// the last `draw`, so the platform can size the window to
    /// `content*scale + inset`. `(0, 0)` before the first frame.
    pub fn content_inset(&self) -> (f32, f32) {
        self.content_inset
    }

    // --- input --------------------------------------------------------------

    /// Latch host button state (already OR'd with touch) as the abstract input
    /// for the next frame. The session applies the config remap.
    pub fn set_button_state(&mut self, state: input::ButtonState) {
        let mut a = AbstractInput::none();
        a.set(GbButton::A, state.a);
        a.set(GbButton::B, state.b);
        a.set(GbButton::Start, state.start);
        a.set(GbButton::Select, state.select);
        a.set(GbButton::Up, state.up);
        a.set(GbButton::Down, state.down);
        a.set(GbButton::Left, state.left);
        a.set(GbButton::Right, state.right);
        self.input = a;
    }

    // --- feature hotkeys (the platform maps keys to these) ------------------

    pub fn quicksave(&mut self, timestamp: u64) -> Result<(), String> {
        self.session.quicksave(timestamp).map_err(|e| e.to_string())
    }

    pub fn quickload(&mut self) -> Result<(), String> {
        self.session.quickload().map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn toggle_fast_forward(&mut self) {
        self.session.toggle_fast_forward();
    }

    pub fn is_fast_forward(&self) -> bool {
        matches!(self.session.mode(), RunMode::FastForward(_))
    }

    pub fn frame_advance(&mut self) {
        self.session.frame_advance();
        self.user_paused = true;
        self.manually_paused = true;
    }

    /// Hold-to-rewind: step back one snapshot, refresh the presented frame.
    pub fn rewind(&mut self) {
        if self.session.rewind().is_some() {
            self.frame = Some(self.session.gb_mut().get_current_frame());
        }
    }

    pub fn rewind_enabled(&self) -> bool {
        self.session.config().rewind.enabled
    }

    /// Request a debug single-frame step (honored while paused).
    pub fn request_step_frame(&mut self) {
        self.step_single_frame = true;
    }

    /// Request a debug single-instruction step.
    pub fn request_step_cycle(&mut self) {
        self.step_single_cycle = true;
    }

    /// Whether debug stepping is currently applicable (paused or errored).
    pub fn stepping_allowed(&self) -> bool {
        self.manually_paused || self.error_state.is_some()
    }

    // --- machine lifecycle (byte-level; platform resolves paths) ------------

    /// Load a ROM from raw bytes (platform resolves `FileData::Path` → bytes on
    /// desktop; web/Android pass bytes directly). `path` is the display/name for
    /// title + printer output (`None` for content-only sources).
    pub fn load_rom_bytes(&mut self, bytes: Vec<u8>, path: Option<String>) -> Result<(), String> {
        let rom_id = self.session.finish_load_rom(&bytes).map_err(|e| e.to_string())?;
        let _ = rom_id;
        self.rom_bytes = Some(bytes);
        self.current_rom_path = path;
        self.error_state = None;
        self.frame = None;
        if self.auto_paused_no_content {
            self.is_paused = false;
            self.user_paused = false;
            self.manually_paused = false;
            self.auto_paused_no_content = false;
        }
        Ok(())
    }

    /// The BIOS path currently attached (so the platform can re-read it on a
    /// state load, mirroring the old World behavior).
    pub fn current_bios_path(&self) -> Option<&str> {
        self.current_bios_path.as_deref()
    }

    /// Record the BIOS path the platform (re)attached to the machine.
    pub fn set_bios_path(&mut self, path: Option<String>) {
        self.current_bios_path = path;
    }

    /// Load a savestate from raw bytes, re-attaching the current ROM if present.
    /// `reload_rom` supplies `(path, bytes)` for the ROM to reinsert (the
    /// platform reads it from disk); `None` keeps the already-loaded ROM. The
    /// core-side re-attach logic lives in the session; this wrapper keeps the
    /// app's `rom_bytes` / path / pause bookkeeping in sync.
    pub fn load_state_bytes(
        &mut self,
        state: &[u8],
        reload_rom: Option<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        // Prefer the ROM the caller supplied; else keep the already-loaded bytes
        // (a same-ROM reload has `reload_rom == None`).
        let rom_bytes = reload_rom
            .as_ref()
            .map(|(_, b)| b.clone())
            .or_else(|| self.rom_bytes.clone());
        if let Some((path, _)) = &reload_rom {
            self.current_rom_path = Some(path.clone());
        }
        let reload_slice = reload_rom.as_ref().map(|(_, b)| b.as_slice());
        let rom_id = rom_bytes.as_deref().map(rustyboi_session::sha256).unwrap_or([0u8; 32]);
        self.session
            .finish_load_state(state, reload_slice, rom_id)
            .map_err(|e| e.to_string())?;
        let has_content = self.session.gb().has_rom() || self.session.gb().has_bios();
        self.rom_bytes = rom_bytes;
        self.error_state = None;
        self.frame = None;
        if self.auto_paused_no_content && has_content {
            self.is_paused = false;
            self.auto_paused_no_content = false;
        }
        Ok(())
    }

    /// Serialize the current machine state to bytes (for File → Save State; the
    /// platform writes them).
    pub fn state_bytes(&self) -> Result<Vec<u8>, String> {
        self.session.gb().to_state_bytes().map_err(|e| e.to_string())
    }

    // --- UI state snapshot --------------------------------------------------

    fn ui_state(&self) -> SessionUiState {
        let cfg = self.session.config();
        SessionUiState {
            hardware: self.session.hardware_choice(),
            palette: self.session.palette(),
            rewind_enabled: cfg.rewind.enabled,
            rewind_interval_frames: cfg.rewind.interval_frames,
            rewind_depth: cfg.rewind.depth,
            sgb_border: self.session.sgb_border(),
            fast_forward: self.is_fast_forward(),
            touch_controls: self.session.touch_controls(),
            slots: self.session.list_slots(),
        }
    }

    // --- present ------------------------------------------------------------

    /// Convert the latest presented frame to the RGBA source the renderer
    /// uploads, preferring the SGB composite when the toggle is on and the
    /// machine offers one.
    fn present(&self) -> Option<GameFrame> {
        if self.session.sgb_border()
            && let Some(rgb) = self.session.gb().sgb_composited_frame()
        {
            let mut rgba = Vec::with_capacity((rgb.len() / 3) * 4);
            for chunk in rgb.chunks_exact(3) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            return Some(GameFrame { size: SourceSize::Sgb, rgba });
        }

        // The DMG presentation palette is session-owned; convert its choice.
        let palette = ColorPalette::from_choice(self.session.palette());
        let gb_frame = self.frame.as_ref()?;
        let rgba = match gb_frame {
            gb::Frame::Monochrome(data) => convert_to_rgba(data, &palette).to_vec(),
            gb::Frame::Color(data) => {
                let mut rgba = vec![0u8; ppu::FRAMEBUFFER_SIZE * 4];
                for (i, chunk) in data.chunks(3).enumerate() {
                    let offset = i * 4;
                    rgba[offset] = chunk[0];
                    rgba[offset + 1] = chunk[1];
                    rgba[offset + 2] = chunk[2];
                    rgba[offset + 3] = 255;
                }
                rgba
            }
        };
        Some(GameFrame { size: SourceSize::Gb, rgba })
    }

    // --- run one emulation frame -------------------------------------------

    /// Advance the machine one presented frame per the current mode, pacing to
    /// ~60fps. Returns the audio samples produced (for the platform to play) and
    /// whether a rewind snapshot handoff should be pumped (offloaded mode). The
    /// caller pumps its rewind/printer workers around this.
    ///
    /// Debug stepping (single/multi frame/cycle) is handled first and bypasses
    /// pacing. Breakpoint-aware run is used when breakpoints are set.
    pub fn run_frame(&mut self) -> FrameStep {
        // Debug: single frame step.
        if self.step_single_frame {
            self.step_single_frame = false;
            match self.run_frame_on_core() {
                Some((frame, _bp)) => self.frame = Some(frame),
                None => {
                    self.error_state = Some("Emulator crashed during frame step".to_string());
                    self.frame = None;
                }
            }
            return FrameStep::default();
        }
        // Debug: single instruction step.
        if self.step_single_cycle {
            self.step_single_cycle = false;
            self.step_one_instruction("during cycle step");
            return FrameStep::default();
        }
        // Debug: multi-cycle step (request set by `Session::apply`).
        if let Some(count) = self.session.take_step_cycles() {
            let gb = self.session.gb_mut();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for _ in 0..count {
                    let (_bp, _cycles) = gb.step_instruction(false);
                }
                gb.get_current_frame()
            }));
            match result {
                Ok(frame) => self.frame = Some(frame),
                Err(p) => {
                    self.error_state = Some(panic_message(p, &format!("during multi-cycle step ({count})")));
                    self.frame = None;
                }
            }
            return FrameStep::default();
        }
        // Debug: multi-frame step (request set by `Session::apply`).
        if let Some(count) = self.session.take_step_frames() {
            let mut final_frame = None;
            let mut ok = true;
            for _ in 0..count {
                match self.run_frame_on_core() {
                    Some((frame, _bp)) => final_frame = Some(frame),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                self.frame = final_frame;
            } else {
                self.error_state = Some(format!("Emulator crashed during multi-frame step ({count})"));
                self.frame = None;
            }
            return FrameStep::default();
        }

        // Frame-advance runs exactly one frame even while paused.
        if self.error_state.is_none() && matches!(self.session.mode(), RunMode::FrameAdvance) {
            let output = self.session.run_frame(self.input);
            self.frame = Some(output.frame);
            return FrameStep { audio: output.audio, pump_workers: true };
        }

        if self.error_state.is_some() || self.is_paused {
            return FrameStep::default();
        }

        // Pace to ~60fps (host concern; kept here as it belongs to the run loop
        // rather than the window). Wasm builds skip the spin/sleep.
        self.pace();
        self.last_frame_time = Instant::now();

        if self.session.gb().get_breakpoints().is_empty() {
            let output = self.session.run_frame(self.input);
            if output.advanced {
                self.frame = Some(output.frame);
                self.record_fps();
            } else {
                self.frame = Some(output.frame);
            }
            FrameStep { audio: output.audio, pump_workers: true }
        } else {
            match self.run_frame_on_core() {
                Some((frame, bp)) => {
                    self.frame = Some(frame);
                    self.record_fps();
                    if bp {
                        self.is_paused = true;
                        self.breakpoint_hit = true;
                    }
                    FrameStep::default()
                }
                None => {
                    self.error_state = Some("Emulator crashed".to_string());
                    self.frame = None;
                    FrameStep::default()
                }
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn pace(&self) {
        let elapsed = self.last_frame_time.elapsed();
        if elapsed < TARGET_FRAME_TIME {
            let remaining = TARGET_FRAME_TIME - elapsed;
            if remaining > Duration::from_micros(100) {
                std::thread::sleep(remaining - Duration::from_micros(50));
            }
            while self.last_frame_time.elapsed() < TARGET_FRAME_TIME {
                std::hint::spin_loop();
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn pace(&self) {}

    fn run_frame_on_core(&mut self) -> Option<(gb::Frame, bool)> {
        let gb = self.session.gb_mut();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| gb.run_until_frame(false))).ok()
    }

    fn step_one_instruction(&mut self, label: &str) {
        let gb = self.session.gb_mut();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_bp, _cycles) = gb.step_instruction(false);
            gb.get_current_frame()
        }));
        match result {
            Ok(frame) => self.frame = Some(frame),
            Err(p) => {
                self.error_state = Some(panic_message(p, label));
                self.frame = None;
            }
        }
    }

    fn record_fps(&mut self) {
        let now = Instant::now();
        self.frame_times.push(now);
        if self.frame_times.len() > 60 {
            self.frame_times.remove(0);
        }
        let n = self.frame_times.len();
        if n >= 2 {
            let dur = self.frame_times[n - 1].duration_since(self.frame_times[0]);
            if dur.as_secs_f64() > 0.0 {
                self.fps = (n as f64 - 1.0) / dur.as_secs_f64();
            }
        }
    }

    /// Whether the title should be refreshed (rate-limited to twice a second),
    /// returning the title text when due. The platform sets the window title.
    pub fn title_if_due(&mut self) -> Option<String> {
        let now = Instant::now();
        if now.duration_since(self.last_title_update).as_millis() < 500 {
            return None;
        }
        self.last_title_update = now;
        let paused = self.manually_paused || self.error_state.is_some();
        let title = if self.error_state.is_some() {
            format!("RustyBoi - ERROR | {:.1} FPS", self.fps)
        } else if paused {
            format!("RustyBoi - PAUSED | {:.1} FPS", self.fps)
        } else {
            format!("RustyBoi | {:.1} FPS", self.fps)
        };
        Some(title)
    }

    /// Whether a breakpoint was hit since the last check (and clear it). The
    /// platform surfaces a status line.
    pub fn take_breakpoint_hit(&mut self) -> bool {
        let hit = self.breakpoint_hit;
        self.breakpoint_hit = false;
        hit
    }

    // --- draw (UI + render) -------------------------------------------------

    /// Run the egui UI, dispatch its actions, and render the composited frame.
    /// `extra_events` are platform-injected egui events (Android IME). Returns
    /// the platform requests produced (exit / resize / save / status / error).
    ///
    /// `resolve_gui_action` is a platform callback that turns a UI action into
    /// bytes when the OS is required (reading a picked ROM/state file). It
    /// returns `None` for actions it doesn't handle (all the pure ones), which
    /// the app then applies itself.
    pub fn draw(
        &mut self,
        window: &winit::window::Window,
        ui: &mut UiHost,
        renderer: &mut Renderer,
        extra_events: ExtraEvents,
        mut resolve_gui_action: impl FnMut(&GuiAction) -> Option<ResolvedAction>,
    ) -> Vec<PlatformRequest> {
        let mut requests = Vec::new();

        let paused_for_ui = self.manually_paused || self.error_state.is_some();
        let registers = Some(self.session.gb().get_cpu_registers());
        let ui_state = self.ui_state();

        // The UI needs &GB while the app also mutates itself below; run the UI
        // first, collecting its output, then drop the borrow.
        let (paint, ui_frame) = {
            let gb_ref = self.session.gb();
            ui.run(window, paused_for_ui, registers, Some(gb_ref), &ui_state, extra_events)
        };

        // Dispatch the action.
        if let Some(action) = ui_frame.action {
            self.dispatch_action(action, &mut requests, &mut resolve_gui_action);
        }

        // Apply any UI-error-overlay clears the shared driver requested.
        requests.retain(|r| {
            if matches!(r, PlatformRequest::ClearError) {
                ui.clear_error();
                false
            } else {
                true
            }
        });

        // Auto-pause when a menu is open, respecting manual pause.
        let should_be_paused = self.manually_paused || ui_frame.menu_open;
        if should_be_paused != self.is_paused {
            if should_be_paused {
                self.is_paused = true;
            } else if !self.user_paused && self.error_state.is_none() {
                self.is_paused = false;
            }
        }

        // Surface any error to the UI.
        if let Some(err) = self.error_state.clone() {
            ui.set_error(err);
            self.manually_paused = self.user_paused || self.error_state.is_some();
        }

        // Measure the chrome inset (menu bar + status panel) in logical points
        // from this frame's surface vs. central region, so the platform can grow
        // the window to make the central rect exactly content*scale. Dynamic:
        // recomputed every frame, so an upstream egui size change is absorbed.
        let ppp = paint.pixels_per_point.max(0.01);
        let (surf_w, surf_h) = renderer.surface_size();
        let inset_w = ((surf_w as f32 - ui_frame.region.width).max(0.0)) / ppp;
        let inset_h = ((surf_h as f32 - ui_frame.region.height).max(0.0)) / ppp;
        self.content_inset = (inset_w, inset_h);

        // Render: game letterboxed into the central region, egui on top.
        let game = self.present();
        if let Err(e) = renderer.render(game.as_ref(), ui_frame.region, paint) {
            match e {
                wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated => {
                    let (w, h) = renderer.surface_size();
                    renderer.resize(w, h);
                }
                wgpu::SurfaceError::OutOfMemory => {
                    requests.push(PlatformRequest::Error("GPU out of memory".into()));
                }
                wgpu::SurfaceError::Timeout => {}
            }
        }

        requests
    }

    /// Apply a UI action. ROM/state loads need the platform's file resolver, so
    /// they are handled here (resolve → session load → app pause bookkeeping);
    /// every other action is routed through the shared [`drive_action`] contract
    /// so its behavior is implemented once in `Session::apply`.
    fn dispatch_action(
        &mut self,
        action: GuiAction,
        requests: &mut Vec<PlatformRequest>,
        resolve: &mut impl FnMut(&GuiAction) -> Option<ResolvedAction>,
    ) {
        match action {
            // OS-requiring loads: resolve to bytes here (the resolver reads the
            // path / content), then apply with the app-side pause bookkeeping.
            action @ (GuiAction::LoadRom(_) | GuiAction::LoadState(_)) => {
                match resolve(&action) {
                    Some(ResolvedAction::LoadRom { bytes, path }) => {
                        match self.load_rom_bytes(bytes, path) {
                            Ok(()) => {
                                self.manually_paused = self.user_paused;
                                requests.push(PlatformRequest::ClearError);
                                let (w, h) = self.content_size();
                                requests.push(PlatformRequest::ResizeContent { width: w, height: h });
                                requests.push(PlatformRequest::Status("ROM loaded".into()));
                            }
                            Err(e) => requests.push(PlatformRequest::Error(format!("Failed to load ROM: {e}"))),
                        }
                    }
                    Some(ResolvedAction::LoadState { state, reload_rom }) => {
                        match self.load_state_bytes(&state, reload_rom) {
                            Ok(()) => {
                                self.manually_paused = self.user_paused || self.error_state.is_some();
                                requests.push(PlatformRequest::ClearError);
                                requests.push(PlatformRequest::Status("State loaded".into()));
                            }
                            Err(e) => requests.push(PlatformRequest::Error(format!("Failed to load state: {e}"))),
                        }
                    }
                    None => {}
                }
            }

            // Everything else: one shared behavior path via the contract driver.
            other => {
                drive_action(self, other, now_epoch_secs());
                requests.append(&mut self.pending_requests);
            }
        }
    }
}

/// A `GuiAction` the platform resolved into bytes the app can apply.
pub enum ResolvedAction {
    LoadRom { bytes: Vec<u8>, path: Option<String> },
    LoadState { state: Vec<u8>, reload_rom: Option<(String, Vec<u8>)> },
}

/// The app is a windowed [`Frontend`]: the shared [`drive_action`] driver
/// performs each UI command through these capability methods. Missing one is a
/// compile error at the `drive_action::<App>` call site in `dispatch_action`.
impl Frontend for App {
    fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    fn set_status(&mut self, message: String) {
        self.pending_requests.push(PlatformRequest::Status(message));
    }

    fn set_error(&mut self, message: String) {
        self.pending_requests.push(PlatformRequest::Error(message));
    }

    fn clear_error(&mut self) {
        self.pending_requests.push(PlatformRequest::ClearError);
    }

    fn exit(&mut self) {
        self.pending_requests.push(PlatformRequest::Exit);
    }

    fn resize_content(&mut self, width: u32, height: u32) {
        self.pending_requests
            .push(PlatformRequest::ResizeContent { width, height });
    }

    fn save_state_bytes(&mut self, path: std::path::PathBuf, bytes: Vec<u8>) {
        self.pending_requests
            .push(PlatformRequest::SaveStateBytes { path, bytes });
    }

    fn load_file(&mut self, file: FileData) {
        // Loads are intercepted in `dispatch_action` (they need the resolver);
        // if one reaches here, surface it to the platform to read + feed back.
        self.pending_requests.push(PlatformRequest::LoadFile(file));
    }

    fn on_pause_changed(&mut self, hint: PauseHint) {
        match hint {
            PauseHint::TogglePause => {
                self.user_paused = !self.user_paused;
                self.manually_paused = self.user_paused || self.error_state.is_some();
                self.is_paused = self.manually_paused;
            }
            PauseHint::Restart => {
                // The session already power-cycled; clear app run state to match.
                self.error_state = None;
                self.frame = None;
                self.is_paused = false;
                self.user_paused = false;
                self.manually_paused = self.user_paused;
            }
            PauseHint::ClearError => {
                self.error_state = None;
                self.is_paused = true;
                self.manually_paused = self.user_paused;
            }
            PauseHint::FrameAdvance => {
                self.user_paused = true;
                self.manually_paused = true;
            }
            PauseHint::SetHardware => {
                // Rebuild cleared the machine; drop app run state but keep the
                // user's pause choice (pre-refactor behavior).
                self.error_state = None;
                self.frame = None;
            }
            PauseHint::Load => {}
        }
    }

    #[cfg(target_os = "android")]
    fn android_library(&mut self, action: UiAction) {
        self.pending_requests
            .push(PlatformRequest::AndroidLibrary(action));
    }
}

/// What a `run_frame` produced for the platform to act on.
#[derive(Default)]
pub struct FrameStep {
    /// Stereo samples generated this frame (empty when nothing advanced).
    pub audio: Vec<(f32, f32)>,
    /// Whether the platform should pump its rewind/printer workers this frame
    /// (true only when an emulation frame actually advanced through the session).
    pub pump_workers: bool,
}

/// Current epoch seconds for savestate-slot timestamps (0 if before epoch). The
/// app is otherwise clock-free; slot timestamps are a native affordance.
fn now_epoch_secs() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

fn panic_message(panic_info: Box<dyn std::any::Any + Send>, context: &str) -> String {
    if let Some(s) = panic_info.downcast_ref::<&str>() {
        format!("Emulator panic {context}: {s}")
    } else if let Some(s) = panic_info.downcast_ref::<String>() {
        format!("Emulator panic {context}: {s}")
    } else {
        format!("Emulator panic {context}: Unknown error")
    }
}

fn convert_to_rgba(
    frame: &[u8; ppu::FRAMEBUFFER_SIZE],
    palette: &ColorPalette,
) -> [u8; ppu::FRAMEBUFFER_SIZE * 4] {
    let mut out = [0; ppu::FRAMEBUFFER_SIZE * 4];
    let colors = palette.get_rgba_colors();
    for (i, &pixel) in frame.iter().enumerate() {
        let rgba = colors.get(pixel as usize).unwrap_or(&colors[3]);
        let offset = i * 4;
        out[offset..offset + 4].copy_from_slice(rgba);
    }
    out
}

#[cfg(test)]
mod restart_tests {
    //! Regression coverage for the Restart action preserving user settings.
    //!
    //! Restart must power-cycle the *same console* the user configured, not fall
    //! back to a default machine. The old implementation reset the `GB` in place
    //! (`GB::reset`), which does not re-apply the model-derived hardware flags set
    //! only in `GB::new` (SGB/CGB/MGB/AGB + PPU/APU revision gates) — so an SGB
    //! (or any non-default model) silently degraded on restart. These tests pin
    //! the rebuild path Restart now uses to the session's chosen hardware.
    use rustyboi_core_lib::cartridge::Cartridge;
    use rustyboi_core_lib::gb::{Hardware, GB};
    use rustyboi_session::config::{Config, DmgPalette};
    use rustyboi_session::ports::{MemRumble, MemStorage, MemWebcam};
    use rustyboi_session::session::{Ports, Session};

    fn ports() -> Ports {
        Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        }
    }

    /// Minimal 32KB NoMBC ROM (SGB-flagged), enough to insert a cartridge.
    fn tiny_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x146] = 0x03; // SGB support flag
        rom
    }

    // The mechanism the fix relies on: `GB::reset` (old restart) drops the SGB
    // model state, while rebuilding via `GB::new(hardware)` (new restart)
    // restores it. If this ever flips, in-place reset would again be viable and
    // the frontend rebuild could be reconsidered.
    #[test]
    fn in_place_reset_loses_model_state_rebuild_keeps_it() {
        let mut gb = GB::new(Hardware::SGB);
        assert!(gb.sgb().is_some(), "fresh SGB machine must expose SGB state");

        gb.reset();
        assert!(
            gb.sgb().is_none(),
            "in-place reset drops SGB model state (the old-restart bug)"
        );

        // The rebuild path Restart now takes.
        let rebuilt = GB::new(Hardware::SGB);
        assert!(rebuilt.sgb().is_some(), "rebuild restores SGB model state");
    }

    // A session-level stand-in for `App::restart`: replacing the machine with a
    // fresh `GB::new(session.hardware())` preserves the hardware model AND leaves
    // the session config (hardware override + DMG palette) untouched.
    #[test]
    fn restart_rebuild_preserves_hardware_and_palette() {
        let mut cfg = Config::default();
        cfg.hardware = Hardware::SGB;
        let custom = DmgPalette { shades: [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16]] };
        cfg.dmg_palette = custom;

        let mut session = Session::new(cfg, ports(), [0u8; 32]);
        let mut gb = GB::new(session.hardware());
        gb.insert(Cartridge::from_bytes(&tiny_rom()).unwrap());
        gb.skip_bios();
        session.replace_machine(gb, [0u8; 32]);
        assert!(session.gb().sgb().is_some());

        // Simulate Restart: rebuild for the session's current hardware.
        let mut rebuilt = GB::new(session.hardware());
        rebuilt.insert(Cartridge::from_bytes(&tiny_rom()).unwrap());
        rebuilt.skip_bios();
        session.replace_machine(rebuilt, [0u8; 32]);

        assert_eq!(session.hardware(), Hardware::SGB, "hardware override preserved");
        assert!(session.gb().sgb().is_some(), "SGB model survives restart");
        assert_eq!(session.config().dmg_palette, custom, "DMG palette preserved");
    }
}

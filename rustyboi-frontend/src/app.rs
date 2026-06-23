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

use std::time::Instant;

use rustyboi_core_lib::{gb, input, ppu};
use rustyboi_session::action::{FileData, LoadPurpose};
use rustyboi_session::apply::FetchPurpose;
use rustyboi_session::{AbstractInput, GbButton, RunMode, Session, SessionUiState};
#[cfg(target_os = "android")]
use rustyboi_session::UiAction;

use rustyboi_egui_lib::actions::GuiAction;

use crate::contract::{drive_action, Frontend, PauseHint};
use rustyboi_session::{frame_to_pixels, rgb_to_pixels, PaletteChoice, PixelOrder};
use crate::renderer::{GameFrame, Present, SourceSize};
use crate::ui_host::{ExtraEvents, UiHost};

/// Something only the platform (OS/window/fs) can do, surfaced by the app for
/// the platform to perform after a `draw`.
#[derive(Debug)]
pub enum PlatformRequest {
    /// The user asked to quit.
    Exit,
    /// Toggle host fullscreen (desktop flips the winit window; Android no-ops).
    ToggleFullscreen,
    /// The window should be resized to fit the given content aspect at the
    /// current scale (used when the SGB border toggles the presented size).
    /// Dimensions are the un-scaled content size in pixels; the platform
    /// multiplies by its scale factor.
    ResizeContent { width: u32, height: u32 },
    /// The UI requested a state save to an arbitrary path (File → Save State).
    /// The platform serializes and writes; the app hands over the bytes.
    SaveStateBytes { path: std::path::PathBuf, bytes: Vec<u8> },
    /// Deliver `bytes` to the user as a file named `suggested_name` (File →
    /// Export battery/RTC/state). The platform picks a location and writes.
    SaveBytes { suggested_name: String, bytes: Vec<u8> },
    /// The UI picked a file to load. The platform reads the bytes (path on
    /// desktop, content on web/Android) and feeds them back via the finisher for
    /// `purpose`.
    LoadFile { file: FileData, purpose: LoadPurpose },
    /// The UI asked to fetch `urls` (tried in order) over HTTP. The platform
    /// performs the GET (background thread on desktop/Android) and feeds the body
    /// back to the session for `purpose`.
    FetchUrl { urls: Vec<String>, purpose: FetchPurpose },
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

    input: AbstractInput,

    /// Held gamepad buttons this frame, forwarded to the keybind editor so it can
    /// capture controller presses (egui never sees pad input). Set by the platform
    /// alongside `set_button_state`.
    held_pad: std::collections::HashSet<rustyboi_session::input_config::PadButton>,

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

    // Perf readout. The app holds NO pacing logic and reads no pacing clock:
    // the platform's tick loop owns the shared `rustyboi_session::pacing`
    // Regulator and feeds this meter via [`App::note_frames`].
    meter: rustyboi_session::pacing::RateMeter,
    last_title_update: Instant,

    /// The most recent chrome inset in *logical points*: how much wider/taller
    /// the window is than the egui central region (menu bar + any status
    /// panel). Measured each `draw` so the platform can size the window to
    /// `content*scale + inset`, letting the game fill the central rect at full
    /// integer scale with no letterbox bars. Dynamic — never a static offset.
    content_inset: (f32, f32),

    /// Platform safe-area insets `(left, top, right, bottom)` in PHYSICAL pixels.
    /// The game region is shrunk by these so it is not drawn behind system bars /
    /// display cutouts. Zero on desktop/web; set from Android `content_rect`.
    safe_insets: [f32; 4],

    /// Reused RGBA upload scratch for `present`, so the per-frame frame-to-RGBA
    /// conversion (up to SGB 256×224×4) doesn't heap-allocate every frame.
    rgba_scratch: Vec<u8>,
}

impl App {
    /// Build the app around a prepared `Session`. `should_pause` is the initial
    /// pause state (true when neither ROM nor BIOS is loaded). The `UiHost` and
    /// `Renderer` are created separately by the platform and passed into
    /// [`App::draw`].
    pub fn new(
        mut session: Session,
        palette: PaletteChoice,
        rom_path: Option<String>,
        bios_path: Option<String>,
        should_pause: bool,
    ) -> Self {
        // Seed the session's presentation palette from the CLI/config choice so
        // the shared `apply`/`ui_state` path renders from one source (no persist
        // at startup — only a user SetPalette writes config).
        session.init_palette_choice(palette);
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
            input: AbstractInput::none(),
            held_pad: std::collections::HashSet::new(),
            pending_requests: Vec::new(),
            user_paused: should_pause,
            manually_paused: should_pause,
            auto_paused_no_content: should_pause,
            breakpoint_hit: false,
            meter: rustyboi_session::pacing::RateMeter::new(),
            last_title_update: now,
            content_inset: (0.0, 0.0),
            safe_insets: [0.0; 4],
            rgba_scratch: Vec::new(),
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
        self.meter.fps()
    }

    /// Record one tick into the shared rate meter: `emulated` frames advanced
    /// (the FPS readout is game speed). The platform's tick loop calls this
    /// every tick, including idle ones.
    pub fn note_frames(&mut self, now_seconds: f64, emulated: u32) {
        self.meter.record(now_seconds, emulated);
    }

    /// Cumulative drift versus a perfect 59.7275fps timeline (diagnostics).
    pub fn drift_frames(&self, now_seconds: f64) -> f64 {
        self.meter.drift_frames(now_seconds)
    }

    /// Whether emulation is effectively halted this tick (explicit pause or a
    /// crash overlay) — the regulator banks nothing during these.
    pub fn is_effectively_paused(&self) -> bool {
        self.error_state.is_some() || self.is_paused
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

    /// Record the gamepad buttons held this frame, for the keybind editor's
    /// bind-by-press / chord recording (egui can't observe pad input).
    pub fn set_held_pad(
        &mut self,
        pad: std::collections::HashSet<rustyboi_session::input_config::PadButton>,
    ) {
        self.held_pad = pad;
    }

    /// Set the platform safe-area insets `(left, top, right, bottom)` in PHYSICAL
    /// pixels (Android system bars / display cutout). The game region is shrunk by
    /// these so it is not drawn behind them. Zero on desktop/web.
    pub fn set_safe_insets(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        self.safe_insets = [left.max(0.0), top.max(0.0), right.max(0.0), bottom.max(0.0)];
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

    /// Toggle user pause (mirrors the `TogglePause` action's pause bookkeeping),
    /// for platform hotkey dispatch that doesn't route through `dispatch_action`.
    pub fn toggle_pause(&mut self) {
        self.user_paused = !self.user_paused;
        self.manually_paused = self.user_paused || self.error_state.is_some();
        self.is_paused = self.manually_paused;
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
        self.session.finish_load_rom(&bytes).map_err(|e| e.to_string())?;
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
    /// app's path / pause bookkeeping in sync.
    pub fn load_state_bytes(
        &mut self,
        state: &[u8],
        reload_rom: Option<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        // Prefer the ROM the caller supplied; else keep the currently-loaded
        // cartridge (a same-ROM reload has `reload_rom == None`, and the session
        // re-attaches the ROM from its own live machine). The ROM id re-keys the
        // slot: hash the supplied bytes, else reuse the session's current id — so
        // no frontend-side ROM copy needs to be retained.
        if let Some((path, _)) = &reload_rom {
            self.current_rom_path = Some(path.clone());
        }
        let reload_slice = reload_rom.as_ref().map(|(_, b)| b.as_slice());
        let rom_id = reload_slice
            .map(rustyboi_session::sha256)
            .unwrap_or_else(|| self.session.rom_id());
        self.session
            .finish_load_state(state, reload_slice, rom_id)
            .map_err(|e| e.to_string())?;
        let has_content = self.session.gb().has_rom() || self.session.gb().has_bios();
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
    pub fn state_bytes(&mut self) -> Result<Vec<u8>, String> {
        self.session.gb_mut().to_state_bytes().map_err(|e| e.to_string())
    }

    // --- UI state snapshot --------------------------------------------------

    fn ui_state(&self) -> SessionUiState {
        let cfg = self.session.config();
        SessionUiState {
            hardware: self.session.hardware_choice(),
            palette: self.session.palette(),
            gbc_dmg_palette: self.session.gbc_dmg_palette(),
            dmg_palette_active: self.session.dmg_palette_active(),
            color_correction: self.session.color_correction(),
            use_real_boot_rom: self.session.use_real_boot_rom(),
            texture_filter: self.session.texture_filter(),
            lcd_effect: self.session.lcd_effect(),
            printer_scale: self.session.printer_scale(),
            touch_opacity: self.session.touch_opacity(),
            rewind_enabled: cfg.rewind.enabled,
            rewind_interval_frames: cfg.rewind.interval_frames,
            rewind_depth: cfg.rewind.depth,
            volume: cfg.volume,
            scaling: cfg.scaling,
            graphics_backend: cfg.graphics_backend,
            sgb_border: self.session.sgb_border(),
            paused: self.session.is_paused(),
            fast_forward: self.is_fast_forward(),
            fast_forward_factor: cfg.fast_forward_factor,
            touch_controls: self.session.touch_controls(),
            show_fps: self.session.show_fps(),
            printer_attached: self.session.gb().printer_attached(),
            recording: self.session.is_recording(),
            replaying: self.session.is_playing(),
            slots: self.session.list_slots(),
            cheats: self.session.cheats().map(str::to_owned).collect(),
            fetched_cheats: self.session.fetched_cheats().to_vec(),
            has_battery: self.session.has_battery(),
            has_rtc: self.session.has_rtc(),
            has_rom: self.session.gb().has_rom(),
            game_name: self.session.game_name().map(str::to_owned),
            input: self.session.input_config().clone(),
        }
    }

    // --- present ------------------------------------------------------------

    /// Convert the latest presented frame to the RGBA source the renderer
    /// uploads, preferring the SGB composite when the toggle is on and the
    /// machine offers one.
    fn present(&mut self) -> Option<GameFrame<'_>> {
        // All conversions fill the reused `rgba_scratch` so the desktop present
        // path never heap-allocates the (up to 256×224×4) RGBA buffer per frame.
        let scratch = &mut self.rgba_scratch;
        if self.session.sgb_border()
            && let Some(rgb) = self.session.gb().sgb_composited_frame()
        {
            scratch.clear();
            scratch.resize((rgb.len() / 3) * 4, 0);
            rgb_to_pixels(&rgb[..], PixelOrder::Rgba, scratch);
            return Some(GameFrame { size: SourceSize::Sgb, rgba: scratch });
        }

        // The DMG presentation palette is session-owned; the shared packer maps
        // the frame into RGBA (byte-identical to the old inlined conversion).
        let shades = self.session.palette().rgba_shades();
        let gb_frame = self.frame.as_ref()?;
        scratch.clear();
        scratch.resize(ppu::FRAMEBUFFER_SIZE * 4, 0);
        frame_to_pixels(gb_frame, &shades, PixelOrder::Rgba, scratch);
        Some(GameFrame { size: SourceSize::Gb, rgba: scratch })
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
            return FrameStep { audio: output.audio, pump_workers: true, advanced: output.advanced };
        }

        if self.error_state.is_some() || self.is_paused {
            return FrameStep::default();
        }

        // No pacing here — by design. The platform's tick loop owns the shared
        // `rustyboi_session::pacing::Regulator` (wall-clock token bucket with a
        // bounded DAC trim) and calls `run_frame` exactly as many times per
        // tick as the regulator grants. The app never sleeps and never reads a
        // pacing clock, so game speed is identical on every platform and
        // host-timer quirks (macOS sleep coalescing) cannot slow it.
        if self.session.gb().get_breakpoints().is_empty() {
            let output = self.session.run_frame(self.input);
            self.frame = Some(output.frame);
            FrameStep { audio: output.audio, pump_workers: true, advanced: output.advanced }
        } else {
            match self.run_frame_on_core() {
                Some((frame, bp)) => {
                    self.frame = Some(frame);
                    if bp {
                        self.is_paused = true;
                        self.breakpoint_hit = true;
                    }
                    FrameStep { advanced: true, ..FrameStep::default() }
                }
                None => {
                    self.error_state = Some("Emulator crashed".to_string());
                    self.frame = None;
                    FrameStep::default()
                }
            }
        }
    }

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

    /// Whether the title should be refreshed (rate-limited to twice a second),
    /// returning the title text when due. The platform sets the window title.
    pub fn title_if_due(&mut self) -> Option<String> {
        let now = Instant::now();
        if now.duration_since(self.last_title_update).as_millis() < 500 {
            return None;
        }
        self.last_title_update = now;
        // Lead with the identified game (No-Intro name, else header title).
        let app = match self.session.game_name() {
            Some(g) => format!("{g} — RustyBoi"),
            None => "RustyBoi".to_string(),
        };
        let paused = self.manually_paused || self.error_state.is_some();
        let title = if self.error_state.is_some() {
            format!("{app} - ERROR | {:.1} FPS", self.fps())
        } else if paused {
            format!("{app} - PAUSED | {:.1} FPS", self.fps())
        } else {
            format!("{app} | {:.1} FPS", self.fps())
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
        renderer: &mut dyn Present,
        extra_events: ExtraEvents,
        fullscreen: bool,
        mut resolve_gui_action: impl FnMut(&GuiAction) -> Option<ResolvedAction>,
    ) -> Vec<PlatformRequest> {
        let mut requests = Vec::new();

        let paused_for_ui = self.manually_paused || self.error_state.is_some();
        let ui_state = self.ui_state();

        // Build the debug read-model only when a debug panel is open (the common
        // case builds nothing). Detail comes from the Gui's open-panel state,
        // read before we borrow the session so the borrows don't overlap.
        let debug_snapshot = if ui.any_debug_panel_open() {
            Some(self.session.debug_snapshot(ui.wanted_debug_detail()))
        } else {
            None
        };

        // Run the UI first, collecting its output, then drop the borrow.
        let (paint, ui_frame) = {
            // Desktop renders every frame (force_repaint: true); repaint-gating is
            // a web concern (its main thread also composites the worker's frames).
            ui.run(
                window,
                crate::ui_host::UiRunInputs {
                    paused: paused_for_ui,
                    debug: debug_snapshot.as_ref(),
                    fullscreen,
                    session: &ui_state,
                    extra_events,
                    held_pad: &self.held_pad,
                    force_repaint: true,
                    fps: self.fps() as f32,
                },
            )
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

        // Render: game letterboxed into the central region, egui on top. Push the
        // current presentation policy from the session config first (one shared
        // site): letterboxing, texture filter, and LCD post-process effect.
        renderer.set_scaling_mode(self.session.scaling_mode());
        renderer.set_texture_filter(self.session.texture_filter());
        renderer.set_lcd_effect(self.session.lcd_effect());
        // Shrink the game region by the platform safe-area insets so it is not
        // drawn behind system bars / a display cutout (Android). No-op elsewhere.
        // Computed before `present` borrows self.
        let [si_l, si_t, si_r, si_b] = self.safe_insets;
        let mut region = ui_frame.region;
        region.x += si_l;
        region.y += si_t;
        region.width = (region.width - si_l - si_r).max(0.0);
        region.height = (region.height - si_t - si_b).max(0.0);
        let game = self.present();
        // Reconfigure + retry next frame (the platform loop syncs the surface to
        // the window size before the next render). Validation errors surface
        // through the device error scope, so any other status just skips this
        // frame (Timeout/Occluded never reach here — render() maps them to Ok).
        if let Err(wgpu::SurfaceStatus::Lost | wgpu::SurfaceStatus::Outdated) =
            renderer.render(game.as_ref(), region, paint)
        {
            let (w, h) = renderer.surface_size();
            renderer.resize(w, h);
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
            // OS-requiring loads/imports: resolve to bytes here (the resolver
            // reads the path / content), then apply with the app-side bookkeeping.
            action @ (GuiAction::LoadRom(_)
            | GuiAction::LoadState(_)
            | GuiAction::ImportState(_)
            | GuiAction::ImportBatterySave(_)
            | GuiAction::ImportRtc(_)
            | GuiAction::ApplyPatch(_)
            | GuiAction::LoadMovie(_)) => {
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
                    Some(ResolvedAction::ImportBattery { bytes }) => {
                        match self.session.finish_import_battery(&bytes) {
                            Ok(()) => requests.push(PlatformRequest::Status("Battery save imported".into())),
                            Err(e) => requests.push(PlatformRequest::Error(format!("Failed to import battery save: {e}"))),
                        }
                    }
                    Some(ResolvedAction::ImportRtc { bytes }) => {
                        match self.session.finish_import_rtc(&bytes) {
                            Ok(()) => requests.push(PlatformRequest::Status("RTC imported".into())),
                            Err(e) => requests.push(PlatformRequest::Error(format!("Failed to import RTC: {e}"))),
                        }
                    }
                    Some(ResolvedAction::ApplyPatch { bytes }) => {
                        match self.session.apply_rom_patch(&bytes) {
                            Ok(_) => {
                                self.error_state = None;
                                self.frame = None;
                                requests.push(PlatformRequest::ClearError);
                                requests.push(PlatformRequest::Status("Patch applied".into()));
                            }
                            Err(e) => requests.push(PlatformRequest::Error(format!("Failed to apply patch: {e}"))),
                        }
                    }
                    Some(ResolvedAction::LoadMovie { bytes }) => {
                        match self.session.finish_load_movie(&bytes) {
                            Ok(()) => {
                                self.manually_paused = self.user_paused;
                                requests.push(PlatformRequest::ClearError);
                                requests.push(PlatformRequest::Status("Movie loaded — replaying".into()));
                            }
                            Err(e) => requests.push(PlatformRequest::Error(format!("Failed to load movie: {e}"))),
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
    ImportBattery { bytes: Vec<u8> },
    ImportRtc { bytes: Vec<u8> },
    ApplyPatch { bytes: Vec<u8> },
    LoadMovie { bytes: Vec<u8> },
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

    fn toggle_fullscreen(&mut self) {
        // Forward to the platform loop (which owns the winit window).
        self.pending_requests.push(PlatformRequest::ToggleFullscreen);
    }

    fn resize_content(&mut self, width: u32, height: u32) {
        self.pending_requests
            .push(PlatformRequest::ResizeContent { width, height });
    }

    fn save_state_bytes(&mut self, path: std::path::PathBuf, bytes: Vec<u8>) {
        self.pending_requests
            .push(PlatformRequest::SaveStateBytes { path, bytes });
    }

    fn save_bytes(&mut self, suggested_name: String, bytes: Vec<u8>) {
        self.pending_requests
            .push(PlatformRequest::SaveBytes { suggested_name, bytes });
    }

    fn load_file(&mut self, file: FileData, purpose: LoadPurpose) {
        // Loads are intercepted in `dispatch_action` (they need the resolver);
        // if one reaches here, surface it to the platform to read + feed back.
        self.pending_requests
            .push(PlatformRequest::LoadFile { file, purpose });
    }

    fn fetch_url(&mut self, urls: Vec<String>, purpose: FetchPurpose) {
        // The platform loop owns the HTTP background thread; hand the request off.
        self.pending_requests
            .push(PlatformRequest::FetchUrl { urls, purpose });
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
    /// Whether an emulation frame actually advanced (feeds the rate meter —
    /// audio emptiness can't stand in for this: the breakpoint path advances
    /// without routing audio, and uncapped fast-forward advances muted).
    pub advanced: bool,
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

#[cfg(test)]
mod fast_forward_tests {
    use super::App;
    use rustyboi_session::action::PaletteChoice;
    use rustyboi_session::config::Config;
    use rustyboi_session::ports::{MemRumble, MemStorage, MemWebcam};
    use rustyboi_session::session::{Ports, Session};

    fn app() -> App {
        let ports = Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        };
        let session = Session::new(Config::default(), ports, [0u8; 32]);
        App::new(session, PaletteChoice::Grayscale, None, None, true)
    }

    #[test]
    fn hotkey_press_toggles_on_and_off() {
        let mut a = app();
        assert!(!a.is_fast_forward());
        a.toggle_fast_forward();
        assert!(a.is_fast_forward(), "first press engages fast-forward");
        a.toggle_fast_forward();
        assert!(!a.is_fast_forward(), "second press disengages fast-forward");
    }

    #[test]
    fn menu_toggled_fast_forward_latches() {
        let mut a = app();
        a.session_mut().toggle_fast_forward();
        assert!(a.is_fast_forward());
        assert!(a.is_fast_forward());
        a.session_mut().toggle_fast_forward();
        assert!(!a.is_fast_forward());
    }
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
        let custom = DmgPalette { shades: [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16]] };
        let cfg = Config { hardware: Hardware::SGB, dmg_palette: custom, ..Default::default() };

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

#[cfg(test)]
mod pause_and_load_tests {
    use super::App;
    use crate::contract::{Frontend, PauseHint};
    use rustyboi_session::action::PaletteChoice;
    use rustyboi_session::config::Config;
    use rustyboi_session::ports::{MemRumble, MemStorage, MemWebcam};
    use rustyboi_session::session::{Ports, Session};

    fn ports() -> Ports {
        Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        }
    }

    // A paused app with no content (the `should_pause = true` startup state):
    // every pause flag set and the auto-pause latch armed.
    fn paused_app() -> App {
        let session = Session::new(Config::default(), ports(), [0u8; 32]);
        App::new(session, PaletteChoice::Grayscale, None, None, true)
    }

    /// Minimal valid 32KB NoMBC ROM.
    fn tiny_rom() -> Vec<u8> {
        vec![0u8; 0x8000]
    }

    // TogglePause flips user pause and mirrors it onto manual + effective pause.
    #[test]
    fn on_pause_changed_toggle_flips_all_three() {
        let mut a = paused_app();
        assert!(a.user_paused && a.manually_paused && a.is_paused);
        a.on_pause_changed(PauseHint::TogglePause);
        assert!(!a.user_paused && !a.manually_paused && !a.is_paused, "toggle off");
        a.on_pause_changed(PauseHint::TogglePause);
        assert!(a.user_paused && a.manually_paused && a.is_paused, "toggle back on");
    }

    // Restart clears error/frame and every pause flag (fresh, running machine).
    #[test]
    fn on_pause_changed_restart_clears_everything() {
        let mut a = paused_app();
        a.error_state = Some("boom".into());
        a.on_pause_changed(PauseHint::Restart);
        assert!(a.error_state.is_none());
        assert!(a.frame.is_none());
        assert!(!a.user_paused && !a.manually_paused && !a.is_paused);
    }

    // ClearError drops the error but leaves the machine paused for debugging.
    #[test]
    fn on_pause_changed_clear_error_pauses() {
        let mut a = paused_app();
        a.user_paused = false;
        a.error_state = Some("boom".into());
        a.on_pause_changed(PauseHint::ClearError);
        assert!(a.error_state.is_none());
        assert!(a.is_paused, "cleared error keeps a pause");
        assert!(!a.manually_paused, "manual mirrors user_paused (false)");
    }

    // FrameAdvance forces a manual/user pause (the machine stops after the step).
    #[test]
    fn on_pause_changed_frame_advance_forces_manual() {
        let mut a = paused_app();
        a.user_paused = false;
        a.manually_paused = false;
        a.on_pause_changed(PauseHint::FrameAdvance);
        assert!(a.user_paused && a.manually_paused);
    }

    // SetHardware rebuilds the machine (clear error + frame) but leaves the pause
    // state exactly as the user had it.
    #[test]
    fn on_pause_changed_set_hardware_keeps_pause() {
        let mut a = paused_app();
        a.error_state = Some("boom".into());
        a.frame = None;
        a.user_paused = false;
        a.manually_paused = false;
        a.is_paused = false;
        a.on_pause_changed(PauseHint::SetHardware);
        assert!(a.error_state.is_none(), "error cleared");
        assert!(!a.is_paused && !a.user_paused && !a.manually_paused, "pause untouched");
    }

    // Load is a no-op in the pause state machine (loads do their own bookkeeping).
    #[test]
    fn on_pause_changed_load_is_a_noop() {
        let mut a = paused_app();
        a.on_pause_changed(PauseHint::Load);
        assert!(a.user_paused && a.manually_paused && a.is_paused, "unchanged");
    }

    // A successful ROM load auto-unpauses (the no-content latch releases) and
    // clears any error/frame.
    #[test]
    fn load_rom_bytes_auto_unpauses() {
        let mut a = paused_app();
        a.error_state = Some("stale".into());
        a.load_rom_bytes(tiny_rom(), Some("game.gb".into())).expect("valid ROM loads");
        assert!(!a.is_paused && !a.user_paused && !a.manually_paused);
        assert!(!a.auto_paused_no_content, "no-content latch released");
        assert!(a.error_state.is_none() && a.frame.is_none());
    }

    // A failed ROM load surfaces the error to the caller and preserves the
    // pre-load pause bookkeeping (the auto-pause latch stays armed).
    #[test]
    fn load_rom_bytes_failure_preserves_state() {
        let mut a = paused_app();
        let err = a.load_rom_bytes(vec![0u8; 4], None); // too small to be a cartridge
        assert!(err.is_err(), "an invalid ROM must fail");
        assert!(a.is_paused && a.auto_paused_no_content, "pause state preserved");
    }

    // A successful state load (with the ROM re-supplied) auto-unpauses once the
    // machine has content again.
    #[test]
    fn load_state_bytes_auto_unpauses_with_content() {
        // Produce a valid savestate from a running machine.
        let mut src = paused_app();
        src.load_rom_bytes(tiny_rom(), Some("game.gb".into())).unwrap();
        let state = src.state_bytes().expect("serialize state");

        let mut a = paused_app();
        a.load_state_bytes(&state, Some(("game.gb".into(), tiny_rom())))
            .expect("state loads with re-supplied ROM");
        assert!(!a.is_paused, "content restored → auto-unpause");
        assert!(!a.auto_paused_no_content);
    }

    // A failed state load returns the error and preserves the pause bookkeeping.
    #[test]
    fn load_state_bytes_failure_preserves_state() {
        let mut a = paused_app();
        assert!(a.load_state_bytes(&[0u8; 8], None).is_err(), "garbage state fails");
        assert!(a.is_paused && a.auto_paused_no_content, "pause state preserved");
    }

    // Safe-area insets are clamped to non-negative (a system can report negatives).
    #[test]
    fn set_safe_insets_clamps_negatives_to_zero() {
        let mut a = paused_app();
        a.set_safe_insets(-5.0, 2.0, -3.0, 7.0);
        assert_eq!(a.safe_insets, [0.0, 2.0, 0.0, 7.0]);
    }

    // A breakpoint-hit flag is latching: read-once returns true then clears.
    #[test]
    fn take_breakpoint_hit_clears_on_read() {
        let mut a = paused_app();
        assert!(!a.take_breakpoint_hit(), "starts clear");
        a.breakpoint_hit = true;
        assert!(a.take_breakpoint_hit(), "first read sees the hit");
        assert!(!a.take_breakpoint_hit(), "and clears it");
    }
}

#[cfg(test)]
mod panic_message_tests {
    use super::panic_message;

    #[test]
    fn formats_str_string_and_unknown_payloads() {
        // The two common panic payload shapes carry their message through.
        assert_eq!(
            panic_message(Box::new("boom"), "during frame"),
            "Emulator panic during frame: boom"
        );
        assert_eq!(
            panic_message(Box::new("boom".to_string()), "on load"),
            "Emulator panic on load: boom"
        );
        // Any other payload type degrades to a generic message, never panics.
        assert_eq!(
            panic_message(Box::new(42i32), "on load"),
            "Emulator panic on load: Unknown error"
        );
    }
}

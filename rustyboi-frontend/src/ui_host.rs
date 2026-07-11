//! The portable egui host. Owns the egui `Context`, the `egui_winit::State`
//! input bridge, and the `Gui` (from `rustyboi-egui`). Runs one egui frame and
//! hands back the tessellated paint jobs + texture deltas (for the renderer to
//! composite) plus the resulting `GuiAction` / menu-open flag / central region.
//!
//! This replaces the old platform `framework.rs`. It is window-agnostic apart
//! from bridging winit *events* into egui (that's egui-winit's whole job); the
//! platform still owns the window and forwards its winit events here.

use std::sync::{Arc, Mutex};

use egui::{Context, ViewportId};
use winit::event_loop::EventLoopWindowTarget;
use winit::window::Window;

use rustyboi_egui_lib::actions::{GuiAction, SessionUiState};
use rustyboi_egui_lib::Gui;
use rustyboi_session::{DebugDetail, DebugSnapshot};

use crate::renderer::{EguiPaint, PhysicalRect};

/// Extra egui events a platform may inject each frame before the UI runs (used
/// on Android to synthesize IME Text/Backspace events winit drops). Desktop
/// passes an empty vec.
pub type ExtraEvents = Vec<egui::Event>;

/// The per-frame inputs to [`UiHost::run`], grouped so the call stays readable
/// (and doesn't trip `too_many_arguments`). Borrows live only for the call.
pub struct UiRunInputs<'a> {
    /// Whether emulation is paused (drives the Pause/Resume label + overlay).
    pub paused: bool,
    /// The debug read-model for the debug panels, if any are open.
    pub debug: Option<&'a DebugSnapshot>,
    /// The session state the menus render their current selections from.
    pub session: &'a SessionUiState,
    /// Extra egui events to inject before the UI runs (Android IME synthesis).
    pub extra_events: ExtraEvents,
    /// Gamepad buttons currently held (forces a repaint for the keybind editor).
    pub held_pad: &'a std::collections::HashSet<rustyboi_session::input_config::PadButton>,
    /// Force a repaint even when egui sees no change (fresh session snapshot,
    /// status/error text the caller knows about). Desktop always passes `true`.
    pub force_repaint: bool,
}

/// The egui host: context + winit input bridge + the UI.
pub struct UiHost {
    egui_ctx: Context,
    egui_state: egui_winit::State,
    pixels_per_point: f32,
    gui: Gui,
    /// Repaint-gating state (see `run`). `pending_repaint` carries egui's own
    /// "animate me another frame" request forward; the `cached_*` values are the
    /// last laid-out frame's metadata, returned when the UI is reused.
    pending_repaint: bool,
    have_cache: bool,
    cached_ppp: f32,
    cached_region: PhysicalRect,
    cached_menu_open: bool,
}

/// One laid-out egui frame's UI result: the action to apply, whether a menu is
/// open (so the app can auto-pause), and the central region (physical pixels)
/// the game is drawn into.
pub struct UiFrame {
    pub action: Option<GuiAction>,
    pub menu_open: bool,
    pub region: PhysicalRect,
}

impl UiHost {
    /// Build the host bound to `event_loop` (egui-winit needs it for clipboard
    /// / IME wiring). `pixels_per_point` is the initial DPI scale;
    /// `max_texture_size` comes from the wgpu device limits. An optional
    /// externally-owned pending-dialog slot lets the file-picker callback
    /// survive a `UiHost` teardown (Android surface suspend/resume).
    pub fn new<T>(
        event_loop: &EventLoopWindowTarget<T>,
        pixels_per_point: f32,
        max_texture_size: usize,
        pending_dialog_result: Option<Arc<Mutex<Option<GuiAction>>>>,
    ) -> Self {
        let egui_ctx = Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(pixels_per_point),
            Some(max_texture_size),
        );
        let gui = match pending_dialog_result {
            Some(arc) => Gui::with_pending_dialog_result(arc),
            None => Gui::new(),
        };
        Self {
            egui_ctx,
            egui_state,
            pixels_per_point,
            gui,
            pending_repaint: false,
            have_cache: false,
            cached_ppp: pixels_per_point,
            cached_region: PhysicalRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
            cached_menu_open: false,
        }
    }

    /// Clone of the pending-dialog Arc so a caller can keep it alive across a
    /// `UiHost` teardown (Android surface suspend/resume).
    pub fn pending_dialog_result(&self) -> Arc<Mutex<Option<GuiAction>>> {
        self.gui.pending_dialog_result()
    }

    /// Forward a winit window event to egui (mouse/keyboard/touch/IME).
    pub fn handle_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        let _ = self.egui_state.on_window_event(window, event);
    }

    /// Latest on-screen Game Boy control state captured by the egui touch
    /// overlay during the most recent `run`.
    pub fn touch_button_state(&self) -> rustyboi_core_lib::input::ButtonState {
        self.gui.touch_button_state()
    }

    /// Whether egui currently wants keyboard input (a text field is focused,
    /// e.g. the cheat-code entry). The web adapter uses this to suppress
    /// keyboard→GB-button input while the user is typing in the UI.
    pub fn wants_keyboard_input(&self) -> bool {
        self.egui_ctx.wants_keyboard_input()
    }

    /// The heavy [`DebugSnapshot`] sections the currently-open debug panels need.
    /// The caller builds only these; on web, if [`DebugDetail::is_empty`] AND no
    /// baseline-only panel is open (see [`UiHost::any_debug_panel_open`]) nothing
    /// is posted.
    pub fn wanted_debug_detail(&self) -> DebugDetail {
        self.gui.debug_detail()
    }

    /// Whether any debug panel that renders from a snapshot is open. When true the
    /// frontend must supply a snapshot even if [`UiHost::wanted_debug_detail`] is
    /// empty (the CPU / PPU / Breakpoint panels use only the baseline).
    pub fn any_debug_panel_open(&self) -> bool {
        self.gui.any_debug_panel_open()
    }

    /// Update the DPI scale (winit `ScaleFactorChanged`).
    pub fn set_pixels_per_point(&mut self, scale: f32) {
        self.pixels_per_point = scale;
    }

    pub fn set_error(&mut self, message: String) {
        self.gui.set_error(message);
    }

    pub fn clear_error(&mut self) {
        self.gui.clear_error();
    }

    pub fn set_status(&mut self, message: String) {
        self.gui.set_status(message);
    }

    /// Mutable access to the Android ROM library panel (JNI callbacks push
    /// tree-URI / scan-results / status text into it).
    #[cfg(target_os = "android")]
    pub fn library_panel_mut(&mut self) -> &mut rustyboi_egui_lib::library::LibraryPanel {
        self.gui.library_panel_mut()
    }

    /// Run one egui frame. `extra_events` are injected before the UI runs
    /// (Android IME synthesis). Returns the paint output for the renderer plus
    /// the UI result (action / menu-open / game region in physical pixels).
    pub fn run(&mut self, window: &Window, inputs: UiRunInputs) -> (EguiPaint, UiFrame) {
        let UiRunInputs {
            paused,
            debug,
            session,
            extra_events,
            held_pad,
            force_repaint,
        } = inputs;
        let mut raw_input = self.egui_state.take_egui_input(window);
        raw_input.events.extend(extra_events);

        // Repaint gating: when nothing can have changed the UI this frame — no
        // input events, egui isn't animating (no pending repaint), and the caller
        // didn't force it — reuse last frame's egui geometry so the renderer can
        // skip egui's per-frame vertex/texture upload. The game texture is
        // uploaded + drawn separately, so it still animates every frame. The
        // caller passes `force_repaint` for anything egui can't see (a fresh
        // session snapshot, status/error text); desktop passes `true` (no gating).
        // Held pad buttons aren't egui events, so force a repaint while any is
        // down — the keybind editor captures gamepad presses each frame.
        let dirty = force_repaint
            || !raw_input.events.is_empty()
            || !held_pad.is_empty()
            || self.pending_repaint
            || !self.have_cache;
        if !dirty {
            return (
                EguiPaint {
                    jobs: Vec::new(),
                    textures: egui::TexturesDelta::default(),
                    pixels_per_point: self.cached_ppp,
                    reuse: true,
                },
                UiFrame {
                    action: None,
                    menu_open: self.cached_menu_open,
                    region: self.cached_region,
                },
            );
        }

        let mut ui_result = None;
        let full_output = self.egui_ctx.run(raw_input, |egui_ctx| {
            ui_result =
                Some(self.gui.ui(egui_ctx, paused, debug, session, held_pad));
        });

        self.egui_state
            .handle_platform_output(window, full_output.platform_output);
        self.pending_repaint = self.egui_ctx.has_requested_repaint();

        // Use egui's *authoritative* pixels-per-point for both tessellation and
        // the renderer's ScreenDescriptor. egui-winit keeps this in sync with the
        // window's DPI (`take_egui_input` feeds the viewport's native ppp; DPI
        // changes arrive as window events), so it is always correct for the
        // frame we just laid out. Our own `self.pixels_per_point` was seeded once
        // at construction and only refreshed on `ScaleFactorChanged` — using it
        // here caused a layout/raster mismatch (glyphs too wide + aliased) when
        // the two drifted. Keep the field only to seed the constructor.
        let ppp = self.egui_ctx.pixels_per_point();
        let jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
        let paint = EguiPaint {
            jobs,
            textures: full_output.textures_delta,
            pixels_per_point: ppp,
            reuse: false,
        };

        let frame = match ui_result {
            Some(out) => {
                // egui reports the central region in logical points; convert to
                // physical pixels for the renderer's scissor/viewport.
                let c = out.central_rect;
                let region = PhysicalRect {
                    x: c.x * ppp,
                    y: c.y * ppp,
                    width: c.width * ppp,
                    height: c.height * ppp,
                };
                // Cache for reuse frames.
                self.cached_region = region;
                self.cached_menu_open = out.menu_open;
                self.cached_ppp = ppp;
                self.have_cache = true;
                UiFrame { action: out.action, menu_open: out.menu_open, region }
            }
            None => UiFrame {
                action: None,
                menu_open: false,
                region: PhysicalRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
            },
        };

        (paint, frame)
    }
}

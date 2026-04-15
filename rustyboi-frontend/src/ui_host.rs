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

use rustyboi_core_lib::{cpu, gb};
use rustyboi_egui_lib::actions::{GuiAction, SessionUiState};
use rustyboi_egui_lib::Gui;

use crate::renderer::{EguiPaint, PhysicalRect};

/// Extra egui events a platform may inject each frame before the UI runs (used
/// on Android to synthesize IME Text/Backspace events winit drops). Desktop
/// passes an empty vec.
pub type ExtraEvents = Vec<egui::Event>;

/// The egui host: context + winit input bridge + the UI.
pub struct UiHost {
    egui_ctx: Context,
    egui_state: egui_winit::State,
    pixels_per_point: f32,
    gui: Gui,
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
        Self { egui_ctx, egui_state, pixels_per_point, gui }
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
    pub fn run(
        &mut self,
        window: &Window,
        paused: bool,
        registers: Option<&cpu::registers::Registers>,
        gb: Option<&gb::GB>,
        session: &SessionUiState,
        extra_events: ExtraEvents,
    ) -> (EguiPaint, UiFrame) {
        let mut raw_input = self.egui_state.take_egui_input(window);
        raw_input.events.extend(extra_events);

        let mut ui_result = None;
        let full_output = self.egui_ctx.run(raw_input, |egui_ctx| {
            ui_result = Some(self.gui.ui(egui_ctx, paused, registers, gb, session));
        });

        self.egui_state
            .handle_platform_output(window, full_output.platform_output);

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
        };

        let frame = match ui_result {
            Some(out) => {
                // egui reports the central region in logical points; convert to
                // physical pixels for the renderer's scissor/viewport.
                let c = out.central_rect;
                UiFrame {
                    action: out.action,
                    menu_open: out.menu_open,
                    region: PhysicalRect {
                        x: c.x * ppp,
                        y: c.y * ppp,
                        width: c.width * ppp,
                        height: c.height * ppp,
                    },
                }
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

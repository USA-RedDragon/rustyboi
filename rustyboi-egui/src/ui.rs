use rustyboi_core_lib::{cpu, gb, input};

#[cfg(not(target_os = "android"))]
use std::env;
use std::sync::{Arc, Mutex};
use egui::Context;
use crate::actions::{GuiAction, SessionUiState};
// Hardware / palette pickers live only in the desktop Settings menu bar.
#[cfg(not(target_os = "android"))]
use crate::actions::{HardwareChoice, PaletteChoice};
use crate::file_dialog::{self, FileDialogBuilder};
#[cfg(target_os = "android")]
use crate::library::LibraryPanel;
use crate::touch_controls;

pub const PANEL_BACKGROUND: egui::Color32 = egui::Color32::from_rgba_premultiplied(64, 64, 64, 220);

/// Render a single toggle row in the mobile menu overlay. Behaves like
/// `ui.checkbox(...)` but lays out as a full-width row with a check
/// glyph on the right so it matches the rest of the touch-sized rows.
#[cfg(target_os = "android")]
fn mobile_toggle_row(ui: &mut egui::Ui, size: egui::Vec2, label: &str, value: &mut bool) {
    let glyph = if *value { "☑" } else { "☐" };
    let text = format!("{glyph}  {label}");
    if ui
        .add(egui::Button::new(text).min_size(size))
        .clicked()
    {
        *value = !*value;
    }
}

/// The egui central region (in logical egui points, top-left origin) where the
/// emulator framebuffer should be drawn — i.e. below the menu bar and above the
/// status panel. Convert to physical pixels with `pixels_per_point`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CentralRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Result of laying out one egui frame.
pub struct UiOutput {
    pub action: Option<GuiAction>,
    pub menu_open: bool,
    pub central_rect: CentralRect,
}

pub struct Gui {
    error_message: Option<String>,
    #[cfg(not(target_os = "android"))]
    status_message: Option<String>,
    show_cpu_registers: bool,
    show_stack_explorer: bool,
    show_memory_explorer: bool,
    show_ppu_debug: bool,
    show_sprite_debug: bool,
    show_palette_explorer: bool,
    show_tile_explorer: bool,
    show_keybind_settings: bool,
    show_breakpoint_panel: bool,
    breakpoint_address_input: String,
    pub(super) stack_scroll_offset: i16,
    pub(super) memory_explorer_address: String,
    pub(super) memory_explorer_parsed_address: u16,
    pub(super) memory_scroll_offset: i16,
    pub(super) step_count: u32,
    // Button hold state tracking
    pub(super) step_cycles_held_frames: u32,
    pub(super) step_frames_held_frames: u32,
    // Sprite debug state
    pub(super) selected_sprite_index: Option<u8>,
    // Tile explorer state for CGB
    pub(super) tile_explorer_vram_bank: u8,
    pub(super) tile_explorer_palette: u8,
    // File dialog result tracking
    pending_dialog_result: Arc<Mutex<Option<GuiAction>>>,
    // On-screen Game Boy controls state (mirrors winit `key_held` on desktop).
    // Mutated by the touch panel each frame; latest snapshot read by the
    // platform loop and OR'd with keyboard input.
    touch_buttons: input::ButtonState,
    // Tracks active multi-touch positions across frames so the touch
    // overlay can recognise more than one finger at once.
    touch_state: touch_controls::TouchState,
    // Whether to draw the on-screen Game Boy controls. Defaults to true on
    // Android, false on other platforms; can be toggled via the View menu.
    show_touch_controls: bool,
    /// Android-only on-screen ROM library. Opened automatically when no
    /// ROM is loaded so the user has a path forward.
    #[cfg(target_os = "android")]
    library: LibraryPanel,
    /// Android-only state: whether the full-screen menu overlay,
    /// triggered by the floating ☰ soft button, is currently visible.
    /// Replaces the desktop top menu bar on mobile.
    #[cfg(target_os = "android")]
    show_mobile_menu: bool,
}

impl Default for Gui {
    fn default() -> Self {
        Self::new()
    }
}

impl Gui {
    pub fn new() -> Self {
        Self::with_pending_dialog_result(Arc::new(Mutex::new(None)))
    }

    /// Create a `Gui` that shares an externally-owned pending dialog result
    /// slot. Used on Android, where the rendering surface (and hence the
    /// `Framework`/`Gui`) is torn down while the SAF picker is in front;
    /// keeping the Arc outside the `Gui` lets the picked-file callback
    /// land in the slot that the *next* `Gui` will read.
    pub fn with_pending_dialog_result(
        pending_dialog_result: Arc<Mutex<Option<GuiAction>>>,
    ) -> Self {
        Self {
            error_message: None,
            #[cfg(not(target_os = "android"))]
            status_message: None,
            show_cpu_registers: false,
            show_stack_explorer: false,
            show_memory_explorer: false,
            show_ppu_debug: false,
            show_sprite_debug: false,
            show_palette_explorer: false,
            show_tile_explorer: false,
            show_keybind_settings: false,
            show_breakpoint_panel: false,
            breakpoint_address_input: String::from("0000"),
            stack_scroll_offset: 0,
            memory_explorer_address: String::from("0000"),
            memory_explorer_parsed_address: 0x0000,
            memory_scroll_offset: 0,
            step_count: 1,
            step_cycles_held_frames: 0,
            step_frames_held_frames: 0,
            selected_sprite_index: None,
            tile_explorer_vram_bank: 0,
            tile_explorer_palette: 0,
            pending_dialog_result,
            touch_buttons: input::ButtonState::default(),
            touch_state: touch_controls::TouchState::default(),
            show_touch_controls: cfg!(target_os = "android"),
            #[cfg(target_os = "android")]
            library: {
                let mut p = LibraryPanel::default();
                p.open = true;
                p
            },
            #[cfg(target_os = "android")]
            show_mobile_menu: false,
        }
    }

    /// Mutable access to the Android ROM library panel. The platform
    /// event loop uses this to push scan results, tree-URI updates,
    /// and status text in from native callbacks.
    #[cfg(target_os = "android")]
    pub fn library_panel_mut(&mut self) -> &mut LibraryPanel {
        &mut self.library
    }

    /// Clone of the pending-dialog Arc so callers can persist it across
    /// `Gui`/`Framework` recreation (Android surface suspend/resume).
    pub fn pending_dialog_result(&self) -> Arc<Mutex<Option<GuiAction>>> {
        Arc::clone(&self.pending_dialog_result)
    }

    /// Latest on-screen control state captured this frame. Read by the
    /// platform loop after `ui()` and OR'd with keyboard input before
    /// being handed to the emulator.
    pub fn touch_button_state(&self) -> input::ButtonState {
        self.touch_buttons
    }

    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &Context, paused: bool, registers: Option<&cpu::registers::Registers>, gb: Option<&gb::GB>, session: &SessionUiState) -> UiOutput {
        let mut action = None;
        let mut any_menu_open = false;

        // Check for pending dialog results first
        if let Ok(mut pending) = self.pending_dialog_result.try_lock()
            && let Some(pending_action) = pending.take() {
                action = Some(pending_action);
            }

        // The desktop menu bar consumes the top of the screen with
        // hover-driven submenus, which is unusable on a touch device.
        // On Android the same actions are surfaced via a floating
        // ☰ soft button + full-screen overlay (see
        // `render_mobile_soft_button` / `render_mobile_menu_overlay`).
        #[cfg(not(target_os = "android"))]
        self.render_menu_bar(
            ctx,
            &mut action,
            &mut any_menu_open,
            paused,
            gb.map(|g| g.printer_attached()),
            session,
        );
        self.render_debug_panels(ctx, registers, gb, &mut action, paused);
        #[cfg(target_os = "android")]
        if let Some(lib_action) = self.library.show(ctx) {
            action = Some(lib_action);
        }
        #[cfg(not(target_os = "android"))]
        self.render_status_panel(ctx);

        // The central region left over after the top menu bar and bottom status
        // panel have claimed their space (debug panels are floating Windows and
        // don't shrink it). Captured in egui points before the error panel — if
        // shown — consumes the central area. The emulator framebuffer must be
        // drawn only inside this rect. Recomputed every frame, so it tracks the
        // menu bar opening/closing, theme/font changes, DPI and resizes.
        let central = ctx.available_rect();
        let central_rect = CentralRect {
            x: central.min.x,
            y: central.min.y,
            width: central.width().max(0.0),
            height: central.height().max(0.0),
        };

        self.render_error_panel(ctx, &mut action);

        // Android mobile menu: floating soft button + full-screen
        // overlay. Rendered after the debug panels / error overlay so
        // it can sit above any background UI, but before the touch
        // overlay so the overlay's backdrop intercepts touches that
        // would otherwise press D-pad / A-B buttons underneath.
        #[cfg(target_os = "android")]
        {
            self.render_mobile_soft_button(ctx);
            if self.show_mobile_menu {
                self.render_mobile_menu_overlay(ctx, &mut action, paused, session);
                any_menu_open = true;
            }
        }

        // Suppress on-screen controls while the mobile menu overlay is
        // open so taps on menu items do not also fire emulator inputs.
        let suppress_touch = {
            #[cfg(target_os = "android")]
            { self.show_mobile_menu }
            #[cfg(not(target_os = "android"))]
            { false }
        };
        if self.show_touch_controls && !suppress_touch {
            self.touch_buttons = touch_controls::show(ctx, &mut self.touch_state);
        } else {
            self.touch_buttons = input::ButtonState::default();
        }

        UiOutput {
            action,
            menu_open: any_menu_open,
            central_rect,
        }
    }
    #[cfg(not(target_os = "android"))]
    fn render_menu_bar(&mut self, ctx: &Context, action: &mut Option<GuiAction>, any_menu_open: &mut bool, paused: bool, printer_attached: Option<bool>, session: &SessionUiState) {
        egui::TopBottomPanel::top("menubar_container").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    *any_menu_open = true;
                    #[cfg(target_os = "android")]
                    {
                        if ui.button("ROM Library…").clicked() {
                            self.library.open = true;
                            ui.close_menu();
                        }
                        ui.separator();
                    }
                    if ui.button("Load ROM").clicked() {
                        let mut dialog = file_dialog::new()
                            .add_filter("Game Boy ROM", &["gb", "gbc", "zip"])
                            .add_filter("All Files", &["*"]);
                        if env::current_dir().is_ok() {
                            dialog = dialog.set_directory(env::current_dir().unwrap());
                        }
                        let result_holder = Arc::clone(&self.pending_dialog_result);
                        dialog.pick_file(move |file_data| {
                            if let Some(file_data) = file_data
                                && let Ok(mut pending) = result_holder.lock() {
                                    *pending = Some(GuiAction::LoadRom(file_data));
                            }
                        });
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Save State").clicked() {
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let file_name = format!("save_{}", timestamp);
                        let mut dialog = file_dialog::new()
                            .add_filter("RustyBoi Save State", &["rustyboisave"])
                            .set_file_name(file_name);
                        if env::current_dir().is_ok() {
                            dialog = dialog.set_directory(env::current_dir().unwrap());
                        }
                        let result_holder = Arc::clone(&self.pending_dialog_result);
                        dialog.save_file(move |path| {
                            if let Some(path) = path
                                && let Ok(mut pending) = result_holder.lock() {
                                    *pending = Some(GuiAction::SaveState(path));
                                }
                        });
                        ui.close_menu();
                    }
                    if ui.button("Load State").clicked() {
                        let mut dialog = file_dialog::new()
                            .add_filter("RustyBoi Save State", &["rustyboisave"])
                            .add_filter("All Files", &["*"]);
                        if env::current_dir().is_ok() {
                            dialog = dialog.set_directory(env::current_dir().unwrap());
                        }
                        let result_holder = Arc::clone(&self.pending_dialog_result);
                        dialog.pick_file(move |file_data| {
                            if let Some(file_data) = file_data
                                && let Ok(mut pending) = result_holder.lock() {
                                    *pending = Some(GuiAction::LoadState(file_data));
                            }
                        });
                        ui.close_menu();
                    }
                    ui.separator();
                    // Quick + numbered savestate slots (via the session). The
                    // quick slot has dedicated hotkeys (F5/F8); the numbered
                    // slots (0-9) are keyed by ROM id under the save dir.
                    if ui.button("Quicksave (F5)").clicked() {
                        *action = Some(GuiAction::Quicksave);
                        ui.close_menu();
                    }
                    if ui.button("Quickload (F8)").clicked() {
                        *action = Some(GuiAction::Quickload);
                        ui.close_menu();
                    }
                    ui.menu_button("Save State to Slot", |ui| {
                        for slot in 0u32..10 {
                            let filled = session.slots.contains(&slot);
                            let label = if filled {
                                format!("Slot {slot} (overwrite)")
                            } else {
                                format!("Slot {slot}")
                            };
                            if ui.button(label).clicked() {
                                *action = Some(GuiAction::SaveSlot(slot));
                                ui.close_menu();
                            }
                        }
                    });
                    ui.menu_button("Load State from Slot", |ui| {
                        if session.slots.is_empty() {
                            ui.label("No saved slots");
                        }
                        for &slot in &session.slots {
                            if ui.button(format!("Slot {slot}")).clicked() {
                                *action = Some(GuiAction::LoadSlot(slot));
                                ui.close_menu();
                            }
                        }
                    });
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        *action = Some(GuiAction::Exit);
                        ui.close_menu();
                    }
                });

                ui.menu_button("Emulation", |ui| {
                    *any_menu_open = true;
                    if ui.button("Restart").clicked() {
                        *action = Some(GuiAction::Restart);
                        ui.close_menu();
                    }
                    ui.separator();
                    let pause_text = if paused { "Resume" } else { "Pause" };
                    if ui.button(pause_text).clicked() {
                        *action = Some(GuiAction::TogglePause);
                        ui.close_menu();
                    }
                    let ff_text = if session.fast_forward {
                        "Fast-Forward: On (Tab)"
                    } else {
                        "Fast-Forward: Off (Tab)"
                    };
                    if ui.button(ff_text).clicked() {
                        *action = Some(GuiAction::ToggleFastForward);
                        ui.close_menu();
                    }
                    if ui.button("Frame Advance (Backslash)").clicked() {
                        *action = Some(GuiAction::FrameAdvance);
                        ui.close_menu();
                    }
                    ui.separator();
                    let mut sgb_border = session.sgb_border;
                    if ui.checkbox(&mut sgb_border, "SGB border").clicked() {
                        *action = Some(GuiAction::ToggleSgbBorder);
                        ui.close_menu();
                    }
                    if let Some(attached) = printer_attached {
                        ui.separator();
                        let printer_text = if attached {
                            "Disconnect Game Boy Printer"
                        } else {
                            "Connect Game Boy Printer"
                        };
                        if ui.button(printer_text).clicked() {
                            *action = Some(GuiAction::TogglePrinter);
                            ui.close_menu();
                        }
                    }
                });

                ui.menu_button("Debug", |ui| {
                    *any_menu_open = true;
                    ui.checkbox(&mut self.show_cpu_registers, "CPU Registers");
                    ui.checkbox(&mut self.show_stack_explorer, "Stack Explorer");
                    ui.checkbox(&mut self.show_memory_explorer, "Memory Explorer");
                    ui.checkbox(&mut self.show_ppu_debug, "PPU");
                    ui.checkbox(&mut self.show_sprite_debug, "Sprite Debug");
                    ui.checkbox(&mut self.show_palette_explorer, "Palette Explorer");
                    ui.checkbox(&mut self.show_tile_explorer, "Tile Explorer");
                    ui.separator();
                    ui.checkbox(&mut self.show_breakpoint_panel, "Breakpoint Manager");
                });

                ui.menu_button("Settings", |ui| {
                    *any_menu_open = true;
                    ui.checkbox(&mut self.show_keybind_settings, "Keybind Settings");

                    ui.separator();
                    ui.menu_button("Hardware Model", |ui| {
                        for (choice, label) in [
                            (HardwareChoice::Dmg, "DMG (Game Boy)"),
                            (HardwareChoice::Cgb, "CGB (Game Boy Color)"),
                            (HardwareChoice::Sgb, "SGB (Super Game Boy)"),
                        ] {
                            let selected = session.hardware == choice;
                            if ui.radio(selected, label).clicked() && !selected {
                                *action = Some(GuiAction::SetHardware(choice));
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        ui.small("Changing hardware restarts the ROM.");
                    });

                    ui.menu_button("DMG Palette", |ui| {
                        for (choice, label) in [
                            (PaletteChoice::Grayscale, "Grayscale"),
                            (PaletteChoice::OriginalGreen, "Original Green"),
                            (PaletteChoice::Blue, "Blue"),
                            (PaletteChoice::Brown, "Brown"),
                            (PaletteChoice::Red, "Red"),
                        ] {
                            let selected = session.palette == choice;
                            if ui.radio(selected, label).clicked() && !selected {
                                *action = Some(GuiAction::SetPalette(choice));
                                ui.close_menu();
                            }
                        }
                    });

                    ui.menu_button("Rewind", |ui| {
                        let mut enabled = session.rewind_enabled;
                        if ui.checkbox(&mut enabled, "Enable rewind").clicked() {
                            *action = Some(GuiAction::SetRewindEnabled(enabled));
                        }
                        ui.separator();
                        ui.label("Snapshot interval (frames)");
                        for interval in [2u32, 4, 6, 10] {
                            let selected = session.rewind_interval_frames == interval;
                            if ui.radio(selected, format!("{interval}")).clicked() && !selected {
                                *action = Some(GuiAction::SetRewindInterval(interval));
                            }
                        }
                        ui.separator();
                        ui.label("History depth (snapshots)");
                        for depth in [30usize, 60, 90, 180] {
                            let selected = session.rewind_depth == depth;
                            if ui.radio(selected, format!("{depth}")).clicked() && !selected {
                                *action = Some(GuiAction::SetRewindDepth(depth));
                            }
                        }
                    });
                });
            });
        });
    }

    fn render_debug_panels(&mut self, ctx: &Context, registers: Option<&cpu::registers::Registers>, gb: Option<&gb::GB>, action: &mut Option<GuiAction>, paused: bool) {
        if self.show_cpu_registers {
            self.render_cpu_registers_panel(ctx, registers, gb, action, paused);
        }

        if self.show_stack_explorer {
            self.render_stack_explorer_panel(ctx, registers, gb);
        }

        if self.show_memory_explorer {
            self.render_memory_explorer_panel(ctx, gb);
        }

        if self.show_ppu_debug {
            self.render_ppu_debug_panel(ctx, gb);
        }

        if self.show_sprite_debug {
            self.render_sprite_debug_panel(ctx, gb);
        }

        if self.show_palette_explorer {
            self.render_palette_explorer_panel(ctx, gb);
        }

        if self.show_tile_explorer {
            self.render_tile_explorer_panel(ctx, gb);
        }

        if self.show_keybind_settings {
            self.render_keybind_settings_panel(ctx);
        }

        if self.show_breakpoint_panel {
            self.render_breakpoint_panel(ctx, action, gb);
        }
    }

    #[cfg(not(target_os = "android"))]
    fn render_status_panel(&mut self, ctx: &Context) {
        if let Some(status_msg) = &self.status_message.clone() {
            let mut clear_status = false;
            // Floating overlay (not a bottom panel): it must paint *over* the
            // framebuffer without claiming layout space, so `available_rect`
            // stays full and the emulator image never shifts when a status
            // message appears/disappears.
            egui::Area::new(egui::Id::new("status_overlay"))
                .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
                .interactable(true)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("✅");
                            ui.label(status_msg);
                            if ui.button("✕").clicked() {
                                clear_status = true;
                            }
                        });
                    });
                });

            if clear_status {
                self.status_message = None;
            }
        }
    }

    fn render_error_panel(&mut self, ctx: &Context, action: &mut Option<GuiAction>) {
        if let Some(error_msg) = &self.error_message.clone() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("🚨 Emulator Crashed");
                ui.separator();

                ui.label("The Game Boy emulator has encountered a fatal error and has stopped running.");
                ui.label("The GUI remains open for debugging purposes.");

                ui.add_space(10.0);

                ui.label("Error Details:");
                ui.group(|ui| {
                    ui.add(egui::TextEdit::multiline(&mut error_msg.as_str())
                        .desired_width(f32::INFINITY)
                        .desired_rows(6)
                        .font(egui::TextStyle::Monospace));
                });

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.button("🔄 Restart Emulation").clicked() {
                        *action = Some(GuiAction::Restart);
                    }

                    if ui.button("Clear Error (Debug Mode)").clicked() {
                        *action = Some(GuiAction::ClearError);
                    }
                });
            });
        }
    }

    pub fn set_error(&mut self, error_message: String) {
        self.error_message = Some(error_message);
    }

    pub fn clear_error(&mut self) {
        self.error_message = None;
    }

    pub fn set_status(&mut self, status_message: String) {
        #[cfg(target_os = "android")]
        {
            // On Android, route transient status messages through a
            // native system Toast instead of the desktop-style bottom
            // status panel. The egui `status_message` field is left
            // unset so `render_status_panel` (also skipped on Android)
            // does not draw anything.
            crate::android_bridge::show_toast(status_message);
        }
        #[cfg(not(target_os = "android"))]
        {
            self.status_message = Some(status_message);
        }
    }

    /// Compute the touch-sized "unit" the mobile UI uses to scale
    /// itself. Mirrors the formula in [`touch_controls`] so the soft
    /// button and on-screen D-pad/A-B groups stay visually consistent
    /// across phones, tablets and foldables.
    #[cfg(target_os = "android")]
    fn mobile_unit(ctx: &Context) -> f32 {
        let screen = ctx.screen_rect();
        (screen.height() * 0.18)
            .min(screen.width() * 0.09)
            .clamp(56.0, 130.0)
    }

    /// Draw the floating ☰ soft button in the top-left corner. Tapping
    /// it toggles the full-screen mobile menu overlay. Replaces the
    /// desktop top menu bar on Android.
    ///
    /// Painted manually (rect + centered glyph) to match the on-screen
    /// touch controls' style exactly — using `egui::Button` here gave
    /// it extra padding, a left-aligned glyph (because `min_size` only
    /// expands the frame, not the text rect), and a darker fill than
    /// the D-pad/A-B buttons.
    #[cfg(target_os = "android")]
    fn render_mobile_soft_button(&mut self, ctx: &Context) {
        let unit = Self::mobile_unit(ctx) * 0.75;
        let margin = unit * 0.35 * 0.75;
        let screen = ctx.screen_rect();
        let pos = egui::Pos2::new(screen.left() + margin, screen.top() + margin);
        let size = egui::Vec2::splat(unit);

        egui::Area::new(egui::Id::new("mobile_menu_soft_button"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                // Match touch_controls::draw_button colors so the soft
                // button reads as part of the same overlay family.
                let fill = if resp.is_pointer_button_down_on() {
                    egui::Color32::from_rgba_premultiplied(220, 220, 220, 220)
                } else {
                    egui::Color32::from_rgba_premultiplied(60, 60, 60, 160)
                };
                let stroke = egui::Stroke::new(
                    2.0,
                    egui::Color32::from_rgba_premultiplied(230, 230, 230, 220),
                );
                let painter = ui.painter();
                painter.rect(rect, rect.width() * 0.18, fill, stroke);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "☰",
                    egui::FontId::proportional(rect.width() * 0.55),
                    egui::Color32::WHITE,
                );
                if resp.clicked() {
                    self.show_mobile_menu = !self.show_mobile_menu;
                }
            });
    }

    /// Draw the full-screen menu overlay that takes the place of the
    /// desktop top menu bar on Android. The overlay consists of:
    ///   1. A dimmed full-screen backdrop `Area` that swallows taps
    ///      (and dismisses the menu when tapped outside the panel).
    ///   2. A centered title-less `Window` with the same actions the
    ///      desktop bar exposes — File / Emulation / Debug / Settings —
    ///      laid out as wide vertically-stacked buttons for touch.
    #[cfg(target_os = "android")]
    fn render_mobile_menu_overlay(
        &mut self,
        ctx: &Context,
        action: &mut Option<GuiAction>,
        paused: bool,
        session: &SessionUiState,
    ) {
        let screen = ctx.screen_rect();
        let unit = Self::mobile_unit(ctx);
        let row_height = unit * 0.6;

        // Dimmed backdrop. Allocates a full-screen click-sense rect
        // so taps outside the menu panel close the menu.
        let mut close_requested = false;
        egui::Area::new(egui::Id::new("mobile_menu_backdrop"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.left_top())
            .show(ctx, |ui| {
                let (rect, resp) =
                    ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter().rect_filled(
                    rect,
                    0.0,
                    egui::Color32::from_black_alpha(160),
                );
                if resp.clicked() {
                    close_requested = true;
                }
            });

        let panel_width = (screen.width() * 0.8).clamp(320.0, 640.0);
        let panel_max_height = screen.height() * 0.85;

        // Menu panel. Rendered AFTER the backdrop in this frame so it
        // paints on top of the dimming layer despite both layers being
        // at `Order::Foreground`.
        egui::Window::new("mobile_menu_window")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(egui::Vec2::new(panel_width, panel_max_height))
            .frame(egui::Frame::window(&ctx.style()).fill(PANEL_BACKGROUND))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Menu");
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("✕").size(row_height * 0.55),
                                    )
                                    .min_size(egui::Vec2::new(row_height, row_height)),
                                )
                                .clicked()
                            {
                                close_requested = true;
                            }
                        },
                    );
                });
                ui.separator();

                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .max_height(panel_max_height - row_height * 2.0)
                    .show(ui, |ui| {
                        let row_size =
                            egui::Vec2::new(ui.available_width(), row_height);
                        let mut close_after_action = false;

                        // --- File -----------------------------------
                        ui.label(egui::RichText::new("File").strong());
                        if ui
                            .add(
                                egui::Button::new("ROM Library…").min_size(row_size),
                            )
                            .clicked()
                        {
                            self.library.open = true;
                            close_after_action = true;
                        }
                        if ui
                            .add(egui::Button::new("Load ROM").min_size(row_size))
                            .clicked()
                        {
                            let dialog = file_dialog::new()
                                .add_filter("Game Boy ROM", &["gb", "gbc", "zip"])
                                .add_filter("All Files", &["*"]);
                            let result_holder = Arc::clone(&self.pending_dialog_result);
                            dialog.pick_file(move |file_data| {
                                if let Some(file_data) = file_data
                                    && let Ok(mut pending) = result_holder.lock()
                                {
                                    *pending = Some(GuiAction::LoadRom(file_data));
                                }
                            });
                            close_after_action = true;
                        }
                        if ui
                            .add(egui::Button::new("Save State").min_size(row_size))
                            .clicked()
                        {
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs();
                            let file_name = format!("save_{}", timestamp);
                            let dialog = file_dialog::new()
                                .add_filter("RustyBoi Save State", &["rustyboisave"])
                                .set_file_name(file_name);
                            let result_holder = Arc::clone(&self.pending_dialog_result);
                            dialog.save_file(move |path| {
                                if let Some(path) = path
                                    && let Ok(mut pending) = result_holder.lock()
                                {
                                    *pending = Some(GuiAction::SaveState(path));
                                }
                            });
                            close_after_action = true;
                        }
                        if ui
                            .add(egui::Button::new("Load State").min_size(row_size))
                            .clicked()
                        {
                            let dialog = file_dialog::new()
                                .add_filter("RustyBoi Save State", &["rustyboisave"])
                                .add_filter("All Files", &["*"]);
                            let result_holder = Arc::clone(&self.pending_dialog_result);
                            dialog.pick_file(move |file_data| {
                                if let Some(file_data) = file_data
                                    && let Ok(mut pending) = result_holder.lock()
                                {
                                    *pending = Some(GuiAction::LoadState(file_data));
                                }
                            });
                            close_after_action = true;
                        }
                        if ui
                            .add(egui::Button::new("Exit").min_size(row_size))
                            .clicked()
                        {
                            *action = Some(GuiAction::Exit);
                            close_after_action = true;
                        }

                        ui.add_space(row_height * 0.25);

                        // --- Emulation ------------------------------
                        ui.label(egui::RichText::new("Emulation").strong());
                        if ui
                            .add(egui::Button::new("Restart").min_size(row_size))
                            .clicked()
                        {
                            *action = Some(GuiAction::Restart);
                            close_after_action = true;
                        }
                        let pause_text = if paused { "Resume" } else { "Pause" };
                        if ui
                            .add(egui::Button::new(pause_text).min_size(row_size))
                            .clicked()
                        {
                            *action = Some(GuiAction::TogglePause);
                            close_after_action = true;
                        }
                        if ui
                            .add(egui::Button::new("Quicksave").min_size(row_size))
                            .clicked()
                        {
                            *action = Some(GuiAction::Quicksave);
                            close_after_action = true;
                        }
                        if ui
                            .add(egui::Button::new("Quickload").min_size(row_size))
                            .clicked()
                        {
                            *action = Some(GuiAction::Quickload);
                            close_after_action = true;
                        }
                        let ff_text = if session.fast_forward {
                            "Fast-Forward: On"
                        } else {
                            "Fast-Forward: Off"
                        };
                        if ui
                            .add(egui::Button::new(ff_text).min_size(row_size))
                            .clicked()
                        {
                            *action = Some(GuiAction::ToggleFastForward);
                            close_after_action = true;
                        }
                        if ui
                            .add(egui::Button::new("Frame Advance").min_size(row_size))
                            .clicked()
                        {
                            *action = Some(GuiAction::FrameAdvance);
                            close_after_action = true;
                        }
                        {
                            let mut sgb_border = session.sgb_border;
                            mobile_toggle_row(ui, row_size, "SGB border", &mut sgb_border);
                            if sgb_border != session.sgb_border {
                                *action = Some(GuiAction::ToggleSgbBorder);
                            }
                        }

                        ui.add_space(row_height * 0.25);

                        // --- Debug ----------------------------------
                        ui.label(egui::RichText::new("Debug").strong());
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "CPU Registers",
                            &mut self.show_cpu_registers,
                        );
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "Stack Explorer",
                            &mut self.show_stack_explorer,
                        );
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "Memory Explorer",
                            &mut self.show_memory_explorer,
                        );
                        mobile_toggle_row(ui, row_size, "PPU", &mut self.show_ppu_debug);
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "Sprite Debug",
                            &mut self.show_sprite_debug,
                        );
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "Palette Explorer",
                            &mut self.show_palette_explorer,
                        );
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "Tile Explorer",
                            &mut self.show_tile_explorer,
                        );
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "Breakpoint Manager",
                            &mut self.show_breakpoint_panel,
                        );

                        ui.add_space(row_height * 0.25);

                        // --- Settings -------------------------------
                        ui.label(egui::RichText::new("Settings").strong());
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "Keybind Settings",
                            &mut self.show_keybind_settings,
                        );
                        // View toggle: lets the user hide the touch
                        // overlay even on Android (useful with a Bluetooth
                        // gamepad).
                        mobile_toggle_row(
                            ui,
                            row_size,
                            "On-screen Controls",
                            &mut self.show_touch_controls,
                        );

                        if close_after_action {
                            close_requested = true;
                        }
                    });
            });

        if close_requested {
            self.show_mobile_menu = false;
        }
    }

    fn render_breakpoint_panel(&mut self, ctx: &Context, action: &mut Option<GuiAction>, gb: Option<&gb::GB>) {
        egui::Window::new("Breakpoint Manager")
            .default_width(300.0)
            .frame(egui::Frame::window(&ctx.style()).fill(PANEL_BACKGROUND))
            .show(ctx, |ui| {
                ui.heading("Breakpoints");
                ui.separator();

                // Input for new breakpoint address
                ui.horizontal(|ui| {
                    ui.label("Address:");
                    ui.add(egui::TextEdit::singleline(&mut self.breakpoint_address_input)
                        .desired_width(80.0)
                        .font(egui::TextStyle::Monospace));

                    if ui.button("Add").clicked() {
                        // Parse the address from hex string
                        if let Ok(address) = u16::from_str_radix(self.breakpoint_address_input.trim_start_matches("0x"), 16) {
                            *action = Some(GuiAction::SetBreakpoint(address));
                            self.breakpoint_address_input = String::from("0000");
                        }
                    }
                });

                ui.small("Enter address in hex format (e.g., 0100, FFAA)");
                ui.separator();

                // Display current breakpoints if we have access to GB
                if let Some(gb) = gb {
                    ui.label("Active Breakpoints:");
                    ui.separator();

                    let breakpoints: Vec<u16> = gb.get_breakpoints().iter().cloned().collect();
                    if breakpoints.is_empty() {
                        ui.label("No breakpoints set");
                    } else {
                        // Sort breakpoints for consistent display
                        let mut sorted_breakpoints = breakpoints.clone();
                        sorted_breakpoints.sort();

                        for &address in &sorted_breakpoints {
                            ui.horizontal(|ui| {
                                ui.monospace(format!("{:04X}", address));
                                if ui.small_button("✕").clicked() {
                                    *action = Some(GuiAction::RemoveBreakpoint(address));
                                }
                            });
                        }

                        ui.separator();
                        if ui.button("Clear All").clicked() {
                            // Remove all breakpoints by sending individual remove actions
                            // We'll handle this in the main loop
                            for &address in &breakpoints {
                                *action = Some(GuiAction::RemoveBreakpoint(address));
                            }
                        }
                    }

                    ui.separator();
                    ui.small("Click ✕ to remove a breakpoint");
                } else {
                    ui.label("Game Boy not available");
                }
            });
    }
}

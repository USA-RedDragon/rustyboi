use rustyboi_core_lib::{cpu, gb};

use std::env;
use std::sync::{Arc, Mutex};
use egui::Context;
use crate::actions::GuiAction;
use crate::file_dialog::{self, FileDialogBuilder};

pub const PANEL_BACKGROUND: egui::Color32 = egui::Color32::from_rgba_premultiplied(64, 64, 64, 220);

pub struct Gui {
    error_message: Option<String>,
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
}

impl Gui {
    pub fn new() -> Self {
        Self { 
            error_message: None,
            status_message: None,
            show_cpu_registers: true,
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
            pending_dialog_result: Arc::new(Mutex::new(None)),
        }
    }

    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &Context, paused: bool, registers: Option<&cpu::registers::Registers>, gb: Option<&gb::GB>) -> (Option<GuiAction>, bool) {
        let mut action = None;
        let mut any_menu_open = false;
        
        // Check for pending dialog results first
        if let Ok(mut pending) = self.pending_dialog_result.try_lock()
            && let Some(pending_action) = pending.take() {
                action = Some(pending_action);
            }
        
        self.render_menu_bar(ctx, &mut action, &mut any_menu_open, paused);
        self.render_debug_panels(ctx, registers, gb, &mut action, paused);
        self.render_status_panel(ctx);
        self.render_error_panel(ctx, &mut action);
        
        (action, any_menu_open)
    }

    fn render_menu_bar(&mut self, ctx: &Context, action: &mut Option<GuiAction>, any_menu_open: &mut bool, paused: bool) {
        egui::TopBottomPanel::top("menubar_container").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    *any_menu_open = true;
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

    fn render_status_panel(&mut self, ctx: &Context) {
        if let Some(status_msg) = &self.status_message.clone() {
            let mut clear_status = false;
            egui::TopBottomPanel::bottom("status_panel").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("✅");
                    ui.label(status_msg);
                    if ui.button("✕").clicked() {
                        clear_status = true;
                    }
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
        self.status_message = Some(status_message);
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

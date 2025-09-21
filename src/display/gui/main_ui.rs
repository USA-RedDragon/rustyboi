use std::env;
use egui::Context;
use super::actions::GuiAction;

pub(crate) struct Gui {
    error_message: Option<String>,
    status_message: Option<String>,
    show_cpu_registers: bool,
    show_stack_explorer: bool,
    show_memory_explorer: bool,
    show_ppu_debug: bool,
    show_sprite_debug: bool,
    show_palette_explorer: bool,
    show_tile_explorer: bool,
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
}

impl Gui {
    pub(crate) fn new() -> Self {
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
            stack_scroll_offset: 0,
            memory_explorer_address: String::from("0000"),
            memory_explorer_parsed_address: 0x0000,
            memory_scroll_offset: 0,
            step_count: 1,
            step_cycles_held_frames: 0,
            step_frames_held_frames: 0,
            selected_sprite_index: None,
        }
    }

    /// Create the UI using egui.
    pub(crate) fn ui(&mut self, ctx: &Context, paused: bool, registers: Option<&crate::cpu::registers::Registers>, gb: Option<&crate::gb::GB>) -> (Option<GuiAction>, bool) {
        let mut action = None;
        let mut any_menu_open = false;
        
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
                        let mut dialog = rfd::FileDialog::new()
                            .add_filter("Game Boy ROM", &["gb", "gbc"])
                            .add_filter("All Files", &["*"]);
                        if env::current_dir().is_ok() {
                            dialog = dialog.set_directory(env::current_dir().unwrap());
                        }
                        if let Some(path) = dialog.pick_file() {
                            *action = Some(GuiAction::LoadRom(path));
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Save State").clicked() {
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let file_name = format!("save_{}", timestamp);
                        let mut dialog = rfd::FileDialog::new()
                            .add_filter("RustyBoi Save State", &["rustyboisave"])
                            .set_file_name(file_name);
                        if env::current_dir().is_ok() {
                            dialog = dialog.set_directory(env::current_dir().unwrap());
                        }
                        if let Some(path) = dialog.save_file() {
                            *action = Some(GuiAction::SaveState(path));
                        }
                        ui.close_menu();
                    }
                    if ui.button("Load State").clicked() {
                        let mut dialog = rfd::FileDialog::new()
                            .add_filter("RustyBoi Save State", &["rustyboisave"])
                            .add_filter("All Files", &["*"]);
                        if env::current_dir().is_ok() {
                            dialog = dialog.set_directory(env::current_dir().unwrap());
                        }
                        if let Some(path) = dialog.pick_file() {
                            *action = Some(GuiAction::LoadState(path));
                        }
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
                });
            });
        });
    }

    fn render_debug_panels(&mut self, ctx: &Context, registers: Option<&crate::cpu::registers::Registers>, gb: Option<&crate::gb::GB>, action: &mut Option<GuiAction>, paused: bool) {
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

    pub(crate) fn set_error(&mut self, error_message: String) {
        self.error_message = Some(error_message);
    }

    pub(crate) fn clear_error(&mut self) {
        self.error_message = None;
    }

    pub(crate) fn set_status(&mut self, status_message: String) {
        self.status_message = Some(status_message);
    }
}

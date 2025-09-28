use egui::Context;
use crate::ui::Gui;

impl Gui {
    pub(in crate) fn render_keybind_settings_panel(&mut self, ctx: &Context) {
        egui::Window::new("Keybind Settings")
            .default_pos([300.0, 50.0])
            .default_size([300.0, 400.0])
            .collapsible(true)
            .resizable(true)
            .frame(egui::Frame::window(&ctx.style()).fill(crate::ui::PANEL_BACKGROUND))
            .show(ctx, |ui| {
                ui.heading("Game Boy Controls");
                ui.separator();
                
                ui.label("Configure the keyboard mappings for Game Boy controls:");
                ui.add_space(10.0);
                
                // For now, we'll display the current keybinds as read-only
                // In a full implementation, these would be editable
                ui.group(|ui| {
                    ui.label("Current Keybinds:");
                    ui.monospace("A Button: Z");
                    ui.monospace("B Button: X");
                    ui.monospace("Start: Enter");
                    ui.monospace("Select: Space");
                    ui.monospace("Up: Arrow Up");
                    ui.monospace("Down: Arrow Down");
                    ui.monospace("Left: Arrow Left");
                    ui.monospace("Right: Arrow Right");
                });
                
                ui.add_space(10.0);
                
                ui.group(|ui| {
                    ui.label("⚠️ Note:");
                    ui.label("Keybind customization is not yet fully implemented.");
                    ui.label("This panel shows the current default mappings.");
                    ui.label("Future versions will allow live editing of these bindings.");
                });
                
                ui.add_space(10.0);
                
                if ui.button("Reset to Defaults").clicked() {
                    // This would reset keybinds to defaults
                    // For now, it's just a placeholder
                }
            });
    }
}

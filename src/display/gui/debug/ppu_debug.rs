use egui::Context;
use super::super::main_ui::Gui;

impl Gui {
    pub(in crate::display::gui) fn render_ppu_debug_panel(&mut self, ctx: &Context, gb: Option<&crate::gb::GB>) {
        if let Some(gb_ref) = gb {
            let (ppu, pixel_buffer) = gb_ref.get_ppu_debug_info();
            egui::Window::new("PPU Debug")
                .default_pos([640.0, 50.0])
                .default_size([250.0, 400.0])
                .collapsible(true)
                .resizable(false)
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgba_unmultiplied(64, 64, 64, 220)))
                .show(ctx, |ui| {
                    ui.set_width(230.0);
                    
                    // PPU Status
                    ui.monospace(egui::RichText::new(format!("Disabled: {}", if ppu.is_disabled() { "YES" } else { "NO" }))
                        .color(if ppu.is_disabled() { egui::Color32::LIGHT_RED } else { egui::Color32::LIGHT_GREEN }));
                    
                    let state_str = match ppu.get_state() {
                        crate::ppu::ppu::State::OAMSearch => "OAM Search",
                        crate::ppu::ppu::State::PixelTransfer => "Pixel Transfer", 
                        crate::ppu::ppu::State::HBlank => "H-Blank",
                        crate::ppu::ppu::State::VBlank => "V-Blank",
                    };
                    ui.monospace(egui::RichText::new(format!("State: {}", state_str)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("Ticks: {}", ppu.get_ticks())).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("Current X: {}", ppu.get_x())).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("Has Frame: {}", if ppu.has_frame() { "YES" } else { "NO" }))
                        .color(if ppu.has_frame() { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));
                    
                    ui.separator();
                    
                    // MMIO Registers
                    let ly = gb_ref.read_memory(crate::ppu::ppu::LY);
                    let scy = gb_ref.read_memory(crate::ppu::ppu::SCY);
                    let bgp = gb_ref.read_memory(crate::ppu::ppu::BGP);
                    let lyc = gb_ref.read_memory(crate::ppu::ppu::LYC);
                    let lcd_control = gb_ref.read_memory(crate::ppu::ppu::LCD_CONTROL);
                    let lcd_status = gb_ref.read_memory(crate::ppu::ppu::LCD_STATUS);

                    ui.monospace(egui::RichText::new(format!("LY: {:02X} ({})", ly, ly)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("LYC: {:02X} ({})", lyc, lyc)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("SCY: {:02X} ({})", scy, scy)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("BGP: {:02X}", bgp)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("LCD_CTRL: {:02X}", lcd_control)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("LCD_STAT: {:02X}", lcd_status)).color(egui::Color32::WHITE));
                    
                    ui.separator();
                    
                    // Pixel Fetcher Buffer
                    ui.small(egui::RichText::new("Fetcher Pixel Buffer:").color(egui::Color32::LIGHT_GRAY));
                    ui.horizontal(|ui| {
                        for (i, &pixel) in pixel_buffer.iter().enumerate() {
                            let color = match pixel {
                                0 => egui::Color32::WHITE,
                                1 => egui::Color32::LIGHT_GRAY,
                                2 => egui::Color32::DARK_GRAY,
                                3 => egui::Color32::BLACK,
                                _ => egui::Color32::RED, // Invalid value
                            };
                            ui.small(egui::RichText::new(format!("{}", pixel)).color(color));
                            if i < 7 { ui.small("|"); }
                        }
                    });
                    
                    ui.separator();
                    ui.small(egui::RichText::new("PPU Debug Information").color(egui::Color32::LIGHT_GRAY));
                });
        }
    }
}

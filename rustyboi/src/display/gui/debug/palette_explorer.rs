use egui::Context;
use super::super::main_ui::Gui;

impl Gui {
    pub(in crate::display::gui) fn render_palette_explorer_panel(&mut self, ctx: &Context, gb: Option<&crate::gb::GB>) {
        if let Some(gb_ref) = gb {
            egui::Window::new("Palette Explorer")
                .default_pos([900.0, 50.0])
                .default_size([250.0, 500.0])
                .collapsible(true)
                .resizable(true)
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgba_unmultiplied(64, 64, 64, 220)))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.set_width(180.0);
                        
                        // Background Palette (BGP)
                        ui.heading("Background Palette (BGP)");
                        let bgp = gb_ref.read_memory(crate::ppu::BGP);
                        ui.monospace(egui::RichText::new(format!("BGP: {:02X}", bgp)).color(egui::Color32::YELLOW));
                    
                    ui.separator();
                    
                    // Show each palette entry with color representation
                    for i in 0..4 {
                        let palette_bits = (bgp >> (i * 2)) & 0x03;
                        let color_name = match palette_bits {
                            0 => "White",
                            1 => "Light Gray", 
                            2 => "Dark Gray",
                            3 => "Black",
                            _ => "Invalid",
                        };
                        
                        // Convert to actual RGB colors for display
                        let display_color = match palette_bits {
                            0 => egui::Color32::from_rgb(255, 255, 255), // White
                            1 => egui::Color32::from_rgb(170, 170, 170), // Light Gray
                            2 => egui::Color32::from_rgb(85, 85, 85),    // Dark Gray  
                            3 => egui::Color32::from_rgb(0, 0, 0),       // Black
                            _ => egui::Color32::RED,
                        };
                        
                        ui.horizontal(|ui| {
                            // Color swatch
                            let (rect, _) = ui.allocate_exact_size(
                                egui::Vec2::new(20.0, 16.0), 
                                egui::Sense::hover()
                            );
                            ui.painter().rect_filled(rect, 2.0, display_color);
                            ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::WHITE));
                            
                            // Palette info
                            ui.monospace(egui::RichText::new(format!("P{}: {} ({:02b})", i, color_name, palette_bits))
                                .color(egui::Color32::WHITE));
                        });
                    }
                    
                    ui.separator();

                    // Sprite Palettes (OBP0 and OBP1)
                    ui.heading("Sprite Palettes");
                    let obp0 = gb_ref.read_memory(crate::ppu::OBP0);
                    let obp1 = gb_ref.read_memory(crate::ppu::OBP1);
                    
                    // OBP0 Palette
                    ui.monospace(egui::RichText::new(format!("OBP0: {:02X}", obp0)).color(egui::Color32::LIGHT_BLUE));
                    for i in 0..4 {
                        let palette_bits = (obp0 >> (i * 2)) & 0x03;
                        let color_name = match palette_bits {
                            0 => if i == 0 { "Transparent" } else { "White" },
                            1 => "Light Gray", 
                            2 => "Dark Gray",
                            3 => "Black",
                            _ => "Invalid",
                        };
                        
                        let display_color = if i == 0 {
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 100) // Transparent
                        } else {
                            match palette_bits {
                                0 => egui::Color32::from_rgb(255, 255, 255), // White
                                1 => egui::Color32::from_rgb(170, 170, 170), // Light Gray
                                2 => egui::Color32::from_rgb(85, 85, 85),    // Dark Gray  
                                3 => egui::Color32::from_rgb(0, 0, 0),       // Black
                                _ => egui::Color32::RED,
                            }
                        };
                        
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::Vec2::new(20.0, 16.0), 
                                egui::Sense::hover()
                            );
                            ui.painter().rect_filled(rect, 2.0, display_color);
                            ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::WHITE));
                            
                            ui.monospace(egui::RichText::new(format!("P{}: {} ({:02b})", i, color_name, palette_bits))
                                .color(egui::Color32::WHITE));
                        });
                    }
                    
                    ui.separator();
                    
                    // OBP1 Palette
                    ui.monospace(egui::RichText::new(format!("OBP1: {:02X}", obp1)).color(egui::Color32::LIGHT_BLUE));
                    for i in 0..4 {
                        let palette_bits = (obp1 >> (i * 2)) & 0x03;
                        let color_name = match palette_bits {
                            0 => if i == 0 { "Transparent" } else { "White" },
                            1 => "Light Gray", 
                            2 => "Dark Gray",
                            3 => "Black",
                            _ => "Invalid",
                        };
                        
                        let display_color = if i == 0 {
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 100) // Transparent
                        } else {
                            match palette_bits {
                                0 => egui::Color32::from_rgb(255, 255, 255), // White
                                1 => egui::Color32::from_rgb(170, 170, 170), // Light Gray
                                2 => egui::Color32::from_rgb(85, 85, 85),    // Dark Gray  
                                3 => egui::Color32::from_rgb(0, 0, 0),       // Black
                                _ => egui::Color32::RED,
                            }
                        };
                        
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::Vec2::new(20.0, 16.0), 
                                egui::Sense::hover()
                            );
                            ui.painter().rect_filled(rect, 2.0, display_color);
                            ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::WHITE));
                            
                            ui.monospace(egui::RichText::new(format!("P{}: {} ({:02b})", i, color_name, palette_bits))
                                .color(egui::Color32::WHITE));
                        });
                    }
                    
                    ui.separator();
                    ui.small(egui::RichText::new("Note: For sprites, P0 is always transparent").color(egui::Color32::LIGHT_GRAY));
                    });
                });
        }
    }
}

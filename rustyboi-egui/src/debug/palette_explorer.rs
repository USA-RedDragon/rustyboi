use egui::Context;
use rustyboi_session::DebugSnapshot;
use crate::ui::Gui;

impl Gui {
    pub(in crate) fn render_palette_explorer_panel(&mut self, ctx: &Context, debug: Option<&DebugSnapshot>) {
        if let Some(snap) = debug {
            egui::Window::new("Palette Explorer")
                .default_pos([900.0, 50.0])
                .default_size([250.0, 500.0])
                .collapsible(true)
                .resizable(true)
                .frame(egui::Frame::window(&ctx.style()).fill(crate::ui::PANEL_BACKGROUND))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.set_width(200.0);

                        // Show different palettes based on hardware type
                        if snap.cgb {
                            self.render_cgb_palettes(ui, snap);
                        } else {
                            self.render_dmg_palettes(ui, snap);
                        }
                    });
                });
        }
    }

    fn render_dmg_palettes(&self, ui: &mut egui::Ui, snap: &DebugSnapshot) {
        // Background Palette (BGP)
        ui.heading("Background Palette (BGP)");
        let bgp = snap.mmio.bgp;
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
        let obp0 = snap.mmio.obp0;
        let obp1 = snap.mmio.obp1;

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
    }

    fn render_cgb_palettes(&self, ui: &mut egui::Ui, snap: &DebugSnapshot) {
        // CGB Background Palettes
        ui.heading("CGB Background Palettes");

        // Show current palette spec register
        let bcps = snap.mmio.bcps;
        ui.monospace(egui::RichText::new(format!("BCPS: {:02X} (Auto-inc: {}, Addr: {:02X})",
            bcps,
            if bcps & 0x80 != 0 { "On" } else { "Off" },
            bcps & 0x3F
        )).color(egui::Color32::YELLOW));

        ui.separator();

        // Show all 8 background palettes
        for palette in 0..8 {
            ui.collapsing(format!("BG Palette {}", palette), |ui| {
                for color in 0..4 {
                    // Get RGB555 color from the captured palette table
                    let rgb555 = snap.cgb_bg_rgb555(palette, color).unwrap_or(0);
                    let (r, g, b) = snap.cgb_bg_rgb(palette, color).unwrap_or((0, 0, 0));

                    ui.horizontal(|ui| {
                        // Color swatch
                        let (rect, _) = ui.allocate_exact_size(
                            egui::Vec2::new(24.0, 18.0),
                            egui::Sense::hover()
                        );
                        ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(r, g, b));
                        ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::WHITE));

                        // Color info
                        ui.monospace(egui::RichText::new(format!("C{}: RGB({:02X},{:02X},{:02X}) ${:04X}",
                            color, r, g, b, rgb555))
                            .color(egui::Color32::WHITE));
                    });
                }
            });
        }

        ui.separator();

        // CGB Object Palettes
        ui.heading("CGB Object Palettes");

        // Show current palette spec register
        let ocps = snap.mmio.ocps;
        ui.monospace(egui::RichText::new(format!("OCPS: {:02X} (Auto-inc: {}, Addr: {:02X})",
            ocps,
            if ocps & 0x80 != 0 { "On" } else { "Off" },
            ocps & 0x3F
        )).color(egui::Color32::LIGHT_BLUE));

        ui.separator();

        // Show all 8 object palettes
        for palette in 0..8 {
            ui.collapsing(format!("OBJ Palette {}", palette), |ui| {
                for color in 0..4 {
                    // Get RGB555 color from the captured palette table
                    let rgb555 = snap.cgb_obj_rgb555(palette, color).unwrap_or(0);
                    let (r, g, b) = snap.cgb_obj_rgb(palette, color).unwrap_or((0, 0, 0));

                    let color_name = if color == 0 { " (Transparent)" } else { "" };

                    ui.horizontal(|ui| {
                        // Color swatch (show transparency for color 0)
                        let (rect, _) = ui.allocate_exact_size(
                            egui::Vec2::new(24.0, 18.0),
                            egui::Sense::hover()
                        );

                        if color == 0 {
                            // Show checkerboard pattern for transparency
                            ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(200, 200, 200));
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(rect.min + egui::Vec2::new(0.0, 0.0), egui::Vec2::new(12.0, 9.0)),
                                0.0, egui::Color32::from_rgb(150, 150, 150)
                            );
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(rect.min + egui::Vec2::new(12.0, 9.0), egui::Vec2::new(12.0, 9.0)),
                                0.0, egui::Color32::from_rgb(150, 150, 150)
                            );
                        } else {
                            ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(r, g, b));
                        }
                        ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0, egui::Color32::WHITE));

                        // Color info
                        ui.monospace(egui::RichText::new(format!("C{}: RGB({:02X},{:02X},{:02X}) ${:04X}{}",
                            color, r, g, b, rgb555, color_name))
                            .color(egui::Color32::WHITE));
                    });
                }
            });
        }

        ui.separator();

        // CGB-specific register info
        ui.heading("CGB Registers");
        let vbk = snap.mmio.vbk;
        let svbk = snap.mmio.svbk;
        let key1 = snap.mmio.key1;

        ui.monospace(egui::RichText::new(format!("VBK: {:02X} (VRAM Bank: {})", vbk, vbk & 1))
            .color(egui::Color32::LIGHT_GREEN));
        ui.monospace(egui::RichText::new(format!("SVBK: {:02X} (WRAM Bank: {})", svbk, if svbk & 7 == 0 { 1 } else { svbk & 7 }))
            .color(egui::Color32::LIGHT_GREEN));
        ui.monospace(egui::RichText::new(format!("KEY1: {:02X} (Speed: {}x, Prepare: {})",
            key1,
            if key1 & 0x80 != 0 { "2" } else { "1" },
            if key1 & 0x01 != 0 { "Yes" } else { "No" }
        )).color(egui::Color32::LIGHT_GREEN));

        ui.separator();
        ui.small(egui::RichText::new("Note: Object color 0 is always transparent").color(egui::Color32::LIGHT_GRAY));
    }
}

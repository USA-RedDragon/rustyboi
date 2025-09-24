use egui::Context;
use super::super::main_ui::Gui;

impl Gui {
    pub(in crate::display::gui) fn render_tile_explorer_panel(&mut self, ctx: &Context, gb: Option<&crate::gb::GB>) {
        if let Some(gb_ref) = gb {
            egui::Window::new("Tile Explorer")
                .default_pos([1120.0, 50.0])
                .default_size([350.0, 500.0])
                .collapsible(true)
                .resizable(true)
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgba_unmultiplied(64, 64, 64, 220)))
                .show(ctx, |ui| {
                    ui.set_min_width(320.0);
                    
                    // Get current palette for color mapping
                    let bgp = gb_ref.read_memory(crate::ppu::BGP);
                    
                    ui.monospace(egui::RichText::new("VRAM Tile Data").color(egui::Color32::YELLOW));
                    ui.small(egui::RichText::new("8x8 pixel tiles from 0x8000-0x97FF").color(egui::Color32::LIGHT_GRAY));
                    
                    ui.separator();
                    
                    // Display tiles in a grid (16 tiles per row)
                    let tiles_per_row = 16;
                    let tile_size = 20.0; // Each tile displayed as 20x20 pixels
                    let total_tiles = 384; // Total tiles in VRAM (0x8000-0x97FF = 6KB / 16 bytes per tile)
                    
                    egui::ScrollArea::vertical()
                        .show(ui, |ui| {
                            for tile_row in 0..(total_tiles / tiles_per_row) {
                                ui.horizontal(|ui| {
                                    for tile_col in 0..tiles_per_row {
                                        let tile_index = tile_row * tiles_per_row + tile_col;
                                        if tile_index >= total_tiles { break; }
                                        
                                        // Calculate VRAM address for this tile (each tile is 16 bytes)
                                        let tile_addr = 0x8000u16 + (tile_index as u16 * 16);
                                        
                                        // Allocate space for the tile
                                        let (tile_rect, response) = ui.allocate_exact_size(
                                            egui::Vec2::new(tile_size, tile_size),
                                            egui::Sense::hover()
                                        );
                                        
                                        // Draw the tile
                                        for y in 0..8 {
                                            // Read the two bytes for this line of the tile
                                            let low_byte = gb_ref.read_memory(tile_addr + (y * 2));
                                            let high_byte = gb_ref.read_memory(tile_addr + (y * 2) + 1);
                                            
                                            for x in 0..8 {
                                                // Extract pixel value (2 bits)
                                                let bit = 7 - x; // Pixels are stored MSB first
                                                let low_bit = (low_byte >> bit) & 1;
                                                let high_bit = (high_byte >> bit) & 1;
                                                let pixel_value = (high_bit << 1) | low_bit;
                                                
                                                // Apply palette mapping
                                                let palette_bits = (bgp >> (pixel_value * 2)) & 0x03;
                                                let pixel_color = match palette_bits {
                                                    0 => egui::Color32::from_rgb(255, 255, 255), // White
                                                    1 => egui::Color32::from_rgb(170, 170, 170), // Light Gray
                                                    2 => egui::Color32::from_rgb(85, 85, 85),    // Dark Gray
                                                    3 => egui::Color32::from_rgb(0, 0, 0),       // Black
                                                    _ => egui::Color32::RED, // Should never happen
                                                };
                                                
                                                // Calculate pixel position within the tile
                                                let pixel_size = tile_size / 8.0;
                                                let pixel_x = tile_rect.min.x + (x as f32 * pixel_size);
                                                let pixel_y = tile_rect.min.y + (y as f32 * pixel_size);
                                                let pixel_rect = egui::Rect::from_min_size(
                                                    egui::Pos2::new(pixel_x, pixel_y),
                                                    egui::Vec2::new(pixel_size, pixel_size)
                                                );
                                                
                                                ui.painter().rect_filled(pixel_rect, 0.0, pixel_color);
                                            }
                                        }
                                        
                                        // Draw tile border
                                        ui.painter().rect_stroke(tile_rect, 0.0, egui::Stroke::new(0.5, egui::Color32::GRAY));
                                        
                                        // Show tooltip with tile info on hover
                                        if response.hovered() {
                                            response.on_hover_text(format!(
                                                "Tile #{}\nVRAM: 0x{:04X}-0x{:04X}",
                                                tile_index,
                                                tile_addr,
                                                tile_addr + 15
                                            ));
                                        }
                                        
                                        ui.add_space(2.0); // Small gap between tiles
                                    }
                                });
                                ui.add_space(2.0); // Small gap between rows
                            }
                        });
                    
                    ui.separator();
                    ui.small(egui::RichText::new("Hover tiles for details").color(egui::Color32::LIGHT_GRAY));
                    ui.small(egui::RichText::new("Uses current BGP palette").color(egui::Color32::LIGHT_GRAY));
                });
        }
    }
}

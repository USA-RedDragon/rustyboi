use egui::Context;
use super::super::main_ui::Gui;

impl Gui {
    pub(in crate::display::gui) fn render_sprite_debug_panel(&mut self, ctx: &Context, gb: Option<&crate::gb::GB>) {
        if let Some(gb_ref) = gb {
            egui::Window::new("Sprite Debug")
                .default_pos([900.0, 50.0])
                .default_size([400.0, 600.0])
                .collapsible(true)
                .resizable(true)
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgba_unmultiplied(64, 64, 64, 220)))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        // Current scanline sprites
                        let (ppu, _) = gb_ref.get_ppu_debug_info();
                        let ly = gb_ref.read_memory(crate::ppu::LY);
                        let sprites_count = ppu.get_sprites_on_line_count();
                        
                        ui.heading("Current Scanline Sprites");
                        ui.monospace(egui::RichText::new(format!("Line {}: {} sprites found", ly, sprites_count))
                            .color(if sprites_count > 0 { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));
                        
                        if sprites_count > 0 {
                            ui.separator();
                            ui.small("Sprites displayed in priority order (left to right):");
                            // Note: We'll need to add a method to get the actual sprite data
                            ui.small("(Sprite data display would require additional PPU methods)");
                        }
                        
                        ui.separator();
                        
                        // OAM Table
                        ui.heading("OAM Table (40 sprites)");
                        ui.small("Format: [Y] [X] [Tile] [Attr] - Status");
                        
                        egui::Grid::new("oam_grid")
                            .striped(true)
                            .spacing([4.0, 2.0])
                            .show(ui, |ui| {
                                // Header
                                ui.strong("Spr");
                                ui.strong("Y");
                                ui.strong("X");
                                ui.strong("Tile");
                                ui.strong("Attr");
                                ui.strong("Status");
                                ui.strong("Preview");
                                ui.end_row();
                                
                                for sprite_index in 0..40 {
                                    let oam_base = 0xFE00 + (sprite_index * 4);
                                    let sprite_y = gb_ref.read_memory(oam_base);
                                    let sprite_x = gb_ref.read_memory(oam_base + 1);
                                    let tile_index = gb_ref.read_memory(oam_base + 2);
                                    let attributes = gb_ref.read_memory(oam_base + 3);
                                    
                                    // Calculate screen position
                                    let screen_y = sprite_y.wrapping_sub(16);
                                    
                                    // Determine if sprite is visible on current line
                                    let lcd_control = gb_ref.read_memory(crate::ppu::LCD_CONTROL);
                                    let sprite_height = if (lcd_control & 0x04) != 0 { 16 } else { 8 };
                                    let on_current_line = ly >= screen_y && ly < screen_y + sprite_height;
                                    
                                    // Determine visibility
                                    let visible = sprite_y != 0 && sprite_y < 160 && sprite_x != 0 && sprite_x < 168;
                                    
                                    let row_color = if on_current_line {
                                        egui::Color32::LIGHT_GREEN
                                    } else if visible {
                                        egui::Color32::WHITE
                                    } else {
                                        egui::Color32::GRAY
                                    };
                                    
                                    // Check if this sprite is selected
                                    let is_selected = self.selected_sprite_index == Some(sprite_index as u8);
                                    let button_color = if is_selected {
                                        egui::Color32::YELLOW
                                    } else {
                                        row_color
                                    };
                                    
                                    // Make sprite index clickable
                                    if ui.add(egui::Button::new(egui::RichText::new(format!("{:02}", sprite_index)).color(button_color)).small()).clicked() {
                                        self.selected_sprite_index = Some(sprite_index as u8);
                                    }
                                    
                                    ui.monospace(egui::RichText::new(format!("{:02X}", sprite_y)).color(row_color));
                                    ui.monospace(egui::RichText::new(format!("{:02X}", sprite_x)).color(row_color));
                                    ui.monospace(egui::RichText::new(format!("{:02X}", tile_index)).color(row_color));
                                    ui.monospace(egui::RichText::new(format!("{:02X}", attributes)).color(row_color));
                                    
                                    // Status
                                    let status = if on_current_line {
                                        "ON LINE"
                                    } else if visible {
                                        "VISIBLE"
                                    } else {
                                        "HIDDEN"
                                    };
                                    ui.small(egui::RichText::new(status).color(row_color));
                                    
                                    // Render sprite preview
                                    self.render_sprite_preview(ui, gb_ref, tile_index, attributes, sprite_height);
                                    
                                    ui.end_row();
                                }
                            });
                        
                        ui.separator();
                        
                        // Sprite attribute decoder
                        ui.heading("Attribute Decoder");
                        
                        if let Some(selected_index) = self.selected_sprite_index {
                            ui.small(format!("Selected Sprite: {}", selected_index));
                            ui.separator();
                            
                            // Get the selected sprite's data
                            let oam_base = 0xFE00 + (selected_index as u16 * 4);
                            let sprite_y = gb_ref.read_memory(oam_base);
                            let sprite_x = gb_ref.read_memory(oam_base + 1);
                            let tile_index = gb_ref.read_memory(oam_base + 2);
                            let attributes = gb_ref.read_memory(oam_base + 3);
                            
                            // Calculate screen position
                            let screen_y = sprite_y.wrapping_sub(16);
                            let screen_x = sprite_x.wrapping_sub(8);
                            
                            // Basic sprite info
                            ui.monospace(format!("OAM Position: Y={:02X} ({}) X={:02X} ({})", sprite_y, sprite_y, sprite_x, sprite_x));
                            ui.monospace(format!("Screen Position: Y={} X={}", screen_y, screen_x));
                            ui.monospace(format!("Tile Index: {:02X} ({})", tile_index, tile_index));
                            ui.monospace(format!("Attributes: {:02X} ({:08b})", attributes, attributes));
                            
                            ui.separator();
                            
                            // Decode attribute bits
                            let priority = (attributes & 0x80) != 0;
                            let y_flip = (attributes & 0x40) != 0;
                            let x_flip = (attributes & 0x20) != 0;
                            let palette = (attributes & 0x10) != 0;
                            
                            ui.heading("Attribute Details:");
                            ui.monospace(egui::RichText::new(format!("Bit 7 - Priority: {} ({})", 
                                if priority { "1" } else { "0" },
                                if priority { "Behind BG colors 1-3" } else { "Above BG" }
                            )).color(if priority { egui::Color32::LIGHT_RED } else { egui::Color32::LIGHT_GREEN }));
                            
                            ui.monospace(egui::RichText::new(format!("Bit 6 - Y-Flip: {} ({})", 
                                if y_flip { "1" } else { "0" },
                                if y_flip { "Vertically mirrored" } else { "Normal" }
                            )).color(if y_flip { egui::Color32::YELLOW } else { egui::Color32::WHITE }));
                            
                            ui.monospace(egui::RichText::new(format!("Bit 5 - X-Flip: {} ({})", 
                                if x_flip { "1" } else { "0" },
                                if x_flip { "Horizontally mirrored" } else { "Normal" }
                            )).color(if x_flip { egui::Color32::YELLOW } else { egui::Color32::WHITE }));
                            
            ui.monospace(egui::RichText::new(format!("Bit 4 - Palette: {} ({})", 
                if palette { "1" } else { "0" },
                if palette { "OBP1" } else { "OBP0" }
            )).color(if palette { egui::Color32::LIGHT_BLUE } else { egui::Color32::from_rgb(0, 255, 255) }));                            ui.monospace(egui::RichText::new("Bits 3-0 - Unused").color(egui::Color32::GRAY));
                            
                            ui.separator();
                            
                            // Show visibility status
                            let lcd_control = gb_ref.read_memory(crate::ppu::LCD_CONTROL);
                            let sprite_height = if (lcd_control & 0x04) != 0 { 16 } else { 8 };
                            let ly = gb_ref.read_memory(crate::ppu::LY);
                            let on_current_line = ly >= screen_y && ly < screen_y + sprite_height;
                            let visible = sprite_y != 0 && sprite_y < 160 && sprite_x != 0 && sprite_x < 168;
                            
                            ui.heading("Visibility:");
                            ui.monospace(format!("Sprite Height: {}px", sprite_height));
                            ui.monospace(egui::RichText::new(format!("On Current Line ({}): {}", ly, on_current_line))
                                .color(if on_current_line { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));
                            ui.monospace(egui::RichText::new(format!("Generally Visible: {}", visible))
                                .color(if visible { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));
                                
                            if ui.button("Clear Selection").clicked() {
                                self.selected_sprite_index = None;
                            }
                        } else {
                            ui.small("Click on a sprite index above to see detailed attribute information");
                            ui.separator();
                            
                            // Show general format information
                            ui.monospace("Bit 7: Priority (0=Above BG, 1=Behind BG colors 1-3)");
                            ui.monospace("Bit 6: Y-Flip (0=Normal, 1=Vertically mirrored)");
                            ui.monospace("Bit 5: X-Flip (0=Normal, 1=Horizontally mirrored)"); 
                            ui.monospace("Bit 4: Palette (0=OBP0, 1=OBP1)");
                            ui.monospace("Bits 3-0: Unused (should be 0)");
                        }
                    });
                });
        }
    }

    // Helper method to render a small sprite preview
    fn render_sprite_preview(&self, ui: &mut egui::Ui, gb_ref: &crate::gb::GB, tile_index: u8, attributes: u8, sprite_height: u8) {
        let tile_size = 16.0; // Small preview size
        
        // Get sprite palettes
        let palette_bit = (attributes & 0x10) != 0;
        let palette_reg = if palette_bit { 
            gb_ref.read_memory(crate::ppu::OBP1)
        } else { 
            gb_ref.read_memory(crate::ppu::OBP0)
        };
        
        // Get flip flags
        let x_flip = (attributes & 0x20) != 0;
        let y_flip = (attributes & 0x40) != 0;
        
        // Allocate space for the sprite preview
        let (sprite_rect, response) = ui.allocate_exact_size(
            egui::Vec2::new(tile_size, tile_size),
            egui::Sense::hover()
        );
        
        // For 8x16 sprites, we only show the top tile for now
        let display_tile = if sprite_height == 16 {
            tile_index & 0xFE // Even tile (top half)
        } else {
            tile_index
        };
        
        // Calculate VRAM address for this tile (sprites always use $8000 method)
        let tile_addr = 0x8000u16 + (display_tile as u16 * 16);
        
        // Draw the sprite tile
        for y in 0..8 {
            // Read the two bytes for this line of the tile
            let actual_y = if y_flip { 7 - y } else { y };
            let low_byte = gb_ref.read_memory(tile_addr + (actual_y * 2));
            let high_byte = gb_ref.read_memory(tile_addr + (actual_y * 2) + 1);
            
            for x in 0..8 {
                // Extract pixel value (2 bits)
                let actual_x = if x_flip { x } else { 7 - x };
                let low_bit = (low_byte >> actual_x) & 1;
                let high_bit = (high_byte >> actual_x) & 1;
                let pixel_value = (high_bit << 1) | low_bit;
                
                // Apply sprite palette mapping (0 is transparent)
                let pixel_color = if pixel_value == 0 {
                    egui::Color32::TRANSPARENT
                } else {
                    let palette_bits = (palette_reg >> (pixel_value * 2)) & 0x03;
                    match palette_bits {
                        0 => egui::Color32::from_rgb(255, 255, 255), // White
                        1 => egui::Color32::from_rgb(170, 170, 170), // Light Gray
                        2 => egui::Color32::from_rgb(85, 85, 85),    // Dark Gray
                        3 => egui::Color32::from_rgb(0, 0, 0),       // Black
                        _ => egui::Color32::RED, // Should never happen
                    }
                };
                
                // Calculate pixel position within the sprite preview
                let pixel_size = tile_size / 8.0;
                let pixel_x = sprite_rect.min.x + (x as f32 * pixel_size);
                let pixel_y = sprite_rect.min.y + (y as f32 * pixel_size);
                let pixel_rect = egui::Rect::from_min_size(
                    egui::Pos2::new(pixel_x, pixel_y),
                    egui::Vec2::new(pixel_size, pixel_size)
                );
                
                // Only draw non-transparent pixels
                if pixel_value != 0 {
                    ui.painter().rect_filled(pixel_rect, 0.0, pixel_color);
                }
            }
        }
        
        // Draw sprite border (checkerboard background for transparency)
        for y in 0..8 {
            for x in 0..8 {
                let pixel_size = tile_size / 8.0;
                let pixel_x = sprite_rect.min.x + (x as f32 * pixel_size);
                let pixel_y = sprite_rect.min.y + (y as f32 * pixel_size);
                let pixel_rect = egui::Rect::from_min_size(
                    egui::Pos2::new(pixel_x, pixel_y),
                    egui::Vec2::new(pixel_size, pixel_size)
                );
                
                // Checkerboard pattern for transparency
                if (x + y) % 2 == 0 {
                    ui.painter().rect_filled(pixel_rect, 0.0, egui::Color32::from_rgb(240, 240, 240));
                } else {
                    ui.painter().rect_filled(pixel_rect, 0.0, egui::Color32::from_rgb(200, 200, 200));
                }
            }
        }
        
        // Re-draw the sprite on top of the checkerboard
        for y in 0..8 {
            let actual_y = if y_flip { 7 - y } else { y };
            let low_byte = gb_ref.read_memory(tile_addr + (actual_y * 2));
            let high_byte = gb_ref.read_memory(tile_addr + (actual_y * 2) + 1);
            
            for x in 0..8 {
                let actual_x = if x_flip { x } else { 7 - x };
                let low_bit = (low_byte >> actual_x) & 1;
                let high_bit = (high_byte >> actual_x) & 1;
                let pixel_value = (high_bit << 1) | low_bit;
                
                if pixel_value != 0 {
                    let palette_bits = (palette_reg >> (pixel_value * 2)) & 0x03;
                    let pixel_color = match palette_bits {
                        0 => egui::Color32::from_rgb(255, 255, 255), // White
                        1 => egui::Color32::from_rgb(170, 170, 170), // Light Gray
                        2 => egui::Color32::from_rgb(85, 85, 85),    // Dark Gray
                        3 => egui::Color32::from_rgb(0, 0, 0),       // Black
                        _ => egui::Color32::RED,
                    };
                    
                    let pixel_size = tile_size / 8.0;
                    let pixel_x = sprite_rect.min.x + (x as f32 * pixel_size);
                    let pixel_y = sprite_rect.min.y + (y as f32 * pixel_size);
                    let pixel_rect = egui::Rect::from_min_size(
                        egui::Pos2::new(pixel_x, pixel_y),
                        egui::Vec2::new(pixel_size, pixel_size)
                    );
                    
                    ui.painter().rect_filled(pixel_rect, 0.0, pixel_color);
                }
            }
        }
        
        // Draw border around the sprite
        ui.painter().rect_stroke(sprite_rect, 0.0, egui::Stroke::new(0.5, egui::Color32::GRAY));
        
        // Show tooltip with sprite info on hover
        if response.hovered() {
            let palette_name = if palette_bit { "OBP1" } else { "OBP0" };
            let flips = format!("{}{}",
                if x_flip { "X-Flip " } else { "" },
                if y_flip { "Y-Flip" } else { "" }
            );
            response.on_hover_text(format!(
                "Tile: 0x{:02X}\nPalette: {}\nFlips: {}\nVRAM: 0x{:04X}",
                display_tile, palette_name, 
                if flips.is_empty() { "None" } else { &flips },
                tile_addr
            ));
        }
    }
}

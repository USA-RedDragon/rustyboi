use egui::Context;
use rustyboi_session::DebugSnapshot;
use crate::ui::Gui;

/// Sprite-preview atlas: 40 sprites stacked as 8x8 cells in one column.
const SPRITE_ATLAS_W: usize = 8;
const SPRITE_ATLAS_H: usize = 40 * 8; // 320
/// On-screen preview size (matches the old 16px cells).
const PREVIEW_DISPLAY: f32 = 16.0;

impl Gui {
    pub(in crate) fn render_sprite_debug_panel(&mut self, ctx: &Context, debug: Option<&DebugSnapshot>) {
        if let Some(snap) = debug {
            egui::Window::new("Sprite Debug")
                .default_pos([900.0, 50.0])
                .default_size([400.0, 600.0])
                .collapsible(true)
                .resizable(true)
                .frame(egui::Frame::window(&ctx.style_of(ctx.theme())).fill(crate::ui::PANEL_BACKGROUND))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        // Current scanline sprites
                        let ly = snap.mmio.ly;
                        let sprites_count = snap.ppu.sprites_on_line;

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

                        // Bake all 40 sprite previews into one column atlas
                        // (8 x 320) and upload it once; each grid row then draws
                        // its 8x8 cell as a UV sub-image instead of ~192 rects.
                        let sprite_height =
                            if (snap.mmio.lcdc & 0x04) != 0 { 16u8 } else { 8 };
                        let atlas = self.build_sprite_atlas(snap, sprite_height);
                        let sprite_tex = self.sprite_atlas_tex.update(
                            ctx,
                            "sprite_atlas",
                            SPRITE_ATLAS_W,
                            SPRITE_ATLAS_H,
                            atlas,
                        );

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
                                    let sprite_y = snap.oam_byte(oam_base);
                                    let sprite_x = snap.oam_byte(oam_base + 1);
                                    let tile_index = snap.oam_byte(oam_base + 2);
                                    let attributes = snap.oam_byte(oam_base + 3);

                                    // Calculate screen position
                                    let screen_y = sprite_y.wrapping_sub(16);

                                    // Determine if sprite is visible on current line
                                    let lcd_control = snap.mmio.lcdc;
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

                                    // Render sprite preview: one UV sub-image
                                    // into the pre-baked atlas (16x16 on screen).
                                    draw_sprite_preview(
                                        ui,
                                        snap,
                                        sprite_tex,
                                        sprite_index as usize,
                                        tile_index,
                                        attributes,
                                        sprite_height,
                                    );

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
                            let sprite_y = snap.oam_byte(oam_base);
                            let sprite_x = snap.oam_byte(oam_base + 1);
                            let tile_index = snap.oam_byte(oam_base + 2);
                            let attributes = snap.oam_byte(oam_base + 3);

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
                            let dmg_palette = (attributes & 0x10) != 0;

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

                            // Show different attribute meanings based on hardware
                            if snap.cgb {
                                // CGB mode - different bit meanings
                                let vram_bank = (attributes & 0x08) != 0;
                                let cgb_palette = attributes & 0x07;

                                ui.monospace(egui::RichText::new(format!("Bit 4 - DMG Palette: {} ({})",
                                    if dmg_palette { "1" } else { "0" },
                                    if dmg_palette { "OBP1" } else { "OBP0" }
                                )).color(egui::Color32::GRAY));
                                ui.small(egui::RichText::new("  (DMG compatibility only)").color(egui::Color32::GRAY));

                                ui.monospace(egui::RichText::new(format!("Bit 3 - VRAM Bank: {} (Bank {})",
                                    if vram_bank { "1" } else { "0" },
                                    if vram_bank { "1" } else { "0" }
                                )).color(if vram_bank { egui::Color32::LIGHT_GREEN } else { egui::Color32::WHITE }));

                                ui.monospace(egui::RichText::new(format!("Bits 2-0 - CGB Palette: {:03b} (Palette {})",
                                    cgb_palette, cgb_palette
                                )).color(egui::Color32::LIGHT_BLUE));
                            } else {
                                // DMG mode - standard interpretation
                                ui.monospace(egui::RichText::new(format!("Bit 4 - Palette: {} ({})",
                                    if dmg_palette { "1" } else { "0" },
                                    if dmg_palette { "OBP1" } else { "OBP0" }
                                )).color(if dmg_palette { egui::Color32::LIGHT_BLUE } else { egui::Color32::from_rgb(0, 255, 255) }));

                                ui.monospace(egui::RichText::new("Bits 3-0 - Unused").color(egui::Color32::GRAY));
                            }

                            ui.separator();

                            // Show visibility status
                            let lcd_control = snap.mmio.lcdc;
                            let sprite_height = if (lcd_control & 0x04) != 0 { 16 } else { 8 };
                            let ly = snap.mmio.ly;
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

                            if snap.cgb {
                                ui.monospace("Bit 4: DMG Palette (0=OBP0, 1=OBP1) - compatibility only");
                                ui.monospace("Bit 3: VRAM Bank (0=Bank 0, 1=Bank 1)");
                                ui.monospace("Bits 2-0: CGB Palette (0-7)");
                            } else {
                                ui.monospace("Bit 4: Palette (0=OBP0, 1=OBP1)");
                                ui.monospace("Bits 3-0: Unused (should be 0)");
                            }
                        }
                    });
                });
        }
    }

    /// Bake all 40 sprite previews into one column atlas (8 x 320): each
    /// sprite is an 8x8 cell stacked vertically, checkerboard behind the
    /// transparent (colour 0) pixels. Uploaded once; each grid row draws its
    /// cell as a UV sub-image.
    fn build_sprite_atlas(&self, snap: &DebugSnapshot, sprite_height: u8) -> Vec<egui::Color32> {
        let light = egui::Color32::from_rgb(240, 240, 240);
        let dark = egui::Color32::from_rgb(200, 200, 200);
        let mut pixels = vec![light; SPRITE_ATLAS_W * SPRITE_ATLAS_H];
        for sprite_index in 0..40usize {
            let oam_base = 0xFE00 + (sprite_index as u16 * 4);
            let tile_index = snap.oam_byte(oam_base + 2);
            let attributes = snap.oam_byte(oam_base + 3);
            let x_flip = (attributes & 0x20) != 0;
            let y_flip = (attributes & 0x40) != 0;
            // 8x16 sprites: show the top tile (even index).
            let display_tile = if sprite_height == 16 { tile_index & 0xFE } else { tile_index };
            let vram_bank = if snap.cgb && (attributes & 0x08) != 0 { 1 } else { 0 };
            let tile_addr = 0x8000u16 + (display_tile as u16 * 16);

            for y in 0..8usize {
                let actual_y = if y_flip { 7 - y } else { y } as u16;
                let low_byte = snap.vram_byte(vram_bank, tile_addr + actual_y * 2);
                let high_byte = snap.vram_byte(vram_bank, tile_addr + actual_y * 2 + 1);
                let px_y = sprite_index * 8 + y;
                for x in 0..8usize {
                    let actual_x = if x_flip { x } else { 7 - x };
                    let low_bit = (low_byte >> actual_x) & 1;
                    let high_bit = (high_byte >> actual_x) & 1;
                    let pixel_value = (high_bit << 1) | low_bit;
                    // Colour 0 is transparent → show the checkerboard base.
                    let color = if pixel_value == 0 {
                        if (x + y) % 2 == 0 { light } else { dark }
                    } else {
                        self.get_sprite_pixel_color(snap, attributes, pixel_value)
                    };
                    pixels[px_y * SPRITE_ATLAS_W + x] = color;
                }
            }
        }
        pixels
    }


    fn get_sprite_pixel_color(&self, snap: &DebugSnapshot, attributes: u8, pixel_value: u8) -> egui::Color32 {
        if snap.cgb {
            // CGB mode - use CGB palette
            let cgb_palette = attributes & 0x07;
            let (r, g, b) = snap
                .cgb_obj_rgb(cgb_palette, pixel_value)
                .unwrap_or((0, 0, 0));
            egui::Color32::from_rgb(r, g, b)
        } else {
            // DMG mode - use monochrome palette
            let palette_bit = (attributes & 0x10) != 0;
            let palette_reg = if palette_bit {
                snap.mmio.obp1
            } else {
                snap.mmio.obp0
            };

            let palette_bits = (palette_reg >> (pixel_value * 2)) & 0x03;
            match palette_bits {
                0 => egui::Color32::from_rgb(255, 255, 255), // White
                1 => egui::Color32::from_rgb(170, 170, 170), // Light Gray
                2 => egui::Color32::from_rgb(85, 85, 85),    // Dark Gray
                3 => egui::Color32::from_rgb(0, 0, 0),       // Black
                _ => egui::Color32::RED, // Should never happen
            }
        }
    }
}

/// Draw one sprite's preview cell from the pre-baked atlas at `sprite_tex`, as
/// a 16x16 nearest-filtered UV sub-image, with the same hover tooltip the old
/// per-pixel preview showed.
#[allow(clippy::too_many_arguments)]
fn draw_sprite_preview(
    ui: &mut egui::Ui,
    snap: &DebugSnapshot,
    sprite_tex: egui::TextureId,
    sprite_index: usize,
    tile_index: u8,
    attributes: u8,
    sprite_height: u8,
) {
    let v0 = sprite_index as f32 / 40.0;
    let v1 = (sprite_index + 1) as f32 / 40.0;
    let image = egui::Image::new(egui::load::SizedTexture::new(
        sprite_tex,
        egui::vec2(PREVIEW_DISPLAY, PREVIEW_DISPLAY),
    ))
    .uv(egui::Rect::from_min_max(egui::pos2(0.0, v0), egui::pos2(1.0, v1)))
    .texture_options(egui::TextureOptions::NEAREST)
    .sense(egui::Sense::hover());
    let resp = ui.add(image);

    if resp.hovered() {
        let display_tile = if sprite_height == 16 { tile_index & 0xFE } else { tile_index };
        let tile_addr = 0x8000u16 + (display_tile as u16 * 16);
        let x_flip = (attributes & 0x20) != 0;
        let y_flip = (attributes & 0x40) != 0;
        let (palette_info, vram_info) = if snap.cgb {
            let bank = if (attributes & 0x08) != 0 { 1 } else { 0 };
            (format!("CGB Pal {}", attributes & 0x07), format!(" Bank {}", bank))
        } else {
            let name = if (attributes & 0x10) != 0 { "OBP1" } else { "OBP0" };
            (name.to_string(), String::new())
        };
        let flips = format!(
            "{}{}",
            if x_flip { "X-Flip " } else { "" },
            if y_flip { "Y-Flip" } else { "" }
        );
        resp.on_hover_text(format!(
            "Tile: 0x{:02X}\nPalette: {}\nFlips: {}\nVRAM: 0x{:04X}{}",
            display_tile,
            palette_info,
            if flips.is_empty() { "None" } else { &flips },
            tile_addr,
            vram_info
        ));
    }
}

use egui::Context;
use rustyboi_session::DebugSnapshot;
use crate::ui::Gui;

/// Tiles per atlas row / column, and the native pixel size of the tile atlas.
const TILES_PER_ROW: usize = 16;
const TOTAL_TILES: usize = 384; // 0x8000-0x97FF = 6 KB / 16 bytes per tile
const TILE_ROWS: usize = TOTAL_TILES / TILES_PER_ROW; // 24
const ATLAS_W: usize = TILES_PER_ROW * 8; // 128
const ATLAS_H: usize = TILE_ROWS * 8; // 192
/// On-screen size of one 8x8 tile (matches the old 20px cells).
const TILE_DISPLAY: f32 = 20.0;

impl Gui {
    pub(in crate) fn render_tile_explorer_panel(&mut self, ctx: &Context, debug: Option<&DebugSnapshot>) {
        if let Some(snap) = debug {
            egui::Window::new("Tile Explorer")
                .default_pos([1120.0, 50.0])
                .default_size([350.0, 500.0])
                .collapsible(true)
                .resizable(true)
                .frame(egui::Frame::window(&ctx.style_of(ctx.theme())).fill(crate::ui::PANEL_BACKGROUND))
                .show(ctx, |ui| {
                    ui.set_min_width(320.0);

                    ui.monospace(egui::RichText::new("VRAM Tile Data").color(egui::Color32::YELLOW));
                    ui.small(egui::RichText::new("8x8 pixel tiles from 0x8000-0x97FF").color(egui::Color32::LIGHT_GRAY));

                    // CGB/DMG specific controls
                    if snap.cgb {
                        ui.separator();
                        let current_vbk = snap.mmio.vbk & 1;
                        ui.horizontal(|ui| {
                            ui.label("VRAM Bank:");
                            ui.radio_value(&mut self.tile_explorer_vram_bank, 0, "Bank 0");
                            ui.radio_value(&mut self.tile_explorer_vram_bank, 1, "Bank 1");
                            ui.label(format!("(Current: {})", current_vbk));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Palette:");
                            ui.radio_value(&mut self.tile_explorer_palette, 0, "BG Pal 0");
                            for i in 1..8 {
                                ui.radio_value(&mut self.tile_explorer_palette, i, format!("BG {}", i));
                            }
                        });
                    } else {
                        // Get current palette for DMG color mapping
                        let bgp = snap.mmio.bgp;
                        ui.small(egui::RichText::new(format!("Using BGP palette: {:02X}", bgp)).color(egui::Color32::LIGHT_GRAY));
                    }

                    ui.separator();

                    // Bake all 384 tiles into one atlas texture and draw it as a
                    // single scaled image, rather than emitting 384*64 rects.
                    let bank = if snap.cgb { self.tile_explorer_vram_bank } else { 0 };
                    let pixels = build_tile_atlas(self, snap, bank);
                    let tex = self
                        .tile_atlas_tex
                        .update(ctx, "tile_atlas", ATLAS_W, ATLAS_H, pixels);

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let size = egui::vec2(
                            TILES_PER_ROW as f32 * TILE_DISPLAY,
                            TILE_ROWS as f32 * TILE_DISPLAY,
                        );
                        let image = egui::Image::new(egui::load::SizedTexture::new(tex, size))
                            .texture_options(egui::TextureOptions::NEAREST)
                            .sense(egui::Sense::hover());
                        let resp = ui.add(image);
                        let rect = resp.rect;

                        // Grid lines between tiles (a few dozen strokes, not
                        // thousands of rects).
                        let painter = ui.painter_at(rect);
                        let grid = egui::Stroke::new(0.5, egui::Color32::from_gray(90));
                        for c in 0..=TILES_PER_ROW {
                            let x = rect.min.x + c as f32 * TILE_DISPLAY;
                            painter.vline(x, rect.min.y..=rect.max.y, grid);
                        }
                        for r in 0..=TILE_ROWS {
                            let y = rect.min.y + r as f32 * TILE_DISPLAY;
                            painter.hline(rect.min.x..=rect.max.x, y, grid);
                        }

                        // Hover: map the pointer to a tile and show its details,
                        // plus a highlight box (replaces the per-tile hover).
                        if let Some(pos) = resp.hover_pos() {
                            let col = ((pos.x - rect.min.x) / TILE_DISPLAY).floor() as i32;
                            let row = ((pos.y - rect.min.y) / TILE_DISPLAY).floor() as i32;
                            if (0..TILES_PER_ROW as i32).contains(&col)
                                && (0..TILE_ROWS as i32).contains(&row)
                            {
                                let tile_index = row as usize * TILES_PER_ROW + col as usize;
                                let tile_addr = 0x8000u16 + (tile_index as u16 * 16);
                                let hl = egui::Rect::from_min_size(
                                    egui::pos2(
                                        rect.min.x + col as f32 * TILE_DISPLAY,
                                        rect.min.y + row as f32 * TILE_DISPLAY,
                                    ),
                                    egui::vec2(TILE_DISPLAY, TILE_DISPLAY),
                                );
                                painter.rect_stroke(
                                    hl,
                                    0.0,
                                    egui::Stroke::new(1.5, egui::Color32::YELLOW),
                                    egui::StrokeKind::Middle,
                                );
                                resp.on_hover_text(format!(
                                    "Tile #{}\nVRAM: 0x{:04X}-0x{:04X}",
                                    tile_index,
                                    tile_addr,
                                    tile_addr + 15
                                ));
                            }
                        }
                    });

                    ui.separator();
                    ui.small(egui::RichText::new("Hover tiles for details").color(egui::Color32::LIGHT_GRAY));
                    if snap.cgb {
                        ui.small(egui::RichText::new(format!("Showing VRAM Bank {} with CGB Palette {}",
                            self.tile_explorer_vram_bank, self.tile_explorer_palette)).color(egui::Color32::LIGHT_GRAY));
                    } else {
                        ui.small(egui::RichText::new("Uses current BGP palette").color(egui::Color32::LIGHT_GRAY));
                    }
                });
        }
    }
}

/// Decode all 384 VRAM tiles into a `ATLAS_W`×`ATLAS_H` row-major pixel buffer,
/// 16 tiles per row. Same palette mapping the panel used per-pixel, done once.
fn build_tile_atlas(gui: &Gui, snap: &DebugSnapshot, bank: u8) -> Vec<egui::Color32> {
    let mut pixels = vec![egui::Color32::BLACK; ATLAS_W * ATLAS_H];
    let bgp = snap.mmio.bgp;
    for tile_index in 0..TOTAL_TILES {
        let tile_col = tile_index % TILES_PER_ROW;
        let tile_row = tile_index / TILES_PER_ROW;
        let tile_addr = 0x8000u16 + (tile_index as u16 * 16);
        for y in 0..8u16 {
            let low_byte = snap.vram_byte(bank, tile_addr + (y * 2));
            let high_byte = snap.vram_byte(bank, tile_addr + (y * 2) + 1);
            let px_y = tile_row * 8 + y as usize;
            for x in 0..8usize {
                let bit = 7 - x; // Pixels are stored MSB first
                let low_bit = (low_byte >> bit) & 1;
                let high_bit = (high_byte >> bit) & 1;
                let pixel_value = (high_bit << 1) | low_bit;

                let color = if snap.cgb {
                    let (r, g, b) = snap
                        .cgb_bg_rgb(gui.tile_explorer_palette, pixel_value)
                        .unwrap_or((0, 0, 0));
                    egui::Color32::from_rgb(r, g, b)
                } else {
                    let palette_bits = (bgp >> (pixel_value * 2)) & 0x03;
                    match palette_bits {
                        0 => egui::Color32::from_rgb(255, 255, 255),
                        1 => egui::Color32::from_rgb(170, 170, 170),
                        2 => egui::Color32::from_rgb(85, 85, 85),
                        _ => egui::Color32::from_rgb(0, 0, 0),
                    }
                };
                pixels[px_y * ATLAS_W + tile_col * 8 + x] = color;
            }
        }
    }
    pixels
}

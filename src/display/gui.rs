use std::env;

use egui::{ClippedPrimitive, Context, TexturesDelta, ViewportId};
use egui_wgpu::{Renderer, ScreenDescriptor};
use pixels::{wgpu, PixelsContext};
use winit::event_loop::EventLoopWindowTarget;
use winit::window::Window;

pub enum GuiAction {
    Exit,
    SaveState(std::path::PathBuf),
    LoadState(std::path::PathBuf),
    LoadRom(std::path::PathBuf),
    TogglePause,
    Restart,
    ClearError,
}

pub(crate) struct Framework {
    egui_ctx: Context,
    egui_state: egui_winit::State,
    screen_descriptor: ScreenDescriptor,
    renderer: Renderer,
    paint_jobs: Vec<ClippedPrimitive>,
    textures: TexturesDelta,

    gui: Gui,
}

struct Gui {
    error_message: Option<String>,
    status_message: Option<String>,
    show_cpu_registers: bool,
    show_stack_explorer: bool,
    show_memory_explorer: bool,
    show_ppu_debug: bool,
    show_palette_explorer: bool,
    show_tile_explorer: bool,
    stack_scroll_offset: i16,
    memory_explorer_address: String,
    memory_explorer_parsed_address: u16,
    memory_scroll_offset: i16,
}

impl Gui {
    fn new() -> Self {
        Self { 
            error_message: None,
            status_message: None,
            show_cpu_registers: true,
            show_stack_explorer: false,
            show_memory_explorer: false,
            show_ppu_debug: false,
            show_palette_explorer: false,
            show_tile_explorer: false,
            stack_scroll_offset: 0,
            memory_explorer_address: String::from("0000"),
            memory_explorer_parsed_address: 0x0000,
            memory_scroll_offset: 0,
        }
    }

    /// Create the UI using egui.
    fn ui(&mut self, ctx: &Context, paused: bool, registers: Option<&crate::cpu::registers::Registers>, gb: Option<&crate::gb::GB>) -> (Option<GuiAction>, bool) {
        let mut action = None;
        let mut any_menu_open = false;
        
        egui::TopBottomPanel::top("menubar_container").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    any_menu_open = true;
                    if ui.button("Load ROM").clicked() {
                        let mut dialog = rfd::FileDialog::new()
                            .add_filter("Game Boy ROM", &["gb", "gbc"])
                            .add_filter("All Files", &["*"]);
                        if env::current_dir().is_ok() {
                            dialog = dialog.set_directory(env::current_dir().unwrap());
                        }
                        if let Some(path) = dialog.pick_file() {
                            action = Some(GuiAction::LoadRom(path));
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
                            action = Some(GuiAction::SaveState(path));
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
                            action = Some(GuiAction::LoadState(path));
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        action = Some(GuiAction::Exit);
                        ui.close_menu();
                    }
                });
                
                ui.menu_button("Emulation", |ui| {
                    any_menu_open = true;
                    if ui.button("Restart").clicked() {
                        action = Some(GuiAction::Restart);
                        ui.close_menu();
                    }
                    ui.separator();
                    let pause_text = if paused { "Resume" } else { "Pause" };
                    if ui.button(pause_text).clicked() {
                        action = Some(GuiAction::TogglePause);
                        ui.close_menu();
                    }
                });

                ui.menu_button("Debug", |ui| {
                    any_menu_open = true;
                    ui.checkbox(&mut self.show_cpu_registers, "CPU Registers");
                    ui.checkbox(&mut self.show_stack_explorer, "Stack Explorer");
                    ui.checkbox(&mut self.show_memory_explorer, "Memory Explorer");
                    ui.checkbox(&mut self.show_ppu_debug, "PPU");
                    ui.checkbox(&mut self.show_palette_explorer, "Palette Explorer");
                    ui.checkbox(&mut self.show_tile_explorer, "Tile Explorer");
                });
            });
        });

        // Stack overlay
        if self.show_stack_explorer {
            if let Some(regs) = registers {
                if let Some(gb_ref) = gb {
                    egui::Window::new("Stack Explorer")
                        .default_pos([220.0, 50.0])
                        .default_size([180.0, 400.0])
                        .collapsible(true)
                        .resizable(false)
                        .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgba_unmultiplied(64, 64, 64, 220)))
                        .show(ctx, |ui| {
                            ui.set_width(160.0);
                            
                            let sp = regs.sp;
                            ui.monospace(egui::RichText::new(format!("SP: {:04X}", sp)).color(egui::Color32::YELLOW));
                            
                            if ui.button("↑ Scroll Up").clicked() {
                                if self.stack_scroll_offset < 100 { // Reasonable upper limit
                                    self.stack_scroll_offset = self.stack_scroll_offset.saturating_add(1);
                                }
                            }
                            
                            ui.separator();
                            
                            // Show stack contents around SP with scroll offset
                            let base_start = sp.saturating_sub(8); // 4 entries above SP (8 bytes)
                            let scroll_adjustment = (self.stack_scroll_offset as i32) * 2; // 2 bytes per scroll step
                            let start_addr = if scroll_adjustment >= 0 {
                                base_start.saturating_sub(scroll_adjustment as u16)
                            } else {
                                base_start.saturating_add((-scroll_adjustment) as u16)
                            };
                            let end_addr = std::cmp::min(start_addr.saturating_add(16), 0xFFFF); // Show 9 entries, capped at 0xFFFF
                            
                            for addr in (start_addr..=end_addr).step_by(2) {
                                let val1 = gb_ref.read_memory(addr);
                                let val2 = if addr < 0xFFFF { gb_ref.read_memory(addr + 1) } else { 0 };
                                let word_val = ((val2 as u16) << 8) | (val1 as u16);
                                
                                let color = if addr == sp {
                                    egui::Color32::YELLOW // Highlight current SP
                                } else if addr < sp {
                                    egui::Color32::LIGHT_GRAY // Above SP (older entries)
                                } else {
                                    egui::Color32::GRAY // Below SP (unused)
                                };
                                
                                let marker = if addr == sp { "→" } else { " " };
                                ui.monospace(egui::RichText::new(format!("{} {:04X}: {:04X}", marker, addr, word_val)).color(color));
                            }
                            
                            ui.separator();
                            
                            if ui.button("↓ Scroll Down").clicked() {
                                if self.stack_scroll_offset > -100 { // Reasonable lower limit
                                    self.stack_scroll_offset = self.stack_scroll_offset.saturating_sub(1);
                                }
                            }
                            
                            // Reset button
                            ui.horizontal(|ui| {
                                if ui.button("Center on SP").clicked() {
                                    self.stack_scroll_offset = 0;
                                }
                                ui.small(egui::RichText::new(format!("Offset: {}", self.stack_scroll_offset)).color(egui::Color32::LIGHT_GRAY));
                            });
                            
                            ui.separator();
                            ui.small(egui::RichText::new("Yellow = SP position").color(egui::Color32::LIGHT_GRAY));
                        });
                }
            }
        }

        // Memory explorer overlay
        if self.show_memory_explorer {
            if let Some(gb_ref) = gb {
                egui::Window::new("Memory Explorer")
                    .default_pos([410.0, 50.0])
                    .default_size([220.0, 400.0])
                    .collapsible(true)
                    .resizable(false)
                    .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgba_unmultiplied(64, 64, 64, 220)))
                    .show(ctx, |ui| {
                        ui.set_width(200.0);
                        
                        // Address input field
                        ui.horizontal(|ui| {
                            ui.label("Address:");
                            if ui.text_edit_singleline(&mut self.memory_explorer_address).changed() {
                                // Parse hex input (with or without 0x prefix)
                                let clean_input = if self.memory_explorer_address.starts_with("0x") || self.memory_explorer_address.starts_with("0X") {
                                    &self.memory_explorer_address[2..]
                                } else {
                                    &self.memory_explorer_address
                                };
                                
                                if let Ok(addr) = u16::from_str_radix(clean_input, 16) {
                                    self.memory_explorer_parsed_address = addr;
                                    self.memory_scroll_offset = 0; // Reset scroll when address changes
                                }
                            }
                        });
                        
                        // Scroll up button (move pointer to lower addresses)
                        if ui.button("↑ Move Up").clicked() {
                            // Ensure we don't go below 0x0000
                            if self.memory_explorer_parsed_address >= 2 {
                                self.memory_explorer_parsed_address = self.memory_explorer_parsed_address.saturating_sub(2);
                                self.memory_explorer_address = format!("{:04X}", self.memory_explorer_parsed_address);
                            }
                        }
                        
                        ui.separator();
                        
                        // Show memory contents around the current address (fixed view)
                        let start_addr = self.memory_explorer_parsed_address.saturating_sub(8); // 4 entries above (8 bytes)
                        let end_addr = std::cmp::min(start_addr.saturating_add(16), 0xFFFF); // Show 9 entries (18 bytes), capped at 0xFFFF
                        
                        for addr in (start_addr..=end_addr).step_by(2) {
                            let val1 = gb_ref.read_memory(addr);
                            let val2 = if addr < 0xFFFF { gb_ref.read_memory(addr + 1) } else { 0 };
                            let word_val = ((val2 as u16) << 8) | (val1 as u16);
                            
                            let color = if addr == self.memory_explorer_parsed_address {
                                egui::Color32::YELLOW // Highlight target address
                            } else if addr < self.memory_explorer_parsed_address {
                                egui::Color32::LIGHT_GRAY // Before target
                            } else {
                                egui::Color32::GRAY // After target
                            };
                            
                            let marker = if addr == self.memory_explorer_parsed_address { "→" } else { " " };
                            ui.monospace(egui::RichText::new(format!("{} {:04X}: {:04X}", marker, addr, word_val)).color(color));
                        }
                        
                        ui.separator();
                        
                        // Scroll down button (move pointer to higher addresses)
                        if ui.button("↓ Move Down").clicked() {
                            // Ensure we don't go above 0xFFFF
                            if self.memory_explorer_parsed_address <= 0xFFFF - 2 {
                                self.memory_explorer_parsed_address = self.memory_explorer_parsed_address.saturating_add(2);
                                self.memory_explorer_address = format!("{:04X}", self.memory_explorer_parsed_address);
                            }
                        }
                        
                        // Navigation buttons
                        ui.horizontal(|ui| {
                            if ui.button("+0x10").clicked() {
                                // Add 0x10, but clamp to maximum valid address (0xFFFE for 16-bit words)
                                let new_addr = self.memory_explorer_parsed_address.saturating_add(0x10);
                                self.memory_explorer_parsed_address = std::cmp::min(new_addr, 0xFFFE);
                                self.memory_explorer_address = format!("{:04X}", self.memory_explorer_parsed_address);
                            }
                        });
                        
                        ui.horizontal(|ui| {
                            if ui.button("-0x10").clicked() {
                                // Subtract 0x10, but clamp to minimum valid address (0x0000)
                                self.memory_explorer_parsed_address = self.memory_explorer_parsed_address.saturating_sub(0x10);
                                self.memory_explorer_address = format!("{:04X}", self.memory_explorer_parsed_address);
                            }
                            ui.small(egui::RichText::new(format!("Current: {:04X}", self.memory_explorer_parsed_address)).color(egui::Color32::LIGHT_GRAY));
                        });
                        
                        ui.separator();
                        ui.small(egui::RichText::new("Yellow = target address").color(egui::Color32::LIGHT_GRAY));
                        ui.small(egui::RichText::new("Input: hex (with/without 0x)").color(egui::Color32::LIGHT_GRAY));
                    });
            }
        }

        // PPU Debug overlay
        if self.show_ppu_debug {
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

        // Palette Explorer overlay
        if self.show_palette_explorer {
            if let Some(gb_ref) = gb {
                egui::Window::new("Palette Explorer")
                    .default_pos([900.0, 50.0])
                    .default_size([200.0, 300.0])
                    .collapsible(true)
                    .resizable(false)
                    .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgba_unmultiplied(64, 64, 64, 220)))
                    .show(ctx, |ui| {
                        ui.set_width(180.0);
                        
                        // Background Palette (BGP)
                        let bgp = gb_ref.read_memory(crate::ppu::ppu::BGP);
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
                        
                        // Bit breakdown visualization
                        ui.small(egui::RichText::new("Bit Layout:").color(egui::Color32::LIGHT_GRAY));
                        ui.horizontal(|ui| {
                            for bit in (0..8).rev() {
                                let bit_set = (bgp >> bit) & 1 == 1;
                                let bit_color = if bit_set { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY };
                                ui.small(egui::RichText::new(format!("{}", if bit_set { "1" } else { "0" })).color(bit_color));
                            }
                        });
                        
                        // Bit labels
                        ui.horizontal(|ui| {
                            ui.small(egui::RichText::new("P3").color(egui::Color32::LIGHT_GRAY));
                            ui.small(egui::RichText::new("  ").color(egui::Color32::LIGHT_GRAY));
                            ui.small(egui::RichText::new("P2").color(egui::Color32::LIGHT_GRAY));
                            ui.small(egui::RichText::new("  ").color(egui::Color32::LIGHT_GRAY));
                            ui.small(egui::RichText::new("P1").color(egui::Color32::LIGHT_GRAY));
                            ui.small(egui::RichText::new("  ").color(egui::Color32::LIGHT_GRAY));
                            ui.small(egui::RichText::new("P0").color(egui::Color32::LIGHT_GRAY));
                        });
                        
                        ui.separator();
                        ui.small(egui::RichText::new("P0=Background, P3=Foreground").color(egui::Color32::LIGHT_GRAY));
                        ui.small(egui::RichText::new("Each palette: 2 bits").color(egui::Color32::LIGHT_GRAY));
                    });
            }
        }

        // Tile Explorer overlay
        if self.show_tile_explorer {
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
                        let bgp = gb_ref.read_memory(crate::ppu::ppu::BGP);
                        
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

        // Debug overlay
        if self.show_cpu_registers {
            if let Some(regs) = registers {
                if let Some(gb_ref) = gb {
                    egui::Window::new("CPU Registers")
                        .default_pos([10.0, 50.0])
                        .default_size([200.0, 400.0])
                        .collapsible(true)
                        .resizable(false)
                        .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgba_unmultiplied(64, 64, 64, 220)))
                        .show(ctx, |ui| {
                            ui.set_width(180.0);
                            
                            // Use rich text for better color control
                            ui.monospace(egui::RichText::new(format!("A: {:02X}    F: {:02X}", regs.a, regs.f)).color(egui::Color32::WHITE));
                            ui.monospace(egui::RichText::new(format!("B: {:02X}    C: {:02X}", regs.b, regs.c)).color(egui::Color32::WHITE));
                            ui.monospace(egui::RichText::new(format!("D: {:02X}    E: {:02X}", regs.d, regs.e)).color(egui::Color32::WHITE));
                            ui.monospace(egui::RichText::new(format!("H: {:02X}    L: {:02X}", regs.h, regs.l)).color(egui::Color32::WHITE));
                            ui.separator();
                            
                            // Pretty-print the flags (F register bits 7-4)
                            let z_flag = (regs.f & 0x80) != 0; // Bit 7: Zero flag
                            let n_flag = (regs.f & 0x40) != 0; // Bit 6: Subtract flag
                            let h_flag = (regs.f & 0x20) != 0; // Bit 5: Half Carry flag
                            let c_flag = (regs.f & 0x10) != 0; // Bit 4: Carry flag
                            
                            ui.horizontal(|ui| {
                                ui.monospace(egui::RichText::new(format!("Z:{}", if z_flag { "1" } else { "0" }))
                                    .color(if z_flag { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));
                                ui.monospace(egui::RichText::new(format!("N:{}", if n_flag { "1" } else { "0" }))
                                    .color(if n_flag { egui::Color32::LIGHT_BLUE } else { egui::Color32::GRAY }));
                                ui.monospace(egui::RichText::new(format!("H:{}", if h_flag { "1" } else { "0" }))
                                    .color(if h_flag { egui::Color32::YELLOW } else { egui::Color32::GRAY }));
                                ui.monospace(egui::RichText::new(format!("C:{}", if c_flag { "1" } else { "0" }))
                                    .color(if c_flag { egui::Color32::LIGHT_RED } else { egui::Color32::GRAY }));
                            });
                            ui.separator();
                            ui.monospace(egui::RichText::new(format!("PC: {:04X}", regs.pc.saturating_sub(1))).color(egui::Color32::WHITE));
                            ui.monospace(egui::RichText::new(format!("SP: {:04X}", regs.sp)).color(egui::Color32::WHITE));
                            ui.separator();
                            ui.monospace(egui::RichText::new(format!("IME: {}", if regs.ime { "ON" } else { "OFF" })).color(egui::Color32::WHITE));
                            ui.separator();
                            
                            // Instruction viewer around PC
                            ui.small(egui::RichText::new("Instructions:").color(egui::Color32::LIGHT_GRAY));
                            let pc = regs.pc;
                            let display_pc = pc.saturating_sub(1); // Show the instruction that was just executed
                            let start_addr = display_pc.saturating_sub(1); // 1 byte before the executed instruction
                            let end_addr = display_pc.saturating_add(4);   // 4 bytes after the executed instruction
                            
                            for addr in start_addr..=end_addr {
                                let byte_val = gb_ref.read_memory(addr);
                                
                                let color = if addr == display_pc {
                                    egui::Color32::YELLOW // Highlight the instruction that was just executed
                                } else if addr < display_pc {
                                    egui::Color32::LIGHT_GRAY // Before executed instruction
                                } else {
                                    egui::Color32::GRAY // After executed instruction (upcoming)
                                };
                                
                                let marker = if addr == display_pc { "→" } else { " " };
                                ui.monospace(egui::RichText::new(format!("{} {:04X}: {:02X}", marker, addr, byte_val)).color(color));
                            }
                            
                            ui.separator();
                            ui.small(egui::RichText::new("F = step frame").color(egui::Color32::LIGHT_GRAY));
                            ui.small(egui::RichText::new("N = step cycle").color(egui::Color32::LIGHT_GRAY));
                        });
                }
            }
        }

        // Display status message if there's one
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

        // Display error panel if there's an error
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
                        action = Some(GuiAction::Restart);
                    }
                    
                    if ui.button("Clear Error (Debug Mode)").clicked() {
                        action = Some(GuiAction::ClearError);
                    }
                });
            });
        }
        
        (action, any_menu_open)
    }

    fn set_error(&mut self, error_message: String) {
        self.error_message = Some(error_message);
    }

    fn clear_error(&mut self) {
        self.error_message = None;
    }

    fn set_status(&mut self, status_message: String) {
        self.status_message = Some(status_message);
    }
}

impl Framework {
    pub(crate) fn new<T>(
        event_loop: &EventLoopWindowTarget<T>,
        width: u32,
        height: u32,
        scale_factor: f32,
        pixels: &pixels::Pixels,
    ) -> Self {
        let max_texture_size = pixels.device().limits().max_texture_dimension_2d as usize;

        let egui_ctx = Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            ViewportId::ROOT,
            event_loop,
            Some(scale_factor),
            Some(max_texture_size),
        );
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: scale_factor,
        };
        let renderer = Renderer::new(pixels.device(), pixels.render_texture_format(), None, 1);
        let textures = TexturesDelta::default();
        let gui = Gui::new();

        Self {
            egui_ctx,
            egui_state,
            screen_descriptor,
            renderer,
            paint_jobs: Vec::new(),
            textures,
            gui,
        }
    }

    pub(crate) fn handle_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        let _ = self.egui_state.on_window_event(window, event);
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.screen_descriptor.size_in_pixels = [width, height];
        }
    }

    pub(crate) fn scale_factor(&mut self, scale_factor: f64) {
        self.screen_descriptor.pixels_per_point = scale_factor as f32;
    }

    pub(crate) fn set_error(&mut self, error_message: String) {
        self.gui.set_error(error_message);
    }

    pub(crate) fn clear_error(&mut self) {
        self.gui.clear_error();
    }

    pub(crate) fn set_status(&mut self, status_message: String) {
        self.gui.set_status(status_message);
    }

    pub(crate) fn prepare(&mut self, window: &Window, paused: bool, registers: Option<&crate::cpu::registers::Registers>, gb: Option<&crate::gb::GB>) -> (Option<GuiAction>, bool) {
        let raw_input = self.egui_state.take_egui_input(window);
        let mut result = (None, false);
        let output = self.egui_ctx.run(raw_input, |egui_ctx| {
            result = self.gui.ui(egui_ctx, paused, registers, gb);
        });

        self.textures.append(output.textures_delta);
        self.egui_state
            .handle_platform_output(window, output.platform_output);
        self.paint_jobs = self
            .egui_ctx
            .tessellate(output.shapes, self.screen_descriptor.pixels_per_point);
            
        result
    }

    pub(crate) fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        render_target: &wgpu::TextureView,
        context: &PixelsContext,
    ) {
        for (id, image_delta) in &self.textures.set {
            self.renderer
                .update_texture(&context.device, &context.queue, *id, image_delta);
        }
        self.renderer.update_buffers(
            &context.device,
            &context.queue,
            encoder,
            &self.paint_jobs,
            &self.screen_descriptor,
        );

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: render_target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            self.renderer
                .render(&mut rpass, &self.paint_jobs, &self.screen_descriptor);
        }

        let textures = std::mem::take(&mut self.textures);
        for id in &textures.free {
            self.renderer.free_texture(id);
        }
    }
}

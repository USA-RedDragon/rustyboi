use egui::Context;
use rustyboi_core_lib::gb;
use crate::ui::Gui;

impl Gui {
    pub(in crate) fn render_memory_explorer_panel(&mut self, ctx: &Context, gb: Option<&gb::GB>) {
        if let Some(gb_ref) = gb {
            egui::Window::new("Memory Explorer")
                .default_pos([410.0, 50.0])
                .default_size([220.0, 400.0])
                .collapsible(true)
                .resizable(false)
                .frame(egui::Frame::window(&ctx.style()).fill(crate::ui::PANEL_BACKGROUND))
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
                        if self.memory_explorer_parsed_address >= 1 {
                            self.memory_explorer_parsed_address = self.memory_explorer_parsed_address.saturating_sub(1);
                            self.memory_explorer_address = format!("{:04X}", self.memory_explorer_parsed_address);
                        }
                    }
                    
                    ui.separator();
                    
                    // Show memory contents around the current address (fixed view)
                    let start_addr = self.memory_explorer_parsed_address.saturating_sub(4); // 4 entries above
                    let end_addr = std::cmp::min(start_addr.saturating_add(8), 0xFFFF); // Show 9 entries
                    
                    for addr in (start_addr..=end_addr).step_by(1) {
                        let val = gb_ref.read_memory(addr);
                        
                        let color = if addr == self.memory_explorer_parsed_address {
                            egui::Color32::YELLOW // Highlight target address
                        } else if addr < self.memory_explorer_parsed_address {
                            egui::Color32::LIGHT_GRAY // Before target
                        } else {
                            egui::Color32::GRAY // After target
                        };
                        
                        let marker = if addr == self.memory_explorer_parsed_address { "→" } else { " " };
                        ui.monospace(egui::RichText::new(format!("{} {:04X}: {:02X}", marker, addr, val)).color(color));
                    }
                    
                    ui.separator();
                    
                    // Scroll down button (move pointer to higher addresses)
                    if ui.button("↓ Move Down").clicked() {
                        // Ensure we don't go above 0xFFFF
                        if self.memory_explorer_parsed_address < 0xFFFF {
                            self.memory_explorer_parsed_address = self.memory_explorer_parsed_address.saturating_add(1);
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
}

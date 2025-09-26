use egui::Context;
use super::super::main_ui::Gui;

impl Gui {
    pub(in crate::display::gui) fn render_stack_explorer_panel(&mut self, ctx: &Context, registers: Option<&crate::cpu::registers::Registers>, gb: Option<&crate::gb::GB>) {
        if let Some(regs) = registers
            && let Some(gb_ref) = gb {
                egui::Window::new("Stack Explorer")
                    .default_pos([220.0, 50.0])
                    .default_size([180.0, 400.0])
                    .collapsible(true)
                    .resizable(false)
                    .frame(egui::Frame::window(&ctx.style()).fill(crate::display::gui::main_ui::PANEL_BACKGROUND))
                    .show(ctx, |ui| {
                        ui.set_width(160.0);
                        
                        let sp = regs.sp;
                        ui.monospace(egui::RichText::new(format!("SP: {:04X}", sp)).color(egui::Color32::YELLOW));
                        
                        if ui.button("↑ Scroll Up").clicked()
                            && self.stack_scroll_offset < 100 { // Reasonable upper limit
                                self.stack_scroll_offset = self.stack_scroll_offset.saturating_add(1);
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
                        
                        if ui.button("↓ Scroll Down").clicked()
                            && self.stack_scroll_offset > -100 { // Reasonable lower limit
                                self.stack_scroll_offset = self.stack_scroll_offset.saturating_sub(1);
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

use egui::Context;
use super::super::actions::GuiAction;
use super::super::main_ui::Gui;
use crate::cpu::disassembler::Disassembler;

impl Gui {
    pub(in crate::display::gui) fn render_cpu_registers_panel(&mut self, ctx: &Context, registers: Option<&crate::cpu::registers::Registers>, gb: Option<&crate::gb::GB>, action: &mut Option<GuiAction>, paused: bool) {
        if let Some(regs) = registers
            && let Some(gb_ref) = gb {
                egui::Window::new("CPU Registers")
                    .default_pos([10.0, 50.0])
                    .default_size([250.0, 400.0])
                    .collapsible(true)
                    .resizable(false)
                    .frame(egui::Frame::window(&ctx.style()).fill(crate::display::gui::main_ui::PANEL_BACKGROUND))
                    .show(ctx, |ui| {
                        ui.set_width(230.0);
                        
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
                        let display_pc = pc.saturating_sub(0); // Show the instruction that was just executed
                        
                        // Display exactly 5 instructions starting from the current PC
                        let mut addr = display_pc;
                        let mut instruction_count = 0;
                        const MAX_INSTRUCTIONS: usize = 5;
                        
                        while instruction_count < MAX_INSTRUCTIONS {
                            let (mnemonic, instruction_length) = Disassembler::disassemble_with_reader(addr, |address| gb_ref.read_memory(address));
                            
                            let color = if addr == display_pc {
                                egui::Color32::YELLOW // Highlight the instruction that was just executed
                            } else if addr < display_pc {
                                egui::Color32::LIGHT_GRAY // Before executed instruction
                            } else {
                                egui::Color32::GRAY // After executed instruction (upcoming)
                            };
                            
                            let marker = if addr == display_pc { "→" } else { " " };
                            
                            // Show the first byte and mnemonic for single-byte instructions
                            // For multi-byte instructions, show the full instruction with all bytes
                            let bytes = if instruction_length == 1 {
                                format!("{:02X}", gb_ref.read_memory(addr))
                            } else if instruction_length == 2 {
                                format!("{:02X} {:02X}", 
                                    gb_ref.read_memory(addr), 
                                    gb_ref.read_memory(addr + 1))
                            } else {
                                format!("{:02X} {:02X} {:02X}", 
                                    gb_ref.read_memory(addr), 
                                    gb_ref.read_memory(addr + 1),
                                    gb_ref.read_memory(addr + 2))
                            };
                            
                            ui.monospace(egui::RichText::new(format!("{} {:04X}: {:8} {}", marker, addr, bytes, mnemonic)).color(color));
                            
                            addr += instruction_length;
                            instruction_count += 1;
                        }
                        ui.separator();

                        // Step controls
                        ui.small(egui::RichText::new("Step Controls:").color(egui::Color32::LIGHT_GRAY));
                        
                        // Slider for step count
                        ui.horizontal(|ui| {
                            ui.label("Steps:");
                            ui.add(egui::Slider::new(&mut self.step_count, 1..=100)
                                .text("instructions"));
                        });
                        
                        ui.separator();
                        
                        // Step buttons
                        ui.horizontal(|ui| {
                            if paused {
                                // Step Cycles button with hold functionality
                                let cycles_response = ui.button("Step Cycles");
                                if cycles_response.clicked() {
                                    // Initial press - execute immediately
                                    *action = Some(GuiAction::StepCycles(self.step_count));
                                    self.step_cycles_held_frames = 0;
                                } else if cycles_response.is_pointer_button_down_on() {
                                    // Button is being held down
                                    self.step_cycles_held_frames += 1;
                                    // After 15 frames (250ms at 60fps), start repeating every 4 frames (67ms at 60fps)
                                    if self.step_cycles_held_frames > 15 && (self.step_cycles_held_frames - 15).is_multiple_of(4) {
                                        *action = Some(GuiAction::StepCycles(self.step_count));
                                    }
                                } else {
                                    // Button released - reset state
                                    self.step_cycles_held_frames = 0;
                                }
                                
                                // Step Frames button with hold functionality
                                let frames_response = ui.button("Step Frames");
                                if frames_response.clicked() {
                                    // Initial press - execute immediately
                                    *action = Some(GuiAction::StepFrames(self.step_count));
                                    self.step_frames_held_frames = 0;
                                } else if frames_response.is_pointer_button_down_on() {
                                    // Button is being held down
                                    self.step_frames_held_frames += 1;
                                    // After 15 frames (250ms at 60fps), start repeating every 4 frames (67ms at 60fps)
                                    if self.step_frames_held_frames > 15 && (self.step_frames_held_frames - 15).is_multiple_of(4) {
                                        *action = Some(GuiAction::StepFrames(self.step_count));
                                    }
                                } else {
                                    // Button released - reset state
                                    self.step_frames_held_frames = 0;
                                }
                            } else {
                                ui.add_enabled(false, egui::Button::new("Step Cycles"));
                                ui.add_enabled(false, egui::Button::new("Step Frames"));
                            }
                        });
                        
                        if !paused {
                            ui.small(egui::RichText::new("(Pause to enable stepping)").color(egui::Color32::GRAY));
                        }
                        
                        ui.separator();
                        ui.small(egui::RichText::new("F = step frame | N = step cycle").color(egui::Color32::LIGHT_GRAY));
                    });
            }
    }
}

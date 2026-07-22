use egui::Context;
use crate::actions::GuiAction;
use crate::ui::Gui;
use rustyboi_debugger_lib::disassembler::Disassembler;
use rustyboi_session::DebugSnapshot;

/// One disassembled line of the CPU panel's instruction walk.
pub(crate) struct DisasmLine {
    pub addr: u16,
    pub bytes: String,
    pub mnemonic: String,
}

/// Walk `count` instructions forward from `start`, disassembling each one.
///
/// Every address step wraps: code can legitimately execute in HRAM right up to
/// 0xFFFE, so a plain `+` here panics in debug builds (and silently wraps in
/// release, making the two profiles disagree).
pub(crate) fn disassemble_walk<F>(start: u16, count: usize, mut read: F) -> Vec<DisasmLine>
where
    F: FnMut(u16) -> u8,
{
    let mut addr = start;
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        let (mnemonic, instruction_length) = Disassembler::disassemble_with_reader(addr, &mut read);

        // Show the first byte and mnemonic for single-byte instructions; for
        // multi-byte instructions, show all the bytes.
        let bytes = match instruction_length {
            1 => format!("{:02X}", read(addr)),
            2 => format!("{:02X} {:02X}", read(addr), read(addr.wrapping_add(1))),
            _ => format!(
                "{:02X} {:02X} {:02X}",
                read(addr),
                read(addr.wrapping_add(1)),
                read(addr.wrapping_add(2))
            ),
        };

        lines.push(DisasmLine { addr, bytes, mnemonic });
        addr = addr.wrapping_add(instruction_length);
    }
    lines
}

impl Gui {
    pub(in crate) fn render_cpu_registers_panel(&mut self, ctx: &Context, debug: Option<&DebugSnapshot>, action: &mut Option<GuiAction>, paused: bool) {
        if let Some(snap) = debug {
                let regs = &snap.cpu;
                egui::Window::new("CPU Registers")
                    .default_pos([10.0, 50.0])
                    .default_size([250.0, 400.0])
                    .collapsible(true)
                    .resizable(false)
                    .frame(egui::Frame::window(&ctx.style_of(ctx.theme())).fill(crate::ui::PANEL_BACKGROUND))
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
                        const MAX_INSTRUCTIONS: usize = 5;

                        for line in disassemble_walk(display_pc, MAX_INSTRUCTIONS, |address| snap.code_byte(address)) {
                            let color = if line.addr == display_pc {
                                egui::Color32::YELLOW // Highlight the instruction that was just executed
                            } else if line.addr < display_pc {
                                egui::Color32::LIGHT_GRAY // Before executed instruction
                            } else {
                                egui::Color32::GRAY // After executed instruction (upcoming)
                            };

                            let marker = if line.addr == display_pc { "→" } else { " " };

                            ui.monospace(egui::RichText::new(format!("{} {:04X}: {:8} {}", marker, line.addr, line.bytes, line.mnemonic)).color(color));
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

#[cfg(test)]
mod tests {
    use super::disassemble_walk;

    #[test]
    fn walk_wraps_past_the_top_of_the_address_space() {
        // 0xFA = LD A,(a16), a 3-byte instruction: starting at 0xFFFE the
        // operand fetches and the post-step both run off the end of the map.
        let lines = disassemble_walk(0xFFFE, 5, |_| 0xFA);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].addr, 0xFFFE);
        assert_eq!(lines[1].addr, 0x0001);
        assert_eq!(lines[0].bytes, "FA FA FA");
    }

    #[test]
    fn walk_wraps_from_the_last_byte() {
        let lines = disassemble_walk(0xFFFF, 5, |addr| if addr == 0xFFFF { 0x00 } else { 0xFA });
        assert_eq!(lines[0].addr, 0xFFFF);
        assert_eq!(lines[0].mnemonic, "NOP");
        assert_eq!(lines[1].addr, 0x0000);
    }

    #[test]
    fn walk_reports_addresses_and_bytes_in_order() {
        // NOP, LD B,d8 (2 bytes), LD BC,d16 (3 bytes), NOP, NOP
        let prog = [0x00u8, 0x06, 0x11, 0x01, 0x34, 0x12, 0x00, 0x00];
        let lines = disassemble_walk(0x0100, 5, |addr| prog[addr.wrapping_sub(0x0100) as usize]);
        let addrs: Vec<u16> = lines.iter().map(|l| l.addr).collect();
        assert_eq!(addrs, vec![0x0100, 0x0101, 0x0103, 0x0106, 0x0107]);
        assert_eq!(lines[0].bytes, "00");
        assert_eq!(lines[1].bytes, "06 11");
        assert_eq!(lines[2].bytes, "01 34 12");
    }
}

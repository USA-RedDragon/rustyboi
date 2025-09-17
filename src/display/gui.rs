use std::env;

use egui::{ClippedPrimitive, Context, TexturesDelta, ViewportId};
use egui_wgpu::{Renderer, ScreenDescriptor};
use pixels::{wgpu, PixelsContext};
use winit::event_loop::EventLoopWindowTarget;
use winit::window::Window;

#[derive(Debug, Clone)]
pub enum GuiAction {
    Exit,
    SaveState(std::path::PathBuf),
    TogglePause,
    Restart,
    ClearError,
    StepFrame,
    StepCycle,
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
    show_debug_overlay: bool,
}

impl Gui {
    fn new() -> Self {
        Self { 
            error_message: None,
            status_message: None,
            show_debug_overlay: true,
        }
    }

    /// Create the UI using egui.
    fn ui(&mut self, ctx: &Context, paused: bool, registers: Option<&crate::cpu::registers::Registers>) -> (Option<GuiAction>, bool) {
        let mut action = None;
        let mut any_menu_open = false;
        
        egui::TopBottomPanel::top("menubar_container").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    any_menu_open = true;
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
                    if ui.checkbox(&mut self.show_debug_overlay, "Show Debug Overlay").clicked() {
                        ui.close_menu();
                    }
                });
            });
        });

        // Debug overlay
        if self.show_debug_overlay {
            if let Some(regs) = registers {
                egui::Window::new("CPU Registers")
                    .default_pos([10.0, 50.0])
                    .default_size([200.0, 300.0])
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
                        ui.monospace(egui::RichText::new(format!("PC: {:04X}", regs.pc)).color(egui::Color32::WHITE));
                        ui.monospace(egui::RichText::new(format!("SP: {:04X}", regs.sp)).color(egui::Color32::WHITE));
                        ui.separator();
                        ui.monospace(egui::RichText::new(format!("IME: {}", if regs.ime { "ON" } else { "OFF" })).color(egui::Color32::WHITE));
                        ui.separator();
                        ui.small(egui::RichText::new("F = step frame").color(egui::Color32::LIGHT_GRAY));
                        ui.small(egui::RichText::new("N = step cycle").color(egui::Color32::LIGHT_GRAY));
                    });
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

    pub(crate) fn prepare(&mut self, window: &Window, paused: bool, registers: Option<&crate::cpu::registers::Registers>) -> (Option<GuiAction>, bool) {
        let raw_input = self.egui_state.take_egui_input(window);
        let mut result = (None, false);
        let output = self.egui_ctx.run(raw_input, |egui_ctx| {
            result = self.gui.ui(egui_ctx, paused, registers);
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

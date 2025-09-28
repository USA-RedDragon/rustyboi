use egui::{ClippedPrimitive, Context, TexturesDelta, ViewportId};
use egui_wgpu::{Renderer, ScreenDescriptor};
use wgpu;
use winit::window::Window;

use rustyboi_core_lib::{cpu, gb};
use rustyboi_egui_lib::Gui;
use rustyboi_egui_lib::actions::GuiAction;

pub struct Framework {
    egui_ctx: Context,
    egui_state: egui_winit::State,
    screen_descriptor: ScreenDescriptor,
    renderer: Renderer,
    paint_jobs: Vec<ClippedPrimitive>,
    textures: TexturesDelta,

    gui: Gui,
}

impl Framework {
    pub fn new(
        width: u32,
        height: u32,
        scale_factor: f32,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> Self {
        let max_texture_size = device.limits().max_texture_dimension_2d as usize;

        let egui_ctx = Context::default();
        // Create a placeholder egui_state that we'll replace when we have a window
        // For now, we'll create it with default values and replace it later
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            ViewportId::ROOT,
            &egui_winit::winit::event_loop::EventLoop::new().unwrap(),
            Some(scale_factor),
            None, // theme
            Some(max_texture_size),
        );
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: scale_factor,
        };
        let renderer = Renderer::new(device, format, None, 1, false);
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

    pub fn setup_window(&mut self, window: &Window) {
        // Re-initialize egui_state with the actual window
        let scale_factor = self.screen_descriptor.pixels_per_point;
        let max_texture_size = 8192; // reasonable default
        self.egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            ViewportId::ROOT,
            window,
            Some(scale_factor),
            None, // theme
            Some(max_texture_size),
        );
    }

    pub fn handle_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        let _ = self.egui_state.on_window_event(window, event);
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.screen_descriptor.size_in_pixels = [width, height];
        }
    }

    pub fn scale_factor(&mut self, scale_factor: f64) {
        self.screen_descriptor.pixels_per_point = scale_factor as f32;
    }

    pub fn set_error(&mut self, error_message: String) {
        self.gui.set_error(error_message);
    }

    pub fn clear_error(&mut self) {
        self.gui.clear_error();
    }

    pub fn set_status(&mut self, status_message: String) {
        self.gui.set_status(status_message);
    }

    pub fn prepare(&mut self, window: &Window, paused: bool, registers: Option<&cpu::registers::Registers>, gb: Option<&gb::GB>) -> (Option<GuiAction>, bool) {
        let raw_input = self.egui_state.take_egui_input(window);
        let mut ui_result = None;
        let full_output = self.egui_ctx.run(raw_input, |egui_ctx| {
            ui_result = Some(self.gui.ui(egui_ctx, paused, registers, gb));
        });

        self.textures.append(full_output.textures_delta);
        self.egui_state
            .handle_platform_output(window, full_output.platform_output);

        self.paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, self.screen_descriptor.pixels_per_point);
            
        ui_result.unwrap_or((None, false))
    }

    pub fn render(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        render_target: &wgpu::TextureView,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        for (id, image_delta) in &self.textures.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }
        self.renderer.update_buffers(
            device,
            queue,
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

            // self.renderer
            //     .render(&mut rpass, &self.paint_jobs, &self.screen_descriptor);
        }

        let textures = std::mem::take(&mut self.textures);
        for id in &textures.free {
            self.renderer.free_texture(id);
        }
    }
}

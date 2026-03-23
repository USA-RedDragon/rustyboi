use egui::{ClippedPrimitive, Context, TexturesDelta, ViewportId};
use egui_wgpu::{Renderer, ScreenDescriptor};
use pixels::{wgpu, PixelsContext};
use std::sync::{Arc, Mutex};
use winit::event_loop::EventLoopWindowTarget;
use winit::window::Window;

use rustyboi_core_lib::{cpu, gb};
use rustyboi_egui_lib::Gui;
use rustyboi_egui_lib::actions::GuiAction;

use crate::game_renderer::PhysicalRect;

pub struct Framework {
    egui_ctx: Context,
    egui_state: egui_winit::State,
    screen_descriptor: ScreenDescriptor,
    renderer: Renderer,
    paint_jobs: Vec<ClippedPrimitive>,
    textures: TexturesDelta,

    gui: Gui,

    /// Last observed GameTextInput buffer text, diffed each frame in
    /// `prepare()` to synthesize egui Text / Backspace events.
    /// android-game-activity's `TextEvent` is dropped by winit 0.29
    /// ("Unknown android_activity input event TextEvent"), so we read
    /// the buffer directly via `AndroidApp::text_input_state()`.
    #[cfg(target_os = "android")]
    last_ime_text: String,
}

impl Framework {
    pub fn new<T>(
        event_loop: &EventLoopWindowTarget<T>,
        width: u32,
        height: u32,
        scale_factor: f32,
        pixels: &pixels::Pixels,
        pending_dialog_result: Option<Arc<Mutex<Option<GuiAction>>>>,
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
        let gui = match pending_dialog_result {
            Some(arc) => Gui::with_pending_dialog_result(arc),
            None => Gui::new(),
        };

        Self {
            egui_ctx,
            egui_state,
            screen_descriptor,
            renderer,
            paint_jobs: Vec::new(),
            textures,
            gui,
            #[cfg(target_os = "android")]
            last_ime_text: String::new(),
        }
    }

    /// Clone of the pending-dialog Arc so the event loop can keep it alive
    /// across `Framework` recreation on Android surface suspend/resume.
    pub fn pending_dialog_result(&self) -> Arc<Mutex<Option<GuiAction>>> {
        self.gui.pending_dialog_result()
    }

    pub fn handle_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        let _ = self.egui_state.on_window_event(window, event);
    }

    /// Latest on-screen Game Boy control state captured by the egui touch
    /// overlay during the most recent `prepare` call.
    pub fn touch_button_state(&self) -> rustyboi_core_lib::input::ButtonState {
        self.gui.touch_button_state()
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

    /// Mutable access to the Android ROM library panel. Used by the
    /// event loop to push tree-URI / scan-results / status text into
    /// the panel from JNI callbacks.
    #[cfg(target_os = "android")]
    pub fn library_panel_mut(
        &mut self,
    ) -> &mut rustyboi_egui_lib::library::LibraryPanel {
        self.gui.library_panel_mut()
    }

    /// Runs the egui frame and returns the resulting action, whether a menu is
    /// open, and the emulator framebuffer's target region in physical pixels
    /// (the egui central region, below the menu bar and above the status panel).
    pub fn prepare(&mut self, window: &Window, paused: bool, registers: Option<&cpu::registers::Registers>, gb: Option<&gb::GB>, session: &rustyboi_egui_lib::actions::SessionUiState) -> (Option<GuiAction>, bool, PhysicalRect) {
        #[cfg_attr(not(target_os = "android"), allow(unused_mut))]
        let mut raw_input = self.egui_state.take_egui_input(window);
        // winit 0.29's android-game-activity backend drops GameTextInput
        // `TextEvent`s (logs "Unknown android_activity input event
        // TextEvent"), so we read the buffer ourselves once per frame
        // and diff it against the last observed value to emit egui
        // events. We deliberately do NOT clear the GameTextInput buffer
        // here: if we did, IME-side backspace would never appear as a
        // shrink in the next poll. Letting the buffer grow/shrink
        // naturally and tracking `last_ime_text` lets us detect both
        // committed text and deletions.
        #[cfg(target_os = "android")]
        if crate::android::ime_initialized() {
            let app = crate::android::android_app();
            let state = app.text_input_state();
            let prev = self.last_ime_text.as_str();
            if state.text != prev {
                let common: usize = prev
                    .chars()
                    .zip(state.text.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                let prev_chars = prev.chars().count();
                let new_chars = state.text.chars().count();
                let backspaces = prev_chars.saturating_sub(common);
                for _ in 0..backspaces {
                    raw_input.events.push(egui::Event::Key {
                        key: egui::Key::Backspace,
                        physical_key: Some(egui::Key::Backspace),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                    raw_input.events.push(egui::Event::Key {
                        key: egui::Key::Backspace,
                        physical_key: Some(egui::Key::Backspace),
                        pressed: false,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                if new_chars > common {
                    let new_text: String = state.text.chars().skip(common).collect();
                    if !new_text.is_empty() {
                        raw_input.events.push(egui::Event::Text(new_text));
                    }
                }
                self.last_ime_text = state.text;
            }
        }
        let mut ui_result = None;
        let full_output = self.egui_ctx.run(raw_input, |egui_ctx| {
            ui_result = Some(self.gui.ui(egui_ctx, paused, registers, gb, session));
        });

        self.textures.append(full_output.textures_delta);
        self.egui_state
            .handle_platform_output(window, full_output.platform_output);

        let ppp = self.screen_descriptor.pixels_per_point;
        self.paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, ppp);

        match ui_result {
            Some(out) => {
                // egui reports the central region in logical points; convert to
                // physical pixels for the wgpu scissor/viewport.
                let c = out.central_rect;
                let region = PhysicalRect {
                    x: c.x * ppp,
                    y: c.y * ppp,
                    width: c.width * ppp,
                    height: c.height * ppp,
                };
                (out.action, out.menu_open, region)
            }
            None => {
                // Fall back to the whole surface if the UI produced nothing.
                let [w, h] = self.screen_descriptor.size_in_pixels;
                (None, false, PhysicalRect { x: 0.0, y: 0.0, width: w as f32, height: h as f32 })
            }
        }
    }

    pub fn render(
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

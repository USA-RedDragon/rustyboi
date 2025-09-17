use crate::config;
use crate::display::gui::{Framework, GuiAction};
use crate::gb;
use crate::ppu;

use winit::dpi::LogicalSize;
use winit::event::{Event,WindowEvent};
use winit::event_loop::EventLoop;
use winit::keyboard::KeyCode;
use winit::window::WindowBuilder;
use winit_input_helper::WinitInputHelper;
use pixels::{Error,Pixels,SurfaceTexture};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 144;

pub fn run_with_gui(gb: gb::GB, config: &config::CleanConfig) -> Result<(), Error> {
    let event_loop = EventLoop::new().unwrap();
    let mut input = WinitInputHelper::new();
    let window = {
        let size = LogicalSize::new((WIDTH * (config.scale as u32)) as f64, (HEIGHT * (config.scale as u32)) as f64);
        WindowBuilder::new()
            .with_title("RustyBoi")
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(WIDTH as f64, HEIGHT as f64))
            .build(&event_loop)
            .unwrap()
    };

    let (mut pixels, mut framework) = {
        let window_size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
        let surface_texture = SurfaceTexture::new(window_size.width, window_size.height, &window);
        let pixels = Pixels::new(WIDTH, HEIGHT, surface_texture)?;
        let framework = Framework::new(
            &event_loop,
            window_size.width,
            window_size.height,
            scale_factor,
            &pixels,
        );

        (pixels, framework)
    };
    let mut world = World::new(gb); 
    let mut manually_paused = false;

    let res = event_loop.run(|event, elwt| {
        if input.update(&event) {
            if input.key_pressed(KeyCode::Escape) || input.close_requested() {
                elwt.exit();
                return;
            }

            // Handle F key for frame stepping when paused
            if input.key_pressed(KeyCode::KeyF) {
                if manually_paused || world.error_state.is_some() {
                    world.step_single_frame = true;
                    window.request_redraw();
                }
            }

            // Handle N key for cycle stepping when paused
            if input.key_pressed(KeyCode::KeyN) {
                if manually_paused || world.error_state.is_some() {
                    world.step_single_cycle = true;
                    window.request_redraw();
                }
            }

            if let Some(scale_factor) = input.scale_factor() {
                framework.scale_factor(scale_factor);
            }

            // Update internal state and request a redraw (only if not resizing)
            world.update();
            window.request_redraw();
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                if let Err(err) = pixels.resize_surface(size.width, size.height) {
                    println!("Failed to resize surface during window event: {}", err);
                    elwt.exit();
                    return;
                }
                framework.resize(size.width, size.height);
            }
            Event::WindowEvent {
                event: WindowEvent::RedrawRequested,
                ..
            } => {
                world.draw(pixels.frame_mut());

                let gui_paused_state = manually_paused || world.error_state.is_some();
                // Always pass register data for the debug overlay, regardless of pause state
                let registers = Some(world.gb.get_cpu_registers());
                let gb_ref = Some(&world.gb);
                let (gui_action, menu_open) = framework.prepare(&window, gui_paused_state, registers, gb_ref);
                
                // Handle GUI actions
                match gui_action {
                    Some(GuiAction::Exit) => {
                        elwt.exit();
                        return;
                    }
                    Some(GuiAction::SaveState(path)) => {
                        match world.save_state(path) {
                            Ok(saved_path) => {
                                framework.set_status(format!("State saved to: {}", saved_path));
                            }
                            Err(e) => {
                                framework.set_error(format!("Failed to save state: {}", e));
                            }
                        }
                    }
                    Some(GuiAction::Restart) => {
                        world.restart();
                        manually_paused = false;
                        framework.clear_error();
                        framework.set_status("Emulation restarted".to_string());
                        window.request_redraw();
                    }
                    Some(GuiAction::ClearError) => {
                        world.clear_error();
                        world.pause();
                        manually_paused = true;
                        framework.clear_error();
                        framework.set_status("Error cleared for debugging - CPU state preserved".to_string());
                        window.request_redraw();
                    }
                    Some(GuiAction::TogglePause) => {
                        manually_paused = !manually_paused;
                        world.toggle_pause();
                    }
                    None => {}
                }

                // Auto-pause when menu is open, but respect manual pause state
                let should_be_paused = manually_paused || menu_open;
                if should_be_paused != world.is_paused {
                    if should_be_paused {
                        world.pause();
                    } else {
                        // Only auto-resume if not manually paused
                        if !manually_paused {
                            world.resume();
                        }
                    }
                }
                if let Some(error_msg) = &world.error_state {
                    framework.set_error(error_msg.clone());
                }

                let render_result = pixels.render_with(|encoder, render_target, context| {
                    context.scaling_renderer.render(encoder, render_target);

                    framework.render(encoder, render_target, context);

                    Ok(())
                });

                if let Err(err) = render_result {
                    println!("Render error: {}", err);
                    window.request_redraw();
                }
            }
            Event::WindowEvent { event, .. } => {
                framework.handle_event(&window, &event);
            }
            _ => (),
        }
    });
    res.map_err(|e| Error::UserDefined(Box::new(e)))
}

struct World {
    gb: gb::GB,
    frame: Option<[u8; ppu::FRAMEBUFFER_SIZE * 4]>,
    error_state: Option<String>,
    is_paused: bool,
    step_single_frame: bool,
    step_single_cycle: bool,
}

impl World {
    fn new(gb: gb::GB) -> Self {
        Self {
            gb,
            frame: None,
            error_state: None,
            is_paused: false,
            step_single_frame: false,
            step_single_cycle: false,
        }
    }

    fn save_state(&self, path: std::path::PathBuf) -> Result<String, std::io::Error> {
        let filename = path.to_string_lossy().to_string();
        self.gb.to_state_file(&filename)?;
        println!("Game state saved to: {}", filename);
        Ok(filename)
    }

    fn toggle_pause(&mut self) {
        self.is_paused = !self.is_paused;
    }

    fn pause(&mut self) {
        self.is_paused = true;
    }

    fn resume(&mut self) {
        self.is_paused = false;
    }

    fn restart(&mut self) {
        // Reset the Game Boy to its initial state
        self.gb.reset();
        
        // Clear any error state
        self.error_state = None;
        
        // Clear the current frame
        self.frame = None;
        
        // Reset pause state
        self.is_paused = false;
    }

    fn clear_error(&mut self) {
        self.error_state = None;
    }

    fn draw(&mut self, frame: &mut [u8]) {
        if let Some(rgba_frame) = &self.frame {
            frame.copy_from_slice(rgba_frame);
            self.frame = None;
        }
    }

    fn update(&mut self) {
        // Handle single frame stepping
        if self.step_single_frame {
            self.step_single_frame = false;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.gb.step_single_frame()
            }));
            match result {
                Ok(frame_data) => {
                    self.frame = Some(convert_to_rgba(&frame_data));
                }
                Err(panic_info) => {
                    let error_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                        format!("Emulator panic during frame step: {}", s)
                    } else if let Some(s) = panic_info.downcast_ref::<String>() {
                        format!("Emulator panic during frame step: {}", s)
                    } else {
                        "Emulator panic during frame step: Unknown error".to_string()
                    };
                    self.error_state = Some(error_msg);
                    self.frame = None;
                }
            }
            return;
        }

        // Handle single cycle stepping
        if self.step_single_cycle {
            self.step_single_cycle = false;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.gb.step_single_cycle();
                // For cycle stepping, we need to get the current frame even if incomplete
                self.gb.get_current_frame()
            }));
            match result {
                Ok(frame_data) => {
                    self.frame = Some(convert_to_rgba(&frame_data));
                }
                Err(panic_info) => {
                    let error_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                        format!("Emulator panic during cycle step: {}", s)
                    } else if let Some(s) = panic_info.downcast_ref::<String>() {
                        format!("Emulator panic during cycle step: {}", s)
                    } else {
                        "Emulator panic during cycle step: Unknown error".to_string()
                    };
                    self.error_state = Some(error_msg);
                    self.frame = None;
                }
            }
            return;
        }

        // Skip updating if we're in an error state or paused
        if self.error_state.is_some() || self.is_paused {
            return;
        }

        // Catch panics from the Game Boy emulator
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.gb.run_until_frame()
        }));

        match result {
            Ok(frame_data) => {
                self.frame = Some(convert_to_rgba(&frame_data));
            }
            Err(panic_info) => {
                // Convert panic info to a string for debugging
                let error_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    format!("Emulator panic: {}", s)
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    format!("Emulator panic: {}", s)
                } else {
                    "Emulator panic: Unknown error".to_string()
                };
                
                self.error_state = Some(error_msg);
                println!("Game Boy emulator crashed: {}", self.error_state.as_ref().unwrap());
                self.frame = None;
            }
        }
    }
}

fn convert_to_rgba(frame: &[u8; ppu::FRAMEBUFFER_SIZE]) -> [u8; ppu::FRAMEBUFFER_SIZE * 4] {
    let mut rgba_frame = [0; ppu::FRAMEBUFFER_SIZE * 4];
    for (i, &pixel) in frame.iter().enumerate() {
        let rgba = match pixel {
            0 => [0xFF, 0xFF, 0xFF, 0xFF], // White
            1 => [0xAA, 0xAA, 0xAA, 0xFF], // Light gray
            2 => [0x55, 0x55, 0x55, 0xFF], // Dark gray
            3 => [0x00, 0x00, 0x00, 0xFF], // Black
            _ => [0xFF, 0x00, 0xFF, 0xFF], // Fallback (magenta)
        };
        let offset = i * 4;
        rgba_frame[offset..offset + 4].copy_from_slice(&rgba);
    }
    rgba_frame
}

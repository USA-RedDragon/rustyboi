use crate::config;
use crate::display::gui::{Framework, GuiAction};
use crate::gb;
use crate::ppu;

use std::time::{Duration, Instant};
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
    let mut world = World::new_with_paths(gb, config.rom.clone(), config.bios.clone(), config.palette); 
    
    // Enable audio output
    if let Err(e) = world.enable_audio() {
        println!("Failed to initialize audio: {}", e);
        println!("Continuing without audio...");
    }
    
    let mut manually_paused = false;
    let mut user_paused = false; // Track user-initiated pause separate from debug pause

    // Debounce timing for F and N keys
    const DEBOUNCE_DURATION: Duration = Duration::from_millis(250); // Time to wait before auto-repeat
    const REPEAT_INTERVAL: Duration = Duration::from_millis(67); // Execute every ~67ms when held (about 15fps)
    let mut f_key_press_time: Option<Instant> = None;
    let mut n_key_press_time: Option<Instant> = None;
    let mut f_key_processed_initial = false;
    let mut n_key_processed_initial = false;
    let mut f_last_repeat_time: Option<Instant> = None;
    let mut n_last_repeat_time: Option<Instant> = None;

    let res = event_loop.run(|event, elwt| {
        if input.update(&event) {
            if input.key_pressed(KeyCode::Escape) || input.close_requested() {
                elwt.exit();
                return;
            }

            // Handle F key for frame stepping with debounce
            if input.key_pressed(KeyCode::KeyF) {
                if manually_paused || world.error_state.is_some() {
                    // Initial press - execute immediately
                    world.step_single_frame = true;
                    let now = Instant::now();
                    f_key_press_time = Some(now);
                    f_last_repeat_time = Some(now);
                    f_key_processed_initial = true;
                    window.request_redraw();
                }
            } else if input.key_held(KeyCode::KeyF) {
                if manually_paused || world.error_state.is_some() {
                    if let Some(press_time) = f_key_press_time {
                        // Check if debounce period has elapsed
                        if press_time.elapsed() >= DEBOUNCE_DURATION {
                            // Check if enough time has passed since last repeat
                            if let Some(last_repeat) = f_last_repeat_time {
                                if last_repeat.elapsed() >= REPEAT_INTERVAL {
                                    world.step_single_frame = true;
                                    f_last_repeat_time = Some(Instant::now());
                                    window.request_redraw();
                                }
                            }
                        }
                    }
                }
            } else {
                // Key released - reset state
                f_key_press_time = None;
                f_key_processed_initial = false;
                f_last_repeat_time = None;
            }

            // Handle N key for cycle stepping with debounce
            if input.key_pressed(KeyCode::KeyN) {
                if manually_paused || world.error_state.is_some() {
                    // Initial press - execute immediately
                    world.step_single_cycle = true;
                    let now = Instant::now();
                    n_key_press_time = Some(now);
                    n_last_repeat_time = Some(now);
                    n_key_processed_initial = true;
                    window.request_redraw();
                }
            } else if input.key_held(KeyCode::KeyN) {
                if manually_paused || world.error_state.is_some() {
                    if let Some(press_time) = n_key_press_time {
                        // Check if debounce period has elapsed
                        if press_time.elapsed() >= DEBOUNCE_DURATION {
                            // Check if enough time has passed since last repeat
                            if let Some(last_repeat) = n_last_repeat_time {
                                if last_repeat.elapsed() >= REPEAT_INTERVAL {
                                    world.step_single_cycle = true;
                                    n_last_repeat_time = Some(Instant::now());
                                    window.request_redraw();
                                }
                            }
                        }
                    }
                }
            } else {
                // Key released - reset state
                n_key_press_time = None;
                n_key_processed_initial = false;
                n_last_repeat_time = None;
            }

            if let Some(scale_factor) = input.scale_factor() {
                framework.scale_factor(scale_factor);
            }

            // Handle Game Boy input based on keybinds
            let a = input.key_held(config.keybinds.a);
            let b = input.key_held(config.keybinds.b);
            let start = input.key_held(config.keybinds.start);
            let select = input.key_held(config.keybinds.select);
            let up = input.key_held(config.keybinds.up);
            let down = input.key_held(config.keybinds.down);
            let left = input.key_held(config.keybinds.left);
            let right = input.key_held(config.keybinds.right);
            
            world.set_input_state(a, b, start, select, up, down, left, right);

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
                
                // Update window title with performance metrics
                world.update_window_title(&window, gui_paused_state);
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
                    Some(GuiAction::LoadState(path)) => {
                        match world.load_state(path) {
                            Ok(loaded_path) => {
                                // Keep user pause state when loading state
                                manually_paused = user_paused || world.error_state.is_some();
                                framework.clear_error();
                                framework.set_status(format!("State loaded from: {}", loaded_path));
                                window.request_redraw();
                            }
                            Err(e) => {
                                framework.set_error(format!("Failed to load state: {}", e));
                            }
                        }
                    }
                    Some(GuiAction::LoadRom(path)) => {
                        match world.load_rom(path) {
                            Ok(loaded_path) => {
                                // Keep user pause state when loading ROM
                                manually_paused = user_paused;
                                framework.clear_error();
                                framework.set_status(format!("ROM loaded from: {}", loaded_path));
                                window.request_redraw();
                            }
                            Err(e) => {
                                framework.set_error(format!("Failed to load ROM: {}", e));
                            }
                        }
                    }
                    Some(GuiAction::Restart) => {
                        world.restart();
                        // Keep user pause state when restarting
                        manually_paused = user_paused;
                        framework.clear_error();
                        framework.set_status("Emulation restarted".to_string());
                        window.request_redraw();
                    }
                    Some(GuiAction::ClearError) => {
                        world.clear_error();
                        world.pause();
                        // Update manually_paused to reflect only user pause state after clearing error
                        manually_paused = user_paused;
                        framework.clear_error();
                        framework.set_status("Error cleared for debugging - CPU state preserved".to_string());
                        window.request_redraw();
                    }
                    Some(GuiAction::TogglePause) => {
                        user_paused = !user_paused;
                        manually_paused = user_paused || world.error_state.is_some();
                        world.toggle_pause();
                    }
                    Some(GuiAction::StepCycles(count)) => {
                        world.step_multiple_cycles = Some(count);
                        window.request_redraw();
                    }
                    Some(GuiAction::StepFrames(count)) => {
                        world.step_multiple_frames = Some(count);
                        window.request_redraw();
                    }
                    Some(GuiAction::SetBreakpoint(address)) => {
                        world.add_breakpoint(address);
                        framework.set_status(format!("Breakpoint set at ${:04X}", address));
                        window.request_redraw();
                    }
                    Some(GuiAction::RemoveBreakpoint(address)) => {
                        world.remove_breakpoint(address);
                        framework.set_status(format!("Breakpoint removed from ${:04X}", address));
                        window.request_redraw();
                    }
                    None => {}
                }

                // Auto-pause when menu is open, but respect manual pause state
                let should_be_paused = manually_paused || menu_open;
                if should_be_paused != world.is_paused {
                    if should_be_paused {
                        world.pause();
                    } else {
                        // Only auto-resume if not manually paused and no error
                        if !user_paused && world.error_state.is_none() {
                            world.resume();
                        }
                    }
                }

                // Check for breakpoint hits and notify user
                if world.check_and_clear_breakpoint_hit() {
                    let pc = world.gb.get_cpu_registers().pc;
                    manually_paused = true; // Ensure we stay paused
                    user_paused = true; // User should explicitly resume
                    framework.set_status(format!("Breakpoint hit at PC: ${:04X}", pc));
                }

                if let Some(error_msg) = &world.error_state {
                    framework.set_error(error_msg.clone());
                    // Update manually_paused to include error state
                    manually_paused = user_paused || world.error_state.is_some();
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
    step_multiple_cycles: Option<u32>,
    step_multiple_frames: Option<u32>,
    current_rom_path: Option<String>,
    current_bios_path: Option<String>,
    // FPS and performance tracking
    frame_times: Vec<Instant>,
    last_title_update: Instant,
    // Frame timing for 60fps
    last_frame_time: Instant,
    // Breakpoint status
    breakpoint_hit: bool,
    // Color palette
    palette: config::ColorPalette,
}

impl World {
    fn new_with_paths(gb: gb::GB, rom_path: Option<String>, bios_path: Option<String>, palette: config::ColorPalette) -> Self {
        let now = Instant::now();
        Self {
            gb,
            frame: None,
            error_state: None,
            is_paused: false,
            step_single_frame: false,
            step_single_cycle: false,
            step_multiple_cycles: None,
            step_multiple_frames: None,
            current_rom_path: rom_path,
            current_bios_path: bios_path,
            frame_times: Vec::with_capacity(60), // Store last 60 frame times for FPS calculation
            last_title_update: now,
            last_frame_time: now,
            breakpoint_hit: false,
            palette,
        }
    }

    fn save_state(&self, path: std::path::PathBuf) -> Result<String, std::io::Error> {
        let filename = path.to_string_lossy().to_string();
        self.gb.to_state_file(&filename)?;
        println!("Game state saved to: {}", filename);
        Ok(filename)
    }

    fn enable_audio(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.gb.enable_audio()
    }

    fn disable_audio(&mut self) {
        self.gb.disable_audio();
    }

    fn set_audio_volume(&mut self, volume: f32) {
        self.gb.set_audio_volume(volume);
    }

    fn load_state(&mut self, path: std::path::PathBuf) -> Result<String, Box<dyn std::error::Error>> {
        let filename = path.to_string_lossy().to_string();
        
        // Save the current ROM and BIOS paths before loading state
        let saved_rom_path = self.current_rom_path.clone();
        let saved_bios_path = self.current_bios_path.clone();
        
        // Load the new state
        self.gb = crate::gb::GB::from_state_file(&filename)?;
        
        // Reload the ROM if we had one loaded
        if let Some(rom_path) = saved_rom_path {
            match crate::cartridge::Cartridge::load(&rom_path) {
                Ok(cartridge) => {
                    self.gb.insert(cartridge);
                    self.current_rom_path = Some(rom_path);
                    println!("Reloaded ROM: {}", self.current_rom_path.as_ref().unwrap());
                }
                Err(e) => {
                    println!("Warning: Failed to reload ROM {}: {}", rom_path, e);
                    self.current_rom_path = None;
                }
            }
        }
        
        // Reload the BIOS if we had one loaded
        if let Some(bios_path) = saved_bios_path {
            match self.gb.load_bios(&bios_path) {
                Ok(_) => {
                    self.current_bios_path = Some(bios_path);
                    println!("Reloaded BIOS: {}", self.current_bios_path.as_ref().unwrap());
                }
                Err(e) => {
                    println!("Warning: Failed to reload BIOS {}: {}", bios_path, e);
                    self.current_bios_path = None;
                }
            }
        }
        
        // Clear any error state
        self.error_state = None;
        
        // Clear the current frame
        self.frame = None;
        
        // Reset pause state
        self.is_paused = false;
        
        println!("Game state loaded from: {}", filename);
        Ok(filename)
    }

    fn load_rom(&mut self, path: std::path::PathBuf) -> Result<String, Box<dyn std::error::Error>> {
        let filename = path.to_string_lossy().to_string();
        let cartridge = crate::cartridge::Cartridge::load(&filename)?;
        self.gb.insert(cartridge);
        
        // Track the current ROM path
        self.current_rom_path = Some(filename.clone());
        
        // Reset the emulator to a clean state after loading the ROM
        self.gb.reset();
        
        // Clear any error state
        self.error_state = None;
        
        // Clear the current frame
        self.frame = None;
        
        // Reset pause state
        self.is_paused = false;
        
        println!("ROM loaded from: {}", filename);
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

    fn run_until_frame(&mut self) -> Option<[u8; ppu::FRAMEBUFFER_SIZE * 4]> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Collect audio when running frames
            self.gb.run_until_frame(true)
        }));

        match result {
            Ok((frame_data, _audio_samples, _breakpoint_hit)) => Some(convert_to_rgba(&frame_data, &self.palette)),
            Err(panic_info) => {
                // Convert panic info to a string for debugging
                let error_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    format!("Emulator panic: {}", s)
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    format!("Emulator panic: {}", s)
                } else {
                    "Emulator panic: Unknown error".to_string()
                };

                println!("Game Boy emulator crashed: {}", error_msg);
                None
            }
        }
    }

    fn run_until_frame_with_breakpoints(&mut self) -> (Option<[u8; ppu::FRAMEBUFFER_SIZE * 4]>, bool) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Collect audio when running frames
            self.gb.run_until_frame(true)
        }));

        match result {
            Ok((frame_data, _audio_samples, breakpoint_hit)) => (Some(convert_to_rgba(&frame_data, &self.palette)), breakpoint_hit),
            Err(panic_info) => {
                // Convert panic info to a string for debugging
                let error_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    format!("Emulator panic: {}", s)
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    format!("Emulator panic: {}", s)
                } else {
                    "Emulator panic: Unknown error".to_string()
                };

                println!("Game Boy emulator crashed: {}", error_msg);
                (None, false)
            }
        }
    }

    fn update(&mut self) {
        // Handle single frame stepping
        if self.step_single_frame {
            self.step_single_frame = false;
            match self.run_until_frame() {
                Some(frame_data) => {
                    self.frame = Some(frame_data);
                }
                None => {
                    self.error_state = Some("Emulator crashed during frame step".to_string());
                    self.frame = None;
                }
            }
            return;
        }

        // Handle single cycle stepping
        if self.step_single_cycle {
            self.step_single_cycle = false;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Collect audio for cycle stepping too
                let (_audio_samples, _breakpoint_hit) = self.gb.step_instruction(true);
                // For cycle stepping, we need to get the current frame even if incomplete
                self.gb.get_current_frame()
            }));
            match result {
                Ok(frame_data) => {
                    self.frame = Some(convert_to_rgba(&frame_data, &self.palette));
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

        // Handle multiple cycle stepping
        if let Some(count) = self.step_multiple_cycles.take() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for _ in 0..count {
                    // Collect audio for cycle stepping
                    let (_audio_samples, _breakpoint_hit) = self.gb.step_instruction(true);
                }
                self.gb.get_current_frame()
            }));
            match result {
                Ok(frame_data) => {
                    self.frame = Some(convert_to_rgba(&frame_data, &self.palette));
                }
                Err(panic_info) => {
                    let error_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                        format!("Emulator panic during multi-cycle step ({}): {}", count, s)
                    } else if let Some(s) = panic_info.downcast_ref::<String>() {
                        format!("Emulator panic during multi-cycle step ({}): {}", count, s)
                    } else {
                        format!("Emulator panic during multi-cycle step ({}): Unknown error", count)
                    };
                    self.error_state = Some(error_msg);
                    self.frame = None;
                }
            }
            return;
        }

        // Handle multiple frame stepping
        if let Some(count) = self.step_multiple_frames.take() {
            let mut success = true;
            let mut final_frame = None;
            
            for _ in 0..count {
                match self.run_until_frame() {
                    Some(_) => {}, // Continue to next frame
                    None => {
                        success = false;
                        break;
                    }
                }
            }
            
            if success {
                final_frame = Some(self.gb.get_current_frame());
            }
            
            match final_frame {
                Some(frame_data) => {
                    self.frame = Some(convert_to_rgba(&frame_data, &self.palette));
                }
                None => {
                    self.error_state = Some(format!("Emulator crashed during multi-frame step ({})", count));
                    self.frame = None;
                }
            }
            return;
        }

        // Skip updating if we're in an error state or paused
        if self.error_state.is_some() || self.is_paused {
            return;
        }

        // Frame timing: target 60fps (16.75ms per frame)
        const TARGET_FRAME_TIME: Duration = Duration::from_micros(16750); // ~59.7 fps
        let now = Instant::now();
        let elapsed_since_last_frame = now.duration_since(self.last_frame_time);
        
        // Only update if enough time has passed
        if elapsed_since_last_frame < TARGET_FRAME_TIME {
            let remaining = TARGET_FRAME_TIME - elapsed_since_last_frame;
            // Sleep for most of the remaining time
            if remaining > Duration::from_micros(100) {
                std::thread::sleep(remaining - Duration::from_micros(50));
            }
            // Spin for precision
            while self.last_frame_time.elapsed() < TARGET_FRAME_TIME {
                std::hint::spin_loop();
            }
        }
        
        self.last_frame_time = Instant::now();

        // Use breakpoint-aware version if we have any breakpoints set
        if self.gb.get_breakpoints().is_empty() {
            // No breakpoints - use regular version for better performance
            match self.run_until_frame() {
                Some(frame_data) => {
                    self.frame = Some(frame_data);
                    self.update_performance_metrics();
                }
                None => {
                    self.error_state = Some("Emulator crashed".to_string());
                    println!("Game Boy emulator crashed: {}", self.error_state.as_ref().unwrap());
                    self.frame = None;
                }
            }
        } else {
            // We have breakpoints - use breakpoint-aware version
            let (frame_result, breakpoint_hit) = self.run_until_frame_with_breakpoints();
            match frame_result {
                Some(frame_data) => {
                    self.frame = Some(frame_data);
                    self.update_performance_metrics();
                    
                    // If a breakpoint was hit, pause emulation
                    if breakpoint_hit {
                        self.is_paused = true;
                        self.breakpoint_hit = true;
                        println!("Breakpoint hit at PC: {:04X}", self.gb.get_cpu_registers().pc);
                    }
                }
                None => {
                    self.error_state = Some("Emulator crashed".to_string());
                    println!("Game Boy emulator crashed: {}", self.error_state.as_ref().unwrap());
                    self.frame = None;
                }
            }
        }
    }

    fn update_performance_metrics(&mut self) {
        let now = Instant::now();
        
        // Track frame times for FPS calculation
        self.frame_times.push(now);
        
        // Keep only the last 60 frame times (1 second at 60 FPS)
        if self.frame_times.len() > 60 {
            self.frame_times.remove(0);
        }
    }

    fn get_fps(&self) -> f64 {
        let frame_count = self.frame_times.len();
        if frame_count < 2 {
            return 0.0;
        }
        
        let duration = self.frame_times[frame_count - 1].duration_since(self.frame_times[0]);
        if duration.as_secs_f64() == 0.0 {
            return 0.0;
        }
        
        (frame_count as f64 - 1.0) / duration.as_secs_f64()
    }

    fn update_window_title(&mut self, window: &winit::window::Window, is_paused: bool) {
        let now = Instant::now();
        
        // Update title every 500ms to avoid excessive updates
        if now.duration_since(self.last_title_update).as_millis() >= 500 {
            let fps = self.get_fps();
            
            let title = if self.error_state.is_some() {
                format!("RustyBoi - ERROR | {:.1} FPS", fps)
            } else if is_paused {
                format!("RustyBoi - PAUSED | {:.1} FPS", fps)
            } else {
                format!("RustyBoi | {:.1} FPS", fps)
            };
            
            window.set_title(&title);
            self.last_title_update = now;
        }
    }

    fn set_input_state(&mut self, a: bool, b: bool, start: bool, select: bool, up: bool, down: bool, left: bool, right: bool) {
        self.gb.set_input_state(a, b, start, select, up, down, left, right);
    }

    // Breakpoint management methods
    fn add_breakpoint(&mut self, address: u16) {
        self.gb.add_breakpoint(address);
    }

    fn remove_breakpoint(&mut self, address: u16) {
        self.gb.remove_breakpoint(address);
    }

    fn check_and_clear_breakpoint_hit(&mut self) -> bool {
        let hit = self.breakpoint_hit;
        self.breakpoint_hit = false;
        hit
    }
}

fn convert_to_rgba(frame: &[u8; ppu::FRAMEBUFFER_SIZE], palette: &config::ColorPalette) -> [u8; ppu::FRAMEBUFFER_SIZE * 4] {
    let mut rgba_frame = [0; ppu::FRAMEBUFFER_SIZE * 4];
    let colors = palette.get_rgba_colors();
    
    for (i, &pixel) in frame.iter().enumerate() {
        let rgba = colors.get(pixel as usize).unwrap_or(&colors[3]);
        let offset = i * 4;
        rgba_frame[offset..offset + 4].copy_from_slice(rgba);
    }
    rgba_frame
}

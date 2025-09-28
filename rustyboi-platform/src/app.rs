use egui_wgpu::wgpu::SurfaceError;
use egui_wgpu::{wgpu, ScreenDescriptor};
use std::sync::Arc;
use web_time::{Instant, Duration};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::KeyCode;
use winit::window::{Window, WindowId};

use crate::config;
use crate::framework::Framework;
use crate::input::InputHandler;
use crate::renderer::WgpuRenderer;
use rustyboi_egui_lib::{actions::GuiAction, actions::FileData, Gui};
use rustyboi_core_lib::{cartridge, gb, ppu, input};

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use winit::platform::web::WindowExtWebSys;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 144;

struct World {
    gb: gb::GB,
    frame: Option<gb::Frame>,
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
    // Track if emulator was auto-paused due to missing ROM/BIOS
    auto_paused_no_content: bool,
}

impl World {
    fn new(gb: gb::GB, config: Option<config::CleanConfig>) -> Self {
        let (rom_path, bios_path, palette) = if let Some(cfg) = config {
            (cfg.rom, cfg.bios, cfg.palette)
        } else {
            (None, None, config::ColorPalette::default())
        };
        
        Self::new_with_paths(gb, rom_path, bios_path, palette)
    }

    fn new_with_paths(gb: gb::GB, rom_path: Option<String>, bios_path: Option<String>, palette: config::ColorPalette) -> Self {
        let now = Instant::now();
        
        // Check if both ROM and BIOS are missing - if so, start paused
        let should_start_paused = !gb.has_rom() && !gb.has_bios();
        
        Self {
            gb,
            frame: None,
            error_state: None,
            is_paused: should_start_paused,
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
            auto_paused_no_content: should_start_paused,
        }
    }

    fn save_state(&self, path: std::path::PathBuf) -> Result<String, std::io::Error> {
        let filename = path.to_string_lossy().to_string();
        self.gb.to_state_file(&filename)?;
        println!("Game state saved to: {}", filename);
        Ok(filename)
    }

    fn enable_audio(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let output = Box::new(crate::audio::Output::new()?);
        self.gb.enable_audio(output)
    }

    fn load_state(&mut self, file_data: FileData) -> Result<String, Box<dyn std::error::Error>> {
        // Save the current ROM and BIOS paths before loading state
        let saved_rom_path = self.current_rom_path.clone();
        let saved_bios_path = self.current_bios_path.clone();
        
        // Load the new state and get filename
        let filename = match file_data {
            #[cfg(not(target_arch = "wasm32"))]
            FileData::Path(path) => {
                let filename = path.to_string_lossy().to_string();
                self.gb = gb::GB::from_state_file(&filename)?;
                filename
            }
            #[cfg(target_arch = "wasm32")]
            FileData::Contents { name, data } => {
                // For WASM, parse the data directly
                let saved_state = String::from_utf8(data)?;
                self.gb = serde_json::from_str(&saved_state)?;
                name
            }
        };
        
        // Reload the ROM if we had one loaded
        if let Some(rom_path) = saved_rom_path {
            match cartridge::Cartridge::load(&rom_path) {
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
        
        // If emulator was auto-paused due to no content and state has content, unpause it
        if self.auto_paused_no_content && (self.gb.has_rom() || self.gb.has_bios()) {
            self.is_paused = false;
            self.auto_paused_no_content = false;
        }
        
        println!("Game state loaded from: {}", filename);
        Ok(filename)
    }

    fn load_rom(&mut self, file_data: FileData) -> Result<String, Box<dyn std::error::Error>> {
        let (filename, cartridge, has_file_path) = match file_data {
            #[cfg(not(target_arch = "wasm32"))]
            FileData::Path(path) => {
                let filename = path.to_string_lossy().to_string();
                let cartridge = cartridge::Cartridge::load(&filename)?;
                (filename, cartridge, true)
            }
            #[cfg(target_arch = "wasm32")]
            FileData::Contents { name, data } => {
                // For WASM, load from memory
                let cartridge = cartridge::Cartridge::from_bytes(&data)?;
                (name, cartridge, false)
            }
        };
        self.gb.insert(cartridge);
        
        // Track the current ROM path
        self.current_rom_path = if has_file_path {
            Some(filename.clone())
        } else {
            None // No file path for WASM content
        };
        
        // Reset the emulator to a clean state after loading the ROM
        self.gb.reset();
        
        // Clear any error state
        self.error_state = None;
        
        // Clear the current frame
        self.frame = None;
        
        // If emulator was auto-paused due to no content, unpause it now
        if self.auto_paused_no_content {
            self.is_paused = false;
            self.auto_paused_no_content = false;
        }
        
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

    /// Check if the emulator should be automatically unpaused due to ROM loading
    fn should_auto_unpause(&self) -> bool {
        !self.auto_paused_no_content && !self.is_paused
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

    fn draw(&mut self, frame: &mut [u8]) -> bool {
        if let Some(gb_frame) = self.frame.as_ref() {
            let rgba_frame = match gb_frame {
                gb::Frame::Monochrome(data) => {
                    // Convert monochrome framebuffer to RGBA using the palette
                    convert_to_rgba(&data, &self.palette)
                }
                gb::Frame::Color(data) => {
                    // Convert color framebuffer (RGB888) to RGBA8888
                    let mut rgba = [0u8; ppu::FRAMEBUFFER_SIZE * 4];
                    for (i, chunk) in data.chunks(3).enumerate() {
                        let offset = i * 4;
                        rgba[offset] = chunk[0];     // R
                        rgba[offset + 1] = chunk[1]; // G
                        rgba[offset + 2] = chunk[2]; // B
                        rgba[offset + 3] = 255;      // A
                    }
                    rgba
                }
                
            };
            frame.copy_from_slice(&rgba_frame);
            self.frame = None;
            true
        } else {
            false
        }
    }

    fn run_until_frame(&mut self) -> Option<gb::Frame> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Collect audio when running frames
            self.gb.run_until_frame(true)
        }));

        match result {
            Ok((frame, _breakpoint_hit)) => Some(frame),
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

    fn run_until_frame_with_breakpoints(&mut self) -> (Option<gb::Frame>, bool) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Collect audio when running frames
            self.gb.run_until_frame(true)
        }));

        match result {
            Ok((frame, breakpoint_hit)) => (Some(frame), breakpoint_hit),
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
                Some(frame) => {
                    self.frame = Some(frame);
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
                let (_breakpoint_hit, _cycles) = self.gb.step_instruction(true);
                // For cycle stepping, we need to get the current frame even if incomplete
                self.gb.get_current_frame()
            }));
            match result {
                Ok(frame) => {
                    self.frame = Some(frame);
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
                    let (_breakpoint_hit, _cycles) = self.gb.step_instruction(true);
                }
                self.gb.get_current_frame()
            }));
            match result {
                Ok(frame) => {
                    self.frame = Some(frame);
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
                Some(frame) => {
                    self.frame = Some(frame);
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
            
            
            // Sleep for most of the remaining time (not supported in WASM)
            #[cfg(not(target_arch = "wasm32"))]
            if remaining > Duration::from_micros(100) {
                std::thread::sleep(remaining - Duration::from_micros(50));
            }
            // Spin for precision
            while self.last_frame_time.elapsed() < TARGET_FRAME_TIME {
                std::hint::spin_loop();
            }
        } else if elapsed_since_last_frame.as_millis() > 25 {
            // Frame took too long
            println!("Slow frame: {}ms (target: {}ms)", 
                    elapsed_since_last_frame.as_millis(), 
                    TARGET_FRAME_TIME.as_millis());
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

    fn set_input_state(&mut self, state: input::ButtonState) {
        self.gb.set_input_state(state);
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

pub struct AppState {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub surface: wgpu::Surface<'static>,
    pub scale_factor: f32,
    pub egui_renderer: EguiRenderer,
}

impl AppState {
    async fn new(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'static>,
        window: &Window,
        width: u32,
        height: u32,
    ) -> Self {
        let power_pref = wgpu::PowerPreference::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power_pref,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .expect("Failed to find an appropriate adapter");

        let features = wgpu::Features::empty();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    required_features: features,
                    required_limits: Default::default(),
                    memory_hints: Default::default(),
                    trace: Default::default()
                }
            )
            .await
            .expect("Failed to create device");

        let swapchain_capabilities = surface.get_capabilities(&adapter);
        
        // For WASM/WebGPU, use the first available format instead of hardcoding
        let swapchain_format = if swapchain_capabilities.formats.is_empty() {
            // Fallback to a common format
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            // Prefer Bgra8UnormSrgb if available, otherwise use the first supported format
            swapchain_capabilities
                .formats
                .iter()
                .find(|&&format| format == wgpu::TextureFormat::Bgra8UnormSrgb)
                .copied()
                .unwrap_or(swapchain_capabilities.formats[0])
        };

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: swapchain_format,
            width,
            height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 0,
            alpha_mode: swapchain_capabilities.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &surface_config);

        let egui_renderer = EguiRenderer::new(&device, surface_config.format, None, 1, window);

        let scale_factor = 1.0;

        Self {
            device,
            queue,
            surface,
            surface_config,
            egui_renderer,
            scale_factor,
        }
    }

    fn resize_surface(&mut self, width: u32, height: u32) {
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }
}

pub struct App {
    instance: wgpu::Instance,
    state: Option<AppState>,
    window: Option<Arc<Window>>,
    #[cfg(target_arch = "wasm32")]
    initializing: bool,
    #[cfg(target_arch = "wasm32")]
    shared_state: Rc<RefCell<Option<AppState>>>,
    // Game Boy emulator components
    renderer: Option<WgpuRenderer>,
    framework: Option<Framework>,
    gui: Option<Gui>,
    world: Option<World>,
    // Game Boy screen texture for egui rendering
    gameboy_texture_handle: Option<egui::TextureHandle>,
    gameboy_has_rendered_frame: bool,
    input: Option<InputHandler>,
    config: Option<config::CleanConfig>,
    last_frame_time: Instant,
    // Debounce timing for F and N keys
    f_key_press_time: Option<Instant>,
    n_key_press_time: Option<Instant>,
    f_key_processed_initial: bool,
    n_key_processed_initial: bool,
    f_last_repeat_time: Option<Instant>,
    n_last_repeat_time: Option<Instant>,
    manually_paused: bool,
    user_paused: bool,
}

impl App {
    pub fn new() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            // Create default config and Game Boy for WASM
            let config = config::CleanConfig::default();
            let mut gb = gb::GB::new(config.hardware);
            gb.skip_bios();
            let world = World::new(gb, Some(config.clone()));
            let input = InputHandler::new();
            
            let instance = egui_wgpu::wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
            Self {
                instance,
                state: None,
                window: None,
                initializing: false,
                shared_state: Rc::new(RefCell::new(None)),
                renderer: None,
                framework: None,
                gui: Some(Gui::new()),
                world: Some(world),
                gameboy_texture_handle: None,
                gameboy_has_rendered_frame: false,
                input: Some(input),
                config: Some(config),
                last_frame_time: Instant::now(),
                f_key_press_time: None,
                n_key_press_time: None,
                f_key_processed_initial: false,
                n_key_processed_initial: false,
                f_last_repeat_time: None,
                n_last_repeat_time: None,
                manually_paused: false,
                user_paused: false,
            }
        }
        
        #[cfg(not(target_arch = "wasm32"))]
        {
            let instance = egui_wgpu::wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
            Self {
                instance,
                state: None,
                window: None,
                renderer: None,
                framework: None,
                gui: None,
                world: None,
                gameboy_texture_handle: None,
                gameboy_has_rendered_frame: false,
                input: None,
                config: None,
                last_frame_time: Instant::now(),
                f_key_press_time: None,
                n_key_press_time: None,
                f_key_processed_initial: false,
                n_key_processed_initial: false,
                f_last_repeat_time: None,
                n_last_repeat_time: None,
                manually_paused: false,
                user_paused: false,
            }
        }
    }

    pub fn new_with_gb(gb: gb::GB, config: config::CleanConfig) -> Self {
        let instance = egui_wgpu::wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        
        // Create World instance
        let world = World::new_with_paths(gb, config.rom.clone(), config.bios.clone(), config.palette);
        let should_start_paused = world.is_paused;
        
        Self {
            instance,
            state: None,
            window: None,
            #[cfg(target_arch = "wasm32")]
            initializing: false,
            #[cfg(target_arch = "wasm32")]
            shared_state: Rc::new(RefCell::new(None)),
            renderer: None,
            framework: None,
            gui: Some(Gui::new()),
            world: Some(world),
            gameboy_texture_handle: None,
            gameboy_has_rendered_frame: false,
            input: Some(InputHandler::new()),
            config: Some(config),
            last_frame_time: Instant::now(),
            f_key_press_time: None,
            n_key_press_time: None,
            f_key_processed_initial: false,
            n_key_processed_initial: false,
            f_last_repeat_time: None,
            n_last_repeat_time: None,
            manually_paused: should_start_paused,
            user_paused: should_start_paused,
        }
    }

    async fn set_window(&mut self, window: Window) {
        let window = Arc::new(window);
        
        let (initial_width, initial_height) = {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let scale = self.config.as_ref().map(|c| c.scale as u32).unwrap_or(7);
                let width = 160 * scale;
                let height = 144 * scale;
                let _ = window.request_inner_size(PhysicalSize::new(width, height));
                (width, height)
            }
            #[cfg(target_arch = "wasm32")]
            {
                // For WASM, don't request window size - let CSS handle it
                // Just use the current window size
                let current_size = window.inner_size();
                (current_size.width, current_size.height)
            }
        };

        let surface = self
            .instance
            .create_surface(window.clone())
            .expect("Failed to create surface!");

        let state = AppState::new(
            &self.instance,
            surface,
            &window,
            initial_width,
            initial_height,
        )
        .await;

        // Game Boy rendering will be handled directly in handle_redraw

        self.window.get_or_insert(window);
        self.state.get_or_insert(state);

        // Trigger a resize event now that the WASM state is ready
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Ok(event) = web_sys::Event::new("resize") {
                    let _ = window.dispatch_event(&event);
                    web_sys::console::log_1(&"WASM state ready - triggered resize event".into());
                }
            }
        }

        // Enable audio if we have a world
        if let Some(world) = &mut self.world {
            if let Err(e) = world.enable_audio() {
                println!("Failed to initialize audio: {}", e);
                println!("Continuing without audio...");
            }
        }
    }


    fn handle_resized(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            if let Some(state) = self.state.as_mut() {
                state.resize_surface(width, height);
            }
        }
    }

    fn handle_redraw(&mut self) {
        // Attempt to handle minimizing window
        if let Some(window) = self.window.as_ref() {
            if let Some(min) = window.is_minimized() {
                if min {
                    println!("Window is minimized");
                    return;
                }
            }
        }

        // For WASM, check if async initialization is complete
        #[cfg(target_arch = "wasm32")]
        if self.state.is_none() {
            // Check if the shared state has been initialized
            if let Ok(shared_state) = self.shared_state.try_borrow() {
                if shared_state.is_some() {
                    // Move the state from shared to local
                    drop(shared_state);
                    if let Ok(mut shared_state) = self.shared_state.try_borrow_mut() {
                        if let Some(state) = shared_state.take() {
                            self.state = Some(state);
                            
                            // Trigger a resize event now that the WASM state is ready
                            if let Some(window) = web_sys::window() {
                                if let Ok(event) = web_sys::Event::new("resize") {
                                    let _ = window.dispatch_event(&event);
                                    web_sys::console::log_1(&"WASM state ready (shared) - triggered resize event".into());
                                }
                            }
                        }
                    }
                }
            }
            
            // If still no state, skip rendering this frame
            if self.state.is_none() {
                web_sys::console::log_1(&"WASM state not ready, skipping frame".into());
                return;
            }
        }

        let state = match self.state.as_mut() {
            Some(state) => state,
            None => return, // No state available, skip rendering
        };

        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [state.surface_config.width, state.surface_config.height],
            pixels_per_point: self.window.as_ref().unwrap().scale_factor() as f32
                * state.scale_factor,
        };

        let window = self.window.as_ref().unwrap();

        // Game Boy emulator rendering
            
        let gui_action = if self.gui.is_some() {
            // Update Game Boy emulation
            if let Some(world) = &mut self.world {
                world.update();
            }

            // Game Boy framebuffer will be handled in egui rendering section

            let gui_paused_state = if let Some(world) = &self.world {
                self.manually_paused || world.error_state.is_some()
            } else {
                true
            };
            
            // Update window title with performance metrics
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(world) = &mut self.world {
                world.update_window_title(window, gui_paused_state);
            }
            
            // Prepare GUI using rustyboi-egui::Gui
            let gui_result = if let Some(gui) = &mut self.gui {
                // Start egui frame
                state.egui_renderer.begin_frame(window);
                
                // Set dark theme to avoid white backgrounds
                state.egui_renderer.context().set_visuals(egui::Visuals::dark());
                
                // Render GUI and get actions (with optional Game Boy data)
                let (registers, gb_ref) = if let Some(world) = &self.world {
                    (Some(world.gb.get_cpu_registers()), Some(&world.gb))
                } else {
                    (None, None)
                };
                
                let (gui_action, any_menu_open) = gui.ui(state.egui_renderer.context(), gui_paused_state, registers, gb_ref);
                
                // Render Game Boy screen
                if let Some(world_mut) = &mut self.world {
                    // Get the Game Boy framebuffer
                    let mut frame_buffer = vec![0u8; WIDTH as usize * HEIGHT as usize * 4];
                    let has_new_frame = world_mut.draw(&mut frame_buffer);
                    
                    // Only create and update texture when we have a new frame
                    if has_new_frame {
                        // Convert framebuffer to egui image
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [WIDTH as usize, HEIGHT as usize],
                            &frame_buffer,
                        );
                        
                        // Always create a new texture handle to avoid any white initialization
                        // This ensures we only ever show textures with our actual image data
                        let texture_handle = state.egui_renderer.context().load_texture(
                            "gameboy_screen", 
                            image,
                            egui::TextureOptions::NEAREST,
                        );
                        self.gameboy_texture_handle = Some(texture_handle);
                        self.gameboy_has_rendered_frame = true;
                    }
                    
                    // Only render Game Boy screen if we have both a texture AND have rendered at least one frame
                    if let Some(texture_handle) = &self.gameboy_texture_handle {
                        if self.gameboy_has_rendered_frame {
                        egui::CentralPanel::default()
                            .frame(egui::Frame::NONE.fill(egui::Color32::TRANSPARENT))
                            .show(state.egui_renderer.context(), |ui| {
                                // Get available space and calculate scale to fit
                                let available_rect = ui.available_rect_before_wrap();
                                let config_scale = self.config.as_ref().map(|c| c.scale as f32).unwrap_or(5.0);
                                
                                // Calculate ideal size based on config scale
                                let ideal_width = WIDTH as f32 * config_scale;
                                let ideal_height = HEIGHT as f32 * config_scale;
                                
                                // Scale down if needed to fit in available space, but keep aspect ratio
                                let scale_x = available_rect.width() / ideal_width;
                                let scale_y = available_rect.height() / ideal_height;
                                let final_scale = scale_x.min(scale_y).min(1.0) * config_scale;
                                
                                let final_size = egui::Vec2::new(
                                    WIDTH as f32 * final_scale,
                                    HEIGHT as f32 * final_scale,
                                );
                                
                                ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::TopDown), |ui| {
                                    ui.add_sized(
                                        final_size,
                                        egui::Image::new(texture_handle)
                                            .fit_to_exact_size(final_size)
                                    );
                                });
                            });
                        }
                    }
                }                (gui_action, any_menu_open)
            } else {
                // If no world or gui, still start egui frame for fallback UI
                state.egui_renderer.begin_frame(window);
                
                egui::Window::new("RustyBoi")
                    .resizable(true)
                    .default_open(true)
                    .show(state.egui_renderer.context(), |ui| {
                        ui.label("Game Boy emulator ready");
                        ui.label("Load a ROM to start playing");
                    });
                
                (None, false)
            };
            
            let (gui_action, menu_open) = gui_result;

            // Auto-pause when menu is open, but respect manual pause state
            let should_be_paused = self.manually_paused || menu_open;
            if let Some(world) = &mut self.world {
                if should_be_paused != world.is_paused {
                    if should_be_paused {
                        world.pause();
                    } else {
                        // Only auto-resume if not manually paused, no error, AND not auto-paused due to no content
                        if !self.user_paused && world.error_state.is_none() && !world.auto_paused_no_content {
                            world.resume();
                        }
                    }
                }

                // Check for breakpoint hits and notify user
                if world.check_and_clear_breakpoint_hit() {
                    self.manually_paused = true; // Ensure we stay paused
                    self.user_paused = true; // User should explicitly resume
                    // The GUI component will handle showing this in its UI
                }

                // Update manually_paused to include error state
                self.manually_paused = self.user_paused || world.error_state.is_some();
            }

            // Render Game Boy framebuffer if available
            if let (Some(world), Some(renderer)) = (&mut self.world, &mut self.renderer) {
                let mut frame_buffer = vec![0u8; WIDTH as usize * HEIGHT as usize * 4];
                let has_new_frame = world.draw(&mut frame_buffer);
                
                if has_new_frame {
                    renderer.update_texture(&frame_buffer);
                }
            }

            // Finish egui frame and render everything
            let surface_texture = state.surface.get_current_texture();
            
            match surface_texture {
                Err(SurfaceError::Outdated) => {
                    println!("wgpu surface outdated");
                    return;
                }
                Err(err) => {
                    println!("Surface error: {:?}", err);
                    return;
                }
                Ok(surface_texture) => {
                    let surface_view = surface_texture
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());

                    let mut encoder = state
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                    
                    // No background clearing - let the Game Boy screen show through
                        
                    // Then render egui on top with Load to preserve the background
                    state.egui_renderer.end_frame_and_draw(
                        &state.device,
                        &state.queue,
                        &mut encoder,
                        window,
                        &surface_view,
                        screen_descriptor,
                    );
                    
                    state.queue.submit(Some(encoder.finish()));
                    surface_texture.present();
                }
            }

            gui_action
        } else {
            None
        };

        // Handle GUI actions
        if let Some(action) = gui_action {
            // Move gui_action handling outside of the state borrow scope
            let action_copy = action;
            match action_copy {
                GuiAction::LoadRom(file_data) => {
                    if let Some(world) = &mut self.world {
                        match world.load_rom(file_data) {
                            Ok(msg) => {
                                #[cfg(target_arch = "wasm32")]
                                web_sys::console::log_1(&format!("WASM ROM loaded successfully: {}", msg).into());
                            }
                            Err(err) => {
                                #[cfg(target_arch = "wasm32")]
                                web_sys::console::log_1(&format!("WASM ROM load error: {}", err).into());
                            }
                        }
                    }
                }
                GuiAction::SaveState(path) => {
                    if let Some(world) = &self.world {
                        match world.save_state(path) {
                            Ok(_msg) => {
                                // Success handling
                            }
                            Err(_err) => {
                                // Error handling
                            }
                        }
                    }
                }
                GuiAction::LoadState(file_data) => {
                    if let Some(world) = &mut self.world {
                        match world.load_state(file_data) {
                            Ok(_msg) => {
                                // Success handling
                            }
                            Err(_err) => {
                                // Error handling
                            }
                        }
                    }
                }
                GuiAction::Exit => {
                    window.request_redraw();
                    std::process::exit(0);
                }
                GuiAction::StepCycles(cycles) => {
                    if let Some(world) = &mut self.world {
                        world.step_multiple_cycles = Some(cycles);
                    }
                }
                GuiAction::StepFrames(frames) => {
                    if let Some(world) = &mut self.world {
                        world.step_multiple_frames = Some(frames);
                    }
                }
                GuiAction::Restart => {
                    if let Some(world) = &mut self.world {
                        world.restart();
                    }
                }
                GuiAction::TogglePause => {
                    if let Some(world) = &mut self.world {
                        world.toggle_pause();
                        // Update our tracking of user pause state
                        self.user_paused = world.is_paused;
                        self.manually_paused = self.user_paused || world.error_state.is_some();
                    }
                }
                GuiAction::ClearError => {
                    if let Some(world) = &mut self.world {
                        world.clear_error();
                        // Update pause state after clearing error
                        self.manually_paused = self.user_paused || world.error_state.is_some();
                    }
                }
                GuiAction::SetBreakpoint(address) => {
                    if let Some(world) = &mut self.world {
                        world.add_breakpoint(address);
                    }
                }
                GuiAction::RemoveBreakpoint(address) => {
                    if let Some(world) = &mut self.world {
                        world.remove_breakpoint(address);
                    }
                }
            }
        }

        // Request another redraw to keep the emulator running continuously
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }




}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_arch = "wasm32")]
        let window = {
            use winit::platform::web::WindowAttributesExtWebSys;
            use wasm_bindgen::JsCast;
            
            // Get the existing canvas element
            let canvas = web_sys::window()
                .and_then(|win| win.document())
                .and_then(|doc| doc.get_element_by_id("rustyboi-canvas"))
                .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
                .expect("Failed to find canvas element with id 'rustyboi-canvas'");
            
            let window_attributes = Window::default_attributes()
                .with_canvas(Some(canvas));
                
            event_loop.create_window(window_attributes).unwrap()
        };
        
        #[cfg(not(target_arch = "wasm32"))]
        let window = event_loop
            .create_window(Window::default_attributes())
            .unwrap();
        
        #[cfg(not(target_arch = "wasm32"))]
        {
            pollster::block_on(self.set_window(window));
            // Request initial redraw to start the rendering loop
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        
        #[cfg(target_arch = "wasm32")]
        {
            // For WASM, store the window and start async initialization
            let window = Arc::new(window);
            let scale = self.config.as_ref().map(|c| c.scale as u32).unwrap_or(7);
            let initial_width = 160 * scale;
            let initial_height = 144 * scale;
            let _ = window.request_inner_size(PhysicalSize::new(initial_width, initial_height));
            
            // Log window size for debugging
            let actual_size = window.inner_size();
            web_sys::console::log_1(&format!("WASM window size: {}x{}", actual_size.width, actual_size.height).into());
            
            self.window = Some(window.clone());
            
            // Calculate current viewport size first
            let current_size = web_sys::window()
                .and_then(|win| {
                    let width = win.inner_width().ok()?.as_f64()? as u32;
                    let height = win.inner_height().ok()?.as_f64()? as u32;
                    // Ensure minimum size
                    let width = width.max(800);
                    let height = height.max(600);
                    Some(PhysicalSize::new(width, height))
                })
                .unwrap_or(PhysicalSize::new(800, 600));
            
            web_sys::console::log_1(&format!("WASM detected viewport size: {}x{}", current_size.width, current_size.height).into());
            
            // For WASM, add the canvas to the DOM and set up proper sizing
            use wasm_bindgen::JsCast;
            
            // Set up window resize handler
            use std::rc::Rc;
            let window_clone = Rc::new(window.clone());
            let closure = wasm_bindgen::closure::Closure::wrap(Box::new({
                let window = window_clone.clone();
                move |_e: web_sys::Event| {
                    // Get current viewport size
                    let new_size = web_sys::window()
                        .and_then(|win| {
                            let width = win.inner_width().ok()?.as_f64()? as u32;
                            let height = win.inner_height().ok()?.as_f64()? as u32;
                            Some(PhysicalSize::new(width, height))
                        })
                        .unwrap_or(PhysicalSize::new(1360, 768));
                    
                    
                    
                    let _ = window.request_inner_size(new_size);
                }
            }) as Box<dyn FnMut(_)>);
            
            web_sys::window()
                .unwrap()
                .add_event_listener_with_callback("resize", closure.as_ref().unchecked_ref())
                .unwrap();
            closure.forget();
            
            // Don't override canvas size - let JavaScript handle sizing
            web_sys::console::log_1(&"WASM skipping window size request to preserve canvas sizing".into());

            // Start async initialization
            if !self.initializing {
                self.initializing = true;
                let instance = self.instance.clone();
                let shared_state = self.shared_state.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let surface = instance
                        .create_surface(window.clone())
                        .expect("Failed to create surface!");
                    
                    let actual_size = window.inner_size();
                    let width = if actual_size.width > 0 { actual_size.width } else { initial_width };
                    let height = if actual_size.height > 0 { actual_size.height } else { initial_height };
                    
                    let state = AppState::new(&instance, surface, &window, width, height).await;
                    
                    // Set the shared state
                    *shared_state.borrow_mut() = Some(state);
                    
                    // Request multiple redraws to ensure rendering starts
                    window.request_redraw();
                    
                    // Use web_sys::setTimeout for delayed redraw
                    let window_clone = window.clone();
                    let closure = wasm_bindgen::closure::Closure::wrap(Box::new(move || {
                        window_clone.request_redraw();
                    }) as Box<dyn FnMut()>);
                    
                    web_sys::window()
                        .unwrap()
                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                            closure.as_ref().unchecked_ref(),
                            100
                        )
                        .unwrap();
                    closure.forget();
                });
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        // Update input handler and handle Game Boy inputs
        if let Some(input) = &mut self.input {
            // Create a proper event for the input handler
            let winit_event: winit::event::Event<()> = winit::event::Event::WindowEvent { 
                window_id, 
                event: event.clone() 
            };
            if input.update(&winit_event) {
                self.handle_input_events(event_loop);
            }
        }

        // Let egui process the event, but only if state exists
        let mut needs_repaint = false;
        if let (Some(state), Some(window)) = (self.state.as_mut(), self.window.as_ref()) {
            needs_repaint = state.egui_renderer.handle_input(window, &event);
        }

        // Handle framework events if available
        if let (Some(framework), Some(window)) = (&mut self.framework, self.window.as_ref()) {
            framework.handle_event(window, &event);
        }

        match event {
            WindowEvent::CloseRequested => {
                println!("The close button was pressed; stopping");
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                self.handle_redraw();
            }
            WindowEvent::Resized(new_size) => {
                self.handle_resized(new_size.width, new_size.height);
                // Request redraw on resize
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            _ => {
                // For other events, request redraw only if egui needs it
                if needs_repaint {
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
            }
        }
    }
}

impl App {
    fn handle_input_events(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(input) = &self.input {
            if input.key_pressed(KeyCode::Escape) || input.close_requested() {
                event_loop.exit();
                return;
            }

            // Handle F key for frame stepping with debounce
            if input.key_pressed(KeyCode::KeyF) {
                if let Some(world) = &mut self.world {
                    if self.manually_paused || world.error_state.is_some() {
                        // Initial press - execute immediately
                        world.step_single_frame = true;
                        let now = Instant::now();
                        self.f_key_press_time = Some(now);
                        self.f_last_repeat_time = Some(now);
                        self.f_key_processed_initial = true;
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                }
            } else if input.key_held(KeyCode::KeyF) {
                if let Some(world) = &mut self.world {
                    if (self.manually_paused || world.error_state.is_some())
                        && let Some(press_time) = self.f_key_press_time {
                            // Check if debounce period has elapsed
                            const DEBOUNCE_DURATION: Duration = Duration::from_millis(250);
                            const REPEAT_INTERVAL: Duration = Duration::from_millis(67);
                            if press_time.elapsed() >= DEBOUNCE_DURATION {
                                // Check if enough time has passed since last repeat
                                if let Some(last_repeat) = self.f_last_repeat_time
                                    && last_repeat.elapsed() >= REPEAT_INTERVAL {
                                        world.step_single_frame = true;
                                        self.f_last_repeat_time = Some(Instant::now());
                                        if let Some(window) = &self.window {
                                            window.request_redraw();
                                        }
                                    }
                            }
                        }
                }
            } else {
                // Key released - reset state
                self.f_key_press_time = None;
                self.f_key_processed_initial = false;
                self.f_last_repeat_time = None;
            }

            // Handle N key for cycle stepping with debounce
            if input.key_pressed(KeyCode::KeyN) {
                if let Some(world) = &mut self.world {
                    if self.manually_paused || world.error_state.is_some() {
                        // Initial press - execute immediately
                        world.step_single_cycle = true;
                        let now = Instant::now();
                        self.n_key_press_time = Some(now);
                        self.n_last_repeat_time = Some(now);
                        self.n_key_processed_initial = true;
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                }
            } else if input.key_held(KeyCode::KeyN) {
                if let Some(world) = &mut self.world {
                    if (self.manually_paused || world.error_state.is_some())
                        && let Some(press_time) = self.n_key_press_time {
                            const DEBOUNCE_DURATION: Duration = Duration::from_millis(250);
                            const REPEAT_INTERVAL: Duration = Duration::from_millis(67);
                            // Check if debounce period has elapsed
                            if press_time.elapsed() >= DEBOUNCE_DURATION {
                                // Check if enough time has passed since last repeat
                                if let Some(last_repeat) = self.n_last_repeat_time
                                    && last_repeat.elapsed() >= REPEAT_INTERVAL {
                                        world.step_single_cycle = true;
                                        self.n_last_repeat_time = Some(Instant::now());
                                        if let Some(window) = &self.window {
                                            window.request_redraw();
                                        }
                                    }
                            }
                        }
                }
            } else {
                // Key released - reset state
                self.n_key_press_time = None;
                self.n_key_processed_initial = false;
                self.n_last_repeat_time = None;
            }

            if let Some(scale_factor) = input.scale_factor() {
                if let Some(framework) = &mut self.framework {
                    framework.scale_factor(scale_factor);
                }
            }

            // Handle Game Boy input based on keybinds
            if let (Some(world), Some(config)) = (&mut self.world, &self.config) {
                let button_state = input::ButtonState {
                    a: input.key_held(config.keybinds.a),
                    b: input.key_held(config.keybinds.b),
                    start: input.key_held(config.keybinds.start),
                    select: input.key_held(config.keybinds.select),
                    up: input.key_held(config.keybinds.up),
                    down: input.key_held(config.keybinds.down),
                    left: input.key_held(config.keybinds.left),
                    right: input.key_held(config.keybinds.right),
                };
                
                world.set_input_state(button_state);

                // Update internal state and request a redraw (only if not resizing)
                world.update();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }
    }
}

use egui::Context;
use egui_wgpu::wgpu::{CommandEncoder, Device, Queue, StoreOp, TextureFormat, TextureView};
use egui_wgpu::Renderer;
use egui_winit::State;

pub struct EguiRenderer {
    state: State,
    renderer: Renderer,
    frame_started: bool,
}

impl EguiRenderer {
    pub fn context(&self) -> &Context {
        self.state.egui_ctx()
    }

    pub fn new(
        device: &Device,
        output_color_format: TextureFormat,
        output_depth_format: Option<TextureFormat>,
        msaa_samples: u32,
        window: &Window,
    ) -> EguiRenderer {
        let egui_context = Context::default();

        let egui_state = egui_winit::State::new(
            egui_context,
            egui::viewport::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(2 * 1024), // default dimension is 2048
        );
        let egui_renderer = Renderer::new(
            device,
            output_color_format,
            output_depth_format,
            msaa_samples,
            true,
        );

        EguiRenderer {
            state: egui_state,
            renderer: egui_renderer,
            frame_started: false,
        }
    }

    pub fn handle_input(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        response.repaint
    }

    pub fn ppp(&mut self, v: f32) {
        self.context().set_pixels_per_point(v);
    }

    pub fn begin_frame(&mut self, window: &Window) {
        let raw_input = self.state.take_egui_input(window);
        self.state.egui_ctx().begin_pass(raw_input);
        self.frame_started = true;
    }

    pub fn end_frame_and_draw(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        window: &Window,
        window_surface_view: &TextureView,
        screen_descriptor: ScreenDescriptor,
    ) {
        if !self.frame_started {
            panic!("begin_frame must be called before end_frame_and_draw can be called!");
        }

        self.ppp(screen_descriptor.pixels_per_point);

        let full_output = self.state.egui_ctx().end_pass();

        // Handle platform output more carefully to avoid animation loops
        let platform_output = full_output.platform_output;
        
        self.state
            .handle_platform_output(window, platform_output);

        // Don't automatically request redraws from here - let input events handle it

        // Always render - the change detection was causing panels to blink
        let tris = self
            .state
            .egui_ctx()
            .tessellate(full_output.shapes, self.state.egui_ctx().pixels_per_point());
        for (id, image_delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(device, queue, *id, image_delta);
        }
        self.renderer
            .update_buffers(device, queue, encoder, &tris, &screen_descriptor);
        let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: window_surface_view,
                resolve_target: None,
                ops: egui_wgpu::wgpu::Operations {
                    load: egui_wgpu::wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            label: Some("egui main render pass"),
            occlusion_query_set: None,
        });

        self.renderer
            .render(&mut rpass.forget_lifetime(), &tris, &screen_descriptor);
        for x in &full_output.textures_delta.free {
            self.renderer.free_texture(x)
        }

        self.frame_started = false;
    }


}

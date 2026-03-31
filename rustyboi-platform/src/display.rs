use crate::config;
use crate::framework::Framework;
use crate::game_renderer::GameRenderer;
use rustyboi_egui_lib::actions::GuiAction;
use rustyboi_egui_lib::actions::FileData;
use rustyboi_core_lib::{cartridge, gb, ppu, input};
use rustyboi_session::Session;

use std::time::{Duration, Instant};
#[cfg(not(target_os = "android"))]
use winit::dpi::LogicalSize;
use winit::event::{Event,WindowEvent};
use winit::event_loop::EventLoop;
use winit::keyboard::KeyCode;
use winit::window::WindowBuilder;
use winit_input_helper::WinitInputHelper;
use pixels::{Error, Pixels, SurfaceTexture};
#[cfg(target_arch = "wasm32")]
use pixels::PixelsBuilder;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use winit::platform::web::WindowExtWebSys;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 144;

#[cfg(target_arch = "wasm32")]
/// Retrieve current width and height dimensions of browser client window
fn get_window_size() -> LogicalSize<f64> {
    let client_window = web_sys::window().unwrap();
    LogicalSize::new(
        client_window.inner_width().unwrap().as_f64().unwrap(),
        client_window.inner_height().unwrap().as_f64().unwrap(),
    )
}

#[cfg(target_arch = "wasm32")]
pub async fn run_with_gui_async(gb: Box<gb::GB>, config: config::CleanConfig) {
    let event_loop = EventLoop::new().unwrap();
    let window = {
        let size = LogicalSize::new((WIDTH * (config.scale as u32)) as f64, (HEIGHT * (config.scale as u32)) as f64);
        WindowBuilder::new()
            .with_title("RustyBoi")
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(WIDTH as f64, HEIGHT as f64))
            .build(&event_loop)
            .unwrap()
    };

    web_sys::window()
        .and_then(|win| win.document())
        .and_then(|doc| doc.body())
        .and_then(|body| {
            body.append_child(&web_sys::Element::from(window.canvas().unwrap()))
                .ok()
        })
        .expect("couldn't append canvas to document body");

    let window = Rc::new(window);

    let closure = wasm_bindgen::closure::Closure::wrap(Box::new({
        let window = Rc::clone(&window);
        move |_e: web_sys::Event| {
            let _ = window.request_inner_size(get_window_size());
        }
    }) as Box<dyn FnMut(_)>);
    web_sys::window()
        .unwrap()
        .add_event_listener_with_callback("resize", closure.as_ref().unchecked_ref())
        .unwrap();
    closure.forget();

    // Trigger initial resize event
    let _ = window.request_inner_size(get_window_size());

    let (pixels, framework, game_renderer) = async {
        let scale_factor = window.scale_factor() as f32;
        let window_size = get_window_size().to_physical::<u32>(scale_factor as f64);
        let surface_texture = SurfaceTexture::new(window_size.width, window_size.height, window.as_ref());
        let pixels = PixelsBuilder::new(WIDTH, HEIGHT, surface_texture)
            .texture_format(pixels::wgpu::TextureFormat::Rgba8Unorm)
            .surface_texture_format(pixels::wgpu::TextureFormat::Rgba8Unorm)
            .wgpu_backend(pixels::wgpu::Backends::all())
            .build_async()
            .await
            .expect("Failed to create Pixels instance");
        let framework = Framework::new(
            &event_loop,
            window_size.width,
            window_size.height,
            scale_factor,
            &pixels,
            None,
        );
        let game_renderer = build_game_renderer(&pixels);

        (pixels, framework, game_renderer)
    }.await;
    match run_gui_loop(event_loop, &window, Some(pixels), Some(framework), Some(game_renderer), gb, &config) {
        Ok(_) => (),
        Err(e) => eprintln!("Error in GUI loop: {}", e),
    }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn run_with_gui(gb: Box<gb::GB>, config: &config::CleanConfig) -> Result<(), Error> {
    let event_loop = EventLoop::new().unwrap();
    let window = {
        let size = LogicalSize::new((WIDTH * (config.scale as u32)) as f64, (HEIGHT * (config.scale as u32)) as f64);
        WindowBuilder::new()
            .with_title("RustyBoi")
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(WIDTH as f64, HEIGHT as f64))
            .build(&event_loop)
            .unwrap()
    };

    // Pixels and Framework are created lazily on Event::Resumed so the
    // same loop body works on platforms (Android) where the rendering
    // surface only becomes available after the window is shown.
    run_gui_loop(event_loop, &window, None, None, None, gb, config)
}

/// Build the emulator-framebuffer renderer. The renderer owns its own RGBA
/// source texture(s) (160x144 normal, 256x224 SGB) and uploads frames itself,
/// so it is independent of the `pixels` framebuffer's fixed size.
fn build_game_renderer(pixels: &Pixels) -> GameRenderer {
    GameRenderer::new(pixels.device(), pixels.render_texture_format())
}

/// Android entry. Builds an `EventLoop` bound to the supplied `AndroidApp`,
/// constructs the window, and hands off to the shared loop. The render
/// surface is created lazily in `Event::Resumed`.
#[cfg(target_os = "android")]
pub fn run_with_gui_android(
    app: winit::platform::android::activity::AndroidApp,
    gb: Box<gb::GB>,
    config: &config::CleanConfig,
) -> Result<(), Error> {
    use crate::android::raw_log;
    use winit::platform::android::EventLoopBuilderExtAndroid;

    raw_log("run_with_gui_android: building EventLoop");
    let event_loop = winit::event_loop::EventLoopBuilder::<()>::with_user_event()
        .with_android_app(app)
        .build()
        .map_err(|e| {
            // `Error::UserDefined`'s Display drops the inner cause, so log
            // the real reason before we wrap it. This is what surfaces
            // winit's "EventLoop already created" / OS errors on
            // recreate-from-recents on Android.
            raw_log(&format!("run_with_gui_android: EventLoop build failed: {e:?} ({e})"));
            log::error!("EventLoop build failed: {e:?} ({e})");
            Error::UserDefined(Box::new(e))
        })?;
    raw_log("run_with_gui_android: EventLoop built");
    let window = WindowBuilder::new()
        .with_title("RustyBoi")
        .build(&event_loop)
        .map_err(|e| {
            raw_log(&format!("run_with_gui_android: Window build failed: {e:?} ({e})"));
            log::error!("Window build failed: {e:?} ({e})");
            Error::UserDefined(Box::new(e))
        })?;
    raw_log("run_with_gui_android: Window built, entering loop");
    let r = run_gui_loop(event_loop, &window, None, None, None, gb, config);
    raw_log("run_with_gui_android: loop returned");
    r
}

/// Create the wgpu `Pixels` surface and the egui `Framework` for `window`.
/// Used by `Event::Resumed` to (re)create render state when the platform
/// makes a surface available.
fn create_render_state<'win, T>(
    event_loop: &winit::event_loop::EventLoopWindowTarget<T>,
    window: &'win winit::window::Window,
    pending_dialog_result: Option<std::sync::Arc<std::sync::Mutex<Option<rustyboi_egui_lib::actions::GuiAction>>>>,
) -> Result<(Pixels<'win>, Framework, GameRenderer), Error> {
    let window_size = window.inner_size();
    let scale_factor = window.scale_factor() as f32;
    let surface_texture = SurfaceTexture::new(window_size.width, window_size.height, window);
    let pixels = Pixels::new(WIDTH, HEIGHT, surface_texture)?;
    let framework = Framework::new(
        event_loop,
        window_size.width.max(1),
        window_size.height.max(1),
        scale_factor,
        &pixels,
        pending_dialog_result,
    );
    let game_renderer = build_game_renderer(&pixels);
    Ok((pixels, framework, game_renderer))
}

/// Base directory the session ports read/write savestates + config under.
fn save_base() -> std::path::PathBuf {
    #[cfg(target_os = "android")]
    {
        crate::android::save_dir()
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        crate::ports::desktop_save_dir()
    }
    #[cfg(target_arch = "wasm32")]
    {
        // The wasm build does not yet persist through the filesystem; a real
        // IndexedDB-backed Storage arrives with the web frontend adapter.
        std::path::PathBuf::from(".")
    }
}

fn run_gui_loop<'win>(
    event_loop: EventLoop<()>,
    window: &'win winit::window::Window,
    mut pixels: Option<Pixels<'win>>,
    mut framework: Option<Framework>,
    mut game_renderer: Option<GameRenderer>,
    gb: Box<gb::GB>,
    config: &config::CleanConfig,
) -> Result<(), Error> {
    let mut input = WinitInputHelper::new();

    // Build the desktop/Android service ports and load the persisted session
    // config, then let the CLI hardware choice win for this launch.
    let ports = crate::ports::build_ports(save_base());
    let mut session_config = rustyboi_session::Config::load(ports.storage.as_ref());
    session_config.hardware = config.hardware;

    // ROM bytes (for the session's ROM-id slot keying). On the startup path the
    // GB may already have a cartridge inserted; read the source bytes so the id
    // matches what a later in-app Load ROM of the same file would produce.
    let rom_bytes = config
        .rom
        .as_ref()
        .and_then(|p| std::fs::read(p).ok());

    let mut world = World::new_with_paths(
        gb,
        rom_bytes,
        config.rom.clone(),
        config.bios.clone(),
        config.palette,
        session_config,
        ports,
    );
    if config.printer {
        world.gb_mut().attach_printer();
        println!("Game Boy Printer attached to the link port");
    }

    // Persist the pending-dialog-result `Arc` across `Framework`
    // suspend/resume cycles. On Android the SAF picker takes our activity
    // off-screen, which destroys the rendering surface and drops the
    // `Framework` (and its `Gui`). If we recreated the `Arc` inside each
    // new `Gui`, the callback registered before suspend would write into
    // an orphaned slot and the picked file would never reach the loop.
    let pending_dialog_result: std::sync::Arc<
        std::sync::Mutex<Option<rustyboi_egui_lib::actions::GuiAction>>,
    > = match framework.as_ref() {
        Some(fw) => fw.pending_dialog_result(),
        None => std::sync::Arc::new(std::sync::Mutex::new(None)),
    };

    // Enable audio output
    if let Err(e) = world.enable_audio() {
        println!("Failed to initialize audio: {}", e);
        println!("Continuing without audio...");
    }

    // Start paused if no ROM and no BIOS are loaded
    let should_start_paused = world.is_paused;
    let mut manually_paused = should_start_paused;
    let mut user_paused = should_start_paused; // Track user-initiated pause separate from debug pause

    // Debounce timing for F and N keys
    const DEBOUNCE_DURATION: Duration = Duration::from_millis(250); // Time to wait before auto-repeat
    const REPEAT_INTERVAL: Duration = Duration::from_millis(67); // Execute every ~67ms when held (about 15fps)
    let mut f_key_press_time: Option<Instant> = None;
    let mut n_key_press_time: Option<Instant> = None;
    let mut f_last_repeat_time: Option<Instant> = None;
    let mut n_last_repeat_time: Option<Instant> = None;

    let res = event_loop.run(|event, elwt| {
        // Surface lifecycle: on platforms like Android the rendering surface
        // is created/destroyed at runtime. Desktop fires Resumed once at
        // startup and Suspended at shutdown, so this path is also exercised
        // there and the loop runs identically across targets.
        match &event {
            Event::Resumed => {
                if pixels.is_none() {
                    match create_render_state(elwt, window, Some(pending_dialog_result.clone())) {
                        Ok((p, f, g)) => {
                            pixels = Some(p);
                            framework = Some(f);
                            game_renderer = Some(g);
                            window.request_redraw();
                            // On Android, hydrate the freshly-built ROM
                            // Library panel from persisted state. We do
                            // this every Resumed so that surface
                            // recreation (background → foreground)
                            // re-applies the persisted tree URI without
                            // forcing the user to re-pick.
                            #[cfg(target_os = "android")]
                            if let Some(ref mut fw) = framework {
                                let state = crate::library::LibraryState::load();
                                // Always hydrate recents so the user
                                // sees their MRU list immediately on
                                // resume, even before the scan
                                // finishes.
                                fw.library_panel_mut().set_recents(state.recents.clone());
                                if state.tree_uri.is_some() {
                                    if let Ok(mut slot) = pending_dialog_result.lock() {
                                        *slot = Some(
                                            rustyboi_egui_lib::actions::GuiAction::SetLibraryTreeUri(
                                                state.tree_uri,
                                            ),
                                        );
                                    }
                                } else {
                                    fw.library_panel_mut().set_status(Some(
                                        "Pick your ROMs folder to get started.".into(),
                                    ));
                                }
                            }
                        }
                        Err(err) => {
                            println!("Failed to create render state on Resumed: {}", err);
                            elwt.exit();
                            return;
                        }
                    }
                }
            }
            Event::Suspended => {
                // Drop GPU-bound state; keep `World` (CPU/PPU/audio) alive.
                framework = None;
                pixels = None;
                game_renderer = None;
            }
            _ => {}
        }

        if input.update(&event) {
            if input.key_pressed(KeyCode::Escape) || input.close_requested() {
                elwt.exit();
                return;
            }

            // --- Session feature hotkeys ------------------------------------
            // Quicksave / quickload (F5 / F8).
            if input.key_pressed(KeyCode::F5) {
                match world.quicksave() {
                    Ok(()) => println!("Quicksaved"),
                    Err(e) => println!("Quicksave failed: {e}"),
                }
                window.request_redraw();
            }
            if input.key_pressed(KeyCode::F8) {
                match world.quickload() {
                    Ok(()) => window.request_redraw(),
                    Err(e) => println!("Quickload failed: {e}"),
                }
            }
            // Fast-forward / turbo (Tab toggles).
            if input.key_pressed(KeyCode::Tab) {
                world.toggle_fast_forward();
            }
            // Frame advance (Backslash): one frame, then pause.
            if input.key_pressed(KeyCode::Backslash) {
                world.frame_advance();
                user_paused = true;
                manually_paused = true;
                window.request_redraw();
            }
            // Hold-to-rewind (Backspace): step back one snapshot per frame while
            // held. Gated on rewind being enabled in config.
            if input.key_held(KeyCode::Backspace) && world.session.config().rewind.enabled {
                world.rewind();
                window.request_redraw();
            }

            // Handle F key for frame stepping with debounce
            if input.key_pressed(KeyCode::KeyF) {
                if manually_paused || world.error_state.is_some() {
                    // Initial press - execute immediately
                    world.step_single_frame = true;
                    let now = Instant::now();
                    f_key_press_time = Some(now);
                    f_last_repeat_time = Some(now);
                    window.request_redraw();
                }
            } else if input.key_held(KeyCode::KeyF) {
                if (manually_paused || world.error_state.is_some())
                    && let Some(press_time) = f_key_press_time {
                        // Check if debounce period has elapsed
                        if press_time.elapsed() >= DEBOUNCE_DURATION {
                            // Check if enough time has passed since last repeat
                            if let Some(last_repeat) = f_last_repeat_time
                                && last_repeat.elapsed() >= REPEAT_INTERVAL {
                                    world.step_single_frame = true;
                                    f_last_repeat_time = Some(Instant::now());
                                    window.request_redraw();
                                }
                        }
                    }
            } else {
                // Key released - reset state
                f_key_press_time = None;
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
                    window.request_redraw();
                }
            } else if input.key_held(KeyCode::KeyN) {
                if (manually_paused || world.error_state.is_some())
                    && let Some(press_time) = n_key_press_time {
                        // Check if debounce period has elapsed
                        if press_time.elapsed() >= DEBOUNCE_DURATION {
                            // Check if enough time has passed since last repeat
                            if let Some(last_repeat) = n_last_repeat_time
                                && last_repeat.elapsed() >= REPEAT_INTERVAL {
                                    world.step_single_cycle = true;
                                    n_last_repeat_time = Some(Instant::now());
                                    window.request_redraw();
                                }
                        }
                    }
            } else {
                // Key released - reset state
                n_key_press_time = None;
                n_last_repeat_time = None;
            }

            if let Some(scale_factor) = input.scale_factor()
                && let Some(framework) = framework.as_mut()
            {
                framework.scale_factor(scale_factor);
            }

            // Handle Game Boy input based on keybinds, OR'd with any
            // on-screen touch controls captured by the egui overlay.
            let mut button_state = input::ButtonState {
                a: input.key_held(config.keybinds.a),
                b: input.key_held(config.keybinds.b),
                start: input.key_held(config.keybinds.start),
                select: input.key_held(config.keybinds.select),
                up: input.key_held(config.keybinds.up),
                down: input.key_held(config.keybinds.down),
                left: input.key_held(config.keybinds.left),
                right: input.key_held(config.keybinds.right),
            };
            if let Some(framework) = framework.as_ref() {
                let touch = framework.touch_button_state();
                button_state.a |= touch.a;
                button_state.b |= touch.b;
                button_state.start |= touch.start;
                button_state.select |= touch.select;
                button_state.up |= touch.up;
                button_state.down |= touch.down;
                button_state.left |= touch.left;
                button_state.right |= touch.right;
            }

            world.set_input_state(button_state);

            // Update internal state and request a redraw (only if not resizing)
            world.update();
            window.request_redraw();
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                if let Some(pixels) = pixels.as_mut()
                    && let Err(err) = pixels.resize_surface(size.width.max(1), size.height.max(1)) {
                        println!("Failed to resize surface during window event: {}", err);
                        elwt.exit();
                        return;
                    }
                if let Some(framework) = framework.as_mut() {
                    framework.resize(size.width.max(1), size.height.max(1));
                }
            }
            Event::WindowEvent {
                event: WindowEvent::RedrawRequested,
                ..
            } => {
                // Skip drawing while we have no surface (e.g. Android suspended).
                let (pixels, framework, game_renderer) = match (pixels.as_mut(), framework.as_mut(), game_renderer.as_mut()) {
                    (Some(p), Some(f), Some(g)) => (p, f, g),
                    _ => return,
                };
                // Convert the latest emulator frame (or SGB composite) to the
                // RGBA source the renderer uploads below.
                let present = world.present();
                let gui_paused_state = manually_paused || world.error_state.is_some();

                // Update window title with performance metrics
                world.update_window_title(window, gui_paused_state);
                // Always pass register data for the debug overlay, regardless of pause state
                let ui_state = world.ui_state();
                let registers = Some(world.gb().get_cpu_registers());
                let gb_ref = Some(world.gb());
                let (gui_action, menu_open, game_region) = framework.prepare(window, gui_paused_state, registers, gb_ref, &ui_state);

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
                                // If emulator was auto-paused due to no content and now has content, unpause
                                if world.should_auto_unpause() {
                                    manually_paused = false;
                                    user_paused = false;
                                } else {
                                    // Keep user pause state when loading state
                                    manually_paused = user_paused || world.error_state.is_some();
                                }
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
                        #[cfg(target_os = "android")]
                        log::info!("event loop: handling GuiAction::LoadRom");
                        match world.load_rom(path) {
                            Ok(loaded_path) => {
                                // If emulator was auto-paused due to no content and now has ROM, unpause
                                if world.should_auto_unpause() {
                                    manually_paused = false;
                                    user_paused = false;
                                } else {
                                    // Keep user pause state when loading ROM
                                    manually_paused = user_paused;
                                }
                                framework.clear_error();
                                framework.set_status(format!("ROM loaded from: {}", loaded_path));
                                window.request_redraw();
                            }
                            Err(e) => {
                                #[cfg(target_os = "android")]
                                log::error!("event loop: load_rom returned error: {e}");
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
                    Some(GuiAction::TogglePrinter) => {
                        if world.gb().printer_attached() {
                            world.gb_mut().detach_serial_device();
                            framework.set_status("Game Boy Printer disconnected".to_string());
                        } else {
                            world.gb_mut().attach_printer();
                            framework.set_status(
                                "Game Boy Printer connected - prints are saved next to the ROM"
                                    .to_string(),
                            );
                        }
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
                    Some(GuiAction::SaveSlot(slot)) => match world.save_slot(slot) {
                        Ok(()) => framework.set_status(format!("Saved to slot {slot}")),
                        Err(e) => framework.set_error(format!("Failed to save slot {slot}: {e}")),
                    },
                    Some(GuiAction::LoadSlot(slot)) => match world.load_slot(slot) {
                        Ok(()) => {
                            framework.clear_error();
                            framework.set_status(format!("Loaded slot {slot}"));
                            window.request_redraw();
                        }
                        Err(e) => framework.set_error(format!("Failed to load slot {slot}: {e}")),
                    },
                    Some(GuiAction::Quicksave) => match world.quicksave() {
                        Ok(()) => framework.set_status("Quicksaved".to_string()),
                        Err(e) => framework.set_error(format!("Quicksave failed: {e}")),
                    },
                    Some(GuiAction::Quickload) => match world.quickload() {
                        Ok(()) => {
                            framework.clear_error();
                            framework.set_status("Quickloaded".to_string());
                            window.request_redraw();
                        }
                        Err(e) => framework.set_error(format!("Quickload failed: {e}")),
                    },
                    Some(GuiAction::ToggleFastForward) => {
                        world.toggle_fast_forward();
                        framework.set_status(
                            if world.is_fast_forward() { "Fast-forward on" } else { "Fast-forward off" }
                                .to_string(),
                        );
                    }
                    Some(GuiAction::FrameAdvance) => {
                        world.frame_advance();
                        // Frame-advance runs one frame then pauses; keep the GUI
                        // pause bookkeeping in sync so it doesn't fight the mode.
                        user_paused = true;
                        manually_paused = true;
                        window.request_redraw();
                    }
                    Some(GuiAction::ToggleSgbBorder) => {
                        world.toggle_sgb_border();
                        window.request_redraw();
                    }
                    Some(GuiAction::SetHardware(choice)) => {
                        use rustyboi_egui_lib::actions::HardwareChoice;
                        let hw = match choice {
                            HardwareChoice::Dmg => gb::Hardware::DMG,
                            HardwareChoice::Cgb => gb::Hardware::CGB,
                            HardwareChoice::Sgb => gb::Hardware::SGB,
                        };
                        world.set_hardware(hw);
                        framework.clear_error();
                        framework.set_status(format!("Hardware set to {choice:?}; ROM restarted"));
                        window.request_redraw();
                    }
                    Some(GuiAction::SetPalette(choice)) => {
                        use rustyboi_egui_lib::actions::PaletteChoice;
                        let palette = match choice {
                            PaletteChoice::Grayscale => config::ColorPalette::Grayscale,
                            PaletteChoice::OriginalGreen => config::ColorPalette::OriginalGreen,
                            PaletteChoice::Blue => config::ColorPalette::Blue,
                            PaletteChoice::Brown => config::ColorPalette::Brown,
                            PaletteChoice::Red => config::ColorPalette::Red,
                        };
                        world.set_palette(palette);
                        window.request_redraw();
                    }
                    Some(GuiAction::SetRewindEnabled(enabled)) => {
                        world.set_rewind_enabled(enabled);
                    }
                    Some(GuiAction::SetRewindInterval(interval)) => {
                        world.set_rewind_interval(interval);
                    }
                    Some(GuiAction::SetRewindDepth(depth)) => {
                        world.set_rewind_depth(depth);
                    }
                    #[cfg(target_os = "android")]
                    Some(GuiAction::OpenRomTree) => {
                        log::info!("event loop: OpenRomTree");
                        let pending = framework.pending_dialog_result();
                        rustyboi_egui_lib::android_bridge::pick_tree(Box::new(move |uri| {
                            if let Ok(mut slot) = pending.lock() {
                                *slot = Some(GuiAction::SetLibraryTreeUri(uri));
                            }
                        }));
                    }
                    #[cfg(target_os = "android")]
                    Some(GuiAction::RescanLibrary) => {
                        log::info!("event loop: RescanLibrary");
                        let tree_uri = framework
                            .library_panel_mut()
                            .tree_uri()
                            .map(str::to_owned);
                        if let Some(uri) = tree_uri {
                            framework.library_panel_mut().begin_scan();
                            let pending = framework.pending_dialog_result();
                            rustyboi_egui_lib::android_bridge::scan_library(
                                uri,
                                Box::new(move |entries| {
                                    if let Ok(mut slot) = pending.lock() {
                                        *slot = Some(GuiAction::SetLibraryEntries(entries));
                                    }
                                }),
                            );
                        } else {
                            framework
                                .library_panel_mut()
                                .set_status(Some("No library folder selected".into()));
                        }
                    }
                    #[cfg(target_os = "android")]
                    Some(GuiAction::LoadRomFromUri(uri)) => {
                        log::info!("event loop: LoadRomFromUri");
                        // Promote the URI in the MRU recents list and
                        // persist before we even know whether the load
                        // will succeed: tapping a recent is itself the
                        // intent signal, and treating a load failure
                        // as a reason to forget the entry would mean
                        // a flaky scan removes useful history.
                        let mut state = crate::library::LibraryState::load();
                        state.touch_recent(&uri);
                        state.save();
                        framework
                            .library_panel_mut()
                            .set_recents(state.recents.clone());
                        let pending = framework.pending_dialog_result();
                        rustyboi_egui_lib::android_bridge::load_rom_from_uri(
                            uri,
                            Box::new(move |file_data| {
                                if let Ok(mut slot) = pending.lock()
                                    && let Some(file_data) = file_data
                                {
                                    *slot = Some(GuiAction::LoadRom(file_data));
                                }
                            }),
                        );
                    }
                    #[cfg(target_os = "android")]
                    Some(GuiAction::SetLibraryTreeUri(uri)) => {
                        log::info!("event loop: SetLibraryTreeUri({:?})", uri);
                        let mut state = crate::library::LibraryState::load();
                        let tree_changed = state.tree_uri != uri;
                        state.tree_uri = uri.clone();
                        if tree_changed {
                            // Picking a new root invalidates the cached
                            // entry list; otherwise we'd briefly show
                            // ROMs from the old tree.
                            state.cached_entries.clear();
                        }
                        state.save();
                        framework.library_panel_mut().set_tree_uri(uri.clone());
                        // Hydrate from cache so the user sees the
                        // previous list immediately; a fresh scan runs
                        // below to pick up new/removed ROMs.
                        framework
                            .library_panel_mut()
                            .set_entries(state.cached_entries.clone());
                        if let Some(u) = uri {
                            framework.library_panel_mut().begin_scan();
                            let pending = framework.pending_dialog_result();
                            rustyboi_egui_lib::android_bridge::scan_library(
                                u,
                                Box::new(move |entries| {
                                    if let Ok(mut slot) = pending.lock() {
                                        *slot = Some(GuiAction::SetLibraryEntries(entries));
                                    }
                                }),
                            );
                        }
                    }
                    #[cfg(target_os = "android")]
                    Some(GuiAction::SetLibraryEntries(entries)) => {
                        match entries {
                            Some(entries) => {
                                log::info!(
                                    "event loop: SetLibraryEntries ({} items)",
                                    entries.len()
                                );
                                // Persist the fresh scan so the next
                                // resume can show the library
                                // immediately.
                                let mut state = crate::library::LibraryState::load();
                                state.cached_entries = entries.clone();
                                state.save();
                                framework.library_panel_mut().set_entries(entries);
                            }
                            None => {
                                log::warn!("event loop: library scan failed");
                                framework.library_panel_mut().set_status(Some(
                                    "Scan failed: tree no longer accessible. Re-pick the folder.".into(),
                                ));
                            }
                        }
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
                    let pc = world.gb().get_cpu_registers().pc;
                    manually_paused = true; // Ensure we stay paused
                    user_paused = true; // User should explicitly resume
                    framework.set_status(format!("Breakpoint hit at PC: ${:04X}", pc));
                }

                if let Some(error_msg) = &world.error_state {
                    framework.set_error(error_msg.clone());
                    // Update manually_paused to include error state
                    manually_paused = user_paused || world.error_state.is_some();
                }

                // Surface size in physical pixels; the game region (already in
                // physical pixels) is clamped to it inside the renderer.
                let surface = window.inner_size();
                let surface_size = (surface.width, surface.height);
                let render_result = pixels.render_with(|encoder, render_target, context| {
                    // Upload the latest emulator frame into the matching source
                    // texture (160x144 normal, 256x224 SGB border), then draw it
                    // only into the egui central region (below the menu bar,
                    // above the status panel), letterboxed. egui paints on top.
                    if let Some((size, rgba)) = present.as_ref() {
                        game_renderer.upload(&context.queue, *size, rgba);
                    }
                    game_renderer.render(
                        encoder,
                        &context.queue,
                        render_target,
                        surface_size,
                        game_region,
                    );

                    framework.render(encoder, render_target, context);

                    Ok(())
                });

                if let Err(err) = render_result {
                    println!("Render error: {}", err);
                    window.request_redraw();
                }
            }
            Event::WindowEvent { event, .. } => {
                if let Some(framework) = framework.as_mut() {
                    framework.handle_event(window, &event);
                }
            }
            _ => (),
        }
    });
    res.map_err(|e| Error::UserDefined(Box::new(e)))
}

/// Current epoch seconds, for savestate-slot timestamps. Falls back to 0
/// ("unknown") if the clock is before the epoch.
fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build a `Session` around an already-prepared `GB`, deriving the ROM id from
/// `rom_bytes` (all-zero when no cartridge is inserted). Reuses the platform's
/// persisted [`Config`] loaded through the port storage.
fn session_from_gb(
    gb: Box<gb::GB>,
    rom_bytes: Option<&[u8]>,
    config: rustyboi_session::Config,
    ports: rustyboi_session::Ports,
) -> Session {
    let rom_id = rom_bytes.map(rustyboi_session::sha256).unwrap_or([0u8; 32]);
    // `Session` owns the GB by value; unbox it (GB is heap-heavy but the move
    // is a memcpy of the box's contents, done once per ROM load).
    Session::with_gb(*gb, config, ports, rom_id)
}

struct World {
    /// The frontend-agnostic feature layer: owns the `GB`, config, ports,
    /// run-mode, savestate slots, rewind, TAS, and cheats. All per-frame
    /// stepping, input, and state ops route through it.
    session: Session,
    /// Latest presented frame from `run_frame` (or a debug step).
    frame: Option<gb::Frame>,
    /// Host audio output; fed the samples `run_frame` returns each frame.
    audio: Option<crate::audio::Output>,
    error_state: Option<String>,
    is_paused: bool,
    step_single_frame: bool,
    step_single_cycle: bool,
    step_multiple_cycles: Option<u32>,
    step_multiple_frames: Option<u32>,
    current_rom_path: Option<String>,
    current_bios_path: Option<String>,
    /// Raw ROM bytes of the loaded cartridge, kept so a slot/state load can
    /// re-derive the ROM id and reinsert the cartridge.
    rom_bytes: Option<Vec<u8>>,
    /// Latest input, already resolved to abstract GB buttons.
    input: rustyboi_session::AbstractInput,
    // FPS and performance tracking
    frame_times: Vec<Instant>,
    last_title_update: Instant,
    // Frame timing for 60fps
    last_frame_time: Instant,
    // Breakpoint status
    breakpoint_hit: bool,
    // Color palette (monochrome DMG shade ramp for presentation)
    palette: config::ColorPalette,
    // Track if emulator was auto-paused due to missing ROM/BIOS
    auto_paused_no_content: bool,
    /// Present the 256x224 SGB border composite when the machine offers one.
    sgb_border: bool,
    /// Background serializer for rewind snapshots (native desktop only). When
    /// present, the session runs in offloaded-capture mode: the emulation thread
    /// only clones the machine, this worker does the `to_state_bytes` serialize
    /// off-thread, and finished blobs are pushed back into the rewind ring.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    rewind_worker: Option<crate::rewind_worker::RewindWorker>,
    /// Background PNG encoder/writer for Game Boy Printer output (native
    /// desktop). Created lazily on the first print so non-printing sessions pay
    /// nothing. Keeps the encode + blocking disk write off the emulation thread.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    png_worker: Option<crate::png_worker::PngWorker>,
    /// `(stem, next_index)` for `<stem>-print-<n>.png`. The index is monotonic
    /// per stem so back-to-back prints never race on the same filename while an
    /// earlier async write is still in flight (the on-disk `exists()` check
    /// alone would collide). Re-seeded from disk when the ROM stem changes.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    next_print_index: Option<(String, u32)>,
}

impl World {
    fn new_with_paths(
        gb: Box<gb::GB>,
        rom_bytes: Option<Vec<u8>>,
        rom_path: Option<String>,
        bios_path: Option<String>,
        palette: config::ColorPalette,
        config: rustyboi_session::Config,
        ports: rustyboi_session::Ports,
    ) -> Self {
        let now = Instant::now();

        // Check if both ROM and BIOS are missing - if so, start paused
        let should_start_paused = !gb.has_rom() && !gb.has_bios();

        // `mut` only used on native, where offloaded rewind is enabled below.
        #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(unused_mut))]
        let mut session = session_from_gb(gb, rom_bytes.as_deref(), config, ports);

        // Native desktop: run rewind capture in offloaded mode so the expensive
        // savestate serialize happens on a worker thread, not the emulation
        // thread. Wasm has no threads; Android keeps the inline path.
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        let rewind_worker = {
            session.set_rewind_offloaded(true);
            Some(crate::rewind_worker::RewindWorker::new())
        };

        Self {
            session,
            frame: None,
            audio: None,
            error_state: None,
            is_paused: should_start_paused,
            step_single_frame: false,
            step_single_cycle: false,
            step_multiple_cycles: None,
            step_multiple_frames: None,
            current_rom_path: rom_path,
            current_bios_path: bios_path,
            rom_bytes,
            input: rustyboi_session::AbstractInput::none(),
            frame_times: Vec::with_capacity(60), // Store last 60 frame times for FPS calculation
            last_title_update: now,
            last_frame_time: now,
            breakpoint_hit: false,
            palette,
            auto_paused_no_content: should_start_paused,
            sgb_border: true,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            rewind_worker,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            png_worker: None,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            next_print_index: None,
        }
    }

    /// Borrow the underlying `GB` for presentation / debug panels.
    fn gb(&self) -> &gb::GB {
        self.session.gb()
    }

    /// Mutable `GB` access for host-side debug tooling (breakpoints, cycle
    /// stepping). Feature operations go through the session, not this.
    fn gb_mut(&mut self) -> &mut gb::GB {
        self.session.gb_mut()
    }

    /// Persist the current machine state to an arbitrary file path (the File
    /// menu's "Save State"). Slot saves go through the session instead.
    fn save_state(&self, path: std::path::PathBuf) -> Result<String, Box<dyn std::error::Error>> {
        let filename = path.to_string_lossy().to_string();
        let bytes = self.session.gb().to_state_bytes()?;
        std::fs::write(&filename, bytes)?;
        println!("Game state saved to: {}", filename);
        Ok(filename)
    }

    /// Save the current machine into numbered session slot `slot`.
    fn save_slot(&mut self, slot: u32) -> Result<(), String> {
        self.session
            .save_slot(slot, now_epoch_secs())
            .map_err(|e| e.to_string())
    }

    /// Load numbered session slot `slot`, restoring the machine.
    fn load_slot(&mut self, slot: u32) -> Result<(), String> {
        self.session.load_slot(slot).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Quicksave / quickload via the reserved session quick slot.
    fn quicksave(&mut self) -> Result<(), String> {
        self.session.quicksave(now_epoch_secs()).map_err(|e| e.to_string())
    }

    fn quickload(&mut self) -> Result<(), String> {
        self.session.quickload().map(|_| ()).map_err(|e| e.to_string())
    }

    /// Slot numbers that currently hold a saved state for this ROM.
    fn list_slots(&self) -> Vec<u32> {
        self.session.list_slots()
    }

    /// Hand any rewind snapshot captured this frame to the background
    /// serializer and push back any it has finished. Called once per emulated
    /// frame. On the emulation thread this costs only a `GB::clone` (taken
    /// inside the session) plus a channel send/recv — never a serialize.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn pump_rewind_worker(&mut self) {
        let Some(worker) = self.rewind_worker.as_mut() else { return };
        if let Some((frame, gb)) = self.session.take_pending_snapshot() {
            worker.submit(frame, gb);
        }
        for done in worker.drain_finished() {
            self.session.push_rewind_bytes(done.frame, done.bytes);
        }
    }

    /// No-op stub on platforms without the offload worker (wasm/android keep the
    /// session's inline capture path).
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    fn pump_rewind_worker(&mut self) {}

    /// Step back to the most recent rewind snapshot (hold-to-rewind hotkey).
    fn rewind(&mut self) {
        if self.session.rewind().is_some() {
            self.frame = Some(self.session.gb_mut().get_current_frame());
        }
    }

    /// Toggle fast-forward on/off (Normal ↔ FastForward at the config factor).
    fn toggle_fast_forward(&mut self) {
        use rustyboi_session::RunMode;
        match self.session.mode() {
            RunMode::FastForward(_) => self.session.set_mode(RunMode::Normal),
            _ => self.session.fast_forward(),
        }
    }

    /// Whether fast-forward is currently engaged.
    fn is_fast_forward(&self) -> bool {
        matches!(self.session.mode(), rustyboi_session::RunMode::FastForward(_))
    }

    /// Queue a single-frame advance. The session's `FrameAdvance` mode runs one
    /// frame on the next `update` (honored even while the World is paused) and
    /// then returns the session to `Paused`.
    fn frame_advance(&mut self) {
        self.session.frame_advance();
    }

    fn toggle_sgb_border(&mut self) {
        self.sgb_border = !self.sgb_border;
    }

    /// Change the emulated hardware model, persist it, and rebuild the machine
    /// from the cached ROM bytes so the change takes effect immediately.
    fn set_hardware(&mut self, hardware: gb::Hardware) {
        let mut cfg = self.session.config().clone();
        cfg.hardware = hardware;
        self.session.set_config(cfg);
        // Rebuild the machine on the new hardware (same ROM).
        let mut gb = Box::new(gb::GB::new(hardware));
        if let Some(bytes) = self.rom_bytes.clone()
            && let Ok(cartridge) = cartridge::Cartridge::from_bytes(&bytes)
        {
            gb.insert(cartridge);
            gb.skip_bios();
        }
        let rom_id = self
            .rom_bytes
            .as_deref()
            .map(rustyboi_session::sha256)
            .unwrap_or([0u8; 32]);
        self.session.replace_machine(*gb, rom_id);
        self.error_state = None;
        self.frame = None;
        self.persist_config();
    }

    /// Set the presentation palette and persist the equivalent DMG shades.
    fn set_palette(&mut self, palette: config::ColorPalette) {
        self.palette = palette;
        let mut cfg = self.session.config().clone();
        cfg.dmg_palette = rustyboi_session::config::DmgPalette {
            shades: palette.get_rgba_colors(),
        };
        self.session.set_config(cfg);
        self.persist_config();
    }

    fn set_rewind_enabled(&mut self, enabled: bool) {
        let mut cfg = self.session.config().clone();
        cfg.rewind.enabled = enabled;
        self.session.set_config(cfg);
        self.persist_config();
    }

    fn set_rewind_interval(&mut self, interval_frames: u32) {
        let mut cfg = self.session.config().clone();
        cfg.rewind.interval_frames = interval_frames.max(1);
        self.session.set_config(cfg);
        self.persist_config();
    }

    fn set_rewind_depth(&mut self, depth: usize) {
        let mut cfg = self.session.config().clone();
        cfg.rewind.depth = depth.max(1);
        self.session.set_config(cfg);
        self.persist_config();
    }

    /// Persist the session config through the storage port, logging on failure
    /// (a failed config write should never crash the emulator).
    fn persist_config(&mut self) {
        if let Err(e) = self.session.save_config() {
            println!("Failed to save config: {e}");
        }
    }

    /// Snapshot the session-owned state the menus render (current selections,
    /// slot list, run mode).
    fn ui_state(&self) -> rustyboi_egui_lib::actions::SessionUiState {
        use rustyboi_egui_lib::actions::{HardwareChoice, PaletteChoice, SessionUiState};
        let cfg = self.session.config();
        let hardware = match cfg.hardware {
            gb::Hardware::DMG | gb::Hardware::MGB => HardwareChoice::Dmg,
            gb::Hardware::SGB => HardwareChoice::Sgb,
            _ => HardwareChoice::Cgb,
        };
        let palette = match self.palette {
            config::ColorPalette::Grayscale => PaletteChoice::Grayscale,
            config::ColorPalette::OriginalGreen => PaletteChoice::OriginalGreen,
            config::ColorPalette::Blue => PaletteChoice::Blue,
            config::ColorPalette::Brown => PaletteChoice::Brown,
            config::ColorPalette::Red => PaletteChoice::Red,
        };
        SessionUiState {
            hardware,
            palette,
            rewind_enabled: cfg.rewind.enabled,
            rewind_interval_frames: cfg.rewind.interval_frames,
            rewind_depth: cfg.rewind.depth,
            sgb_border: self.sgb_border,
            fast_forward: self.is_fast_forward(),
            slots: self.list_slots(),
        }
    }

    fn enable_audio(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // The session installs its own capturing audio sink into the GB and
        // returns the produced samples from `run_frame`; the host output is a
        // pure sink we push those samples into. So we start the device here and
        // keep it in `World`, rather than installing it into the GB.
        let mut output = crate::audio::Output::new()?;
        output.start_device()?;
        self.audio = Some(output);
        Ok(())
    }

    fn load_state(&mut self, file_data: FileData) -> Result<String, Box<dyn std::error::Error>> {
        // Save the current ROM and BIOS paths before loading state
        let saved_rom_path = self.current_rom_path.clone();
        let saved_bios_path = self.current_bios_path.clone();

        // Deserialize into a fresh GB, then reattach the ROM/BIOS below.
        let (mut gb, filename) = match file_data {
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            FileData::Path(path) => {
                let filename = path.to_string_lossy().to_string();
                (gb::GB::from_state_file(&filename)?, filename)
            }
            #[cfg(any(target_arch = "wasm32", target_os = "android"))]
            FileData::Contents { name, data } => {
                (gb::GB::from_state_bytes(&data)?, name)
            }
        };

        // Reload the ROM if we had one loaded, re-deriving the ROM id from its
        // bytes so session slots stay keyed to the right game.
        let mut rom_bytes = None;
        if let Some(rom_path) = saved_rom_path {
            match std::fs::read(&rom_path)
                .map_err(|e| e.into())
                .and_then(|bytes| cartridge::Cartridge::from_bytes(&bytes).map(|c| (bytes, c)))
            {
                Ok((bytes, cartridge)) => {
                    gb.insert(cartridge);
                    rom_bytes = Some(bytes);
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
            match gb.load_bios(&bios_path) {
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

        let has_content = gb.has_rom() || gb.has_bios();
        let rom_id = rom_bytes
            .as_deref()
            .map(rustyboi_session::sha256)
            .unwrap_or([0u8; 32]);
        self.rom_bytes = rom_bytes;
        self.session.replace_machine(gb, rom_id);

        // Clear any error state
        self.error_state = None;

        // Clear the current frame
        self.frame = None;

        // If emulator was auto-paused due to no content and state has content, unpause it
        if self.auto_paused_no_content && has_content {
            self.is_paused = false;
            self.auto_paused_no_content = false;
        }

        println!("Game state loaded from: {}", filename);
        Ok(filename)
    }

    fn load_rom(&mut self, file_data: FileData) -> Result<String, Box<dyn std::error::Error>> {
        #[cfg(target_os = "android")]
        log::info!("load_rom: entering");
        let (filename, cartridge, rom_bytes, has_file_path) = match file_data {
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            FileData::Path(path) => {
                let filename = path.to_string_lossy().to_string();
                // Read the raw bytes ourselves so we can derive the session ROM
                // id (SHA-256) for savestate-slot keying.
                let bytes = std::fs::read(&filename)?;
                let cartridge = cartridge::Cartridge::from_bytes(&bytes)?;
                (filename, cartridge, Some(bytes), true)
            }
            #[cfg(any(target_arch = "wasm32", target_os = "android"))]
            FileData::Contents { name, data } => {
                #[cfg(target_os = "android")]
                log::info!("load_rom: parsing {} bytes ({})", data.len(), name);
                #[cfg(any(target_arch = "wasm32", target_os = "android"))]
                #[allow(unused_mut)]
                let mut cartridge = cartridge::Cartridge::from_bytes(&data).map_err(|e| {
                    #[cfg(target_os = "android")]
                    log::error!("load_rom: Cartridge::from_bytes failed: {e}");
                    e
                })?;
                // Battery-backed RAM persistence on Android lives entirely
                // outside the app's internal data dir: the Kotlin
                // `loadRomEntry` path opens (or creates) a sibling
                // `<rom-stem>.sav` next to the ROM via SAF and passes
                // a writable file descriptor through JNI. We pop that
                // fd here, wrap it in a `File`, and hand it to the
                // cartridge which then streams per-byte writes for
                // crash durability.
                //
                // If no fd is pending (e.g. legacy single-document
                // picker path), the cart runs without persistence and
                // we log a warning.
                #[cfg(target_os = "android")]
                {
                    if cartridge.has_battery() {
                        if let Some(fd) = crate::android::take_pending_sav_fd() {
                            // SAFETY: the fd was just handed to us by
                            // `ParcelFileDescriptor.detachFd()` in
                            // Kotlin; ownership transfers to this File
                            // which will close it on drop.
                            let file = unsafe {
                                use std::os::fd::FromRawFd;
                                std::fs::File::from_raw_fd(fd)
                            };
                            log::info!(
                                "load_rom: attaching SAF sav fd ({fd})"
                            );
                            if let Err(e) = cartridge.attach_save_file_from(file) {
                                log::error!(
                                    "load_rom: attach_save_file_from failed: {e}"
                                );
                            }
                        } else {
                            log::warn!(
                                "load_rom: battery-backed cart loaded \
                                 without a sav fd; saves WILL NOT \
                                 persist. Use the ROM Library to \
                                 enable persistence."
                            );
                        }
                    }
                }
                (name, cartridge, Some(data), false)
            }
        };

        // Build a fresh machine for the new cartridge, mirroring the startup
        // path (skip BIOS so games boot), then hand it to the session which
        // re-keys its slots to this ROM's id.
        let mut gb = Box::new(gb::GB::new(self.session.hardware()));
        gb.insert(cartridge);
        gb.skip_bios();
        let rom_id = rom_bytes
            .as_deref()
            .map(rustyboi_session::sha256)
            .unwrap_or([0u8; 32]);
        self.rom_bytes = rom_bytes;
        self.session.replace_machine(*gb, rom_id);

        // Track the current ROM path
        self.current_rom_path = if has_file_path {
            Some(filename.clone())
        } else {
            None // No file path for WASM content
        };

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
        // Reset the Game Boy to its initial state (same ROM/BIOS, same id).
        self.session.gb_mut().reset();
        // Rewind history is now stale (it points at the pre-reset timeline).
        self.session.clear_rewind();

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

    /// Convert the latest presented frame to an RGBA source ready for the
    /// [`GameRenderer`], preferring the 256x224 SGB border composite when the
    /// border toggle is on and the machine offers one. Returns the source size
    /// (so the renderer picks the matching texture) and the RGBA bytes.
    fn present(&self) -> Option<(crate::game_renderer::SourceSize, Vec<u8>)> {
        use crate::game_renderer::SourceSize;

        // SGB border composite (only Some on SGB hardware with a border loaded).
        if self.sgb_border
            && let Some(rgb) = self.session.gb().sgb_composited_frame()
        {
            let mut rgba = Vec::with_capacity((rgb.len() / 3) * 4);
            for chunk in rgb.chunks_exact(3) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            return Some((SourceSize::Sgb, rgba));
        }

        let gb_frame = self.frame.as_ref()?;
        let rgba = match gb_frame {
            gb::Frame::Monochrome(data) => convert_to_rgba(data, &self.palette).to_vec(),
            gb::Frame::Color(data) => {
                let mut rgba = vec![0u8; ppu::FRAMEBUFFER_SIZE * 4];
                for (i, chunk) in data.chunks(3).enumerate() {
                    let offset = i * 4;
                    rgba[offset] = chunk[0];
                    rgba[offset + 1] = chunk[1];
                    rgba[offset + 2] = chunk[2];
                    rgba[offset + 3] = 255;
                }
                rgba
            }
        };
        Some((SourceSize::Gb, rgba))
    }

    /// Run one frame directly on the core (bypassing the session), catching an
    /// emulator panic. Used by the debug stepping paths and the breakpoint-aware
    /// run; `collect_audio` is false there since audio is not presented while
    /// single-stepping. Returns the frame and whether a breakpoint was hit.
    fn run_frame_on_core(&mut self) -> Option<(gb::Frame, bool)> {
        let gb = self.session.gb_mut();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gb.run_until_frame(false)
        }));
        match result {
            Ok((frame, breakpoint_hit)) => Some((frame, breakpoint_hit)),
            Err(panic_info) => {
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

    /// Drain prints completed by an attached Game Boy Printer and write each
    /// as `<rom-stem>-print-<n>.png` next to the ROM (the same place the
    /// battery `.sav` lives). No-op unless a printer is attached and a game
    /// finished a print since the last call.
    fn drain_printer_sheets(&mut self) {
        let sheets = self.session.gb_mut().take_printer_sheets();
        if sheets.is_empty() {
            return;
        }
        #[cfg(any(target_arch = "wasm32", target_os = "android"))]
        {
            log::warn!("{} print(s) captured but this platform has no print sink", sheets.len());
        }
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            let stem = self
                .current_rom_path
                .as_ref()
                .map(|p| {
                    let path = std::path::Path::new(p);
                    path.with_extension("").to_string_lossy().into_owned()
                })
                .unwrap_or_else(|| "rustyboi".to_string());
            // Seed the print index from disk once per stem (first free slot),
            // then keep it monotonic in memory so back-to-back prints never
            // collide while an earlier async write is still in flight.
            let mut n = match &self.next_print_index {
                Some((s, i)) if *s == stem => *i,
                _ => {
                    let mut i = 1u32;
                    while std::path::Path::new(&format!("{stem}-print-{i}.png")).exists() {
                        i += 1;
                    }
                    i
                }
            };

            // Encode + disk write happen off the emulation thread; here we only
            // pick a free filename and hand the (cheap, Clone) sheet over.
            let worker = self
                .png_worker
                .get_or_insert_with(crate::png_worker::PngWorker::new);
            for sheet in sheets {
                let path = format!("{stem}-print-{n}.png");
                n += 1;
                worker.write_sheet(std::path::PathBuf::from(path), sheet);
            }
            self.next_print_index = Some((stem, n));
        }
    }

    /// Step exactly one instruction on the core (debug N key), catching a panic
    /// and surfacing the current (possibly incomplete) frame. Audio is not
    /// collected while single-stepping.
    fn step_one_instruction(&mut self, label: &str) {
        let gb = self.session.gb_mut();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_breakpoint_hit, _cycles) = gb.step_instruction(false);
            gb.get_current_frame()
        }));
        match result {
            Ok(frame) => self.frame = Some(frame),
            Err(panic_info) => {
                self.error_state = Some(panic_message(panic_info, label));
                self.frame = None;
            }
        }
    }

    fn update(&mut self) {
        self.drain_printer_sheets();
        // Handle single frame stepping (debug): run one frame on the core.
        if self.step_single_frame {
            self.step_single_frame = false;
            match self.run_frame_on_core() {
                Some((frame, _bp)) => self.frame = Some(frame),
                None => {
                    self.error_state = Some("Emulator crashed during frame step".to_string());
                    self.frame = None;
                }
            }
            return;
        }

        // Handle single cycle stepping (debug).
        if self.step_single_cycle {
            self.step_single_cycle = false;
            self.step_one_instruction("during cycle step");
            return;
        }

        // Handle multiple cycle stepping (debug).
        if let Some(count) = self.step_multiple_cycles.take() {
            let gb = self.session.gb_mut();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for _ in 0..count {
                    let (_breakpoint_hit, _cycles) = gb.step_instruction(false);
                }
                gb.get_current_frame()
            }));
            match result {
                Ok(frame) => self.frame = Some(frame),
                Err(panic_info) => {
                    self.error_state =
                        Some(panic_message(panic_info, &format!("during multi-cycle step ({count})")));
                    self.frame = None;
                }
            }
            return;
        }

        // Handle multiple frame stepping (debug).
        if let Some(count) = self.step_multiple_frames.take() {
            let mut final_frame = None;
            let mut success = true;
            for _ in 0..count {
                match self.run_frame_on_core() {
                    Some((frame, _bp)) => final_frame = Some(frame),
                    None => {
                        success = false;
                        break;
                    }
                }
            }
            if success {
                self.frame = final_frame;
            } else {
                self.error_state =
                    Some(format!("Emulator crashed during multi-frame step ({count})"));
                self.frame = None;
            }
            return;
        }

        // Frame-advance runs exactly one frame even while the World is paused
        // (the session's FrameAdvance mode auto-returns to Paused after). This
        // is what makes the Frame Advance hotkey / menu work while paused.
        if self.error_state.is_none()
            && matches!(self.session.mode(), rustyboi_session::RunMode::FrameAdvance)
        {
            let output = self.session.run_frame(self.input);
            self.pump_rewind_worker();
            self.frame = Some(output.frame);
            if let Some(audio) = self.audio.as_mut() {
                audio.push_samples(&output.audio);
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

        // Only update if enough time has passed. Fast-forward runs several GB
        // frames per presented frame (inside the session), so we still pace on
        // the presented-frame cadence and let the extra frames burst.
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
        } else if elapsed_since_last_frame.as_millis() > 25 {
            // Frame took too long
            println!("Slow frame: {}ms (target: {}ms)",
                    elapsed_since_last_frame.as_millis(),
                    TARGET_FRAME_TIME.as_millis());
        }

        self.last_frame_time = Instant::now();

        // Breakpoint debugging bypasses the session (it needs the per-frame
        // breakpoint-hit flag the session's run loop discards); otherwise the
        // whole feature stack — input remap, run mode, rewind capture, TAS,
        // cheats, audio capture — runs inside `Session::run_frame`.
        if self.session.gb().get_breakpoints().is_empty() {
            let output = self.session.run_frame(self.input);
            self.pump_rewind_worker();
            if output.advanced {
                self.frame = Some(output.frame);
                if let Some(audio) = self.audio.as_mut() {
                    audio.push_samples(&output.audio);
                }
                self.update_performance_metrics();
            } else {
                // Paused mode (e.g. after a frame-advance): keep last frame.
                self.frame = Some(output.frame);
            }
        } else {
            match self.run_frame_on_core() {
                Some((frame_data, breakpoint_hit)) => {
                    self.frame = Some(frame_data);
                    self.update_performance_metrics();
                    if breakpoint_hit {
                        self.is_paused = true;
                        self.breakpoint_hit = true;
                        println!(
                            "Breakpoint hit at PC: {:04X}",
                            self.session.gb().get_cpu_registers().pc
                        );
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

    /// Latch the host's classified button state as the session's abstract
    /// input for the next frame. The session applies the config remap; we don't
    /// touch the GB directly.
    fn set_input_state(&mut self, state: input::ButtonState) {
        use rustyboi_session::GbButton;
        let mut a = rustyboi_session::AbstractInput::none();
        a.set(GbButton::A, state.a);
        a.set(GbButton::B, state.b);
        a.set(GbButton::Start, state.start);
        a.set(GbButton::Select, state.select);
        a.set(GbButton::Up, state.up);
        a.set(GbButton::Down, state.down);
        a.set(GbButton::Left, state.left);
        a.set(GbButton::Right, state.right);
        self.input = a;
    }

    // Breakpoint management methods (host-side debug tooling → core directly).
    fn add_breakpoint(&mut self, address: u16) {
        self.session.gb_mut().add_breakpoint(address);
    }

    fn remove_breakpoint(&mut self, address: u16) {
        self.session.gb_mut().remove_breakpoint(address);
    }

    fn check_and_clear_breakpoint_hit(&mut self) -> bool {
        let hit = self.breakpoint_hit;
        self.breakpoint_hit = false;
        hit
    }
}

/// Format a caught emulator panic into a user-facing error string, tagged with
/// `context` (e.g. "during cycle step").
fn panic_message(panic_info: Box<dyn std::any::Any + Send>, context: &str) -> String {
    if let Some(s) = panic_info.downcast_ref::<&str>() {
        format!("Emulator panic {context}: {s}")
    } else if let Some(s) = panic_info.downcast_ref::<String>() {
        format!("Emulator panic {context}: {s}")
    } else {
        format!("Emulator panic {context}: Unknown error")
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

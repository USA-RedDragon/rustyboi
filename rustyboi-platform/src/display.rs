use crate::config;
use crate::framework::Framework;
use rustyboi_egui_lib::actions::GuiAction;
use rustyboi_egui_lib::actions::FileData;
use rustyboi_core_lib::{cartridge, gb, ppu, input};

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

    let (pixels, framework) = async {
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

        (pixels, framework)
    }.await;
    match run_gui_loop(event_loop, &window, Some(pixels), Some(framework), gb, &config) {
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
    run_gui_loop(event_loop, &window, None, None, gb, config)
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
    let r = run_gui_loop(event_loop, &window, None, None, gb, config);
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
) -> Result<(Pixels<'win>, Framework), Error> {
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
    Ok((pixels, framework))
}

fn run_gui_loop<'win>(
    event_loop: EventLoop<()>,
    window: &'win winit::window::Window,
    mut pixels: Option<Pixels<'win>>,
    mut framework: Option<Framework>,
    gb: Box<gb::GB>,
    config: &config::CleanConfig,
) -> Result<(), Error> {
    let mut input = WinitInputHelper::new();
    let mut world = World::new_with_paths(gb, config.rom.clone(), config.bios.clone(), config.palette);

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
    let mut f_key_processed_initial = false;
    let mut n_key_processed_initial = false;
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
                        Ok((p, f)) => {
                            pixels = Some(p);
                            framework = Some(f);
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
            }
            _ => {}
        }

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
                n_key_processed_initial = false;
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
                let (pixels, framework) = match (pixels.as_mut(), framework.as_mut()) {
                    (Some(p), Some(f)) => (p, f),
                    _ => return,
                };
                world.draw(pixels.frame_mut());
                let gui_paused_state = manually_paused || world.error_state.is_some();
                
                // Update window title with performance metrics
                world.update_window_title(window, gui_paused_state);
                // Always pass register data for the debug overlay, regardless of pause state
                let registers = Some(world.gb.get_cpu_registers());
                let gb_ref = Some(&*world.gb);
                let (gui_action, menu_open) = framework.prepare(window, gui_paused_state, registers, gb_ref);
                
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
                if let Some(framework) = framework.as_mut() {
                    framework.handle_event(window, &event);
                }
            }
            _ => (),
        }
    });
    res.map_err(|e| Error::UserDefined(Box::new(e)))
}

struct World {
    gb: Box<gb::GB>,
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
    fn new_with_paths(gb: Box<gb::GB>, rom_path: Option<String>, bios_path: Option<String>, palette: config::ColorPalette) -> Self {
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
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            FileData::Path(path) => {
                let filename = path.to_string_lossy().to_string();
                *self.gb = gb::GB::from_state_file(&filename)?;
                filename
            }
            #[cfg(any(target_arch = "wasm32", target_os = "android"))]
            FileData::Contents { name, data } => {
                // For WASM/Android, parse the bytes directly
                *self.gb = gb::GB::from_state_bytes(&data)?;
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
        #[cfg(target_os = "android")]
        log::info!("load_rom: entering");
        let (filename, cartridge, has_file_path) = match file_data {
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            FileData::Path(path) => {
                let filename = path.to_string_lossy().to_string();
                let cartridge = cartridge::Cartridge::load(&filename)?;
                (filename, cartridge, true)
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

    fn draw(&mut self, frame: &mut [u8]) {
        if let Some(gb_frame) = self.frame.as_ref() {
            let rgba_frame = match gb_frame {
                gb::Frame::Monochrome(data) => {
                    // Convert monochrome framebuffer to RGBA using the palette
                    convert_to_rgba(data, &self.palette)
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

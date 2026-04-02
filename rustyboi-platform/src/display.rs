//! The thin platform event loop. Creates the winit window + wgpu surface/device,
//! pumps winit events into abstract input + UI events, and drives the portable
//! `rustyboi_frontend::App` (which owns all UI/render/logic). Audio, file
//! dialogs, worker threads, and the Android JNI entry stay here; everything
//! window-agnostic lives in the frontend.

use crate::config;
use crate::error::PlatformError;
use rustyboi_core_lib::{gb, input};
use rustyboi_frontend_lib::actions::{FileData, GuiAction};
use rustyboi_frontend_lib::{App, PlatformRequest, Renderer, ResolvedAction, UiHost};
use rustyboi_session::Session;

use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(not(target_os = "android"))]
use winit::dpi::LogicalSize;
use winit::event::{Event, WindowEvent};
use winit::event_loop::EventLoop;
use winit::keyboard::KeyCode;
use winit::window::{Window, WindowBuilder};
use winit_input_helper::WinitInputHelper;

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use winit::platform::web::WindowExtWebSys;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 144;

/// GPU + UI state that is (re)created when a rendering surface appears and torn
/// down when it goes away (desktop startup/shutdown, Android foreground/back).
/// The `App` (Session + run state) persists across these cycles.
struct RenderState {
    renderer: Renderer,
    ui: UiHost,
}

#[cfg(target_arch = "wasm32")]
fn get_window_size() -> LogicalSize<f64> {
    let client_window = web_sys::window().unwrap();
    LogicalSize::new(
        client_window.inner_width().unwrap().as_f64().unwrap(),
        client_window.inner_height().unwrap().as_f64().unwrap(),
    )
}

/// Create the wgpu surface + device + queue from `window`, then build the
/// frontend `Renderer` and `UiHost`. This is the only place a raw window handle
/// touches wgpu; the resulting handles are window-agnostic afterwards. Safe API
/// throughout (`Arc<Window>` gives the surface a `'static` owning handle).
fn create_render_state<T>(
    event_loop: &winit::event_loop::EventLoopWindowTarget<T>,
    window: Arc<Window>,
    pending_dialog_result: Option<
        std::sync::Arc<std::sync::Mutex<Option<GuiAction>>>,
    >,
) -> Result<RenderState, PlatformError> {
    let size = window.inner_size();
    let width = size.width.max(1);
    let height = size.height.max(1);
    let scale_factor = window.scale_factor() as f32;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    // `Arc<Window>: Into<SurfaceTarget<'static>>` — no unsafe.
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| PlatformError::new(format!("create_surface: {e}")))?;

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: Some(&surface),
    }))
    .ok_or_else(|| PlatformError::new("no compatible wgpu adapter"))?;

    // Phase 1 is desktop (native backends): request the adapter's full default
    // limits so the egui atlas has room on hi-DPI. The web adapter (phase 2)
    // will request `downlevel_webgl2_defaults()` instead.
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("rustyboi_device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
        },
        None,
    ))
    .map_err(|e| PlatformError::new(format!("request_device: {e}")))?;

    // Pick a non-sRGB surface format if available so the game texture (uploaded
    // as *_Srgb) composites the same as before; fall back to the first format.
    let caps = surface.get_capabilities(&adapter);
    let surface_format = caps
        .formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .unwrap_or(caps.formats[0]);

    let max_texture_size = device.limits().max_texture_dimension_2d as usize;
    let renderer = Renderer::new(surface, device, queue, surface_format, width, height);
    let ui = UiHost::new(event_loop, scale_factor, max_texture_size, pending_dialog_result);

    Ok(RenderState { renderer, ui })
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn run_with_gui(gb: Box<gb::GB>, config: &config::CleanConfig) -> Result<(), PlatformError> {
    let event_loop = EventLoop::new().map_err(PlatformError::from_display)?;
    let size = LogicalSize::new(
        (WIDTH * (config.scale as u32)) as f64,
        (HEIGHT * (config.scale as u32)) as f64,
    );
    let window = WindowBuilder::new()
        .with_title("RustyBoi")
        .with_inner_size(size)
        .with_min_inner_size(LogicalSize::new(WIDTH as f64, HEIGHT as f64))
        .build(&event_loop)
        .map_err(PlatformError::from_display)?;
    run_gui_loop(event_loop, Arc::new(window), gb, config)
}

#[cfg(target_arch = "wasm32")]
pub async fn run_with_gui_async(gb: Box<gb::GB>, config: config::CleanConfig) {
    let event_loop = EventLoop::new().unwrap();
    let size = LogicalSize::new(
        (WIDTH * (config.scale as u32)) as f64,
        (HEIGHT * (config.scale as u32)) as f64,
    );
    let window = WindowBuilder::new()
        .with_title("RustyBoi")
        .with_inner_size(size)
        .with_min_inner_size(LogicalSize::new(WIDTH as f64, HEIGHT as f64))
        .build(&event_loop)
        .unwrap();

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
    let _ = window.request_inner_size(get_window_size());

    if let Err(e) = run_gui_loop(event_loop, window, gb, &config) {
        eprintln!("Error in GUI loop: {e}");
    }
}

/// Android entry. Builds an `EventLoop` bound to the supplied `AndroidApp` and
/// hands off to the shared loop; the render surface is created lazily on
/// `Event::Resumed`.
#[cfg(target_os = "android")]
pub fn run_with_gui_android(
    app: winit::platform::android::activity::AndroidApp,
    gb: Box<gb::GB>,
    config: &config::CleanConfig,
) -> Result<(), PlatformError> {
    use crate::android::raw_log;
    use winit::platform::android::EventLoopBuilderExtAndroid;

    raw_log("run_with_gui_android: building EventLoop");
    let event_loop = winit::event_loop::EventLoopBuilder::<()>::with_user_event()
        .with_android_app(app)
        .build()
        .map_err(|e| {
            raw_log(&format!("run_with_gui_android: EventLoop build failed: {e:?} ({e})"));
            log::error!("EventLoop build failed: {e:?} ({e})");
            PlatformError::new(format!("EventLoop build failed: {e}"))
        })?;
    raw_log("run_with_gui_android: EventLoop built");
    let window = WindowBuilder::new()
        .with_title("RustyBoi")
        .build(&event_loop)
        .map_err(|e| {
            raw_log(&format!("run_with_gui_android: Window build failed: {e:?} ({e})"));
            log::error!("Window build failed: {e:?} ({e})");
            PlatformError::new(format!("Window build failed: {e}"))
        })?;
    raw_log("run_with_gui_android: Window built, entering loop");
    let r = run_gui_loop(event_loop, Arc::new(window), gb, config);
    raw_log("run_with_gui_android: loop returned");
    r
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
        std::path::PathBuf::from(".")
    }
}

/// Current epoch seconds, for savestate-slot timestamps.
fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build a `Session` around an already-prepared `GB`, deriving the ROM id from
/// `rom_bytes` (all-zero when no cartridge is inserted).
fn session_from_gb(
    gb: Box<gb::GB>,
    rom_bytes: Option<&[u8]>,
    config: rustyboi_session::Config,
    ports: rustyboi_session::Ports,
) -> Session {
    let rom_id = rom_bytes.map(rustyboi_session::sha256).unwrap_or([0u8; 32]);
    Session::with_gb(*gb, config, ports, rom_id)
}

fn run_gui_loop(
    event_loop: EventLoop<()>,
    window: Arc<Window>,
    gb: Box<gb::GB>,
    config: &config::CleanConfig,
) -> Result<(), PlatformError> {
    let mut input = WinitInputHelper::new();

    let ports = crate::ports::build_ports(save_base());
    let mut session_config = rustyboi_session::Config::load(ports.storage.as_ref());
    session_config.hardware = config.hardware;

    let rom_bytes = config.rom.as_ref().and_then(|p| std::fs::read(p).ok());

    // `mut` only used on native, where offloaded rewind is enabled below.
    #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(unused_mut))]
    let mut session = session_from_gb(gb, rom_bytes.as_deref(), session_config, ports);

    // Native desktop: offloaded rewind capture (worker serializes off-thread).
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let mut rewind_worker = {
        session.set_rewind_offloaded(true);
        Some(crate::rewind_worker::RewindWorker::new())
    };
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let mut png_worker: Option<crate::png_worker::PngWorker> = None;
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let mut next_print_index: Option<(String, u32)> = None;

    let should_start_paused = !session.gb().has_rom() && !session.gb().has_bios();

    let mut app = App::new(
        session,
        config.palette,
        config.rom.clone(),
        config.bios.clone(),
        rom_bytes,
        should_start_paused,
    );

    if config.printer {
        app.gb_mut().attach_printer();
        println!("Game Boy Printer attached to the link port");
    }

    // Audio output device (cpal/rodio). The session returns samples from
    // run_frame; we push them into this pure sink.
    let mut audio = match crate::audio::Output::new().and_then(|mut o| {
        o.start_device()?;
        Ok(o)
    }) {
        Ok(o) => Some(o),
        Err(e) => {
            println!("Failed to initialize audio: {e}; continuing without audio");
            None
        }
    };

    // Persist the pending-dialog-result Arc across UiHost suspend/resume cycles
    // (Android's SAF picker destroys the surface and drops the UiHost).
    let pending_dialog_result: std::sync::Arc<
        std::sync::Mutex<Option<GuiAction>>,
    > = std::sync::Arc::new(std::sync::Mutex::new(None));

    let mut render_state: Option<RenderState> = None;

    // Track the presented content size (GB 160x144 vs SGB 256x224) so the window
    // auto-fits when an SGB border appears/disappears without an explicit toggle
    // (e.g. an SGB ROM booting from the CLI). Seeded to the GB size so a plain
    // DMG/CGB game never triggers a spurious resize.
    #[cfg(not(target_os = "android"))]
    let mut last_content_size = (WIDTH, HEIGHT);
    // Last window inner size (logical) we requested, so the continuous fit only
    // resizes when the target actually moves (avoids a resize/relayout feedback
    // loop). `None` until the first fit.
    #[cfg(not(target_os = "android"))]
    let mut last_fit_logical: Option<(u32, u32)> = None;
    // Debounced aspect-snap state. During an interactive resize the window must
    // follow the cursor freely (requesting a size every `Resized` fights the
    // compositor — the rapid back-and-forth). So we only record the desired
    // aspect-correct size as `pending_snap` and apply it once the resize has
    // settled (no `Resized` for `SNAP_DEBOUNCE`). `resize_burst_start` is the
    // size at the start of the current drag, used to pick the driving axis.
    #[cfg(not(target_os = "android"))]
    let mut pending_snap: Option<winit::dpi::PhysicalSize<u32>> = None;
    #[cfg(not(target_os = "android"))]
    let mut last_resize_at: Option<Instant> = None;
    #[cfg(not(target_os = "android"))]
    let mut resize_burst_start: Option<winit::dpi::PhysicalSize<u32>> = None;
    #[cfg(not(target_os = "android"))]
    const SNAP_DEBOUNCE: Duration = Duration::from_millis(140);

    // Debounce timing for the F (frame-step) and N (cycle-step) debug keys.
    const DEBOUNCE_DURATION: Duration = Duration::from_millis(250);
    const REPEAT_INTERVAL: Duration = Duration::from_millis(67);
    let mut f_key_press_time: Option<Instant> = None;
    let mut n_key_press_time: Option<Instant> = None;
    let mut f_last_repeat_time: Option<Instant> = None;
    let mut n_last_repeat_time: Option<Instant> = None;

    let res = event_loop.run(|event, elwt| {
        match &event {
            Event::Resumed => {
                if render_state.is_none() {
                    match create_render_state(elwt, window.clone(), Some(pending_dialog_result.clone())) {
                        Ok(rs) => {
                            render_state = Some(rs);
                            window.request_redraw();
                            #[cfg(target_os = "android")]
                            if let Some(rs) = render_state.as_mut() {
                                let state = crate::library::LibraryState::load();
                                rs.ui.library_panel_mut().set_recents(state.recents.clone());
                                if state.tree_uri.is_some() {
                                    if let Ok(mut slot) = pending_dialog_result.lock() {
                                        *slot = Some(GuiAction::SetLibraryTreeUri(state.tree_uri));
                                    }
                                } else {
                                    rs.ui
                                        .library_panel_mut()
                                        .set_status(Some("Pick your ROMs folder to get started.".into()));
                                }
                            }
                        }
                        Err(err) => {
                            println!("Failed to create render state on Resumed: {err}");
                            elwt.exit();
                            return;
                        }
                    }
                }
            }
            Event::Suspended => {
                render_state = None;
            }
            _ => {}
        }

        if input.update(&event) {
            if input.key_pressed(KeyCode::Escape) || input.close_requested() {
                elwt.exit();
                return;
            }

            // --- session feature hotkeys ---
            if input.key_pressed(KeyCode::F5) {
                match app.quicksave(now_epoch_secs()) {
                    Ok(()) => println!("Quicksaved"),
                    Err(e) => println!("Quicksave failed: {e}"),
                }
                window.request_redraw();
            }
            if input.key_pressed(KeyCode::F8) {
                match app.quickload() {
                    Ok(()) => window.request_redraw(),
                    Err(e) => println!("Quickload failed: {e}"),
                }
            }
            if input.key_pressed(KeyCode::Tab) {
                app.toggle_fast_forward();
            }
            if input.key_pressed(KeyCode::Backslash) {
                app.frame_advance();
                window.request_redraw();
            }
            if input.key_held(KeyCode::Backspace) && app.rewind_enabled() {
                app.rewind();
                window.request_redraw();
            }

            // F: frame stepping with debounce (paused/errored only).
            if input.key_pressed(KeyCode::KeyF) {
                if app.stepping_allowed() {
                    app.request_step_frame();
                    let now = Instant::now();
                    f_key_press_time = Some(now);
                    f_last_repeat_time = Some(now);
                    window.request_redraw();
                }
            } else if input.key_held(KeyCode::KeyF) {
                if app.stepping_allowed()
                    && let Some(press_time) = f_key_press_time
                    && press_time.elapsed() >= DEBOUNCE_DURATION
                    && let Some(last_repeat) = f_last_repeat_time
                    && last_repeat.elapsed() >= REPEAT_INTERVAL
                {
                    app.request_step_frame();
                    f_last_repeat_time = Some(Instant::now());
                    window.request_redraw();
                }
            } else {
                f_key_press_time = None;
                f_last_repeat_time = None;
            }

            // N: cycle stepping with debounce (paused/errored only).
            if input.key_pressed(KeyCode::KeyN) {
                if app.stepping_allowed() {
                    app.request_step_cycle();
                    let now = Instant::now();
                    n_key_press_time = Some(now);
                    n_last_repeat_time = Some(now);
                    window.request_redraw();
                }
            } else if input.key_held(KeyCode::KeyN) {
                if app.stepping_allowed()
                    && let Some(press_time) = n_key_press_time
                    && press_time.elapsed() >= DEBOUNCE_DURATION
                    && let Some(last_repeat) = n_last_repeat_time
                    && last_repeat.elapsed() >= REPEAT_INTERVAL
                {
                    app.request_step_cycle();
                    n_last_repeat_time = Some(Instant::now());
                    window.request_redraw();
                }
            } else {
                n_key_press_time = None;
                n_last_repeat_time = None;
            }

            if let Some(scale_factor) = input.scale_factor()
                && let Some(rs) = render_state.as_mut()
            {
                rs.ui.set_pixels_per_point(scale_factor as f32);
            }

            // Game Boy input from keybinds, OR'd with any egui touch controls.
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
            if let Some(rs) = render_state.as_ref() {
                let touch = rs.ui.touch_button_state();
                button_state.a |= touch.a;
                button_state.b |= touch.b;
                button_state.start |= touch.start;
                button_state.select |= touch.select;
                button_state.up |= touch.up;
                button_state.down |= touch.down;
                button_state.left |= touch.left;
                button_state.right |= touch.right;
            }
            app.set_button_state(button_state);

            // Advance one presented frame (paced inside the app), play audio,
            // pump the workers.
            let step = app.run_frame();
            if step.pump_workers {
                #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
                pump_workers(
                    &mut app,
                    rewind_worker.as_mut(),
                    &mut png_worker,
                    &mut next_print_index,
                );
                #[cfg(any(target_arch = "wasm32", target_os = "android"))]
                drain_printer_sheets_unsupported(&mut app);
            }
            if let Some(a) = audio.as_mut() {
                a.push_samples(&step.audio);
            }
            window.request_redraw();
        }

        match event {
            Event::WindowEvent { event: WindowEvent::Resized(size), .. } => {
                if let Some(rs) = render_state.as_mut() {
                    rs.renderer.resize(size.width.max(1), size.height.max(1));
                }
                // Aspect-lock (debounced): compute the aspect-correct size for
                // this resize but DON'T apply it now — requesting a size mid-drag
                // fights the compositor. Record it as `pending_snap`; the redraw
                // loop applies it once the resize settles. The window follows the
                // cursor freely during the drag (the game renders aspect-fit with
                // a transient bar), then snaps once on release.
                #[cfg(not(target_os = "android"))]
                {
                    let now = Instant::now();
                    // A gap since the last resize means a new drag burst began;
                    // baseline the driving-axis detection to this size.
                    let new_burst = last_resize_at
                        .map(|t| now.duration_since(t) > SNAP_DEBOUNCE)
                        .unwrap_or(true);
                    if new_burst {
                        resize_burst_start = Some(size);
                    }
                    last_resize_at = Some(now);

                    let (cw, ch) = app.content_size();
                    let aspect = cw as f32 / ch as f32;
                    let sf = window.scale_factor() as f32;
                    let (iw, ih) = app.content_inset();
                    let (iw_p, ih_p) = (iw * sf, ih * sf);
                    let (new_w, new_h) = (size.width as f32, size.height as f32);
                    let base = resize_burst_start.unwrap_or(size);
                    let dw = (new_w - base.width as f32).abs();
                    let dh = (new_h - base.height as f32).abs();
                    let (corr_w, corr_h) = if dh > dw {
                        let avail_h = (new_h - ih_p).max(1.0);
                        ((avail_h * aspect + iw_p).round(), new_h.round())
                    } else {
                        let avail_w = (new_w - iw_p).max(1.0);
                        (new_w.round(), (avail_w / aspect + ih_p).round())
                    };
                    let corr = winit::dpi::PhysicalSize::new(
                        (corr_w as u32).max(1),
                        (corr_h as u32).max(1),
                    );
                    pending_snap = if (corr.width as f32 - new_w).abs() > 1.0
                        || (corr.height as f32 - new_h).abs() > 1.0
                    {
                        Some(corr)
                    } else {
                        None
                    };
                }
            }
            Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } => {
                let Some(rs) = render_state.as_mut() else { return };

                if let Some(title) = app.title_if_due() {
                    window.set_title(&title);
                }

                // Keep the render surface locked to the live window size *before*
                // laying out egui. egui lays out using `window.inner_size()`; if
                // the surface (self.config) lags behind — which it does after a
                // programmatic `request_inner_size`, since the `Resized` event is
                // async — egui renders into a differently-sized target, scaling
                // the UI text. Syncing here (a cheap no-op when unchanged) makes
                // the layout size and the render target size always agree.
                {
                    let phys = window.inner_size();
                    let (pw, ph) = (phys.width.max(1), phys.height.max(1));
                    if (pw, ph) != rs.renderer.surface_size() {
                        rs.renderer.resize(pw, ph);
                    }
                }

                // Android IME: synthesize egui events winit drops (see below).
                let extra_events = collect_extra_egui_events();

                let requests = app.draw(&window, &mut rs.ui, &mut rs.renderer, extra_events, |action| {
                    resolve_gui_action(action)
                });

                for req in requests {
                    match req {
                        PlatformRequest::Exit => {
                            elwt.exit();
                            return;
                        }
                        PlatformRequest::ResizeContent { width, height } => {
                            // Just record the new content size; the continuous
                            // fit below sizes the window as content*scale + the
                            // measured chrome inset (menu bar / status panel) so
                            // the game fills the central rect with no letterbox.
                            #[cfg(not(target_os = "android"))]
                            {
                                last_content_size = (width, height);
                            }
                            #[cfg(target_os = "android")]
                            {
                                let _ = (width, height);
                            }
                        }
                        PlatformRequest::SaveStateBytes { path, bytes } => {
                            match std::fs::write(&path, &bytes) {
                                Ok(()) => rs.ui.set_status(format!("State saved to: {}", path.display())),
                                Err(e) => rs.ui.set_error(format!("Failed to save state: {e}")),
                            }
                        }
                        PlatformRequest::Status(s) => rs.ui.set_status(s),
                        PlatformRequest::Error(e) => rs.ui.set_error(e),
                        #[cfg(target_os = "android")]
                        PlatformRequest::AndroidLibrary(action) => {
                            handle_android_library(action, &mut rs.ui, &pending_dialog_result);
                        }
                    }
                }

                // Breakpoint-hit notification (surface the PC in the status bar).
                if app.take_breakpoint_hit() {
                    let pc = app.gb().get_cpu_registers().pc;
                    rs.ui.set_status(format!("Breakpoint hit at PC: ${pc:04X}"));
                }

                // Programmatic fit: size the window so the egui central rect is
                // exactly content*scale (game fills it, no bars). Target =
                // content*scale + the measured chrome inset. Fires ONLY on the
                // first frame (inset now known) and when the content size changes
                // (SGB border appearing/disappearing) — never continuously, so it
                // does not fight a user resize. These are not during a drag, so
                // request_inner_size is safe here.
                #[cfg(not(target_os = "android"))]
                {
                    let content = app.content_size();
                    let content_changed = content != last_content_size;
                    last_content_size = content;
                    if content_changed || last_fit_logical.is_none() {
                        let scale = config.scale.max(1) as u32;
                        let (inset_w, inset_h) = app.content_inset();
                        let target = (
                            (content.0 * scale + inset_w.round() as u32).max(1),
                            (content.1 * scale + inset_h.round() as u32).max(1),
                        );
                        last_fit_logical = Some(target);
                        let _ = window.request_inner_size(LogicalSize::new(
                            target.0 as f64,
                            target.1 as f64,
                        ));
                    }

                    // Apply a debounced aspect-snap once the user's resize has
                    // settled (no Resized for SNAP_DEBOUNCE). This is the only
                    // aspect correction that touches a user-driven size, and it
                    // fires after the drag ends, so it never fights the drag.
                    if let Some(snap) = pending_snap {
                        let settled = last_resize_at
                            .map(|t| t.elapsed() >= SNAP_DEBOUNCE)
                            .unwrap_or(false);
                        if settled {
                            pending_snap = None;
                            resize_burst_start = None;
                            let _ = window.request_inner_size(snap);
                        }
                    }
                }
            }
            Event::WindowEvent { event, .. } => {
                if let Some(rs) = render_state.as_mut() {
                    rs.ui.handle_event(&window, &event);
                }
            }
            _ => {}
        }
    });
    res.map_err(PlatformError::from_display)
}

/// Turn an OS-requiring UI action into bytes the app can apply. Handles the file
/// reads (`LoadRom`/`LoadState` with a `Path`, or content bytes on web/Android);
/// returns `None` for actions the app handles itself.
fn resolve_gui_action(action: &GuiAction) -> Option<ResolvedAction> {
    match action {
        GuiAction::LoadRom(file_data) => {
            let (bytes, path) = read_file_data(file_data)?;
            Some(ResolvedAction::LoadRom { bytes, path })
        }
        GuiAction::LoadState(file_data) => {
            let (state, _path) = read_file_data(file_data)?;
            // Re-attach the current ROM on a state load: the app reads it back
            // from disk via the reload_rom bytes we supply here. We don't have
            // the app's current ROM path in this stateless closure, so we let
            // the app keep its already-loaded cartridge (state deserialization
            // reinstates memory; the ROM bytes it already holds stay valid).
            Some(ResolvedAction::LoadState { state, reload_rom: None })
        }
        _ => None,
    }
}

/// Read the bytes + display name behind a `FileData` (a disk path on desktop, or
/// already-loaded content on web/Android).
fn read_file_data(file_data: &FileData) -> Option<(Vec<u8>, Option<String>)> {
    match file_data {
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        FileData::Path(path) => {
            let name = path.to_string_lossy().to_string();
            std::fs::read(path).ok().map(|b| (b, Some(name)))
        }
        #[cfg(any(target_arch = "wasm32", target_os = "android"))]
        FileData::Contents { name, data } => Some((data.clone(), Some(name.clone()))),
    }
}

/// Collect any extra egui events to inject before the UI runs. On Android this
/// diffs the GameTextInput buffer (winit 0.29 drops `TextEvent`) into egui
/// Text/Backspace events. Empty everywhere else.
fn collect_extra_egui_events() -> Vec<rustyboi_frontend_lib::egui_events::Event> {
    #[cfg(target_os = "android")]
    {
        crate::android::drain_ime_egui_events()
    }
    #[cfg(not(target_os = "android"))]
    {
        Vec::new()
    }
}

/// Drain a rewind snapshot to the background serializer, push back finished
/// blobs, and drain any finished printer sheets to the PNG worker. Called once
/// per emulated frame (native desktop only; wasm/Android keep inline capture).
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn pump_workers(
    app: &mut App,
    rewind_worker: Option<&mut crate::rewind_worker::RewindWorker>,
    png_worker: &mut Option<crate::png_worker::PngWorker>,
    next_print_index: &mut Option<(String, u32)>,
) {
    // Rewind: hand the cheap clone off-thread; push back completed serializes.
    if let Some(worker) = rewind_worker {
        if let Some((frame, gb)) = app.session_mut().take_pending_snapshot() {
            worker.submit(frame, gb);
        }
        for done in worker.drain_finished() {
            app.session_mut().push_rewind_bytes(done.frame, done.bytes);
        }
    }

    // Printer: drain finished sheets, encode + write off-thread.
    let sheets = app.gb_mut().take_printer_sheets();
    if sheets.is_empty() {
        return;
    }
    let stem = app
        .current_rom_path()
        .map(|p| std::path::Path::new(p).with_extension("").to_string_lossy().into_owned())
        .unwrap_or_else(|| "rustyboi".to_string());
    let mut n = match next_print_index.as_ref() {
        Some((s, i)) if *s == stem => *i,
        _ => {
            let mut i = 1u32;
            while std::path::Path::new(&format!("{stem}-print-{i}.png")).exists() {
                i += 1;
            }
            i
        }
    };
    let worker = png_worker.get_or_insert_with(crate::png_worker::PngWorker::new);
    for sheet in sheets {
        let path = format!("{stem}-print-{n}.png");
        n += 1;
        worker.write_sheet(std::path::PathBuf::from(path), sheet);
    }
    *next_print_index = Some((stem, n));
}

/// Service an Android ROM-library / SAF action the app handed back: it needs the
/// JNI bridge (`android_bridge`) + the library panel + persisted `LibraryState`,
/// all platform-owned. Ported from the old display event-loop Android arms.
#[cfg(target_os = "android")]
fn handle_android_library(
    action: GuiAction,
    ui: &mut UiHost,
    pending_dialog_result: &std::sync::Arc<std::sync::Mutex<Option<GuiAction>>>,
) {
    use rustyboi_frontend_lib::android_bridge;

    match action {
        GuiAction::OpenRomTree => {
            let pending = pending_dialog_result.clone();
            android_bridge::pick_tree(Box::new(move |uri| {
                if let Ok(mut slot) = pending.lock() {
                    *slot = Some(GuiAction::SetLibraryTreeUri(uri));
                }
            }));
        }
        GuiAction::RescanLibrary => {
            let tree_uri = ui.library_panel_mut().tree_uri().map(str::to_owned);
            if let Some(uri) = tree_uri {
                ui.library_panel_mut().begin_scan();
                let pending = pending_dialog_result.clone();
                android_bridge::scan_library(
                    uri,
                    Box::new(move |entries| {
                        if let Ok(mut slot) = pending.lock() {
                            *slot = Some(GuiAction::SetLibraryEntries(entries));
                        }
                    }),
                );
            } else {
                ui.library_panel_mut().set_status(Some("No library folder selected".into()));
            }
        }
        GuiAction::LoadRomFromUri(uri) => {
            let mut state = crate::library::LibraryState::load();
            state.touch_recent(&uri);
            state.save();
            ui.library_panel_mut().set_recents(state.recents.clone());
            let pending = pending_dialog_result.clone();
            android_bridge::load_rom_from_uri(
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
        GuiAction::SetLibraryTreeUri(uri) => {
            let mut state = crate::library::LibraryState::load();
            let tree_changed = state.tree_uri != uri;
            state.tree_uri = uri.clone();
            if tree_changed {
                state.cached_entries.clear();
            }
            state.save();
            ui.library_panel_mut().set_tree_uri(uri.clone());
            ui.library_panel_mut().set_entries(state.cached_entries.clone());
            if let Some(u) = uri {
                ui.library_panel_mut().begin_scan();
                let pending = pending_dialog_result.clone();
                android_bridge::scan_library(
                    u,
                    Box::new(move |entries| {
                        if let Ok(mut slot) = pending.lock() {
                            *slot = Some(GuiAction::SetLibraryEntries(entries));
                        }
                    }),
                );
            }
        }
        GuiAction::SetLibraryEntries(entries) => match entries {
            Some(entries) => {
                let mut state = crate::library::LibraryState::load();
                state.cached_entries = entries.clone();
                state.save();
                ui.library_panel_mut().set_entries(entries);
            }
            None => {
                ui.library_panel_mut().set_status(Some(
                    "Scan failed: tree no longer accessible. Re-pick the folder.".into(),
                ));
            }
        },
        _ => {}
    }
}

/// On wasm/Android there is no off-thread PNG sink; drain and warn (rewind stays
/// on the session's inline capture path there, so nothing else to pump).
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
fn drain_printer_sheets_unsupported(app: &mut App) {
    let sheets = app.gb_mut().take_printer_sheets();
    if !sheets.is_empty() {
        log::warn!("{} print(s) captured but this platform has no print sink", sheets.len());
    }
}

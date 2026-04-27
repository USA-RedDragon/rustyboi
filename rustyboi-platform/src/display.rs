//! The thin platform event loop. Creates the winit window + wgpu surface/device,
//! pumps winit events into abstract input + UI events, and drives the portable
//! `rustyboi_frontend::App` (which owns all UI/render/logic). Audio, file
//! dialogs, worker threads, and the Android JNI entry stay here; everything
//! window-agnostic lives in the frontend.

use crate::config;
use crate::error::PlatformError;
use rustyboi_core_lib::gb;
use rustyboi_frontend_lib::actions::{FileData, GuiAction};
use rustyboi_frontend_lib::{App, PlatformRequest, Renderer, ResolvedAction, UiHost};
use rustyboi_session::input_config::{FiredHotkey, HeldInputs, HotkeyAction, KeyName};
// Desktop (gilrs) + Android (native key events) both map physical pads to this.
#[cfg(not(target_arch = "wasm32"))]
use rustyboi_session::input_config::PadButton;
use rustyboi_session::Session;

use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(not(target_os = "android"))]
use winit::dpi::LogicalSize;
use winit::event::{Event, WindowEvent};
use winit::event_loop::EventLoop;
use winit::keyboard::KeyCode;
use winit::window::{Window, WindowBuilder};
// Fullscreen is only toggled on desktop (the Android window is already fullscreen).
#[cfg(not(target_os = "android"))]
use winit::window::Fullscreen;
use winit_input_helper::WinitInputHelper;

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use winit::platform::web::WindowExtWebSys;

// Used by the desktop + wasm window sizing; the Android entry sizes to the
// native surface, so these are unreferenced there.
#[cfg_attr(target_os = "android", allow(dead_code))]
const WIDTH: u32 = 160;
#[cfg_attr(target_os = "android", allow(dead_code))]
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
    // Pass the boxed GB straight through — never `*gb`, which would move the
    // ~207 KB machine onto the stack and overflow Android's main-thread stack.
    let mut session = Session::with_gb(gb, config, ports, rom_id);
    if let Some(bytes) = rom_bytes {
        session.set_rom_identity(bytes);
    }
    session
}

/// Collect the keyboard keys held this frame into a [`HeldInputs`] (pad filled
/// separately). Probes every bindable [`KeyName`] via winit `key_held`.
fn held_inputs_from_keyboard(input: &WinitInputHelper) -> HeldInputs {
    let mut held = HeldInputs::new();
    for k in KeyName::ALL {
        if input.key_held(rustyboi_frontend_lib::keymap::key_code(k)) {
            held.keys.insert(k);
        }
    }
    held
}

/// Map an Android `android.view.KeyEvent` gamepad keycode to a [`PadButton`].
/// winit has no gamepad backend on Android; controller buttons arrive as unmapped
/// native key events. Face/shoulder/trigger/start/select + a digital d-pad are
/// covered; analog sticks (delivered as motion events winit drops) are not.
#[cfg(target_os = "android")]
fn android_pad_button(code: u32) -> Option<PadButton> {
    Some(match code {
        96 => PadButton::South,          // BUTTON_A
        97 => PadButton::East,           // BUTTON_B
        99 => PadButton::West,           // BUTTON_X
        100 => PadButton::North,         // BUTTON_Y
        102 => PadButton::LeftShoulder,  // BUTTON_L1
        103 => PadButton::RightShoulder, // BUTTON_R1
        104 => PadButton::LeftTrigger,   // BUTTON_L2
        105 => PadButton::RightTrigger,  // BUTTON_R2
        108 => PadButton::Start,         // BUTTON_START
        109 => PadButton::Select,        // BUTTON_SELECT
        19 => PadButton::DpadUp,         // DPAD_UP
        20 => PadButton::DpadDown,       // DPAD_DOWN
        21 => PadButton::DpadLeft,       // DPAD_LEFT
        22 => PadButton::DpadRight,      // DPAD_RIGHT
        _ => return None,
    })
}

/// Fold all connected gamepads' held buttons into the abstract [`PadButton`] set:
/// standard face/shoulder/trigger buttons + D-pad via the hat OR the left stick.
/// Draining `next_event` refreshes the cached state `is_pressed`/`value` read.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn collect_gamepad_held(gilrs: &mut gilrs::Gilrs, pad: &mut std::collections::HashSet<PadButton>) {
    use gilrs::{Axis, Button};
    while gilrs.next_event().is_some() {}
    const DZ: f32 = 0.5;
    for (_id, gp) in gilrs.gamepads() {
        let mut hold = |cond: bool, b: PadButton| {
            if cond {
                pad.insert(b);
            }
        };
        hold(gp.is_pressed(Button::South), PadButton::South);
        hold(gp.is_pressed(Button::East), PadButton::East);
        hold(gp.is_pressed(Button::West), PadButton::West);
        hold(gp.is_pressed(Button::North), PadButton::North);
        hold(gp.is_pressed(Button::Start), PadButton::Start);
        hold(gp.is_pressed(Button::Select), PadButton::Select);
        hold(gp.is_pressed(Button::LeftTrigger), PadButton::LeftShoulder);
        hold(gp.is_pressed(Button::RightTrigger), PadButton::RightShoulder);
        hold(gp.is_pressed(Button::LeftTrigger2), PadButton::LeftTrigger);
        hold(gp.is_pressed(Button::RightTrigger2), PadButton::RightTrigger);
        hold(gp.is_pressed(Button::DPadUp), PadButton::DpadUp);
        hold(gp.is_pressed(Button::DPadDown), PadButton::DpadDown);
        hold(gp.is_pressed(Button::DPadLeft), PadButton::DpadLeft);
        hold(gp.is_pressed(Button::DPadRight), PadButton::DpadRight);
        // Analog sticks as discrete directions past a deadzone (gilrs: +Y up,
        // +X right). Bound alongside the d-pad by default, but separately
        // mappable — so the sticks and d-pad are interchangeable.
        hold(gp.value(Axis::LeftStickY) > DZ, PadButton::LStickUp);
        hold(gp.value(Axis::LeftStickY) < -DZ, PadButton::LStickDown);
        hold(gp.value(Axis::LeftStickX) < -DZ, PadButton::LStickLeft);
        hold(gp.value(Axis::LeftStickX) > DZ, PadButton::LStickRight);
        hold(gp.value(Axis::RightStickY) > DZ, PadButton::RStickUp);
        hold(gp.value(Axis::RightStickY) < -DZ, PadButton::RStickDown);
        hold(gp.value(Axis::RightStickX) < -DZ, PadButton::RStickLeft);
        hold(gp.value(Axis::RightStickX) > DZ, PadButton::RStickRight);
    }
}

/// Perform a fired hotkey on the desktop app. Returns `true` if the event loop
/// should exit (Exit action). Turbo is handled inside the resolver (it drives
/// the button state), so no dispatch is needed here for it. FastForward/Rewind
/// are hold actions (fire every active frame); the rest fire on the rising edge.
#[cfg_attr(target_os = "android", allow(unused_variables))]
fn dispatch_hotkey(
    app: &mut App,
    fired: FiredHotkey,
    window: &Window,
    elwt: &winit::event_loop::EventLoopWindowTarget<()>,
    is_fullscreen: &mut bool,
) -> bool {
    match fired.action {
        HotkeyAction::FastForward => {
            // Hold action: engage on the rising edge, released when the chord
            // drops (no longer fired) — handled by the caller each frame.
            if fired.rising && !app.is_fast_forward() {
                app.toggle_fast_forward();
            }
        }
        HotkeyAction::Rewind => {
            if app.rewind_enabled() {
                app.rewind();
                window.request_redraw();
            }
        }
        HotkeyAction::Quicksave if fired.rising => {
            match app.quicksave(now_epoch_secs()) {
                Ok(()) => println!("Quicksaved"),
                Err(e) => println!("Quicksave failed: {e}"),
            }
            window.request_redraw();
        }
        HotkeyAction::Quickload if fired.rising => match app.quickload() {
            Ok(()) => window.request_redraw(),
            Err(e) => println!("Quickload failed: {e}"),
        },
        HotkeyAction::FrameAdvance if fired.rising => {
            app.frame_advance();
            window.request_redraw();
        }
        HotkeyAction::TogglePause if fired.rising => {
            app.toggle_pause();
            window.request_redraw();
        }
        HotkeyAction::ToggleFullscreen if fired.rising => {
            #[cfg(not(target_os = "android"))]
            {
                *is_fullscreen = !*is_fullscreen;
                window.set_fullscreen(is_fullscreen.then(|| Fullscreen::Borderless(None)));
            }
        }
        HotkeyAction::Exit if fired.rising => {
            elwt.exit();
            return true;
        }
        _ => {}
    }
    false
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

    // Native desktop: physical gamepad support (gilrs). `None` if no backend is
    // available; buttons are OR'd into the keyboard/touch input each frame.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let mut gilrs = gilrs::Gilrs::new().ok();
    // Android has no gilrs backend: controller buttons arrive as native key
    // events. Track the held pad-button set here and merge into HeldInputs.
    #[cfg(target_os = "android")]
    let mut android_pad: std::collections::HashSet<PadButton> = std::collections::HashSet::new();

    // Cheat-DB HTTP fetch worker (desktop + Android; wasm uses browser fetch).
    // Created lazily on the first `Get cheats` so a session that never fetches
    // pays nothing.
    #[cfg(not(target_arch = "wasm32"))]
    let mut fetch_worker: Option<crate::fetch_worker::FetchWorker> = None;

    // Per-frame edge/phase state for the shared input resolver (hotkey rising
    // edges + the turbo autofire square wave). Persists across frames.
    let mut resolve_state = rustyboi_session::ResolveState::new();

    let should_start_paused = !session.gb().has_rom() && !session.gb().has_bios();

    let mut app = App::new(
        session,
        config.palette,
        config.rom.clone(),
        config.bios.clone(),
        should_start_paused,
    );

    if config.printer {
        app.gb_mut().attach_printer();
        println!("Game Boy Printer attached to the link port");
    }

    // No-Intro game-name index: load cached DATs immediately, download any that
    // are missing. The data is CC-BY-SA-4.0 libretro-database material that is
    // never embedded in the binary; `no_intro_fetch_urls` logs the attribution.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let urls = app.session().no_intro_fetch_urls();
        let (cached, missing) = crate::no_intro_cache::split_cached(&save_base(), &urls);
        if !cached.is_empty() {
            app.session_mut().finish_no_intro_dats(&cached);
        }
        for url in missing {
            fetch_worker
                .get_or_insert_with(crate::fetch_worker::FetchWorker::new)
                .submit(vec![url], rustyboi_session::FetchPurpose::NoIntro);
        }
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

    // Tracks whether the borderless-fullscreen toggle is currently on.
    #[cfg(not(target_os = "android"))]
    let mut is_fullscreen = false;

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

        // Android gamepad: face/shoulder/start/select buttons arrive as unmapped
        // native key events (winit_input_helper only tracks KeyCode-mapped keys).
        #[cfg(target_os = "android")]
        if let Event::WindowEvent { event: WindowEvent::KeyboardInput { event: key, .. }, .. } = &event {
            use winit::keyboard::{NativeKeyCode, PhysicalKey};
            if let PhysicalKey::Unidentified(NativeKeyCode::Android(code)) = key.physical_key {
                if let Some(pb) = android_pad_button(code) {
                    if key.state == winit::event::ElementState::Pressed {
                        android_pad.insert(pb);
                    } else {
                        android_pad.remove(&pb);
                    }
                }
            }
        }

        if input.update(&event) {
            if input.key_pressed(KeyCode::Escape) || input.close_requested() {
                elwt.exit();
                return;
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

            // Build the raw held-input set (keyboard + gamepad) and resolve it
            // through the shared config: GB-button bindings drive the button
            // state, chord hotkeys drive features. Then OR the egui touch overlay
            // on top and dispatch any fired hotkeys.
            #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(unused_mut))]
            let mut held = held_inputs_from_keyboard(&input);
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            if let Some(g) = gilrs.as_mut() {
                collect_gamepad_held(g, &mut held.pad);
            }
            #[cfg(target_os = "android")]
            {
                held.pad.extend(android_pad.iter().copied());
                // Analog sticks + hat arrive via Java (onGenericMotionEvent →
                // JNI). Android axes: +X right, +Y down. Hat covers controllers
                // that report the d-pad as an axis rather than key events.
                let [lx, ly, rx, ry, hx, hy, lt, rt] = crate::android::gamepad_axes();
                let dz = 0.5;
                let mut on = |cond: bool, b: PadButton| {
                    if cond {
                        held.pad.insert(b);
                    }
                };
                on(ly < -dz, PadButton::LStickUp);
                on(ly > dz, PadButton::LStickDown);
                on(lx < -dz, PadButton::LStickLeft);
                on(lx > dz, PadButton::LStickRight);
                on(ry < -dz, PadButton::RStickUp);
                on(ry > dz, PadButton::RStickDown);
                on(rx < -dz, PadButton::RStickLeft);
                on(rx > dz, PadButton::RStickRight);
                on(hy < -dz, PadButton::DpadUp);
                on(hy > dz, PadButton::DpadDown);
                on(hx < -dz, PadButton::DpadLeft);
                on(hx > dz, PadButton::DpadRight);
                // Analog L2/R2 rest at 0, pressed toward 1.
                on(lt > dz, PadButton::LeftTrigger);
                on(rt > dz, PadButton::RightTrigger);
            }
            let (mut button_state, fired) =
                app.session().config().input.resolve(&held, &mut resolve_state);
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
            // Forward the held pad set so the keybind editor can capture gamepad
            // presses (egui never sees pad input).
            app.set_held_pad(held.pad.clone());

            // Fast-forward is a hold action: keep it engaged only while the
            // chord is active this frame, so releasing the chord turns it off.
            let ff_active = fired
                .iter()
                .any(|f| matches!(f.action, HotkeyAction::FastForward));
            if !ff_active && app.is_fast_forward() {
                app.toggle_fast_forward();
            }
            for f in fired {
                #[cfg(not(target_os = "android"))]
                let exit = dispatch_hotkey(&mut app, f, &window, elwt, &mut is_fullscreen);
                #[cfg(target_os = "android")]
                let exit = {
                    let mut dummy = false;
                    dispatch_hotkey(&mut app, f, &window, elwt, &mut dummy)
                };
                if exit {
                    return;
                }
            }

            // Android: keep the game region inside the safe area (system bars /
            // display cutout) so it isn't clipped behind them. No-op elsewhere.
            #[cfg(target_os = "android")]
            if let Some(rs) = render_state.as_ref() {
                let (w, h) = rs.renderer.surface_size();
                let (l, t, r, b) = crate::android::safe_area_insets(w, h);
                app.set_safe_insets(l, t, r, b);
            }

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

                // Deliver any completed cheat-DB fetches into the session so the
                // cheat picker shows them; report the outcome in the status bar.
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(worker) = fetch_worker.as_mut() {
                    for done in worker.drain_finished() {
                        use rustyboi_session::FetchPurpose;
                        match (done.purpose, done.result) {
                            (FetchPurpose::Cheats, Ok(body)) => {
                                let n = app.session_mut().finish_fetched_cheats(&body);
                                if n == 0 {
                                    rs.ui.set_status("No cheats found for this game".into());
                                } else {
                                    rs.ui.set_status(format!("Fetched {n} cheats"));
                                }
                            }
                            (FetchPurpose::NoIntro, Ok(body)) => {
                                // Cache the downloaded DAT so we don't re-fetch it
                                // next launch, then feed it into the index and
                                // re-resolve the current ROM's display name.
                                if let Some(url) = done.url.as_deref() {
                                    crate::no_intro_cache::store(&save_base(), url, &body);
                                }
                                app.session_mut()
                                    .finish_no_intro_dats(std::slice::from_ref(&body));
                                if let Some(title) = app.title_if_due() {
                                    window.set_title(&title);
                                }
                            }
                            (FetchPurpose::Cheats, Err(e)) => {
                                // A failed cheat fetch is not fatal — surface it in
                                // the status bar, never the crash screen.
                                rs.ui.set_status(format!("Cheat fetch failed: {e}"));
                            }
                            (FetchPurpose::NoIntro, Err(e)) => {
                                // No-Intro identification is best-effort; a failed
                                // DAT download just leaves games on their header
                                // titles. Log, don't nag the user.
                                log::warn!("No-Intro DAT fetch failed: {e}");
                            }
                        }
                    }
                }

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
                        PlatformRequest::ToggleFullscreen => {
                            #[cfg(not(target_os = "android"))]
                            {
                                is_fullscreen = !is_fullscreen;
                                window.set_fullscreen(
                                    is_fullscreen.then(|| Fullscreen::Borderless(None)),
                                );
                            }
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
                        PlatformRequest::SaveBytes { suggested_name, bytes } => {
                            match save_bytes_to_file(&suggested_name, &bytes) {
                                Ok(Some(path)) => rs.ui.set_status(format!("Saved to: {}", path.display())),
                                Ok(None) => {}
                                Err(e) => rs.ui.set_error(format!("Failed to save file: {e}")),
                            }
                        }
                        PlatformRequest::Status(s) => rs.ui.set_status(s),
                        PlatformRequest::Error(e) => rs.ui.set_error(e),
                        PlatformRequest::ClearError => rs.ui.clear_error(),
                        // ROM/state loads + battery/RTC imports are resolved inside
                        // `App::draw` (they need the file resolver), so this arm is
                        // unreachable on desktop/Android; kept for the shared
                        // contract (the web worker services it). Log if it fires.
                        PlatformRequest::LoadFile { .. } => {
                            log::warn!("LoadFile request reached the platform loop unexpectedly");
                        }
                        PlatformRequest::FetchUrl { urls, purpose } => {
                            fetch_worker
                                .get_or_insert_with(crate::fetch_worker::FetchWorker::new)
                                .submit(urls, purpose);
                        }
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
        GuiAction::LoadState(file_data) | GuiAction::ImportState(file_data) => {
            let (state, _path) = read_file_data(file_data)?;
            // Re-attach the current ROM on a state load: the app reads it back
            // from disk via the reload_rom bytes we supply here. We don't have
            // the app's current ROM path in this stateless closure, so we let
            // the app keep its already-loaded cartridge (state deserialization
            // reinstates memory; the ROM bytes it already holds stay valid).
            Some(ResolvedAction::LoadState { state, reload_rom: None })
        }
        GuiAction::ImportBatterySave(file_data) => {
            let (bytes, _path) = read_file_data(file_data)?;
            Some(ResolvedAction::ImportBattery { bytes })
        }
        GuiAction::ImportRtc(file_data) => {
            let (bytes, _path) = read_file_data(file_data)?;
            Some(ResolvedAction::ImportRtc { bytes })
        }
        GuiAction::ApplyPatch(file_data) => {
            let (bytes, _path) = read_file_data(file_data)?;
            Some(ResolvedAction::ApplyPatch { bytes })
        }
        _ => None,
    }
}

/// Deliver export bytes to a user-chosen file (File → Export battery/RTC/state).
/// Desktop pops a native save dialog seeded with `suggested_name`; Android writes
/// into the app files dir under that name. Returns the written path, or `None`
/// when the user cancelled the dialog.
fn save_bytes_to_file(
    suggested_name: &str,
    bytes: &[u8],
) -> Result<Option<std::path::PathBuf>, std::io::Error> {
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    {
        let Some(path) = rfd::FileDialog::new().set_file_name(suggested_name).save_file() else {
            return Ok(None);
        };
        std::fs::write(&path, bytes)?;
        Ok(Some(path))
    }
    #[cfg(target_os = "android")]
    {
        let path = crate::android::save_dir().join(suggested_name);
        std::fs::write(&path, bytes)?;
        Ok(Some(path))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (suggested_name, bytes);
        Ok(None)
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

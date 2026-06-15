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
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::KeyCode;
use winit::window::{Window, WindowId};
// Fullscreen is only toggled on desktop (the Android window is already fullscreen).
#[cfg(not(target_os = "android"))]
use winit::window::Fullscreen;
use winit_input_helper::WinitInputHelper;

// The platform crate's own wasm rendering path is legacy — the web frontend is
// `rustyboi-web`, and nothing builds this crate for wasm. These winit-0.29-era
// imports are kept cfg-gated so native/Android builds ignore them.
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use winit::event::Event;
#[cfg(target_arch = "wasm32")]
use winit::window::WindowBuilder;
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
fn create_render_state(
    window: Arc<Window>,
    pending_dialog_result: Option<
        std::sync::Arc<std::sync::Mutex<Option<GuiAction>>>,
    >,
) -> Result<RenderState, PlatformError> {
    let size = window.inner_size();
    let width = size.width.max(1);
    let height = size.height.max(1);
    let scale_factor = window.scale_factor() as f32;

    // wgpu 29's `InstanceDescriptor` holds a non-Default `display` handle field,
    // so it can't be spread from `Default::default()`; set fields explicitly.
    let make_instance = |backends| {
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        })
    };
    // Prefer a Vulkan-only instance when a Vulkan adapter is actually available.
    // `Backends::all()` also spins up the GL/Mesa backend, which drags in LLVM
    // (the Mesa shader compiler), tens of MB of resident code we never use on a
    // Vulkan GPU. Try Vulkan first; only fall back to all backends when Vulkan
    // can't supply a compatible adapter (older GL-only hardware, some VMs).
    let try_backends =
        |backends| -> Option<(wgpu::Instance, wgpu::Surface<'static>, wgpu::Adapter)> {
            let instance = make_instance(backends);
            let surface = instance.create_surface(window.clone()).ok()?;
            let adapter = pollster::block_on(instance.request_adapter(
                &wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::default(),
                    force_fallback_adapter: false,
                    compatible_surface: Some(&surface),
                },
            ))
            .ok()?;
            Some((instance, surface, adapter))
        };
    let (_instance, surface, adapter) = try_backends(wgpu::Backends::VULKAN)
        .or_else(|| try_backends(wgpu::Backends::all()))
        .ok_or_else(|| PlatformError::new("no compatible wgpu adapter".to_string()))?;
    log::info!("wgpu adapter: {:?}", adapter.get_info());

    let mut limits = wgpu::Limits::downlevel_defaults();
    limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rustyboi_device"),
        required_features: wgpu::Features::empty(),
        required_limits: limits,
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    }))
    .map_err(|e| PlatformError::new(format!("request_device: {e}")))?;

    // Pick a non-sRGB surface format if available so the game texture (uploaded
    // as *_Srgb) composites the same as before; fall back to the first format.
    let caps = surface.get_capabilities(&adapter);
    // Prefer Mailbox: it presents without blocking on vsync, so emulation isn't
    // stalled during the present and the audio ring can hold a much smaller
    // cushion. Fall back to Fifo (always supported) where Mailbox isn't offered.
    let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    };
    log::info!("surface present modes: {:?}; using {:?}", caps.present_modes, present_mode);
    let surface_format = caps
        .formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .unwrap_or(caps.formats[0]);

    let max_texture_size = device.limits().max_texture_dimension_2d as usize;
    let renderer = Renderer::new(surface, device, queue, surface_format, width, height, present_mode);
    let ui = UiHost::new(&window, scale_factor, max_texture_size, pending_dialog_result);

    Ok(RenderState { renderer, ui })
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn run_with_gui(gb: Box<gb::GB>, config: &config::CleanConfig) -> Result<(), PlatformError> {
    // winit 0.30: the window is created inside `ApplicationHandler::resumed`, so
    // the entry point just builds the event loop and hands off to the shared
    // handler (see `run_gui_loop`).
    let event_loop = EventLoop::new().map_err(PlatformError::from_display)?;
    run_gui_loop(event_loop, gb, config)
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
    // winit 0.30 moved `with_user_event` from `EventLoopBuilder` onto `EventLoop`.
    let event_loop = winit::event_loop::EventLoop::<()>::with_user_event()
        .with_android_app(app)
        .build()
        .map_err(|e| {
            raw_log(&format!("run_with_gui_android: EventLoop build failed: {e:?} ({e})"));
            log::error!("EventLoop build failed: {e:?} ({e})");
            PlatformError::new(format!("EventLoop build failed: {e}"))
        })?;
    // The window is created lazily in `ApplicationHandler::resumed` (winit 0.30).
    raw_log("run_with_gui_android: EventLoop built, entering loop");
    let r = run_gui_loop(event_loop, gb, config);
    raw_log("run_with_gui_android: loop returned");
    r
}

/// Base directory the session ports read/write savestates + config under.
fn save_base() -> std::path::PathBuf {
    #[cfg(target_os = "android")]
    {
        crate::android::save_dir()
    }
    #[cfg(target_os = "ios")]
    {
        crate::ios::save_dir()
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android"), not(target_os = "ios")))]
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
#[cfg(any(target_os = "android", test))]
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

/// Pure inset math for [`crate::android::safe_area_insets`]: the gap `(left, top,
/// right, bottom)` in surface pixels between the full surface and the content
/// rect (system bars + display cutout). Split out here (host-compiled) so it can
/// be unit-tested without the Android runtime that supplies the rect. A
/// degenerate/empty rect (before the first insets arrive) yields no insets so the
/// game region is never collapsed to nothing.
#[cfg(any(target_os = "android", test))]
pub(crate) fn safe_insets_from_rect(
    surface_w: u32,
    surface_h: u32,
    rect_left: i32,
    rect_top: i32,
    rect_right: i32,
    rect_bottom: i32,
) -> (f32, f32, f32, f32) {
    if rect_right <= rect_left || rect_bottom <= rect_top {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let left = rect_left.max(0) as f32;
    let top = rect_top.max(0) as f32;
    let right = (surface_w as i32 - rect_right).max(0) as f32;
    let bottom = (surface_h as i32 - rect_bottom).max(0) as f32;
    (left, top, right, bottom)
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
    event_loop: &ActiveEventLoop,
    is_fullscreen: &mut bool,
) -> bool {
    match fired.action {
        HotkeyAction::FastForward => {
            // Fully handled once per frame by `App::tick_fast_forward_hold`
            // (engage on hold, release on chord drop, leave menu latch alone),
            // so there is nothing to do on the per-fired-hotkey pass.
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
            event_loop.exit();
            return true;
        }
        _ => {}
    }
    false
}

fn run_gui_loop(
    event_loop: EventLoop<()>,
    gb: Box<gb::GB>,
    config: &config::CleanConfig,
) -> Result<(), PlatformError> {
    let input = WinitInputHelper::new();

    let ports = crate::ports::build_ports(save_base());
    let mut session_config = rustyboi_session::Config::load(ports.storage.as_ref());
    session_config.hardware = config.hardware;

    // `mut` only used on native, where offloaded rewind is enabled below.
    #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(unused_mut))]
    let mut session = {
        session_from_gb(gb, config.rom.as_ref().and_then(|p| std::fs::read(p).ok()).as_deref(), session_config, ports)
    };

    // Native desktop: offloaded rewind capture (worker serializes off-thread).
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let rewind_worker = {
        session.set_rewind_offloaded(true);
        Some(crate::rewind_worker::RewindWorker::new())
    };
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let png_worker: Option<crate::png_worker::PngWorker> = None;
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let next_print_index: Option<(String, u32)> = None;

    // Native desktop: physical gamepad support (gilrs). `None` if no backend is
    // available; buttons are OR'd into the keyboard/touch input each frame.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let gilrs = gilrs::Gilrs::new().ok();
    // Android has no gilrs backend: controller buttons arrive as native key
    // events. Track the held pad-button set here and merge into HeldInputs.
    #[cfg(target_os = "android")]
    let android_pad: std::collections::HashSet<PadButton> = std::collections::HashSet::new();

    // Cheat-DB HTTP fetch worker (desktop + Android; wasm uses browser fetch).
    // Created lazily on the first `Get cheats` so a session that never fetches
    // pays nothing.
    #[cfg(not(target_arch = "wasm32"))]
    let mut fetch_worker: Option<crate::fetch_worker::FetchWorker> = None;

    // Per-frame edge/phase state for the shared input resolver (hotkey rising
    // edges + the turbo autofire square wave). Persists across frames.
    let resolve_state = rustyboi_session::ResolveState::new();

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
    let audio = match crate::audio::Output::new().and_then(|mut o| {
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

    let render_state: Option<RenderState> = None;

    // Track the presented content size (GB 160x144 vs SGB 256x224) so the window
    // auto-fits when an SGB border appears/disappears without an explicit toggle
    // (e.g. an SGB ROM booting from the CLI). Seeded to the GB size so a plain
    // DMG/CGB game never triggers a spurious resize.
    #[cfg(not(target_os = "android"))]
    let last_content_size = (WIDTH, HEIGHT);
    // Last window inner size (logical) we requested, so the continuous fit only
    // resizes when the target actually moves (avoids a resize/relayout feedback
    // loop). `None` until the first fit.
    #[cfg(not(target_os = "android"))]
    let last_fit_logical: Option<(u32, u32)> = None;
    // Debounced aspect-snap state. During an interactive resize the window must
    // follow the cursor freely (requesting a size every `Resized` fights the
    // compositor — the rapid back-and-forth). So we only record the desired
    // aspect-correct size as `pending_snap` and apply it once the resize has
    // settled (no `Resized` for `SNAP_DEBOUNCE`). `resize_burst_start` is the
    // size at the start of the current drag, used to pick the driving axis.
    #[cfg(not(target_os = "android"))]
    let pending_snap: Option<winit::dpi::PhysicalSize<u32>> = None;
    #[cfg(not(target_os = "android"))]
    let last_resize_at: Option<Instant> = None;
    #[cfg(not(target_os = "android"))]
    let resize_burst_start: Option<winit::dpi::PhysicalSize<u32>> = None;
    let mut gui = GuiApp {
        config,
        window: None,
        render_state,
        input,
        app,
        audio,
        resolve_state,
        pending_dialog_result,
        f_key_press_time: None,
        n_key_press_time: None,
        f_last_repeat_time: None,
        n_last_repeat_time: None,
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        rewind_worker,
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        png_worker,
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        next_print_index,
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        gilrs,
        #[cfg(not(target_arch = "wasm32"))]
        fetch_worker,
        #[cfg(target_os = "android")]
        android_pad,
        #[cfg(not(target_os = "android"))]
        is_fullscreen: false,
        #[cfg(not(target_os = "android"))]
        last_content_size,
        #[cfg(not(target_os = "android"))]
        last_fit_logical,
        #[cfg(not(target_os = "android"))]
        pending_snap,
        #[cfg(not(target_os = "android"))]
        last_resize_at,
        #[cfg(not(target_os = "android"))]
        resize_burst_start,
    };
    event_loop.run_app(&mut gui).map_err(PlatformError::from_display)
}

// Debounce/repeat timing for the F (frame-step) and N (cycle-step) debug keys,
// and the aspect-snap settle delay. Module-level so the handler methods share them.
const DEBOUNCE_DURATION: Duration = Duration::from_millis(250);
const REPEAT_INTERVAL: Duration = Duration::from_millis(67);
#[cfg(not(target_os = "android"))]
const SNAP_DEBOUNCE: Duration = Duration::from_millis(140);

/// The winit 0.30 `ApplicationHandler`. It owns every piece of state the old
/// `event_loop.run` closure captured. The window + GPU `RenderState` are created
/// lazily in `resumed` and dropped in `suspended`; the emulation `App` persists.
struct GuiApp<'c> {
    #[cfg_attr(target_os = "android", allow(unused))]
    config: &'c config::CleanConfig,
    window: Option<Arc<Window>>,
    render_state: Option<RenderState>,
    input: WinitInputHelper,
    app: App,
    audio: Option<crate::audio::Output>,
    resolve_state: rustyboi_session::ResolveState,
    pending_dialog_result: Arc<std::sync::Mutex<Option<GuiAction>>>,
    f_key_press_time: Option<Instant>,
    n_key_press_time: Option<Instant>,
    f_last_repeat_time: Option<Instant>,
    n_last_repeat_time: Option<Instant>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    rewind_worker: Option<crate::rewind_worker::RewindWorker>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    png_worker: Option<crate::png_worker::PngWorker>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    next_print_index: Option<(String, u32)>,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    gilrs: Option<gilrs::Gilrs>,
    #[cfg(not(target_arch = "wasm32"))]
    fetch_worker: Option<crate::fetch_worker::FetchWorker>,
    #[cfg(target_os = "android")]
    android_pad: std::collections::HashSet<PadButton>,
    #[cfg(not(target_os = "android"))]
    is_fullscreen: bool,
    #[cfg(not(target_os = "android"))]
    last_content_size: (u32, u32),
    #[cfg(not(target_os = "android"))]
    last_fit_logical: Option<(u32, u32)>,
    #[cfg(not(target_os = "android"))]
    pending_snap: Option<winit::dpi::PhysicalSize<u32>>,
    #[cfg(not(target_os = "android"))]
    last_resize_at: Option<Instant>,
    #[cfg(not(target_os = "android"))]
    resize_burst_start: Option<winit::dpi::PhysicalSize<u32>>,
}

impl ApplicationHandler for GuiApp<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // winit 0.30 creates windows here (an `ActiveEventLoop` is required).
        if self.window.is_none() {
            #[cfg(not(mobile))]
            let attrs = {
                let size = LogicalSize::new(
                    (WIDTH * (self.config.scale as u32)) as f64,
                    (HEIGHT * (self.config.scale as u32)) as f64,
                );
                Window::default_attributes()
                    .with_title("RustyBoi")
                    .with_inner_size(size)
                    .with_min_inner_size(LogicalSize::new(WIDTH as f64, HEIGHT as f64))
            };
            // Mobile (Android + iOS): the compositor sizes the surface fullscreen.
            #[cfg(mobile)]
            let attrs = Window::default_attributes().with_title("RustyBoi");
            match event_loop.create_window(attrs) {
                Ok(w) => self.window = Some(Arc::new(w)),
                Err(e) => {
                    println!("Failed to create window on Resumed: {e}");
                    event_loop.exit();
                    return;
                }
            }
        }
        if self.render_state.is_none() {
            let window = self.window.clone().expect("window created above");
            match create_render_state(window.clone(), Some(self.pending_dialog_result.clone())) {
                Ok(rs) => {
                    self.render_state = Some(rs);
                    window.request_redraw();
                    #[cfg(target_os = "android")]
                    if let Some(rs) = self.render_state.as_mut() {
                        let state = crate::library::LibraryState::load();
                        rs.ui.library_panel_mut().set_recents(state.recents.clone());
                        if state.tree_uri.is_some() {
                            if let Ok(mut slot) = self.pending_dialog_result.lock() {
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
                    event_loop.exit();
                }
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // On mobile, backgrounding is the last reliable chance to flush SRAM
        // before the OS may reclaim the app. Persists through the storage port
        // (no-op for non-battery carts). Desktop keeps its own sidecar `.sav`.
        #[cfg(mobile)]
        self.app.session_mut().persist_battery();
        self.render_state = None;
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        // winit_input_helper 0.17: clear per-step input state at the batch start.
        self.input.step();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Android gamepad: face/shoulder/start/select buttons arrive as unmapped
        // native key events (winit_input_helper only tracks KeyCode-mapped keys).
        #[cfg(target_os = "android")]
        if let WindowEvent::KeyboardInput { event: ref key, .. } = event {
            use winit::keyboard::{NativeKeyCode, PhysicalKey};
            if let PhysicalKey::Unidentified(NativeKeyCode::Android(code)) = key.physical_key
                && let Some(pb) = android_pad_button(code)
            {
                if key.state == winit::event::ElementState::Pressed {
                    self.android_pad.insert(pb);
                } else {
                    self.android_pad.remove(&pb);
                }
            }
        }

        // Feed the input helper every event (it returns true only on
        // RedrawRequested, which we handle explicitly below).
        let _ = self.input.process_window_event(&event);

        match event {
            WindowEvent::Resized(size) => self.handle_resize(size),
            WindowEvent::RedrawRequested => self.frame_tick(event_loop),
            other => {
                if let (Some(rs), Some(window)) =
                    (self.render_state.as_mut(), self.window.as_ref())
                {
                    rs.ui.handle_event(window, &other);
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // winit_input_helper 0.17: close the step, then drive a continuous redraw
        // (the compositor paces it) so the game + UI keep advancing every frame.
        self.input.end_step();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    // Tear everything down HERE — while the event loop (and, on Wayland, its live
    // compositor connection) still exists — rather than leaving it to the
    // implicit drop after `run_app` returns.
    //
    // The GPU/UI (`render_state`) owns egui-winit's clipboard, which on Wayland
    // spawns a `smithay-clipboard` worker thread. Dropping it late, during
    // process teardown, raced libwayland's global cleanup and segfaulted in that
    // worker (`wl_proxy_destroy` on primary-selection objects whose connection
    // was already gone). Dropping it now joins the worker cleanly against a live
    // connection. Order matters: the wgpu surface (in `render_state`) borrows the
    // window, so it must drop before the window; audio + the background workers
    // are stopped deterministically too.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.render_state = None;
        self.audio = None;
        // The background-worker fields are target-gated (see the struct), so the
        // drops must carry the matching cfgs or non-desktop builds fail to
        // compile with "no field" errors.
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            self.rewind_worker = None;
            self.png_worker = None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.fetch_worker = None;
        }
        self.window = None;
    }
}

impl GuiApp<'_> {
    /// One presented frame (runs on each `RedrawRequested`): resolve input,
    /// advance emulation + audio, then draw egui + the game. Merges the old
    /// input-update block and the RedrawRequested render arm.
    fn frame_tick(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else { return };

        if self.input.key_pressed(KeyCode::Escape) || self.input.close_requested() {
            event_loop.exit();
            return;
        }

        // F: frame stepping with debounce (paused/errored only).
        if self.input.key_pressed(KeyCode::KeyF) {
            if self.app.stepping_allowed() {
                self.app.request_step_frame();
                let now = Instant::now();
                self.f_key_press_time = Some(now);
                self.f_last_repeat_time = Some(now);
            }
        } else if self.input.key_held(KeyCode::KeyF) {
            if self.app.stepping_allowed()
                && let Some(press_time) = self.f_key_press_time
                && press_time.elapsed() >= DEBOUNCE_DURATION
                && let Some(last_repeat) = self.f_last_repeat_time
                && last_repeat.elapsed() >= REPEAT_INTERVAL
            {
                self.app.request_step_frame();
                self.f_last_repeat_time = Some(Instant::now());
            }
        } else {
            self.f_key_press_time = None;
            self.f_last_repeat_time = None;
        }

        // N: cycle stepping with debounce (paused/errored only).
        if self.input.key_pressed(KeyCode::KeyN) {
            if self.app.stepping_allowed() {
                self.app.request_step_cycle();
                let now = Instant::now();
                self.n_key_press_time = Some(now);
                self.n_last_repeat_time = Some(now);
            }
        } else if self.input.key_held(KeyCode::KeyN) {
            if self.app.stepping_allowed()
                && let Some(press_time) = self.n_key_press_time
                && press_time.elapsed() >= DEBOUNCE_DURATION
                && let Some(last_repeat) = self.n_last_repeat_time
                && last_repeat.elapsed() >= REPEAT_INTERVAL
            {
                self.app.request_step_cycle();
                self.n_last_repeat_time = Some(Instant::now());
            }
        } else {
            self.n_key_press_time = None;
            self.n_last_repeat_time = None;
        }

        if let Some(scale_factor) = self.input.scale_factor()
            && let Some(rs) = self.render_state.as_mut()
        {
            rs.ui.set_pixels_per_point(scale_factor as f32);
        }

        // Build the raw held-input set (keyboard + gamepad) and resolve it through
        // the shared config: GB-button bindings drive the button state, chord
        // hotkeys drive features. Then OR the egui touch overlay on top and
        // dispatch any fired hotkeys.
        #[cfg_attr(any(target_arch = "wasm32", target_os = "android"), allow(unused_mut))]
        let mut held = held_inputs_from_keyboard(&self.input);
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        if let Some(g) = self.gilrs.as_mut() {
            collect_gamepad_held(g, &mut held.pad);
        }
        #[cfg(target_os = "android")]
        {
            held.pad.extend(self.android_pad.iter().copied());
            // Analog sticks + hat arrive via Java (onGenericMotionEvent → JNI).
            // Android axes: +X right, +Y down. Hat covers controllers that report
            // the d-pad as an axis rather than key events.
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
            self.app.session().config().input.resolve(&held, &mut self.resolve_state);
        if let Some(rs) = self.render_state.as_ref() {
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
        self.app.set_button_state(button_state);
        // Forward the held pad set so the keybind editor can capture gamepad
        // presses (egui never sees pad input).
        self.app.set_held_pad(held.pad.clone());

        // Reconcile fast-forward with the held-hotkey state. This engages the
        // hold gesture (Tab) and releases it when the chord drops, while leaving
        // a menu/touch toggle latched (the Android path, which has no keyboard).
        let ff_active = fired
            .iter()
            .any(|f| matches!(f.action, HotkeyAction::FastForward));
        self.app.tick_fast_forward_hold(ff_active);
        for f in fired {
            #[cfg(not(target_os = "android"))]
            let exit = dispatch_hotkey(&mut self.app, f, &window, event_loop, &mut self.is_fullscreen);
            #[cfg(target_os = "android")]
            let exit = {
                let mut dummy = false;
                dispatch_hotkey(&mut self.app, f, &window, event_loop, &mut dummy)
            };
            if exit {
                return;
            }
        }

        // Android: keep the game region inside the safe area (system bars /
        // display cutout) so it isn't clipped behind them. No-op elsewhere.
        #[cfg(target_os = "android")]
        if let Some(rs) = self.render_state.as_ref() {
            let (w, h) = rs.renderer.surface_size();
            let (l, t, r, b) = crate::android::safe_area_insets(w, h);
            self.app.set_safe_insets(l, t, r, b);
        }

        // Advance one presented frame (paced inside the app), play audio, pump
        // the workers. Report the audio backlog first so the app can pace off it
        // (audio-clocked pacing): run ahead to refill the cushion after a hitch,
        // pace to real time once it's full.
        self.app.set_audio_backlog(self.audio.as_ref().map(|a| a.queued_frames()));
        let step = self.app.run_frame();
        if step.pump_workers {
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            pump_workers(
                &mut self.app,
                self.rewind_worker.as_mut(),
                &mut self.png_worker,
                &mut self.next_print_index,
            );
            #[cfg(any(target_arch = "wasm32", target_os = "android"))]
            drain_printer_sheets_unsupported(&mut self.app);
        }
        if let Some(a) = self.audio.as_mut() {
            a.push_samples(&step.audio);
        }

        // Android: the render/present is vsync-gated (Fifo) and dips below 60fps
        // on a 60Hz surface whenever a frame grazes the vsync budget. With one
        // emulation frame per present, that would slow the game AND starve audio
        // in lockstep. Decouple them: while the audio backlog is short, advance
        // extra emulation frames *without* rendering, feeding their audio. The
        // game then runs at true 60fps with a fed audio cushion; the display just
        // shows a couple fewer frames. Audio-clocked pacing keeps these extra
        // `run_frame`s from sleeping. Capped so a long present stall can't spiral.
        #[cfg(target_os = "android")]
        {
            const AUDIO_CATCHUP_TARGET: usize = 2;
            const AUDIO_CATCHUP_MAX_FRAMES: u32 = 4;
            let mut extra = 0;
            while extra < AUDIO_CATCHUP_MAX_FRAMES
                && self.audio.as_ref().is_some_and(|a| a.queued_frames() < AUDIO_CATCHUP_TARGET)
            {
                self.app.set_audio_backlog(self.audio.as_ref().map(|a| a.queued_frames()));
                let catchup = self.app.run_frame();
                if catchup.audio.is_empty() {
                    break; // paused / not advancing — nothing to feed.
                }
                if let Some(a) = self.audio.as_mut() {
                    a.push_samples(&catchup.audio);
                }
                extra += 1;
            }
        }

        self.draw_frame(&window, event_loop);
    }

    /// Composite egui + the game onto the surface and service the `App`'s
    /// platform requests. (The old RedrawRequested arm.)
    fn draw_frame(&mut self, window: &Arc<Window>, event_loop: &ActiveEventLoop) {
        let Some(rs) = self.render_state.as_mut() else { return };

        // Deliver any completed cheat-DB fetches into the session so the cheat
        // picker shows them; report the outcome in the status bar.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(worker) = self.fetch_worker.as_mut() {
            for done in worker.drain_finished() {
                use rustyboi_session::FetchPurpose;
                match (done.purpose, done.result) {
                    (FetchPurpose::Cheats, Ok(body)) => {
                        let n = self.app.session_mut().finish_fetched_cheats(&body);
                        if n == 0 {
                            rs.ui.set_status("No cheats found for this game".into());
                        } else {
                            rs.ui.set_status(format!("Fetched {n} cheats"));
                        }
                    }
                    (FetchPurpose::NoIntro, Ok(body)) => {
                        // Cache the downloaded DAT so we don't re-fetch it next
                        // launch, then feed it into the index and re-resolve the
                        // current ROM's display name.
                        if let Some(url) = done.url.as_deref() {
                            crate::no_intro_cache::store(&save_base(), url, &body);
                        }
                        self.app
                            .session_mut()
                            .finish_no_intro_dats(std::slice::from_ref(&body));
                        if let Some(title) = self.app.title_if_due() {
                            window.set_title(&title);
                        }
                    }
                    (FetchPurpose::Cheats, Err(e)) => {
                        // A failed cheat fetch is not fatal — surface it in the
                        // status bar, never the crash screen.
                        rs.ui.set_status(format!("Cheat fetch failed: {e}"));
                    }
                    (FetchPurpose::NoIntro, Err(e)) => {
                        // No-Intro identification is best-effort; a failed DAT
                        // download just leaves games on their header titles.
                        log::warn!("No-Intro DAT fetch failed: {e}");
                    }
                }
            }
        }

        if let Some(title) = self.app.title_if_due() {
            window.set_title(&title);
        }

        // Keep the render surface locked to the live window size *before* laying
        // out egui (egui lays out using `window.inner_size()`; the `Resized` event
        // is async, so the surface can lag after a programmatic resize). Syncing
        // here (a cheap no-op when unchanged) keeps layout and target size in step.
        {
            let phys = window.inner_size();
            let (pw, ph) = (phys.width.max(1), phys.height.max(1));
            if (pw, ph) != rs.renderer.surface_size() {
                rs.renderer.resize(pw, ph);
            }
        }

        // Android IME: synthesize egui events winit drops.
        let extra_events = collect_extra_egui_events();

        // The menu-bar auto-hide flag is a desktop concern (Android is always
        // fullscreen and uses the mobile menu; `is_fullscreen` only exists there).
        #[cfg(not(target_os = "android"))]
        let fullscreen = self.is_fullscreen;
        #[cfg(target_os = "android")]
        let fullscreen = false;
        let requests = self.app.draw(window, &mut rs.ui, &mut rs.renderer, extra_events, fullscreen, resolve_gui_action);

        for req in requests {
            match req {
                PlatformRequest::Exit => {
                    event_loop.exit();
                    return;
                }
                PlatformRequest::ToggleFullscreen => {
                    #[cfg(not(target_os = "android"))]
                    {
                        self.is_fullscreen = !self.is_fullscreen;
                        window.set_fullscreen(
                            self.is_fullscreen.then(|| Fullscreen::Borderless(None)),
                        );
                    }
                }
                PlatformRequest::ResizeContent { width, height } => {
                    // Just record the new content size; the continuous fit below
                    // sizes the window as content*scale + the measured chrome
                    // inset so the game fills the central rect with no letterbox.
                    #[cfg(not(target_os = "android"))]
                    {
                        self.last_content_size = (width, height);
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
                // unreachable on desktop/Android; kept for the shared contract
                // (the web worker services it). Log if it fires.
                PlatformRequest::LoadFile { .. } => {
                    log::warn!("LoadFile request reached the platform loop unexpectedly");
                }
                PlatformRequest::FetchUrl { urls, purpose } => {
                    self.fetch_worker
                        .get_or_insert_with(crate::fetch_worker::FetchWorker::new)
                        .submit(urls, purpose);
                }
                #[cfg(target_os = "android")]
                PlatformRequest::AndroidLibrary(action) => {
                    handle_android_library(action, &mut rs.ui, &self.pending_dialog_result);
                }
            }
        }

        // Breakpoint-hit notification (surface the PC in the status bar).
        if self.app.take_breakpoint_hit() {
            let pc = self.app.gb().get_cpu_registers().pc;
            rs.ui.set_status(format!("Breakpoint hit at PC: ${pc:04X}"));
        }

        // Programmatic fit: size the window so the egui central rect is exactly
        // content*scale (game fills it, no bars). Target = content*scale + the
        // measured chrome inset. Fires ONLY on the first frame (inset now known)
        // and when the content size changes (SGB border appearing/disappearing) —
        // never continuously, so it does not fight a user resize.
        #[cfg(not(target_os = "android"))]
        {
            let content = self.app.content_size();
            let content_changed = content != self.last_content_size;
            self.last_content_size = content;
            if content_changed || self.last_fit_logical.is_none() {
                let scale = self.config.scale.max(1) as u32;
                let (inset_w, inset_h) = self.app.content_inset();
                let target = (
                    (content.0 * scale + inset_w.round() as u32).max(1),
                    (content.1 * scale + inset_h.round() as u32).max(1),
                );
                self.last_fit_logical = Some(target);
                let _ = window.request_inner_size(LogicalSize::new(
                    target.0 as f64,
                    target.1 as f64,
                ));
            }

            // Apply a debounced aspect-snap once the user's resize has settled
            // (no Resized for SNAP_DEBOUNCE). This is the only aspect correction
            // that touches a user-driven size, and it fires after the drag ends.
            if let Some(snap) = self.pending_snap {
                let settled = self
                    .last_resize_at
                    .map(|t| t.elapsed() >= SNAP_DEBOUNCE)
                    .unwrap_or(false);
                if settled {
                    self.pending_snap = None;
                    self.resize_burst_start = None;
                    let _ = window.request_inner_size(snap);
                }
            }
        }
    }

    /// Resize the surface and record a debounced aspect-snap (desktop). (The old
    /// `WindowEvent::Resized` arm.)
    fn handle_resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if let Some(rs) = self.render_state.as_mut() {
            rs.renderer.resize(size.width.max(1), size.height.max(1));
        }
        // Aspect-lock (debounced): compute the aspect-correct size for this resize
        // but DON'T apply it now — requesting a size mid-drag fights the
        // compositor. Record it as `pending_snap`; `draw_frame` applies it once the
        // resize settles. The window follows the cursor freely during the drag
        // (the game renders aspect-fit with a transient bar), then snaps on release.
        #[cfg(not(target_os = "android"))]
        {
            let now = Instant::now();
            // A gap since the last resize means a new drag burst began; baseline
            // the driving-axis detection to this size.
            let new_burst = self
                .last_resize_at
                .map(|t| now.duration_since(t) > SNAP_DEBOUNCE)
                .unwrap_or(true);
            if new_burst {
                self.resize_burst_start = Some(size);
            }
            self.last_resize_at = Some(now);

            let (cw, ch) = self.app.content_size();
            let aspect = cw as f32 / ch as f32;
            let sf = self.window.as_ref().map_or(1.0, |w| w.scale_factor()) as f32;
            let (iw, ih) = self.app.content_inset();
            let (iw_p, ih_p) = (iw * sf, ih * sf);
            let (new_w, new_h) = (size.width as f32, size.height as f32);
            let base = self.resize_burst_start.unwrap_or(size);
            let dw = (new_w - base.width as f32).abs();
            let dh = (new_h - base.height as f32).abs();
            let (corr_w, corr_h) = if dh > dw {
                let avail_h = (new_h - ih_p).max(1.0);
                ((avail_h * aspect + iw_p).round(), new_h.round())
            } else {
                let avail_w = (new_w - iw_p).max(1.0);
                (new_w.round(), (avail_w / aspect + ih_p).round())
            };
            let corr = winit::dpi::PhysicalSize::new((corr_w as u32).max(1), (corr_h as u32).max(1));
            self.pending_snap = if (corr.width as f32 - new_w).abs() > 1.0
                || (corr.height as f32 - new_h).abs() > 1.0
            {
                Some(corr)
            } else {
                None
            };
        }
    }
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
        GuiAction::LoadMovie(file_data) => {
            let (bytes, _path) = read_file_data(file_data)?;
            Some(ResolvedAction::LoadMovie { bytes })
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
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
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
    // iOS: write into the app's Documents dir. With UIFileSharingEnabled +
    // LSSupportsOpeningDocumentsInPlace (Info.plist) the file is retrievable
    // through the Files app / Finder, so exports aren't lost.
    #[cfg(target_os = "ios")]
    {
        let path = crate::ios::documents_dir().join(suggested_name);
        std::fs::write(&path, bytes)?;
        Ok(Some(path))
    }
    // wasm: no filesystem export target.
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
        #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
        FileData::Path(path) => {
            let name = path.to_string_lossy().to_string();
            std::fs::read(path).ok().map(|b| (b, Some(name)))
        }
        #[cfg(any(target_arch = "wasm32", target_os = "android", target_os = "ios"))]
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

    // Printer: drain finished photos (strips already stitched into one long
    // sheet by the session), encode + write off-thread.
    let sheets = app.session_mut().take_prints();
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
    let sheets = app.session_mut().take_prints();
    if !sheets.is_empty() {
        log::warn!("{} print(s) captured but this platform has no print sink", sheets.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Android gamepad keycode → PadButton map (Android delivers controller
    // buttons as unmapped native key events; winit has no Android gamepad
    // backend). Pins the face/shoulder/trigger/start/select + d-pad codes and
    // that unknown codes map to None.
    #[test]
    fn android_pad_button_maps_known_codes() {
        let cases = [
            (96, PadButton::South),
            (97, PadButton::East),
            (99, PadButton::West),
            (100, PadButton::North),
            (102, PadButton::LeftShoulder),
            (103, PadButton::RightShoulder),
            (104, PadButton::LeftTrigger),
            (105, PadButton::RightTrigger),
            (108, PadButton::Start),
            (109, PadButton::Select),
            (19, PadButton::DpadUp),
            (20, PadButton::DpadDown),
            (21, PadButton::DpadLeft),
            (22, PadButton::DpadRight),
        ];
        for (code, want) in cases {
            assert_eq!(android_pad_button(code), Some(want), "keycode {code}");
        }
        // Unmapped codes (e.g. BUTTON_MODE 110, letter keys) yield None.
        for code in [0u32, 1, 98, 101, 106, 107, 110, 200] {
            assert_eq!(android_pad_button(code), None, "keycode {code} should be unmapped");
        }
    }

    #[test]
    fn safe_insets_from_rect_computes_the_gap() {
        // 1000x600 surface, content rect inset 10 left / 20 top and ending at
        // 980 x 560 → right gap 20, bottom gap 40.
        assert_eq!(safe_insets_from_rect(1000, 600, 10, 20, 980, 560), (10.0, 20.0, 20.0, 40.0));
        // Full-surface content rect → no insets.
        assert_eq!(safe_insets_from_rect(1000, 600, 0, 0, 1000, 600), (0.0, 0.0, 0.0, 0.0));
        // Degenerate/empty rect (before first insets) → no insets, never collapse.
        assert_eq!(safe_insets_from_rect(1000, 600, 0, 0, 0, 0), (0.0, 0.0, 0.0, 0.0));
        assert_eq!(safe_insets_from_rect(1000, 600, 5, 5, 3, 3), (0.0, 0.0, 0.0, 0.0));
        // A content rect extending past the surface never yields negative insets.
        assert_eq!(safe_insets_from_rect(100, 100, -10, -10, 200, 200), (0.0, 0.0, 0.0, 0.0));
    }
}

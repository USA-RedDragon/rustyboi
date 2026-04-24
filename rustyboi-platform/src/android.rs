//! Android entry point.
//!
//! `cargo-apk` produces a `cdylib` and the platform's `NativeActivity`
//! looks up `android_main` by symbol. We pick that up via winit's
//! `android-native-activity` feature, which dispatches the
//! `AndroidApp` into here.
//!
//! The function:
//! 1. Initializes `android_logger` so `log` calls land in `adb logcat`.
//! 2. Stashes the `AndroidApp` on a global so JNI helpers can read
//!    `internal_data_path()` / the activity context.
//! 3. Installs the `rustyboi-egui` android bridge with file picker /
//!    file saver implementations that drive Storage Access Framework
//!    intents through JNI.
//! 4. Hands control to the shared `run` module.

#![cfg(target_os = "android")]
#![allow(unsafe_code)]

use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::thread;

use jni::objects::{JByteArray, JClass, JObject, JObjectArray, JString};
use jni::sys::jobject;
use jni::{JNIEnv, JavaVM};
use rustyboi_frontend_lib::actions::{FileData, LibraryEntry};
use rustyboi_frontend_lib::android_bridge::{
    self, LoadRomCallback, PickFileCallback, ScanCallback, TreePickCallback,
};
use winit::platform::android::activity::AndroidApp;

static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

/// Pending ROM-pick callback. Java's `onActivityResult` hops back into
/// `Java_dev_mcswain_rustyboi_RustyboiActivity_nativeOnRomPicked` (or the
/// cancellation variant) and the JNI extern pops this and invokes it.
///
/// SAF only allows a single in-flight `startActivityForResult` per request
/// code; we serialize requests by overwriting any prior pending callback
/// with `None` (cancelled).
static PENDING_PICK: Mutex<Option<PickFileCallback>> = Mutex::new(None);

/// SAF tree-picker callback (set when the user invokes the ROM library
/// "Pick folder…" button).
static PENDING_TREE: Mutex<Option<TreePickCallback>> = Mutex::new(None);

/// Library scan callback (set when the platform asks the Kotlin side to
/// enumerate the picked tree).
static PENDING_SCAN: Mutex<Option<ScanCallback>> = Mutex::new(None);

/// Load-rom-from-uri callback (set when the user taps a library entry).
static PENDING_LOAD: Mutex<Option<LoadRomCallback>> = Mutex::new(None);

/// File descriptor for the SAF-backed sibling `.sav` corresponding to
/// the ROM that's currently being loaded. The Kotlin `loadRomEntry`
/// path opens/creates `<rom-stem>.sav` next to the ROM, detaches the
/// `ParcelFileDescriptor`, and hands the integer fd through JNI. Rust
/// takes ownership via [`take_pending_sav_fd`] and wraps it in a
/// `std::fs::File` for the cartridge save layer.
static PENDING_SAV_FD: Mutex<Option<RawFd>> = Mutex::new(None);

/// Pop the pending SAV file descriptor, if any. Caller owns it.
pub fn take_pending_sav_fd() -> Option<RawFd> {
    PENDING_SAV_FD
        .lock()
        .expect("PENDING_SAV_FD poisoned")
        .take()
}

/// Set once the soft IME has been shown at least once. Used by the
/// install_ime handler to seed an empty `TextInputState` exactly
/// once; android-activity 0.5.2's `text_input_state()` reads through
/// a null `GameTextInput` buffer pointer until the state has been
/// written at least once, so we initialize it on the first
/// show_soft_input call.
static IME_INITIALIZED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the GameTextInput buffer has been initialized and is safe
/// to read via `AndroidApp::text_input_state()`.
pub fn ime_initialized() -> bool {
    IME_INITIALIZED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Last observed GameTextInput buffer, diffed each frame to synthesize egui
/// events. Lives here (not in the frontend `UiHost`) because reading the buffer
/// needs the `AndroidApp` handle, which is platform-only. Formerly held by the
/// platform `Framework`.
static LAST_IME_TEXT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Diff the GameTextInput buffer against the last poll and emit egui Text /
/// Backspace events. winit 0.29's android-game-activity backend drops
/// GameTextInput `TextEvent`s ("Unknown android_activity input event
/// TextEvent"), so the frontend `UiHost` gets these injected each frame via
/// `App::draw`'s `extra_events`. We deliberately do NOT clear the buffer here so
/// IME-side backspace shows up as a shrink on the next poll.
pub fn drain_ime_egui_events() -> Vec<rustyboi_frontend_lib::egui_events::Event> {
    use rustyboi_frontend_lib::egui_events::{Event, Key, Modifiers};

    let mut events = Vec::new();
    if !ime_initialized() {
        return events;
    }
    let app = android_app();
    let state = app.text_input_state();
    let mut last = LAST_IME_TEXT.lock().unwrap();
    if state.text == *last {
        return events;
    }
    let common: usize = last
        .chars()
        .zip(state.text.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let prev_chars = last.chars().count();
    let new_chars = state.text.chars().count();
    let backspaces = prev_chars.saturating_sub(common);
    for _ in 0..backspaces {
        events.push(Event::Key {
            key: Key::Backspace,
            physical_key: Some(Key::Backspace),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        events.push(Event::Key {
            key: Key::Backspace,
            physical_key: Some(Key::Backspace),
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
    }
    if new_chars > common {
        let new_text: String = state.text.chars().skip(common).collect();
        if !new_text.is_empty() {
            events.push(Event::Text(new_text));
        }
    }
    *last = state.text;
    events
}

/// Returns the `AndroidApp` handle stashed by `android_main`.
pub fn android_app() -> &'static AndroidApp {
    ANDROID_APP
        .get()
        .expect("AndroidApp has not been installed; android_main must run first")
}

/// Safe-area insets `(left, top, right, bottom)` in surface pixels: the gap
/// between the full surface (`surface_w` x `surface_h`) and the content rect
/// (system bars + display cutout). The frontend shrinks the game region by these
/// so it is not drawn behind them (e.g. the Z-Fold exterior display in landscape,
/// where the game's bottom was clipped behind the navigation bar).
pub fn safe_area_insets(surface_w: u32, surface_h: u32) -> (f32, f32, f32, f32) {
    let rect = android_app().content_rect();
    // Before the first insets arrive `content_rect` can be empty/zero; treat a
    // degenerate rect as "no insets" so we never collapse the game to nothing.
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let left = rect.left.max(0) as f32;
    let top = rect.top.max(0) as f32;
    let right = (surface_w as i32 - rect.right).max(0) as f32;
    let bottom = (surface_h as i32 - rect.bottom).max(0) as f32;
    (left, top, right, bottom)
}

/// Returns the directory the app should use for app-internal config
/// metadata (the persisted ROM-library state, log redirects, etc).
/// Battery save data does NOT live here — it follows the user's ROMs
/// via SAF document fds.
pub fn data_dir() -> PathBuf {
    let path: PathBuf = android_app()
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
    let _ = std::fs::create_dir_all(&path);
    path
}

/// Directory used to land save-state JSON dumps from the `Save State`
/// menu. SAF doesn't have a Save-As flow that fits this UX, so we keep
/// these as app-internal for now. Battery `.sav` files do NOT live
/// here.
pub fn save_dir() -> PathBuf {
    let mut path: PathBuf = android_app()
        .internal_data_path()
        .unwrap_or_else(|| PathBuf::from("/data/local/tmp"));
    path.push("saves");
    let _ = std::fs::create_dir_all(&path);
    path
}

/// NativeActivity entry point. Symbol exported as `android_main`.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    raw_log("android_main: entered");
    redirect_stdio_to_logcat();
    android_logger::init_once(
        android_logger::Config::default()
            // Info is plenty; Debug pulls in extremely chatty per-frame
            // logs from wgpu_core/wgpu_hal and the JNI crate.
            .with_max_level(log::LevelFilter::Info)
            // winit's Android backend spams WARN "TODO: ..." stubs for
            // unimplemented lifecycle hooks; silence them (they are harmless).
            .with_filter(
                android_logger::FilterBuilder::new()
                    .parse("info,winit=error")
                    .build(),
            )
            .with_tag("rustyboi"),
    );
    raw_log("android_main: android_logger initialized");
    // Force backtraces on so panic hook output is useful in logcat.
    // SAFETY: single-threaded at this point — android_main has just begun.
    unsafe {
        std::env::set_var("RUST_BACKTRACE", "1");
    }
    // Route Rust panics into logcat so we don't lose the message under
    // a bare tombstone. Capture a backtrace alongside the panic info.
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        raw_log(&format!("rustyboi panic: {info}\n{bt}"));
        log::error!("rustyboi panic: {info}\n{bt}");
    }));
    raw_log("android_main: panic hook installed");
    log::info!("rustyboi android_main starting");

    let _ = ANDROID_APP.set(app.clone());
    raw_log("android_main: ANDROID_APP stashed");
    // Install file-picker / file-saver bridges that route through SAF.
    android_bridge::install(
        Box::new(|callback| {
            // Stash the callback; the matching `nativeOnRomPicked` JNI extern
            // will pop it once the user selects a file (or cancels).
            {
                let mut slot = PENDING_PICK.lock().expect("PENDING_PICK poisoned");
                if let Some(prev) = slot.replace(callback) {
                    // Drop a previous in-flight request by cancelling it.
                    prev(None);
                }
            }

            // Invoke `RustyboiActivity.pickRomFromSaf()` on the JVM. The Java
            // side itself dispatches to the UI thread, so we just need an
            // attached JNIEnv and the activity context.
            let dispatched = with_activity(|env, activity| {
                match env.call_method(activity, "pickRomFromSaf", "()V", &[]) {
                    Ok(_) => true,
                    Err(e) => {
                        log::error!("call pickRomFromSaf failed: {e}");
                        false
                    }
                }
            })
            .unwrap_or(false);

            if !dispatched {
                // Couldn't reach Java — surface a cancellation immediately.
                let cb = {
                    let mut slot = PENDING_PICK.lock().expect("PENDING_PICK poisoned");
                    slot.take()
                };
                if let Some(cb) = cb {
                    cb(None);
                }
            }
        }),
        Box::new(|file_name, callback| {
            // Save states still go to the app's internal files dir; SAF
            // doesn't fit the Save-As UX. Battery `.sav` files use the
            // library load path and SAF fds instead.
            let mut path = save_dir();
            let name = file_name.unwrap_or_else(|| "save.rustyboisave".to_string());
            path.push(name);
            thread::spawn(move || callback(Some(path)));
        }),
    );

    // Install the ROM library bridge: tree pick, recursive scan, and
    // SAF-backed ROM load (which also opens the sibling `.sav` fd).
    android_bridge::install_library(
        Box::new(|callback| {
            {
                let mut slot = PENDING_TREE.lock().expect("PENDING_TREE poisoned");
                if let Some(prev) = slot.replace(callback) {
                    prev(None);
                }
            }
            let dispatched = with_activity(|env, activity| {
                match env.call_method(activity, "pickLibraryTree", "()V", &[]) {
                    Ok(_) => true,
                    Err(e) => {
                        log::error!("call pickLibraryTree failed: {e}");
                        false
                    }
                }
            })
            .unwrap_or(false);
            if !dispatched {
                let cb = PENDING_TREE.lock().expect("PENDING_TREE poisoned").take();
                if let Some(cb) = cb {
                    cb(None);
                }
            }
        }),
        Box::new(|tree_uri, callback| {
            {
                let mut slot = PENDING_SCAN.lock().expect("PENDING_SCAN poisoned");
                if let Some(prev) = slot.replace(callback) {
                    prev(None);
                }
            }
            let dispatched = with_activity(|env, activity| {
                let juri = match env.new_string(&tree_uri) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("new_string(tree_uri) failed: {e}");
                        return false;
                    }
                };
                match env.call_method(
                    activity,
                    "scanLibrary",
                    "(Ljava/lang/String;)V",
                    &[(&juri).into()],
                ) {
                    Ok(_) => true,
                    Err(e) => {
                        log::error!("call scanLibrary failed: {e}");
                        false
                    }
                }
            })
            .unwrap_or(false);
            if !dispatched {
                let cb = PENDING_SCAN.lock().expect("PENDING_SCAN poisoned").take();
                if let Some(cb) = cb {
                    cb(None);
                }
            }
        }),
        Box::new(|rom_uri, callback| {
            {
                let mut slot = PENDING_LOAD.lock().expect("PENDING_LOAD poisoned");
                if let Some(prev) = slot.replace(callback) {
                    prev(None);
                }
            }
            let dispatched = with_activity(|env, activity| {
                let juri = match env.new_string(&rom_uri) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("new_string(rom_uri) failed: {e}");
                        return false;
                    }
                };
                match env.call_method(
                    activity,
                    "loadRomEntry",
                    "(Ljava/lang/String;)V",
                    &[(&juri).into()],
                ) {
                    Ok(_) => true,
                    Err(e) => {
                        log::error!("call loadRomEntry failed: {e}");
                        false
                    }
                }
            })
            .unwrap_or(false);
            if !dispatched {
                let cb = PENDING_LOAD.lock().expect("PENDING_LOAD poisoned").take();
                if let Some(cb) = cb {
                    cb(None);
                }
            }
        }),
    );

    // Install the soft-keyboard bridge so egui text fields can raise
    // and dismiss the on-screen IME on Android. We drive the IME via
    // android-activity's GameTextInput integration directly
    // (`AndroidApp::show_soft_input` / `hide_soft_input`); committed
    // text is then read each frame in `Framework::prepare` via
    // `text_input_state()` and diffed into egui events.
    //
    // The first time we show the IME we also seed an empty
    // `TextInputState` via `set_text_input_state`. android-activity
    // 0.5.2's `text_input_state()` calls `slice::from_raw_parts` on
    // the underlying `GameTextInput` buffer's UTF-8 pointer, which is
    // null until the state is first written; seeding here gives that
    // pointer a valid backing array so subsequent polls don't trip
    // Rust's null-pointer UB check.
    android_bridge::install_ime(Box::new(|visible| {
        use winit::platform::android::activity::input::{TextInputState, TextSpan};
        let app = android_app();
        if visible {
            if !IME_INITIALIZED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                app.set_text_input_state(TextInputState {
                    text: String::new(),
                    selection: TextSpan { start: 0, end: 0 },
                    compose_region: None,
                });
            }
            app.show_soft_input(true);
        } else {
            app.hide_soft_input(false);
        }
    }));

    // Install the toast bridge so transient status messages surface as
    // system Toasts instead of the desktop-only on-screen status bar.
    android_bridge::install_toast(Box::new(|message| {
        let _ = with_activity(|env, activity| {
            let jmsg = match env.new_string(&message) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("new_string(toast message) failed: {e}");
                    return;
                }
            };
            match env.call_method(
                activity,
                "showToast",
                "(Ljava/lang/String;)V",
                &[(&jmsg).into()],
            ) {
                Ok(_) => {}
                Err(e) => log::error!("call showToast failed: {e}"),
            }
        });
    }));

    raw_log("android_main: bridge installed, calling run_android");
    if let Err(e) = crate::run::run_android(app) {
        // `Error::UserDefined` Display drops the inner cause, so dump the
        // chain via Debug + source() so we don't lose information when
        // bubbling up a winit/pixels/etc. failure.
        raw_log(&format!("run_android failed: {e:?} ({e})"));
        log::error!("rustyboi exited with error: {e:?} ({e})");
        let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
        while let Some(s) = src {
            raw_log(&format!("  caused by: {s}"));
            log::error!("  caused by: {s}");
            src = s.source();
        }
    }
    raw_log("android_main: run_android returned");
    // winit 0.29's `EventLoop` can only be built once per process on
    // Android. If we let `android_main` return normally, GameActivity
    // keeps the process alive and the next activity launch (e.g.
    // resuming from Recents) re-enters `android_main`, where
    // `EventLoopBuilder::build()` then fails. Force-exit the process so
    // every launch starts from a clean slate.
    std::process::exit(0);
}

/// Direct `__android_log_print` so we can trace crashes that happen before
/// `android_logger::init_once` completes (or if the panic hook itself fails).
pub fn raw_log(msg: &str) {
    use std::ffi::CString;
    use std::os::raw::c_char;
    const ANDROID_LOG_INFO: i32 = 4;
    let tag = CString::new("rustyboi").unwrap();
    let body = CString::new(msg).unwrap_or_else(|_| CString::new("<bad msg>").unwrap());
    unsafe extern "C" {
        fn __android_log_write(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
    }
    unsafe {
        __android_log_write(ANDROID_LOG_INFO, tag.as_ptr(), body.as_ptr());
    }
}

/// Redirect the process's `stdout` and `stderr` file descriptors into
/// `logcat`. Without this, anything written via `println!` / `eprintln!`
/// (and stdout of dependencies like `wgpu` panic backtraces) is silently
/// discarded on Android because the runtime attaches no terminal. Each
/// fd is replaced with the write end of a pipe; a background thread reads
/// from the read end line-by-line and forwards each line to
/// `__android_log_write`.
///
/// Safe to call once; subsequent calls are no-ops because `redirected` is
/// guarded by an `OnceLock`.
fn redirect_stdio_to_logcat() {
    use std::ffi::CString;
    use std::io::{BufRead, BufReader};
    use std::os::fd::FromRawFd;
    use std::os::raw::c_char;
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.set(()).is_err() {
        return;
    }
    unsafe extern "C" {
        fn __android_log_write(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
    }
    fn pipe_to_logcat(target_fd: i32, prio: i32, tag: &'static str) {
        let mut fds: [libc::c_int; 2] = [0; 2];
        // SAFETY: passing a valid local array; pipe() writes two fds.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            raw_log("redirect_stdio_to_logcat: pipe() failed");
            return;
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        // SAFETY: dup2 swaps stdout/stderr to the pipe's write end.
        if unsafe { libc::dup2(write_fd, target_fd) } < 0 {
            raw_log("redirect_stdio_to_logcat: dup2() failed");
            unsafe {
                libc::close(read_fd);
                libc::close(write_fd);
            }
            return;
        }
        // Close the now-duplicate write fd; the real stdout/stderr still
        // refers to the pipe via target_fd.
        unsafe {
            libc::close(write_fd);
        }
        thread::Builder::new()
            .name(format!("logcat-{tag}"))
            .spawn(move || {
                // SAFETY: read_fd is exclusively owned by this thread.
                let file = unsafe { std::fs::File::from_raw_fd(read_fd) };
                let reader = BufReader::new(file);
                let tag_c = CString::new(tag).unwrap();
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    let Ok(body) = CString::new(line) else { continue };
                    unsafe {
                        __android_log_write(prio, tag_c.as_ptr(), body.as_ptr());
                    }
                }
            })
            .ok();
    }
    // ANDROID_LOG_INFO = 4, ANDROID_LOG_WARN = 5.
    pipe_to_logcat(libc::STDOUT_FILENO, 4, "rustyboi/stdout");
    pipe_to_logcat(libc::STDERR_FILENO, 5, "rustyboi/stderr");
}

/// Helper: with a JNIEnv attached to the current thread and the activity
/// `JObject` (the NativeActivity context), run `f`.
pub fn with_activity<F, R>(f: F) -> Option<R>
where
    F: for<'a> FnOnce(&mut JNIEnv<'a>, &JObject<'a>) -> R,
{
    let ctx = ndk_context::android_context();
    // Safety: `android_context()` exposes the JavaVM/context for the
    // lifetime of the process.
    let vm = unsafe { JavaVM::from_raw(ctx.vm() as *mut _) }.ok()?;
    let activity = unsafe { JObject::from_raw(ctx.context() as jobject) };
    let mut env = vm.attach_current_thread().ok()?;
    Some(f(&mut env, &activity))
}

/// The device's trusted CA certificates (DER), from the default X509
/// `TrustManager`. These become a rustls root store so TLS validates against the
/// OS trust anchors — but with rustls's own (revocation-free) verification, NOT
/// Android's `TrustManager`, which mandates OCSP revocation and hard-fails on
/// OCSP-less certs like Let's Encrypt / github (rustls-platform-verifier #221).
pub fn system_ca_certs() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm() as *mut _) }) else {
        return out;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return out;
    };
    let _ = collect_ca_certs(&mut env, &mut out);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    raw_log(&format!("system_ca_certs: {} certs", out.len()));
    out
}

fn collect_ca_certs(env: &mut JNIEnv<'_>, out: &mut Vec<Vec<u8>>) -> Option<()> {
    let algo = env
        .call_static_method(
            "javax/net/ssl/TrustManagerFactory",
            "getDefaultAlgorithm",
            "()Ljava/lang/String;",
            &[],
        )
        .ok()?
        .l()
        .ok()?;
    let algo = unsafe { JString::from_raw(algo.into_raw()) };
    let tmf = env
        .call_static_method(
            "javax/net/ssl/TrustManagerFactory",
            "getInstance",
            "(Ljava/lang/String;)Ljavax/net/ssl/TrustManagerFactory;",
            &[(&algo).into()],
        )
        .ok()?
        .l()
        .ok()?;
    // init(null) -> use the platform's default trust store.
    env.call_method(&tmf, "init", "(Ljava/security/KeyStore;)V", &[(&JObject::null()).into()])
        .ok()?;
    let tms = env
        .call_method(&tmf, "getTrustManagers", "()[Ljavax/net/ssl/TrustManager;", &[])
        .ok()?
        .l()
        .ok()?;
    let tms = unsafe { JObjectArray::from_raw(tms.into_raw()) };
    let x509 = env.find_class("javax/net/ssl/X509TrustManager").ok()?;
    for i in 0..env.get_array_length(&tms).unwrap_or(0) {
        let tm = env.get_object_array_element(&tms, i).ok()?;
        if !env.is_instance_of(&tm, &x509).unwrap_or(false) {
            continue;
        }
        let issuers = env
            .call_method(&tm, "getAcceptedIssuers", "()[Ljava/security/cert/X509Certificate;", &[])
            .ok()?
            .l()
            .ok()?;
        let issuers = unsafe { JObjectArray::from_raw(issuers.into_raw()) };
        for j in 0..env.get_array_length(&issuers).unwrap_or(0) {
            let cert = env.get_object_array_element(&issuers, j).ok()?;
            let der = env
                .call_method(&cert, "getEncoded", "()[B", &[])
                .ok()?
                .l()
                .ok()?;
            let der = unsafe { JByteArray::from_raw(der.into_raw()) };
            if let Ok(bytes) = env.convert_byte_array(&der) {
                out.push(bytes);
            }
        }
    }
    Some(())
}

// ---------------------------------------------------------------------------
// JNI externs called from `RustyboiActivity.java`.
// ---------------------------------------------------------------------------

/// Called from Java when the SAF picker returns a chosen ROM.
///
/// This path is the legacy single-document picker (File → Load ROM).
/// Battery save persistence is unavailable on this path because we
/// can't reliably open a writable sibling `.sav` from a per-document
/// SAF grant; users who want save persistence should use the ROM
/// Library instead.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_mcswain_rustyboi_RustyboiActivity_nativeOnRomPicked<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    bytes: JByteArray<'local>,
    file_name: JString<'local>,
) {
    let data: Vec<u8> = match env.convert_byte_array(&bytes) {
        Ok(v) => v,
        Err(e) => {
            log::error!("nativeOnRomPicked: convert_byte_array failed: {e}");
            invoke_pending(None);
            return;
        }
    };
    let name: String = match env.get_string(&file_name) {
        Ok(s) => s.into(),
        Err(e) => {
            log::warn!("nativeOnRomPicked: failed to read filename: {e}");
            "rom.gb".to_string()
        }
    };
    log::info!(
        "nativeOnRomPicked: received {} bytes ({})",
        data.len(),
        name
    );
    // No SAV fd available on this path — leave PENDING_SAV_FD untouched.
    invoke_pending(Some(FileData::Contents { name, data }));
}

/// Called from Java when the SAF picker is dismissed or fails.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_mcswain_rustyboi_RustyboiActivity_nativeOnRomPickCancelled<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    log::info!("nativeOnRomPickCancelled");
    invoke_pending(None);
}

use std::sync::atomic::{AtomicU32, Ordering};

// Latest gamepad axis values (f32 bit-patterns), written by the Java
// onGenericMotionEvent callback (UI thread) and read by the render loop. winit
// drops gamepad motion events, so analog sticks + hat come through Java/JNI here;
// buttons still arrive as native key events handled in the event loop.
static PAD_LX: AtomicU32 = AtomicU32::new(0);
static PAD_LY: AtomicU32 = AtomicU32::new(0);
static PAD_RX: AtomicU32 = AtomicU32::new(0);
static PAD_RY: AtomicU32 = AtomicU32::new(0);
static PAD_HX: AtomicU32 = AtomicU32::new(0);
static PAD_HY: AtomicU32 = AtomicU32::new(0);
static PAD_LT: AtomicU32 = AtomicU32::new(0);
static PAD_RT: AtomicU32 = AtomicU32::new(0);

/// Latest gamepad axes `[lx, ly, rx, ry, hat_x, hat_y, l_trigger, r_trigger]`
/// (Android convention: sticks/hat +X right +Y down in [-1, 1]; triggers in
/// [0, 1]). The render loop derives stick/hat/trigger directions from these.
pub fn gamepad_axes() -> [f32; 8] {
    [
        f32::from_bits(PAD_LX.load(Ordering::Relaxed)),
        f32::from_bits(PAD_LY.load(Ordering::Relaxed)),
        f32::from_bits(PAD_RX.load(Ordering::Relaxed)),
        f32::from_bits(PAD_RY.load(Ordering::Relaxed)),
        f32::from_bits(PAD_HX.load(Ordering::Relaxed)),
        f32::from_bits(PAD_HY.load(Ordering::Relaxed)),
        f32::from_bits(PAD_LT.load(Ordering::Relaxed)),
        f32::from_bits(PAD_RT.load(Ordering::Relaxed)),
    ]
}

/// Java `onGenericMotionEvent` forwards controller axes here.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_mcswain_rustyboi_RustyboiActivity_nativeOnGamepadAxes<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    lx: f32,
    ly: f32,
    rx: f32,
    ry: f32,
    hat_x: f32,
    hat_y: f32,
    lt: f32,
    rt: f32,
) {
    PAD_LX.store(lx.to_bits(), Ordering::Relaxed);
    PAD_LY.store(ly.to_bits(), Ordering::Relaxed);
    PAD_RX.store(rx.to_bits(), Ordering::Relaxed);
    PAD_RY.store(ry.to_bits(), Ordering::Relaxed);
    PAD_HX.store(hat_x.to_bits(), Ordering::Relaxed);
    PAD_HY.store(hat_y.to_bits(), Ordering::Relaxed);
    PAD_LT.store(lt.to_bits(), Ordering::Relaxed);
    PAD_RT.store(rt.to_bits(), Ordering::Relaxed);
}

/// Bind this process's native sockets + DNS to the active network.
///
/// A native app's `getaddrinfo` (and thus `ureq`) is not associated with any
/// network by default, so from a worker thread it fails with `EAI_NODATA` ("No
/// address associated with hostname") even with INTERNET granted and the device
/// online — while the framework HTTP stack (browsers) works. Java
/// `bindProcessToNetwork` does not reliably reach native resolution (per the
/// Android docs, native socket calls may bypass Java-level bindings); the NDK
/// `android_setprocnetwork` (from `<android/multinetwork.h>`, API 23) does, per
/// its contract "all host name resolutions will be limited to network as well".
///
/// The only way to obtain the active network's `net_handle_t` is Java's
/// `Network.getNetworkHandle()` (needs ACCESS_NETWORK_STATE), which we then pass
/// to the native call. Idempotent; call before networking.
pub fn bind_process_to_network() {
    let ctx = ndk_context::android_context();
    let vm = match unsafe { JavaVM::from_raw(ctx.vm() as *mut _) } {
        Ok(v) => v,
        Err(e) => {
            raw_log(&format!("bind_process_to_network: vm: {e}"));
            return;
        }
    };
    let mut env = match vm.attach_current_thread() {
        Ok(e) => e,
        Err(e) => {
            raw_log(&format!("bind_process_to_network: attach: {e}"));
            return;
        }
    };
    let handle = active_network_handle(&mut env, ctx.context() as jobject);
    // Never let a pending JNI exception reach the guard's detach (that aborts).
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    match handle {
        Some(h) => {
            let r = unsafe { ndk_sys::android_setprocnetwork(h) };
            let e1 = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            raw_log(&format!("android_setprocnetwork(0x{h:x}) = {r} (errno {e1})"));
            if r != 0 {
                // Alternate: bind only the resolver (API 31+), which has laxer
                // requirements than binding all sockets.
                let rd = unsafe { ndk_sys::android_setprocdns(h) };
                let e2 = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                raw_log(&format!("android_setprocdns(0x{h:x}) = {rd} (errno {e2})"));
            }
        }
        None => raw_log("bind_process_to_network: no usable network handle"),
    }
}

/// Take + return the pending Java exception's `toString()` (clearing it), or None.
fn take_exception(env: &mut JNIEnv<'_>) -> Option<String> {
    if !env.exception_check().unwrap_or(false) {
        return None;
    }
    let exc = env.exception_occurred().ok();
    let _ = env.exception_clear();
    exc.and_then(|exc| {
        env.call_method(&exc, "toString", "()Ljava/lang/String;", &[])
            .and_then(|v| v.l())
            .ok()
            .and_then(|o| {
                let s = unsafe { JString::from_raw(o.into_raw()) };
                env.get_string(&s).ok().map(|js| js.to_string_lossy().into_owned())
            })
    })
}

fn net_handle(env: &mut JNIEnv<'_>, net: &JObject<'_>) -> Option<u64> {
    env.call_method(net, "getNetworkHandle", "()J", &[]).ok()?.j().ok().map(|h| h as u64)
}

fn active_network_handle(env: &mut JNIEnv<'_>, context_raw: jobject) -> Option<u64> {
    let activity = unsafe { JObject::from_raw(context_raw) };
    let svc = env.new_string("connectivity").ok()?;
    let cm = env
        .call_method(
            &activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[(&svc).into()],
        )
        .ok()?
        .l()
        .ok()?;
    if cm.is_null() {
        raw_log("bind: no ConnectivityManager");
        return None;
    }

    // Preferred: the active/default network.
    match env
        .call_method(&cm, "getActiveNetwork", "()Landroid/net/Network;", &[])
        .and_then(|v| v.l())
    {
        Ok(net) if !net.is_null() => {
            if let Some(h) = net_handle(env, &net) {
                return Some(h);
            }
        }
        Ok(_) => raw_log("bind: getActiveNetwork returned null; scanning all networks"),
        Err(_) => {
            let msg = take_exception(env).unwrap_or_else(|| "error".into());
            raw_log(&format!("bind: getActiveNetwork threw: {msg}"));
        }
    }

    // Fallback: any network reporting the INTERNET capability (NET_CAPABILITY_
    // INTERNET = 12). getActiveNetwork can be null for a native process even when
    // WiFi is up.
    let arr = env
        .call_method(&cm, "getAllNetworks", "()[Landroid/net/Network;", &[])
        .and_then(|v| v.l());
    let arr = match arr {
        Ok(a) if !a.is_null() => unsafe { JObjectArray::from_raw(a.into_raw()) },
        _ => {
            let msg = take_exception(env).unwrap_or_else(|| "null".into());
            raw_log(&format!("bind: getAllNetworks failed: {msg}"));
            return None;
        }
    };
    let len = env.get_array_length(&arr).unwrap_or(0);
    for i in 0..len {
        let Ok(net) = env.get_object_array_element(&arr, i) else {
            continue;
        };
        let caps = env
            .call_method(
                &cm,
                "getNetworkCapabilities",
                "(Landroid/net/Network;)Landroid/net/NetworkCapabilities;",
                &[(&net).into()],
            )
            .and_then(|v| v.l());
        let Ok(caps) = caps else {
            let _ = take_exception(env);
            continue;
        };
        if caps.is_null() {
            continue;
        }
        let has_internet = env
            .call_method(&caps, "hasCapability", "(I)Z", &[12i32.into()])
            .and_then(|v| v.z())
            .unwrap_or(false);
        if has_internet {
            if let Some(h) = net_handle(env, &net) {
                raw_log(&format!("bind: using network #{i} (has INTERNET)"));
                return Some(h);
            }
        }
    }
    raw_log("bind: no network with INTERNET capability");
    None
}

fn invoke_pending(result: Option<FileData>) {
    let cb = {
        let mut slot = PENDING_PICK.lock().expect("PENDING_PICK poisoned");
        slot.take()
    };
    if let Some(cb) = cb {
        cb(result);
    } else {
        log::warn!("rom-pick callback fired with no pending request");
    }
}

// ---------------------------------------------------------------------------
// ROM library JNI externs.
// ---------------------------------------------------------------------------

/// Called from Kotlin after `OpenDocumentTree` returns. `tree_uri` is
/// the persistable tree URI string; an empty/null string means the
/// user cancelled or the grant could not be persisted.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_mcswain_rustyboi_RustyboiActivity_nativeOnTreePicked<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    tree_uri: JString<'local>,
) {
    let uri: Option<String> = if tree_uri.is_null() {
        None
    } else {
        match env.get_string(&tree_uri) {
            Ok(s) => {
                let s: String = s.into();
                if s.is_empty() { None } else { Some(s) }
            }
            Err(e) => {
                log::warn!("nativeOnTreePicked: get_string failed: {e}");
                None
            }
        }
    };
    log::info!("nativeOnTreePicked: {:?}", uri);
    let cb = PENDING_TREE.lock().expect("PENDING_TREE poisoned").take();
    if let Some(cb) = cb {
        cb(uri);
    } else {
        log::warn!("tree-pick callback fired with no pending request");
    }
}

/// Called from Kotlin once the library scan has completed. The payload
/// is a UTF-8 JSON array of `{uri, name, rel_path, size_bytes}`
/// objects. A null/empty string means the tree was no longer
/// accessible.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_mcswain_rustyboi_RustyboiActivity_nativeOnLibraryScanResult<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    entries_json: JString<'local>,
) {
    let cb = PENDING_SCAN.lock().expect("PENDING_SCAN poisoned").take();
    let Some(cb) = cb else {
        log::warn!("scan callback fired with no pending request");
        return;
    };
    if entries_json.is_null() {
        cb(None);
        return;
    }
    let s: String = match env.get_string(&entries_json) {
        Ok(s) => s.into(),
        Err(e) => {
            log::warn!("nativeOnLibraryScanResult: get_string failed: {e}");
            cb(None);
            return;
        }
    };
    let entries = match parse_library_entries(&s) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("nativeOnLibraryScanResult: parse failed: {e}");
            cb(None);
            return;
        }
    };
    log::info!("nativeOnLibraryScanResult: {} entries", entries.len());
    cb(Some(entries));
}

fn parse_library_entries(json: &str) -> Result<Vec<LibraryEntry>, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let arr = match v.as_array() {
        Some(a) => a,
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let uri = item.get("uri").and_then(|x| x.as_str()).unwrap_or("");
        let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let rel_path = item.get("rel_path").and_then(|x| x.as_str()).unwrap_or("");
        let size_bytes = item
            .get("size_bytes")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let crc32 = item.get("crc32").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        if uri.is_empty() {
            continue;
        }
        out.push(LibraryEntry {
            uri: uri.to_string(),
            name: name.to_string(),
            rel_path: rel_path.to_string(),
            size_bytes,
            crc32,
        });
    }
    Ok(out)
}

/// Called from Kotlin after `loadRomEntry` finishes. `rom_bytes`
/// contains the full ROM image; `display_name` is the SAF document
/// name; `sav_fd` is either a valid fd (rw) for the sibling `.sav` or
/// -1 if no save fd could be obtained (the cart will run without
/// persistence in that case).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_mcswain_rustyboi_RustyboiActivity_nativeOnRomLoaded<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    rom_bytes: JByteArray<'local>,
    display_name: JString<'local>,
    sav_fd: jni::sys::jint,
) {
    let data: Vec<u8> = match env.convert_byte_array(&rom_bytes) {
        Ok(v) => v,
        Err(e) => {
            log::error!("nativeOnRomLoaded: convert_byte_array failed: {e}");
            invoke_load(None);
            return;
        }
    };
    let name: String = match env.get_string(&display_name) {
        Ok(s) => s.into(),
        Err(e) => {
            log::warn!("nativeOnRomLoaded: get_string failed: {e}");
            "rom.gb".to_string()
        }
    };
    if sav_fd >= 0 {
        log::info!("nativeOnRomLoaded: sav fd {sav_fd}");
        *PENDING_SAV_FD.lock().expect("PENDING_SAV_FD poisoned") =
            Some(sav_fd as RawFd);
    } else {
        log::info!("nativeOnRomLoaded: no sav fd (-1)");
        *PENDING_SAV_FD.lock().expect("PENDING_SAV_FD poisoned") = None;
    }
    log::info!(
        "nativeOnRomLoaded: {} bytes ({})",
        data.len(),
        name
    );
    invoke_load(Some(FileData::Contents { name, data }));
}

/// Called from Kotlin when `loadRomEntry` failed (URI gone, IO error).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_mcswain_rustyboi_RustyboiActivity_nativeOnRomLoadFailed<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    log::warn!("nativeOnRomLoadFailed");
    invoke_load(None);
}

fn invoke_load(result: Option<FileData>) {
    let cb = PENDING_LOAD.lock().expect("PENDING_LOAD poisoned").take();
    if let Some(cb) = cb {
        cb(result);
    } else {
        log::warn!("load-rom callback fired with no pending request");
    }
}

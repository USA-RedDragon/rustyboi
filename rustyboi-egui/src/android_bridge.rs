//! Platform bridge used on Android.
//!
//! The egui crate is platform-agnostic and can't talk to Android APIs
//! directly. The platform crate's `android_main` installs callbacks
//! here at startup; the GUI invokes them when the user picks a file
//! or asks to save state. The callbacks return their results
//! asynchronously by invoking the supplied closure (off-thread).

#![cfg(target_os = "android")]

use std::path::PathBuf;
use std::sync::Mutex;

use super::actions::{FileData, LibraryEntry};

pub type PickFileCallback = Box<dyn FnOnce(Option<FileData>) + Send + 'static>;
pub type SaveFileCallback = Box<dyn FnOnce(Option<PathBuf>) + Send + 'static>;

pub type PickFileHandler = Box<dyn Fn(PickFileCallback) + Send + Sync + 'static>;
pub type SaveFileHandler = Box<dyn Fn(Option<String>, SaveFileCallback) + Send + Sync + 'static>;

/// Result of an SAF tree pick. `None` means cancelled.
pub type TreePickCallback = Box<dyn FnOnce(Option<String>) + Send + 'static>;
pub type TreePickHandler = Box<dyn Fn(TreePickCallback) + Send + Sync + 'static>;

/// Result of a library scan. `None` means the tree URI was no longer
/// accessible.
pub type ScanCallback = Box<dyn FnOnce(Option<Vec<LibraryEntry>>) + Send + 'static>;
pub type ScanHandler = Box<dyn Fn(String, ScanCallback) + Send + Sync + 'static>;

/// Result of loading a ROM via SAF. The platform layer then drives the
/// usual `GuiAction::LoadRom` flow with the returned `FileData`.
pub type LoadRomCallback = Box<dyn FnOnce(Option<FileData>) + Send + 'static>;
pub type LoadRomHandler = Box<dyn Fn(String, LoadRomCallback) + Send + Sync + 'static>;

/// Show / hide the on-screen keyboard. winit's android-game-activity
/// backend does not currently route `Window::set_ime_allowed()` to
/// `InputMethodManager`, so we plumb this manually: egui widgets call
/// [`set_ime_visible`] when they gain or lose focus, which forwards to
/// the Kotlin activity over JNI.
pub type ImeHandler = Box<dyn Fn(bool) + Send + Sync + 'static>;

/// Handler that displays a transient system Toast on Android.
/// Used in place of the egui status bar on mobile, where the host
/// platform already provides a well-known non-blocking notification
/// affordance.
pub type ToastHandler = Box<dyn Fn(String) + Send + Sync + 'static>;

struct Handlers {
    pick: Option<PickFileHandler>,
    save: Option<SaveFileHandler>,
    pick_tree: Option<TreePickHandler>,
    scan: Option<ScanHandler>,
    load_rom: Option<LoadRomHandler>,
    ime: Option<ImeHandler>,
    toast: Option<ToastHandler>,
}

// Poison-recovering per the tree-wide convention (see `rustyboi_core_lib::ir`).
// This is the cascade's worst case: the accessors below invoke a handler *while
// holding the guard*, so one panicking platform callback would poison the lock
// and permanently brick every file pick, save, scan and toast for the rest of
// the process. The guarded value is a registry of independent handler slots
// that a panic in a handler cannot corrupt, so recovery is exactly right.
static HANDLERS: Mutex<Handlers> = Mutex::new(Handlers {
    pick: None,
    save: None,
    pick_tree: None,
    scan: None,
    load_rom: None,
    ime: None,
    toast: None,
});

/// Install the platform-provided handlers. Call once from `android_main`.
pub fn install(pick: PickFileHandler, save: SaveFileHandler) {
    let mut h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    h.pick = Some(pick);
    h.save = Some(save);
}

/// Install the Android library handlers. Call once from `android_main`
/// in addition to [`install`]. Separate so the existing API doesn't
/// move.
pub fn install_library(
    pick_tree: TreePickHandler,
    scan: ScanHandler,
    load_rom: LoadRomHandler,
) {
    let mut h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    h.pick_tree = Some(pick_tree);
    h.scan = Some(scan);
    h.load_rom = Some(load_rom);
}

pub(crate) fn pick_file(callback: PickFileCallback) {
    let h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pick) = h.pick.as_ref() {
        pick(callback);
    } else {
        // No handler installed yet; cancel.
        callback(None);
    }
}

pub(crate) fn save_file(file_name: Option<String>, callback: SaveFileCallback) {
    let h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(save) = h.save.as_ref() {
        save(file_name, callback);
    } else {
        callback(None);
    }
}

/// Ask the platform layer to launch the SAF tree picker. Result lands
/// on the supplied callback asynchronously.
pub fn pick_tree(callback: TreePickCallback) {
    let h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handler) = h.pick_tree.as_ref() {
        handler(callback);
    } else {
        callback(None);
    }
}

/// Ask the platform layer to scan the given SAF tree URI for ROMs.
pub fn scan_library(tree_uri: String, callback: ScanCallback) {
    let h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handler) = h.scan.as_ref() {
        handler(tree_uri, callback);
    } else {
        callback(None);
    }
}

/// Ask the platform layer to load the ROM identified by the supplied
/// SAF document URI. The platform side is responsible for opening the
/// ROM bytes, locating/creating the sibling `.sav`, and (on success)
/// stashing the SAV fd before invoking the callback.
pub fn load_rom_from_uri(uri: String, callback: LoadRomCallback) {
    let h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handler) = h.load_rom.as_ref() {
        handler(uri, callback);
    } else {
        callback(None);
    }
}

/// Install the soft-keyboard handler. Call once from `android_main`.
pub fn install_ime(handler: ImeHandler) {
    let mut h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    h.ime = Some(handler);
}

/// Request that the on-screen keyboard be shown (`true`) or hidden
/// (`false`). No-op if the IME handler hasn't been installed yet.
pub fn set_ime_visible(visible: bool) {
    let h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handler) = h.ime.as_ref() {
        handler(visible);
    }
}

/// Install the toast handler. Call once from `android_main`.
pub fn install_toast(handler: ToastHandler) {
    let mut h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    h.toast = Some(handler);
}

/// Display a transient system Toast with `message`. No-op if the
/// toast handler hasn't been installed yet (e.g. before `android_main`
/// has finished wiring the bridge).
pub fn show_toast(message: impl Into<String>) {
    let h = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handler) = h.toast.as_ref() {
        handler(message.into());
    }
}

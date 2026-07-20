//! Platform bridge used on iOS.
//!
//! The egui crate is platform-agnostic and can't talk to UIKit directly. The
//! platform crate's `run_ios` installs a pick-file handler here at startup; the
//! GUI invokes it when the user picks a ROM. The handler presents a
//! `UIDocumentPickerViewController` (via objc2) and returns the picked bytes
//! asynchronously by invoking the supplied closure. Mirrors `android_bridge`,
//! trimmed to the one affordance iOS needs (exports go through `SaveBytes` /
//! the Documents dir, not a save dialog).

#![cfg(target_os = "ios")]

use std::sync::Mutex;

use super::actions::FileData;

pub type PickFileCallback = Box<dyn FnOnce(Option<FileData>) + Send + 'static>;
pub type PickFileHandler = Box<dyn Fn(PickFileCallback) + Send + Sync + 'static>;

// Poison-recovering per the tree-wide convention (see `rustyboi_core_lib::ir`);
// `pick_file` invokes the handler while holding the guard, so a panicking
// handler would otherwise brick the picker permanently. Plain handler slot.
static HANDLER: Mutex<Option<PickFileHandler>> = Mutex::new(None);

/// Install the platform-provided pick-file handler. Call once from `run_ios`.
pub fn install(pick: PickFileHandler) {
    *HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(pick);
}

/// Ask the platform layer to present the document picker. The picked file's
/// bytes land on `callback` (or `None` if cancelled / no handler installed).
pub(crate) fn pick_file(callback: PickFileCallback) {
    let h = HANDLER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pick) = h.as_ref() {
        pick(callback);
    } else {
        callback(None);
    }
}

//! iOS platform layer: sandbox directories and the native document picker.
//!
//! The egui crate can't call UIKit, so `run_ios` installs
//! [`present_document_picker`] on `ios_bridge`; the GUI calls it when the user
//! picks a ROM. We present a `UIDocumentPickerViewController` (via objc2, no
//! Objective-C source) and deliver the picked bytes back through the stashed
//! callback. Everything here runs on the main thread — the winit event loop
//! *is* `UIApplicationMain`, and the picker is only ever launched from the egui
//! draw on that thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSArray, NSData, NSObjectProtocol, NSURL};
use objc2_ui_kit::{UIApplication, UIDocumentPickerDelegate, UIDocumentPickerViewController};
use objc2_uniform_type_identifiers::UTTypeData;

use rustyboi_frontend_lib::ios_bridge::PickFileCallback;
use rustyboi_frontend_lib::FileData;

// ---------------------------------------------------------------------------
// Sandbox directories
// ---------------------------------------------------------------------------

/// The app sandbox root. On iOS `HOME` is set to the container by the runtime
/// (same value as `NSHomeDirectory()`), so no UIKit call is needed.
fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Where savestates + session config live: `Library/Application Support`
/// (backed up, not user-visible). Created if missing.
pub fn save_dir() -> PathBuf {
    let dir = home().join("Library/Application Support/rustyboi");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The user-visible `Documents` dir (surfaced in Files/Finder via the
/// file-sharing Info.plist keys) — where exports land. Created if missing.
pub fn documents_dir() -> PathBuf {
    let dir = home().join("Documents");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

// ---------------------------------------------------------------------------
// Document picker
// ---------------------------------------------------------------------------

// The in-flight pick callback (one picker at a time; main thread only).
static PENDING: Mutex<Option<PickFileCallback>> = Mutex::new(None);
// A strong ref to the live delegate, kept because `picker.delegate` is weak.
// Stored as a raw pointer (Retained isn't Send) and reclaimed in `finish`.
static DELEGATE_PTR: AtomicUsize = AtomicUsize::new(0);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustyboiDocumentPickerDelegate"]
    struct PickerDelegate;

    unsafe impl NSObjectProtocol for PickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn did_pick(&self, _picker: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            finish(read_first_url(urls));
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn was_cancelled(&self, _picker: &UIDocumentPickerViewController) {
            finish(None);
        }
    }
);

/// Read the first picked URL's bytes into a `FileData::Contents`. The URL is
/// security-scoped: bracket the read with start/stop access.
fn read_first_url(urls: &NSArray<NSURL>) -> Option<FileData> {
    let url = urls.firstObject()?;
    // SAFETY: called on a URL vended by the picker, on the main thread.
    let accessed = unsafe { url.startAccessingSecurityScopedResource() };
    let data = NSData::dataWithContentsOfURL(&url);
    if accessed {
        unsafe { url.stopAccessingSecurityScopedResource() };
    }
    let data = data?;
    let name = url
        .lastPathComponent()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "rom.gb".to_string());
    Some(FileData::Contents {
        name,
        data: data.to_vec(),
    })
}

/// Reclaim the retained delegate and invoke the stashed callback exactly once.
fn finish(result: Option<FileData>) {
    let raw = DELEGATE_PTR.swap(0, Ordering::SeqCst);
    if raw != 0 {
        // SAFETY: `raw` came from `Retained::into_raw` in `present_document_picker`.
        drop(unsafe { Retained::from_raw(raw as *mut PickerDelegate) });
    }
    if let Some(cb) = PENDING.lock().expect("ios picker PENDING poisoned").take() {
        cb(result);
    }
}

/// Present the system document picker for a ROM. Installed on `ios_bridge` from
/// `run_ios`; invoked from the egui draw (main thread). Delivers the picked
/// file's bytes (or `None` on cancel/failure) to `callback`.
// `keyWindow` is deprecated for multi-scene apps; this is a single-window winit
// app (no scene manifest), so it returns the one correct window.
#[allow(deprecated)]
pub fn present_document_picker(callback: PickFileCallback) {
    let Some(mtm) = MainThreadMarker::new() else {
        callback(None);
        return;
    };
    *PENDING.lock().expect("ios picker PENDING poisoned") = Some(callback);

    let app = UIApplication::sharedApplication(mtm);
    let Some(root) = app.keyWindow().and_then(|w| w.rootViewController()) else {
        finish(None);
        return;
    };

    // `UTTypeData` = any file; the user points at their .gb/.gbc/.zip.
    // SAFETY: `UTTypeData` is a framework constant (a `&'static UTType`).
    let data_type = unsafe { UTTypeData };
    let types = NSArray::from_slice(&[data_type]);
    let picker = UIDocumentPickerViewController::initForOpeningContentTypes(
        UIDocumentPickerViewController::alloc(mtm),
        &types,
    );

    let delegate = PickerDelegate::alloc(mtm).set_ivars(());
    let delegate: Retained<PickerDelegate> = unsafe { msg_send![super(delegate), init] };
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // Keep the delegate alive past this call (the picker holds it weakly).
    DELEGATE_PTR.store(Retained::into_raw(delegate) as usize, Ordering::SeqCst);

    root.presentViewController_animated_completion(&picker, true, None);
}

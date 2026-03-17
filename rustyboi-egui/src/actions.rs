pub enum FileData {
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    Path(std::path::PathBuf),
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    Contents { name: String, data: Vec<u8> },
}

/// A single ROM discovered by the Android library scanner.
///
/// The `uri` is an opaque SAF document URI string (e.g.
/// `content://com.android.externalstorage.documents/tree/.../document/...`).
/// `rel_path` is a slash-separated path from the picked tree root used
/// purely for display.
#[cfg(target_os = "android")]
#[derive(Clone, Debug)]
pub struct LibraryEntry {
    pub uri: String,
    pub name: String,
    pub rel_path: String,
    pub size_bytes: u64,
}

pub enum GuiAction {
    Exit,
    SaveState(std::path::PathBuf),
    LoadState(FileData),
    LoadRom(FileData),
    TogglePause,
    /// Plug/unplug a Game Boy Printer on the link port.
    TogglePrinter,
    Restart,
    ClearError,
    StepCycles(u32),
    StepFrames(u32),
    SetBreakpoint(u16),
    RemoveBreakpoint(u16),
    /// User asked to pick a new ROM library root (SAF tree).
    #[cfg(target_os = "android")]
    OpenRomTree,
    /// User asked to rescan the existing library tree.
    #[cfg(target_os = "android")]
    RescanLibrary,
    /// User clicked a library entry; load that ROM via SAF.
    #[cfg(target_os = "android")]
    LoadRomFromUri(String),
    /// Internal: the Android tree-pick callback completed. `None` =
    /// user cancelled or the grant could not be persisted. Pushed by
    /// the JNI callback; the event loop applies it to the library
    /// panel.
    #[cfg(target_os = "android")]
    SetLibraryTreeUri(Option<String>),
    /// Internal: the Android library scan callback returned `entries`
    /// (possibly empty). `None` means the tree URI was no longer
    /// accessible.
    #[cfg(target_os = "android")]
    SetLibraryEntries(Option<Vec<LibraryEntry>>),
}

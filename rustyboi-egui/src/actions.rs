#[derive(Debug)]
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

/// A four-shade DMG palette choice surfaced in the Settings menu. Mirrors the
/// platform's built-in palettes without the egui crate depending on the
/// platform crate; the adapter maps it to concrete RGBA shades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteChoice {
    Grayscale,
    OriginalGreen,
    Blue,
    Brown,
    Red,
}

/// The hardware model choices surfaced in the Settings menu. Mirrors the
/// core's `Hardware` without pulling its full enum surface into the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareChoice {
    Dmg,
    Cgb,
    Sgb,
}

/// A snapshot of session-owned state the menus need to render current
/// selections (checkmarks, radio dots, slot list). Passed into the UI each
/// frame; the UI never mutates the session directly, it only emits
/// [`GuiAction`]s the adapter applies.
#[derive(Clone, Debug)]
pub struct SessionUiState {
    pub hardware: HardwareChoice,
    pub palette: PaletteChoice,
    pub rewind_enabled: bool,
    pub rewind_interval_frames: u32,
    pub rewind_depth: usize,
    pub sgb_border: bool,
    pub fast_forward: bool,
    /// Slot numbers that currently hold a saved state, ascending.
    pub slots: Vec<u32>,
}

impl Default for SessionUiState {
    fn default() -> Self {
        SessionUiState {
            hardware: HardwareChoice::Cgb,
            palette: PaletteChoice::Grayscale,
            rewind_enabled: true,
            rewind_interval_frames: 6,
            rewind_depth: 90,
            sgb_border: true,
            fast_forward: false,
            slots: Vec::new(),
        }
    }
}

#[derive(Debug)]
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
    /// Save the current machine into numbered savestate slot `n`.
    SaveSlot(u32),
    /// Load numbered savestate slot `n`.
    LoadSlot(u32),
    /// Quicksave to the reserved quick slot.
    Quicksave,
    /// Quickload from the reserved quick slot.
    Quickload,
    /// Toggle fast-forward / turbo on and off.
    ToggleFastForward,
    /// Advance exactly one frame, then pause.
    FrameAdvance,
    /// Toggle presenting the Super Game Boy border composite.
    ToggleSgbBorder,
    /// Change the emulated hardware model (rebuilds the machine).
    SetHardware(HardwareChoice),
    /// Change the DMG presentation palette.
    SetPalette(PaletteChoice),
    /// Enable/disable rewind capture.
    SetRewindEnabled(bool),
    /// Set the rewind snapshot interval (frames between captures).
    SetRewindInterval(u32),
    /// Set how many rewind snapshots are retained.
    SetRewindDepth(usize),
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

//! The canonical, toolkit-agnostic UI-action contract.
//!
//! Every user command any windowed frontend issues is one [`UiAction`] variant.
//! [`Session::apply`](crate::session::Session::apply) is the single implementation
//! of their behavior; pure/session actions are handled there fully, while actions
//! that need an OS service (opening a file, writing bytes to a path, exiting,
//! resizing the window) come back as [`PlatformRequest`]s the frontend performs.
//!
//! The egui widgets, the desktop `App`, the Android adapter, and (later) the web
//! worker all speak this one vocabulary: the widgets emit `UiAction`s and never
//! implement behavior; the frontends call `apply` and service the requests.
//!
//! [`COMMANDS`] is the menu/keymap source of truth: a frontend builds its menus
//! and default key/overlay bindings by iterating it, so adding a command here
//! surfaces it everywhere.

use crate::input::GbButton;
use crate::input_config::InputConfig;
use serde::{Deserialize, Serialize};

/// A file handed to the session by the frontend's picker. Desktop passes a path
/// (the frontend reads it); web/Android pass already-loaded bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileData {
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    Path(std::path::PathBuf),
    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    Contents { name: String, data: Vec<u8> },
}

/// What a picked file is being loaded AS, so the frontend routes the resolved
/// bytes to the right `finish_*` (a ROM, a savestate, a battery `.sav`, or an
/// `.rtc` blob). Carried alongside the [`FileData`] on a
/// [`LoadFile`](crate::apply::PlatformRequest::LoadFile) request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadPurpose {
    Rom,
    State,
    Battery,
    Rtc,
}

/// A single ROM discovered by the Android library scanner.
///
/// The `uri` is an opaque SAF document URI string (e.g.
/// `content://com.android.externalstorage.documents/tree/.../document/...`).
/// `rel_path` is a slash-separated path from the picked tree root used purely
/// for display.
#[cfg(target_os = "android")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    pub uri: String,
    pub name: String,
    pub rel_path: String,
    pub size_bytes: u64,
}

/// A four-shade DMG palette choice surfaced in the Settings menu. Toolkit- and
/// platform-agnostic; the adapter maps it to concrete RGBA shades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteChoice {
    Grayscale,
    OriginalGreen,
    Blue,
    Brown,
    Red,
}

/// The hardware model choices surfaced in the Settings menu. Mirrors the core's
/// `Hardware` without pulling its full enum surface into the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareChoice {
    Dmg,
    Cgb,
    Sgb,
}

/// How the emulated frame is fit into its render region (letterboxing policy).
/// `FitAspect` is the historical behavior (aspect-preserving contain);
/// `IntegerAspect` snaps to the largest whole-number scale; `Stretch` fills the
/// region on both axes, ignoring aspect. Serde-derived so it persists in
/// [`Config`](crate::config::Config).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalingMode {
    FitAspect,
    IntegerAspect,
    Stretch,
}

impl Default for ScalingMode {
    fn default() -> Self {
        ScalingMode::FitAspect
    }
}

/// A snapshot of session-owned state the menus render current selections from
/// (checkmarks, radio dots, slot list). The UI never mutates the session
/// directly; it reads this and emits [`UiAction`]s the session applies.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionUiState {
    pub hardware: HardwareChoice,
    pub palette: PaletteChoice,
    pub rewind_enabled: bool,
    pub rewind_interval_frames: u32,
    pub rewind_depth: usize,
    /// Master output volume, 0..=100 (scales the session's drained audio copy).
    pub volume: u8,
    /// How the frame is letterboxed in the render region.
    pub scaling: ScalingMode,
    pub sgb_border: bool,
    pub fast_forward: bool,
    /// Whether the on-screen touch overlay is shown.
    pub touch_controls: bool,
    /// Slot numbers that currently hold a saved state, ascending.
    pub slots: Vec<u32>,
    /// Active cheat codes, in insertion order.
    pub cheats: Vec<String>,
    /// Whether the inserted cartridge has battery-backed SRAM (gates the
    /// Import/Export Battery Save menu items).
    pub has_battery: bool,
    /// Whether the inserted cartridge has a real-time clock (gates the
    /// Import/Export RTC menu items).
    pub has_rtc: bool,
    /// The live rebindable input map (GB-button bindings + chord hotkeys) the
    /// keybind editor reads/writes. Mirrors [`Config::input`](crate::config::Config).
    pub input: InputConfig,
}

impl Default for SessionUiState {
    fn default() -> Self {
        SessionUiState {
            hardware: HardwareChoice::Cgb,
            palette: PaletteChoice::Grayscale,
            rewind_enabled: true,
            rewind_interval_frames: 6,
            rewind_depth: 90,
            volume: 100,
            scaling: ScalingMode::FitAspect,
            sgb_border: true,
            fast_forward: false,
            touch_controls: cfg!(target_os = "android"),
            slots: Vec::new(),
            cheats: Vec::new(),
            has_battery: false,
            has_rtc: false,
            input: InputConfig::default(),
        }
    }
}

/// The authoritative set of user commands. Every frontend emits these; the
/// behavior is implemented exactly once in
/// [`Session::apply`](crate::session::Session::apply).
#[derive(Debug)]
pub enum UiAction {
    /// The user asked to quit.
    Exit,
    /// Save the current machine state to an arbitrary path (File → Save State).
    SaveState(std::path::PathBuf),
    /// Load a savestate from a picked file.
    LoadState(FileData),
    /// Load a ROM from a picked file.
    LoadRom(FileData),
    /// Import a savestate from a picked file (explicit File → Import, distinct
    /// from the numbered/quick slots).
    ImportState(FileData),
    /// Export the current machine state as a downloadable/saveable file (File →
    /// Export). Unlike [`SaveState`](Self::SaveState) this carries no path, so it
    /// works uniformly on web (browser download) as well as desktop/Android.
    ExportState,
    /// Import a battery `.sav` image into the current cartridge.
    ImportBatterySave(FileData),
    /// Export the current cartridge's battery SRAM as a `.sav` file.
    ExportBatterySave,
    /// Import an `.rtc` clock blob into the current cartridge.
    ImportRtc(FileData),
    /// Export the current cartridge's RTC state as a `.rtc` file.
    ExportRtc,
    /// Toggle pause / resume.
    TogglePause,
    /// Plug/unplug a Game Boy Printer on the link port.
    TogglePrinter,
    /// Power-cycle the current console.
    Restart,
    /// Clear the crash overlay, keeping CPU state for debugging.
    ClearError,
    /// Run `n` CPU instructions (debug multi-step).
    StepCycles(u32),
    /// Run `n` frames (debug multi-step).
    StepFrames(u32),
    /// Set a PC breakpoint.
    SetBreakpoint(u16),
    /// Remove a PC breakpoint.
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
    /// Toggle the on-screen touch controls overlay.
    ToggleTouchControls,
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
    /// Set the master output volume (0..=100).
    SetVolume(u8),
    /// Set how the frame is letterboxed in the render region.
    SetScalingMode(ScalingMode),
    /// Toggle host fullscreen (platform hook: desktop window / web canvas;
    /// Android is already fullscreen). Transient — not persisted config.
    ToggleFullscreen,
    /// Replace the rebindable input map (GB-button bindings + chord hotkeys).
    /// Emitted by the keybind editor; persisted to config in `Session::apply`.
    SetInputConfig(InputConfig),
    /// Add a Game Genie / GameShark cheat code (session-lifetime).
    AddCheat(String),
    /// Remove a previously-added cheat by its raw code string.
    RemoveCheat(String),
    /// User asked to pick a new ROM library root (SAF tree).
    #[cfg(target_os = "android")]
    OpenRomTree,
    /// User asked to rescan the existing library tree.
    #[cfg(target_os = "android")]
    RescanLibrary,
    /// User clicked a library entry; load that ROM via SAF.
    #[cfg(target_os = "android")]
    LoadRomFromUri(String),
    /// Internal: the Android tree-pick callback completed. `None` = cancelled or
    /// the grant could not be persisted.
    #[cfg(target_os = "android")]
    SetLibraryTreeUri(Option<String>),
    /// Internal: the Android library scan callback returned `entries`. `None`
    /// means the tree URI was no longer accessible.
    #[cfg(target_os = "android")]
    SetLibraryEntries(Option<Vec<LibraryEntry>>),
}

impl UiAction {
    /// The [`ActionKind`] discriminant for this action (menu/keymap binding).
    pub fn kind(&self) -> ActionKind {
        match self {
            UiAction::Exit => ActionKind::Exit,
            UiAction::SaveState(_) => ActionKind::SaveState,
            UiAction::LoadState(_) => ActionKind::LoadState,
            UiAction::LoadRom(_) => ActionKind::LoadRom,
            UiAction::ImportState(_) => ActionKind::ImportState,
            UiAction::ExportState => ActionKind::ExportState,
            UiAction::ImportBatterySave(_) => ActionKind::ImportBatterySave,
            UiAction::ExportBatterySave => ActionKind::ExportBatterySave,
            UiAction::ImportRtc(_) => ActionKind::ImportRtc,
            UiAction::ExportRtc => ActionKind::ExportRtc,
            UiAction::TogglePause => ActionKind::TogglePause,
            UiAction::TogglePrinter => ActionKind::TogglePrinter,
            UiAction::Restart => ActionKind::Restart,
            UiAction::ClearError => ActionKind::ClearError,
            UiAction::StepCycles(_) => ActionKind::StepCycles,
            UiAction::StepFrames(_) => ActionKind::StepFrames,
            UiAction::SetBreakpoint(_) => ActionKind::SetBreakpoint,
            UiAction::RemoveBreakpoint(_) => ActionKind::RemoveBreakpoint,
            UiAction::SaveSlot(_) => ActionKind::SaveSlot,
            UiAction::LoadSlot(_) => ActionKind::LoadSlot,
            UiAction::Quicksave => ActionKind::Quicksave,
            UiAction::Quickload => ActionKind::Quickload,
            UiAction::ToggleFastForward => ActionKind::ToggleFastForward,
            UiAction::FrameAdvance => ActionKind::FrameAdvance,
            UiAction::ToggleSgbBorder => ActionKind::ToggleSgbBorder,
            UiAction::ToggleTouchControls => ActionKind::ToggleTouchControls,
            UiAction::SetHardware(_) => ActionKind::SetHardware,
            UiAction::SetPalette(_) => ActionKind::SetPalette,
            UiAction::SetRewindEnabled(_) => ActionKind::SetRewindEnabled,
            UiAction::SetRewindInterval(_) => ActionKind::SetRewindInterval,
            UiAction::SetRewindDepth(_) => ActionKind::SetRewindDepth,
            UiAction::SetVolume(_) => ActionKind::SetVolume,
            UiAction::SetScalingMode(_) => ActionKind::SetScalingMode,
            UiAction::ToggleFullscreen => ActionKind::ToggleFullscreen,
            UiAction::SetInputConfig(_) => ActionKind::SetInputConfig,
            UiAction::AddCheat(_) => ActionKind::AddCheat,
            UiAction::RemoveCheat(_) => ActionKind::RemoveCheat,
            #[cfg(target_os = "android")]
            UiAction::OpenRomTree => ActionKind::OpenRomTree,
            #[cfg(target_os = "android")]
            UiAction::RescanLibrary => ActionKind::RescanLibrary,
            #[cfg(target_os = "android")]
            UiAction::LoadRomFromUri(_) => ActionKind::LoadRomFromUri,
            #[cfg(target_os = "android")]
            UiAction::SetLibraryTreeUri(_) => ActionKind::SetLibraryTreeUri,
            #[cfg(target_os = "android")]
            UiAction::SetLibraryEntries(_) => ActionKind::SetLibraryEntries,
        }
    }
}

/// A payload-free discriminant naming a [`UiAction`] variant, used by the
/// [`COMMANDS`] table so menus/keymaps can describe an action without carrying
/// its runtime data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Exit,
    SaveState,
    LoadState,
    LoadRom,
    ImportState,
    ExportState,
    ImportBatterySave,
    ExportBatterySave,
    ImportRtc,
    ExportRtc,
    TogglePause,
    TogglePrinter,
    Restart,
    ClearError,
    StepCycles,
    StepFrames,
    SetBreakpoint,
    RemoveBreakpoint,
    SaveSlot,
    LoadSlot,
    Quicksave,
    Quickload,
    ToggleFastForward,
    FrameAdvance,
    ToggleSgbBorder,
    ToggleTouchControls,
    SetHardware,
    SetPalette,
    SetRewindEnabled,
    SetRewindInterval,
    SetRewindDepth,
    SetVolume,
    SetScalingMode,
    ToggleFullscreen,
    SetInputConfig,
    AddCheat,
    RemoveCheat,
    #[cfg(target_os = "android")]
    OpenRomTree,
    #[cfg(target_os = "android")]
    RescanLibrary,
    #[cfg(target_os = "android")]
    LoadRomFromUri,
    #[cfg(target_os = "android")]
    SetLibraryTreeUri,
    #[cfg(target_os = "android")]
    SetLibraryEntries,
}

/// Which top-level menu a command belongs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCategory {
    File,
    Emulation,
    Debug,
    Settings,
    View,
}

/// A host-agnostic key binding a frontend maps to its own key vocabulary. Only
/// the keys rustyboi actually binds are named; the frontend classifies its own
/// events into these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyBind {
    F5,
    F8,
    Tab,
    Backslash,
    Backspace,
    KeyF,
    KeyN,
    Escape,
}

/// A menu/keymap descriptor for one command. Frontends iterate [`COMMANDS`] to
/// build menus, default keymaps, and (for the overlay) button mappings, so a new
/// command added to the table surfaces everywhere without per-frontend edits.
#[derive(Clone, Copy, Debug)]
pub struct CommandDescriptor {
    pub action_kind: ActionKind,
    pub label: &'static str,
    pub category: MenuCategory,
    pub default_keybind: Option<KeyBind>,
    /// The GB button this command maps to when surfaced on an input overlay
    /// (none for menu-only commands).
    pub overlay_button: Option<GbButton>,
}

/// The canonical command table: the single source menus + default keymaps are
/// generated from. Payload-carrying actions (slots, breakpoints, hardware,
/// palette, rewind values, file loads) are surfaced by their frontends with the
/// concrete value; this table names the command itself, its menu home, its
/// default key, and any overlay button.
pub const COMMANDS: &[CommandDescriptor] = &[
    CommandDescriptor {
        action_kind: ActionKind::LoadRom,
        label: "Load ROM",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SaveState,
        label: "Save State",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::LoadState,
        label: "Load State",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ImportState,
        label: "Import Save State…",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ExportState,
        label: "Export Save State…",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ImportBatterySave,
        label: "Import Battery Save…",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ExportBatterySave,
        label: "Export Battery Save…",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ImportRtc,
        label: "Import RTC…",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ExportRtc,
        label: "Export RTC…",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::Quicksave,
        label: "Quicksave",
        category: MenuCategory::File,
        default_keybind: Some(KeyBind::F5),
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::Quickload,
        label: "Quickload",
        category: MenuCategory::File,
        default_keybind: Some(KeyBind::F8),
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SaveSlot,
        label: "Save State to Slot",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::LoadSlot,
        label: "Load State from Slot",
        category: MenuCategory::File,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::Exit,
        label: "Exit",
        category: MenuCategory::File,
        default_keybind: Some(KeyBind::Escape),
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::Restart,
        label: "Restart",
        category: MenuCategory::Emulation,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::TogglePause,
        label: "Pause",
        category: MenuCategory::Emulation,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ToggleFastForward,
        label: "Fast-Forward",
        category: MenuCategory::Emulation,
        default_keybind: Some(KeyBind::Tab),
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::FrameAdvance,
        label: "Frame Advance",
        category: MenuCategory::Emulation,
        default_keybind: Some(KeyBind::Backslash),
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ToggleSgbBorder,
        label: "SGB border",
        category: MenuCategory::Emulation,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::TogglePrinter,
        label: "Game Boy Printer",
        category: MenuCategory::Emulation,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::StepFrames,
        label: "Step Frames",
        category: MenuCategory::Debug,
        default_keybind: Some(KeyBind::KeyF),
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::StepCycles,
        label: "Step Cycles",
        category: MenuCategory::Debug,
        default_keybind: Some(KeyBind::KeyN),
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetBreakpoint,
        label: "Set Breakpoint",
        category: MenuCategory::Debug,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetHardware,
        label: "Hardware Model",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetPalette,
        label: "DMG Palette",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetRewindEnabled,
        label: "Rewind",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::ToggleTouchControls,
        label: "On-screen Controls",
        category: MenuCategory::View,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::AddCheat,
        label: "Cheats",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
];

impl HardwareChoice {
    /// Map a core [`Hardware`](rustyboi_core_lib::gb::Hardware) to the menu
    /// choice (DMG/MGB collapse to Dmg; anything else that isn't SGB is Cgb).
    pub fn from_hardware(hw: rustyboi_core_lib::gb::Hardware) -> Self {
        use rustyboi_core_lib::gb::Hardware;
        match hw {
            Hardware::DMG | Hardware::MGB => HardwareChoice::Dmg,
            Hardware::SGB => HardwareChoice::Sgb,
            _ => HardwareChoice::Cgb,
        }
    }

    /// The core [`Hardware`](rustyboi_core_lib::gb::Hardware) for this choice.
    pub fn to_hardware(self) -> rustyboi_core_lib::gb::Hardware {
        use rustyboi_core_lib::gb::Hardware;
        match self {
            HardwareChoice::Dmg => Hardware::DMG,
            HardwareChoice::Cgb => Hardware::CGB,
            HardwareChoice::Sgb => Hardware::SGB,
        }
    }
}

impl PaletteChoice {
    /// The choice whose [`rgba_shades`](Self::rgba_shades) match `shades`, or
    /// `Grayscale` if none do (e.g. a custom palette persisted by a frontend).
    pub fn from_shades(shades: [[u8; 4]; 4]) -> Self {
        for choice in [
            PaletteChoice::Grayscale,
            PaletteChoice::OriginalGreen,
            PaletteChoice::Blue,
            PaletteChoice::Brown,
            PaletteChoice::Red,
        ] {
            if choice.rgba_shades() == shades {
                return choice;
            }
        }
        PaletteChoice::Grayscale
    }

    /// RGBA shades for this palette, GB colors 0-3 (lightest→darkest).
    pub fn rgba_shades(self) -> [[u8; 4]; 4] {
        match self {
            PaletteChoice::Grayscale => [
                [0xFF, 0xFF, 0xFF, 0xFF],
                [0xAA, 0xAA, 0xAA, 0xFF],
                [0x55, 0x55, 0x55, 0xFF],
                [0x00, 0x00, 0x00, 0xFF],
            ],
            PaletteChoice::OriginalGreen => [
                [0x9B, 0xBC, 0x0F, 0xFF],
                [0x8B, 0xAC, 0x0F, 0xFF],
                [0x30, 0x62, 0x30, 0xFF],
                [0x0F, 0x38, 0x0F, 0xFF],
            ],
            PaletteChoice::Blue => [
                [0xE0, 0xF8, 0xFF, 0xFF],
                [0x86, 0xC0, 0xEA, 0xFF],
                [0x2E, 0x59, 0x8D, 0xFF],
                [0x1A, 0x1C, 0x2C, 0xFF],
            ],
            PaletteChoice::Brown => [
                [0xFF, 0xF6, 0xD3, 0xFF],
                [0xBF, 0x8B, 0x67, 0xFF],
                [0x7F, 0x4F, 0x24, 0xFF],
                [0x33, 0x20, 0x14, 0xFF],
            ],
            PaletteChoice::Red => [
                [0xFF, 0xE4, 0xE1, 0xFF],
                [0xFF, 0xA5, 0x9E, 0xFF],
                [0xBF, 0x30, 0x30, 0xFF],
                [0x7F, 0x0A, 0x0A, 0xFF],
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_kind_is_known() {
        assert!(!COMMANDS.is_empty());
        for c in COMMANDS {
            let _ = c.action_kind;
            let _ = c.category;
        }
    }
}

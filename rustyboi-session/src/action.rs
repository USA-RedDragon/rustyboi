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
    /// An IPS/UPS/BPS ROM patch, applied to the currently-loaded ROM.
    Patch,
    /// A real boot ROM image (DMG or CGB), supplied to the session for the
    /// real-boot-ROM feature.
    BootRom,
    /// A recorded TAS movie (`.rbmovie`), replayed deterministically.
    Movie,
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
    /// CRC32 of the file, computed by the Kotlin scanner (and cached per URI), so
    /// the library can show the canonical No-Intro name via
    /// [`no_intro::name_for_crc`](crate::no_intro::name_for_crc). `0` = unknown.
    pub crc32: u32,
}

/// The monochrome DMG palette (tint) applied to original Game Boy output, on
/// DMG/MGB hardware. Toolkit- and platform-agnostic; the adapter maps it to
/// concrete RGBA shades. These are the four hardware-grounded renderings:
/// neutral grayscale, the classic DMG green (raw and LCD-corrected), and the
/// cooler Game Boy Pocket grayscale — no invented colour sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaletteChoice {
    Grayscale,
    GreenLinear,
    GreenLcd,
    Pocket,
}

/// The CGB colorization applied to a DMG game running on CGB/AGB hardware:
/// `Auto` keeps the boot ROM's title-hash pick; `Scheme(id)` forces one of the
/// boot-ROM button-combo palettes
/// ([`cgb_compat_palette::COMBO_SCHEMES`](rustyboi_core_lib::cgb_compat_palette::COMBO_SCHEMES)).
/// No effect on DMG/MGB hardware (monochrome) or on CGB titles (own colours).
/// The `Scheme` payload is the boot ROM's palette id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GbcDmgPalette {
    #[default]
    Auto,
    Scheme(u8),
}

impl GbcDmgPalette {
    /// Every choice (Auto first, then the 12 boot-ROM schemes) paired with its
    /// menu label — the single list the Settings UI and the libretro option are
    /// both built from.
    pub fn choices() -> Vec<(GbcDmgPalette, &'static str)> {
        let mut v = vec![(GbcDmgPalette::Auto, "Auto (title hash)")];
        for (_, label, pid) in rustyboi_core_lib::cgb_compat_palette::COMBO_SCHEMES {
            v.push((GbcDmgPalette::Scheme(pid), label));
        }
        v
    }

    /// The forced boot-ROM palette id, or `None` for `Auto` (title-hash pick).
    pub fn forced_id(self) -> Option<u8> {
        match self {
            GbcDmgPalette::Auto => None,
            GbcDmgPalette::Scheme(id) => Some(id),
        }
    }

    /// The stable string id for the libretro option (`auto`, or the scheme's id).
    pub fn option_id(self) -> &'static str {
        match self {
            GbcDmgPalette::Auto => "auto",
            GbcDmgPalette::Scheme(id) => {
                rustyboi_core_lib::cgb_compat_palette::COMBO_SCHEMES
                    .iter()
                    .find(|(_, _, pid)| *pid == id)
                    .map(|(s, _, _)| *s)
                    .unwrap_or("auto")
            }
        }
    }

    /// Parse a string id (see [`option_id`](Self::option_id)), or `None`.
    pub fn from_option_id(id: &str) -> Option<Self> {
        if id == "auto" {
            return Some(GbcDmgPalette::Auto);
        }
        rustyboi_core_lib::cgb_compat_palette::COMBO_SCHEMES
            .iter()
            .find(|(s, _, _)| *s == id)
            .map(|(_, _, pid)| GbcDmgPalette::Scheme(*pid))
    }
}

/// The hardware model choices surfaced in the Settings menu — a lossless 1:1
/// mirror of the core [`Hardware`](rustyboi_core_lib::gb::Hardware) so every
/// silicon revision the core emulates is selectable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareChoice {
    Dmg,
    Dmg0,
    Mgb,
    Sgb,
    Sgb2,
    Cgb0,
    Cgbb,
    Cgb,
    Cgbe,
    Agb,
}

/// The texture sampling filter used when the emulator frame is scaled up for
/// display. `Nearest` (default) keeps the crisp pixel grid; `Linear` smooths.
/// Presentation-only — a frontend renderer concern, never touches emulation.
/// Serde-derived so it persists in [`Config`](crate::config::Config).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextureFilter {
    #[default]
    Nearest,
    Linear,
}

/// An optional LCD post-process effect applied by the renderer. `Off` (default)
/// is the plain scaled frame; `Grid` darkens a subpixel gap between source
/// pixels (an LCD-cell look); `Scanlines` darkens between source rows.
/// Presentation-only. Serde-derived so it persists in
/// [`Config`](crate::config::Config).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LcdEffect {
    #[default]
    Off,
    Grid,
    Scanlines,
}

/// The integer upscale factors offered for saved Game Boy Printer output — the
/// single list the Settings menu and the libretro option are built from.
pub const PRINTER_SCALES: [u8; 6] = [1, 2, 3, 4, 5, 8];

/// How the emulated frame is fit into its render region (letterboxing policy).
/// `FitAspect` is the historical behavior (aspect-preserving contain);
/// `IntegerAspect` snaps to the largest whole-number scale; `Stretch` fills the
/// region on both axes, ignoring aspect. Serde-derived so it persists in
/// [`Config`](crate::config::Config).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalingMode {
    #[default]
    FitAspect,
    IntegerAspect,
    Stretch,
}

/// A snapshot of session-owned state the menus render current selections from
/// (checkmarks, radio dots, slot list). The UI never mutates the session
/// directly; it reads this and emits [`UiAction`]s the session applies.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionUiState {
    pub hardware: HardwareChoice,
    pub palette: PaletteChoice,
    /// CGB colorization scheme for DMG games (Auto / a boot-ROM scheme).
    pub gbc_dmg_palette: GbcDmgPalette,
    /// Whether the DMG palette settings apply to the loaded game (false for a
    /// CGB title, which supplies its own colours — the menu is greyed out).
    pub dmg_palette_active: bool,
    /// CGB colour-correction curve (raw RGB555 vs a hardware-LCD approximation).
    pub color_correction: crate::CgbColorConversion,
    /// Whether a real boot ROM is run (when one has been supplied) instead of
    /// the synthetic post-boot state.
    pub use_real_boot_rom: bool,
    /// Texture sampling filter for upscaling the frame (presentation-only).
    pub texture_filter: TextureFilter,
    /// LCD post-process effect (presentation-only).
    pub lcd_effect: LcdEffect,
    /// Integer upscale factor for saved Game Boy Printer output.
    pub printer_scale: u8,
    /// On-screen touch control opacity, 0..=100 (percent).
    pub touch_opacity: u8,
    pub rewind_enabled: bool,
    pub rewind_interval_frames: u32,
    pub rewind_depth: usize,
    /// Master output volume, 0..=100 (scales the session's drained audio copy).
    pub volume: u8,
    /// How the frame is letterboxed in the render region.
    pub scaling: ScalingMode,
    pub sgb_border: bool,
    /// Whether emulation is paused (drives the Pause/Resume menu label). On
    /// desktop the frontend owns pause and passes it separately, so this is only
    /// meaningful for the web adapter, whose pause lives in the session.
    pub paused: bool,
    pub fast_forward: bool,
    /// Whether the on-screen touch overlay is shown.
    pub touch_controls: bool,
    /// Whether a Game Boy Printer is currently attached to the link port (drives
    /// the Connect/Disconnect menu label).
    pub printer_attached: bool,
    /// Whether a TAS movie is currently being recorded (drives the
    /// Record/Stop-Recording menu label).
    pub recording: bool,
    /// Whether a TAS movie is currently playing back (gates the Stop-Replay menu
    /// item; live input is suppressed while true).
    pub replaying: bool,
    /// Slot numbers that currently hold a saved state, ascending.
    pub slots: Vec<u32>,
    /// Active cheat codes, in insertion order.
    pub cheats: Vec<String>,
    /// Cheats fetched from the libretro cheat DB awaiting the user's selection
    /// (empty until a `Get cheats` fetch completes; cleared when dismissed).
    pub fetched_cheats: Vec<crate::cheat_db::FetchedCheat>,
    /// Whether the inserted cartridge has battery-backed SRAM (gates the
    /// Import/Export Battery Save menu items).
    pub has_battery: bool,
    /// Whether the inserted cartridge has a real-time clock (gates the
    /// Import/Export RTC menu items).
    pub has_rtc: bool,
    /// Whether a ROM is currently loaded (gates the Apply Patch menu item).
    pub has_rom: bool,
    /// The loaded game's display name (No-Intro name, else header title), for
    /// the window/tab title and the ROM library. `None` when unidentifiable.
    pub game_name: Option<String>,
    /// The live rebindable input map (GB-button bindings + chord hotkeys) the
    /// keybind editor reads/writes. Mirrors [`Config::input`](crate::config::Config).
    pub input: InputConfig,
}

impl Default for SessionUiState {
    fn default() -> Self {
        SessionUiState {
            hardware: HardwareChoice::Cgb,
            palette: PaletteChoice::Grayscale,
            gbc_dmg_palette: GbcDmgPalette::Auto,
            dmg_palette_active: true,
            color_correction: crate::CgbColorConversion::Linear,
            use_real_boot_rom: false,
            texture_filter: TextureFilter::Nearest,
            lcd_effect: LcdEffect::Off,
            printer_scale: 5,
            touch_opacity: 100,
            rewind_enabled: true,
            rewind_interval_frames: 6,
            rewind_depth: 90,
            volume: 100,
            scaling: ScalingMode::FitAspect,
            sgb_border: true,
            paused: false,
            fast_forward: false,
            touch_controls: cfg!(target_os = "android"),
            printer_attached: false,
            recording: false,
            replaying: false,
            slots: Vec::new(),
            cheats: Vec::new(),
            fetched_cheats: Vec::new(),
            has_battery: false,
            has_rtc: false,
            has_rom: false,
            game_name: None,
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
    /// Apply an IPS/UPS/BPS ROM patch (romhack/translation) to the loaded ROM.
    ApplyPatch(FileData),
    /// Export the current cartridge's RTC state as a `.rtc` file.
    ExportRtc,
    /// Toggle pause / resume.
    TogglePause,
    /// Start recording a TAS movie from the current machine state, or stop the
    /// in-progress recording and hand the finished movie back as a saveable
    /// `.rbmovie` file (File → Export). One toggle drives both.
    ToggleRecording,
    /// Load a recorded TAS movie from a picked file and begin deterministic
    /// playback.
    LoadMovie(FileData),
    /// Stop movie playback, resuming live input.
    StopReplay,
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
    /// Change the CGB colorization for DMG games (Auto / a boot-ROM scheme).
    SetGbcDmgPalette(GbcDmgPalette),
    /// Change the CGB colour-correction curve (Linear/LCD).
    SetColorCorrection(crate::CgbColorConversion),
    /// Enable/disable running a real boot ROM (rebuilds the machine).
    SetRealBootRom(bool),
    /// Change the upscale texture filter (nearest/linear) — presentation-only.
    SetTextureFilter(TextureFilter),
    /// Change the LCD post-process effect — presentation-only.
    SetLcdEffect(LcdEffect),
    /// Change the integer upscale factor for saved Game Boy Printer output.
    SetPrinterScale(u8),
    /// Change the on-screen touch control opacity (0..=100 percent).
    SetTouchOpacity(u8),
    /// Supply real boot ROM bytes from a picked file (routed like a battery/RTC
    /// import through the frontend's file resolver).
    LoadBootRom(FileData),
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
    /// Add several cheat codes at once (the user's selection from the fetched
    /// cheat-DB list). Each is added through the same path as [`AddCheat`].
    AddCheats(Vec<String>),
    /// Remove a previously-added cheat by its raw code string.
    RemoveCheat(String),
    /// Fetch this game's cheats from the libretro cheat DB (identifies the loaded
    /// ROM via No-Intro, emits a [`FetchUrl`](crate::apply::PlatformRequest::FetchUrl)).
    GetCheats,
    /// Discard the fetched-cheat list (the user closed the picker).
    ClearFetchedCheats,
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
            UiAction::ApplyPatch(_) => ActionKind::ApplyPatch,
            UiAction::ExportRtc => ActionKind::ExportRtc,
            UiAction::TogglePause => ActionKind::TogglePause,
            UiAction::ToggleRecording => ActionKind::ToggleRecording,
            UiAction::LoadMovie(_) => ActionKind::LoadMovie,
            UiAction::StopReplay => ActionKind::StopReplay,
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
            UiAction::SetGbcDmgPalette(_) => ActionKind::SetGbcDmgPalette,
            UiAction::SetColorCorrection(_) => ActionKind::SetColorCorrection,
            UiAction::SetRealBootRom(_) => ActionKind::SetRealBootRom,
            UiAction::SetTextureFilter(_) => ActionKind::SetTextureFilter,
            UiAction::SetLcdEffect(_) => ActionKind::SetLcdEffect,
            UiAction::SetPrinterScale(_) => ActionKind::SetPrinterScale,
            UiAction::SetTouchOpacity(_) => ActionKind::SetTouchOpacity,
            UiAction::LoadBootRom(_) => ActionKind::LoadBootRom,
            UiAction::SetRewindEnabled(_) => ActionKind::SetRewindEnabled,
            UiAction::SetRewindInterval(_) => ActionKind::SetRewindInterval,
            UiAction::SetRewindDepth(_) => ActionKind::SetRewindDepth,
            UiAction::SetVolume(_) => ActionKind::SetVolume,
            UiAction::SetScalingMode(_) => ActionKind::SetScalingMode,
            UiAction::ToggleFullscreen => ActionKind::ToggleFullscreen,
            UiAction::SetInputConfig(_) => ActionKind::SetInputConfig,
            UiAction::AddCheat(_) => ActionKind::AddCheat,
            UiAction::AddCheats(_) => ActionKind::AddCheats,
            UiAction::RemoveCheat(_) => ActionKind::RemoveCheat,
            UiAction::GetCheats => ActionKind::GetCheats,
            UiAction::ClearFetchedCheats => ActionKind::ClearFetchedCheats,
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
    ApplyPatch,
    TogglePause,
    ToggleRecording,
    LoadMovie,
    StopReplay,
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
    SetGbcDmgPalette,
    SetColorCorrection,
    SetRealBootRom,
    SetTextureFilter,
    SetLcdEffect,
    SetPrinterScale,
    SetTouchOpacity,
    LoadBootRom,
    SetRewindEnabled,
    SetRewindInterval,
    SetRewindDepth,
    SetVolume,
    SetScalingMode,
    ToggleFullscreen,
    SetInputConfig,
    AddCheat,
    AddCheats,
    RemoveCheat,
    GetCheats,
    ClearFetchedCheats,
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
        action_kind: ActionKind::ApplyPatch,
        label: "Apply Patch…",
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
        action_kind: ActionKind::ToggleRecording,
        label: "Record Movie",
        category: MenuCategory::Emulation,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::LoadMovie,
        label: "Play Movie…",
        category: MenuCategory::Emulation,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::StopReplay,
        label: "Stop Replay",
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
        action_kind: ActionKind::SetGbcDmgPalette,
        label: "GBC Palette (DMG games)",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetColorCorrection,
        label: "GBC Color Correction",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetTextureFilter,
        label: "Texture Filter",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetLcdEffect,
        label: "LCD Effect",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetPrinterScale,
        label: "Printer Scale",
        category: MenuCategory::Settings,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetTouchOpacity,
        label: "On-screen Control Opacity",
        category: MenuCategory::View,
        default_keybind: None,
        overlay_button: None,
    },
    CommandDescriptor {
        action_kind: ActionKind::SetRealBootRom,
        label: "Real Boot ROM",
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

/// A console family, used to group the (10) [`HardwareChoice`] variants into
/// submenus in the Settings UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareFamily {
    GameBoy,
    SuperGameBoy,
    GameBoyColor,
    GameBoyAdvance,
}

impl HardwareFamily {
    /// The family submenu label.
    pub fn label(self) -> &'static str {
        match self {
            HardwareFamily::GameBoy => "Game Boy",
            HardwareFamily::SuperGameBoy => "Super Game Boy",
            HardwareFamily::GameBoyColor => "Game Boy Color",
            HardwareFamily::GameBoyAdvance => "Game Boy Advance",
        }
    }

    /// The choices under this family, in display order.
    pub fn choices(self) -> &'static [HardwareChoice] {
        match self {
            HardwareFamily::GameBoy => {
                &[HardwareChoice::Dmg, HardwareChoice::Dmg0, HardwareChoice::Mgb]
            }
            HardwareFamily::SuperGameBoy => &[HardwareChoice::Sgb, HardwareChoice::Sgb2],
            HardwareFamily::GameBoyColor => &[
                HardwareChoice::Cgb0,
                HardwareChoice::Cgbb,
                HardwareChoice::Cgb,
                HardwareChoice::Cgbe,
            ],
            HardwareFamily::GameBoyAdvance => &[HardwareChoice::Agb],
        }
    }
}

impl HardwareChoice {
    /// Every model, in display order. `from_option_id` searches this, and a test
    /// pins that [`FAMILIES`](Self::FAMILIES) collectively cover it.
    pub const ALL: [HardwareChoice; 10] = [
        HardwareChoice::Dmg,
        HardwareChoice::Dmg0,
        HardwareChoice::Mgb,
        HardwareChoice::Sgb,
        HardwareChoice::Sgb2,
        HardwareChoice::Cgb0,
        HardwareChoice::Cgbb,
        HardwareChoice::Cgb,
        HardwareChoice::Cgbe,
        HardwareChoice::Agb,
    ];

    /// The families, in display order, for building grouped Settings submenus.
    pub const FAMILIES: [HardwareFamily; 4] = [
        HardwareFamily::GameBoy,
        HardwareFamily::SuperGameBoy,
        HardwareFamily::GameBoyColor,
        HardwareFamily::GameBoyAdvance,
    ];

    /// The stable lowercase string id for this model — the canonical mapping the
    /// libretro core option keys are drawn from, so a frontend that speaks string
    /// ids (libretro) never hand-maintains a second model table. Round-trips with
    /// [`from_option_id`](Self::from_option_id).
    pub fn option_id(self) -> &'static str {
        match self {
            HardwareChoice::Dmg => "dmg",
            HardwareChoice::Dmg0 => "dmg0",
            HardwareChoice::Mgb => "mgb",
            HardwareChoice::Sgb => "sgb",
            HardwareChoice::Sgb2 => "sgb2",
            HardwareChoice::Cgb0 => "cgb0",
            HardwareChoice::Cgbb => "cgbb",
            HardwareChoice::Cgb => "cgb",
            HardwareChoice::Cgbe => "cgbe",
            HardwareChoice::Agb => "agb",
        }
    }

    /// Parse a lowercase model id (see [`option_id`](Self::option_id)), or `None`
    /// if unrecognized.
    pub fn from_option_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.option_id() == id)
    }

    /// A short human label for this specific model.
    pub fn label(self) -> &'static str {
        match self {
            HardwareChoice::Dmg => "DMG (Game Boy)",
            HardwareChoice::Dmg0 => "DMG0 (early Japanese)",
            HardwareChoice::Mgb => "MGB (Game Boy Pocket)",
            HardwareChoice::Sgb => "SGB (Super Game Boy)",
            HardwareChoice::Sgb2 => "SGB2 (Super Game Boy 2)",
            HardwareChoice::Cgb0 => "CGB0 (earliest)",
            HardwareChoice::Cgbb => "CGB-B",
            HardwareChoice::Cgb => "CGB (Game Boy Color)",
            HardwareChoice::Cgbe => "CGB-E",
            HardwareChoice::Agb => "AGB (Game Boy Advance)",
        }
    }

    /// Map a core [`Hardware`](rustyboi_core_lib::gb::Hardware) to the menu
    /// choice — a lossless 1:1 mapping.
    pub fn from_hardware(hw: rustyboi_core_lib::gb::Hardware) -> Self {
        use rustyboi_core_lib::gb::Hardware;
        match hw {
            Hardware::DMG => HardwareChoice::Dmg,
            Hardware::DMG0 => HardwareChoice::Dmg0,
            Hardware::MGB => HardwareChoice::Mgb,
            Hardware::SGB => HardwareChoice::Sgb,
            Hardware::SGB2 => HardwareChoice::Sgb2,
            Hardware::CGB0 => HardwareChoice::Cgb0,
            Hardware::CGBB => HardwareChoice::Cgbb,
            Hardware::CGB => HardwareChoice::Cgb,
            Hardware::CGBE => HardwareChoice::Cgbe,
            Hardware::AGB => HardwareChoice::Agb,
        }
    }

    /// The core [`Hardware`](rustyboi_core_lib::gb::Hardware) for this choice.
    pub fn to_hardware(self) -> rustyboi_core_lib::gb::Hardware {
        use rustyboi_core_lib::gb::Hardware;
        match self {
            HardwareChoice::Dmg => Hardware::DMG,
            HardwareChoice::Dmg0 => Hardware::DMG0,
            HardwareChoice::Mgb => Hardware::MGB,
            HardwareChoice::Sgb => Hardware::SGB,
            HardwareChoice::Sgb2 => Hardware::SGB2,
            HardwareChoice::Cgb0 => Hardware::CGB0,
            HardwareChoice::Cgbb => Hardware::CGBB,
            HardwareChoice::Cgb => Hardware::CGB,
            HardwareChoice::Cgbe => Hardware::CGBE,
            HardwareChoice::Agb => Hardware::AGB,
        }
    }
}

impl PaletteChoice {
    /// All choices, in display order (also the `from_shades` search order).
    pub const ALL: [PaletteChoice; 4] = [
        PaletteChoice::Grayscale,
        PaletteChoice::GreenLinear,
        PaletteChoice::GreenLcd,
        PaletteChoice::Pocket,
    ];

    /// A short human label for the Settings menu.
    pub fn label(self) -> &'static str {
        match self {
            PaletteChoice::Grayscale => "Grayscale",
            PaletteChoice::GreenLinear => "Green (linear)",
            PaletteChoice::GreenLcd => "Green (LCD)",
            PaletteChoice::Pocket => "Game Boy Pocket",
        }
    }

    /// The stable lowercase string id for this palette — the canonical mapping
    /// the libretro core option keys are drawn from, so a frontend that speaks
    /// string ids never hand-maintains a second (drift-prone) palette table.
    /// Round-trips with [`from_option_id`](Self::from_option_id).
    pub fn option_id(self) -> &'static str {
        match self {
            PaletteChoice::Grayscale => "grayscale",
            PaletteChoice::GreenLinear => "green",
            PaletteChoice::GreenLcd => "greenlcd",
            PaletteChoice::Pocket => "pocket",
        }
    }

    /// Parse a lowercase palette id (see [`option_id`](Self::option_id)), or
    /// `None` if unrecognized.
    pub fn from_option_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.option_id() == id)
    }

    /// Parse a palette name (CLI `--palette` / config string), or `None` if
    /// unrecognized. Accepts the historical aliases the desktop CLI used.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "grayscale" | "gray" | "grey" => Some(Self::Grayscale),
            "green" | "greenlinear" | "original" | "gameboy" => Some(Self::GreenLinear),
            "greenlcd" | "lcd" | "dmg" => Some(Self::GreenLcd),
            "pocket" | "mgb" => Some(Self::Pocket),
            _ => None,
        }
    }

    /// The choice whose [`rgba_shades`](Self::rgba_shades) match `shades`, or
    /// `Grayscale` if none do (e.g. a custom palette persisted by a frontend).
    pub fn from_shades(shades: [[u8; 4]; 4]) -> Self {
        for choice in Self::ALL {
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
            // The classic DMG green, raw (uncorrected).
            PaletteChoice::GreenLinear => [
                [0x9B, 0xBC, 0x0F, 0xFF],
                [0x8B, 0xAC, 0x0F, 0xFF],
                [0x30, 0x62, 0x30, 0xFF],
                [0x0F, 0x38, 0x0F, 0xFF],
            ],
            // The DMG green as its LCD panel renders it (lighter, gamma-tinted).
            PaletteChoice::GreenLcd => [
                [0xE0, 0xF8, 0xD0, 0xFF],
                [0x88, 0xC0, 0x70, 0xFF],
                [0x34, 0x68, 0x56, 0xFF],
                [0x08, 0x18, 0x20, 0xFF],
            ],
            // The cooler Game Boy Pocket grayscale.
            PaletteChoice::Pocket => [
                [0xC4, 0xCF, 0xA1, 0xFF],
                [0x8B, 0x95, 0x6D, 0xFF],
                [0x4D, 0x53, 0x3C, 0xFF],
                [0x1F, 0x1F, 0x1F, 0xFF],
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

    // The libretro core generates its option table from these ids and parses
    // options back through `from_option_id`, so the two MUST round-trip for every
    // variant — otherwise an option would be un-selectable or silently ignored.
    #[test]
    fn hardware_option_ids_round_trip() {
        for c in HardwareChoice::ALL {
            assert_eq!(HardwareChoice::from_option_id(c.option_id()), Some(c));
        }
        // Ids are unique.
        let mut ids: Vec<&str> = HardwareChoice::ALL.iter().map(|c| c.option_id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), HardwareChoice::ALL.len());
        assert_eq!(HardwareChoice::from_option_id("auto"), None);
    }

    // The family submenus must collectively cover every model exactly once, so
    // the grouped GUI picker can reach all of `ALL`.
    #[test]
    fn families_cover_every_hardware_choice_once() {
        let mut grouped: Vec<HardwareChoice> = HardwareChoice::FAMILIES
            .iter()
            .flat_map(|f| f.choices().iter().copied())
            .collect();
        grouped.sort_by_key(|c| *c as u8);
        let mut all: Vec<HardwareChoice> = HardwareChoice::ALL.to_vec();
        all.sort_by_key(|c| *c as u8);
        assert_eq!(grouped, all);
    }

    #[test]
    fn palette_option_ids_round_trip() {
        for p in PaletteChoice::ALL {
            assert_eq!(PaletteChoice::from_option_id(p.option_id()), Some(p));
            // `rgba_shades` is injective across the set, so `from_shades` also
            // recovers the choice (the frontends rely on this).
            assert_eq!(PaletteChoice::from_shades(p.rgba_shades()), p);
        }
        let mut ids: Vec<&str> = PaletteChoice::ALL.iter().map(|p| p.option_id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PaletteChoice::ALL.len());
    }
}

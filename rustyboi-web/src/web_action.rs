//! Serde transport bridge between the main thread (egui UI) and the worker
//! (`Session`).
//!
//! `rustyboi-session`'s [`UiAction`] and [`SessionUiState`] are deliberately not
//! `serde`-derived (that crate's contract is toolkit-agnostic), so the web
//! adapter defines its own wire types here and converts to/from the session
//! ones. The egui `Gui` on the main thread emits real [`UiAction`]s; we lower the
//! ones the web UI can produce into [`WebAction`], JSON-encode them, post them to
//! the worker, and raise them back into a [`UiAction`] there. The worker builds a
//! [`WebUiState`] from the session each frame and posts it back for the UI to
//! render from.
//!
//! Loads (`LoadRom`/`LoadState`) are NOT lowered here — the main thread resolves
//! the picked file to bytes itself (rfd async picker) and posts the raw bytes to
//! the worker, which feeds them to `finish_load_rom`/`finish_load_state`. Debug
//! actions (breakpoints, stepping) are deferred to Phase B (they need a live
//! `&GB` snapshot layer), so they are dropped rather than lowered.

use serde::{Deserialize, Serialize};

fn default_volume() -> u8 {
    100
}

use rustyboi_session::action::{HardwareChoice, PaletteChoice, ScalingMode};
use rustyboi_session::{InputConfig, SessionUiState, UiAction};

/// Serializable mirror of [`HardwareChoice`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebHardware {
    Dmg,
    Cgb,
    Sgb,
}

impl From<HardwareChoice> for WebHardware {
    fn from(h: HardwareChoice) -> Self {
        match h {
            HardwareChoice::Dmg => WebHardware::Dmg,
            HardwareChoice::Cgb => WebHardware::Cgb,
            HardwareChoice::Sgb => WebHardware::Sgb,
        }
    }
}

impl From<WebHardware> for HardwareChoice {
    fn from(h: WebHardware) -> Self {
        match h {
            WebHardware::Dmg => HardwareChoice::Dmg,
            WebHardware::Cgb => HardwareChoice::Cgb,
            WebHardware::Sgb => HardwareChoice::Sgb,
        }
    }
}

/// Serializable mirror of [`PaletteChoice`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WebPalette {
    Grayscale,
    OriginalGreen,
    Blue,
    Brown,
    Red,
}

impl From<PaletteChoice> for WebPalette {
    fn from(p: PaletteChoice) -> Self {
        match p {
            PaletteChoice::Grayscale => WebPalette::Grayscale,
            PaletteChoice::OriginalGreen => WebPalette::OriginalGreen,
            PaletteChoice::Blue => WebPalette::Blue,
            PaletteChoice::Brown => WebPalette::Brown,
            PaletteChoice::Red => WebPalette::Red,
        }
    }
}

impl From<WebPalette> for PaletteChoice {
    fn from(p: WebPalette) -> Self {
        match p {
            WebPalette::Grayscale => PaletteChoice::Grayscale,
            WebPalette::OriginalGreen => PaletteChoice::OriginalGreen,
            WebPalette::Blue => PaletteChoice::Blue,
            WebPalette::Brown => PaletteChoice::Brown,
            WebPalette::Red => PaletteChoice::Red,
        }
    }
}

/// The subset of [`UiAction`] the web UI can emit and the worker services. Loads
/// and debug actions are excluded (see the module docs). This is the JSON wire
/// format posted `main -> worker`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WebAction {
    TogglePause,
    TogglePrinter,
    Restart,
    ClearError,
    SaveSlot(u32),
    LoadSlot(u32),
    Quicksave,
    Quickload,
    ToggleFastForward,
    FrameAdvance,
    ToggleSgbBorder,
    ToggleTouchControls,
    SetHardware(WebHardware),
    SetPalette(WebPalette),
    SetRewindEnabled(bool),
    SetRewindInterval(u32),
    SetRewindDepth(usize),
    SetVolume(u8),
    SetScalingMode(ScalingMode),
    SetInputConfig(InputConfig),
    AddCheat(String),
    RemoveCheat(String),
}

impl WebAction {
    /// Lower a [`UiAction`] the egui `Gui` emitted into the wire form, if the web
    /// worker can service it. Loads and debug/OS-only actions return `None`
    /// (loads are handled on the main thread; debug is Phase B).
    pub fn from_ui_action(action: &UiAction) -> Option<WebAction> {
        Some(match action {
            UiAction::TogglePause => WebAction::TogglePause,
            UiAction::TogglePrinter => WebAction::TogglePrinter,
            UiAction::Restart => WebAction::Restart,
            UiAction::ClearError => WebAction::ClearError,
            UiAction::SaveSlot(n) => WebAction::SaveSlot(*n),
            UiAction::LoadSlot(n) => WebAction::LoadSlot(*n),
            UiAction::Quicksave => WebAction::Quicksave,
            UiAction::Quickload => WebAction::Quickload,
            UiAction::ToggleFastForward => WebAction::ToggleFastForward,
            UiAction::FrameAdvance => WebAction::FrameAdvance,
            UiAction::ToggleSgbBorder => WebAction::ToggleSgbBorder,
            UiAction::ToggleTouchControls => WebAction::ToggleTouchControls,
            UiAction::SetHardware(h) => WebAction::SetHardware((*h).into()),
            UiAction::SetPalette(p) => WebAction::SetPalette((*p).into()),
            UiAction::SetRewindEnabled(b) => WebAction::SetRewindEnabled(*b),
            UiAction::SetRewindInterval(n) => WebAction::SetRewindInterval(*n),
            UiAction::SetRewindDepth(n) => WebAction::SetRewindDepth(*n),
            UiAction::SetVolume(v) => WebAction::SetVolume(*v),
            UiAction::SetScalingMode(m) => WebAction::SetScalingMode(*m),
            UiAction::SetInputConfig(i) => WebAction::SetInputConfig(i.clone()),
            UiAction::AddCheat(c) => WebAction::AddCheat(c.clone()),
            UiAction::RemoveCheat(c) => WebAction::RemoveCheat(c.clone()),
            _ => return None,
        })
    }

    /// Raise the wire form back into a [`UiAction`] the worker applies via the
    /// shared `Session::apply` contract.
    pub fn into_ui_action(self) -> UiAction {
        match self {
            WebAction::TogglePause => UiAction::TogglePause,
            WebAction::TogglePrinter => UiAction::TogglePrinter,
            WebAction::Restart => UiAction::Restart,
            WebAction::ClearError => UiAction::ClearError,
            WebAction::SaveSlot(n) => UiAction::SaveSlot(n),
            WebAction::LoadSlot(n) => UiAction::LoadSlot(n),
            WebAction::Quicksave => UiAction::Quicksave,
            WebAction::Quickload => UiAction::Quickload,
            WebAction::ToggleFastForward => UiAction::ToggleFastForward,
            WebAction::FrameAdvance => UiAction::FrameAdvance,
            WebAction::ToggleSgbBorder => UiAction::ToggleSgbBorder,
            WebAction::ToggleTouchControls => UiAction::ToggleTouchControls,
            WebAction::SetHardware(h) => UiAction::SetHardware(h.into()),
            WebAction::SetPalette(p) => UiAction::SetPalette(p.into()),
            WebAction::SetRewindEnabled(b) => UiAction::SetRewindEnabled(b),
            WebAction::SetRewindInterval(n) => UiAction::SetRewindInterval(n),
            WebAction::SetRewindDepth(n) => UiAction::SetRewindDepth(n),
            WebAction::SetVolume(v) => UiAction::SetVolume(v),
            WebAction::SetScalingMode(m) => UiAction::SetScalingMode(m),
            WebAction::SetInputConfig(i) => UiAction::SetInputConfig(i),
            WebAction::AddCheat(c) => UiAction::AddCheat(c),
            WebAction::RemoveCheat(c) => UiAction::RemoveCheat(c),
        }
    }
}

/// Serializable mirror of [`SessionUiState`], the snapshot the worker posts to
/// the main thread each time it changes. The UI's egui `Gui` reads from a
/// reconstructed `SessionUiState` (via [`WebUiState::into_session`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebUiState {
    pub hardware: WebHardware,
    pub palette: WebPalette,
    pub rewind_enabled: bool,
    pub rewind_interval_frames: u32,
    pub rewind_depth: usize,
    #[serde(default = "default_volume")]
    pub volume: u8,
    #[serde(default)]
    pub scaling: ScalingMode,
    pub sgb_border: bool,
    pub fast_forward: bool,
    pub touch_controls: bool,
    pub slots: Vec<u32>,
    pub cheats: Vec<String>,
    #[serde(default)]
    pub has_battery: bool,
    #[serde(default)]
    pub has_rtc: bool,
    #[serde(default)]
    pub has_rom: bool,
    #[serde(default)]
    pub game_name: Option<String>,
    #[serde(default)]
    pub input: InputConfig,
}

impl WebUiState {
    /// Build the wire snapshot from the session's [`SessionUiState`].
    pub fn from_session(s: &SessionUiState) -> WebUiState {
        WebUiState {
            hardware: s.hardware.into(),
            palette: s.palette.into(),
            rewind_enabled: s.rewind_enabled,
            rewind_interval_frames: s.rewind_interval_frames,
            rewind_depth: s.rewind_depth,
            volume: s.volume,
            scaling: s.scaling,
            sgb_border: s.sgb_border,
            fast_forward: s.fast_forward,
            touch_controls: s.touch_controls,
            slots: s.slots.clone(),
            cheats: s.cheats.clone(),
            has_battery: s.has_battery,
            has_rtc: s.has_rtc,
            has_rom: s.has_rom,
            game_name: s.game_name.clone(),
            input: s.input.clone(),
        }
    }

    /// Reconstruct a [`SessionUiState`] for the egui `Gui` to render from.
    pub fn into_session(self) -> SessionUiState {
        SessionUiState {
            hardware: self.hardware.into(),
            palette: self.palette.into(),
            rewind_enabled: self.rewind_enabled,
            rewind_interval_frames: self.rewind_interval_frames,
            rewind_depth: self.rewind_depth,
            volume: self.volume,
            scaling: self.scaling,
            sgb_border: self.sgb_border,
            fast_forward: self.fast_forward,
            touch_controls: self.touch_controls,
            slots: self.slots,
            cheats: self.cheats,
            has_battery: self.has_battery,
            has_rtc: self.has_rtc,
            has_rom: self.has_rom,
            game_name: self.game_name,
            input: self.input,
        }
    }
}

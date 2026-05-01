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

fn default_true() -> bool {
    true
}

fn default_printer_scale() -> u8 {
    5
}

fn default_touch_opacity() -> u8 {
    100
}

// The choice enums (`HardwareChoice`, `PaletteChoice`) and the value enums
// (`ScalingMode`, `TextureFilter`, `LcdEffect`, `CgbColorConversion`) are all
// serde-derived in the shared crate, so the web wire uses them directly — no
// per-frontend mirror types.
use rustyboi_session::action::{
    GbcDmgPalette, HardwareChoice, LcdEffect, PaletteChoice, ScalingMode, TextureFilter,
};
use rustyboi_session::{CgbColorConversion, FetchedCheat, InputConfig, SessionUiState, UiAction};

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
    SetHardware(HardwareChoice),
    SetPalette(PaletteChoice),
    SetGbcDmgPalette(GbcDmgPalette),
    SetColorCorrection(CgbColorConversion),
    SetRealBootRom(bool),
    SetTextureFilter(TextureFilter),
    SetLcdEffect(LcdEffect),
    SetPrinterScale(u8),
    SetTouchOpacity(u8),
    SetRewindEnabled(bool),
    SetRewindInterval(u32),
    SetRewindDepth(usize),
    SetVolume(u8),
    SetScalingMode(ScalingMode),
    SetInputConfig(InputConfig),
    AddCheat(String),
    AddCheats(Vec<String>),
    RemoveCheat(String),
    GetCheats,
    ClearFetchedCheats,
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
            UiAction::SetHardware(h) => WebAction::SetHardware(*h),
            UiAction::SetPalette(p) => WebAction::SetPalette(*p),
            UiAction::SetGbcDmgPalette(g) => WebAction::SetGbcDmgPalette(*g),
            UiAction::SetColorCorrection(c) => WebAction::SetColorCorrection(*c),
            UiAction::SetRealBootRom(b) => WebAction::SetRealBootRom(*b),
            UiAction::SetTextureFilter(f) => WebAction::SetTextureFilter(*f),
            UiAction::SetLcdEffect(e) => WebAction::SetLcdEffect(*e),
            UiAction::SetPrinterScale(s) => WebAction::SetPrinterScale(*s),
            UiAction::SetTouchOpacity(o) => WebAction::SetTouchOpacity(*o),
            UiAction::SetRewindEnabled(b) => WebAction::SetRewindEnabled(*b),
            UiAction::SetRewindInterval(n) => WebAction::SetRewindInterval(*n),
            UiAction::SetRewindDepth(n) => WebAction::SetRewindDepth(*n),
            UiAction::SetVolume(v) => WebAction::SetVolume(*v),
            UiAction::SetScalingMode(m) => WebAction::SetScalingMode(*m),
            UiAction::SetInputConfig(i) => WebAction::SetInputConfig(i.clone()),
            UiAction::AddCheat(c) => WebAction::AddCheat(c.clone()),
            UiAction::AddCheats(c) => WebAction::AddCheats(c.clone()),
            UiAction::RemoveCheat(c) => WebAction::RemoveCheat(c.clone()),
            UiAction::GetCheats => WebAction::GetCheats,
            UiAction::ClearFetchedCheats => WebAction::ClearFetchedCheats,
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
            WebAction::SetHardware(h) => UiAction::SetHardware(h),
            WebAction::SetPalette(p) => UiAction::SetPalette(p),
            WebAction::SetGbcDmgPalette(g) => UiAction::SetGbcDmgPalette(g),
            WebAction::SetColorCorrection(c) => UiAction::SetColorCorrection(c),
            WebAction::SetRealBootRom(b) => UiAction::SetRealBootRom(b),
            WebAction::SetTextureFilter(f) => UiAction::SetTextureFilter(f),
            WebAction::SetLcdEffect(e) => UiAction::SetLcdEffect(e),
            WebAction::SetPrinterScale(s) => UiAction::SetPrinterScale(s),
            WebAction::SetTouchOpacity(o) => UiAction::SetTouchOpacity(o),
            WebAction::SetRewindEnabled(b) => UiAction::SetRewindEnabled(b),
            WebAction::SetRewindInterval(n) => UiAction::SetRewindInterval(n),
            WebAction::SetRewindDepth(n) => UiAction::SetRewindDepth(n),
            WebAction::SetVolume(v) => UiAction::SetVolume(v),
            WebAction::SetScalingMode(m) => UiAction::SetScalingMode(m),
            WebAction::SetInputConfig(i) => UiAction::SetInputConfig(i),
            WebAction::AddCheat(c) => UiAction::AddCheat(c),
            WebAction::AddCheats(c) => UiAction::AddCheats(c),
            WebAction::RemoveCheat(c) => UiAction::RemoveCheat(c),
            WebAction::GetCheats => UiAction::GetCheats,
            WebAction::ClearFetchedCheats => UiAction::ClearFetchedCheats,
        }
    }
}

/// Serializable mirror of [`SessionUiState`], the snapshot the worker posts to
/// the main thread each time it changes. The UI's egui `Gui` reads from a
/// reconstructed `SessionUiState` (via [`WebUiState::into_session`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WebUiState {
    pub hardware: HardwareChoice,
    pub palette: PaletteChoice,
    #[serde(default)]
    pub gbc_dmg_palette: GbcDmgPalette,
    #[serde(default = "default_true")]
    pub dmg_palette_active: bool,
    #[serde(default)]
    pub color_correction: CgbColorConversion,
    #[serde(default)]
    pub use_real_boot_rom: bool,
    #[serde(default)]
    pub texture_filter: TextureFilter,
    #[serde(default)]
    pub lcd_effect: LcdEffect,
    #[serde(default = "default_printer_scale")]
    pub printer_scale: u8,
    #[serde(default = "default_touch_opacity")]
    pub touch_opacity: u8,
    pub rewind_enabled: bool,
    pub rewind_interval_frames: u32,
    pub rewind_depth: usize,
    #[serde(default = "default_volume")]
    pub volume: u8,
    #[serde(default)]
    pub scaling: ScalingMode,
    pub sgb_border: bool,
    #[serde(default)]
    pub paused: bool,
    pub fast_forward: bool,
    pub touch_controls: bool,
    #[serde(default)]
    pub printer_attached: bool,
    pub slots: Vec<u32>,
    pub cheats: Vec<String>,
    #[serde(default)]
    pub fetched_cheats: Vec<FetchedCheat>,
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
            hardware: s.hardware,
            palette: s.palette,
            gbc_dmg_palette: s.gbc_dmg_palette,
            dmg_palette_active: s.dmg_palette_active,
            color_correction: s.color_correction,
            use_real_boot_rom: s.use_real_boot_rom,
            texture_filter: s.texture_filter,
            lcd_effect: s.lcd_effect,
            printer_scale: s.printer_scale,
            touch_opacity: s.touch_opacity,
            rewind_enabled: s.rewind_enabled,
            rewind_interval_frames: s.rewind_interval_frames,
            rewind_depth: s.rewind_depth,
            volume: s.volume,
            scaling: s.scaling,
            sgb_border: s.sgb_border,
            paused: s.paused,
            fast_forward: s.fast_forward,
            touch_controls: s.touch_controls,
            printer_attached: s.printer_attached,
            slots: s.slots.clone(),
            cheats: s.cheats.clone(),
            fetched_cheats: s.fetched_cheats.clone(),
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
            hardware: self.hardware,
            palette: self.palette,
            gbc_dmg_palette: self.gbc_dmg_palette,
            dmg_palette_active: self.dmg_palette_active,
            color_correction: self.color_correction,
            use_real_boot_rom: self.use_real_boot_rom,
            texture_filter: self.texture_filter,
            lcd_effect: self.lcd_effect,
            printer_scale: self.printer_scale,
            touch_opacity: self.touch_opacity,
            rewind_enabled: self.rewind_enabled,
            rewind_interval_frames: self.rewind_interval_frames,
            rewind_depth: self.rewind_depth,
            volume: self.volume,
            scaling: self.scaling,
            sgb_border: self.sgb_border,
            paused: self.paused,
            fast_forward: self.fast_forward,
            touch_controls: self.touch_controls,
            printer_attached: self.printer_attached,
            slots: self.slots,
            cheats: self.cheats,
            fetched_cheats: self.fetched_cheats,
            has_battery: self.has_battery,
            has_rtc: self.has_rtc,
            has_rom: self.has_rom,
            game_name: self.game_name,
            input: self.input,
        }
    }
}

//! Rebindable input map + chord-hotkey resolver, shared by every frontend.
//!
//! This is the host-agnostic, WASM-clean core of input handling: a serializable
//! [`InputConfig`] (GB-button bindings + hotkey chords) and a pure [`resolve`]
//! that turns the set of raw inputs held this frame into a core
//! [`ButtonState`] plus the list of hotkeys firing. Each frontend classifies its
//! native events into the abstract [`KeyName`] / [`PadButton`] vocabulary, builds
//! a [`HeldInputs`], and calls [`InputConfig::resolve`]; the returned button state
//! drives the machine and the returned [`FiredHotkey`]s drive features
//! (fast-forward, rewind, quicksave, …).
//!
//! [`GbButton`] and [`ButtonState`] are the session/core types (reused, not
//! redefined) so bindings and chords speak the exact vocabulary the emulator does.

use std::collections::HashSet;

use crate::input::GbButton;
use rustyboi_core_lib::input::ButtonState;
use serde::{Deserialize, Serialize};

/// Editor label for a GB button (the core [`GbButton`] carries no display name).
pub fn gb_label(b: GbButton) -> &'static str {
    match b {
        GbButton::A => "A",
        GbButton::B => "B",
        GbButton::Start => "Start",
        GbButton::Select => "Select",
        GbButton::Up => "Up",
        GbButton::Down => "Down",
        GbButton::Left => "Left",
        GbButton::Right => "Right",
    }
}

/// Host-agnostic keyboard key vocabulary. Frontends map their native key type
/// to/from these names so bindings serialize identically across platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyName {
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9,
    Up, Down, Left, Right,
    Enter, Space, Tab, Backspace, Escape, Backslash,
    ShiftLeft, ShiftRight,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
}

impl KeyName {
    /// Every key, in a stable order (editor dropdowns iterate this).
    pub const ALL: [KeyName; 60] = [
        KeyName::A, KeyName::B, KeyName::C, KeyName::D, KeyName::E, KeyName::F,
        KeyName::G, KeyName::H, KeyName::I, KeyName::J, KeyName::K, KeyName::L,
        KeyName::M, KeyName::N, KeyName::O, KeyName::P, KeyName::Q, KeyName::R,
        KeyName::S, KeyName::T, KeyName::U, KeyName::V, KeyName::W, KeyName::X,
        KeyName::Y, KeyName::Z,
        KeyName::Num0, KeyName::Num1, KeyName::Num2, KeyName::Num3, KeyName::Num4,
        KeyName::Num5, KeyName::Num6, KeyName::Num7, KeyName::Num8, KeyName::Num9,
        KeyName::Up, KeyName::Down, KeyName::Left, KeyName::Right,
        KeyName::Enter, KeyName::Space, KeyName::Tab, KeyName::Backspace,
        KeyName::Escape, KeyName::Backslash, KeyName::ShiftLeft, KeyName::ShiftRight,
        KeyName::F1, KeyName::F2, KeyName::F3, KeyName::F4, KeyName::F5, KeyName::F6,
        KeyName::F7, KeyName::F8, KeyName::F9, KeyName::F10, KeyName::F11, KeyName::F12,
    ];

    /// Keys offered in the editor's key-capture dropdown / labels.
    pub fn label(self) -> &'static str {
        match self {
            KeyName::A => "A", KeyName::B => "B", KeyName::C => "C", KeyName::D => "D",
            KeyName::E => "E", KeyName::F => "F", KeyName::G => "G", KeyName::H => "H",
            KeyName::I => "I", KeyName::J => "J", KeyName::K => "K", KeyName::L => "L",
            KeyName::M => "M", KeyName::N => "N", KeyName::O => "O", KeyName::P => "P",
            KeyName::Q => "Q", KeyName::R => "R", KeyName::S => "S", KeyName::T => "T",
            KeyName::U => "U", KeyName::V => "V", KeyName::W => "W", KeyName::X => "X",
            KeyName::Y => "Y", KeyName::Z => "Z",
            KeyName::Num0 => "0", KeyName::Num1 => "1", KeyName::Num2 => "2",
            KeyName::Num3 => "3", KeyName::Num4 => "4", KeyName::Num5 => "5",
            KeyName::Num6 => "6", KeyName::Num7 => "7", KeyName::Num8 => "8",
            KeyName::Num9 => "9",
            KeyName::Up => "Up", KeyName::Down => "Down",
            KeyName::Left => "Left", KeyName::Right => "Right",
            KeyName::Enter => "Enter", KeyName::Space => "Space", KeyName::Tab => "Tab",
            KeyName::Backspace => "Backspace", KeyName::Escape => "Escape",
            KeyName::Backslash => "Backslash",
            KeyName::ShiftLeft => "LShift", KeyName::ShiftRight => "RShift",
            KeyName::F1 => "F1", KeyName::F2 => "F2", KeyName::F3 => "F3",
            KeyName::F4 => "F4", KeyName::F5 => "F5", KeyName::F6 => "F6",
            KeyName::F7 => "F7", KeyName::F8 => "F8", KeyName::F9 => "F9",
            KeyName::F10 => "F10", KeyName::F11 => "F11", KeyName::F12 => "F12",
        }
    }
}

/// Host-agnostic gamepad button vocabulary. Frontends map their native pad
/// button (gilrs `Button` on desktop, Gamepad-API index on web) to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PadButton {
    South,
    East,
    West,
    North,
    Start,
    Select,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    // Analog-stick directions: "held" when the stick is pushed past a deadzone.
    // Frontends translate axis values into these so a stick maps like any button
    // (e.g. bound alongside the d-pad so the two are interchangeable).
    LStickUp,
    LStickDown,
    LStickLeft,
    LStickRight,
    RStickUp,
    RStickDown,
    RStickLeft,
    RStickRight,
}

impl PadButton {
    pub const ALL: [PadButton; 22] = [
        PadButton::South,
        PadButton::East,
        PadButton::West,
        PadButton::North,
        PadButton::Start,
        PadButton::Select,
        PadButton::DpadUp,
        PadButton::DpadDown,
        PadButton::DpadLeft,
        PadButton::DpadRight,
        PadButton::LeftShoulder,
        PadButton::RightShoulder,
        PadButton::LeftTrigger,
        PadButton::RightTrigger,
        PadButton::LStickUp,
        PadButton::LStickDown,
        PadButton::LStickLeft,
        PadButton::LStickRight,
        PadButton::RStickUp,
        PadButton::RStickDown,
        PadButton::RStickLeft,
        PadButton::RStickRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PadButton::South => "Pad South (A)",
            PadButton::East => "Pad East (B)",
            PadButton::West => "Pad West (X)",
            PadButton::North => "Pad North (Y)",
            PadButton::Start => "Pad Start",
            PadButton::Select => "Pad Select",
            PadButton::DpadUp => "Pad D-Up",
            PadButton::DpadDown => "Pad D-Down",
            PadButton::DpadLeft => "Pad D-Left",
            PadButton::DpadRight => "Pad D-Right",
            PadButton::LeftShoulder => "Pad L1",
            PadButton::RightShoulder => "Pad R1",
            PadButton::LeftTrigger => "Pad L2",
            PadButton::RightTrigger => "Pad R2",
            PadButton::LStickUp => "L-Stick Up",
            PadButton::LStickDown => "L-Stick Down",
            PadButton::LStickLeft => "L-Stick Left",
            PadButton::LStickRight => "L-Stick Right",
            PadButton::RStickUp => "R-Stick Up",
            PadButton::RStickDown => "R-Stick Down",
            PadButton::RStickLeft => "R-Stick Left",
            PadButton::RStickRight => "R-Stick Right",
        }
    }
}

/// A single input source. GB buttons may participate in chords (e.g. Start+A).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputTrigger {
    Key(KeyName),
    Pad(PadButton),
    Gb(GbButton),
}

impl InputTrigger {
    pub fn label(self) -> String {
        match self {
            InputTrigger::Key(k) => format!("Key {}", k.label()),
            InputTrigger::Pad(p) => p.label().to_string(),
            InputTrigger::Gb(b) => format!("GB {}", gb_label(b)),
        }
    }
}

/// The action a hotkey chord triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotkeyAction {
    FastForward,
    Rewind,
    Quicksave,
    Quickload,
    FrameAdvance,
    ToggleFullscreen,
    TogglePause,
    Exit,
    Turbo(GbButton),
}

impl HotkeyAction {
    /// Actions whose effect is a level ("held while active"); the rest are
    /// edge-triggered toggles that fire once when the chord becomes active.
    pub fn is_hold(self) -> bool {
        matches!(
            self,
            HotkeyAction::FastForward | HotkeyAction::Rewind | HotkeyAction::Turbo(_)
        )
    }

    /// The GB button this action consumes (suppressed from normal output while
    /// the chord is active), if any. Turbo replaces the button it drives.
    pub fn consumed_gb(self) -> Option<GbButton> {
        match self {
            HotkeyAction::Turbo(b) => Some(b),
            _ => None,
        }
    }

    pub fn label(self) -> String {
        match self {
            HotkeyAction::FastForward => "Fast-forward".to_string(),
            HotkeyAction::Rewind => "Rewind".to_string(),
            HotkeyAction::Quicksave => "Quicksave".to_string(),
            HotkeyAction::Quickload => "Quickload".to_string(),
            HotkeyAction::FrameAdvance => "Frame-advance".to_string(),
            HotkeyAction::ToggleFullscreen => "Toggle fullscreen".to_string(),
            HotkeyAction::TogglePause => "Toggle pause".to_string(),
            HotkeyAction::Exit => "Exit".to_string(),
            HotkeyAction::Turbo(b) => format!("Turbo {}", gb_label(b)),
        }
    }

    /// Non-Turbo actions, for the editor action dropdown.
    pub const SIMPLE: [HotkeyAction; 8] = [
        HotkeyAction::FastForward,
        HotkeyAction::Rewind,
        HotkeyAction::Quicksave,
        HotkeyAction::Quickload,
        HotkeyAction::FrameAdvance,
        HotkeyAction::ToggleFullscreen,
        HotkeyAction::TogglePause,
        HotkeyAction::Exit,
    ];
}

/// A chord (all triggers held simultaneously) mapped to an action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hotkey {
    pub chord: Vec<InputTrigger>,
    pub action: HotkeyAction,
}

/// The full, serializable, host-agnostic input map shared by all frontends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputConfig {
    /// Each GB button maps to one or more triggers; pressed if any is held.
    #[serde(default = "default_gb_bindings")]
    pub gb_bindings: Vec<(GbButton, Vec<InputTrigger>)>,
    #[serde(default = "default_hotkeys")]
    pub hotkeys: Vec<Hotkey>,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            gb_bindings: default_gb_bindings(),
            hotkeys: default_hotkeys(),
        }
    }
}

fn default_gb_bindings() -> Vec<(GbButton, Vec<InputTrigger>)> {
    use InputTrigger::{Key, Pad};
    use KeyName::*;
    // Each GB button binds a key OR the matching gamepad button, so a keyboard or
    // a controller works out of the box (desktop gilrs, web Gamepad API, Android).
    vec![
        (GbButton::A, vec![Key(Z), Pad(PadButton::South)]),
        (GbButton::B, vec![Key(X), Pad(PadButton::East)]),
        (GbButton::Start, vec![Key(Enter), Pad(PadButton::Start)]),
        (GbButton::Select, vec![Key(ShiftLeft), Pad(PadButton::Select)]),
        (GbButton::Up, vec![Key(KeyName::Up), Pad(PadButton::DpadUp), Pad(PadButton::LStickUp)]),
        (GbButton::Down, vec![Key(KeyName::Down), Pad(PadButton::DpadDown), Pad(PadButton::LStickDown)]),
        (GbButton::Left, vec![Key(KeyName::Left), Pad(PadButton::DpadLeft), Pad(PadButton::LStickLeft)]),
        (GbButton::Right, vec![Key(KeyName::Right), Pad(PadButton::DpadRight), Pad(PadButton::LStickRight)]),
    ]
}

fn default_hotkeys() -> Vec<Hotkey> {
    use HotkeyAction::*;
    use InputTrigger::{Gb, Key, Pad};
    vec![
        Hotkey { chord: vec![Key(KeyName::Tab)], action: FastForward },
        Hotkey { chord: vec![Key(KeyName::Backspace)], action: Rewind },
        Hotkey { chord: vec![Key(KeyName::F5)], action: Quicksave },
        Hotkey { chord: vec![Key(KeyName::F8)], action: Quickload },
        Hotkey { chord: vec![Key(KeyName::Backslash)], action: FrameAdvance },
        // Acceptance examples (chords of mixed trigger kinds):
        Hotkey {
            chord: vec![Gb(GbButton::Start), Gb(GbButton::Select)],
            action: Exit,
        },
        Hotkey {
            chord: vec![Gb(GbButton::Start), Pad(PadButton::RightTrigger)],
            action: FastForward,
        },
        Hotkey {
            chord: vec![Gb(GbButton::Start), Gb(GbButton::A)],
            action: Turbo(GbButton::A),
        },
    ]
}

/// The set of raw inputs currently held this frame. GB buttons are derived
/// from `gb_bindings` during resolution, not supplied here.
#[derive(Debug, Clone, Default)]
pub struct HeldInputs {
    pub keys: HashSet<KeyName>,
    pub pad: HashSet<PadButton>,
}

impl HeldInputs {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Per-frame edge/phase state carried across resolutions by the platform.
#[derive(Debug, Clone, Default)]
pub struct ResolveState {
    /// Whether each hotkey (by index) was active on the previous frame.
    prev_active: Vec<bool>,
    /// Free-running frame counter driving the turbo autofire square wave.
    turbo_phase: u64,
}

impl ResolveState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Autofire flips the driven button every this many frames.
pub const TURBO_PERIOD: u64 = 2;

/// A hotkey that fired this frame (edge for toggles, level for holds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FiredHotkey {
    pub action: HotkeyAction,
    /// True on the rising edge (chord became active this frame).
    pub rising: bool,
}

impl InputConfig {
    fn trigger_held(trigger: InputTrigger, held: &HeldInputs, gb: &ButtonState) -> bool {
        match trigger {
            InputTrigger::Key(k) => held.keys.contains(&k),
            InputTrigger::Pad(p) => held.pad.contains(&p),
            InputTrigger::Gb(b) => gb_button(gb, b),
        }
    }

    /// Resolve the current raw inputs into a Game Boy button state plus the
    /// list of hotkeys firing this frame.
    ///
    /// Contract:
    /// - A GB button is pressed if ANY of its bound triggers is held.
    /// - A hotkey is active iff ALL of its chord triggers are held (empty chord
    ///   never fires). GB-button triggers are evaluated against the raw
    ///   (pre-suppression) GB state so chords like Start+A see A.
    /// - Toggle actions fire once on the rising edge; hold actions fire every
    ///   frame they are active. `FiredHotkey.rising` distinguishes them.
    /// - Turbo(btn): while active, `btn` is driven as an autofire square wave
    ///   (on for TURBO_PERIOD frames, off for TURBO_PERIOD) and its normal
    ///   binding is suppressed. Any GB button consumed by an active action is
    ///   removed from normal output.
    pub fn resolve(
        &self,
        held: &HeldInputs,
        state: &mut ResolveState,
    ) -> (ButtonState, Vec<FiredHotkey>) {
        // Resolve GB buttons from their direct key/pad triggers. A Gb-typed
        // trigger inside a gb_binding is unusual; it sees only the empty
        // baseline here (chords, not bindings, are the place to reference GB
        // buttons), keeping resolution single-pass and order-independent.
        let empty = ButtonState::default();
        let mut raw = ButtonState::default();
        for (button, triggers) in &self.gb_bindings {
            let pressed = triggers.iter().any(|t| Self::trigger_held(*t, held, &empty));
            set_gb_button(&mut raw, *button, pressed);
        }

        if state.prev_active.len() != self.hotkeys.len() {
            state.prev_active = vec![false; self.hotkeys.len()];
        }
        state.turbo_phase = state.turbo_phase.wrapping_add(1);
        let turbo_on = (state.turbo_phase / TURBO_PERIOD) % 2 == 0;

        let mut fired = Vec::new();
        let mut out = raw;
        for (i, hotkey) in self.hotkeys.iter().enumerate() {
            let active = !hotkey.chord.is_empty()
                && hotkey
                    .chord
                    .iter()
                    .all(|t| Self::trigger_held(*t, held, &raw));
            let was = state.prev_active[i];
            state.prev_active[i] = active;

            if !active {
                continue;
            }

            if let Some(consumed) = hotkey.action.consumed_gb() {
                set_gb_button(&mut out, consumed, false);
            }
            if let HotkeyAction::Turbo(btn) = hotkey.action {
                if turbo_on {
                    set_gb_button(&mut out, btn, true);
                }
            }

            if hotkey.action.is_hold() || !was {
                fired.push(FiredHotkey {
                    action: hotkey.action,
                    rising: !was,
                });
            }
        }

        (out, fired)
    }
}

fn gb_button(s: &ButtonState, b: GbButton) -> bool {
    match b {
        GbButton::A => s.a,
        GbButton::B => s.b,
        GbButton::Start => s.start,
        GbButton::Select => s.select,
        GbButton::Up => s.up,
        GbButton::Down => s.down,
        GbButton::Left => s.left,
        GbButton::Right => s.right,
    }
}

fn set_gb_button(s: &mut ButtonState, b: GbButton, v: bool) {
    match b {
        GbButton::A => s.a = v,
        GbButton::B => s.b = v,
        GbButton::Start => s.start = v,
        GbButton::Select => s.select = v,
        GbButton::Up => s.up = v,
        GbButton::Down => s.down = v,
        GbButton::Left => s.left = v,
        GbButton::Right => s.right = v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(ks: &[KeyName]) -> HeldInputs {
        HeldInputs {
            keys: ks.iter().copied().collect(),
            pad: HashSet::new(),
        }
    }

    #[test]
    fn single_key_gb_binding_resolves() {
        let cfg = InputConfig::default();
        let mut st = ResolveState::new();
        let (state, _) = cfg.resolve(&keys(&[KeyName::Z]), &mut st);
        assert!(state.a, "Z should press A");
        assert!(!state.b);
    }

    #[test]
    fn two_trigger_chord_fires_only_when_both_held() {
        let cfg = InputConfig {
            gb_bindings: default_gb_bindings(),
            hotkeys: vec![Hotkey {
                chord: vec![
                    InputTrigger::Gb(GbButton::Start),
                    InputTrigger::Gb(GbButton::Select),
                ],
                action: HotkeyAction::Exit,
            }],
        };
        let mut st = ResolveState::new();

        // Only Start (Enter) held -> not active.
        let (_, fired) = cfg.resolve(&keys(&[KeyName::Enter]), &mut st);
        assert!(fired.is_empty());

        // Both Start (Enter) + Select (LShift) -> fires.
        let (_, fired) = cfg.resolve(&keys(&[KeyName::Enter, KeyName::ShiftLeft]), &mut st);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].action, HotkeyAction::Exit);
        assert!(fired[0].rising);
    }

    #[test]
    fn toggle_fires_once_on_rising_edge() {
        let cfg = InputConfig {
            gb_bindings: default_gb_bindings(),
            hotkeys: vec![Hotkey {
                chord: vec![InputTrigger::Key(KeyName::P)],
                action: HotkeyAction::TogglePause,
            }],
        };
        let mut st = ResolveState::new();

        let (_, f1) = cfg.resolve(&keys(&[KeyName::P]), &mut st);
        assert_eq!(f1.len(), 1, "fires on first frame held");
        let (_, f2) = cfg.resolve(&keys(&[KeyName::P]), &mut st);
        assert!(f2.is_empty(), "does not re-fire while held");
        let (_, f3) = cfg.resolve(&keys(&[]), &mut st);
        assert!(f3.is_empty());
        let (_, f4) = cfg.resolve(&keys(&[KeyName::P]), &mut st);
        assert_eq!(f4.len(), 1, "fires again on next rising edge");
    }

    #[test]
    fn turbo_produces_alternating_button_state() {
        // Start + A -> Turbo A. Normal A is suppressed; A autofires.
        let cfg = InputConfig {
            gb_bindings: default_gb_bindings(),
            hotkeys: vec![Hotkey {
                chord: vec![
                    InputTrigger::Gb(GbButton::Start),
                    InputTrigger::Gb(GbButton::A),
                ],
                action: HotkeyAction::Turbo(GbButton::A),
            }],
        };
        let mut st = ResolveState::new();

        // Hold Enter (Start) + Z (A) across several frames; A must alternate.
        let held = keys(&[KeyName::Enter, KeyName::Z]);
        let mut pattern = Vec::new();
        for _ in 0..(TURBO_PERIOD * 4) {
            let (state, fired) = cfg.resolve(&held, &mut st);
            assert!(state.start, "Start stays pressed alongside turbo");
            assert_eq!(fired.len(), 1);
            assert_eq!(fired[0].action, HotkeyAction::Turbo(GbButton::A));
            pattern.push(state.a);
        }
        assert!(pattern.iter().any(|&v| v), "A on at some point");
        assert!(pattern.iter().any(|&v| !v), "A off at some point");
    }

    #[test]
    fn normal_a_press_works_without_chord() {
        let cfg = InputConfig::default();
        let mut st = ResolveState::new();
        let (state, fired) = cfg.resolve(&keys(&[KeyName::Z]), &mut st);
        assert!(state.a);
        assert!(fired.is_empty());
    }

    #[test]
    fn chord_suppresses_only_consumed_gb_button() {
        // Start+A -> Turbo A. A is consumed (replaced by autofire); Start is not.
        let cfg = InputConfig {
            gb_bindings: default_gb_bindings(),
            hotkeys: vec![Hotkey {
                chord: vec![
                    InputTrigger::Gb(GbButton::Start),
                    InputTrigger::Gb(GbButton::A),
                ],
                action: HotkeyAction::Turbo(GbButton::A),
            }],
        };
        let mut st = ResolveState::new();
        // Across a full turbo cycle, A is sometimes off despite Z held; Start on.
        let held = keys(&[KeyName::Enter, KeyName::Z]);
        let mut saw_a_off = false;
        for _ in 0..(TURBO_PERIOD * 2) {
            let (state, _) = cfg.resolve(&held, &mut st);
            assert!(state.start);
            if !state.a {
                saw_a_off = true;
            }
        }
        assert!(saw_a_off, "normal A press is suppressed/replaced by turbo");
    }

    #[test]
    fn default_config_acceptance_examples_fire() {
        let cfg = InputConfig::default();
        let mut st = ResolveState::new();

        // Start+Select (Enter+LShift) -> Exit toggle on rising edge.
        let (_, fired) = cfg.resolve(&keys(&[KeyName::Enter, KeyName::ShiftLeft]), &mut st);
        assert!(fired.iter().any(|f| f.action == HotkeyAction::Exit && f.rising));

        // Start + Pad R2 -> Fast-forward (hold).
        let mut st2 = ResolveState::new();
        let held = HeldInputs {
            keys: [KeyName::Enter].into_iter().collect(),
            pad: [PadButton::RightTrigger].into_iter().collect(),
        };
        let (_, fired) = cfg.resolve(&held, &mut st2);
        assert!(fired.iter().any(|f| f.action == HotkeyAction::FastForward));
    }

    #[test]
    fn hold_action_fires_every_frame() {
        let cfg = InputConfig {
            gb_bindings: default_gb_bindings(),
            hotkeys: vec![Hotkey {
                chord: vec![InputTrigger::Key(KeyName::Tab)],
                action: HotkeyAction::FastForward,
            }],
        };
        let mut st = ResolveState::new();
        for _ in 0..3 {
            let (_, fired) = cfg.resolve(&keys(&[KeyName::Tab]), &mut st);
            assert_eq!(fired.len(), 1);
            assert_eq!(fired[0].action, HotkeyAction::FastForward);
        }
    }
}

//! Host-agnostic input model.
//!
//! Frontends speak wildly different input vocabularies (winit `KeyCode`,
//! browser `KeyboardEvent.code`, Android `KeyEvent`, libretro `RETRO_DEVICE_ID`
//! bits). We refuse to name any of them here. Instead the adapter classifies
//! each host event into a small abstract set — the eight Game Boy buttons — and
//! feeds a set of currently-pressed [`GbButton`]s to the session as
//! [`AbstractInput`]. A remap table lives in `Config` so a physical host key
//! can be pointed at a different GB button, but the remap is expressed purely
//! in terms of these abstract buttons: the host↔abstract classification is the
//! adapter's job, the abstract↔`ButtonState` mapping is ours.

use rustyboi_core_lib::input::ButtonState;
use serde::{Deserialize, Serialize};

/// The eight logical Game Boy buttons. This is the entire host-agnostic
/// vocabulary the session understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GbButton {
    A,
    B,
    Start,
    Select,
    Up,
    Down,
    Left,
    Right,
}

impl GbButton {
    /// All eight, in a stable order (for remap-table iteration / defaults).
    pub const ALL: [GbButton; 8] = [
        GbButton::A,
        GbButton::B,
        GbButton::Start,
        GbButton::Select,
        GbButton::Up,
        GbButton::Down,
        GbButton::Left,
        GbButton::Right,
    ];

    /// Set this button's field in a `ButtonState`. The single abstract-button →
    /// `ButtonState` field mapping in the crate; `input_config` and `overlay`
    /// both route through this pair rather than re-spelling the match.
    pub fn set(self, s: &mut ButtonState, pressed: bool) {
        match self {
            GbButton::A => s.a = pressed,
            GbButton::B => s.b = pressed,
            GbButton::Start => s.start = pressed,
            GbButton::Select => s.select = pressed,
            GbButton::Up => s.up = pressed,
            GbButton::Down => s.down = pressed,
            GbButton::Left => s.left = pressed,
            GbButton::Right => s.right = pressed,
        }
    }

    /// Read this button's field from a `ButtonState`.
    pub fn get(self, s: &ButtonState) -> bool {
        match self {
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
}

/// The raw per-frame input the adapter hands the session: which abstract GB
/// buttons the host currently has pressed (post host→abstract classification,
/// pre remap). Small and `Copy`; order-independent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AbstractInput {
    pressed: u8,
}

impl AbstractInput {
    /// Empty (nothing pressed).
    pub fn none() -> Self {
        Self::default()
    }

    /// Build from an iterator of pressed abstract buttons.
    pub fn from_pressed<I: IntoIterator<Item = GbButton>>(pressed: I) -> Self {
        let mut a = AbstractInput::none();
        for b in pressed {
            a.set(b, true);
        }
        a
    }

    /// Mark a button pressed/released.
    pub fn set(&mut self, button: GbButton, pressed: bool) {
        let bit = 1u8 << Self::bit_index(button);
        if pressed {
            self.pressed |= bit;
        } else {
            self.pressed &= !bit;
        }
    }

    /// Is this abstract button currently pressed?
    pub fn is_pressed(&self, button: GbButton) -> bool {
        self.pressed & (1u8 << Self::bit_index(button)) != 0
    }

    fn bit_index(button: GbButton) -> u8 {
        match button {
            GbButton::A => 0,
            GbButton::B => 1,
            GbButton::Start => 2,
            GbButton::Select => 3,
            GbButton::Up => 4,
            GbButton::Down => 5,
            GbButton::Left => 6,
            GbButton::Right => 7,
        }
    }

    /// The concrete `ButtonState` the core consumes.
    pub(crate) fn button_state(self) -> ButtonState {
        let mut state = ButtonState::default();
        for b in GbButton::ALL {
            if self.is_pressed(b) {
                b.set(&mut state, true);
            }
        }
        state
    }
}

/// Persisted-config placeholder for the retired abstract-button remap system
/// (the live remapping lives in `InputConfig`). Kept so older `Config` blobs —
/// which carry this table — still load; nothing resolves through it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputMap {
    /// `source[target]` = the abstract button whose press drives `target`.
    source: Vec<(GbButton, GbButton)>,
}

impl Default for InputMap {
    fn default() -> Self {
        InputMap {
            source: GbButton::ALL.iter().map(|&b| (b, b)).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_state_is_passthrough() {
        let input = AbstractInput::from_pressed([GbButton::A, GbButton::Up]);
        let state = input.button_state();
        assert!(state.a && state.up);
        assert!(!state.b && !state.start && !state.down);
    }

    #[test]
    fn abstract_input_set_and_clear() {
        let mut i = AbstractInput::none();
        i.set(GbButton::Start, true);
        assert!(i.is_pressed(GbButton::Start));
        i.set(GbButton::Start, false);
        assert!(!i.is_pressed(GbButton::Start));
    }
}

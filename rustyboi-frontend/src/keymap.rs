//! The single winit-`KeyCode` ⇄ host-agnostic `KeyName` correspondence, shared by
//! the desktop platform (which probes held keys by `KeyCode`) and the web driver
//! (which classifies key events into `KeyName`s). Defined ONCE here; the reverse
//! direction is derived, so a new binding is added in one place.

use rustyboi_session::input_config::KeyName;
use winit::keyboard::KeyCode;

/// The winit physical keycode a [`KeyName`] binds to. Total (every `KeyName` maps).
pub fn key_code(k: KeyName) -> KeyCode {
    use KeyName as N;
    match k {
        N::A => KeyCode::KeyA, N::B => KeyCode::KeyB, N::C => KeyCode::KeyC,
        N::D => KeyCode::KeyD, N::E => KeyCode::KeyE, N::F => KeyCode::KeyF,
        N::G => KeyCode::KeyG, N::H => KeyCode::KeyH, N::I => KeyCode::KeyI,
        N::J => KeyCode::KeyJ, N::K => KeyCode::KeyK, N::L => KeyCode::KeyL,
        N::M => KeyCode::KeyM, N::N => KeyCode::KeyN, N::O => KeyCode::KeyO,
        N::P => KeyCode::KeyP, N::Q => KeyCode::KeyQ, N::R => KeyCode::KeyR,
        N::S => KeyCode::KeyS, N::T => KeyCode::KeyT, N::U => KeyCode::KeyU,
        N::V => KeyCode::KeyV, N::W => KeyCode::KeyW, N::X => KeyCode::KeyX,
        N::Y => KeyCode::KeyY, N::Z => KeyCode::KeyZ,
        N::Num0 => KeyCode::Digit0, N::Num1 => KeyCode::Digit1,
        N::Num2 => KeyCode::Digit2, N::Num3 => KeyCode::Digit3,
        N::Num4 => KeyCode::Digit4, N::Num5 => KeyCode::Digit5,
        N::Num6 => KeyCode::Digit6, N::Num7 => KeyCode::Digit7,
        N::Num8 => KeyCode::Digit8, N::Num9 => KeyCode::Digit9,
        N::Up => KeyCode::ArrowUp, N::Down => KeyCode::ArrowDown,
        N::Left => KeyCode::ArrowLeft, N::Right => KeyCode::ArrowRight,
        N::Enter => KeyCode::Enter, N::Space => KeyCode::Space,
        N::Tab => KeyCode::Tab, N::Backspace => KeyCode::Backspace,
        N::Escape => KeyCode::Escape, N::Backslash => KeyCode::Backslash,
        N::ShiftLeft => KeyCode::ShiftLeft, N::ShiftRight => KeyCode::ShiftRight,
        N::F1 => KeyCode::F1, N::F2 => KeyCode::F2, N::F3 => KeyCode::F3,
        N::F4 => KeyCode::F4, N::F5 => KeyCode::F5, N::F6 => KeyCode::F6,
        N::F7 => KeyCode::F7, N::F8 => KeyCode::F8, N::F9 => KeyCode::F9,
        N::F10 => KeyCode::F10, N::F11 => KeyCode::F11, N::F12 => KeyCode::F12,
    }
}

/// The [`KeyName`] a winit physical keycode binds to, if any — derived from
/// [`key_code`], so the correspondence lives in exactly one table.
pub fn key_name(code: KeyCode) -> Option<KeyName> {
    KeyName::ALL.into_iter().find(|&k| key_code(k) == code)
}

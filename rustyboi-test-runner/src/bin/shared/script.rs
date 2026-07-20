//! The `frame:BUTTONS` input-script DSL shared by the `movie` and `harness`
//! bins (this directory is not auto-binned; each bin pulls it in via
//! `#[path = "shared/script.rs"]`).
//!
//! A script is `;`-separated `frame:BUTTONS` entries; BUTTONS is a
//! `+`-separated list of A,B,START,SELECT,UP,DOWN,LEFT,RIGHT (empty =
//! release everything). Events apply at that frame's start.

use rustyboi_core_lib::input::ButtonState;

pub struct Event {
    pub frame: usize,
    pub buttons: ButtonState,
}

pub fn parse_buttons(spec: &str) -> ButtonState {
    let mut b = ButtonState::default();
    for name in spec.split('+').filter(|s| !s.is_empty()) {
        match name.to_ascii_uppercase().as_str() {
            "A" => b.a = true,
            "B" => b.b = true,
            "START" => b.start = true,
            "SELECT" => b.select = true,
            "UP" => b.up = true,
            "DOWN" => b.down = true,
            "LEFT" => b.left = true,
            "RIGHT" => b.right = true,
            other => panic!("unknown button {other:?}"),
        }
    }
    b
}

/// Parse a script into frame-sorted events.
pub fn parse_script(script: &str) -> Vec<Event> {
    let mut events: Vec<Event> = script
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .map(|entry| {
            let (frame, buttons) = entry
                .split_once(':')
                .unwrap_or_else(|| panic!("bad input event {entry:?} (want frame:BUTTONS)"));
            Event {
                frame: frame.trim().parse().expect("bad frame number"),
                buttons: parse_buttons(buttons.trim()),
            }
        })
        .collect();
    events.sort_by_key(|e| e.frame);
    events
}

/// Expand a script into one `ButtonState` per frame for `frames` frames: the
/// button state at each frame is the most recent event at or before it.
pub fn expand_timeline(script: &str, frames: usize) -> Vec<ButtonState> {
    let events = parse_script(script);
    let mut timeline = Vec::with_capacity(frames);
    let mut cur = ButtonState::default();
    let mut next = 0usize;
    for f in 0..frames {
        while let Some(e) = events.get(next) {
            if e.frame <= f {
                cur = e.buttons;
                next += 1;
            } else {
                break;
            }
        }
        timeline.push(cur);
    }
    timeline
}

//! Main-thread on-screen joypad overlay bridge.
//!
//! The button geometry, hit-testing, and multi-touch resolution all come from
//! the shared, toolkit-agnostic [`rustyboi_session::overlay::TouchLayout`] — the
//! web frontend must NOT invent its own layout. This module is a thin
//! wasm-bindgen wrapper the main-thread JS shell uses to (1) lay out the DOM
//! buttons and (2) turn a set of active pointer coordinates into a pressed-button
//! bitmask it forwards to the worker (`SetTouchMask`).
//!
//! The bitmask convention ([`button_bit`]) is shared with the worker-side input
//! so both ends agree on which bit is which button.

use rustyboi_session::overlay::{OverlayShape, TouchLayout};
use rustyboi_session::GbButton;

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;

/// The bit position for a GB button in the touch bitmask exchanged between the
/// main-thread overlay and the worker. Matches nothing in the core; it is a
/// private wire format shared only by [`TouchOverlay::button_mask`] and
/// `Emulator::set_touch_mask`.
pub(crate) fn button_bit(b: GbButton) -> u8 {
    match b {
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

/// Reconstruct the set of pressed buttons from a bitmask produced by
/// [`TouchOverlay::button_mask`].
pub(crate) fn buttons_from_mask(mask: u8) -> impl Iterator<Item = GbButton> {
    GbButton::ALL
        .into_iter()
        .filter(move |&b| mask & (1u8 << button_bit(b)) != 0)
}

/// Main-thread handle over the shared touch layout. Constructed once in the JS
/// shell; `layout` is called on resize to (re)build the DOM buttons and
/// `button_mask` on every pointer change to derive the pressed set.
#[wasm_bindgen]
pub struct TouchOverlay {
    layout: TouchLayout,
}

#[wasm_bindgen]
impl TouchOverlay {
    /// Build an overlay for a `width x height` CSS-pixel screen rectangle.
    #[wasm_bindgen(constructor)]
    pub fn new(width: f32, height: f32) -> TouchOverlay {
        TouchOverlay { layout: TouchLayout::compute(width, height) }
    }

    /// Recompute geometry for a new screen size (on resize / orientation change).
    pub fn resize(&mut self, width: f32, height: f32) {
        self.layout = TouchLayout::compute(width, height);
    }

    /// The button geometry as an array of plain JS objects
    /// `{ button, label, shape, x, y, w, h }` (rect in screen pixels), for the
    /// shell to position DOM elements. `button` is the lowercase logical name;
    /// `shape` is `"circle"` or `"pill"`.
    pub fn layout(&self) -> Array {
        let out = Array::new();
        for b in &self.layout.buttons {
            let o = Object::new();
            let _ = Reflect::set(&o, &"button".into(), &button_name(b.button).into());
            let _ = Reflect::set(&o, &"label".into(), &b.label.into());
            let shape = match b.shape {
                OverlayShape::Circle => "circle",
                OverlayShape::Pill => "pill",
            };
            let _ = Reflect::set(&o, &"shape".into(), &shape.into());
            let _ = Reflect::set(&o, &"x".into(), &(b.rect.x as f64).into());
            let _ = Reflect::set(&o, &"y".into(), &(b.rect.y as f64).into());
            let _ = Reflect::set(&o, &"w".into(), &(b.rect.w as f64).into());
            let _ = Reflect::set(&o, &"h".into(), &(b.rect.h as f64).into());
            out.push(&o);
        }
        out
    }

    /// The "unit" scale the layout sized itself by (button radius), so the shell
    /// can size fonts/strokes consistently.
    pub fn unit(&self) -> f32 {
        self.layout.unit
    }

    /// Multi-touch: given parallel `xs`/`ys` pointer coordinate arrays (screen
    /// pixels), return the bitmask of every button any pointer is over. Every
    /// pointer is hit-tested against every button, so several fingers press
    /// several buttons at once.
    pub fn button_mask(&self, xs: &[f32], ys: &[f32]) -> u8 {
        let n = xs.len().min(ys.len());
        let pointers: Vec<(f32, f32)> = (0..n).map(|i| (xs[i], ys[i])).collect();
        let state = self.layout.button_state(pointers.iter());
        let mut mask = 0u8;
        for b in GbButton::ALL {
            let pressed = match b {
                GbButton::A => state.a,
                GbButton::B => state.b,
                GbButton::Start => state.start,
                GbButton::Select => state.select,
                GbButton::Up => state.up,
                GbButton::Down => state.down,
                GbButton::Left => state.left,
                GbButton::Right => state.right,
            };
            if pressed {
                mask |= 1u8 << button_bit(b);
            }
        }
        mask
    }
}

fn button_name(b: GbButton) -> &'static str {
    match b {
        GbButton::A => "a",
        GbButton::B => "b",
        GbButton::Start => "start",
        GbButton::Select => "select",
        GbButton::Up => "up",
        GbButton::Down => "down",
        GbButton::Left => "left",
        GbButton::Right => "right",
    }
}

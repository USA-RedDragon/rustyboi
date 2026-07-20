//! Shared, toolkit-agnostic on-screen joypad overlay geometry.
//!
//! The layout math (where the D-pad / A-B / Start-Select buttons sit for a given
//! screen size) and hit-testing live here so every windowed frontend renders the
//! same overlay from the same source. A frontend supplies the current screen
//! rectangle, gets back a [`TouchLayout`] of normalized button rectangles, draws
//! them however its toolkit prefers, and feeds pointer positions through
//! [`TouchLayout::hit_test`] (or [`TouchLayout::button_state`]) to derive input.
//!
//! Nothing here depends on egui/winit/wgpu; the coordinate space is plain `f32`
//! screen pixels with a top-left origin.

use rustyboi_core_lib::input::ButtonState;

use crate::input::GbButton;

/// A rectangle in screen pixels (top-left origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl OverlayRect {
    /// A rect from its center and a size.
    fn from_center(cx: f32, cy: f32, w: f32, h: f32) -> Self {
        OverlayRect { x: cx - w / 2.0, y: cy - h / 2.0, w, h }
    }

    /// Whether `(px, py)` falls inside (inclusive of the min edges).
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    /// Center point.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// How a button's glyph should be drawn: a circle (D-pad / A-B) or a pill
/// (Start / Select). The frontend picks the concrete rendering; this just names
/// the shape so all frontends look alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayShape {
    Circle,
    Pill,
}

/// One drawable button: its hit-test rect, its GB button, its label glyph, and
/// its shape.
#[derive(Clone, Copy, Debug)]
pub struct OverlayButton {
    pub button: GbButton,
    pub rect: OverlayRect,
    pub label: &'static str,
    pub shape: OverlayShape,
}

/// The full overlay geometry for a given screen size. Toolkit-agnostic: a
/// frontend renders each [`OverlayButton`] and hit-tests pointers against it.
#[derive(Clone, Debug)]
pub struct TouchLayout {
    /// The "unit" the layout scaled itself by (button radius scale), exposed so
    /// a frontend can size fonts/strokes consistently.
    pub unit: f32,
    pub buttons: Vec<OverlayButton>,
}

impl TouchLayout {
    /// Compute the overlay geometry for a `width x height` screen (pixels).
    ///
    /// Mirrors the original egui overlay's sizing exactly: a D-pad lower-left,
    /// A/B lower-right, Start/Select center-bottom, with the `unit` bound by both
    /// height and width so the groups never overlap on narrow/foldable screens.
    pub fn compute(width: f32, height: f32) -> TouchLayout {
        let unit = (height * 0.18).min(width * 0.09).clamp(56.0, 130.0);
        let margin = unit * 0.35;
        let mut buttons = Vec::with_capacity(8);

        // --- D-pad (lower-left) ---
        let dpad_w = unit * 3.0;
        let dpad_h = unit * 3.0;
        let dpad_left = margin;
        let dpad_top = height - margin - dpad_h;
        let dcx = dpad_left + dpad_w / 2.0;
        let dcy = dpad_top + dpad_h / 2.0;
        let sq = |cx: f32, cy: f32| OverlayRect::from_center(cx, cy, unit, unit);
        buttons.push(OverlayButton {
            button: GbButton::Up,
            rect: sq(dcx, dcy - unit),
            label: "▲",
            shape: OverlayShape::Circle,
        });
        buttons.push(OverlayButton {
            button: GbButton::Down,
            rect: sq(dcx, dcy + unit),
            label: "▼",
            shape: OverlayShape::Circle,
        });
        buttons.push(OverlayButton {
            button: GbButton::Left,
            rect: sq(dcx - unit, dcy),
            label: "◀",
            shape: OverlayShape::Circle,
        });
        buttons.push(OverlayButton {
            button: GbButton::Right,
            rect: sq(dcx + unit, dcy),
            label: "▶",
            shape: OverlayShape::Circle,
        });

        // --- A/B (lower-right) ---
        let ab_w = unit * 2.8;
        let ab_h = unit * 2.2;
        let ab_left = width - margin - ab_w;
        let ab_top = height - margin - ab_h;
        let a_cx = ab_left + ab_w - unit * 0.6;
        let a_cy = ab_top + unit * 0.7;
        let b_cx = ab_left + unit * 0.6;
        let b_cy = ab_top + ab_h - unit * 0.7;
        buttons.push(OverlayButton {
            button: GbButton::A,
            rect: sq(a_cx, a_cy),
            label: "A",
            shape: OverlayShape::Circle,
        });
        buttons.push(OverlayButton {
            button: GbButton::B,
            rect: sq(b_cx, b_cy),
            label: "B",
            shape: OverlayShape::Circle,
        });

        // --- Start / Select (center-bottom) ---
        let ss_cy = height - margin - (unit * 0.9) * 0.5;
        let ss_cx = width / 2.0;
        let pill_w = unit * 1.6;
        let pill_h = unit * 0.85;
        buttons.push(OverlayButton {
            button: GbButton::Select,
            rect: OverlayRect::from_center(ss_cx - unit, ss_cy, pill_w, pill_h),
            label: "SELECT",
            shape: OverlayShape::Pill,
        });
        buttons.push(OverlayButton {
            button: GbButton::Start,
            rect: OverlayRect::from_center(ss_cx + unit, ss_cy, pill_w, pill_h),
            label: "START",
            shape: OverlayShape::Pill,
        });

        TouchLayout { unit, buttons }
    }

    /// The GB button whose rect contains `(x, y)`, if any (first match).
    pub fn hit_test(&self, x: f32, y: f32) -> Option<GbButton> {
        self.buttons
            .iter()
            .find(|b| b.rect.contains(x, y))
            .map(|b| b.button)
    }

    /// Derive a full [`ButtonState`] from a set of active pointer positions: a
    /// button is pressed if any active pointer falls within its rect. Multi-touch
    /// aware — every pointer is hit-tested against every button.
    pub fn button_state<'a, I>(&self, pointers: I) -> ButtonState
    where
        I: IntoIterator<Item = &'a (f32, f32)>,
    {
        let mut state = ButtonState::default();
        for &(x, y) in pointers {
            for b in &self.buttons {
                if b.rect.contains(x, y) {
                    b.button.set(&mut state, true);
                }
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_centers_map_to_their_button() {
        let l = TouchLayout::compute(1280.0, 720.0);
        for b in &l.buttons {
            let (cx, cy) = b.rect.center();
            assert_eq!(l.hit_test(cx, cy), Some(b.button), "center of {:?}", b.button);
        }
    }

    #[test]
    fn hit_test_outside_all_buttons_is_none() {
        let l = TouchLayout::compute(1280.0, 720.0);
        // Top-center of the screen is clear of every (bottom-anchored) group.
        assert_eq!(l.hit_test(640.0, 4.0), None);
    }

    #[test]
    fn hit_test_boundary_is_exclusive_on_max_edge() {
        let r = OverlayRect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 };
        assert!(r.contains(10.0, 10.0), "min edge inclusive");
        assert!(!r.contains(30.0, 20.0), "max x edge exclusive");
        assert!(!r.contains(20.0, 30.0), "max y edge exclusive");
        assert!(r.contains(29.9, 29.9), "just inside max edge");
    }

    #[test]
    fn button_state_is_multitouch() {
        let l = TouchLayout::compute(1280.0, 720.0);
        let right = l.buttons.iter().find(|b| b.button == GbButton::Right).unwrap();
        let a = l.buttons.iter().find(|b| b.button == GbButton::A).unwrap();
        let pointers = vec![right.rect.center(), a.rect.center()];
        let s = l.button_state(&pointers);
        assert!(s.right && s.a, "both Right and A held at once");
        assert!(!s.left && !s.b);
    }

    #[test]
    fn all_eight_buttons_present() {
        let l = TouchLayout::compute(800.0, 600.0);
        for want in GbButton::ALL {
            assert!(l.buttons.iter().any(|b| b.button == want), "missing {want:?}");
        }
    }
}

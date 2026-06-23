//! On-screen Game Boy controls for touch input, rendered with egui from the
//! shared, toolkit-agnostic [`TouchLayout`] geometry in `rustyboi-session`.
//!
//! The layout math + hit-testing live in `rustyboi_session::overlay`; this file
//! only draws the geometry with egui and derives the pressed [`ButtonState`]
//! from active touches. So desktop, Android (and later web) all render the same
//! overlay from one source of truth.
//!
//! Multi-touch: egui's `Response::is_pointer_button_down_on()` only tracks a
//! single primary pointer, so we consume raw `Event::Touch` events ourselves,
//! keep a map of currently-active touch positions across frames, and hit-test
//! every active touch against the shared layout.

use std::collections::HashMap;

use egui::{Align2, Color32, Context, Event, FontId, Pos2, Rect, Stroke, TouchPhase, Vec2};
use rustyboi_core_lib::input::ButtonState;
use rustyboi_session::overlay::{OverlayRect, OverlayShape, TouchLayout};

/// Cross-frame state for the touch overlay. Tracks each currently pressed touch
/// (by platform-provided id) so we can recognise more than one finger at once.
#[derive(Default)]
pub struct TouchState {
    active: HashMap<u64, Pos2>,
}

/// Render the touch overlay and return the current button state.
/// Draw the on-screen controls at `opacity` (0.0 = invisible .. 1.0 = the full
/// default look) and return the resulting button state.
pub fn show(ctx: &Context, touch_state: &mut TouchState, opacity: f32) -> ButtonState {
    // Drain incoming touch events and update our active-touch map.
    ctx.input(|i| {
        for event in &i.events {
            if let Event::Touch { id, phase, pos, .. } = event {
                match phase {
                    TouchPhase::Start | TouchPhase::Move => {
                        touch_state.active.insert(id.0, *pos);
                    }
                    TouchPhase::End | TouchPhase::Cancel => {
                        touch_state.active.remove(&id.0);
                    }
                }
            }
        }
    });

    let screen = ctx.viewport_rect();
    let layout = TouchLayout::compute(screen.width(), screen.height());

    // Derive pressed state by hit-testing every active touch against the shared
    // layout (multi-touch aware).
    #[cfg_attr(target_os = "android", allow(unused_mut))]
    let mut pointers: Vec<(f32, f32)> = touch_state.active.values().map(|p| (p.x, p.y)).collect();

    ctx.input(|i| {
        if i.pointer.primary_down()
            && let Some(p) = i.pointer.interact_pos()
        {
            pointers.push((p.x, p.y));
        }
    });
    let state = layout.button_state(&pointers);

    // Paint via a single foreground painter so all overlays render on top of the
    // game framebuffer without interfering with egui's layout/interaction.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("rustyboi_touch_controls"),
    ));

    for b in &layout.buttons {
        let held = pressed(&state, b.button);
        match b.shape {
            OverlayShape::Circle => draw_button(&painter, to_rect(b.rect), b.label, held, opacity),
            OverlayShape::Pill => draw_pill(&painter, to_rect(b.rect), b.label, held, opacity),
        }
    }

    // Repaint next frame while any touch is held so state keeps tracking motion.
    if !touch_state.active.is_empty() {
        ctx.request_repaint();
    }

    state
}

fn to_rect(r: OverlayRect) -> Rect {
    Rect::from_min_size(Pos2::new(r.x, r.y), Vec2::new(r.w, r.h))
}

fn pressed(s: &ButtonState, b: rustyboi_session::GbButton) -> bool {
    use rustyboi_session::GbButton;
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

fn draw_pill(painter: &egui::Painter, rect: Rect, label: &str, held: bool, opacity: f32) {
    let fill = if held {
        Color32::from_rgba_premultiplied(180, 180, 180, 200)
    } else {
        Color32::from_rgba_premultiplied(60, 60, 60, 140)
    }
    .linear_multiply(opacity);
    let stroke = Stroke::new(
        2.0,
        Color32::from_rgba_premultiplied(220, 220, 220, 200).linear_multiply(opacity),
    );
    painter.rect(rect, rect.height() * 0.5, fill, stroke, egui::StrokeKind::Middle);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(rect.height() * 0.55),
        Color32::WHITE.linear_multiply(opacity),
    );
}

fn draw_button(painter: &egui::Painter, rect: Rect, label: &str, held: bool, opacity: f32) {
    let fill = if held {
        Color32::from_rgba_premultiplied(220, 220, 220, 220)
    } else {
        Color32::from_rgba_premultiplied(60, 60, 60, 160)
    }
    .linear_multiply(opacity);
    let stroke = Stroke::new(
        2.0,
        Color32::from_rgba_premultiplied(230, 230, 230, 220).linear_multiply(opacity),
    );
    painter.circle(rect.center(), rect.width() * 0.45, fill, stroke);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(rect.width() * 0.40),
        Color32::WHITE.linear_multiply(opacity),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustyboi_session::GbButton;

    /// Set exactly one field of a fresh `ButtonState`, keyed by the button it
    /// should correspond to.
    fn only(button: GbButton) -> ButtonState {
        let mut s = ButtonState::default();
        match button {
            GbButton::A => s.a = true,
            GbButton::B => s.b = true,
            GbButton::Start => s.start = true,
            GbButton::Select => s.select = true,
            GbButton::Up => s.up = true,
            GbButton::Down => s.down = true,
            GbButton::Left => s.left = true,
            GbButton::Right => s.right = true,
        }
        s
    }

    // `pressed` selects one `ButtonState` field per `GbButton`; verify the wiring
    // is a bijection so no button reads a neighbour's bit (a classic copy/paste
    // hazard for eight near-identical arms).
    #[test]
    fn pressed_selects_exactly_the_matching_button() {
        for held in GbButton::ALL {
            let state = only(held);
            for probe in GbButton::ALL {
                assert_eq!(
                    pressed(&state, probe),
                    probe == held,
                    "{held:?} pressed; probing {probe:?}"
                );
            }
        }
    }
}

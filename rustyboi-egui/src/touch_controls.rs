//! On-screen Game Boy controls for touch input.
//!
//! Renders a D-pad on the left and A/B + Start/Select on the right as
//! semi-transparent overlays.
//!
//! Multi-touch: egui's `Response::is_pointer_button_down_on()` only
//! tracks a single primary pointer, so naively wiring buttons up that
//! way means only one button at a time can register as held (e.g. you
//! can't hold Right + A to run-and-jump). Instead, we consume raw
//! `Event::Touch` events from the egui input layer ourselves, keep a
//! map of currently-active touch positions across frames, and decide
//! each button's pressed state by hit-testing every active touch.

use std::collections::HashMap;

use egui::{Align2, Color32, Context, Event, FontId, Pos2, Rect, Stroke, TouchPhase, Vec2};
use rustyboi_core_lib::input::ButtonState;

/// Cross-frame state for the touch overlay. Tracks each currently
/// pressed touch (by platform-provided id) so we can recognise more
/// than one finger at once.
#[derive(Default)]
pub struct TouchState {
    active: HashMap<u64, Pos2>,
}

/// Render the touch overlay and return the current button state.
pub fn show(ctx: &Context, touch_state: &mut TouchState) -> ButtonState {
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

    let screen = ctx.screen_rect();
    let mut state = ButtonState::default();

    // Size buttons relative to screen size; clamp to sane bounds.
    //
    // We bound by both height *and* width: the overlay's horizontal
    // footprint is roughly 10 * unit (D-pad on the left, Start/Select
    // in the middle, A/B on the right), so on near-square or narrow
    // displays (e.g. an unfolded Galaxy Z Fold, or a phone in portrait)
    // a height-only scale produced buttons that overlapped each other.
    // Width-bound `unit` to ~width/11 to guarantee the groups don't
    // touch, and cap the absolute size so the controls don't dominate
    // larger tablet-class screens.
    let unit = (screen.height() * 0.18)
        .min(screen.width() * 0.09)
        .clamp(56.0, 130.0);
    let margin = unit * 0.35;

    // --- D-pad (lower-left) ----------------------------------------
    let dpad_size = Vec2::new(unit * 3.0, unit * 3.0);
    let dpad_rect = Rect::from_min_size(
        Pos2::new(screen.left() + margin, screen.bottom() - margin - dpad_size.y),
        dpad_size,
    );
    let dpad_center = dpad_rect.center();
    let up_rect = button_rect(dpad_center + Vec2::new(0.0, -unit), unit);
    let down_rect = button_rect(dpad_center + Vec2::new(0.0, unit), unit);
    let left_rect = button_rect(dpad_center + Vec2::new(-unit, 0.0), unit);
    let right_rect = button_rect(dpad_center + Vec2::new(unit, 0.0), unit);

    state.up = any_touch_in(touch_state, up_rect);
    state.down = any_touch_in(touch_state, down_rect);
    state.left = any_touch_in(touch_state, left_rect);
    state.right = any_touch_in(touch_state, right_rect);

    // --- A/B (lower-right) -----------------------------------------
    let ab_size = Vec2::new(unit * 2.8, unit * 2.2);
    let ab_rect = Rect::from_min_size(
        Pos2::new(screen.right() - margin - ab_size.x, screen.bottom() - margin - ab_size.y),
        ab_size,
    );
    let a_center = Pos2::new(ab_rect.right() - unit * 0.6, ab_rect.top() + unit * 0.7);
    let b_center = Pos2::new(ab_rect.left() + unit * 0.6, ab_rect.bottom() - unit * 0.7);
    let a_rect = button_rect(a_center, unit);
    let b_rect = button_rect(b_center, unit);

    state.a = any_touch_in(touch_state, a_rect);
    state.b = any_touch_in(touch_state, b_rect);

    // --- Start / Select (center-bottom) ----------------------------
    let ss_size = Vec2::new(unit * 4.0, unit * 0.9);
    let ss_rect = Rect::from_center_size(
        Pos2::new(screen.center().x, screen.bottom() - margin - ss_size.y * 0.5),
        ss_size,
    );
    let pill_size = Vec2::new(unit * 1.6, unit * 0.85);
    let sel_rect = Rect::from_center_size(
        Pos2::new(ss_rect.center().x - unit * 1.0, ss_rect.center().y),
        pill_size,
    );
    let st_rect = Rect::from_center_size(
        Pos2::new(ss_rect.center().x + unit * 1.0, ss_rect.center().y),
        pill_size,
    );

    state.select = any_touch_in(touch_state, sel_rect);
    state.start = any_touch_in(touch_state, st_rect);

    // --- Painting -------------------------------------------------
    // Use a single foreground painter so all overlays render on top of
    // the game framebuffer without interfering with egui's regular
    // layout/interaction system.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("rustyboi_touch_controls"),
    ));

    draw_button(&painter, up_rect, "▲", state.up);
    draw_button(&painter, down_rect, "▼", state.down);
    draw_button(&painter, left_rect, "◀", state.left);
    draw_button(&painter, right_rect, "▶", state.right);
    draw_button(&painter, a_rect, "A", state.a);
    draw_button(&painter, b_rect, "B", state.b);
    draw_pill(&painter, sel_rect, "SELECT", state.select);
    draw_pill(&painter, st_rect, "START", state.start);

    // Ensure egui repaints next frame while any touch is held so that
    // state.* keeps tracking under-finger movement.
    if !touch_state.active.is_empty() {
        ctx.request_repaint();
    }

    state
}

/// Hit-test rectangle used for touches. The visible button is drawn
/// smaller than this so that the press area is forgiving to inaccurate
/// taps; we hit-test the full `unit`-sized square.
fn button_rect(center: Pos2, unit: f32) -> Rect {
    Rect::from_center_size(center, Vec2::splat(unit))
}

fn any_touch_in(touch_state: &TouchState, rect: Rect) -> bool {
    touch_state.active.values().any(|p| rect.contains(*p))
}

fn draw_pill(painter: &egui::Painter, rect: Rect, label: &str, held: bool) {
    let fill = if held {
        Color32::from_rgba_premultiplied(180, 180, 180, 200)
    } else {
        Color32::from_rgba_premultiplied(60, 60, 60, 140)
    };
    let stroke = Stroke::new(2.0, Color32::from_rgba_premultiplied(220, 220, 220, 200));
    painter.rect(rect, rect.height() * 0.5, fill, stroke);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(rect.height() * 0.55),
        Color32::WHITE,
    );
}

fn draw_button(painter: &egui::Painter, rect: Rect, label: &str, held: bool) {
    let fill = if held {
        Color32::from_rgba_premultiplied(220, 220, 220, 220)
    } else {
        Color32::from_rgba_premultiplied(60, 60, 60, 160)
    };
    let stroke = Stroke::new(2.0, Color32::from_rgba_premultiplied(230, 230, 230, 220));
    painter.circle(rect.center(), rect.width() * 0.45, fill, stroke);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(rect.width() * 0.40),
        Color32::WHITE,
    );
}

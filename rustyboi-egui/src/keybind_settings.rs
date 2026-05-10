//! The live keybind editor: rebind GB buttons (key capture) and edit chord
//! hotkeys (record held keys + GB/pad dropdowns + action picker). Reads/writes a
//! working [`InputConfig`] seeded from the persisted `SessionUiState.input` and
//! emits [`GuiAction::SetInputConfig`] on every change, which `Session::apply`
//! persists. Gamepad triggers are added via dropdowns (egui can't poll pads).

use egui::Context;

use crate::actions::{GuiAction, SessionUiState};
use crate::ui::Gui;
use rustyboi_session::input_config::{gb_label, HotkeyAction, InputTrigger, KeyName, PadButton};
use rustyboi_session::{GbButton, Hotkey, InputConfig};

/// Map an egui key to the host-agnostic [`KeyName`] vocabulary, if representable.
fn key_from_egui(key: egui::Key) -> Option<KeyName> {
    use egui::Key as E;
    Some(match key {
        E::A => KeyName::A, E::B => KeyName::B, E::C => KeyName::C,
        E::D => KeyName::D, E::E => KeyName::E, E::F => KeyName::F,
        E::G => KeyName::G, E::H => KeyName::H, E::I => KeyName::I,
        E::J => KeyName::J, E::K => KeyName::K, E::L => KeyName::L,
        E::M => KeyName::M, E::N => KeyName::N, E::O => KeyName::O,
        E::P => KeyName::P, E::Q => KeyName::Q, E::R => KeyName::R,
        E::S => KeyName::S, E::T => KeyName::T, E::U => KeyName::U,
        E::V => KeyName::V, E::W => KeyName::W, E::X => KeyName::X,
        E::Y => KeyName::Y, E::Z => KeyName::Z,
        E::Num0 => KeyName::Num0, E::Num1 => KeyName::Num1,
        E::Num2 => KeyName::Num2, E::Num3 => KeyName::Num3,
        E::Num4 => KeyName::Num4, E::Num5 => KeyName::Num5,
        E::Num6 => KeyName::Num6, E::Num7 => KeyName::Num7,
        E::Num8 => KeyName::Num8, E::Num9 => KeyName::Num9,
        E::ArrowUp => KeyName::Up, E::ArrowDown => KeyName::Down,
        E::ArrowLeft => KeyName::Left, E::ArrowRight => KeyName::Right,
        E::Enter => KeyName::Enter, E::Space => KeyName::Space,
        E::Tab => KeyName::Tab, E::Backspace => KeyName::Backspace,
        E::Escape => KeyName::Escape, E::Backslash => KeyName::Backslash,
        E::F1 => KeyName::F1, E::F2 => KeyName::F2, E::F3 => KeyName::F3,
        E::F4 => KeyName::F4, E::F5 => KeyName::F5, E::F6 => KeyName::F6,
        E::F7 => KeyName::F7, E::F8 => KeyName::F8, E::F9 => KeyName::F9,
        E::F10 => KeyName::F10, E::F11 => KeyName::F11, E::F12 => KeyName::F12,
        _ => return None,
    })
}

impl Gui {
    pub(crate) fn render_keybind_settings_panel(
        &mut self,
        ctx: &Context,
        action: &mut Option<GuiAction>,
        session: &SessionUiState,
        held_pad: &std::collections::HashSet<PadButton>,
    ) {
        // Seed the working copy from persisted state when the panel first opens.
        if self.input_config.is_none() {
            self.input_config = Some(session.input.clone());
        }

        // Capture keyboard input for whichever rebind/record mode is active.
        // Read events once before borrowing self mutably in the closures.
        let (pressed_key, keys_down): (Option<KeyName>, Vec<KeyName>) = ctx.input(|i| {
            let pressed = i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, .. } => key_from_egui(*key),
                _ => None,
            });
            let down = i.keys_down.iter().filter_map(|k| key_from_egui(*k)).collect();
            (pressed, down)
        });

        // A held gamepad button for capture (bind-by-press / chord recording):
        // egui never sees pad input, so the platform passes the held-pad set in.
        let pressed_pad: Option<PadButton> = held_pad.iter().next().copied();

        let mut changed = false;

        egui::Window::new("Controls")
            .default_pos([300.0, 50.0])
            .default_size([360.0, 520.0])
            .collapsible(true)
            .resizable(true)
            .frame(egui::Frame::window(&ctx.style_of(ctx.theme())).fill(crate::ui::PANEL_BACKGROUND))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    changed |= self.gb_bindings_section(ui, pressed_key, pressed_pad);
                    ui.add_space(12.0);
                    ui.separator();
                    changed |= self.hotkeys_section(ui, &keys_down, held_pad);
                    ui.add_space(12.0);
                    ui.separator();
                    if ui.button("Reset to Defaults").clicked() {
                        self.input_config = Some(InputConfig::default());
                        self.rebinding_gb = None;
                        self.recording_chord = None;
                        self.recorded_chord.clear();
                        changed = true;
                    }
                });
            });

        if changed
            && let Some(cfg) = &self.input_config {
                *action = Some(GuiAction::SetInputConfig(cfg.clone()));
            }
    }

    fn gb_bindings_section(
        &mut self,
        ui: &mut egui::Ui,
        pressed_key: Option<KeyName>,
        pressed_pad: Option<PadButton>,
    ) -> bool {
        let Some(cfg) = self.input_config.as_mut() else { return false };
        let mut changed = false;
        ui.heading("Buttons");
        ui.label(
            egui::RichText::new(
                "Add a keyboard key or controller button to each Game Boy button. \
                 A button works if any of its bindings is pressed. Click a binding to remove it.",
            )
            .weak(),
        );
        ui.add_space(6.0);

        // Resolve a pending capture: a keyboard key (Escape cancels) or, since egui
        // can't see the gamepad, a held pad button passed in by the platform.
        if let Some(btn) = self.rebinding_gb {
            let trigger = match (pressed_key, pressed_pad) {
                (Some(KeyName::Escape), _) => {
                    self.rebinding_gb = None;
                    None
                }
                (Some(key), _) => Some(InputTrigger::Key(key)),
                (None, Some(pad)) => Some(InputTrigger::Pad(pad)),
                (None, None) => None,
            };
            if let Some(t) = trigger {
                if let Some((_, triggers)) = cfg.gb_bindings.iter_mut().find(|(b, _)| *b == btn)
                    && !triggers.contains(&t) {
                        triggers.push(t);
                        changed = true;
                    }
                self.rebinding_gb = None;
            }
        }

        let rebinding = self.rebinding_gb;
        let mut add_pad: Option<(GbButton, PadButton)> = None;
        let mut remove: Option<(GbButton, usize)> = None;
        let mut start_capture: Option<GbButton> = None;

        egui::Grid::new("gb_binds")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                for gb in GbButton::ALL {
                    ui.strong(gb_label(gb));
                    ui.horizontal_wrapped(|ui| {
                        let triggers: Vec<InputTrigger> = cfg
                            .gb_bindings
                            .iter()
                            .find(|(b, _)| *b == gb)
                            .map(|(_, t)| t.clone())
                            .unwrap_or_default();
                        for (i, t) in triggers.iter().enumerate() {
                            if ui.small_button(t.label()).on_hover_text("Remove").clicked() {
                                remove = Some((gb, i));
                            }
                        }
                        if rebinding == Some(gb) {
                            ui.label(
                                egui::RichText::new("press a key or button…")
                                    .italics()
                                    .color(egui::Color32::LIGHT_BLUE),
                            );
                        } else {
                            ui.menu_button("Add…", |ui| {
                                if ui.button("Press a key or button").clicked() {
                                    start_capture = Some(gb);
                                    ui.close();
                                }
                                ui.separator();
                                ui.label(egui::RichText::new("Controller").weak());
                                if let Some(p) = pad_menu(ui) {
                                    add_pad = Some((gb, p));
                                    ui.close();
                                }
                            });
                        }
                    });
                    ui.end_row();
                }
            });

        if let Some(gb) = start_capture {
            self.rebinding_gb = Some(gb);
        }
        if let Some((gb, p)) = add_pad
            && let Some((_, tr)) = cfg.gb_bindings.iter_mut().find(|(b, _)| *b == gb) {
                let t = InputTrigger::Pad(p);
                if !tr.contains(&t) {
                    tr.push(t);
                    changed = true;
                }
            }
        if let Some((gb, i)) = remove
            && let Some((_, tr)) = cfg.gb_bindings.iter_mut().find(|(b, _)| *b == gb)
                && i < tr.len() {
                    tr.remove(i);
                    changed = true;
                }
        changed
    }

    fn hotkeys_section(
        &mut self,
        ui: &mut egui::Ui,
        keys_down: &[KeyName],
        held_pad: &std::collections::HashSet<PadButton>,
    ) -> bool {
        let Some(cfg) = self.input_config.as_mut() else { return false };
        let mut changed = false;
        ui.heading("Shortcuts");
        ui.label(
            egui::RichText::new("Hold all the listed buttons together to run the action.").weak(),
        );
        ui.add_space(6.0);

        // Update the recording buffer from currently-held keys AND gamepad buttons
        // (union), so a chord like Select + R-trigger can be recorded by pressing.
        if self.recording_chord.is_some() {
            for k in keys_down {
                let t = InputTrigger::Key(*k);
                if !self.recorded_chord.contains(&t) {
                    self.recorded_chord.push(t);
                }
            }
            for p in held_pad {
                let t = InputTrigger::Pad(*p);
                if !self.recorded_chord.contains(&t) {
                    self.recorded_chord.push(t);
                }
            }
        }

        let recording_idx = self.recording_chord;
        let recorded_preview = chord_label(&self.recorded_chord);
        let mut remove: Option<usize> = None;
        let mut toggle_record: Option<usize> = None;
        let mut add_gb: Option<(usize, GbButton)> = None;
        let mut add_pad: Option<(usize, PadButton)> = None;
        let mut clear: Option<usize> = None;

        for i in 0..cfg.hotkeys.len() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(chord_label(&cfg.hotkeys[i].chord)).monospace());
                ui.label("→");
                let mut act = cfg.hotkeys[i].action;
                if action_combo(ui, i, &mut act) {
                    cfg.hotkeys[i].action = act;
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                let recording = recording_idx == Some(i);
                let rec_label = if recording {
                    format!("Stop ({recorded_preview})")
                } else {
                    "Record".to_string()
                };
                if ui.button(rec_label).clicked() {
                    toggle_record = Some(i);
                }
                ui.menu_button("Add button", |ui| {
                    ui.label(egui::RichText::new("Game Boy").weak());
                    if let Some(b) = gb_menu(ui) {
                        add_gb = Some((i, b));
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Controller").weak());
                    if let Some(p) = pad_menu(ui) {
                        add_pad = Some((i, p));
                        ui.close();
                    }
                });
                if ui.button("Clear").clicked() {
                    clear = Some(i);
                }
                if ui.button("✕").on_hover_text("Delete shortcut").clicked() {
                    remove = Some(i);
                }
            });
            ui.separator();
        }

        if let Some(i) = toggle_record {
            if recording_idx == Some(i) {
                if !self.recorded_chord.is_empty() {
                    cfg.hotkeys[i].chord = self.recorded_chord.clone();
                    changed = true;
                }
                self.recording_chord = None;
                self.recorded_chord.clear();
            } else {
                self.recording_chord = Some(i);
                self.recorded_chord.clear();
            }
        }
        if let Some((i, b)) = add_gb {
            let t = InputTrigger::Gb(b);
            if !cfg.hotkeys[i].chord.contains(&t) {
                cfg.hotkeys[i].chord.push(t);
                changed = true;
            }
        }
        if let Some((i, p)) = add_pad {
            let t = InputTrigger::Pad(p);
            if !cfg.hotkeys[i].chord.contains(&t) {
                cfg.hotkeys[i].chord.push(t);
                changed = true;
            }
        }
        if let Some(i) = clear {
            cfg.hotkeys[i].chord.clear();
            changed = true;
        }
        if let Some(i) = remove {
            cfg.hotkeys.remove(i);
            if self.recording_chord == Some(i) {
                self.recording_chord = None;
                self.recorded_chord.clear();
            }
            changed = true;
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            action_combo(ui, usize::MAX, &mut self.new_hotkey_action);
            if ui.button("Add shortcut").clicked() {
                cfg.hotkeys.push(Hotkey {
                    chord: Vec::new(),
                    action: self.new_hotkey_action,
                });
                changed = true;
            }
        });
        changed
    }
}

fn chord_label(chord: &[InputTrigger]) -> String {
    if chord.is_empty() {
        return "(empty)".to_string();
    }
    chord
        .iter()
        .map(|t| t.label())
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Action dropdown, including Turbo(button) variants. `id` disambiguates the
/// egui widget id (usize::MAX = the "new hotkey" staging row).
fn action_combo(ui: &mut egui::Ui, id: usize, action: &mut HotkeyAction) -> bool {
    let before = *action;
    egui::ComboBox::from_id_salt(("hotkey_action", id))
        .selected_text(action.label())
        .show_ui(ui, |ui| {
            for a in HotkeyAction::SIMPLE {
                ui.selectable_value(action, a, a.label());
            }
            for b in GbButton::ALL {
                let a = HotkeyAction::Turbo(b);
                ui.selectable_value(action, a, a.label());
            }
        });
    *action != before
}

/// Game Boy button list for a menu; returns the clicked button. 8 items, no
/// scroll needed.
fn gb_menu(ui: &mut egui::Ui) -> Option<GbButton> {
    let mut picked = None;
    for b in GbButton::ALL {
        if ui.button(gb_label(b)).clicked() {
            picked = Some(b);
        }
    }
    picked
}

/// Controller-button list for a menu (face/shoulder/d-pad/stick, ~22 items so
/// it scrolls); returns the clicked button.
fn pad_menu(ui: &mut egui::Ui) -> Option<PadButton> {
    let mut picked = None;
    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
        for p in PadButton::ALL {
            if ui.button(p.label()).clicked() {
                picked = Some(p);
            }
        }
    });
    picked
}

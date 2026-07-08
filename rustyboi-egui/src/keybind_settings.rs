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

        let mut changed = false;

        egui::Window::new("Keybind Settings")
            .default_pos([300.0, 50.0])
            .default_size([340.0, 520.0])
            .collapsible(true)
            .resizable(true)
            .frame(egui::Frame::window(&ctx.style()).fill(crate::ui::PANEL_BACKGROUND))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    changed |= self.gb_bindings_section(ui, pressed_key);
                    ui.add_space(12.0);
                    ui.separator();
                    changed |= self.hotkeys_section(ui, &keys_down);
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

        if changed {
            if let Some(cfg) = &self.input_config {
                *action = Some(GuiAction::SetInputConfig(cfg.clone()));
            }
        }
    }

    fn gb_bindings_section(&mut self, ui: &mut egui::Ui, pressed_key: Option<KeyName>) -> bool {
        let Some(cfg) = self.input_config.as_mut() else { return false };
        let mut changed = false;
        ui.heading("Game Boy Buttons");
        ui.label("Click Rebind, then press a key. (First trigger shown; extra triggers preserved.)");
        ui.add_space(6.0);

        // Complete a pending rebind if a key was pressed this frame.
        if let (Some(btn), Some(key)) = (self.rebinding_gb, pressed_key) {
            for (b, triggers) in cfg.gb_bindings.iter_mut() {
                if *b == btn {
                    if triggers.is_empty() {
                        triggers.push(InputTrigger::Key(key));
                    } else {
                        triggers[0] = InputTrigger::Key(key);
                    }
                    break;
                }
            }
            self.rebinding_gb = None;
            changed = true;
        }

        egui::Grid::new("gb_bindings_grid")
            .num_columns(3)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for gb in GbButton::ALL {
                    ui.label(gb_label(gb));
                    let binding_text = cfg
                        .gb_bindings
                        .iter()
                        .find(|(b, _)| *b == gb)
                        .and_then(|(_, t)| t.first())
                        .map(|t| t.label())
                        .unwrap_or_else(|| "(unbound)".to_string());
                    ui.monospace(binding_text);

                    let recording = self.rebinding_gb == Some(gb);
                    let label = if recording { "Press a key..." } else { "Rebind" };
                    if ui.button(label).clicked() {
                        self.rebinding_gb = if recording { None } else { Some(gb) };
                    }
                    ui.end_row();
                }
            });
        changed
    }

    fn hotkeys_section(&mut self, ui: &mut egui::Ui, keys_down: &[KeyName]) -> bool {
        let Some(cfg) = self.input_config.as_mut() else { return false };
        let mut changed = false;
        ui.heading("Hotkeys");
        ui.label("Chord = all triggers held. Record captures currently-held keys.");
        ui.add_space(6.0);

        // Update the recording buffer from currently-held keys (union).
        if self.recording_chord.is_some() {
            for k in keys_down {
                let t = InputTrigger::Key(*k);
                if !self.recorded_chord.contains(&t) {
                    self.recorded_chord.push(t);
                }
            }
        }

        let mut remove: Option<usize> = None;
        for i in 0..cfg.hotkeys.len() {
            ui.horizontal(|ui| {
                let chord_text = chord_label(&cfg.hotkeys[i].chord);
                ui.monospace(chord_text);
                ui.label("→");
                ui.label(cfg.hotkeys[i].action.label());
            });
            ui.horizontal(|ui| {
                let recording = self.recording_chord == Some(i);
                let rec_label = if recording {
                    format!("Recording ({})", chord_label(&self.recorded_chord))
                } else {
                    "Record chord".to_string()
                };
                if ui.button(rec_label).clicked() {
                    if recording {
                        // Commit the recorded chord if non-empty.
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
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
            // Action dropdown for this row.
            let mut act = cfg.hotkeys[i].action;
            if action_combo(ui, i, &mut act) {
                cfg.hotkeys[i].action = act;
                changed = true;
            }
            // Explicit trigger pickers for GB buttons and gamepad buttons
            // (the live recorder only captures keyboard keys).
            ui.horizontal(|ui| {
                if let Some(b) = gb_pick(ui, format!("addgb_{i}")) {
                    let t = InputTrigger::Gb(b);
                    if !cfg.hotkeys[i].chord.contains(&t) {
                        cfg.hotkeys[i].chord.push(t);
                        changed = true;
                    }
                }
                if let Some(p) = pad_pick(ui, format!("addpad_{i}")) {
                    let t = InputTrigger::Pad(p);
                    if !cfg.hotkeys[i].chord.contains(&t) {
                        cfg.hotkeys[i].chord.push(t);
                        changed = true;
                    }
                }
                if ui.button("Clear chord").clicked() {
                    cfg.hotkeys[i].chord.clear();
                    changed = true;
                }
            });
            ui.separator();
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
            if ui.button("Add hotkey").clicked() {
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
    egui::ComboBox::from_id_source(("hotkey_action", id))
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

/// A one-shot GB-button picker (adds the selected GB button to a chord).
fn gb_pick(ui: &mut egui::Ui, id: String) -> Option<GbButton> {
    let mut picked = None;
    egui::ComboBox::from_id_source(id)
        .selected_text("Add GB button")
        .show_ui(ui, |ui| {
            for b in GbButton::ALL {
                if ui.selectable_label(false, gb_label(b)).clicked() {
                    picked = Some(b);
                }
            }
        });
    picked
}

/// A one-shot pad-button picker (adds the selected pad button to a chord).
fn pad_pick(ui: &mut egui::Ui, id: String) -> Option<PadButton> {
    let mut picked = None;
    egui::ComboBox::from_id_source(id)
        .selected_text("Add pad button")
        .show_ui(ui, |ui| {
            for p in PadButton::ALL {
                if ui.selectable_label(false, p.label()).clicked() {
                    picked = Some(p);
                }
            }
        });
    picked
}

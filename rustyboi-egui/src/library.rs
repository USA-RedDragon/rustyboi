//! On-screen ROM library panel (Android-only).
//!
//! Renders a list of ROMs that the platform layer discovered by
//! recursively scanning a user-picked SAF tree. The panel itself is
//! purely a view: scan results and the current tree URI are pushed in
//! from the event loop via [`LibraryPanel::set_entries`] /
//! [`LibraryPanel::set_tree_uri`]. User clicks return [`GuiAction`]s
//! that the event loop dispatches into the Android bridge.

//! Compiled on Android, and on the host under `cfg(test)` so the pure list/label
//! logic ([`LibraryPanel::set_entries`], [`entry_label`]) can be unit-tested; the
//! interactive `show()` renderer stays Android-only (it needs the Android-gated
//! `GuiAction` variants and the platform bridge).
#![cfg(any(target_os = "android", test))]

#[cfg(target_os = "android")]
use egui::Context;

#[cfg(target_os = "android")]
use crate::actions::GuiAction;
use crate::actions::LibraryEntry;
#[cfg(target_os = "android")]
use crate::ui::PANEL_BACKGROUND;

// `show()` (Android-only) is the sole reader of several fields; keep them off the
// host dead-code lint without weakening the check on the real build.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Default)]
pub struct LibraryPanel {
    pub open: bool,
    tree_uri: Option<String>,
    entries: Vec<LibraryEntry>,
    /// SAF URIs of recently opened ROMs, most-recent first. Pushed in
    /// by the platform layer after a successful load and rendered in a
    /// dedicated section at the top of the panel.
    recents: Vec<String>,
    filter: String,
    scanning: bool,
    status: Option<String>,
}

impl LibraryPanel {
    pub fn set_tree_uri(&mut self, uri: Option<String>) {
        self.tree_uri = uri;
    }

    pub fn tree_uri(&self) -> Option<&str> {
        self.tree_uri.as_deref()
    }

    /// Replace the recents list. Caller is responsible for ordering
    /// (most-recent first) and de-duplication.
    pub fn set_recents(&mut self, recents: Vec<String>) {
        self.recents = recents;
    }

    pub fn set_entries(&mut self, entries: Vec<LibraryEntry>) {
        self.entries = entries;
        // Sort case-insensitively by rel_path so the user sees a
        // stable, predictable list regardless of the order SAF
        // returned them in (which is provider-specific).
        self.entries.sort_by(|a, b| {
            let ka = a.rel_path.to_lowercase();
            let kb = b.rel_path.to_lowercase();
            ka.cmp(&kb)
        });
        self.scanning = false;
        self.status = Some(format!("{} ROMs", self.entries.len()));
    }

    pub fn begin_scan(&mut self) {
        self.scanning = true;
        self.status = Some("Scanning…".into());
    }

    pub fn set_status(&mut self, status: Option<String>) {
        self.status = status;
    }

    /// Render the panel; returns a `GuiAction` if the user interacted
    /// with one of the buttons.
    #[cfg(target_os = "android")]
    pub fn show(&mut self, ctx: &Context) -> Option<GuiAction> {
        if !self.open {
            return None;
        }
        let mut action: Option<GuiAction> = None;
        let mut open = self.open;
        // Horizontally center the window on first appearance. `default_pos`
        // only seeds the initial position; egui still remembers any drags
        // the user does afterwards.
        const DEFAULT_WIDTH: f32 = 520.0;
        const DEFAULT_HEIGHT: f32 = 400.0;
        let screen = ctx.viewport_rect();
        let default_pos = egui::Pos2::new(
            (screen.center().x - DEFAULT_WIDTH * 0.5).max(screen.left()),
            screen.top() + 32.0,
        );
        egui::Window::new("ROM Library")
            .open(&mut open)
            .frame(egui::Frame::window(&ctx.style_of(ctx.theme())).fill(PANEL_BACKGROUND))
            .default_pos(default_pos)
            .default_size([DEFAULT_WIDTH, DEFAULT_HEIGHT])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Pick folder…").clicked() {
                        action = Some(GuiAction::OpenRomTree);
                    }
                    let can_rescan = self.tree_uri.is_some() && !self.scanning;
                    if ui
                        .add_enabled(can_rescan, egui::Button::new("Rescan"))
                        .clicked()
                    {
                        action = Some(GuiAction::RescanLibrary);
                    }
                });
                if let Some(uri) = &self.tree_uri {
                    ui.label(egui::RichText::new(uri).small().weak());
                } else {
                    ui.label(
                        egui::RichText::new(
                            "No library folder selected. Pick a folder \
                             containing your ROMs (subfolders are scanned \
                             recursively).",
                        )
                        .italics(),
                    );
                }
                if let Some(status) = &self.status {
                    ui.label(status);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .desired_width(f32::INFINITY),
                    );
                    // winit's android-game-activity backend doesn't
                    // currently raise the soft keyboard when egui
                    // calls `set_ime_allowed(true)`, so drive it
                    // manually via the platform bridge whenever this
                    // widget gains/loses focus.
                    if resp.gained_focus() {
                        crate::android_bridge::set_ime_visible(true);
                    }
                    if resp.lost_focus() {
                        crate::android_bridge::set_ime_visible(false);
                    }
                });
                ui.separator();
                let filter = self.filter.to_lowercase();
                // Build a quick lookup from URI -> entry so the
                // recents section can render full rel_paths and we can
                // dim recents that no longer appear in the current
                // scan (e.g. the ROM was moved or deleted).
                let by_uri: std::collections::HashMap<&str, &LibraryEntry> = self
                    .entries
                    .iter()
                    .map(|e| (e.uri.as_str(), e))
                    .collect();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Recently played section. Only shown when the
                        // filter is empty so it doesn't fight with an
                        // active search.
                        if filter.is_empty() && !self.recents.is_empty() {
                            ui.label(
                                egui::RichText::new("Recently played")
                                    .strong(),
                            );
                            for uri in &self.recents {
                                let (label, present) =
                                    if let Some(entry) = by_uri.get(uri.as_str()) {
                                        (entry_label(entry), true)
                                    } else {
                                        // Recent ROM not in the current
                                        // scan; derive a best-effort
                                        // display name from the URI's
                                        // tail and dim the row.
                                        let tail = uri
                                            .rsplit(['/', '%'])
                                            .next()
                                            .unwrap_or(uri.as_str())
                                            .to_string();
                                        (tail, false)
                                    };
                                let mut btn = egui::Button::new(
                                    if present {
                                        egui::RichText::new(label.clone())
                                    } else {
                                        egui::RichText::new(label.clone()).weak()
                                    },
                                )
                                .min_size(egui::vec2(ui.available_width(), 0.0))
                                .wrap();
                                if !present {
                                    // Tapping a missing recent still
                                    // attempts a load; the SAF layer
                                    // may succeed via persisted grant
                                    // even when the scan missed it
                                    // (e.g. picker stale).
                                    btn = btn.fill(egui::Color32::TRANSPARENT);
                                }
                                if ui.add(btn).clicked() {
                                    action = Some(GuiAction::LoadRomFromUri(uri.clone()));
                                }
                            }
                            ui.separator();
                            ui.label(egui::RichText::new("All ROMs").strong());
                        }
                        for entry in &self.entries {
                            let label = entry_label(entry);
                            if !filter.is_empty()
                                && !label.to_lowercase().contains(&filter)
                                && !entry.rel_path.to_lowercase().contains(&filter)
                            {
                                continue;
                            }
                            let btn = egui::Button::new(label)
                                .min_size(egui::vec2(ui.available_width(), 0.0))
                                .wrap();
                            if ui.add(btn).clicked() {
                                action = Some(GuiAction::LoadRomFromUri(
                                    entry.uri.clone(),
                                ));
                            }
                        }
                    });
            });
        self.open = open;
        action
    }
}

/// Display label for a library entry: the canonical No-Intro name when the
/// scanner's CRC32 resolves to one, else the relative path (or bare filename).
fn entry_label(entry: &LibraryEntry) -> String {
    if entry.crc32 != 0
        && let Some(name) = rustyboi_session::no_intro::name_for_crc(entry.crc32)
    {
        return name.to_string();
    }
    if entry.rel_path.is_empty() {
        entry.name.clone()
    } else {
        entry.rel_path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(rel_path: &str, name: &str, crc32: u32) -> LibraryEntry {
        LibraryEntry {
            uri: format!("content://{rel_path}"),
            name: name.to_string(),
            rel_path: rel_path.to_string(),
            size_bytes: 0,
            crc32,
        }
    }

    #[test]
    fn set_entries_sorts_case_insensitively_and_reports_count() {
        let mut panel = LibraryPanel::default();
        panel.begin_scan();
        panel.set_entries(vec![
            entry("Zelda.gb", "Zelda.gb", 0),
            entry("apple.gb", "apple.gb", 0),
            entry("Banana.gb", "Banana.gb", 0),
        ]);

        let order: Vec<&str> = panel.entries.iter().map(|e| e.rel_path.as_str()).collect();
        // Case-insensitive: 'apple' precedes 'Banana' precedes 'Zelda', which a
        // naive ASCII sort (uppercase < lowercase) would get wrong.
        assert_eq!(order, ["apple.gb", "Banana.gb", "Zelda.gb"]);
        assert_eq!(panel.status.as_deref(), Some("3 ROMs"));
        // Receiving results ends the scanning state.
        assert!(!panel.scanning);
    }

    #[test]
    fn entry_label_prefers_no_intro_then_rel_path_then_name() {
        // Merge a unique CRC into the shared index so this is order-independent.
        const CRC: u32 = 0x1234_ABCD;
        rustyboi_session::no_intro::load_dats(&[
            "game (\n\tname \"Canonical No-Intro Name\"\n\trom ( crc 1234ABCD )\n)\n".to_string(),
        ]);

        // CRC hit wins over both path and name.
        assert_eq!(
            entry_label(&entry("some/path.gb", "path.gb", CRC)),
            "Canonical No-Intro Name"
        );
        // Unknown CRC falls back to the relative path.
        assert_eq!(entry_label(&entry("sub/dir/game.gb", "game.gb", 0)), "sub/dir/game.gb");
        // Empty rel_path falls back to the bare filename.
        assert_eq!(entry_label(&entry("", "bare.gb", 0)), "bare.gb");
    }
}

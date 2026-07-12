use crate::ui::Gui;
use egui::{Color32, Context, RichText};
use rustyboi_session::DebugSnapshot;

/// Human-readable ROM/RAM size (GB sizes are powers of two).
fn human(n: usize) -> String {
    if n == 0 {
        "none".to_string()
    } else if n >= 1 << 20 {
        format!("{} MiB", n as f64 / (1 << 20) as f64)
    } else {
        format!("{} KiB", n as f64 / (1 << 10) as f64)
    }
}

impl Gui {
    pub(crate) fn render_cartridge_info_panel(&mut self, ctx: &Context, debug: Option<&DebugSnapshot>) {
        let info = debug.and_then(|s| s.cartridge.as_ref());
        egui::Window::new("Cartridge Info")
            .default_pos([270.0, 50.0])
            .default_size([320.0, 480.0])
            .collapsible(true)
            .resizable(true)
            .frame(egui::Frame::window(&ctx.style_of(ctx.theme())).fill(crate::ui::PANEL_BACKGROUND))
            .show(ctx, |ui| {
                let Some(c) = info else {
                    ui.label(RichText::new("No cartridge loaded.").color(Color32::GRAY));
                    return;
                };

                let head = |ui: &mut egui::Ui, t: &str| {
                    ui.add_space(4.0);
                    ui.label(RichText::new(t).color(Color32::LIGHT_GRAY).strong());
                };
                // A label/value row inside the grid.
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(RichText::new(k).color(Color32::GRAY));
                    ui.label(RichText::new(v).color(Color32::WHITE).monospace());
                    ui.end_row();
                };
                let flag = |ui: &mut egui::Ui, k: &str, on: bool| {
                    ui.label(RichText::new(k).color(Color32::GRAY));
                    ui.label(if on {
                        RichText::new("yes").color(Color32::LIGHT_GREEN)
                    } else {
                        RichText::new("no").color(Color32::DARK_GRAY)
                    });
                    ui.end_row();
                };

                head(ui, "Identity");
                egui::Grid::new("cart_id").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                    row(ui, "Title", if c.title.is_empty() { "—".into() } else { c.title.clone() });
                    row(ui, "Mapper", c.mapper.clone());
                    row(ui, "Type byte", format!("{:#04X}", c.type_byte));
                    row(ui, "Licensee", c.licensee.clone().unwrap_or_else(|| "unknown".into()));
                    row(ui, "Region", c.destination.clone().unwrap_or_else(|| "—".into()));
                });

                head(ui, "Size");
                egui::Grid::new("cart_size").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                    row(ui, "ROM", format!("{} ({} banks)", human(c.rom_bytes), c.rom_banks));
                    row(ui, "RAM", human(c.ram_bytes));
                    row(ui, "Cur ROM bank", format!("{}", c.cur_rom_bank));
                });

                head(ui, "Compatibility");
                egui::Grid::new("cart_compat").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                    row(ui, "CGB", c.cgb.clone());
                    flag(ui, "SGB", c.sgb);
                });

                head(ui, "Features");
                egui::Grid::new("cart_feat").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                    flag(ui, "Battery", c.battery);
                    flag(ui, "RTC", c.rtc);
                    flag(ui, "Rumble", c.rumble);
                    flag(ui, "Camera", c.camera);
                    flag(ui, "Unlicensed", c.unlicensed);
                });

                head(ui, "Integrity");
                egui::Grid::new("cart_int").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                    row(ui, "CRC32", c.crc32.map_or("—".into(), |v| format!("{v:08X}")));
                    ui.label(RichText::new("Header checksum").color(Color32::GRAY));
                    ui.label(if c.header_checksum_ok {
                        RichText::new("valid").color(Color32::LIGHT_GREEN)
                    } else {
                        RichText::new("BAD").color(Color32::LIGHT_RED)
                    });
                    ui.end_row();
                    row(ui, "Global checksum", format!("{:04X}", c.global_checksum));
                });
            });
    }
}

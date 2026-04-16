use egui::Context;
use rustyboi_session::debug::PpuMode;
use rustyboi_session::DebugSnapshot;
use crate::ui::Gui;

impl Gui {
    pub(in crate) fn render_ppu_debug_panel(&mut self, ctx: &Context, debug: Option<&DebugSnapshot>) {
        if let Some(snap) = debug {
            let ppu = &snap.ppu;
            let mmio = &snap.mmio;
            let pixel_buffer = ppu.fetcher_pixels;
            egui::Window::new("PPU Debug")
                .default_pos([640.0, 50.0])
                .default_size([250.0, 500.0])
                .collapsible(true)
                .resizable(false)
                .frame(egui::Frame::window(&ctx.style()).fill(crate::ui::PANEL_BACKGROUND))
                .show(ctx, |ui| {
                    ui.set_width(230.0);

                    // PPU Status
                    ui.monospace(egui::RichText::new(format!("Disabled: {}", if ppu.disabled { "YES" } else { "NO" }))
                        .color(if ppu.disabled { egui::Color32::LIGHT_RED } else { egui::Color32::LIGHT_GREEN }));

                    let state_str = match ppu.mode {
                        PpuMode::OamSearch => "OAM Search",
                        PpuMode::PixelTransfer => "Pixel Transfer",
                        PpuMode::HBlank => "H-Blank",
                        PpuMode::VBlank => "V-Blank",
                    };
                    ui.monospace(egui::RichText::new(format!("State: {}", state_str)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("Ticks: {}", ppu.ticks)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("Current X: {}", ppu.x)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("Has Frame: {}", if ppu.has_frame { "YES" } else { "NO" }))
                        .color(if ppu.has_frame { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));

                    ui.separator();

                    // MMIO Registers
                    let ly = mmio.ly;
                    let scy = mmio.scy;
                    let lyc = mmio.lyc;
                    let lcd_control = mmio.lcdc;
                    let lcd_status = mmio.stat;

                    ui.monospace(egui::RichText::new(format!("LY: {:02X} ({})", ly, ly)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("LYC: {:02X} ({})", lyc, lyc)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("SCY: {:02X} ({})", scy, scy)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("LCD_CTRL: {:02X}", lcd_control)).color(egui::Color32::WHITE));
                    ui.monospace(egui::RichText::new(format!("LCD_STAT: {:02X}", lcd_status)).color(egui::Color32::WHITE));

                    // Hardware-specific registers
                    if snap.cgb {
                        ui.separator();
                        ui.small(egui::RichText::new("CGB Registers:").color(egui::Color32::LIGHT_GRAY));
                        let vbk = mmio.vbk;
                        let svbk = mmio.svbk;
                        let bcps = mmio.bcps;
                        let ocps = mmio.ocps;

                        ui.monospace(egui::RichText::new(format!("VBK: {:02X} (Bank {})", vbk, vbk & 1)).color(egui::Color32::LIGHT_GREEN));
                        ui.monospace(egui::RichText::new(format!("SVBK: {:02X} (Bank {})", svbk, if svbk & 7 == 0 { 1 } else { svbk & 7 })).color(egui::Color32::LIGHT_GREEN));
                        ui.monospace(egui::RichText::new(format!("BCPS: {:02X} (Addr {:02X})", bcps, bcps & 0x3F)).color(egui::Color32::YELLOW));
                        ui.monospace(egui::RichText::new(format!("OCPS: {:02X} (Addr {:02X})", ocps, ocps & 0x3F)).color(egui::Color32::LIGHT_BLUE));
                    } else {
                        ui.separator();
                        ui.small(egui::RichText::new("DMG Palettes:").color(egui::Color32::LIGHT_GRAY));
                        let bgp = mmio.bgp;
                        let obp0 = mmio.obp0;
                        let obp1 = mmio.obp1;

                        ui.monospace(egui::RichText::new(format!("BGP: {:02X}", bgp)).color(egui::Color32::WHITE));
                        ui.monospace(egui::RichText::new(format!("OBP0: {:02X}", obp0)).color(egui::Color32::LIGHT_BLUE));
                        ui.monospace(egui::RichText::new(format!("OBP1: {:02X}", obp1)).color(egui::Color32::LIGHT_BLUE));
                    }

                    ui.separator();

                    // LCDC Flags
                    ui.small(egui::RichText::new("LCDC Flags:").color(egui::Color32::LIGHT_GRAY));
                    ui.horizontal(|ui| {
                        ui.small(egui::RichText::new(format!("BG: {}", if lcd_control & 0x01 != 0 { "ON" } else { "OFF" }))
                            .color(if lcd_control & 0x01 != 0 { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));
                        ui.small(egui::RichText::new(format!("SPR: {}", if lcd_control & 0x02 != 0 { "ON" } else { "OFF" }))
                            .color(if lcd_control & 0x02 != 0 { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));
                        ui.small(egui::RichText::new(format!("8x{}", if lcd_control & 0x04 != 0 { "16" } else { "8" }))
                            .color(egui::Color32::YELLOW));
                    });

                    ui.separator();

                    // Sprites on current line
                    let sprites_count = ppu.sprites_on_line;
                    ui.small(egui::RichText::new(format!("Sprites on line {}: {}", ly, sprites_count))
                        .color(if sprites_count > 0 { egui::Color32::LIGHT_GREEN } else { egui::Color32::GRAY }));

                    ui.separator();

                    // Pixel Fetcher Buffer
                    ui.small(egui::RichText::new("Fetcher Pixel Buffer:").color(egui::Color32::LIGHT_GRAY));
                    ui.horizontal(|ui| {
                        for (i, &pixel) in pixel_buffer.iter().enumerate() {
                            let color = match pixel {
                                0 => egui::Color32::WHITE,
                                1 => egui::Color32::LIGHT_GRAY,
                                2 => egui::Color32::DARK_GRAY,
                                3 => egui::Color32::BLACK,
                                _ => egui::Color32::RED, // Invalid value
                            };
                            ui.small(egui::RichText::new(format!("{}", pixel)).color(color));
                            if i < 7 { ui.small("|"); }
                        }
                    });

                    ui.separator();
                    ui.small(egui::RichText::new("PPU Debug Information").color(egui::Color32::LIGHT_GRAY));
                });
        }
    }
}

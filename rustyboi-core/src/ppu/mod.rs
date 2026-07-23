pub mod controller;
mod color_mix;
mod fetcher;
mod fifo;
mod frame_out;
mod lcdc;
// PPU-side mode-0 (HBlank) window queries the HDMA engine consults; the engine
// itself is memory::dma::hdma.
mod m0_window;
mod mode3;
mod modes;
mod reads;
mod reg_writes;
mod stat_engine;
mod window_glitch;
mod stat_irq;

pub use controller::{
    ColorCorrection, FetchDebugEvent, FetchDebugEventKind, PixelDebugEvent, Ppu, Sprite, State,
    BGP, FRAMEBUFFER_SIZE, LCD_CONTROL, LCD_STATUS, LY, LYC, OBP0, OBP1, SCX, SCY,
    SgbBorderLayers, SGB_FRAME_HEIGHT, SGB_FRAME_SIZE, SGB_FRAME_WIDTH, WX, WY,
};
pub(crate) use controller::{lcdc_has, LCDCFlags, RenderedFrame};

use crate::ppu;

pub trait Display {
    fn render_frame(&mut self, frame: [u8; ppu::FRAMEBUFFER_SIZE]);
}

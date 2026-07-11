//! `Frame` → packed 32-bit pixel conversion, shared by every frontend adapter.
//!
//! The core returns a [`Frame`] — either monochrome shade indices (0..=3) or
//! RGB888 — and each adapter needs it as 32-bit pixels in its surface's byte
//! order. This is the one place that conversion lives so the desktop, web, and
//! libretro blits can't drift (they previously each inlined the same two-arm
//! match with subtly different byte order and bounds handling).
//!
//! Pure data, WASM-clean: no allocation (the caller supplies the output slice),
//! no host coupling.

use rustyboi_core_lib::gb::Frame;

/// Byte order of the packed 32-bit output. `Rgba` is the wgpu / web-canvas
/// surface order (R, G, B, A); `Bgra` is libretro's `XRGB8888` on a
/// little-endian host (B, G, R, X).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelOrder {
    Rgba,
    Bgra,
}

#[inline]
fn put(out: &mut [u8], o: usize, r: u8, g: u8, b: u8, order: PixelOrder) {
    match order {
        PixelOrder::Rgba => {
            out[o] = r;
            out[o + 1] = g;
            out[o + 2] = b;
        }
        PixelOrder::Bgra => {
            out[o] = b;
            out[o + 1] = g;
            out[o + 2] = r;
        }
    }
    out[o + 3] = 0xFF;
}

/// Pack `frame`'s RGB888 into 32-bit pixels written to `out`, which must be at
/// least `pixels * 4` bytes (160*144 for a GB frame), in the host `order` with
/// opaque alpha. The core now presents an always-RGB frame — the DMG base
/// palette + LCD correction (mono) or the CGB/AGB/SGB colour is already applied
/// — so this is a plain channel expansion, identical for every model.
pub fn frame_to_pixels(frame: &Frame, order: PixelOrder, out: &mut [u8]) {
    rgb_to_pixels(frame.rgb(), order, out);
}

/// Pack an RGB888 buffer (the SGB border composite) into 32-bit pixels written
/// to `out`, which must be at least `rgb.len() / 3 * 4` bytes.
pub fn rgb_to_pixels(rgb: &[u8], order: PixelOrder, out: &mut [u8]) {
    for (i, px) in rgb.chunks_exact(3).enumerate() {
        put(out, i * 4, px[0], px[1], px[2], order);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_to_pixels_expands_rgb_in_both_orders() {
        let mut data = Box::new([0u8; rustyboi_core_lib::ppu::FRAMEBUFFER_SIZE * 3]);
        data[0..3].copy_from_slice(&[0x11, 0x22, 0x33]);
        data[3..6].copy_from_slice(&[0xAA, 0xBB, 0xCC]);
        let frame = Frame(data);

        let mut rgba = vec![0u8; rustyboi_core_lib::ppu::FRAMEBUFFER_SIZE * 4];
        frame_to_pixels(&frame, PixelOrder::Rgba, &mut rgba);
        assert_eq!(&rgba[0..4], &[0x11, 0x22, 0x33, 0xFF]);
        assert_eq!(&rgba[4..8], &[0xAA, 0xBB, 0xCC, 0xFF]);

        let mut bgra = vec![0u8; rustyboi_core_lib::ppu::FRAMEBUFFER_SIZE * 4];
        frame_to_pixels(&frame, PixelOrder::Bgra, &mut bgra);
        assert_eq!(&bgra[0..4], &[0x33, 0x22, 0x11, 0xFF]);
        assert_eq!(&bgra[4..8], &[0xCC, 0xBB, 0xAA, 0xFF]);
    }

    #[test]
    fn rgb_composite_swaps_order() {
        let rgb = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut out = [0u8; 8];
        rgb_to_pixels(&rgb, PixelOrder::Rgba, &mut out);
        assert_eq!(out, [0x10, 0x20, 0x30, 0xFF, 0x40, 0x50, 0x60, 0xFF]);
        rgb_to_pixels(&rgb, PixelOrder::Bgra, &mut out);
        assert_eq!(out, [0x30, 0x20, 0x10, 0xFF, 0x60, 0x50, 0x40, 0xFF]);
    }
}

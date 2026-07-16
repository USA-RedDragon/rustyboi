//! Shared helper for the pixel-grid debug panels (tile explorer, sprite
//! previews).
//!
//! Those panels visualize VRAM/OAM as thousands of Game Boy pixels. Drawing
//! each as its own `rect_filled` emits ~25k+ rects (≈50k triangles) *per
//! frame* — the paint job egui then has to tessellate on the main thread and
//! (on wgpu) re-upload every frame, which drags BOTH the software and GPU
//! backends into single-digit FPS with several panels open.
//!
//! Instead each panel bakes its pixels into a small [`egui::ColorImage`] once
//! per frame and uploads it as a single texture ([`PixelTexture`]); the panel
//! then draws one (or a few) scaled `Image` widgets — two triangles each,
//! nearest-filtered so the Game Boy pixels stay crisp. The per-frame texture
//! upload (tens to ~100 KB) is trivial next to the tessellation it replaces.

use egui::{Color32, ColorImage, Context, TextureHandle, TextureId, TextureOptions};

/// A retained egui texture that a debug panel re-fills each frame from a
/// freshly-baked pixel buffer. The handle persists in the [`Gui`](crate::ui::Gui)
/// so the GPU texture is reused (updated in place) rather than recreated.
#[derive(Default)]
pub(crate) struct PixelTexture {
    handle: Option<TextureHandle>,
}

impl PixelTexture {
    /// Upload `pixels` (row-major, `w`×`h`, opaque `Color32`) and return the
    /// texture id to draw with. Nearest sampling keeps the pixels crisp when
    /// the `Image` widget scales the texture up.
    pub(crate) fn update(
        &mut self,
        ctx: &Context,
        name: &str,
        w: usize,
        h: usize,
        pixels: Vec<Color32>,
    ) -> TextureId {
        debug_assert_eq!(pixels.len(), w * h);
        let image = ColorImage::new([w, h], pixels);
        match &mut self.handle {
            Some(h) => h.set(image, TextureOptions::NEAREST),
            None => self.handle = Some(ctx.load_texture(name, image, TextureOptions::NEAREST)),
        }
        self.handle.as_ref().expect("just set").id()
    }
}

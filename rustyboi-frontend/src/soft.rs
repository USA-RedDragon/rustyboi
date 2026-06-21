//! The self-managed software (CPU) rendering backend — the `Software` graphics
//! backend choice. No GPU driver is initialized at all: the emulator frame is
//! scaled on the CPU, egui's tessellated UI meshes are rasterized on the CPU,
//! and the finished framebuffer is presented through `softbuffer` (shared-memory
//! swapchain on X11/Wayland/Windows/macOS).
//!
//! Split in two so the pixel work is testable without a window:
//! [`SoftCompositor`] does everything into a plain `Vec<u32>` framebuffer
//! (0x00RRGGBB, softbuffer's format), and [`SoftRenderer`] wraps it with the
//! softbuffer surface + the [`Present`] contract the platform drives.
//!
//! Correctness notes mirrored from the wgpu path:
//! - Game placement reuses [`renderer::compute_layout`]'s scissor rect, so both
//!   backends letterbox identically for every [`ScalingMode`].
//! - The LCD grid/scanline effects reproduce `scale.wgsl`'s math (per-texel
//!   fraction → smoothstep gap / mid-row peak), precomputed as per-row and
//!   per-column tables so the blit stays a fetch + multiply per pixel.
//! - egui vertices/textures are premultiplied sRGBA ([`egui::Color32`]); the
//!   wgpu path renders onto a non-sRGB surface with standard
//!   `ONE, ONE_MINUS_SRC_ALPHA` blending, i.e. blending happens in gamma
//!   space — the u8 arithmetic here matches that exactly.
//! - egui's incremental font-atlas deltas MUST be applied on every
//!   non-`reuse` frame, even one that ends up skipped (see `EguiCompositor`'s
//!   rationale in `renderer.rs`); here `apply_textures` runs before any
//!   surface work so the invariant holds trivially.

use crate::renderer::{
    compute_layout, EguiPaint, GameFrame, PhysicalRect, Present, SourceSize,
};
use egui::ClippedPrimitive;
use rustyboi_session::{LcdEffect, ScalingMode, TextureFilter};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use winit::window::Window;

/// One egui texture: straight-off-the-delta premultiplied sRGBA texels plus the
/// sampling filter egui asked for.
struct SoftTexture {
    width: usize,
    height: usize,
    /// Premultiplied sRGBA, row-major.
    pixels: Vec<[u8; 4]>,
    /// Bilinear when egui asked for linear magnification (the font atlas
    /// default), nearest otherwise.
    bilinear: bool,
}

/// CPU compositor: clears, blits the game with filter/effect, rasterizes egui.
/// Owns the egui texture store and the cached paint jobs for `reuse` frames.
pub(crate) struct SoftCompositor {
    textures: HashMap<egui::TextureId, SoftTexture>,
    cached_jobs: Vec<ClippedPrimitive>,
    cached_ppp: f32,
    // Last game frame, retained so ticks without a fresh frame redraw it
    // (mirrors the wgpu path's `has_game` behavior).
    game_rgba: Vec<u8>,
    game_size: Option<SourceSize>,
    pub scaling_mode: ScalingMode,
    pub texture_filter: TextureFilter,
    pub lcd_effect: LcdEffect,
}

impl SoftCompositor {
    pub(crate) fn new() -> Self {
        SoftCompositor {
            textures: HashMap::new(),
            cached_jobs: Vec::new(),
            cached_ppp: 1.0,
            game_rgba: Vec::new(),
            game_size: None,
            scaling_mode: ScalingMode::FitAspect,
            texture_filter: TextureFilter::Nearest,
            lcd_effect: LcdEffect::Off,
        }
    }

    /// Apply egui's incremental texture allocations/updates.
    fn apply_textures(&mut self, deltas: &egui::TexturesDelta) {
        for (id, delta) in &deltas.set {
            let egui::epaint::ImageData::Color(image) = &delta.image;
            let (w, h) = (image.size[0], image.size[1]);
            let src: Vec<[u8; 4]> = image.pixels.iter().map(|c| c.to_array()).collect();
            let bilinear =
                delta.options.magnification == egui::TextureFilter::Linear;
            match delta.pos {
                None => {
                    self.textures.insert(
                        *id,
                        SoftTexture { width: w, height: h, pixels: src, bilinear },
                    );
                }
                Some([x, y]) => {
                    if let Some(t) = self.textures.get_mut(id) {
                        for row in 0..h {
                            let dst_row = y + row;
                            if dst_row >= t.height {
                                break;
                            }
                            let dst_start = dst_row * t.width + x;
                            let n = w.min(t.width.saturating_sub(x));
                            t.pixels[dst_start..dst_start + n]
                                .copy_from_slice(&src[row * w..row * w + n]);
                        }
                    }
                }
            }
        }
    }

    fn free_textures(&mut self, deltas: &egui::TexturesDelta) {
        for id in &deltas.free {
            self.textures.remove(id);
        }
    }

    /// Store the latest game frame (copied: the borrow ends at frame_tick).
    fn upload_game(&mut self, frame: &GameFrame) {
        self.game_rgba.clear();
        self.game_rgba.extend_from_slice(frame.rgba);
        self.game_size = Some(frame.size);
    }

    /// Full-frame composite into `fb` (`0x00RRGGBB`, `w`×`h` physical pixels).
    pub(crate) fn compose(
        &mut self,
        fb: &mut [u32],
        w: u32,
        h: u32,
        game: Option<&GameFrame>,
        region: PhysicalRect,
        egui: EguiPaint,
    ) {
        if let Some(frame) = game {
            self.upload_game(frame);
        }
        let EguiPaint { jobs, textures, pixels_per_point, reuse } = egui;
        if !reuse {
            self.apply_textures(&textures);
            self.cached_jobs = jobs;
            self.cached_ppp = pixels_per_point;
        }

        fb.fill(0); // clear to black, mirroring the wgpu path's clear color

        if let Some(size) = self.game_size {
            let (tw, th) = size.dimensions();
            let (_, scissor) = compute_layout(
                (tw as f32, th as f32),
                (w as f32, h as f32),
                region,
                self.scaling_mode,
            );
            self.blit_game(fb, w, h, (tw, th), scissor);
        }

        let jobs = std::mem::take(&mut self.cached_jobs);
        for prim in &jobs {
            if let egui::epaint::Primitive::Mesh(mesh) = &prim.primitive {
                self.raster_mesh(fb, w, h, mesh, prim.clip_rect, self.cached_ppp);
            }
        }
        self.cached_jobs = jobs;

        self.free_textures(&textures);
    }

    /// Blit the retained game frame into `dst` = (x, y, w, h), applying the
    /// texture filter and LCD effect.
    ///
    /// Structure: everything expensive happens per SOURCE texel, not per dst
    /// pixel. Each dst row first builds a `tw`-wide "texel row" (y-blended for
    /// Linear, straight for Nearest, row effect factor folded in) and then
    /// expands it to the dst width — span fills for Nearest, cached-pair lerps
    /// for Linear. The default path (Nearest, no effect) additionally reuses
    /// identical rows with a single `copy_within`.
    ///
    /// The LCD grid is pixel-based here, NOT the shader's texel-fraction
    /// smoothstep: at exact integer scales the fraction lattice never lands in
    /// the shader's 10% edge band and the grid silently vanishes (a real bug —
    /// the window auto-resize snaps the game to integer scale). Instead the
    /// LAST dst pixel/row of every source texel is dimmed to 80%, which is the
    /// shader's intent (a thin gap between cells) at every scale.
    fn blit_game(
        &self,
        fb: &mut [u32],
        fb_w: u32,
        fb_h: u32,
        (tw, th): (u32, u32),
        dst: (u32, u32, u32, u32),
    ) {
        let (dx, dy, dw, dh) = dst;
        if dw == 0 || dh == 0 || self.game_rgba.len() < (tw * th * 4) as usize {
            return;
        }
        // Per-axis source positions in 16.16 fixed point, sampled at the dst
        // pixel center (matches the GPU sampler's texel addressing).
        let step_x = ((tw as u64) << 16) / dw as u64;
        let step_y = ((th as u64) << 16) / dh as u64;
        let src = &self.game_rgba;

        // Clamp the blit to the framebuffer once so the inner loops carry no
        // per-pixel bounds checks.
        let dwc = dw.min(fb_w.saturating_sub(dx)) as usize;
        let dhc = dh.min(fb_h.saturating_sub(dy));
        if dwc == 0 || dhc == 0 {
            return;
        }

        // Per-column source-texel index (nearest lattice). Grid edge columns =
        // the last dst pixel of each texel (where the index steps).
        let col_nx: Vec<u32> = (0..dwc)
            .map(|c| (((c as u64 * step_x + step_x / 2) >> 16) as u32).min(tw - 1))
            .collect();
        let grid = self.lcd_effect == LcdEffect::Grid;
        let scan = self.lcd_effect == LcdEffect::Scanlines;
        let col_edge: Vec<bool> = if grid {
            (0..dwc)
                .map(|c| c + 1 == dwc || col_nx[c] != col_nx[c + 1])
                .collect()
        } else {
            Vec::new()
        };
        // Scanline row factor (0..=256), from the shader: 1 - 0.4*|f-0.5|*2.
        let row_scan: Vec<u32> = if scan {
            (0..dhc)
                .map(|y| {
                    let f = ((((y as u64) * step_y + step_y / 2) & 0xFFFF) as f32) / 65536.0;
                    ((1.0 - 0.40 * (f - 0.5).abs() * 2.0) * 256.0) as u32
                })
                .collect()
        } else {
            Vec::new()
        };
        // Grid row edges: the last dst row of each source texel row.
        let row_sy = |row: u32| ((row as u64 * step_y + step_y / 2) >> 16) as u32;
        let bilinear = self.texture_filter == TextureFilter::Linear;

        // Scratch texel row: final per-texel colors for the current dst row
        // (y-blend + row effect folded in), expanded below.
        let mut trow: Vec<(u32, u32, u32)> = vec![(0, 0, 0); tw as usize];

        let mut prev_key: Option<(u32, u32, usize)> = None; // (sy, row_mul, fb row start)
        for row in 0..dhc {
            let fy = row as u64 * step_y + step_y / 2;
            let sy0 = ((fy >> 16) as u32).min(th - 1);
            let out_start = ((dy + row) * fb_w + dx) as usize;

            // Row multiplier: scanline factor, or the grid row-edge dim; 256 =
            // identity. (Grid column edges are applied during expansion.)
            let row_edge = grid && (row + 1 == dhc || row_sy(row + 1) != sy0);
            let rm: u32 = if scan {
                row_scan[row as usize]
            } else if row_edge {
                205
            } else {
                256
            };

            // Rows with identical inputs are pure repeats — one memcpy.
            if !bilinear
                && let Some((psy, prm, pstart)) = prev_key
                && psy == sy0
                && prm == rm
            {
                fb.copy_within(pstart..pstart + dwc, out_start);
                prev_key = Some((sy0, rm, out_start));
                continue;
            }

            // Build the texel row (per-texel work: tw items, not dwc).
            if bilinear {
                // y-blend the two neighbouring source rows once per texel.
                let cy = fy.saturating_sub(1 << 15);
                let y0 = ((cy >> 16) as u32).min(th - 1);
                let y1 = (y0 + 1).min(th - 1);
                let wy = ((cy & 0xFFFF) >> 8) as i32;
                let row0 = &src[(y0 * tw * 4) as usize..][..(tw * 4) as usize];
                let row1 = &src[(y1 * tw * 4) as usize..][..(tw * 4) as usize];
                for (t, out) in trow.iter_mut().enumerate() {
                    let i = t * 4;
                    let blend = |a: u8, b: u8| -> u32 {
                        (a as i32 + (((b as i32 - a as i32) * wy) >> 8)) as u32
                    };
                    let (mut r, mut g, mut b) = (
                        blend(row0[i], row1[i]),
                        blend(row0[i + 1], row1[i + 1]),
                        blend(row0[i + 2], row1[i + 2]),
                    );
                    if rm != 256 {
                        r = (r * rm) >> 8;
                        g = (g * rm) >> 8;
                        b = (b * rm) >> 8;
                    }
                    *out = (r, g, b);
                }
            } else {
                let src_row = &src[(sy0 * tw * 4) as usize..][..(tw * 4) as usize];
                for (t, out) in trow.iter_mut().enumerate() {
                    let i = t * 4;
                    let (mut r, mut g, mut b) =
                        (src_row[i] as u32, src_row[i + 1] as u32, src_row[i + 2] as u32);
                    if rm != 256 {
                        r = (r * rm) >> 8;
                        g = (g * rm) >> 8;
                        b = (b * rm) >> 8;
                    }
                    *out = (r, g, b);
                }
            }

            // Expand the texel row to the dst row.
            let out_row = &mut fb[out_start..out_start + dwc];
            if bilinear {
                // x-lerp between cached neighbouring texels; the weight
                // accumulator advances by a constant per pixel. The first
                // pixel's source coordinate is step_x/2 - 0.5 texels (GPU
                // linear-sampler addressing), which can be negative (clamped).
                let mut acc = (step_x / 2) as i64 - (1 << 15);
                let mut x0 = -1i64; // current left texel (-1 = clamped edge)
                let (mut c0, mut c1) = (trow[0], trow[0]);
                for out in out_row.iter_mut() {
                    let nx0 = if acc < 0 { -1 } else { (acc >> 16).min(tw as i64 - 1) };
                    if nx0 != x0 {
                        x0 = nx0;
                        let l = x0.max(0) as usize;
                        let r = ((x0 + 1).max(0) as usize).min(tw as usize - 1);
                        c0 = trow[l];
                        c1 = trow[r];
                    }
                    let wx = if acc < 0 { 0 } else { ((acc & 0xFFFF) >> 8) as i32 };
                    let lerp = |a: u32, b: u32| -> u32 {
                        (a as i32 + (((b as i32 - a as i32) * wx) >> 8)) as u32
                    };
                    let (r, g, b) = (lerp(c0.0, c1.0), lerp(c0.1, c1.1), lerp(c0.2, c1.2));
                    *out = (r << 16) | (g << 8) | b;
                    acc += step_x as i64;
                }
            } else {
                // Span-fill each texel's run of dst pixels.
                let mut c = 0usize;
                while c < dwc {
                    let sx = col_nx[c];
                    let mut end = c + 1;
                    while end < dwc && col_nx[end] == sx {
                        end += 1;
                    }
                    let (r, g, b) = trow[sx as usize];
                    let px = (r << 16) | (g << 8) | b;
                    out_row[c..end].fill(px);
                    c = end;
                }
            }

            // Grid column edges: dim the boundary pixel of every texel.
            if grid {
                for (c, out) in out_row.iter_mut().enumerate() {
                    if col_edge[c] {
                        let v = *out;
                        let r = (((v >> 16) & 0xFF) * 205) >> 8;
                        let g = (((v >> 8) & 0xFF) * 205) >> 8;
                        let b = ((v & 0xFF) * 205) >> 8;
                        *out = (r << 16) | (g << 8) | b;
                    }
                }
            }

            prev_key = Some((sy0, rm, out_start));
        }
    }

    /// Rasterize one egui mesh: textured triangles, premultiplied sRGBA,
    /// `ONE, ONE_MINUS_SRC_ALPHA` blending in gamma space (matching the wgpu
    /// path on a non-sRGB surface), scissored by `clip_rect`.
    fn raster_mesh(
        &self,
        fb: &mut [u32],
        fb_w: u32,
        fb_h: u32,
        mesh: &egui::epaint::Mesh,
        clip_rect: egui::Rect,
        ppp: f32,
    ) {
        let Some(tex) = self.textures.get(&mesh.texture_id) else { return };
        // Clip rect: points → physical pixels, clamped to the framebuffer.
        let cx0 = ((clip_rect.min.x * ppp).floor().max(0.0)) as i64;
        let cy0 = ((clip_rect.min.y * ppp).floor().max(0.0)) as i64;
        let cx1 = ((clip_rect.max.x * ppp).ceil()).min(fb_w as f32) as i64;
        let cy1 = ((clip_rect.max.y * ppp).ceil()).min(fb_h as f32) as i64;
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }

        for tri in mesh.indices.chunks_exact(3) {
            let v = [
                &mesh.vertices[tri[0] as usize],
                &mesh.vertices[tri[1] as usize],
                &mesh.vertices[tri[2] as usize],
            ];
            let p: Vec<(f32, f32)> = v.iter().map(|v| (v.pos.x * ppp, v.pos.y * ppp)).collect();
            // Signed doubled area; sign gives the winding, near-zero is
            // degenerate. egui emits both windings, so normalize by sign.
            let area = (p[1].0 - p[0].0) * (p[2].1 - p[0].1)
                - (p[1].1 - p[0].1) * (p[2].0 - p[0].0);
            if area.abs() < 1e-6 {
                continue;
            }
            let inv_area = 1.0 / area;

            let min_x = p.iter().map(|q| q.0).fold(f32::MAX, f32::min).floor() as i64;
            let max_x = p.iter().map(|q| q.0).fold(f32::MIN, f32::max).ceil() as i64;
            let min_y = p.iter().map(|q| q.1).fold(f32::MAX, f32::min).floor() as i64;
            let max_y = p.iter().map(|q| q.1).fold(f32::MIN, f32::max).ceil() as i64;
            let (min_x, max_x) = (min_x.max(cx0), max_x.min(cx1));
            let (min_y, max_y) = (min_y.max(cy0), max_y.min(cy1));

            // Incremental rasterization: the normalized barycentric weights are
            // affine in x/y, so every interpolated quantity (weights, UV,
            // color) advances by a constant per step — one add each per pixel
            // instead of re-evaluating the barycentric form.
            //
            // d(w_i)/dx and /dy from the edge functions, normalized by area:
            let dw0 = ((p[1].1 - p[2].1) * inv_area, (p[2].0 - p[1].0) * inv_area);
            let dw1 = ((p[2].1 - p[0].1) * inv_area, (p[0].0 - p[2].0) * inv_area);
            // Weights at the top-left sample point (min_x+0.5, min_y+0.5).
            let (sx0, sy0) = (min_x as f32 + 0.5, min_y as f32 + 0.5);
            let w0_at = |x: f32, y: f32| {
                ((p[1].0 - x) * (p[2].1 - y) - (p[1].1 - y) * (p[2].0 - x)) * inv_area
            };
            let w1_at = |x: f32, y: f32| {
                ((p[2].0 - x) * (p[0].1 - y) - (p[2].1 - y) * (p[0].0 - x)) * inv_area
            };
            let (mut w0_row, mut w1_row) = (w0_at(sx0, sy0), w1_at(sx0, sy0));

            // Per-attribute value at the top-left sample + d/dx + d/dy, all via
            // attr = w0*a0 + w1*a1 + (1-w0-w1)*a2 = a2 + w0*(a0-a2) + w1*(a1-a2).
            let attr = |a0: f32, a1: f32, a2: f32| {
                (
                    a2 + w0_row * (a0 - a2) + w1_row * (a1 - a2),
                    dw0.0 * (a0 - a2) + dw1.0 * (a1 - a2), // d/dx
                    dw0.1 * (a0 - a2) + dw1.1 * (a1 - a2), // d/dy
                )
            };
            let vc = |i: usize, ch: usize| v[i].color[ch] as f32;
            let (u_v, u_dx, u_dy) = attr(v[0].uv.x, v[1].uv.x, v[2].uv.x);
            let (v_v, v_dx, v_dy) = attr(v[0].uv.y, v[1].uv.y, v[2].uv.y);
            let (r_v, r_dx, r_dy) = attr(vc(0, 0), vc(1, 0), vc(2, 0));
            let (g_v, g_dx, g_dy) = attr(vc(0, 1), vc(1, 1), vc(2, 1));
            let (b_v, b_dx, b_dy) = attr(vc(0, 2), vc(1, 2), vc(2, 2));
            let (a_v, a_dx, a_dy) = attr(vc(0, 3), vc(1, 3), vc(2, 3));
            let (mut u_row, mut v_row) = (u_v, v_v);
            let (mut r_row, mut g_row, mut b_row, mut a_row) = (r_v, g_v, b_v, a_v);

            for py in min_y..max_y {
                let (mut w0, mut w1) = (w0_row, w1_row);
                let (mut uu, mut vv) = (u_row, v_row);
                let (mut cr, mut cg, mut cb, mut ca) = (r_row, g_row, b_row, a_row);
                let row_base = (py as u32 * fb_w) as usize;
                for px in min_x..max_x {
                    let w2 = 1.0 - w0 - w1;
                    if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                        let [tr, tg, tb, ta] = sample(tex, uu, vv);
                        // Modulate premultiplied texel × premultiplied vertex
                        // color, then src-over in gamma space.
                        let sr = (tr as f32 * cr) / 255.0;
                        let sg = (tg as f32 * cg) / 255.0;
                        let sb = (tb as f32 * cb) / 255.0;
                        let sa = (ta as f32 * ca) / 255.0;
                        let di = row_base + px as usize;
                        let d = fb[di];
                        let inv = 1.0 - sa / 255.0;
                        let dr = (sr + ((d >> 16) & 0xFF) as f32 * inv).min(255.0) as u32;
                        let dg = (sg + ((d >> 8) & 0xFF) as f32 * inv).min(255.0) as u32;
                        let db = (sb + (d & 0xFF) as f32 * inv).min(255.0) as u32;
                        fb[di] = (dr << 16) | (dg << 8) | db;
                    }
                    w0 += dw0.0;
                    w1 += dw1.0;
                    uu += u_dx;
                    vv += v_dx;
                    cr += r_dx;
                    cg += g_dx;
                    cb += b_dx;
                    ca += a_dx;
                }
                w0_row += dw0.1;
                w1_row += dw1.1;
                u_row += u_dy;
                v_row += v_dy;
                r_row += r_dy;
                g_row += g_dy;
                b_row += b_dy;
                a_row += a_dy;
            }
        }
    }
}

/// Sample an egui texture at normalized UV. Bilinear (the atlas default) or
/// nearest per the texture's egui options; clamp addressing.
fn sample(tex: &SoftTexture, u: f32, v: f32) -> [u8; 4] {
    let (w, h) = (tex.width, tex.height);
    if w == 0 || h == 0 {
        return [0; 4];
    }
    let fx = (u * w as f32 - 0.5).max(0.0);
    let fy = (v * h as f32 - 0.5).max(0.0);
    if !tex.bilinear {
        let x = (fx.round() as usize).min(w - 1);
        let y = (fy.round() as usize).min(h - 1);
        return tex.pixels[y * w + x];
    }
    let x0 = (fx as usize).min(w - 1);
    let y0 = (fy as usize).min(h - 1);
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let px = |x: usize, y: usize| tex.pixels[y * w + x];
    let (p00, p10, p01, p11) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0u8; 4];
    for (ch, o) in out.iter_mut().enumerate() {
        let top = p00[ch] as f32 * (1.0 - tx) + p10[ch] as f32 * tx;
        let bot = p01[ch] as f32 * (1.0 - tx) + p11[ch] as f32 * tx;
        *o = (top * (1.0 - ty) + bot * ty) as u8;
    }
    out
}

/// The windowed software backend: [`SoftCompositor`] + a softbuffer surface.
pub struct SoftRenderer {
    // Field order = drop order: the surface must not outlive the context.
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    _context: softbuffer::Context<Arc<Window>>,
    width: u32,
    height: u32,
    compositor: SoftCompositor,
}

impl SoftRenderer {
    /// Build the software swapchain over the platform's window. `width`/
    /// `height` are the window's physical pixel size.
    pub fn new(window: Arc<Window>, width: u32, height: u32) -> Result<Self, String> {
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| format!("softbuffer context: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window)
            .map_err(|e| format!("softbuffer surface: {e}"))?;
        Ok(SoftRenderer {
            surface,
            _context: context,
            width: width.max(1),
            height: height.max(1),
            compositor: SoftCompositor::new(),
        })
    }
}

impl Present for SoftRenderer {
    fn surface_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    fn set_scaling_mode(&mut self, mode: ScalingMode) {
        self.compositor.scaling_mode = mode;
    }

    fn set_texture_filter(&mut self, filter: TextureFilter) {
        self.compositor.texture_filter = filter;
    }

    fn set_lcd_effect(&mut self, effect: LcdEffect) {
        self.compositor.lcd_effect = effect;
    }

    fn render(
        &mut self,
        game: Option<&GameFrame>,
        region: PhysicalRect,
        egui: EguiPaint,
    ) -> Result<(), wgpu::SurfaceStatus> {
        // A failed resize/present is a skipped frame, never a hard error: the
        // platform loop keeps ticking and the next frame retries (softbuffer
        // has no Lost/Outdated protocol to recover from).
        let (Some(w), Some(h)) = (NonZeroU32::new(self.width), NonZeroU32::new(self.height))
        else {
            return Ok(());
        };
        if self.surface.resize(w, h).is_err() {
            return Ok(());
        }
        let Ok(mut buffer) = self.surface.buffer_mut() else {
            return Ok(());
        };
        self.compositor
            .compose(&mut buffer, self.width, self.height, game, region, egui);
        let _ = buffer.present();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::{Mesh, Vertex};
    use egui::{Color32, Pos2, Rect, TextureId};

    fn white_tex_compositor() -> SoftCompositor {
        let mut c = SoftCompositor::new();
        c.textures.insert(
            TextureId::default(),
            SoftTexture {
                width: 2,
                height: 2,
                pixels: vec![[255, 255, 255, 255]; 4],
                bilinear: false,
            },
        );
        c
    }

    fn tri(color: Color32) -> Mesh {
        let mut m = Mesh::default();
        let v = |x: f32, y: f32| Vertex {
            pos: Pos2::new(x, y),
            uv: Pos2::new(0.5, 0.5),
            color,
        };
        // Covers the whole 4x4 left-top half generously.
        m.vertices = vec![v(0.0, 0.0), v(8.0, 0.0), v(0.0, 8.0)];
        m.indices = vec![0, 1, 2];
        m
    }

    #[test]
    fn opaque_triangle_fills_inside_and_respects_clip() {
        let c = white_tex_compositor();
        let mut fb = vec![0u32; 8 * 8];
        let mesh = tri(Color32::from_rgb(255, 0, 0));
        c.raster_mesh(&mut fb, 8, 8, &mesh, Rect::from_min_max(Pos2::ZERO, Pos2::new(8.0, 8.0)), 1.0);
        assert_eq!(fb[0], 0xFF0000, "inside the triangle");
        assert_eq!(fb[7 * 8 + 7], 0, "outside the hypotenuse stays clear");

        // Same triangle, clip to the top-left 2x2: nothing outside may change.
        let mut fb2 = vec![0u32; 8 * 8];
        c.raster_mesh(&mut fb2, 8, 8, &mesh, Rect::from_min_max(Pos2::ZERO, Pos2::new(2.0, 2.0)), 1.0);
        assert_eq!(fb2[0], 0xFF0000);
        assert_eq!(fb2[4], 0, "clipped right");
        assert_eq!(fb2[4 * 8], 0, "clipped below");
    }

    #[test]
    fn winding_does_not_matter() {
        let c = white_tex_compositor();
        let mut m = tri(Color32::WHITE);
        m.indices = vec![2, 1, 0]; // reversed winding
        let mut fb = vec![0u32; 8 * 8];
        c.raster_mesh(&mut fb, 8, 8, &m, Rect::from_min_max(Pos2::ZERO, Pos2::new(8.0, 8.0)), 1.0);
        assert_eq!(fb[0], 0xFFFFFF);
    }

    #[test]
    fn alpha_blends_over_background() {
        let c = white_tex_compositor();
        // 50% premultiplied gray over a mid-gray background.
        let mesh = tri(Color32::from_rgba_premultiplied(128, 128, 128, 128));
        let mut fb = vec![0x00808080u32; 8 * 8];
        c.raster_mesh(&mut fb, 8, 8, &mesh, Rect::from_min_max(Pos2::ZERO, Pos2::new(8.0, 8.0)), 1.0);
        // src + dst*(1-a) = 128 + 128*(1-0.502) ≈ 191
        let px = fb[0];
        let r = (px >> 16) & 0xFF;
        assert!((190..=193).contains(&r), "blended value ≈191, got {r}");
    }

    #[test]
    fn game_blit_letterboxes_and_scales_nearest() {
        let mut c = SoftCompositor::new();
        // 2x2 source: R G / B W, presented into an 8x4 region → FitAspect
        // gives a 4x4 blit centered horizontally (x=2).
        let src: Vec<u8> = [
            [255u8, 0, 0, 255], [0, 255, 0, 255],
            [0, 0, 255, 255], [255, 255, 255, 255],
        ]
        .concat();
        c.game_rgba = src;
        c.game_size = Some(SourceSize::Gb); // size ignored; we pass dims below
        let mut fb = vec![0u32; 8 * 4];
        c.blit_game(&mut fb, 8, 4, (2, 2), (2, 0, 4, 4));
        assert_eq!(fb[0], 0, "letterbox left stays clear");
        assert_eq!(fb[2], 0xFF0000, "top-left quadrant = red");
        assert_eq!(fb[5], 0x00FF00, "top-right quadrant = green");
        assert_eq!(fb[3 * 8 + 2], 0x0000FF, "bottom-left quadrant = blue");
        assert_eq!(fb[3 * 8 + 5], 0xFFFFFF, "bottom-right quadrant = white");
    }

    // Regression: the shader-mirrored grid (texel-fraction smoothstep) is
    // invisible at exact integer scales — the fraction lattice never lands in
    // the 10% edge band (at 5x it samples fractions {0.1,0.3,0.5,0.7,0.9},
    // and smoothstep(0,0.1,0.1) == 1.0 exactly). The desktop window auto-sizes
    // the game to an integer scale, so this was the common case, not an edge
    // case. The pixel-based grid must dim the boundary pixel of every texel at
    // ANY scale.
    #[test]
    fn grid_is_visible_at_exact_integer_scale() {
        let mut c = SoftCompositor::new();
        c.lcd_effect = LcdEffect::Grid;
        // 2x2 white source at exactly 5x: 10x10 dst.
        c.game_rgba = vec![255u8; 2 * 2 * 4];
        c.game_size = Some(SourceSize::Gb);
        let mut fb = vec![0u32; 10 * 10];
        c.blit_game(&mut fb, 10, 10, (2, 2), (0, 0, 10, 10));
        let white = 0xFFFFFF;
        let dim = |v: u32| v != white && v != 0;
        // Texel interiors stay full white; the boundary pixel of each texel
        // (columns 4 and 9, rows 4 and 9) is dimmed.
        assert_eq!(fb[0], white, "interior pixel untouched");
        assert!(dim(fb[4]), "column texel boundary is dimmed");
        assert!(dim(fb[9]), "right-edge boundary is dimmed");
        assert!(dim(fb[4 * 10 + 1]), "row texel boundary is dimmed");
        assert!(dim(fb[9 * 10 + 1]), "bottom-edge boundary is dimmed");
        // And there IS a full-brightness interior (the effect is a grid, not a
        // uniform dim).
        let n_white = fb.iter().filter(|&&v| v == white).count();
        let n_dim = fb.iter().filter(|&&v| dim(v)).count();
        assert_eq!(n_white + n_dim, 100);
        assert!(n_white >= 60 && n_dim >= 30, "white={n_white} dim={n_dim}");
    }

    // The scanline effect must vary WITHIN each source texel row (that is what
    // distinguishes it from a uniform dim), at exact integer scale.
    #[test]
    fn scanlines_vary_within_texel_rows_at_integer_scale() {
        let mut c = SoftCompositor::new();
        c.lcd_effect = LcdEffect::Scanlines;
        c.game_rgba = vec![255u8; 2 * 2 * 4];
        c.game_size = Some(SourceSize::Gb);
        let mut fb = vec![0u32; 10 * 10];
        c.blit_game(&mut fb, 10, 10, (2, 2), (0, 0, 10, 10));
        // Rows 0..5 map to source row 0; brightness must differ across them.
        let row_vals: Vec<u32> = (0..5).map(|r| fb[r * 10] & 0xFF).collect();
        assert!(
            row_vals.iter().any(|&v| v != row_vals[0]),
            "scanline factor must vary within a texel row: {row_vals:?}"
        );
    }

    // The restructured Linear path (per-texel y-blend + cached-pair x-lerp)
    // must produce exactly the classic per-pixel bilinear result.
    #[test]
    fn linear_matches_reference_bilinear() {
        let mut c = SoftCompositor::new();
        c.texture_filter = TextureFilter::Linear;
        let (tw, th) = (4u32, 3u32);
        let mut x = 0x243f6a8885a308d3u64;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        };
        c.game_rgba = (0..tw * th * 4).map(|_| rnd()).collect();
        c.game_size = Some(SourceSize::Gb);
        let (dw, dh) = (13u32, 9u32); // fractional scale on both axes
        let mut fb = vec![0u32; (dw * dh) as usize];
        c.blit_game(&mut fb, dw, dh, (tw, th), (0, 0, dw, dh));

        // Reference: naive per-pixel bilinear with the same fixed-point math.
        let src = &c.game_rgba;
        let step_x = ((tw as u64) << 16) / dw as u64;
        let step_y = ((th as u64) << 16) / dh as u64;
        for py in 0..dh {
            let cy = (py as u64 * step_y + step_y / 2).saturating_sub(1 << 15);
            let y0 = ((cy >> 16) as u32).min(th - 1);
            let y1 = (y0 + 1).min(th - 1);
            let wy = ((cy & 0xFFFF) >> 8) as i32;
            for px in 0..dw {
                let fx = px as u64 * step_x + step_x / 2;
                let (x0, x1, wx) = if fx < (1 << 15) {
                    (0u32, 0u32, 0i32)
                } else {
                    let cx = fx - (1 << 15);
                    let x0 = ((cx >> 16) as u32).min(tw - 1);
                    (x0, (x0 + 1).min(tw - 1), ((cx & 0xFFFF) >> 8) as i32)
                };
                let at = |xx: u32, yy: u32, o: u32| src[((yy * tw + xx) * 4 + o) as usize] as i32;
                let mut expect = 0u32;
                // Same separable order as the implementation (y-blend per
                // texel, then x-lerp) — fixed-point truncation makes the two
                // orders differ by ±1, so the reference must match the order,
                // not just the math.
                for (shift, o) in [(16u32, 0u32), (8, 1), (0, 2)] {
                    let left = at(x0, y0, o) + (((at(x0, y1, o) - at(x0, y0, o)) * wy) >> 8);
                    let right = at(x1, y0, o) + (((at(x1, y1, o) - at(x1, y0, o)) * wy) >> 8);
                    let v = (left + (((right - left) * wx) >> 8)) as u32;
                    expect |= v << shift;
                }
                assert_eq!(
                    fb[(py * dw + px) as usize],
                    expect,
                    "bilinear mismatch at ({px},{py})"
                );
            }
        }
    }

    #[test]
    fn compose_reuse_redraws_cached_jobs() {
        let mut c = white_tex_compositor();
        let mesh = tri(Color32::WHITE);
        let jobs = vec![ClippedPrimitive {
            clip_rect: Rect::from_min_max(Pos2::ZERO, Pos2::new(8.0, 8.0)),
            primitive: egui::epaint::Primitive::Mesh(mesh),
        }];
        let mut fb = vec![0u32; 8 * 8];
        let region = PhysicalRect { x: 0.0, y: 0.0, width: 8.0, height: 8.0 };
        let paint = |jobs, reuse| EguiPaint {
            jobs,
            textures: egui::TexturesDelta::default(),
            pixels_per_point: 1.0,
            reuse,
        };
        c.compose(&mut fb, 8, 8, None, region, paint(jobs, false));
        assert_eq!(fb[0], 0xFFFFFF);
        // Reuse frame: empty jobs, cached geometry must still draw.
        fb.fill(0);
        c.compose(&mut fb, 8, 8, None, region, paint(Vec::new(), true));
        assert_eq!(fb[0], 0xFFFFFF, "reuse frame redraws cached UI");
    }
}

#[cfg(test)]
mod perf_probe {
    use super::*;
    use egui::epaint::{Mesh, Primitive, Vertex};
    use egui::{Color32, Pos2, Rect, TextureId};
    use std::time::Instant;

    /// Synthetic menu-bar-ish egui workload: ~n glyph-sized textured quads.
    fn glyph_jobs(n: usize, _ppp: f32) -> Vec<ClippedPrimitive> {
        let mut mesh = Mesh::default();
        for i in 0..n {
            let x = (i % 100) as f32 * 9.0;
            let y = (i / 100) as f32 * 16.0;
            let base = mesh.vertices.len() as u32;
            let v = |px: f32, py: f32, u: f32, vv: f32| Vertex {
                pos: Pos2::new(px, py),
                uv: Pos2::new(u, vv),
                color: Color32::from_rgba_premultiplied(200, 200, 200, 255),
            };
            mesh.vertices.extend([
                v(x, y, 0.0, 0.0),
                v(x + 8.0, y, 1.0, 0.0),
                v(x + 8.0, y + 14.0, 1.0, 1.0),
                v(x, y + 14.0, 0.0, 1.0),
            ]);
            mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        vec![ClippedPrimitive {
            clip_rect: Rect::from_min_max(Pos2::ZERO, Pos2::new(2000.0, 2000.0)),
            primitive: Primitive::Mesh(mesh),
        }]
    }

    // TEMP-ish probe: print per-piece timings at representative sizes.
    // Run: cargo test -p rustyboi-frontend --release soft_timings -- --nocapture --ignored
    #[test]
    #[ignore = "timing probe, run explicitly with --release"]
    fn soft_timings() {
        let mut c = SoftCompositor::new();
        // Game frame: 160x144 noise RGBA.
        let mut x = 0x9e3779b97f4a7c15u64;
        let mut rnd = || { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x as u8 };
        let src: Vec<u8> = (0..160 * 144 * 4).map(|_| rnd()).collect();
        c.game_rgba = src;
        c.game_size = Some(SourceSize::Gb);
        c.textures.insert(
            TextureId::default(),
            SoftTexture { width: 128, height: 64, pixels: vec![[255; 4]; 128 * 64], bilinear: true },
        );

        for (label, w, h) in [("1000x900 (5x)", 1000u32, 900u32), ("2560x1380 (max)", 2560, 1380)] {
            let mut fb = vec![0u32; (w * h) as usize];
            let dst = (0, 0, (w / 160) * 160, (h / 144) * 144); // integer-ish full blit
            for filter in [TextureFilter::Nearest, TextureFilter::Linear] {
                for effect in [LcdEffect::Off, LcdEffect::Grid, LcdEffect::Scanlines] {
                    c.texture_filter = filter;
                    c.lcd_effect = effect;
                    let t = Instant::now();
                    for _ in 0..20 {
                        c.blit_game(&mut fb, w, h, (160, 144), dst);
                    }
                    eprintln!("BLIT {label} {filter:?}/{effect:?}: {:?}", t.elapsed() / 20);
                }
            }
            let t = Instant::now();
            for _ in 0..20 { fb.fill(0); }
            eprintln!("CLEAR {label}: {:?}", t.elapsed() / 20);
            // egui: 300 glyphs (busy menu bar + a panel worth of text)
            let jobs = glyph_jobs(300, 1.0);
            let t = Instant::now();
            for _ in 0..20 {
                for p in &jobs {
                    if let Primitive::Mesh(m) = &p.primitive {
                        c.raster_mesh(&mut fb, w, h, m, p.clip_rect, 1.0);
                    }
                }
            }
            eprintln!("EGUI-300-glyphs {label}: {:?}", t.elapsed() / 20);
        }
    }
}

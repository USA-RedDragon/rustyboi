// Vertex shader bindings

struct VertexOutput {
    @location(0) tex_coord: vec2<f32>,
    @builtin(position) position: vec4<f32>,
}

struct Locals {
    transform: mat4x4<f32>,
    // Active source texture size in texels (for the LCD-effect texel math).
    source_size: vec2<f32>,
    // 0 = off, 1 = LCD grid, 2 = scanlines.
    effect: u32,
    _pad: u32,
}
@group(0) @binding(2) var<uniform> r_locals: Locals;

@vertex
fn vs_main(
    @location(0) position: vec2<f32>,
) -> VertexOutput {
    var out: VertexOutput;
    out.tex_coord = fma(position, vec2<f32>(0.5, -0.5), vec2<f32>(0.5, 0.5));
    out.position = r_locals.transform * vec4<f32>(position, 0.0, 1.0);
    return out;
}

// Fragment shader bindings

@group(0) @binding(0) var r_tex_color: texture_2d<f32>;
@group(0) @binding(1) var r_tex_sampler: sampler;

@fragment
fn fs_main(@location(0) tex_coord: vec2<f32>) -> @location(0) vec4<f32> {
    let color = textureSample(r_tex_color, r_tex_sampler, tex_coord);
    if (r_locals.effect == 0u) {
        return color;
    }

    // Position within the current source texel (0..1 on each axis).
    let f = fract(tex_coord * r_locals.source_size);

    if (r_locals.effect == 1u) {
        // LCD grid: darken the boundary PIXEL of each source texel so each
        // cell reads as discrete. Pixel-based, not fraction-band-based: the
        // old smoothstep-on-fract form sampled fractions on a lattice that
        // never lands inside its 10% edge band at exact integer scales (the
        // window auto-resize snaps the game to integer scale), making the
        // grid invisible exactly where it is most used. A pixel is a boundary
        // pixel when its right/lower neighbour falls in a different texel
        // (screen-space derivatives give the per-pixel texel step).
        let t = tex_coord * r_locals.source_size;
        let stepv = vec2<f32>(dpdx(t.x), dpdy(t.y));
        let edge = floor(t.x + stepv.x) != floor(t.x) || floor(t.y + stepv.y) != floor(t.y);
        let grid = select(1.0, 0.80, edge);
        return vec4<f32>(color.rgb * grid, color.a);
    }

    // effect == 2u: scanlines — brightness peaks mid-row and dims toward the
    // top/bottom edge of each source row.
    let s = 1.0 - 0.40 * abs(f.y - 0.5) * 2.0;
    return vec4<f32>(color.rgb * s, color.a);
}

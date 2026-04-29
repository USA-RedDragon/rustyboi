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
        // LCD grid: darken a thin gap along both texel edges so each source
        // pixel reads as a discrete cell. `smoothstep` keeps it soft at any
        // scale; the constants trade cell brightness against gap width.
        let gx = smoothstep(0.0, 0.10, f.x) * (1.0 - smoothstep(0.90, 1.0, f.x));
        let gy = smoothstep(0.0, 0.10, f.y) * (1.0 - smoothstep(0.90, 1.0, f.y));
        let grid = mix(0.80, 1.0, gx * gy);
        return vec4<f32>(color.rgb * grid, color.a);
    }

    // effect == 2u: scanlines — brightness peaks mid-row and dims toward the
    // top/bottom edge of each source row.
    let s = 1.0 - 0.40 * abs(f.y - 0.5) * 2.0;
    return vec4<f32>(color.rgb * s, color.a);
}

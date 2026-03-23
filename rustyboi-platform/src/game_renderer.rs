//! Renders the emulator framebuffer texture into an arbitrary rectangular
//! sub-region of the surface (the egui central region), with aspect-ratio
//! letterboxing. Replaces `pixels`' built-in `ScalingRenderer` in the
//! composite so the game is never hidden behind the egui menu/status panels.
//!
//! The renderer owns its RGBA source texture(s) and uploads each frame itself,
//! so the presented image is fully decoupled from `pixels`' fixed 160x144
//! framebuffer. It keeps one texture per supported source size — the normal
//! 160x144 Game Boy screen and the 256x224 Super Game Boy border composite —
//! and picks the one matching the frame handed to [`GameRenderer::upload`].

use pixels::wgpu;
use pixels::wgpu::util::DeviceExt;

/// Normal Game Boy screen dimensions.
pub const GB_WIDTH: u32 = 160;
pub const GB_HEIGHT: u32 = 144;
/// Super Game Boy border composite dimensions.
pub const SGB_WIDTH: u32 = 256;
pub const SGB_HEIGHT: u32 = 224;

/// A rectangle in physical pixels within the surface. Origin is top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

fn f32s_to_bytes(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for v in values {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }
    bytes
}

/// One RGBA source texture at a fixed size, plus the bind group that samples it.
struct Source {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl Source {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniform_buffer: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rustyboi_game_renderer_source_texture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rustyboi_game_renderer_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        Source { width, height, texture, bind_group }
    }

    /// Upload a tightly-packed RGBA8 frame (`width * height * 4` bytes).
    fn upload(&self, queue: &wgpu::Queue, rgba: &[u8]) {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 4),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// Which source is presented this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSize {
    /// Normal 160x144 Game Boy screen.
    Gb,
    /// 256x224 Super Game Boy border composite.
    Sgb,
}

/// Draws the emulator RGBA frame into a target rect of the surface.
pub struct GameRenderer {
    gb_source: Source,
    sgb_source: Source,
    active: SourceSize,
    vertex_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    render_pipeline: wgpu::RenderPipeline,
    clear_color: wgpu::Color,
}

impl GameRenderer {
    pub fn new(device: &wgpu::Device, render_texture_format: wgpu::TextureFormat) -> Self {
        let shader = wgpu::include_wgsl!("../shaders/scale.wgsl");
        let module = device.create_shader_module(shader);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rustyboi_game_renderer_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            lod_min_clamp: 0.0,
            lod_max_clamp: 1.0,
            compare: None,
            anisotropy_clamp: 1,
            border_color: None,
        });

        // One full-screen triangle (see pixels' ScalingRenderer).
        let vertex_data: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rustyboi_game_renderer_vertex_buffer"),
            contents: &f32s_to_bytes(&vertex_data),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let identity: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let transform_bytes = f32s_to_bytes(&identity);
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rustyboi_game_renderer_matrix_uniform_buffer"),
            contents: &transform_bytes,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let vertex_buffer_layout = wgpu::VertexBufferLayout {
            array_stride: 8,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            }],
        };

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rustyboi_game_renderer_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(transform_bytes.len() as u64),
                    },
                    count: None,
                },
            ],
        });

        let gb_source = Source::new(
            device,
            &bind_group_layout,
            &sampler,
            &uniform_buffer,
            GB_WIDTH,
            GB_HEIGHT,
        );
        let sgb_source = Source::new(
            device,
            &bind_group_layout,
            &sampler,
            &uniform_buffer,
            SGB_WIDTH,
            SGB_HEIGHT,
        );

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rustyboi_game_renderer_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rustyboi_game_renderer_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: "vs_main",
                buffers: &[vertex_buffer_layout],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: render_texture_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
        });

        Self {
            gb_source,
            sgb_source,
            active: SourceSize::Gb,
            vertex_buffer,
            uniform_buffer,
            render_pipeline,
            clear_color: wgpu::Color::BLACK,
        }
    }

    /// Upload an RGBA8 frame of the given source size and make it the active
    /// source for the next `render`. `rgba` must be `width * height * 4` bytes.
    pub fn upload(&mut self, queue: &wgpu::Queue, size: SourceSize, rgba: &[u8]) {
        let source = match size {
            SourceSize::Gb => &self.gb_source,
            SourceSize::Sgb => &self.sgb_source,
        };
        source.upload(queue, rgba);
        self.active = size;
    }

    fn active_source(&self) -> &Source {
        match self.active {
            SourceSize::Gb => &self.gb_source,
            SourceSize::Sgb => &self.sgb_source,
        }
    }

    /// Compute the integer-scaled, aspect-preserving destination rect for the
    /// game within `region`, and the NDC transform that places it there.
    ///
    /// `surface` is the full surface size in physical pixels. `region` is the
    /// egui central area (already in physical pixels), clamped to the surface.
    fn layout(
        &self,
        surface: (f32, f32),
        region: PhysicalRect,
    ) -> ([f32; 16], (u32, u32, u32, u32)) {
        let (surface_w, surface_h) = surface;
        let source = self.active_source();
        let texture_width = source.width as f32;
        let texture_height = source.height as f32;

        // Clamp the region to the surface so scissor stays in bounds.
        let rx = region.x.max(0.0);
        let ry = region.y.max(0.0);
        let rw = region.width.clamp(0.0, (surface_w - rx).max(0.0));
        let rh = region.height.clamp(0.0, (surface_h - ry).max(0.0));

        // Integer scale that fits the game inside the region (matches pixels'
        // ScalingRenderer behaviour: floor'd, at least 1x).
        let width_ratio = (rw / texture_width).max(1.0);
        let height_ratio = (rh / texture_height).max(1.0);
        let scale = width_ratio.clamp(1.0, height_ratio).floor();

        let scaled_w = (texture_width * scale).min(rw);
        let scaled_h = (texture_height * scale).min(rh);

        // Center the game within the region (physical pixels, top-left origin).
        let dst_x = rx + (rw - scaled_w) / 2.0;
        let dst_y = ry + (rh - scaled_h) / 2.0;
        let center_x = dst_x + scaled_w / 2.0;
        let center_y = dst_y + scaled_h / 2.0;

        // NDC transform: scale the fullscreen triangle to the game's fraction of
        // the surface, then translate its center. NDC x in [-1,1] left→right,
        // NDC y in [-1,1] bottom→top (so screen-y is flipped).
        let sw = scaled_w / surface_w;
        let sh = scaled_h / surface_h;
        let tx = 2.0 * center_x / surface_w - 1.0;
        let ty = 1.0 - 2.0 * center_y / surface_h;
        #[rustfmt::skip]
        let transform: [f32; 16] = [
            sw,  0.0, 0.0, 0.0,
            0.0, sh,  0.0, 0.0,
            0.0, 0.0, 1.0, 0.0,
            tx,  ty,  0.0, 1.0,
        ];

        let scissor = (
            dst_x as u32,
            dst_y as u32,
            scaled_w as u32,
            scaled_h as u32,
        );
        (transform, scissor)
    }

    /// Draw the active game texture into `region` of the surface. Clears the
    /// whole surface to the border color first, then draws the letterboxed game.
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        queue: &wgpu::Queue,
        render_target: &wgpu::TextureView,
        surface_size: (u32, u32),
        region: PhysicalRect,
    ) {
        let surface = (surface_size.0 as f32, surface_size.1 as f32);
        let (transform, scissor) = self.layout(surface, region);
        queue.write_buffer(&self.uniform_buffer, 0, &f32s_to_bytes(&transform));

        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rustyboi_game_renderer_render_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: render_target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(self.clear_color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // Nothing to draw (e.g. region collapsed to zero) — leave the clear.
        if scissor.2 == 0 || scissor.3 == 0 {
            return;
        }

        rpass.set_pipeline(&self.render_pipeline);
        rpass.set_bind_group(0, &self.active_source().bind_group, &[]);
        rpass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        rpass.set_scissor_rect(scissor.0, scissor.1, scissor.2, scissor.3);
        rpass.draw(0..3, 0..1);
    }
}

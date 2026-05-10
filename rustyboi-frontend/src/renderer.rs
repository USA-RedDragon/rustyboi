//! The portable raw-wgpu renderer. Replaces `pixels` + the old custom
//! `game_renderer` in one type: it owns the surface, device, queue, and surface
//! config, uploads the emulator frame as an RGBA texture (160x144 normal,
//! 256x224 for the SGB border composite), draws it letterboxed into the region
//! below the egui menu bar via a scaling pipeline, then composites the egui UI
//! on top with `egui-wgpu`.
//!
//! The platform crate creates the `wgpu::Surface`/`Device`/`Queue` from its
//! window (the only place a raw window handle is needed) and hands them here;
//! everything after that is window-agnostic, so a later web (WebGL2) or Android
//! adapter reuses this renderer unchanged.

use egui::{ClippedPrimitive, TexturesDelta};
use egui_wgpu::ScreenDescriptor;
use rustyboi_session::{LcdEffect, ScalingMode, TextureFilter};
use wgpu::util::DeviceExt;

/// Size in bytes of the fragment/vertex uniform block: a 4x4 transform (64) +
/// source size `vec2<f32>` (8) + effect `u32` (4) + padding (4) = 80, a
/// multiple of 16 as WGSL requires.
const UNIFORM_BYTES: usize = 80;

/// Build the 80-byte uniform block from the NDC transform, the active source
/// dimensions (for LCD-effect texel math), and the effect selector.
fn uniform_bytes(transform: &[f32; 16], source: (f32, f32), effect: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(UNIFORM_BYTES);
    for v in transform {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }
    bytes.extend_from_slice(&source.0.to_ne_bytes());
    bytes.extend_from_slice(&source.1.to_ne_bytes());
    bytes.extend_from_slice(&effect.to_ne_bytes());
    bytes.extend_from_slice(&0u32.to_ne_bytes()); // padding to 16-byte alignment
    bytes
}

/// Map the session's [`LcdEffect`] to the shader's effect selector.
fn effect_code(effect: LcdEffect) -> u32 {
    match effect {
        LcdEffect::Off => 0,
        LcdEffect::Grid => 1,
        LcdEffect::Scanlines => 2,
    }
}

/// Normal Game Boy screen dimensions.
pub const GB_WIDTH: u32 = 160;
pub const GB_HEIGHT: u32 = 144;
/// Super Game Boy border composite dimensions.
pub const SGB_WIDTH: u32 = 256;
pub const SGB_HEIGHT: u32 = 224;

/// Which source is presented this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSize {
    /// Normal 160x144 Game Boy screen.
    Gb,
    /// 256x224 Super Game Boy border composite.
    Sgb,
}

impl SourceSize {
    /// Source dimensions in pixels.
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            SourceSize::Gb => (GB_WIDTH, GB_HEIGHT),
            SourceSize::Sgb => (SGB_WIDTH, SGB_HEIGHT),
        }
    }
}

/// A rectangle in physical pixels within the surface. Origin is top-left. This
/// is the egui central region (below the menu bar, above the status panel) the
/// game is letterboxed into.
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
/// The `view` is retained so the bind group can be rebuilt when the sampling
/// filter changes without recreating (and re-uploading) the texture.
struct Source {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
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
        format: wgpu::TextureFormat,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rustyboi_game_source_texture"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = Self::make_bind_group(device, layout, sampler, uniform_buffer, &view);
        Source { width, height, texture, view, bind_group }
    }

    fn make_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniform_buffer: &wgpu::Buffer,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rustyboi_game_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
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
        })
    }

    /// Rebuild the bind group against a new sampler (filter change). Reuses the
    /// existing texture + view, so no re-upload is needed.
    fn rebuild_bind_group(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        uniform_buffer: &wgpu::Buffer,
    ) {
        self.bind_group =
            Self::make_bind_group(device, layout, sampler, uniform_buffer, &self.view);
    }

    /// Upload a tightly-packed RGBA8 frame (`width * height * 4` bytes).
    fn upload(&self, queue: &wgpu::Queue, rgba: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
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

/// The RGBA frame + its source size, ready to upload. Produced by the `App`
/// each frame from the emulator's output. Borrows the app's reused RGBA scratch
/// so presenting never heap-allocates per frame.
pub struct GameFrame<'a> {
    pub size: SourceSize,
    pub rgba: &'a [u8],
}

/// Everything egui produced this frame, handed from the `App` to the renderer.
pub struct EguiPaint {
    pub jobs: Vec<ClippedPrimitive>,
    pub textures: TexturesDelta,
    pub pixels_per_point: f32,
    /// The UI is byte-identical to last frame: the renderer skips egui's
    /// texture/vertex upload and redraws its cached jobs (`jobs` is empty here).
    pub reuse: bool,
}

/// Owns the wgpu surface + device + queue and every GPU object needed to draw
/// the emulator frame letterboxed under the egui UI. One `render` call does the
/// whole composite (game scale pass, then egui pass) onto the acquired surface.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    gb_source: Source,
    sgb_source: Source,
    active: SourceSize,
    vertex_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    render_pipeline: wgpu::RenderPipeline,
    /// The two upscale samplers; `texture_filter` selects which is bound.
    nearest_sampler: wgpu::Sampler,
    linear_sampler: wgpu::Sampler,
    /// Retained so a filter change can rebuild the source bind groups.
    bind_group_layout: wgpu::BindGroupLayout,
    /// Current sampling filter + LCD effect, pushed each frame from the session.
    texture_filter: TextureFilter,
    lcd_effect: LcdEffect,
    clear_color: wgpu::Color,
    /// Set once any game frame has been uploaded. Lets a render tick with no
    /// fresh frame redraw the last texture instead of clearing to black — the
    /// web frontend runs the emulator in a worker at 59.7fps, decoupled from the
    /// display's requestAnimationFrame, so refreshes routinely land with no new
    /// frame and would otherwise flash the game area black.
    has_game: bool,
    /// Last frame's egui geometry, redrawn on `reuse` frames (unchanged UI) so
    /// the per-frame vertex/index upload can be skipped.
    egui_jobs: Vec<ClippedPrimitive>,
    /// Frame letterboxing policy the frontend pushes each frame from the session
    /// config. `FitAspect` (default) reproduces the historical layout exactly.
    scaling_mode: ScalingMode,

    egui_renderer: egui_wgpu::Renderer,
}

impl Renderer {
    /// Build the renderer around a surface the platform created from its window.
    /// The platform is responsible for the (safe) `Instance::create_surface`,
    /// adapter request, and device request; it passes the resulting handles in.
    /// `width`/`height` are the surface size in physical pixels.
    pub fn new(
        surface: wgpu::Surface<'static>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = wgpu::include_wgsl!("../shaders/scale.wgsl");
        let module = device.create_shader_module(shader);

        let make_sampler = |filter: wgpu::FilterMode, label: &str| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                lod_min_clamp: 0.0,
                lod_max_clamp: 1.0,
                compare: None,
                anisotropy_clamp: 1,
                border_color: None,
            })
        };
        let nearest_sampler = make_sampler(wgpu::FilterMode::Nearest, "rustyboi_sampler_nearest");
        let linear_sampler = make_sampler(wgpu::FilterMode::Linear, "rustyboi_sampler_linear");
        // Nearest is the default (crisp pixels, the historical behavior).
        let sampler = &nearest_sampler;

        // One full-screen triangle (as in pixels' ScalingRenderer).
        let vertex_data: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rustyboi_game_vertex_buffer"),
            contents: &f32s_to_bytes(&vertex_data),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let identity: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ];
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rustyboi_game_matrix_uniform_buffer"),
            contents: &uniform_bytes(&identity, (GB_WIDTH as f32, GB_HEIGHT as f32), 0),
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
            label: Some("rustyboi_game_bind_group_layout"),
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
                    // The vertex shader reads the transform and the fragment
                    // shader reads the source size + LCD-effect selector, so the
                    // uniform must be visible to BOTH stages.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES as u64),
                    },
                    count: None,
                },
            ],
        });

        // Match the game texture's sRGB-ness to the surface so colors pass
        // through unchanged: an sRGB texture sampled to linear MUST be written to
        // an sRGB surface (hardware re-encodes), and a UNORM texture to a UNORM
        // surface. A mismatch (sRGB texture -> UNORM surface) displays linear
        // values = too dark — this is why Android (non-sRGB surface) looked dark
        // while desktop (sRGB fallback surface) looked right. The GB frame bytes
        // are display-ready (already sRGB-encoded), so pass-through is correct.
        let tex_format = if surface_format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let gb_source = Source::new(
            &device,
            &bind_group_layout,
            sampler,
            &uniform_buffer,
            GB_WIDTH,
            GB_HEIGHT,
            tex_format,
        );
        let sgb_source = Source::new(
            &device,
            &bind_group_layout,
            sampler,
            &uniform_buffer,
            SGB_WIDTH,
            SGB_HEIGHT,
            tex_format,
        );

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rustyboi_game_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rustyboi_game_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[vertex_buffer_layout],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            surface_format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                predictable_texture_filtering: false,
            },
        );

        Self {
            surface,
            device,
            queue,
            config,
            gb_source,
            sgb_source,
            active: SourceSize::Gb,
            vertex_buffer,
            uniform_buffer,
            render_pipeline,
            nearest_sampler,
            linear_sampler,
            bind_group_layout,
            texture_filter: TextureFilter::Nearest,
            lcd_effect: LcdEffect::Off,
            clear_color: wgpu::Color::BLACK,
            has_game: false,
            egui_jobs: Vec::new(),
            scaling_mode: ScalingMode::FitAspect,
            egui_renderer,
        }
    }

    /// Set the frame letterboxing policy used by the next `render`. Frontends
    /// call this each frame from the session config so the setting takes effect.
    pub fn set_scaling_mode(&mut self, mode: ScalingMode) {
        self.scaling_mode = mode;
    }

    /// Set the upscale texture filter. On a change, rebuilds both source bind
    /// groups against the selected sampler (cheap — the textures are reused).
    pub fn set_texture_filter(&mut self, filter: TextureFilter) {
        if filter == self.texture_filter {
            return;
        }
        self.texture_filter = filter;
        let sampler = match filter {
            TextureFilter::Nearest => &self.nearest_sampler,
            TextureFilter::Linear => &self.linear_sampler,
        };
        self.gb_source
            .rebuild_bind_group(&self.device, &self.bind_group_layout, sampler, &self.uniform_buffer);
        self.sgb_source
            .rebuild_bind_group(&self.device, &self.bind_group_layout, sampler, &self.uniform_buffer);
    }

    /// Set the LCD post-process effect used by the next `render` (uniform-driven,
    /// no pipeline rebuild).
    pub fn set_lcd_effect(&mut self, effect: LcdEffect) {
        self.lcd_effect = effect;
    }

    /// A `Device` clone the platform can use to build companion GPU state if
    /// needed. Cheap (wgpu handles are `Arc`-backed).
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The surface texture format, for anything that needs to match it.
    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Resize the surface (physical pixels). No-op on a zero dimension.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Current surface size in physical pixels.
    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Upload an RGBA8 frame of the given source size, making it active for the
    /// next `render`. `rgba` must be `width * height * 4` bytes.
    /// Upload a game frame to its source texture (marking `has_game`). Public so
    /// the web driver can upload directly from its shared buffer and then render
    /// with `game: None` — avoiding a per-frame clone to hand ownership across
    /// the `RefCell` borrow. `render` still uploads internally when passed a frame.
    pub fn upload_game(&mut self, frame: &GameFrame) {
        let source = match frame.size {
            SourceSize::Gb => &self.gb_source,
            SourceSize::Sgb => &self.sgb_source,
        };
        source.upload(&self.queue, frame.rgba);
        self.active = frame.size;
        self.has_game = true;
    }

    fn active_source(&self) -> &Source {
        match self.active {
            SourceSize::Gb => &self.gb_source,
            SourceSize::Sgb => &self.sgb_source,
        }
    }

    /// Compute the integer-scaled, aspect-preserving destination rect for the
    /// active source within `region`, plus the NDC transform placing it there.
    fn layout(&self, surface: (f32, f32), region: PhysicalRect) -> ([f32; 16], (u32, u32, u32, u32)) {
        let source = self.active_source();
        compute_layout(
            (source.width as f32, source.height as f32),
            surface,
            region,
            self.scaling_mode,
        )
    }

    /// Render one full frame: acquire the surface, clear to the border color,
    /// draw the game letterboxed into `region`, then composite egui on top.
    /// `game` is `None` when there is no frame to present yet (still clears +
    /// draws egui). Returns `Err` when the surface must be recreated (the
    /// platform resizes and retries).
    pub fn render(
        &mut self,
        game: Option<&GameFrame>,
        region: PhysicalRect,
        egui: EguiPaint,
    ) -> Result<(), wgpu::SurfaceStatus> {
        if let Some(frame) = game {
            self.upload_game(frame);
        }

        // wgpu 29 returns a `CurrentSurfaceTexture` enum rather than a
        // `Result<_, SurfaceError>`. Present on Success/Suboptimal; hand the
        // status back so the caller reconfigures on Outdated/Lost/Validation, and
        // silently skip the frame on Timeout/Occluded.
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => return Err(wgpu::SurfaceStatus::Outdated),
            wgpu::CurrentSurfaceTexture::Lost => return Err(wgpu::SurfaceStatus::Lost),
            wgpu::CurrentSurfaceTexture::Validation => return Err(wgpu::SurfaceStatus::Validation),
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rustyboi_frame_encoder"),
            });

        // --- game scale pass: clear + letterboxed game ---------------------
        let surface = (self.config.width as f32, self.config.height as f32);
        let (transform, scissor) = self.layout(surface, region);
        let src = self.active_source();
        let source_dims = (src.width as f32, src.height as f32);
        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            &uniform_bytes(&transform, source_dims, effect_code(self.lcd_effect)),
        );
        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rustyboi_game_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Draw only when there is a game frame and a non-empty target.
            if self.has_game && scissor.2 != 0 && scissor.3 != 0 {
                rpass.set_pipeline(&self.render_pipeline);
                rpass.set_bind_group(0, &self.active_source().bind_group, &[]);
                rpass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                rpass.set_scissor_rect(scissor.0, scissor.1, scissor.2, scissor.3);
                rpass.draw(0..3, 0..1);
            }
        }

        // --- egui pass: composite the UI on top ----------------------------
        // When `reuse` is set the UI is unchanged since last frame: skip the
        // texture + vertex/index uploads (egui-wgpu's buffers still hold last
        // frame's geometry) and just redraw the cached jobs. The game underneath
        // still redraws every frame, so only egui's per-frame upload is elided.
        let EguiPaint { jobs, textures, pixels_per_point, reuse } = egui;
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };
        // egui-wgpu's `update_buffers` now returns any command buffers its paint
        // callbacks produced; they must be submitted before this frame's encoder.
        let mut egui_cmd_bufs = Vec::new();
        if !reuse {
            for (id, image_delta) in &textures.set {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *id, image_delta);
            }
            egui_cmd_bufs = self.egui_renderer.update_buffers(
                &self.device,
                &self.queue,
                &mut encoder,
                &jobs,
                &screen_descriptor,
            );
            self.egui_jobs = jobs;
        }
        {
            // egui-wgpu's `render` requires a `RenderPass<'static>` (wgpu 22+);
            // `forget_lifetime` drops the encoder-borrow lifetime (the pass is
            // still dropped before `encoder.finish()`, so this stays sound).
            let mut rpass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("rustyboi_egui_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui_renderer
                .render(&mut rpass, &self.egui_jobs, &screen_descriptor);
        }

        self.queue
            .submit(egui_cmd_bufs.into_iter().chain(std::iter::once(encoder.finish())));
        output.present();

        if !reuse {
            for id in &textures.free {
                self.egui_renderer.free_texture(id);
            }
        }
        Ok(())
    }
}

/// Pure letterbox math: integer-scaled, aspect-preserving placement of a
/// `texture`-sized source into `region` of a `surface` (all physical pixels).
/// Returns the NDC transform for the fullscreen triangle and the scissor rect
/// `(x, y, w, h)`. Separated out so it can be unit-tested without a GPU.
fn compute_layout(
    texture: (f32, f32),
    surface: (f32, f32),
    region: PhysicalRect,
    mode: ScalingMode,
) -> ([f32; 16], (u32, u32, u32, u32)) {
    let (surface_w, surface_h) = surface;
    let (texture_width, texture_height) = texture;

    // Clamp the region to the surface so scissor stays in bounds.
    let rx = region.x.max(0.0);
    let ry = region.y.max(0.0);
    let rw = region.width.clamp(0.0, (surface_w - rx).max(0.0));
    let rh = region.height.clamp(0.0, (surface_h - ry).max(0.0));

    // Placement per scaling mode. FitAspect is the historical path and MUST stay
    // byte-identical: aspect-preserving contain (the largest scale keeping the
    // source inside the region on both axes). IntegerAspect floors that scale to
    // a whole number (≥1). Stretch fills the region on both axes independently.
    let (scaled_w, scaled_h) = match mode {
        ScalingMode::FitAspect => {
            let scale = (rw / texture_width).min(rh / texture_height).max(0.0);
            ((texture_width * scale).min(rw), (texture_height * scale).min(rh))
        }
        ScalingMode::IntegerAspect => {
            let fit = (rw / texture_width).min(rh / texture_height).max(0.0);
            // Floor to a whole scale, but never below 1× unless the region can't
            // hold even 1× (then fall back to the fractional fit so it stays in
            // bounds and a collapsed region still yields a zero scissor).
            let scale = if fit >= 1.0 { fit.floor() } else { fit };
            ((texture_width * scale).min(rw), (texture_height * scale).min(rh))
        }
        ScalingMode::Stretch => (rw, rh),
    };

    // Center within the region (physical pixels, top-left origin).
    let dst_x = rx + (rw - scaled_w) / 2.0;
    let dst_y = ry + (rh - scaled_h) / 2.0;
    let center_x = dst_x + scaled_w / 2.0;
    let center_y = dst_y + scaled_h / 2.0;

    // NDC transform: scale the fullscreen triangle to the source's fraction of
    // the surface, then translate its center. NDC y is flipped vs screen-y.
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

    let scissor = (dst_x as u32, dst_y as u32, scaled_w as u32, scaled_h as u32);
    (transform, scissor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> PhysicalRect {
        PhysicalRect { x, y, width: w, height: h }
    }

    // A 160x144 game in a full 800x720 surface region scales 5x and fills it.
    #[test]
    fn exact_integer_fit_centers_and_fills() {
        let (_t, scissor) = compute_layout((160.0, 144.0), (800.0, 720.0), rect(0.0, 0.0, 800.0, 720.0), ScalingMode::FitAspect);
        assert_eq!(scissor, (0, 0, 800, 720));
    }

    // An off-aspect region fits the limiting axis fractionally (aspect-preserving
    // contain), filling it exactly with a minimal bar on the other axis: 160x144
    // into an 800x700 region scales 700/144 = 4.861x, filling the height (700)
    // and centering the 777px width with small side bars. (In practice the window
    // is aspect-locked so the region matches the source aspect and both axes
    // fill; this covers the transient mid-resize case.)
    #[test]
    fn fractional_fit_fills_limiting_axis() {
        let (_t, scissor) = compute_layout((160.0, 144.0), (800.0, 720.0), rect(0.0, 20.0, 800.0, 700.0), ScalingMode::FitAspect);
        assert_eq!(scissor.3, 700); // height fills exactly
        assert_eq!(scissor.2, 777); // 160 * (700/144) = 777.7 -> 777
        assert_eq!(scissor.1, 20); // no vertical bar on the limiting axis
    }

    // The SGB composite (256x224) uses its own aspect: into 1280x1120 it scales
    // 5x and fills, proving the source-size drives the fit (the sizing-bug fix).
    #[test]
    fn sgb_source_uses_its_own_aspect() {
        let (_t, scissor) = compute_layout((256.0, 224.0), (1280.0, 1120.0), rect(0.0, 0.0, 1280.0, 1120.0), ScalingMode::FitAspect);
        assert_eq!(scissor, (0, 0, 1280, 1120));
    }

    // A collapsed region (menu covering everything) yields a zero-size scissor
    // so `render` skips the draw rather than panicking.
    #[test]
    fn collapsed_region_is_safe() {
        let (_t, scissor) = compute_layout((160.0, 144.0), (800.0, 720.0), rect(0.0, 0.0, 0.0, 0.0), ScalingMode::FitAspect);
        // A zero-size region yields scale 0 and a collapsed scissor.
        assert_eq!(scissor.2, 0);
        assert_eq!(scissor.3, 0);
    }

    // IntegerAspect floors the fit scale: an off-aspect 800x700 region that
    // FitAspect fills fractionally (4.861x) snaps to a whole 4x = 640x576,
    // centered with bars on both axes.
    #[test]
    fn integer_aspect_floors_to_whole_scale() {
        let (_t, scissor) = compute_layout((160.0, 144.0), (800.0, 720.0), rect(0.0, 0.0, 800.0, 700.0), ScalingMode::IntegerAspect);
        assert_eq!(scissor.2, 640); // 160 * 4
        assert_eq!(scissor.3, 576); // 144 * 4
        assert_eq!(scissor.0, 80); // (800 - 640) / 2
        assert_eq!(scissor.1, 62); // (700 - 576) / 2
    }

    // An exact-integer region still fills under IntegerAspect (floor of 5.0 = 5).
    #[test]
    fn integer_aspect_exact_fit_still_fills() {
        let (_t, scissor) = compute_layout((160.0, 144.0), (800.0, 720.0), rect(0.0, 0.0, 800.0, 720.0), ScalingMode::IntegerAspect);
        assert_eq!(scissor, (0, 0, 800, 720));
    }

    // Stretch fills the whole region on both axes, ignoring aspect (distorts).
    #[test]
    fn stretch_fills_region_ignoring_aspect() {
        let (_t, scissor) = compute_layout((160.0, 144.0), (800.0, 720.0), rect(0.0, 20.0, 800.0, 700.0), ScalingMode::Stretch);
        assert_eq!(scissor, (0, 20, 800, 700));
    }
}

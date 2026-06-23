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

/// The per-frame presentation contract the `App` drives — everything a display
/// backend must do: track the surface size, take the session's presentation
/// policy, and composite game + egui into a presented frame. Implemented by the
/// wgpu [`Renderer`] and the CPU [`SoftRenderer`](crate::soft::SoftRenderer)
/// (the `Software` graphics backend), so the app and platform loop are
/// backend-agnostic. `render`'s error type stays `wgpu::SurfaceStatus` — it is
/// the richer of the two backends' failure vocabularies and the platform's
/// reconfigure-on-`Lost`/`Outdated` logic keys off it (a software backend
/// simply never returns those).
pub trait Present {
    fn surface_size(&self) -> (u32, u32);
    fn resize(&mut self, width: u32, height: u32);
    fn set_scaling_mode(&mut self, mode: ScalingMode);
    fn set_texture_filter(&mut self, filter: TextureFilter);
    fn set_lcd_effect(&mut self, effect: LcdEffect);
    /// Upload a game frame, retaining it as the active source for subsequent
    /// `render(game: None, ..)` calls. The web driver uploads directly from
    /// its worker-shared buffer and then renders with `game: None` to avoid a
    /// per-frame clone; both backends retain the last frame either way.
    fn upload_game(&mut self, frame: &GameFrame);
    fn render(
        &mut self,
        game: Option<&GameFrame>,
        region: PhysicalRect,
        egui: EguiPaint,
    ) -> Result<(), wgpu::SurfaceStatus>;
    /// Whether the most recent `render` presented through a mechanism that
    /// blocks the loop at the display's refresh once it outpaces it (a Fifo
    /// swapchain). When true the present IS the tick clock and the platform
    /// loop must not sleep on top of it; when false (Mailbox swapchain,
    /// software blit, or a skipped/occluded frame) the platform throttles the
    /// tick itself. Part of the pacing scheme — see `rustyboi_session::pacing`.
    fn vsync_paced(&self) -> bool;
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
    target: FrameTarget,
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
    /// Whether the most recent `render` actually presented a frame (false when
    /// the surface skipped it: Timeout/Occluded/Outdated/Lost). The platform
    /// tick-throttle reads this — a presented Fifo frame blocks at vsync and
    /// needs no sleep, a skipped one must be throttled or the loop runs hot.
    last_presented: bool,
    /// Frame letterboxing policy the frontend pushes each frame from the session
    /// config. `FitAspect` (default) reproduces the historical layout exactly.
    scaling_mode: ScalingMode,

    egui: EguiCompositor,
}

/// Owns egui-wgpu's `Renderer` plus the cached paint jobs, isolating egui's
/// *incremental* texture bookkeeping from the swapchain.
///
/// egui emits each font-atlas region exactly once — a full allocation
/// (`ImageDelta.pos == None`), then per-glyph partial updates
/// (`pos == Some(_)`). So its texture deltas MUST be applied on every frame egui
/// produces them, *including a frame whose surface acquisition fails and is
/// skipped*. Dropping a delta desyncs egui's renderer permanently: a later
/// partial update lands on a texture that was never allocated and panics with
/// "Tried to update a texture that has not been allocated yet." That was the
/// macOS startup crash — its Retina surface returns `Outdated`/`Timeout` on the
/// first `get_current_texture`, and the old `render` returned *before* uploading
/// the font atlas, so the next frame's partial glyph update blew up (or, when it
/// happened to survive, the atlas was simply missing and no UI drew).
///
/// Keeping the texture handling here, driven only by a `Device`/`Queue`
/// (independent of the surface), lets `render` apply it before touching the
/// swapchain and makes the ordering testable without a window (see the tests).
struct EguiCompositor {
    renderer: egui_wgpu::Renderer,
    /// Last frame's egui geometry, redrawn on `reuse` frames (unchanged UI) so
    /// the per-frame vertex/index upload can be skipped.
    jobs: Vec<ClippedPrimitive>,
}

impl EguiCompositor {
    fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let renderer = egui_wgpu::Renderer::new(
            device,
            surface_format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                predictable_texture_filtering: false,
            },
        );
        Self { renderer, jobs: Vec::new() }
    }

    /// Apply egui's texture allocations/updates. Queue writes only, so this is
    /// independent of the surface and `render` runs it *before* acquiring the
    /// frame — see the type doc for why dropping these on a skipped frame is
    /// fatal.
    fn apply_textures(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, textures: &TexturesDelta) {
        for (id, image_delta) in &textures.set {
            self.renderer.update_texture(device, queue, *id, image_delta);
        }
    }

    /// Upload this frame's vertex/index geometry (caching the jobs for reuse
    /// frames). Returns any command buffers egui paint-callbacks produced, to be
    /// submitted before the frame encoder.
    fn upload_geometry(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        jobs: Vec<ClippedPrimitive>,
        screen_descriptor: &ScreenDescriptor,
    ) -> Vec<wgpu::CommandBuffer> {
        let bufs = self.renderer.update_buffers(device, queue, encoder, &jobs, screen_descriptor);
        self.jobs = jobs;
        bufs
    }

    /// Paint the cached jobs into `render_pass`.
    fn paint(&self, render_pass: &mut wgpu::RenderPass<'static>, screen_descriptor: &ScreenDescriptor) {
        self.renderer.render(render_pass, &self.jobs, screen_descriptor);
    }

    /// Free the textures egui retired this frame. Called after the frame's paint
    /// on a rendered frame, or immediately on a skipped one (where nothing
    /// referenced them), so a coincident surface error never leaks a texture.
    fn free(&mut self, textures: &TexturesDelta) {
        for id in &textures.free {
            self.renderer.free_texture(id);
        }
    }
}

/// Where `render` sends a frame. Production always uses `Surface` — the window
/// swapchain — and that arm behaves exactly as calling the surface directly.
/// Tests use `Offscreen`: a plain texture plus a queue of surface statuses to
/// hand back from `acquire`, so `render`'s surface-error handling (the macOS
/// first-frame path that used to drop egui's font-atlas upload) can be driven
/// deterministically without a window. The whole non-`Surface` arm is
/// `#[cfg(test)]`, so release builds see a single-variant enum and identical
/// codegen.
enum FrameTarget {
    Surface(wgpu::Surface<'static>),
    #[cfg(test)]
    Offscreen {
        texture: wgpu::Texture,
        /// Statuses returned by successive `acquire` calls (front = next); an
        /// empty queue defaults to `Good` (a normal successful acquire).
        statuses: std::collections::VecDeque<wgpu::SurfaceStatus>,
    },
}

/// The outcome of acquiring a frame's color target.
enum FrameAcquire {
    /// A real swapchain image; `present` it after drawing.
    Surface(wgpu::SurfaceTexture),
    /// Draw into the target's own offscreen texture; nothing to present.
    #[cfg(test)]
    Offscreen,
    /// Don't draw this frame; `render` returns this value verbatim. `Ok` skips
    /// silently (Timeout/Occluded), `Err` asks the caller to reconfigure/retry.
    Skip(Result<(), wgpu::SurfaceStatus>),
}

impl FrameTarget {
    /// Acquire the next frame's target, mapping wgpu's surface status onto the
    /// draw-or-skip decision `render` acts on.
    fn acquire(&mut self) -> FrameAcquire {
        match self {
            // wgpu 29 returns a `CurrentSurfaceTexture` enum rather than a
            // `Result<_, SurfaceError>`. Draw on Success/Suboptimal; hand the
            // status back so the caller reconfigures on Outdated/Lost/Validation,
            // and silently skip the frame on Timeout/Occluded.
            FrameTarget::Surface(surface) => match surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t)
                | wgpu::CurrentSurfaceTexture::Suboptimal(t) => FrameAcquire::Surface(t),
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    FrameAcquire::Skip(Ok(()))
                }
                wgpu::CurrentSurfaceTexture::Outdated => {
                    FrameAcquire::Skip(Err(wgpu::SurfaceStatus::Outdated))
                }
                wgpu::CurrentSurfaceTexture::Lost => FrameAcquire::Skip(Err(wgpu::SurfaceStatus::Lost)),
                wgpu::CurrentSurfaceTexture::Validation => {
                    FrameAcquire::Skip(Err(wgpu::SurfaceStatus::Validation))
                }
            },
            #[cfg(test)]
            FrameTarget::Offscreen { statuses, .. } => {
                match statuses.pop_front().unwrap_or(wgpu::SurfaceStatus::Good) {
                    wgpu::SurfaceStatus::Good | wgpu::SurfaceStatus::Suboptimal => {
                        FrameAcquire::Offscreen
                    }
                    wgpu::SurfaceStatus::Timeout | wgpu::SurfaceStatus::Occluded => {
                        FrameAcquire::Skip(Ok(()))
                    }
                    s => FrameAcquire::Skip(Err(s)),
                }
            }
        }
    }
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
        present_mode: wgpu::PresentMode,
    ) -> Self {
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: width.max(1),
            height: height.max(1),
            present_mode,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);

        Self::assemble(FrameTarget::Surface(surface), config, device, queue, surface_format)
    }

    /// Test-only: build an identical renderer that draws into an offscreen
    /// texture instead of a window swapchain, so `render`'s full path — including
    /// the surface-error branch that regressed on macOS — is exercisable
    /// headlessly. Seed the statuses `acquire` should report per frame with
    /// [`Renderer::inject_statuses`].
    #[cfg(test)]
    pub(crate) fn new_offscreen(
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
            desired_maximum_frame_latency: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rustyboi_offscreen_target"),
            size: wgpu::Extent3d { width: config.width, height: config.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: surface_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target = FrameTarget::Offscreen { texture, statuses: std::collections::VecDeque::new() };
        Self::assemble(target, config, device, queue, surface_format)
    }

    /// Shared GPU setup for [`Renderer::new`] and the test offscreen constructor:
    /// everything downstream of the frame target (shaders, samplers, scale
    /// pipeline, game sources, egui compositor).
    fn assemble(
        target: FrameTarget,
        config: wgpu::SurfaceConfiguration,
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
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

        let egui = EguiCompositor::new(&device, surface_format);

        Self {
            target,
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
            last_presented: false,
            scaling_mode: ScalingMode::FitAspect,
            egui,
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
            match &self.target {
                FrameTarget::Surface(surface) => surface.configure(&self.device, &self.config),
                // The offscreen test target keeps its original size; the surface
                // config still tracks the logical size for the screen descriptor.
                #[cfg(test)]
                FrameTarget::Offscreen { .. } => {}
            }
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

        // When `reuse` is set the UI is unchanged since last frame: skip egui's
        // texture + vertex/index uploads (egui-wgpu's buffers still hold last
        // frame's geometry) and just redraw the cached jobs. The game underneath
        // still redraws every frame, so only egui's per-frame upload is elided.
        let EguiPaint { jobs, textures, pixels_per_point, reuse } = egui;

        // Apply egui's incremental texture allocations/updates BEFORE acquiring
        // the surface. egui emits each font-atlas region exactly once, so these
        // MUST land even on a frame the surface then forces us to skip — dropping
        // them desyncs egui's renderer forever (a later partial update panics on
        // an unallocated texture). This is queue-only work, independent of the
        // swapchain, so it is always safe here. See `EguiCompositor` for the full
        // rationale (this was the macOS first-frame crash).
        if !reuse {
            self.egui.apply_textures(&self.device, &self.queue, &textures);
        }

        // Acquire the frame's color target. On every non-rendering outcome the
        // textures egui retired this frame are still freed (nothing painted them,
        // so it is safe) so a coincident surface error never leaks a texture.
        let surface_frame = match self.target.acquire() {
            FrameAcquire::Surface(t) => Some(t),
            #[cfg(test)]
            FrameAcquire::Offscreen => None,
            FrameAcquire::Skip(result) => {
                self.egui.free(&textures);
                self.last_presented = false;
                return result;
            }
        };
        let view = match &surface_frame {
            Some(t) => t.texture.create_view(&wgpu::TextureViewDescriptor::default()),
            // `None` only happens for the offscreen test target (in release the
            // enum has no such arm and this branch is unreachable).
            None => {
                #[cfg(test)]
                {
                    match &self.target {
                        FrameTarget::Offscreen { texture, .. } => {
                            texture.create_view(&wgpu::TextureViewDescriptor::default())
                        }
                        FrameTarget::Surface(_) => unreachable!("surface never acquires as Offscreen"),
                    }
                }
                #[cfg(not(test))]
                unreachable!("surface acquire never yields None")
            }
        };

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
        // Textures were already applied above (before surface acquisition); here
        // we upload this frame's geometry (skipped on `reuse`, where egui-wgpu's
        // buffers still hold last frame's jobs) and paint.
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point,
        };
        // egui-wgpu's `update_buffers` returns any command buffers its paint
        // callbacks produced; they must be submitted before this frame's encoder.
        let mut egui_cmd_bufs = Vec::new();
        if !reuse {
            egui_cmd_bufs =
                self.egui
                    .upload_geometry(&self.device, &self.queue, &mut encoder, jobs, &screen_descriptor);
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
            self.egui.paint(&mut rpass, &screen_descriptor);
        }

        self.queue
            .submit(egui_cmd_bufs.into_iter().chain(std::iter::once(encoder.finish())));
        // Present the swapchain image (the offscreen test target has nothing to
        // present — the drawn texture is read back directly).
        if let Some(t) = surface_frame {
            t.present();
        }
        self.last_presented = true;

        if !reuse {
            self.egui.free(&textures);
        }
        Ok(())
    }
}

#[cfg(test)]
impl Renderer {
    /// Queue the surface statuses the next `render` acquisitions report (only
    /// meaningful for an offscreen test renderer).
    pub(crate) fn inject_statuses(
        &mut self,
        statuses: impl IntoIterator<Item = wgpu::SurfaceStatus>,
    ) {
        if let FrameTarget::Offscreen { statuses: queue, .. } = &mut self.target {
            queue.extend(statuses);
        }
    }

    /// Read the offscreen target back as tightly-packed RGBA8 (`width*height*4`).
    pub(crate) fn read_offscreen(&self) -> Vec<u8> {
        let FrameTarget::Offscreen { texture, .. } = &self.target else {
            panic!("read_offscreen requires an offscreen renderer");
        };
        let (w, h) = (self.config.width, self.config.height);
        let unpadded = w * 4;
        // `copy_texture_to_buffer` requires a 256-byte-aligned row stride.
        let padded = unpadded.div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rustyboi_offscreen_readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        out
    }
}

/// Delegation so the platform/`App` can drive either backend through
/// `dyn Present`; the inherent methods remain the canonical implementations.
impl Present for Renderer {
    fn surface_size(&self) -> (u32, u32) {
        Renderer::surface_size(self)
    }
    fn resize(&mut self, width: u32, height: u32) {
        Renderer::resize(self, width, height)
    }
    fn set_scaling_mode(&mut self, mode: ScalingMode) {
        Renderer::set_scaling_mode(self, mode)
    }
    fn set_texture_filter(&mut self, filter: TextureFilter) {
        Renderer::set_texture_filter(self, filter)
    }
    fn set_lcd_effect(&mut self, effect: LcdEffect) {
        Renderer::set_lcd_effect(self, effect)
    }
    fn upload_game(&mut self, frame: &GameFrame) {
        Renderer::upload_game(self, frame)
    }
    fn render(
        &mut self,
        game: Option<&GameFrame>,
        region: PhysicalRect,
        egui: EguiPaint,
    ) -> Result<(), wgpu::SurfaceStatus> {
        Renderer::render(self, game, region, egui)
    }
    fn vsync_paced(&self) -> bool {
        // A Fifo present blocks the loop at the display refresh once the loop
        // outpaces it; Mailbox never blocks. A skipped frame (occluded window,
        // surface error) paces nothing regardless of mode.
        self.last_presented && self.config.present_mode == wgpu::PresentMode::Fifo
    }
}

/// Pure letterbox math: integer-scaled, aspect-preserving placement of a
/// `texture`-sized source into `region` of a `surface` (all physical pixels).
/// Returns the NDC transform for the fullscreen triangle and the scissor rect
/// `(x, y, w, h)`. Separated out so it can be unit-tested without a GPU.
/// `pub(crate)`: the software backend reuses the scissor rect as its blit
/// destination so both backends share one placement behavior.
pub(crate) fn compute_layout(
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

    // --- Headless GPU tests: the egui texture-ordering invariant -----------
    //
    // These reproduce the macOS startup failure *deterministically and without a
    // window*. egui emits its font atlas as a one-time full allocation
    // (`ImageDelta.pos == None`) followed by per-glyph partial updates
    // (`pos == Some`). The old `render` acquired the surface *before* applying
    // those deltas and returned early on the Retina first-frame `Outdated`,
    // dropping the allocation; egui never re-sent it, so either a later partial
    // update panicked ("Tried to update a texture that has not been allocated
    // yet") or the atlas was simply missing and no UI drew. `EguiCompositor` now
    // owns this bookkeeping and `render` applies it before touching the surface.
    //
    // The tests run wherever a wgpu adapter exists — Metal on the macOS CI
    // runner (the platform that regressed), lavapipe on Linux CI — and skip with
    // a note when none does, so they never spuriously fail on a GPU-less box.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
    mod gpu {
        use super::*;
        use egui::epaint::{Color32, ColorImage, ImageData, ImageDelta, TextureId};
        use egui::TextureOptions;
        use std::sync::Arc;

        /// A headless device + queue (no surface), or `None` when the runner has
        /// no usable wgpu adapter (the test then skips).
        fn headless_gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::all(),
                flags: wgpu::InstanceFlags::default(),
                memory_budget_thresholds: Default::default(),
                backend_options: Default::default(),
                display: None,
            });
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                force_fallback_adapter: false,
                compatible_surface: None,
            }))
            .ok()?;
            let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("rustyboi_test_device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            }))
            .ok()?;
            Some((device, queue))
        }

        /// A whole-texture allocation delta (`pos == None`) — what egui emits the
        /// first time it uploads the font atlas.
        fn full_alloc(size: usize, color: Color32) -> ImageDelta {
            let image = ColorImage::new([size, size], vec![color; size * size]);
            ImageDelta { image: ImageData::Color(Arc::new(image)), options: TextureOptions::default(), pos: None }
        }

        /// A sub-region patch delta (`pos == Some`) — what egui emits when it
        /// packs new glyphs into an already-allocated atlas.
        fn patch(size: usize, at: [usize; 2]) -> ImageDelta {
            let image = ColorImage::new([size, size], vec![Color32::from_rgb(10, 20, 30); size * size]);
            ImageDelta { image: ImageData::Color(Arc::new(image)), options: TextureOptions::default(), pos: Some(at) }
        }

        fn set(id: TextureId, delta: ImageDelta) -> TexturesDelta {
            TexturesDelta { set: vec![(id, delta)], free: vec![] }
        }

        /// Run `f`, returning whether it panicked, without the default hook
        /// spamming the panic to stderr (we *expect* the panic here).
        fn silently_catches_panic(f: impl FnOnce() + std::panic::UnwindSafe) -> bool {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let r = std::panic::catch_unwind(f);
            std::panic::set_hook(prev);
            r.is_err()
        }

        // The failure mode itself: a partial update to a texture that was never
        // allocated is exactly what a dropped delta leaves egui-wgpu to choke on.
        // This pins *why* losing frame 0's allocation is fatal (not merely a
        // blank frame) so the invariant the fix upholds can't be quietly removed.
        #[test]
        fn partial_update_without_allocation_panics() {
            let Some((device, queue)) = headless_gpu() else {
                eprintln!("skipping partial_update_without_allocation_panics: no wgpu adapter");
                return;
            };
            let mut egui = EguiCompositor::new(&device, wgpu::TextureFormat::Rgba8Unorm);
            let deltas = set(TextureId::Managed(0), patch(2, [0, 0]));
            let panicked = silently_catches_panic(std::panic::AssertUnwindSafe(|| {
                egui.apply_textures(&device, &queue, &deltas);
            }));
            assert!(
                panicked,
                "a partial (pos=Some) update to an unallocated texture must panic — \
                 this is the crash a dropped frame-0 allocation causes"
            );
        }

        // The regression guard: the macOS sequence exactly. Frame 0 allocates the
        // atlas but its surface acquisition fails (so nothing is painted); frame 1
        // patches the atlas. Because the allocation is applied independent of the
        // surface, frame 1 must NOT panic. This fails against the pre-fix ordering.
        #[test]
        fn atlas_allocation_survives_a_skipped_surface_frame() {
            let Some((device, queue)) = headless_gpu() else {
                eprintln!("skipping atlas_allocation_survives_a_skipped_surface_frame: no wgpu adapter");
                return;
            };
            let mut egui = EguiCompositor::new(&device, wgpu::TextureFormat::Rgba8Unorm);
            let id = TextureId::Managed(0);

            // Frame 0: atlas allocation. `render` applies texture deltas before
            // acquiring the surface, so this lands even though the frame is then
            // skipped (Retina `Outdated`). Model that skip by applying only the
            // textures and rendering nothing.
            egui.apply_textures(&device, &queue, &set(id, full_alloc(8, Color32::WHITE)));

            // Frame 1: a partial glyph patch to the same atlas. Must succeed
            // because the base allocation from frame 0 is present.
            egui.apply_textures(&device, &queue, &set(id, patch(2, [1, 1])));
            queue.submit(std::iter::empty());
        }

        /// Lay out a real egui frame that paints a solid `color` rect over the
        /// whole `w`x`h` viewport, returning the paint output ready for `render`.
        /// The mesh samples the font atlas' white texel, so the atlas is uploaded
        /// too.
        fn full_screen_egui_frame(w: u32, h: u32, color: Color32) -> EguiPaint {
            let ctx = egui::Context::default();
            let raw_input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(w as f32, h as f32),
                )),
                ..Default::default()
            };
            let full_output = ctx.run_ui(raw_input, |ui| {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w as f32, h as f32)),
                    0.0,
                    color,
                );
            });
            let ppp = ctx.pixels_per_point();
            let jobs = ctx.tessellate(full_output.shapes, ppp);
            EguiPaint { jobs, textures: full_output.textures_delta, pixels_per_point: ppp, reuse: false }
        }

        fn region(w: u32, h: u32) -> PhysicalRect {
            PhysicalRect { x: 0.0, y: 0.0, width: w as f32, height: h as f32 }
        }

        // End-to-end: a real egui frame driven through the whole `Renderer::render`
        // path (game pass + egui pass) onto an offscreen target must produce
        // visible pixels. Guards the other half of the symptom — "the window opens
        // but no egui elements appear."
        #[test]
        fn render_composites_visible_egui_pixels() {
            let Some((device, queue)) = headless_gpu() else {
                eprintln!("skipping render_composites_visible_egui_pixels: no wgpu adapter");
                return;
            };
            const W: u32 = 64;
            const H: u32 = 64;
            let mut renderer =
                Renderer::new_offscreen(device, queue, wgpu::TextureFormat::Rgba8Unorm, W, H);

            let paint = full_screen_egui_frame(W, H, Color32::from_rgb(220, 30, 30));
            renderer.render(None, region(W, H), paint).expect("offscreen render must succeed");

            let px = renderer.read_offscreen();
            let red = px
                .chunks_exact(4)
                .filter(|p| p[0] > 128 && p[1] < 128 && p[2] < 128)
                .count();
            assert!(
                red > (W * H / 2) as usize,
                "egui composite produced no visible pixels ({red} red of {}) — the UI would be blank",
                W * H
            );
        }

        // THE regression guard, exercised end-to-end through `Renderer::render`:
        // the macOS first-frame sequence exactly. Frame 0 carries the atlas
        // allocation but the surface reports `Outdated` (skip + return Err); frame
        // 1 carries a partial atlas update and must render. Against the pre-fix
        // ordering — texture deltas applied only after a *successful* acquire —
        // frame 0's allocation is dropped and frame 1 panics ("Tried to update a
        // texture that has not been allocated yet"). With the fix it renders.
        #[test]
        fn render_keeps_egui_textures_across_a_surface_error() {
            let Some((device, queue)) = headless_gpu() else {
                eprintln!("skipping render_keeps_egui_textures_across_a_surface_error: no wgpu adapter");
                return;
            };
            const W: u32 = 64;
            const H: u32 = 64;
            let mut renderer =
                Renderer::new_offscreen(device, queue, wgpu::TextureFormat::Rgba8Unorm, W, H);
            // Frame 0 errors (Retina first-frame `Outdated`); frame 1 succeeds.
            renderer.inject_statuses([wgpu::SurfaceStatus::Outdated, wgpu::SurfaceStatus::Good]);

            let id = TextureId::Managed(0);

            // Frame 0: allocate the atlas. `render` must return the surface error
            // *after* applying the allocation (not drop it).
            let frame0 = EguiPaint {
                jobs: Vec::new(),
                textures: set(id, full_alloc(8, Color32::WHITE)),
                pixels_per_point: 1.0,
                reuse: false,
            };
            let r0 = renderer.render(None, region(W, H), frame0);
            assert!(
                matches!(r0, Err(wgpu::SurfaceStatus::Outdated)),
                "frame 0 should report the injected surface error, got {r0:?}"
            );

            // Frame 1: a partial atlas patch. Succeeds only because frame 0's
            // allocation survived the skipped frame.
            let frame1 = EguiPaint {
                jobs: Vec::new(),
                textures: set(id, patch(2, [1, 1])),
                pixels_per_point: 1.0,
                reuse: false,
            };
            renderer
                .render(None, region(W, H), frame1)
                .expect("frame 1 must render after a skipped frame 0 — a panic here is the bug");
        }
    }
}

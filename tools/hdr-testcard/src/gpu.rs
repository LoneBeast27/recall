//! wgpu device + surface selection + scissored solid-colour fills.
//!
//! Surface selection is the load-bearing part: to make HDR patches (values
//! above 1.0) meaningful we need an `Rgba16Float` swapchain, which on the
//! DXGI/DX12 backend carries the extended-sRGB (scRGB-linear) colour space that
//! Windows composites without clamping when HDR is on. If only an 8-bit sRGB
//! surface is offered we take it and the fragment output is hardware-clamped to
//! [0,1] on store — reported honestly as `surface="srgb8"` in the startup JSON.
//!
//! Rendering is deliberately shader-light: a single solid-colour pipeline plus
//! per-patch scissor rects. Each patch = one render pass writing its commanded
//! LINEAR value into its scissored region, so a screen capture reads the value
//! back verbatim (Rgba16Float) or clamped (sRGB8). No tonemap, no gamma fiddling
//! in the shader — the surface format alone decides whether >1.0 survives.

use std::sync::Arc;

use winit::window::Window;

use crate::config::{Mode, Patch, GRID_COLS, GRID_ROWS};

/// Which surface class we actually obtained. Drives the startup JSON's
/// `"surface"` field and whether >1.0 values can survive to the display.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    /// FP16 extended-sRGB (scRGB-linear). Values >1.0 pass through.
    Rgba16Float,
    /// 8-bit sRGB. Values are clamped to [0,1] on store.
    Srgb8,
}

impl SurfaceKind {
    pub fn label(&self) -> &'static str {
        match self {
            SurfaceKind::Rgba16Float => "rgba16f",
            SurfaceKind::Srgb8 => "srgb8",
        }
    }
}

/// A 16-byte uniform holding one patch's linear RGBA (matches `Color` in
/// solid.wgsl).
#[repr(C)]
#[derive(Clone, Copy)]
struct ColorUniform {
    rgba: [f32; 4],
}

/// Raw byte view of the uniform for `write_buffer` (avoids a bytemuck dep since
/// the struct is trivially `Copy` and `#[repr(C)]`).
fn color_bytes(c: &ColorUniform) -> &[u8] {
    // SAFETY: `ColorUniform` is `#[repr(C)]`, `Copy`, contains only `f32`, and
    // has no padding (four contiguous f32 = 16 bytes).
    unsafe {
        std::slice::from_raw_parts(
            (c as *const ColorUniform) as *const u8,
            std::mem::size_of::<ColorUniform>(),
        )
    }
}

pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    kind: SurfaceKind,
    mode: Mode,
    /// Kept last so it drops after the surface that borrows from it.
    _window: Arc<Window>,
}

impl Gpu {
    /// The surface class we obtained, for the startup JSON.
    pub fn surface_kind(&self) -> SurfaceKind {
        self.kind
    }

    pub fn new(window: Arc<Window>, mode: Mode) -> Self {
        let size = window.inner_size();
        let (width, height) = (size.width.max(1), size.height.max(1));

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("request adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("hdr-testcard-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .expect("request device");

        // Surface selection: prefer an Rgba16Float (scRGB / extended-sRGB)
        // format so HDR patch values above 1.0 survive; otherwise fall back to
        // an 8-bit sRGB format and accept the [0,1] clamp.
        let caps = surface.get_capabilities(&adapter);
        let (format, kind) = select_surface_format(&caps);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("solid-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("solid.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("solid-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("solid-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("solid-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("solid-color"),
            size: std::mem::size_of::<ColorUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("solid-bind-group"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });

        Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            uniform,
            kind,
            mode,
            _window: window,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Draw the current pattern and present. Returns false on a lost surface.
    pub fn render(&mut self) -> bool {
        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            Cst::Success(t) | Cst::Suboptimal(t) => t,
            Cst::Outdated | Cst::Lost => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            Cst::Timeout | Cst::Occluded | Cst::Validation => return false,
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("testcard-encoder"),
            });

        let (w, h) = (self.config.width, self.config.height);
        let rects = self.viewport_rects(w, h);

        // First pass clears the whole surface to black (the grid's gutter);
        // subsequent passes LOAD and fill only their scissor rect. A per-patch
        // pass is used so each rect gets its own commanded colour without a
        // shader-side branch.
        for (i, (patch, rect)) in rects.iter().enumerate() {
            let color = ColorUniform {
                rgba: [patch.r, patch.g, patch.b, 1.0],
            };
            self.queue
                .write_buffer(&self.uniform, 0, color_bytes(&color));
            // The uniform is single-buffered, so we must submit each patch
            // before overwriting it. One submit per patch keeps this correct
            // and is trivially cheap for <=8 patches at Fifo present.
            let load = if i == 0 {
                wgpu::LoadOp::Clear(wgpu::Color::BLACK)
            } else {
                wgpu::LoadOp::Load
            };
            let mut enc = if i == 0 {
                std::mem::replace(
                    &mut encoder,
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None }),
                )
            } else {
                self.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("testcard-patch"),
                    })
            };
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("testcard-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_scissor_rect(rect.0, rect.1, rect.2, rect.3);
                pass.draw(0..3, 0..1);
            }
            self.queue.submit([enc.finish()]);
        }

        frame.present();
        true
    }

    /// Compute (patch, scissor-rect) pairs for the current mode. Grid mode lays
    /// out `GRID_COLS x GRID_ROWS` cells with a small black gutter between them;
    /// solid mode is a single full-window rect.
    fn viewport_rects(&self, w: u32, h: u32) -> Vec<(Patch, (u32, u32, u32, u32))> {
        match &self.mode {
            Mode::Solid { r, g, b } => {
                vec![(
                    Patch {
                        name: "solid",
                        r: *r,
                        g: *g,
                        b: *b,
                    },
                    (0, 0, w, h),
                )]
            }
            Mode::Grid => {
                let gutter = 6u32;
                let cell_w = w / GRID_COLS;
                let cell_h = h / GRID_ROWS;
                let patches = self.mode.patches();
                let mut out = Vec::with_capacity(patches.len());
                for (idx, patch) in patches.iter().enumerate() {
                    let col = idx as u32 % GRID_COLS;
                    let row = idx as u32 / GRID_COLS;
                    let x = col * cell_w + gutter;
                    let y = row * cell_h + gutter;
                    let rw = cell_w.saturating_sub(gutter * 2).max(1);
                    let rh = cell_h.saturating_sub(gutter * 2).max(1);
                    // Guard against a scissor extending past the surface.
                    let rw = rw.min(w.saturating_sub(x)).max(1);
                    let rh = rh.min(h.saturating_sub(y)).max(1);
                    out.push((*patch, (x, y, rw, rh)));
                }
                out
            }
        }
    }
}

/// Pick the best available surface format: an `Rgba16Float` if the driver
/// exposes one (HDR / scRGB path), otherwise the first sRGB 8-bit format,
/// otherwise whatever is first. Returns the format plus its classification.
fn select_surface_format(caps: &wgpu::SurfaceCapabilities) -> (wgpu::TextureFormat, SurfaceKind) {
    if let Some(f) = caps
        .formats
        .iter()
        .copied()
        .find(|f| *f == wgpu::TextureFormat::Rgba16Float)
    {
        return (f, SurfaceKind::Rgba16Float);
    }
    if let Some(f) = caps.formats.iter().copied().find(|f| f.is_srgb()) {
        return (f, SurfaceKind::Srgb8);
    }
    // Last resort: first format offered. Classify by float-ness for honesty.
    let f = caps.formats[0];
    let kind = if f == wgpu::TextureFormat::Rgba16Float {
        SurfaceKind::Rgba16Float
    } else {
        SurfaceKind::Srgb8
    };
    (f, kind)
}

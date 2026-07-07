//! wgpu device/surface + HDR->SDR tonemap pass for Recall's output window.
//!
//! Each redraw: take the latest captured frame (Rgba16F / scRGB-linear), upload
//! it to a sampled texture, and run `tonemap.wgsl` over a fullscreen triangle
//! into the swapchain. The swapchain is an *-Srgb format so the shader emits
//! linear SDR and the hardware does the sRGB encode.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use winit::window::Window;

use crate::capture::CaptureShared;

/// Matches `Params` in tonemap.wgsl (std140-friendly: 32 bytes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: u32,
    sdr_white: f32,
    hdr_white: f32,
    _pad0: f32,
    /// NDC scale applied in the vertex shader to letterbox the source into the
    /// window without distortion. (1,1) = fill; one axis shrinks otherwise.
    scale: [f32; 2],
    _pad1: [f32; 2],
}

/// Aspect-preserving fit of a `src` rectangle into a `win` rectangle, returned
/// as an NDC scale for the fullscreen triangle.
fn aspect_scale(src_w: u32, src_h: u32, win_w: u32, win_h: u32) -> [f32; 2] {
    if src_w == 0 || src_h == 0 || win_w == 0 || win_h == 0 {
        return [1.0, 1.0];
    }
    let src_aspect = src_w as f32 / src_h as f32;
    let win_aspect = win_w as f32 / win_h as f32;
    if win_aspect > src_aspect {
        // Window wider than source -> bars left/right.
        [src_aspect / win_aspect, 1.0]
    } else {
        // Window taller than source -> bars top/bottom.
        [1.0, win_aspect / src_aspect]
    }
}

/// The uploaded source frame + its bind group, recreated on resolution change.
struct Source {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

/// Offscreen tonemap target + readback staging for the encoder, recreated on
/// source-resolution change. Same shader as the window pass, scale (1,1) —
/// the clip is native capture resolution, WYSIWYG with the live SDR window.
struct EncodeTarget {
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    staging: wgpu::Buffer,
    width: u32,
    height: u32,
    /// Row stride in the staging buffer (COPY_BYTES_PER_ROW_ALIGNMENT-padded).
    padded_bpr: u32,
    // Kept alive for the view's lifetime.
    _texture: wgpu::Texture,
}

pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    encode_pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    encode_uniform: wgpu::Buffer,
    source: Option<Source>,
    encode: Option<EncodeTarget>,
    shared: Arc<CaptureShared>,
    /// Kept last so it drops after the surface that borrows from it.
    _window: Arc<Window>,
}

impl Gpu {
    pub fn new(window: Arc<Window>, shared: Arc<CaptureShared>) -> Self {
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
            label: Some("recall-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .expect("request device");

        // Prefer an sRGB swapchain so the shader can emit linear SDR.
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
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
            label: Some("tonemap-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tonemap.wgsl").into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tonemap-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
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
                    // Vertex stage reads `scale` (letterbox); fragment reads tonemap params.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tonemap-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tonemap-pipeline"),
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

        // Same shader/layout into the encoder's Rgba8UnormSrgb offscreen target.
        let encode_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tonemap-encode-pipeline"),
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
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("src-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tonemap-params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let encode_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tonemap-encode-params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            encode_pipeline,
            bgl,
            sampler,
            uniform,
            encode_uniform,
            source: None,
            encode: None,
            shared,
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

    /// Resolution of the currently uploaded captured frame, if any.
    pub fn source_size(&self) -> Option<(u32, u32)> {
        self.source.as_ref().map(|s| (s.width, s.height))
    }

    /// (Re)create the source texture + bind group for a given capture resolution.
    fn ensure_source(&mut self, width: u32, height: u32) {
        if matches!(&self.source, Some(s) if s.width == width && s.height == height) {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("captured-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tonemap-bind-group"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.uniform.as_entire_binding(),
                },
            ],
        });
        self.source = Some(Source {
            texture,
            bind_group,
            width,
            height,
        });
    }

    /// Pull the freshest captured frame (if any) and upload it to the GPU.
    fn upload_latest(&mut self) {
        let Some(data) = self.shared.latest.lock().ok().and_then(|mut s| s.take()) else {
            return;
        };
        self.ensure_source(data.width, data.height);

        let params = Params {
            mode: self.shared.mode.load(Ordering::Relaxed),
            sdr_white: 1.0,
            hdr_white: 6.0,
            _pad0: 0.0,
            scale: aspect_scale(data.width, data.height, self.config.width, self.config.height),
            _pad1: [0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&params));

        let source = self.source.as_ref().expect("source created above");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data.bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(data.width * 8), // Rgba16F = 8 bytes/pixel
                rows_per_image: Some(data.height),
            },
            wgpu::Extent3d {
                width: data.width,
                height: data.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// (Re)create the offscreen encode target for the current source. The bind
    /// group references the live source texture, so this must run after
    /// `ensure_source` whenever the resolution changed.
    fn ensure_encode_target(&mut self, width: u32, height: u32) {
        if matches!(&self.encode, Some(t) if t.width == width && t.height == height) {
            return;
        }
        let Some(source) = &self.source else { return };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("encode-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let src_view = source
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("encode-bind-group"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.encode_uniform.as_entire_binding(),
                },
            ],
        });
        let padded_bpr = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("encode-staging"),
            size: padded_bpr as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        self.encode = Some(EncodeTarget {
            view,
            bind_group,
            staging,
            width,
            height,
            padded_bpr,
            _texture: texture,
        });
    }

    /// Tonemap the current source into the offscreen target and read it back
    /// as tightly packed RGBA8. Synchronous (blocks ~1-2ms); called at the
    /// pacer cadence, not per swapchain frame. None until a frame exists.
    pub fn encode_frame(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let (sw, sh) = self.source.as_ref().map(|s| (s.width, s.height))?;
        self.ensure_encode_target(sw, sh);

        let params = Params {
            mode: self.shared.mode.load(Ordering::Relaxed),
            sdr_white: 1.0,
            hdr_white: 6.0,
            _pad0: 0.0,
            scale: [1.0, 1.0], // native-resolution fill, no letterbox
            _pad1: [0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.encode_uniform, 0, bytemuck::bytes_of(&params));

        let et = self.encode.as_ref()?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("encode-readback"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tonemap-encode-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &et.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.encode_pipeline);
            pass.set_bind_group(0, &et.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &et._texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &et.staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(et.padded_bpr),
                    rows_per_image: Some(sh),
                },
            },
            wgpu::Extent3d {
                width: sw,
                height: sh,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = et.staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;

        let mut out = Vec::with_capacity((sw * sh * 4) as usize);
        {
            let mapped = slice.get_mapped_range();
            let row_bytes = (sw * 4) as usize;
            for row in 0..sh as usize {
                let s = row * et.padded_bpr as usize;
                out.extend_from_slice(&mapped[s..s + row_bytes]);
            }
        }
        et.staging.unmap();
        Some((sw, sh, out))
    }

    /// Upload the newest frame and present it. Returns false on a lost surface.
    pub fn render(&mut self) -> bool {
        self.upload_latest();

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
                label: Some("recall-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tonemap-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Only draw once we have a frame to show; otherwise the clear stands.
            if let Some(source) = &self.source {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &source.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        frame.present();
        true
    }
}

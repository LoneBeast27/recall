//! The encoder / screenshot readback path: tonemap the current source into the
//! offscreen Rgba8UnormSrgb target and read it back as packed RGBA8. A windowed game's
//! client area is drawn into its place on the canvas (`framing::Layout`); the rest of the
//! target keeps the black clear.

use std::sync::atomic::Ordering;

use super::{EncodeTarget, Gpu, Params};
use crate::framing::{Layout, Rect};

/// A read-back encoder frame: (width, height, RGBA8, the picture's region on the canvas).
pub type EncodedRgba = (u32, u32, Vec<u8>, Option<Rect>);

impl Gpu {
    /// (Re)create the offscreen encode target for the current source. The bind
    /// group references the live source texture, so it is remade whenever the
    /// source (re)created by `ensure_source` changed size, as well as the target.
    fn ensure_encode_target(&mut self, width: u32, height: u32) {
        let Some(source) = &self.source else { return };
        let src = (source.width, source.height);
        if matches!(&self.encode, Some(t) if t.width == width && t.height == height && t.source == src) {
            return;
        }
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
            source: src,
            padded_bpr,
            _texture: texture,
        });
    }

    /// Tonemap the current source into the offscreen target and read it back
    /// as tightly packed RGBA8. Synchronous (blocks ~1-2ms); called at the
    /// pacer cadence, not per swapchain frame. None until a frame exists.
    pub fn encode_frame(&mut self) -> Option<EncodedRgba> {
        let (size, canvas) = self.source.as_ref().map(|s| ((s.width, s.height), s.canvas))?;
        crate::framing::note_source_height(canvas.map_or(size.1, |c| c.1));
        let layout = match canvas {
            Some(c) => Layout::on_canvas(Rect::full(size), crate::framing::capped(c, crate::framing::record_height())),
            None => {
                let height = crate::framing::record_height();
                Layout::sized(size, crate::framing::capped(size, height), height)
            }
        };
        let (sw, sh) = layout.out;
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
            // The fullscreen triangle fills the viewport: the source, 1:1 unless a client
            // area larger than the canvas is scaled down onto it.
            let d = layout.dst;
            pass.set_viewport(d.x as f32, d.y as f32, d.w as f32, d.h as f32, 0.0, 1.0);
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
        Some((sw, sh, out, layout.content()))
    }
}

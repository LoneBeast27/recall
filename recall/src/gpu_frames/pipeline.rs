//! The capture-thread half of the zero-copy path: one fullscreen pass of `tonemap.hlsl` from
//! the WGC texture into a texture from an FFmpeg D3D11 frames pool, all on the capture
//! device. Per frame the CPU only records a draw; pixels never leave the GPU.

use windows::core::{s, Interface, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_SRV_DIMENSION_TEXTURE2D, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

use super::fence::GpuFence;

use super::av::{self, BufRef, DeviceLock, HwFrame};
use super::nv12::{self, Nv12};
use crate::framing::{Layout, Rect};

const SHADER: &str = include_str!("tonemap.hlsl");

/// Matches `cbuffer Params` in tonemap.hlsl (four 16-byte registers).
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
struct Params {
    mode: u32,
    sdr_white: f32,
    hdr_white: f32,
    scaled: u32,
    src_rect: [i32; 4],
    dst_rect: [i32; 4],
    src_srgb: u32,
    pad: [u32; 3],
}

fn rect4(r: Rect) -> [i32; 4] {
    [r.x as i32, r.y as i32, r.w as i32, r.h as i32]
}

/// A source copy for capture textures that cannot be bound as a shader resource.
struct SourceCopy {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    desc: (u32, u32, DXGI_FORMAT),
}

pub struct Pipeline {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    raster: ID3D11RasterizerState,
    /// Bilinear, clamped: only used when a client area is scaled down onto the canvas.
    sampler: ID3D11SamplerState,
    params: ID3D11Buffer,
    last_params: Option<Params>,
    av_device: BufRef,
    frames: Option<(BufRef, u32, u32)>,
    /// Render-target views of pool textures, keyed by (texture pointer, array index).
    rtvs: Vec<(usize, u32, ID3D11RenderTargetView)>,
    copy: Option<SourceCopy>,
    /// Capture texture format and bind flags already logged (once per pipeline).
    logged_source: bool,
    /// Signalled once the GPU is done with the capture surface (None without D3D11.4: no
    /// wait). `armed`: this frame's signal was queued.
    done: Option<GpuFence>,
    armed: bool,
    /// BGRA -> NV12 before NVENC (`nv12.rs`); None = NVENC converts BGRA itself.
    nv12: Option<Nv12>,
    /// The tonemap's BGRA output when the frames are NV12 (the processor reads it).
    canvas: Option<(ID3D11Texture2D, (u32, u32))>,
}

/// Marks an error from the NV12 stage: the pipeline falls back to BGRA frames on it.
const NV12_FAILED: &str = "NV12 stage: ";

// SAFETY: a Pipeline is created and used on the capture thread only; it moves there once.
unsafe impl Send for Pipeline {}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, String> {
    let mut code = None;
    let mut errors = None;
    let res = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("tonemap.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = res {
        let detail = errors
            .map(|b: ID3DBlob| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize())).into_owned()
            })
            .unwrap_or_default();
        return Err(format!("tonemap.hlsl: {e} {detail}"));
    }
    code.ok_or_else(|| "tonemap.hlsl: no bytecode".into())
}

fn blob(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
}

impl Pipeline {
    /// Build the pass on the capture device and wrap the device for FFmpeg/NVENC.
    pub fn new(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self, String> {
        Self::build(device, context, true)
    }

    /// The pipeline with BGRA frames (NVENC converts), as before the NV12 stage (tests compare).
    #[cfg(test)]
    pub fn new_bgra(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self, String> {
        Self::build(device, context, false)
    }

    fn build(device: &ID3D11Device, context: &ID3D11DeviceContext, nv12: bool) -> Result<Self, String> {
        // NVENC, the capture thread and readbacks share this device across threads.
        if let Ok(mt) = context.cast::<ID3D11Multithread>() {
            let _ = unsafe { mt.SetMultithreadProtected(true) };
        }
        super::priority::apply(device);
        let vs_code = compile(s!("vs_main"), s!("vs_5_0"))?;
        let ps_code = compile(s!("ps_main"), s!("ps_5_0"))?;
        let (mut vs, mut ps, mut raster, mut sampler, mut params) = (None, None, None, None, None);
        unsafe {
            device.CreateVertexShader(blob(&vs_code), None, Some(&mut vs)).map_err(|e| format!("vertex shader: {e}"))?;
            device.CreatePixelShader(blob(&ps_code), None, Some(&mut ps)).map_err(|e| format!("pixel shader: {e}"))?;
            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            };
            device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(|e| format!("rasterizer: {e}"))?;
            let sd = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&sd, Some(&mut sampler)).map_err(|e| format!("sampler: {e}"))?;
            let bd = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Params>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            device.CreateBuffer(&bd, None, Some(&mut params)).map_err(|e| format!("constant buffer: {e}"))?;
        }
        let av_device = av::device_ctx(device, context)?;
        let done = GpuFence::new(device);
        let nv12 = if nv12 { Nv12::for_capture(device, context) } else { None };
        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            vs: vs.ok_or("vertex shader missing")?,
            ps: ps.ok_or("pixel shader missing")?,
            raster: raster.ok_or("rasterizer missing")?,
            sampler: sampler.ok_or("sampler missing")?,
            params: params.ok_or("constant buffer missing")?,
            last_params: None,
            av_device,
            frames: None,
            rtvs: Vec::new(),
            copy: None,
            logged_source: false,
            done,
            armed: false,
            nv12,
            canvas: None,
        })
    }

    /// The frames pool for `width`x`height`, recreated when the target size changes.
    fn frames_for(&mut self, width: u32, height: u32) -> Result<&BufRef, String> {
        if !matches!(&self.frames, Some((_, w, h)) if *w == width && *h == height) {
            self.rtvs.clear();
            let pool = match self.nv12.as_mut() {
                Some(stage) => {
                    stage.forget_outputs();
                    // Marked, so `render` falls back to BGRA once instead of failing every frame.
                    av::frames_ctx_as(&self.av_device, width, height, ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12).map_err(|e| format!("{NV12_FAILED}frames pool: {e}"))?
                }
                None => av::frames_ctx(&self.av_device, width, height)?,
            };
            self.frames = Some((pool, width, height));
        }
        Ok(&self.frames.as_ref().expect("set above").0)
    }

    /// Size of the current frames pool (the encoder frame size), if any.
    pub fn target(&self) -> Option<(u32, u32)> {
        self.frames.as_ref().map(|(_, w, h)| (*w, *h))
    }

    fn rtv(&mut self, tex: &ID3D11Texture2D, index: u32) -> Result<ID3D11RenderTargetView, String> {
        let key = tex.as_raw() as usize;
        if let Some((_, _, v)) = self.rtvs.iter().find(|(k, i, _)| *k == key && *i == index) {
            return Ok(v.clone());
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        let mut view = None;
        let rd = if desc.ArraySize > 1 {
            Some(D3D11_RENDER_TARGET_VIEW_DESC {
                Format: desc.Format,
                ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2DARRAY,
                Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                    Texture2DArray: D3D11_TEX2D_ARRAY_RTV { MipSlice: 0, FirstArraySlice: index, ArraySize: 1 },
                },
            })
        } else {
            None
        };
        unsafe {
            self.device
                .CreateRenderTargetView(tex, rd.as_ref().map(|d| d as *const _), Some(&mut view))
                .map_err(|e| format!("frame render target: {e}"))?;
        }
        let view = view.ok_or("render target view missing")?;
        // Pool textures are recycled, so this stays small; cap it in case the pool churns.
        if self.rtvs.len() >= 32 {
            self.rtvs.remove(0);
        }
        self.rtvs.push((key, index, view.clone()));
        Ok(view)
    }

    /// A shader view of the capture texture; textures without the shader-resource bind flag
    /// are first copied (GPU-side) into one that has it.
    fn source_view(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11ShaderResourceView, String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut desc) };
        let mut view = None;
        if desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0 {
            let sd = srv_desc(desc.Format);
            unsafe { self.device.CreateShaderResourceView(src, Some(&sd), Some(&mut view)) }
                .map_err(|e| format!("capture shader view: {e}"))?;
            return view.ok_or_else(|| "capture shader view missing".into());
        }
        let key = (desc.Width, desc.Height, desc.Format);
        if !matches!(&self.copy, Some(c) if c.desc == key) {
            let cd = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut tex = None;
            unsafe { self.device.CreateTexture2D(&cd, None, Some(&mut tex)) }.map_err(|e| format!("capture copy: {e}"))?;
            let tex = tex.ok_or("capture copy missing")?;
            let sd = srv_desc(desc.Format);
            unsafe { self.device.CreateShaderResourceView(&tex, Some(&sd), Some(&mut view)) }
                .map_err(|e| format!("capture copy view: {e}"))?;
            self.copy = Some(SourceCopy { texture: tex, view: view.take().ok_or("capture copy view missing")?, desc: key });
        }
        let copy = self.copy.as_ref().expect("set above");
        unsafe { self.context.CopyResource(&copy.texture, src) };
        Ok(copy.view.clone())
    }

    /// The capture texture's format and bind flags, once per pipeline: shows whether frames
    /// are Bgra8 or fp16 and whether `source_view` needs its extra copy.
    pub fn source_note(&mut self, src: &ID3D11Texture2D) -> Option<String> {
        if std::mem::replace(&mut self.logged_source, true) {
            return None;
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut desc) };
        let srv = desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0;
        Some(format!(
            "capture texture {}x{} format {} bind 0x{:x} ({})",
            desc.Width,
            desc.Height,
            desc.Format.0,
            desc.BindFlags,
            if srv { "shader reads it directly" } else { "copied before each draw" }
        ))
    }

    /// Tonemap `src` into a fresh frame of `layout.out` size: `layout.src` texels into
    /// `layout.dst`, black around it. A Bgra8 source in passthrough that fills the frame
    /// pixel for pixel is copied instead (no draw).
    pub fn render(&mut self, src: &ID3D11Texture2D, layout: &Layout, mode: u32) -> Result<HwFrame, String> {
        match self.render_once(src, layout, mode) {
            Err(e) if e.starts_with(NV12_FAILED) => {
                eprintln!("[recall] frames: BGRA from here on, NVENC converts ({e})");
                self.nv12 = None;
                self.canvas = None;
                self.frames = None;
                self.rtvs.clear();
                self.render_once(src, layout, mode)
            }
            other => other,
        }
    }

    /// With NV12 frames, the exact BGRA picture of `frame` (the newest): the canvas the processor
    /// read it from, which holds the newest frame until the next one. Screenshots copy it, so
    /// they stay exact (full range, full chroma) also on a still screen that sends no frames.
    /// None with BGRA frames: those are the picture.
    pub fn picture(&self, frame: &HwFrame) -> Option<super::Picture> {
        let (canvas, _) = self.canvas.as_ref().filter(|_| self.nv12.is_some())?;
        Some(super::Picture { frame: frame.try_clone()?, canvas: canvas.clone() })
    }

    /// Sleep until the GPU has read the capture surface (typically 0.05-1.5 ms). Bounded: a hung
    /// GPU must not stall capture forever.
    fn wait_done(&self) {
        if let (Some(fence), true) = (&self.done, self.armed) {
            fence.wait(100);
        }
    }

    /// Whether frames are NV12 (the video processor converts) rather than BGRA.
    pub fn nv12(&self) -> bool {
        self.nv12.is_some()
    }

    /// The BGRA texture the tonemap draws into before the processor converts it.
    fn canvas_for(&mut self, size: (u32, u32)) -> Result<ID3D11Texture2D, String> {
        if let Some((tex, s)) = &self.canvas {
            if *s == size {
                return Ok(tex.clone());
            }
        }
        let tex = nv12::canvas(&self.device, size).map_err(|e| format!("{NV12_FAILED}{e}"))?;
        self.canvas = Some((tex.clone(), size));
        Ok(tex)
    }

    fn render_once(&mut self, src: &ID3D11Texture2D, layout: &Layout, mode: u32) -> Result<HwFrame, String> {
        let target = layout.out;
        let convert = self.nv12.is_some();
        let frame = HwFrame::get(self.frames_for(target.0, target.1)?)?;
        let (tex, index) = frame.texture().ok_or("frame has no texture")?;
        let tex = tex.clone();
        let hw = frame.device_hwctx().ok_or("frame has no device")?;
        let _lock = unsafe { DeviceLock::take(hw) };
        let mut sd = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut sd) };
        let src_srgb = sd.Format == DXGI_FORMAT_B8G8R8A8_UNORM;
        if src_srgb && direct_copy(layout) {
            let b = D3D11_BOX {
                left: layout.src.x,
                top: layout.src.y,
                front: 0,
                right: layout.src.x + layout.src.w,
                bottom: layout.src.y + layout.src.h,
                back: 1,
            };
            if convert {
                // Into the canvas first: it keeps the exact picture for screenshots (`picture`),
                // since the capture surface goes back to WGC when this returns.
                let canvas = self.canvas_for(target)?;
                unsafe { self.context.CopySubresourceRegion(&canvas, 0, 0, 0, 0, src, 0, Some(&b)) };
                let stage = self.nv12.as_mut().expect("checked above");
                stage.blt(&canvas, Rect::full(target), &tex, index, target).map_err(|e| format!("{NV12_FAILED}{e}"))?;
            } else {
                unsafe { self.context.CopySubresourceRegion(&tex, index, 0, 0, 0, src, 0, Some(&b)) };
            }
            unsafe {
                self.armed = self.done.as_mut().is_some_and(|f| f.signal(&self.context));
                self.context.Flush();
            }
            drop(_lock);
            self.wait_done();
            return Ok(frame);
        }
        let (draw_tex, draw_index) = if convert { (self.canvas_for(target)?, 0) } else { (tex.clone(), index) };
        let rtv = self.rtv(&draw_tex, draw_index)?;
        let srv = self.source_view(src)?;
        let params = Params {
            mode,
            sdr_white: 1.0,
            hdr_white: 6.0,
            scaled: u32::from(layout.src.size() != layout.dst.size()),
            src_rect: rect4(layout.src),
            dst_rect: rect4(layout.dst),
            src_srgb: u32::from(src_srgb),
            pad: [0; 3],
        };
        let ctx = &self.context;
        unsafe {
            if self.last_params != Some(params) {
                ctx.UpdateSubresource(&self.params, 0, None, (&params as *const Params).cast(), 0, 0);
                self.last_params = Some(params);
            }
            ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: target.0 as f32,
                Height: target.1 as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            ctx.Draw(3, 0);
            // Unbind so the capture texture is not held by the pipeline between frames.
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
        if convert {
            let stage = self.nv12.as_mut().expect("checked above");
            stage.blt(&draw_tex, Rect::full(target), &tex, index, target).map_err(|e| format!("{NV12_FAILED}{e}"))?;
        }
        let ctx = &self.context;
        unsafe {
            self.armed = self.done.as_mut().is_some_and(|f| f.signal(ctx));
            ctx.Flush();
        }
        drop(_lock);
        // The capture surface returns to WGC's one-buffer pool when the callback returns;
        // wait (sleeping, not spinning) until the GPU has read it.
        self.wait_done();
        Ok(frame)
    }
}

/// The source rect lands unscaled on the whole frame, so a copy is the same picture.
fn direct_copy(layout: &Layout) -> bool {
    layout.src.size() == layout.out && layout.dst == Rect::full(layout.out)
}

fn srv_desc(format: DXGI_FORMAT) -> D3D11_SHADER_RESOURCE_VIEW_DESC {
    D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 },
        },
    }
}

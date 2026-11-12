//! Shader steps that resize an NV12 picture on the GPU between the video processor and the
//! encoder ([`PictureScaler`]), and the export's default one, [`Cubic`]: a separable cubic
//! filter (swscale's default bicubic, widened for large reductions) in two passes per plane.
//!
//! Measured 2026-11-12: the D3D11 video processor's own downscale (NVIDIA driver) came out
//! softer than the CPU path's swscale bicubic (SSIM 0.9905 against 0.9918 on a 1440p -> 1080p
//! clip). This pass runs on any D3D11 feature level 11 GPU (GTX 10 series and up), and other
//! steps (an upscaler) implement the same trait.

use windows::core::{s, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_SRV_DIMENSION_TEXTURE2D, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC};

use windows::core::Interface;

const SHADER: &str = include_str!("scale.hlsl");

/// A GPU step that writes `source` (NV12, `from` pixels) into `target` (NV12, `to` pixels).
/// Both textures are on the stage's device; the device lock is held by the caller.
pub(crate) trait PictureScaler {
    fn scale(&mut self, source: &ID3D11Texture2D, from: (u32, u32), target: &ID3D11Texture2D, to: (u32, u32)) -> Result<(), String>;
}

/// Matches `cbuffer Params` in scale.hlsl (two 16-byte registers).
#[repr(C)]
#[derive(Clone, Copy)]
struct Pass {
    ratio: f32,
    support: f32,
    horizontal: i32,
    cosited: i32,
    src_len: i32,
    pad: [i32; 3],
}

/// One plane's intermediate (scaled along x, not yet along y).
struct Half {
    texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    rtv: ID3D11RenderTargetView,
}

pub(crate) struct Cubic {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    raster: ID3D11RasterizerState,
    pass: ID3D11Buffer,
    /// Luma and chroma intermediates for one (from, to) pair.
    halves: Option<((u32, u32), (u32, u32), Half, Half)>,
    /// Plane views of pool textures: (texture, chroma?) -> view.
    srvs: Vec<(usize, bool, ID3D11ShaderResourceView)>,
    rtvs: Vec<(usize, bool, ID3D11RenderTargetView)>,
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob, String> {
    let (mut code, mut errors) = (None, None);
    let res = unsafe {
        D3DCompile(SHADER.as_ptr().cast(), SHADER.len(), s!("scale.hlsl"), None, None, entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut errors))
    };
    if let Err(e) = res {
        let detail = errors.map(|b: ID3DBlob| String::from_utf8_lossy(blob(&b)).into_owned()).unwrap_or_default();
        return Err(format!("scale.hlsl: {e} {detail}"));
    }
    code.ok_or_else(|| "scale.hlsl: no bytecode".into())
}

fn blob(b: &ID3DBlob) -> &[u8] {
    // SAFETY: the blob owns this many bytes for its lifetime.
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
}

impl Cubic {
    pub(crate) fn new(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self, String> {
        let (vs_code, ps_code) = (compile(s!("vs_main"), s!("vs_5_0"))?, compile(s!("ps_main"), s!("ps_5_0"))?);
        let (mut vs, mut ps, mut raster, mut pass) = (None, None, None, None);
        unsafe {
            device.CreateVertexShader(blob(&vs_code), None, Some(&mut vs)).map_err(|e| format!("scale vertex shader: {e}"))?;
            device.CreatePixelShader(blob(&ps_code), None, Some(&mut ps)).map_err(|e| format!("scale pixel shader: {e}"))?;
            let rd = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ..Default::default() };
            device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(|e| format!("scale rasterizer: {e}"))?;
            let bd = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Pass>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            device.CreateBuffer(&bd, None, Some(&mut pass)).map_err(|e| format!("scale constants: {e}"))?;
        }
        Ok(Self {
            device: device.clone(),
            context: context.clone(),
            vs: vs.ok_or("scale vertex shader missing")?,
            ps: ps.ok_or("scale pixel shader missing")?,
            raster: raster.ok_or("scale rasterizer missing")?,
            pass: pass.ok_or("scale constants missing")?,
            halves: None,
            srvs: Vec::new(),
            rtvs: Vec::new(),
        })
    }

    fn half(&self, (w, h): (u32, u32), format: DXGI_FORMAT) -> Result<Half, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            ..Default::default()
        };
        let (mut texture, mut srv, mut rtv) = (None, None, None);
        unsafe {
            self.device.CreateTexture2D(&desc, None, Some(&mut texture)).map_err(|e| format!("scale intermediate: {e}"))?;
            let texture = texture.as_ref().ok_or("scale intermediate missing")?;
            self.device.CreateShaderResourceView(texture, None, Some(&mut srv)).map_err(|e| format!("scale intermediate view: {e}"))?;
            self.device.CreateRenderTargetView(texture, None, Some(&mut rtv)).map_err(|e| format!("scale intermediate target: {e}"))?;
        }
        Ok(Half { texture: texture.expect("checked"), srv: srv.ok_or("view missing")?, rtv: rtv.ok_or("target missing")? })
    }

    /// The luma (R8) or chroma (R8G8) plane of an NV12 texture as a shader input.
    fn plane_srv(&mut self, texture: &ID3D11Texture2D, chroma: bool) -> Result<ID3D11ShaderResourceView, String> {
        let key = texture.as_raw() as usize;
        if let Some((_, _, v)) = self.srvs.iter().find(|(k, c, _)| *k == key && *c == chroma) {
            return Ok(v.clone());
        }
        let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: if chroma { DXGI_FORMAT_R8G8_UNORM } else { DXGI_FORMAT_R8_UNORM },
            ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
        };
        let mut view = None;
        unsafe { self.device.CreateShaderResourceView(texture, Some(&desc), Some(&mut view)) }.map_err(|e| format!("NV12 plane view: {e}"))?;
        let view = view.ok_or("NV12 plane view missing")?;
        if self.srvs.len() >= 64 {
            self.srvs.drain(..2);
        }
        self.srvs.push((key, chroma, view.clone()));
        Ok(view)
    }

    /// The luma or chroma plane of an NV12 texture as a render target.
    fn plane_rtv(&mut self, texture: &ID3D11Texture2D, chroma: bool) -> Result<ID3D11RenderTargetView, String> {
        let key = texture.as_raw() as usize;
        if let Some((_, _, v)) = self.rtvs.iter().find(|(k, c, _)| *k == key && *c == chroma) {
            return Ok(v.clone());
        }
        let desc = D3D11_RENDER_TARGET_VIEW_DESC {
            Format: if chroma { DXGI_FORMAT_R8G8_UNORM } else { DXGI_FORMAT_R8_UNORM },
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } },
        };
        let mut view = None;
        unsafe { self.device.CreateRenderTargetView(texture, Some(&desc), Some(&mut view)) }.map_err(|e| format!("NV12 plane target: {e}"))?;
        let view = view.ok_or("NV12 plane target missing")?;
        if self.rtvs.len() >= 64 {
            self.rtvs.drain(..2);
        }
        self.rtvs.push((key, chroma, view.clone()));
        Ok(view)
    }

    /// One pass: `input` (`src_len` texels along the axis) into `output` (`size`).
    fn pass(&self, input: &ID3D11ShaderResourceView, output: &ID3D11RenderTargetView, size: (u32, u32), horizontal: bool, cosited: bool, src_len: u32, out_len: u32) {
        let ratio = src_len as f32 / out_len.max(1) as f32;
        let pass = Pass { ratio, support: 2.0 * ratio.max(1.0), horizontal: horizontal.into(), cosited: cosited.into(), src_len: src_len as i32, pad: [0; 3] };
        let ctx = &self.context;
        unsafe {
            ctx.UpdateSubresource(&self.pass, 0, None, (&pass as *const Pass).cast(), 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(output.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: size.0 as f32, Height: size.1 as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(input.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.pass.clone())]));
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
    }
}

impl PictureScaler for Cubic {
    fn scale(&mut self, source: &ID3D11Texture2D, from: (u32, u32), target: &ID3D11Texture2D, to: (u32, u32)) -> Result<(), String> {
        if !matches!(&self.halves, Some((f, t, _, _)) if *f == from && *t == to) {
            let luma = self.half((to.0, from.1), DXGI_FORMAT_R8_UNORM)?;
            let chroma = self.half((to.0 / 2, from.1 / 2), DXGI_FORMAT_R8G8_UNORM)?;
            self.halves = Some((from, to, luma, chroma));
        }
        let (src_y, src_uv) = (self.plane_srv(source, false)?, self.plane_srv(source, true)?);
        let (dst_y, dst_uv) = (self.plane_rtv(target, false)?, self.plane_rtv(target, true)?);
        let (_, _, luma, chroma) = self.halves.as_ref().expect("set above");
        let _ = (&luma.texture, &chroma.texture);
        // Luma: along x into the intermediate, then along y into the target.
        self.pass(&src_y, &luma.rtv, (to.0, from.1), true, false, from.0, to.0);
        self.pass(&luma.srv, &dst_y, to, false, false, from.1, to.1);
        // Chroma (half size), centred on both axes as swscale places it, so GPU and CPU
        // exports line up (cosited x measured 0.9901 SSIM on U against swscale's 0.9923).
        self.pass(&src_uv, &chroma.rtv, (to.0 / 2, from.1 / 2), true, false, from.0 / 2, to.0 / 2);
        self.pass(&chroma.srv, &dst_uv, (to.0 / 2, to.1 / 2), false, false, from.1 / 2, to.1 / 2);
        Ok(())
    }
}

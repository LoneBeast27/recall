//! FSR 1 upscale on any D3D11 GPU (GTX 10 series and up, any vendor): the video processor
//! turns the NV12 picture into RGB, AMD's FSR 1 upscales (EASU) and sharpens (RCAS) it in
//! two compute passes, and the video processor turns it back into NV12 for the encoder.
//! Vendor-neutral: this is the tier under a vendor's AI upscaler, and the one used while
//! that upscaler's driver switch is off.

use std::mem::ManuallyDrop;

use windows::core::{s, Interface, PCSTR};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_SRV_DIMENSION_TEXTURE2D};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const SHADER: &str = include_str!("fsr1/fsr1.hlsl");
const FFX_A: &str = include_str!("fsr1/ffx_a.h");
const FFX_FSR1: &str = include_str!("fsr1/ffx_fsr1.h");
/// RCAS sharpness in stops below the maximum (AMD's samples use 0.2).
const SHARPNESS_STOPS: f32 = 0.25;
const MAX_VIEWS: usize = 64;

/// Matches `cbuffer Sizes` in fsr1.hlsl.
#[repr(C)]
struct Sizes {
    sizes: [f32; 4],
    tuning: [f32; 4],
}

pub(crate) struct Fsr1 {
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    to_rgb: (ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor),
    to_yuv: (ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor),
    easu: ID3D11ComputeShader,
    rcas: ID3D11ComputeShader,
    sampler: ID3D11SamplerState,
    sizes: ID3D11Buffer,
    rgb_out_view: ID3D11VideoProcessorOutputView,
    rgb_srv: ID3D11ShaderResourceView,
    scaled_uav: ID3D11UnorderedAccessView,
    scaled_srv: ID3D11ShaderResourceView,
    sharp_uav: ID3D11UnorderedAccessView,
    sharp_in_view: ID3D11VideoProcessorInputView,
    input: (u32, u32),
    out: (u32, u32),
    inputs: Vec<(usize, u32, ID3D11VideoProcessorInputView)>,
    outputs: Vec<(usize, ID3D11VideoProcessorOutputView)>,
    colour: Option<(DXGI_COLOR_SPACE_TYPE, DXGI_COLOR_SPACE_TYPE)>,
}

impl Fsr1 {
    /// Fails (and the caller uses the plain scale) when the video processor cannot read RGBA8
    /// or the shaders do not compile on this GPU.
    pub(crate) fn new(device: &ID3D11Device, context: ID3D11DeviceContext, input: (u32, u32), out: (u32, u32)) -> Result<Self, String> {
        let video: ID3D11VideoDevice = device.cast().map_err(|e| format!("FSR 1 video device: {e}"))?;
        let video_context: ID3D11VideoContext1 = context.cast().map_err(|e| format!("FSR 1 video context: {e}"))?;
        let to_rgb = processor(&video, &video_context, input, input)?;
        let to_yuv = processor(&video, &video_context, out, out)?;
        let reads_rgba = unsafe { to_yuv.0.CheckVideoProcessorFormat(DXGI_FORMAT_R8G8B8A8_UNORM) }.map_err(|e| format!("FSR 1 RGBA input: {e}"))?;
        if reads_rgba & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 {
            return Err("FSR 1: the video processor cannot read RGBA".into());
        }
        let rgb = texture(device, input, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE)?;
        let scaled = texture(device, out, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_UNORDERED_ACCESS | D3D11_BIND_SHADER_RESOURCE)?;
        let both = D3D11_BIND_UNORDERED_ACCESS | D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
        let sharp = texture(device, out, DXGI_FORMAT_R8G8B8A8_UNORM, both)?;
        let (easu_code, rcas_code) = bytecode()?;
        let (easu, rcas) = unsafe {
            let (mut easu, mut rcas) = (None, None);
            device.CreateComputeShader(easu_code, None, Some(&mut easu)).map_err(|e| format!("FSR 1 EASU: {e}"))?;
            device.CreateComputeShader(rcas_code, None, Some(&mut rcas)).map_err(|e| format!("FSR 1 RCAS: {e}"))?;
            (easu.ok_or("FSR 1 EASU missing")?, rcas.ok_or("FSR 1 RCAS missing")?)
        };
        let fsr = Self {
            rgb_out_view: vp_output(&video, &to_rgb.0, &rgb)?,
            rgb_srv: srv(device, &rgb, DXGI_FORMAT_B8G8R8A8_UNORM)?,
            scaled_uav: uav(device, &scaled)?,
            scaled_srv: srv(device, &scaled, DXGI_FORMAT_R8G8B8A8_UNORM)?,
            sharp_uav: uav(device, &sharp)?,
            sharp_in_view: vp_input(&video, &to_yuv.0, &sharp, 0)?,
            sampler: sampler(device)?,
            sizes: constants(device, input, out)?,
            context,
            video,
            video_context,
            to_rgb,
            to_yuv,
            easu,
            rcas,
            input,
            out,
            inputs: Vec::new(),
            outputs: Vec::new(),
            colour: None,
        };
        Ok(fsr)
    }

    /// Upscales `source` (an NV12 decoder picture in colour space `from`, array `slice`)
    /// into `target` (NV12, the output size, colour space `to`). The caller holds the
    /// device lock.
    pub(crate) fn run(
        &mut self,
        source: &ID3D11Texture2D,
        slice: u32,
        target: &ID3D11Texture2D,
        from: DXGI_COLOR_SPACE_TYPE,
        to: DXGI_COLOR_SPACE_TYPE,
    ) -> Result<(), String> {
        if self.colour != Some((from, to)) {
            let rgb = rgb_space(from);
            unsafe {
                self.video_context.VideoProcessorSetStreamColorSpace1(&self.to_rgb.1, 0, from);
                self.video_context.VideoProcessorSetOutputColorSpace1(&self.to_rgb.1, rgb);
                self.video_context.VideoProcessorSetStreamColorSpace1(&self.to_yuv.1, 0, rgb);
                self.video_context.VideoProcessorSetOutputColorSpace1(&self.to_yuv.1, to);
            }
            self.colour = Some((from, to));
        }
        let input = self.input_view(source, slice)?;
        blt(&self.video_context, &self.to_rgb.1, &self.rgb_out_view, input)?;
        self.dispatch(&self.easu, &self.rgb_srv, &self.scaled_uav);
        self.dispatch(&self.rcas, &self.scaled_srv, &self.sharp_uav);
        let output = self.output_view(target)?;
        blt(&self.video_context, &self.to_yuv.1, &output, self.sharp_in_view.clone())
    }

    fn dispatch(&self, shader: &ID3D11ComputeShader, input: &ID3D11ShaderResourceView, output: &ID3D11UnorderedAccessView) {
        let groups = |n: u32| n.div_ceil(8);
        unsafe {
            let c = &self.context;
            c.CSSetShader(shader, None);
            c.CSSetConstantBuffers(0, Some(&[Some(self.sizes.clone())]));
            c.CSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            c.CSSetShaderResources(0, Some(&[Some(input.clone())]));
            c.CSSetUnorderedAccessViews(0, 1, Some(&Some(output.clone())), None);
            c.Dispatch(groups(self.out.0), groups(self.out.1), 1);
            // Unbind, so the next pass can read what this one wrote.
            c.CSSetShaderResources(0, Some(&[None]));
            c.CSSetUnorderedAccessViews(0, 1, Some(&None), None);
        }
    }

    fn input_view(&mut self, texture: &ID3D11Texture2D, slice: u32) -> Result<ID3D11VideoProcessorInputView, String> {
        let key = texture.as_raw() as usize;
        if let Some((_, _, view)) = self.inputs.iter().find(|(k, s, _)| *k == key && *s == slice) {
            return Ok(view.clone());
        }
        let view = vp_input(&self.video, &self.to_rgb.0, texture, slice)?;
        if self.inputs.len() >= MAX_VIEWS {
            self.inputs.remove(0);
        }
        self.inputs.push((key, slice, view.clone()));
        Ok(view)
    }

    fn output_view(&mut self, texture: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorOutputView, String> {
        let key = texture.as_raw() as usize;
        if let Some((_, view)) = self.outputs.iter().find(|(k, _)| *k == key) {
            return Ok(view.clone());
        }
        let view = vp_output(&self.video, &self.to_yuv.0, texture)?;
        if self.outputs.len() >= MAX_VIEWS {
            self.outputs.remove(0);
        }
        self.outputs.push((key, view.clone()));
        Ok(view)
    }
}

/// The GPU export's shader step for an upscale: the stage's processor has already written
/// the picture at the input size in the export's studio-range colour space; FSR 1 writes the
/// output size. The RGB round trip uses the BT.709 matrix both ways, which returns the same
/// YCbCr values whatever the clip's own matrix.
impl crate::export::gpu_scale::PictureScaler for Fsr1 {
    fn scale(&mut self, source: &ID3D11Texture2D, from: (u32, u32), target: &ID3D11Texture2D, to: (u32, u32)) -> Result<(), String> {
        if from != self.input || to != self.out {
            return Err(format!("FSR 1 was set up for {:?} -> {:?}, not {from:?} -> {to:?}", self.input, self.out));
        }
        let space = DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709;
        self.run(source, 0, target, space, space)
    }
}

/// The RGB space a YCbCr space converts to and back: same primaries and gamma, full range.
fn rgb_space(space: DXGI_COLOR_SPACE_TYPE) -> DXGI_COLOR_SPACE_TYPE {
    let wide = matches!(space, DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020 | DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020);
    if wide { DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P2020 } else { DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 }
}

/// Both passes' bytecode, compiled once per process (about 3.8k lines of HLSL at O3): every
/// export attempt, size-cap retries included, reuses it.
fn bytecode() -> Result<(&'static [u8], &'static [u8]), String> {
    static CODE: std::sync::OnceLock<Result<(Vec<u8>, Vec<u8>), String>> = std::sync::OnceLock::new();
    let code = CODE.get_or_init(|| {
        let source = expand_source();
        Ok((blob(&compile(&source, s!("easu"))?).to_vec(), blob(&compile(&source, s!("rcas"))?).to_vec()))
    });
    match code {
        Ok((easu, rcas)) => Ok((easu.as_slice(), rcas.as_slice())),
        Err(why) => Err(why.clone()),
    }
}

fn expand_source() -> String {
    SHADER.replace("//@@FFX_A@@", FFX_A).replace("//@@FFX_FSR1@@", FFX_FSR1)
}

fn compile(source: &str, entry: PCSTR) -> Result<ID3DBlob, String> {
    let (mut code, mut errors) = (None, None);
    let result = unsafe {
        D3DCompile(source.as_ptr().cast(), source.len(), s!("fsr1.hlsl"), None, None, entry, s!("cs_5_0"), D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &mut code, Some(&mut errors))
    };
    if let Err(e) = result {
        let detail = errors.map(|b: ID3DBlob| String::from_utf8_lossy(blob(&b)).into_owned()).unwrap_or_default();
        return Err(format!("fsr1.hlsl: {e} {detail}"));
    }
    code.ok_or_else(|| "fsr1.hlsl: no bytecode".into())
}

fn blob(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
}

fn processor(video: &ID3D11VideoDevice, context: &ID3D11VideoContext1, input: (u32, u32), out: (u32, u32)) -> Result<(ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor), String> {
    let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
    let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: rate,
        InputWidth: input.0,
        InputHeight: input.1,
        OutputFrameRate: rate,
        OutputWidth: out.0,
        OutputHeight: out.1,
        Usage: D3D11_VIDEO_USAGE_OPTIMAL_QUALITY,
    };
    let full = |(w, h): (u32, u32)| RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
    unsafe {
        let enumerator = video.CreateVideoProcessorEnumerator(&desc).map_err(|e| format!("FSR 1 video processor: {e}"))?;
        let processor = video.CreateVideoProcessor(&enumerator, 0).map_err(|e| format!("FSR 1 video processor: {e}"))?;
        context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
        context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
        context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&full(input)));
        context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&full(out)));
        context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full(out)));
        Ok((enumerator, processor))
    }
}

fn blt(context: &ID3D11VideoContext1, processor: &ID3D11VideoProcessor, output: &ID3D11VideoProcessorOutputView, input: ID3D11VideoProcessorInputView) -> Result<(), String> {
    let mut stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: ManuallyDrop::new(Some(input)), ..Default::default() };
    let result = unsafe { context.VideoProcessorBlt(processor, output, 0, std::slice::from_ref(&stream)) };
    unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
    result.map_err(|e| format!("FSR 1 video processor: {e}"))
}

fn texture(device: &ID3D11Device, (w, h): (u32, u32), format: DXGI_FORMAT, bind: D3D11_BIND_FLAG) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut out = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut out)) }.map_err(|e| format!("FSR 1 texture: {e}"))?;
    out.ok_or_else(|| "FSR 1 texture missing".into())
}

fn srv(device: &ID3D11Device, texture: &ID3D11Texture2D, format: DXGI_FORMAT) -> Result<ID3D11ShaderResourceView, String> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
    };
    let mut view = None;
    unsafe { device.CreateShaderResourceView(texture, Some(&desc), Some(&mut view)) }.map_err(|e| format!("FSR 1 SRV: {e}"))?;
    view.ok_or_else(|| "FSR 1 SRV missing".into())
}

fn uav(device: &ID3D11Device, texture: &ID3D11Texture2D) -> Result<ID3D11UnorderedAccessView, String> {
    let desc = D3D11_UNORDERED_ACCESS_VIEW_DESC {
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 } },
    };
    let mut view = None;
    unsafe { device.CreateUnorderedAccessView(texture, Some(&desc), Some(&mut view)) }.map_err(|e| format!("FSR 1 UAV: {e}"))?;
    view.ok_or_else(|| "FSR 1 UAV missing".into())
}

fn vp_input(video: &ID3D11VideoDevice, enumerator: &ID3D11VideoProcessorEnumerator, texture: &ID3D11Texture2D, slice: u32) -> Result<ID3D11VideoProcessorInputView, String> {
    let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        FourCC: 0,
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice } },
    };
    let mut view = None;
    unsafe { video.CreateVideoProcessorInputView(texture, enumerator, &desc, Some(&mut view)) }.map_err(|e| format!("FSR 1 input view: {e}"))?;
    view.ok_or_else(|| "FSR 1 input view missing".into())
}

fn vp_output(video: &ID3D11VideoDevice, enumerator: &ID3D11VideoProcessorEnumerator, texture: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorOutputView, String> {
    let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
    };
    let mut view = None;
    unsafe { video.CreateVideoProcessorOutputView(texture, enumerator, &desc, Some(&mut view)) }.map_err(|e| format!("FSR 1 output view: {e}"))?;
    view.ok_or_else(|| "FSR 1 output view missing".into())
}

fn sampler(device: &ID3D11Device) -> Result<ID3D11SamplerState, String> {
    let desc = D3D11_SAMPLER_DESC {
        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
        ComparisonFunc: D3D11_COMPARISON_NEVER,
        MaxLOD: f32::MAX,
        ..Default::default()
    };
    let mut out = None;
    unsafe { device.CreateSamplerState(&desc, Some(&mut out)) }.map_err(|e| format!("FSR 1 sampler: {e}"))?;
    out.ok_or_else(|| "FSR 1 sampler missing".into())
}

fn constants(device: &ID3D11Device, input: (u32, u32), out: (u32, u32)) -> Result<ID3D11Buffer, String> {
    let values = Sizes { sizes: [input.0 as f32, input.1 as f32, out.0 as f32, out.1 as f32], tuning: [SHARPNESS_STOPS, 0.0, 0.0, 0.0] };
    let desc = D3D11_BUFFER_DESC {
        ByteWidth: std::mem::size_of::<Sizes>() as u32,
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
        ..Default::default()
    };
    let data = D3D11_SUBRESOURCE_DATA { pSysMem: (&values as *const Sizes).cast(), SysMemPitch: 0, SysMemSlicePitch: 0 };
    let mut out = None;
    unsafe { device.CreateBuffer(&desc, Some(&data), Some(&mut out)) }.map_err(|e| format!("FSR 1 constants: {e}"))?;
    out.ok_or_else(|| "FSR 1 constants missing".into())
}

#[cfg(test)]
#[path = "fsr1_tests.rs"]
mod tests;

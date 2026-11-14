//! BGRA -> NV12 on the GPU's video processor, before NVENC. Without it NVENC takes the BGRA
//! frame and converts it itself (on NVIDIA, a CUDA kernel inside the driver; NVENC guide 6.4).
//! The processor writes BT.601 studio-range NV12, the same matrix and range NVENC uses for
//! RGB input (and FFmpeg tags RGB-fed streams as BT.470BG / limited), so a clip is tagged and
//! coloured as before.
//!
//! `RECALL_CAPTURE_NV12=0` keeps the BGRA pool (NVENC converts, as before this stage). Any
//! failure to build or run the processor also falls back to BGRA (logged once).
//!
//! Plain D3D11 video API: no vendor code.

use std::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::framing::Rect;

/// Views kept; the WGC pool and the NV12 pool reuse a few textures, so this never fills.
const MAX_VIEWS: usize = 32;

/// The input the processor reads: sRGB-encoded full-range RGB (WGC Bgra8, or the tonemap output).
pub(super) const INPUT_SPACE: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
/// The output: BT.601 matrix, studio range (what NVENC's own RGB conversion writes).
pub(super) const OUTPUT_SPACE: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601;

/// Tried unless `RECALL_CAPTURE_NV12=0`.
fn wanted() -> bool {
    std::env::var("RECALL_CAPTURE_NV12").map_or(true, |v| v.trim() != "0")
}

struct Processor {
    /// (input texture size, output size) it was made for.
    key: ((u32, u32), (u32, u32)),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// Source rect last set (the capture crop moves with window borders).
    source: Option<Rect>,
}

pub(super) struct Nv12 {
    video: ID3D11VideoDevice,
    context: ID3D11VideoContext1,
    current: Option<Processor>,
    inputs: Vec<(usize, ID3D11VideoProcessorInputView)>,
    outputs: Vec<(usize, u32, ID3D11VideoProcessorOutputView)>,
}

// SAFETY: like Pipeline, made and used on the capture thread only.
unsafe impl Send for Nv12 {}

fn full((w, h): (u32, u32)) -> RECT {
    RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 }
}

fn rect(r: Rect) -> RECT {
    RECT { left: r.x as i32, top: r.y as i32, right: (r.x + r.w) as i32, bottom: (r.y + r.h) as i32 }
}

impl Nv12 {
    fn new(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self, String> {
        let video: ID3D11VideoDevice = device.cast().map_err(|e| format!("no D3D11 video device: {e}"))?;
        let context: ID3D11VideoContext1 = context.cast().map_err(|e| format!("no D3D11 video context 1: {e}"))?;
        let mut stage = Self { video, context, current: None, inputs: Vec::new(), outputs: Vec::new() };
        // Prove the formats and colour spaces once at a common size, so a GPU that cannot do it
        // keeps the BGRA path from the first frame instead of failing mid-recording.
        stage.processor((1920, 1080), (1920, 1080))?;
        Ok(stage)
    }

    /// The stage for a capture device, or None (BGRA frames) when it is switched off or cannot
    /// run here; says which on stderr.
    pub(super) fn for_capture(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Option<Self> {
        if !wanted() {
            eprintln!("[recall] frames: BGRA, NVENC converts (RECALL_CAPTURE_NV12=0)");
            return None;
        }
        match Self::new(device, context) {
            Ok(stage) => {
                eprintln!("[recall] frames: NV12 from the video processor (BT.601 limited)");
                Some(stage)
            }
            Err(e) => {
                eprintln!("[recall] frames: BGRA, NVENC converts ({e})");
                None
            }
        }
    }

    fn processor(&mut self, input: (u32, u32), out: (u32, u32)) -> Result<&mut Processor, String> {
        if !matches!(&self.current, Some(p) if p.key == (input, out)) {
            self.current = None;
            self.inputs.clear();
            self.outputs.clear();
            self.current = Some(make(&self.video, &self.context, input, out)?);
        }
        Ok(self.current.as_mut().expect("set above"))
    }

    /// Convert `source` of `src` (texture slice 0) into `target` (slice `index`) of size `out`.
    /// The caller holds the device lock and flushes.
    pub(super) fn blt(&mut self, src: &ID3D11Texture2D, source: Rect, target: &ID3D11Texture2D, index: u32, out: (u32, u32)) -> Result<(), String> {
        let mut sd = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut sd) };
        let ctx = self.context.clone();
        let p = self.processor((sd.Width, sd.Height), out)?;
        if p.source != Some(source) {
            unsafe { ctx.VideoProcessorSetStreamSourceRect(&p.processor, 0, true, Some(&rect(source))) };
            p.source = Some(source);
        }
        let input = self.input_view(src)?;
        let output = self.output_view(target, index)?;
        let p = self.current.as_ref().expect("made above");
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: ManuallyDrop::new(Some(input)), ..Default::default() };
        let result = unsafe { ctx.VideoProcessorBlt(&p.processor, &output, 0, std::slice::from_ref(&stream)) };
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.map_err(|e| format!("video processor blt: {e}"))
    }

    fn input_view(&mut self, texture: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView, String> {
        let key = texture.as_raw() as usize;
        if let Some((_, view)) = self.inputs.iter().find(|(k, _)| *k == key) {
            return Ok(view.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let enumerator = &self.current.as_ref().ok_or("no video processor")?.enumerator;
        let mut view = None;
        unsafe { self.video.CreateVideoProcessorInputView(texture, enumerator, &desc, Some(&mut view)) }.map_err(|e| format!("video processor input view: {e}"))?;
        let view = view.ok_or("video processor input view missing")?;
        if self.inputs.len() >= MAX_VIEWS {
            self.inputs.remove(0);
        }
        self.inputs.push((key, view.clone()));
        Ok(view)
    }

    fn output_view(&mut self, texture: &ID3D11Texture2D, index: u32) -> Result<ID3D11VideoProcessorOutputView, String> {
        let key = texture.as_raw() as usize;
        if let Some((_, _, view)) = self.outputs.iter().find(|(k, i, _)| *k == key && *i == index) {
            return Ok(view.clone());
        }
        let mut td = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut td) };
        let desc = if td.ArraySize > 1 {
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2DARRAY,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2DArray: D3D11_TEX2D_ARRAY_VPOV { MipSlice: 0, FirstArraySlice: index, ArraySize: 1 } },
            }
        } else {
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            }
        };
        let enumerator = &self.current.as_ref().ok_or("no video processor")?.enumerator;
        let mut view = None;
        unsafe { self.video.CreateVideoProcessorOutputView(texture, enumerator, &desc, Some(&mut view)) }.map_err(|e| format!("video processor output view: {e}"))?;
        let view = view.ok_or("video processor output view missing")?;
        if self.outputs.len() >= MAX_VIEWS {
            self.outputs.remove(0);
        }
        self.outputs.push((key, index, view.clone()));
        Ok(view)
    }

    /// Drop cached output views (the NV12 pool was made again).
    pub(super) fn forget_outputs(&mut self) {
        self.outputs.clear();
    }
}

fn make(video: &ID3D11VideoDevice, context: &ID3D11VideoContext1, input: (u32, u32), out: (u32, u32)) -> Result<Processor, String> {
    let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
    let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: rate,
        InputWidth: input.0,
        InputHeight: input.1,
        OutputFrameRate: rate,
        OutputWidth: out.0,
        OutputHeight: out.1,
        // No scaling happens here (source rect == output size), so speed costs nothing.
        Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
    };
    unsafe {
        let enumerator = video.CreateVideoProcessorEnumerator(&desc).map_err(|e| format!("video processor {}x{} -> {}x{}: {e}", input.0, input.1, out.0, out.1))?;
        let reads = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM).map_err(|e| format!("video processor BGRA input: {e}"))?;
        let writes = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12).map_err(|e| format!("video processor NV12 output: {e}"))?;
        if reads & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0 || writes & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
            return Err(format!("video processor cannot read BGRA ({reads:#x}) or write NV12 ({writes:#x})"));
        }
        if let Ok(e1) = enumerator.cast::<ID3D11VideoProcessorEnumerator1>() {
            let ok = e1
                .CheckVideoProcessorFormatConversion(DXGI_FORMAT_B8G8R8A8_UNORM, INPUT_SPACE, DXGI_FORMAT_NV12, OUTPUT_SPACE)
                .map_err(|e| format!("video processor colour check: {e}"))?;
            if !ok.as_bool() {
                return Err("video processor cannot convert sRGB to BT.601 studio NV12".into());
            }
        }
        let processor = video.CreateVideoProcessor(&enumerator, 0).map_err(|e| format!("create video processor: {e}"))?;
        context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
        // No driver "enhancements": colour conversion only.
        context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
        context.VideoProcessorSetStreamColorSpace1(&processor, 0, INPUT_SPACE);
        context.VideoProcessorSetOutputColorSpace1(&processor, OUTPUT_SPACE);
        context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&full(out)));
        context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full(out)));
        Ok(Processor { key: (input, out), enumerator, processor, source: None })
    }
}

/// A `size` BGRA texture the tonemap draws into and the processor reads.
pub(super) fn canvas(device: &ID3D11Device, size: (u32, u32)) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(|e| format!("canvas: {e}"))?;
    tex.ok_or_else(|| "canvas missing".into())
}

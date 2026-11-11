//! The GPU export path: the clip is decoded on the GPU (D3D11VA), each picture is resized
//! (or copied at its own size) on the GPU by the D3D11 video processor into an NV12 frame,
//! and NVENC encodes that frame from video memory. No picture comes back to system memory.
//!
//! Measured 2026-11-12 with the shipped FFmpeg (20 s of a 1440p60 H.264 clip, NVENC p5): the
//! CPU path (software decode, swscale, NVENC from system memory) took 6.6 CPU-s at the same
//! size and 13.1 at 1080p; decode and encode on the GPU took 0.7 CPU-s.
//!
//! [`GpuStage`] is the one place a picture is changed between decode and encode: a scaler or
//! upscaler (e.g. a video processor stream extension) belongs in [`GpuStage::convert`].
//!
//! Vendor boundary (for AMD and Intel ports): everything in this file is vendor-neutral
//! Windows API (D3D11VA decode, the D3D11 video processor, an NV12 D3D11 frames pool), and
//! AMF (`h264_amf`) and QSV (`h264_qsv`) encoders take the same D3D11 NV12 frames. The
//! NVIDIA-only parts are the encoder choice and its options (`video_encoder.rs`:
//! `VideoBackend::Nvenc`, `nvenc_codec`) and any NVIDIA video processor extension a stage
//! adds (RTX Video Super Resolution); a port replaces those and keeps this path.
//!
//! Anything this path cannot do (no D3D11 decode for the codec or profile, a video processor
//! that refuses the format, a hybrid laptop whose default adapter has no NVENC) is an error,
//! and the export runs again on the CPU path (`edited.rs`, `cfr.rs`).

use std::mem::ManuallyDrop;

use ffmpeg_next as ff;
use ff::ffi as sys;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::video_encoder::SourceVideo;
use crate::gpu_frames::{device_ctx, frames_ctx_as, BufRef, DeviceLock, HwFrame};

/// Decoder surfaces beyond its own reference frames: the pipeline holds the newest decoded
/// picture until a tick needs it.
const EXTRA_DECODER_FRAMES: i32 = 8;
/// Views kept per texture slice; the pools reuse a few textures, so this never fills.
const MAX_VIEWS: usize = 64;

/// The GPU path is tried unless `RECALL_EXPORT_GPU=0` (a check of the CPU path).
pub(crate) fn wanted() -> bool {
    std::env::var("RECALL_EXPORT_GPU").map_or(true, |v| v.trim() != "0")
}

/// A D3D11 hardware decoder for `source` and the stage that turns its pictures into
/// `out`-sized NV12 frames for NVENC, all on one new device.
pub(crate) fn open(source: &SourceVideo, out: (u32, u32)) -> Result<(ff::decoder::Video, GpuStage), String> {
    let (device, context) = video_device()?;
    let av_device = device_ctx(&device, &context)?;
    let decoder = open_decoder(source.params.clone(), &av_device)?;
    let stage = GpuStage::new(&device, context, &av_device, (source.width, source.height), out)?;
    Ok((decoder, stage))
}

fn video_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11 video device: {e}"))?;
    }
    Ok((device.ok_or("no D3D11 device")?, context.ok_or("no D3D11 context")?))
}

/// Picks D3D11 frames when the decoder offers them, else what FFmpeg would pick (software
/// frames, which [`GpuStage::convert`] refuses, so the export reruns on the CPU).
unsafe extern "C" fn pick_d3d11(ctx: *mut sys::AVCodecContext, formats: *const sys::AVPixelFormat) -> sys::AVPixelFormat {
    let mut at = formats;
    unsafe {
        while *at != sys::AVPixelFormat::AV_PIX_FMT_NONE {
            if *at == sys::AVPixelFormat::AV_PIX_FMT_D3D11 {
                return *at;
            }
            at = at.add(1);
        }
        sys::avcodec_default_get_format(ctx, formats)
    }
}

fn open_decoder(params: ff::codec::Parameters, device: &BufRef) -> Result<ff::decoder::Video, String> {
    let mut context = ff::codec::context::Context::from_parameters(params).map_err(|e| format!("create GPU video decoder: {e}"))?;
    // SAFETY: plain fields of a context not yet opened; FFmpeg takes the device reference.
    unsafe {
        let c = context.as_mut_ptr();
        (*c).hw_device_ctx = sys::av_buffer_ref(device.as_ptr());
        (*c).get_format = Some(pick_d3d11);
        (*c).extra_hw_frames = EXTRA_DECODER_FRAMES;
    }
    context.decoder().video().map_err(|e| format!("open GPU video decoder: {e}"))
}

/// The video processor between the decoder and NVENC, and the NV12 frames pool it fills.
pub(crate) struct GpuStage {
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    pool: BufRef,
    input: (u32, u32),
    inputs: Vec<(usize, u32, ID3D11VideoProcessorInputView)>,
    outputs: Vec<(usize, ID3D11VideoProcessorOutputView)>,
    colour: Option<DXGI_COLOR_SPACE_TYPE>,
}

impl GpuStage {
    fn new(device: &ID3D11Device, context: ID3D11DeviceContext, av_device: &BufRef, input: (u32, u32), out: (u32, u32)) -> Result<Self, String> {
        let video: ID3D11VideoDevice = device.cast().map_err(|e| format!("D3D11 video device: {e}"))?;
        let video_context: ID3D11VideoContext1 = context.cast().map_err(|e| format!("D3D11 video context: {e}"))?;
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
        let (enumerator, processor) = unsafe {
            let enumerator = video.CreateVideoProcessorEnumerator(&desc).map_err(|e| format!("video processor for {}x{} -> {}x{}: {e}", input.0, input.1, out.0, out.1))?;
            let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12).map_err(|e| format!("video processor NV12: {e}"))?;
            let both = (D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 | D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0) as u32;
            if support & both != both {
                return Err(format!("the video processor cannot read and write NV12 (support {support:#x})"));
            }
            let processor = video.CreateVideoProcessor(&enumerator, 0).map_err(|e| format!("create video processor: {e}"))?;
            (enumerator, processor)
        };
        let full = |(w, h): (u32, u32)| RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        unsafe {
            video_context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            // No driver "enhancements": the picture is resized, nothing else.
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&full(input)));
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&full(out)));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full(out)));
        }
        let pool = frames_ctx_as(av_device, out.0, out.1, sys::AVPixelFormat::AV_PIX_FMT_NV12)?;
        Ok(Self { context, video, video_context, enumerator, processor, pool, input, inputs: Vec::new(), outputs: Vec::new(), colour: None })
    }

    /// The NV12 frames pool NVENC is opened on.
    pub(crate) fn frames(&self) -> *mut sys::AVBufferRef {
        self.pool.as_ptr()
    }

    /// `decoded` (a D3D11 decoder picture) as a new frame of the output size.
    pub(crate) fn convert(&mut self, decoded: &ff::frame::Video) -> Result<ff::frame::Video, String> {
        if decoded.format() != ff::format::Pixel::D3D11 {
            return Err(format!("the decoder gave {:?} frames, not D3D11 ones", decoded.format()));
        }
        if (decoded.width(), decoded.height()) != self.input {
            return Err(format!("the picture changed size mid-clip ({}x{})", decoded.width(), decoded.height()));
        }
        // SAFETY: a D3D11 frame keeps its texture in data[0] and its array slice in data[1].
        let (source, slice) = unsafe {
            let raw = &*((&(*decoded.as_ptr()).data[0]) as *const *mut u8 as *const *mut std::ffi::c_void);
            (ID3D11Texture2D::from_raw_borrowed(raw).ok_or("decoded frame has no texture")?.clone(), (*decoded.as_ptr()).data[1] as usize as u32)
        };
        let out = HwFrame::get(&self.pool)?;
        let target = out.texture().map(|(t, _)| t.clone()).ok_or("output frame has no texture")?;
        let hw = out.device_hwctx().ok_or("output frame has no device")?;
        {
            let _lock = unsafe { DeviceLock::take(hw) };
            if self.colour.is_none() {
                let space = colour_space(decoded);
                unsafe {
                    self.video_context.VideoProcessorSetStreamColorSpace1(&self.processor, 0, space);
                    // The same space out: the picture is resized, its colours are not converted.
                    self.video_context.VideoProcessorSetOutputColorSpace1(&self.processor, space);
                }
                self.colour = Some(space);
            }
            let input = self.input_view(&source, slice)?;
            let output = self.output_view(&target)?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: ManuallyDrop::new(Some(input)), ..Default::default() };
            let blt = unsafe { self.video_context.VideoProcessorBlt(&self.processor, &output, 0, std::slice::from_ref(&stream)) };
            unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
            blt.map_err(|e| format!("video processor: {e}"))?;
            unsafe { self.context.Flush() };
        }
        // SAFETY: an owned frame handed to the wrapper, which frees it; then pts and colour
        // properties come from the decoded picture.
        let mut frame = unsafe { ff::frame::Video::wrap(out.into_raw()) };
        unsafe { sys::av_frame_copy_props(frame.as_mut_ptr(), decoded.as_ptr()) };
        Ok(frame)
    }

    fn input_view(&mut self, texture: &ID3D11Texture2D, slice: u32) -> Result<ID3D11VideoProcessorInputView, String> {
        let key = texture.as_raw() as usize;
        if let Some((_, _, view)) = self.inputs.iter().find(|(k, s, _)| *k == key && *s == slice) {
            return Ok(view.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice } },
        };
        let mut view = None;
        unsafe { self.video.CreateVideoProcessorInputView(texture, &self.enumerator, &desc, Some(&mut view)) }.map_err(|e| format!("video processor input: {e}"))?;
        let view = view.ok_or("video processor input view missing")?;
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
        let desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let mut view = None;
        unsafe { self.video.CreateVideoProcessorOutputView(texture, &self.enumerator, &desc, Some(&mut view)) }.map_err(|e| format!("video processor output: {e}"))?;
        let view = view.ok_or("video processor output view missing")?;
        if self.outputs.len() >= MAX_VIEWS {
            self.outputs.remove(0);
        }
        self.outputs.push((key, view.clone()));
        Ok(view)
    }
}

/// The YCbCr space `frame` is stored in (matrix and range), BT.709 when it does not say and
/// is HD, BT.601 when it does not say and is SD (as players read untagged video).
fn colour_space(frame: &ff::frame::Video) -> DXGI_COLOR_SPACE_TYPE {
    use sys::AVColorSpace::*;
    // SAFETY: plain fields of a live frame.
    let (range, matrix) = unsafe { ((*frame.as_ptr()).color_range, (*frame.as_ptr()).colorspace) };
    let full = range == sys::AVColorRange::AVCOL_RANGE_JPEG;
    let sd = matches!(matrix, AVCOL_SPC_BT470BG | AVCOL_SPC_SMPTE170M) || (matrix == AVCOL_SPC_UNSPECIFIED && frame.height() < 720);
    let wide = matches!(matrix, AVCOL_SPC_BT2020_NCL | AVCOL_SPC_BT2020_CL);
    match (full, sd, wide) {
        (_, _, true) if full => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P2020,
        (_, _, true) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020,
        (true, true, _) => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P601,
        (true, false, _) => DXGI_COLOR_SPACE_YCBCR_FULL_G22_LEFT_P709,
        (false, true, _) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
        (false, false, _) => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    }
}

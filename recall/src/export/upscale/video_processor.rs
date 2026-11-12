//! The plain GPU scale and the check that a vendor super resolution really engages, both set
//! up exactly like the export's own video processor (`gpu.rs`, `GpuStage::new`): NV12 in and
//! out, OPTIMAL_QUALITY, progressive frames, the driver's auto processing off, studio-range
//! BT.709. A check set up differently could pass while the export got the plain scale.
//! Vendor-neutral: the vendor part is behind `SuperResolution`.

use std::collections::HashMap;
use std::sync::Mutex;

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::{Readiness, SuperResolution};

pub(crate) struct VideoProcessorScaler {
    device: ID3D11VideoDevice,
    context: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
}

impl VideoProcessorScaler {
    /// An NV12 `input` -> `output` scaler. With `super_resolution`, the vendor upscaler is
    /// turned on; without it, this is the plain scale.
    pub(crate) fn new(
        device: &ID3D11Device,
        input: (u32, u32),
        output: (u32, u32),
        super_resolution: Option<&dyn SuperResolution>,
    ) -> Result<Self, String> {
        let video: ID3D11VideoDevice = device.cast().map_err(|e| format!("video device: {e}"))?;
        let immediate = unsafe { device.GetImmediateContext() }.map_err(|e| format!("immediate context: {e}"))?;
        let context: ID3D11VideoContext1 = immediate.cast().map_err(|e| format!("video context: {e}"))?;
        let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_QUALITY,
        };
        let enumerator = unsafe { video.CreateVideoProcessorEnumerator(&desc) }.map_err(|e| format!("processor enumerator: {e}"))?;
        let processor = unsafe { video.CreateVideoProcessor(&enumerator, 0) }.map_err(|e| format!("video processor: {e}"))?;
        let full = |(w, h): (u32, u32)| RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        let space = DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709;
        unsafe {
            context.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&full(input)));
            context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&full(output)));
            context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full(output)));
            context.VideoProcessorSetStreamColorSpace1(&processor, 0, space);
            context.VideoProcessorSetOutputColorSpace1(&processor, space);
        }
        if let Some(sr) = super_resolution {
            sr.set(&context, &processor, true)?;
        }
        Ok(Self { device: video, context, enumerator, processor })
    }

    /// Scales `input` (NV12) into `output` (NV12, render-target bound).
    pub(crate) fn scale(&self, input: &ID3D11Texture2D, output: &ID3D11Texture2D) -> Result<(), String> {
        let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 } },
        };
        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
        };
        let (mut in_view, mut out_view) = (None, None);
        unsafe {
            self.device
                .CreateVideoProcessorInputView(input, &self.enumerator, &in_desc, Some(&mut in_view))
                .map_err(|e| format!("input view: {e}"))?;
            self.device
                .CreateVideoProcessorOutputView(output, &self.enumerator, &out_desc, Some(&mut out_view))
                .map_err(|e| format!("output view: {e}"))?;
        }
        let out_view = out_view.ok_or("no output view")?;
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), pInputSurface: std::mem::ManuallyDrop::new(in_view), ..Default::default() };
        let result = unsafe { self.context.VideoProcessorBlt(&self.processor, &out_view, 0, std::slice::from_ref(&stream)) };
        // The stream struct holds the input view in a ManuallyDrop: release it here.
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.map_err(|e| format!("video processor blt: {e}"))
    }
}

/// Readiness per (adapter, input size, output size), measured once per process: the check
/// renders two output-size frames and reads them back, so size-cap retries reuse it.
type ReadinessKey = (i64, (u32, u32), (u32, u32));
static READINESS: Mutex<Option<HashMap<ReadinessKey, Readiness>>> = Mutex::new(None);

/// Whether `super_resolution` really changes an `input` -> `output` upscale on this device:
/// the same detailed NV12 picture is scaled with it on and without it and the luma compared.
/// GPU only, no window. Cached per adapter and sizes.
pub(crate) fn probe_readiness(device: &ID3D11Device, super_resolution: Option<Box<dyn SuperResolution>>, input: (u32, u32), output: (u32, u32)) -> Readiness {
    let Some(sr) = super_resolution else { return Readiness::Unsupported };
    let key = (adapter_luid(device), input, output);
    if let Some(known) = READINESS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&key) {
        return known.clone();
    }
    let readiness = match (render_pattern(device, None, input, output), render_pattern(device, Some(&*sr), input, output)) {
        (Ok(plain), Ok(enhanced)) if plain != enhanced => Readiness::Ready,
        (Ok(_), Ok(_)) => Readiness::NeedsDriverSetting { requirement: sr.requirement(), setting: sr.driver_setting() },
        _ => Readiness::Unsupported,
    };
    READINESS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(key, readiness.clone());
    readiness
}

fn adapter_luid(device: &ID3D11Device) -> i64 {
    let adapter = device.cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>().and_then(|d| unsafe { d.GetAdapter() });
    adapter.and_then(|a| unsafe { a.GetDesc() }).map_or(0, |d| ((d.AdapterLuid.HighPart as i64) << 32) | d.AdapterLuid.LowPart as i64)
}

/// A video-capable D3D11 device on the default adapter (tests).
#[cfg(test)]
pub(super) fn default_device() -> Result<ID3D11Device, String> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
    let mut device = None;
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
            None,
        )
    }
    .map_err(|e| format!("D3D11 device: {e}"))?;
    device.ok_or_else(|| "D3D11 device: none".into())
}

pub(super) fn nv12_texture(device: &ID3D11Device, (w, h): (u32, u32), init: Option<&[u8]>, staging: bool) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
        BindFlags: if staging { 0 } else { (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32 },
        CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
        MiscFlags: 0,
    };
    let data = init.map(|d| D3D11_SUBRESOURCE_DATA { pSysMem: d.as_ptr().cast(), SysMemPitch: w, SysMemSlicePitch: 0 });
    let mut out = None;
    unsafe { device.CreateTexture2D(&desc, data.as_ref().map(|d| d as *const _), Some(&mut out)) }.map_err(|e| format!("NV12 texture: {e}"))?;
    out.ok_or_else(|| "NV12 texture missing".into())
}

/// Fine luma checker and stripes, neutral chroma: detail a super resolution model sharpens.
pub(super) fn pattern((w, h): (u32, u32)) -> Vec<u8> {
    let mut px = vec![128u8; (w * h * 3 / 2) as usize];
    for y in 0..h {
        for x in 0..w {
            let checker = (x / 3 + y / 3) % 2 == 0;
            px[(y * w + x) as usize] = if y < h / 2 { if x % 2 == 0 { 235 } else { 16 } } else if checker { 220 } else { 30 };
        }
    }
    px
}

/// The luma plane of `texture` (NV12, `size`), through a staging copy.
pub(super) fn luma(device: &ID3D11Device, texture: &ID3D11Texture2D, size: (u32, u32)) -> Result<Vec<u8>, String> {
    let context = unsafe { device.GetImmediateContext() }.map_err(|e| format!("immediate context: {e}"))?;
    let staging = nv12_texture(device, size, None, true)?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let mut out = Vec::with_capacity((size.0 * size.1) as usize);
    unsafe {
        context.CopyResource(&staging, texture);
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("readback: {e}"))?;
        for y in 0..size.1 {
            out.extend_from_slice(std::slice::from_raw_parts((mapped.pData as *const u8).add((y * mapped.RowPitch) as usize), size.0 as usize));
        }
        context.Unmap(&staging, 0);
    }
    Ok(out)
}

fn render_pattern(device: &ID3D11Device, sr: Option<&dyn SuperResolution>, input: (u32, u32), output: (u32, u32)) -> Result<Vec<u8>, String> {
    let source = nv12_texture(device, input, Some(&pattern(input)), false)?;
    let target = nv12_texture(device, output, None, false)?;
    let scaler = VideoProcessorScaler::new(device, input, output, sr)?;
    // The driver's model may need a few frames before it engages.
    for _ in 0..4 {
        scaler.scale(&source, &target)?;
    }
    luma(device, &target, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GPU check, not part of the normal run:
    /// `cargo test probe_reports_driver_readiness -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_reports_driver_readiness() {
        let device = default_device().unwrap();
        let vendor = super::super::adapter_vendor(&device);
        for (input, output) in [((1280, 720), (2560, 1440)), ((1920, 1080), (2560, 1440)), ((1728, 1080), (1920, 1200))] {
            let readiness = probe_readiness(&device, super::super::for_adapter(vendor), input, output);
            eprintln!("{input:?} -> {output:?}: {readiness:?}");
            assert_ne!(readiness, Readiness::Unsupported, "an RTX GPU accepts the request");
        }
    }
}

//! GPU scale on the D3D11 video processor, with an optional vendor super
//! resolution. Vendor-neutral: the vendor part is behind `SuperResolution`.

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::{Readiness, SuperResolution};

pub(crate) struct VideoProcessorScaler {
    device: ID3D11VideoDevice,
    context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    super_resolution: Option<Box<dyn SuperResolution>>,
}

impl VideoProcessorScaler {
    /// A scaler from `input` to `output` size. With `super_resolution`, the
    /// vendor upscaler is turned on; without it, this is the plain scale.
    pub(crate) fn new(
        device: &ID3D11Device,
        input: (u32, u32),
        output: (u32, u32),
        super_resolution: Option<Box<dyn SuperResolution>>,
    ) -> Result<Self, String> {
        let video: ID3D11VideoDevice = device.cast().map_err(|e| format!("video device: {e}"))?;
        let immediate = unsafe { device.GetImmediateContext() }.map_err(|e| format!("immediate context: {e}"))?;
        let context: ID3D11VideoContext = immediate.cast().map_err(|e| format!("video context: {e}"))?;
        let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        let enumerator =
            unsafe { video.CreateVideoProcessorEnumerator(&desc) }.map_err(|e| format!("processor enumerator: {e}"))?;
        let processor = unsafe { video.CreateVideoProcessor(&enumerator, 0) }.map_err(|e| format!("video processor: {e}"))?;
        let source = RECT { left: 0, top: 0, right: input.0 as i32, bottom: input.1 as i32 };
        let target = RECT { left: 0, top: 0, right: output.0 as i32, bottom: output.1 as i32 };
        unsafe {
            context.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source));
            context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&target));
            context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&target));
        }
        if let Some(sr) = &super_resolution {
            sr.set(&context, &processor, true)?;
        }
        Ok(Self { device: video, context, enumerator, processor, super_resolution })
    }

    /// Scales `input` (NV12 or BGRA, shader-resource + render-target bound)
    /// into `output` (render-target bound, NV12 or BGRA for NVENC).
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
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: std::mem::ManuallyDrop::new(in_view),
            ..Default::default()
        };
        let result = unsafe { self.context.VideoProcessorBlt(&self.processor, &out_view, 0, std::slice::from_ref(&stream)) };
        // The stream struct holds the input view in a ManuallyDrop: release it here.
        unsafe { std::mem::ManuallyDrop::drop(&mut stream.pInputSurface) };
        result.map_err(|e| format!("video processor blt: {e}"))
    }

    pub(crate) fn uses_super_resolution(&self) -> bool {
        self.super_resolution.is_some()
    }
}

/// Checks whether `super_resolution` really changes the picture on this
/// device: upscales a detailed 1280x720 test pattern to 2560x1440 with it on
/// and without it, and compares the pixels. GPU only, no window, a few ms.
pub(crate) fn probe_readiness(device: &ID3D11Device, super_resolution: Option<Box<dyn SuperResolution>>) -> Readiness {
    let Some(sr) = super_resolution else { return Readiness::Unsupported };
    let (vendor, setting) = (sr.vendor(), sr.driver_setting());
    match (render_pattern(device, None), render_pattern(device, Some(sr))) {
        (Ok(plain), Ok(enhanced)) if plain != enhanced => Readiness::Ready,
        (Ok(_), Ok(_)) => Readiness::NeedsDriverSetting { vendor, setting },
        _ => Readiness::Unsupported,
    }
}

const PROBE_IN: (u32, u32) = (1280, 720);
const PROBE_OUT: (u32, u32) = (2560, 1440);

fn texture(
    device: &ID3D11Device,
    size: (u32, u32),
    bind: D3D11_BIND_FLAG,
    usage: D3D11_USAGE,
    cpu: u32,
    init: Option<&[u8]>,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: cpu,
        MiscFlags: 0,
    };
    let data = init.map(|d| D3D11_SUBRESOURCE_DATA { pSysMem: d.as_ptr() as _, SysMemPitch: size.0 * 4, SysMemSlicePitch: 0 });
    let mut out = None;
    unsafe { device.CreateTexture2D(&desc, data.as_ref().map(|d| d as *const _), Some(&mut out)) }
        .map_err(|e| format!("probe texture: {e}"))?;
    out.ok_or_else(|| "probe texture: none".into())
}

/// Fine checker and 1-pixel stripes: detail a super resolution model sharpens.
fn pattern() -> Vec<u8> {
    let (w, h) = PROBE_IN;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let checker = if (x / 3 + y / 3) % 2 == 0 { 230 } else { 20 };
            px[i] = checker;
            px[i + 1] = if x % 2 == 0 { 255 } else { 0 };
            px[i + 2] = checker;
            px[i + 3] = 255;
        }
    }
    px
}

fn render_pattern(device: &ID3D11Device, sr: Option<Box<dyn SuperResolution>>) -> Result<Vec<u8>, String> {
    let both = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    let source = texture(device, PROBE_IN, both, D3D11_USAGE_DEFAULT, 0, Some(&pattern()))?;
    let target = texture(device, PROBE_OUT, D3D11_BIND_RENDER_TARGET, D3D11_USAGE_DEFAULT, 0, None)?;
    let staging = texture(device, PROBE_OUT, D3D11_BIND_FLAG(0), D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ.0 as u32, None)?;
    let scaler = VideoProcessorScaler::new(device, PROBE_IN, PROBE_OUT, sr)?;
    // The driver's model may need a few frames before it engages.
    for _ in 0..4 {
        scaler.scale(&source, &target)?;
    }
    let context = unsafe { device.GetImmediateContext() }.map_err(|e| format!("immediate context: {e}"))?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let row_bytes = (PROBE_OUT.0 * 4) as usize;
    let mut out = Vec::with_capacity(row_bytes * PROBE_OUT.1 as usize);
    unsafe {
        context.CopyResource(&staging, &target);
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("probe map: {e}"))?;
        for y in 0..PROBE_OUT.1 {
            let row = std::slice::from_raw_parts((mapped.pData as *const u8).add((y * mapped.RowPitch) as usize), row_bytes);
            out.extend_from_slice(row);
        }
        context.Unmap(&staging, 0);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_1};

    /// GPU check, not part of the normal run:
    /// `cargo test probe_reports_driver_readiness -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_reports_driver_readiness() {
        let mut device = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .unwrap();
        }
        let device = device.unwrap();
        let plain = render_pattern(&device, None).unwrap();
        assert_eq!(plain.len(), (PROBE_OUT.0 * PROBE_OUT.1 * 4) as usize);
        let readiness = probe_readiness(&device, super::super::for_adapter(0x10DE));
        eprintln!("readiness: {readiness:?}; notice: {:?}", readiness.notice());
        assert_ne!(readiness, Readiness::Unsupported, "an RTX GPU accepts the request");
    }
}

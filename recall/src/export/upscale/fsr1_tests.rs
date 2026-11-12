use super::*;
use crate::export::upscale::video_processor::default_device;

const IN: (u32, u32) = (1280, 720);
const OUT: (u32, u32) = (2560, 1440);

fn nv12(device: &ID3D11Device, (w, h): (u32, u32), init: Option<&[u8]>, staging: bool) -> ID3D11Texture2D {
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
    unsafe { device.CreateTexture2D(&desc, data.as_ref().map(|d| d as *const _), Some(&mut out)).unwrap() };
    out.unwrap()
}

/// Luma of `texture` (NV12, OUT size), read back through a staging copy.
fn luma(device: &ID3D11Device, context: &ID3D11DeviceContext, texture: &ID3D11Texture2D) -> Vec<u8> {
    let staging = nv12(device, OUT, None, true);
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let mut out = Vec::with_capacity((OUT.0 * OUT.1) as usize);
    unsafe {
        context.CopyResource(&staging, texture);
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).unwrap();
        for y in 0..OUT.1 {
            let row = std::slice::from_raw_parts((mapped.pData as *const u8).add((y * mapped.RowPitch) as usize), OUT.0 as usize);
            out.extend_from_slice(row);
        }
        context.Unmap(&staging, 0);
    }
    out
}

/// Sharp edges in luma, neutral chroma: what FSR 1 should keep crisp.
fn pattern() -> Vec<u8> {
    let (w, h) = IN;
    let mut px = vec![128u8; (w * h * 3 / 2) as usize];
    for y in 0..h {
        for x in 0..w {
            px[(y * w + x) as usize] = if (x / 4 + y / 4) % 2 == 0 { 220 } else { 30 };
        }
    }
    px
}

/// GPU check, not part of the normal run:
/// `cargo test fsr1_upscales_nv12 -- --ignored --nocapture`
#[test]
#[ignore]
fn fsr1_upscales_nv12() {
    let device = default_device().unwrap();
    let context = unsafe { device.GetImmediateContext() }.unwrap();
    let source = nv12(&device, IN, Some(&pattern()), false);
    let target = nv12(&device, OUT, None, false);
    let mut fsr = Fsr1::new(&device, context.clone(), IN, OUT).expect("FSR 1 on this GPU");
    let started = std::time::Instant::now();
    for _ in 0..30 {
        fsr.run(&source, 0, &target, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709).unwrap();
    }
    let fsr_luma = luma(&device, &context, &target);
    eprintln!("FSR 1 720p -> 1440p: 30 frames + readback {:.1} ms", started.elapsed().as_secs_f64() * 1e3);

    let plain = crate::export::upscale::VideoProcessorScaler::new(&device, IN, OUT, None).unwrap();
    let plain_target = nv12(&device, OUT, None, false);
    plain.scale(&source, &plain_target).unwrap();
    let plain_luma = luma(&device, &context, &plain_target);

    let mean = |v: &[u8]| v.iter().map(|&p| p as f64).sum::<f64>() / v.len() as f64;
    let (fsr_mean, plain_mean) = (mean(&fsr_luma), mean(&plain_luma));
    let differ = fsr_luma.iter().zip(&plain_luma).filter(|(a, b)| a.abs_diff(**b) > 8).count();
    eprintln!("mean luma FSR {fsr_mean:.1} vs plain {plain_mean:.1}; pixels >8 apart: {differ}");
    assert!((fsr_mean - plain_mean).abs() < 6.0, "same picture brightness after the RGB round trip");
    assert!(differ > 10_000, "FSR 1 changes the picture, not a copy of the plain scale");
}

//! The NV12 stage's colour proof, on this GPU, before it is used. DXGI has no colour space
//! with the BT.601 matrix and BT.709 primaries, so the processor is told "BT.709 RGB in,
//! BT.601 YCbCr out"; a driver that read the primaries literally would shift hues. Eight
//! colour bars go through the real processor and each Y/Cb/Cr must land within
//! [`TOLERANCE`] of exact BT.601 studio range, the conversion NVENC itself does for BGRA
//! (measured 11-15: its clips are within 0.5 of the same values). Otherwise frames stay BGRA.

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::nv12::Nv12;
use crate::framing::Rect;

/// Code values each of Y, Cb, Cr may differ from exact BT.601 (rounding; measured <= 1).
pub(super) const TOLERANCE: f32 = 2.0;
/// sRGB bars: white, yellow, cyan, green, magenta, red, blue, black.
pub(super) const BARS: [[u8; 3]; 8] = [[255, 255, 255], [255, 255, 0], [0, 255, 255], [0, 255, 0], [255, 0, 255], [255, 0, 0], [0, 0, 255], [0, 0, 0]];
const BAR_W: u32 = 4;
const SIZE: (u32, u32) = (BAR_W * 8, 4);

/// Exact BT.601 studio-range Y, Cb, Cr of an sRGB-encoded colour.
pub(super) fn bt601([r, g, b]: [u8; 3]) -> [f32; 3] {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    [16.0 + 219.0 * y, 128.0 + 224.0 * (b - y) / 1.772, 128.0 + 224.0 * (r - y) / 1.402]
}

fn texture(device: &ID3D11Device, format: DXGI_FORMAT, usage: D3D11_USAGE, bind: u32, cpu: u32, init: Option<&[u8]>) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: SIZE.0,
        Height: SIZE.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: usage,
        BindFlags: bind,
        CPUAccessFlags: cpu,
        MiscFlags: 0,
    };
    let data = init.map(|b| D3D11_SUBRESOURCE_DATA { pSysMem: b.as_ptr().cast(), SysMemPitch: SIZE.0 * 4, SysMemSlicePitch: 0 });
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, data.as_ref().map(|d| d as *const _), Some(&mut tex)) }.map_err(|e| format!("colour check texture: {e}"))?;
    tex.ok_or_else(|| "colour check texture missing".into())
}

/// The processor's Y, Cb, Cr for each bar (one-time, at pipeline start: the map waits).
pub(super) fn measure(stage: &mut Nv12, device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<[[f32; 3]; 8], String> {
    let mut px = Vec::with_capacity((SIZE.0 * SIZE.1 * 4) as usize);
    for _ in 0..SIZE.1 {
        for x in 0..SIZE.0 {
            let [r, g, b] = BARS[(x / BAR_W) as usize];
            px.extend_from_slice(&[b, g, r, 255]);
        }
    }
    // Like a WGC surface (shader resource + render target): the processor reads it directly.
    let bind = (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32;
    let src = texture(device, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_USAGE_DEFAULT, bind, 0, Some(&px))?;
    let out = texture(device, DXGI_FORMAT_NV12, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0, None)?;
    let staging = texture(device, DXGI_FORMAT_NV12, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32, None)?;
    stage.blt(&src, Rect::full(SIZE), &out, 0, SIZE)?;
    stage.forget_outputs();
    let mut values = [[0f32; 3]; 8];
    unsafe {
        context.CopyResource(&staging, &out);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("colour check map: {e}"))?;
        let pitch = mapped.RowPitch as usize;
        let rows = SIZE.1 as usize;
        // SAFETY: a mapped NV12 surface: `rows` luma rows then rows/2 chroma rows, `pitch` apart.
        let bytes = std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (rows + rows / 2));
        for (i, v) in values.iter_mut().enumerate() {
            let x = i * BAR_W as usize + 1;
            let c = pitch * rows + i * BAR_W as usize; // the bar's first chroma pair (even x)
            *v = [bytes[pitch + x] as f32, bytes[c] as f32, bytes[c + 1] as f32];
        }
        context.Unmap(&staging, 0);
    }
    Ok(values)
}

/// The largest difference from exact BT.601 over all bars and components.
pub(super) fn worst(values: &[[f32; 3]; 8]) -> f32 {
    values.iter().zip(BARS).flat_map(|(got, bar)| got.iter().zip(bt601(bar)).map(|(g, w)| (g - w).abs())).fold(0.0, f32::max)
}

/// Err unless every bar converts within [`TOLERANCE`].
pub(super) fn prove(stage: &mut Nv12, device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<(), String> {
    let values = measure(stage, device, context)?;
    let worst = worst(&values);
    if worst > TOLERANCE {
        return Err(format!("colour check off by {worst:.1} code values (limit {TOLERANCE}): {values:?}"));
    }
    Ok(())
}

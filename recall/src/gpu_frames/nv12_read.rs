//! CPU reads of an NV12 frame (BT.601 studio range, as `nv12.rs` writes it): RGBA for
//! thumbnails, stills and screenshots, and the HUD probe's grey picture from the luma plane.
//! Both run a few times per save or probe, never per frame.

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};

use super::av::{DeviceLock, AVD3D11VADeviceContext};
use crate::hud_probe::{Gray, ANALYSIS_W};

/// Give up on one map after this long.
const MAP_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

pub(super) fn is_nv12(desc: &D3D11_TEXTURE2D_DESC) -> bool {
    desc.Format == DXGI_FORMAT_NV12
}

/// BT.601 studio-range Y'CbCr -> 8-bit sRGB.
pub(super) fn rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let y = (y as f32 - 16.0) * (255.0 / 219.0);
    let (cb, cr) = ((u as f32 - 128.0) * (255.0 / 224.0), (v as f32 - 128.0) * (255.0 / 224.0));
    let px = |x: f32| x.round().clamp(0.0, 255.0) as u8;
    [px(y + 1.402 * cr), px(y - 0.344_136 * cb - 0.714_136 * cr), px(y + 1.772 * cb)]
}

/// Tightly packed RGBA8 (alpha 255) of a mapped NV12 surface: `rows` luma rows of `pitch`
/// bytes, then the interleaved chroma rows.
pub(super) fn rgba(base: &[u8], pitch: usize, rows: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    let uv = rows * pitch;
    for y in 0..h {
        let luma = &base[y * pitch..][..w];
        let chroma = &base[uv + (y / 2) * pitch..][..w.div_ceil(2) * 2];
        for (x, d) in out[y * w * 4..(y + 1) * w * 4].chunks_exact_mut(4).enumerate() {
            let c = (x / 2) * 2;
            let [r, g, b] = rgb(luma[x], chroma[c], chroma[c + 1]);
            d.copy_from_slice(&[r, g, b, 255]);
        }
    }
    out
}

/// The HUD grey picture from the luma plane over `(x, y, w, h)`: the same box sizes as the
/// BGRA path (a 2^`level` mip filter, then `Gray::from_bgra`'s whole factor), and luma brought
/// to full range so the probe's thresholds read it as they read BT.601 luma of BGRA.
pub(super) fn gray(base: &[u8], pitch: usize, (x0, y0, w, h): (usize, usize, usize, usize), level: u32) -> Gray {
    let (mw, mh) = ((w >> level).max(1), (h >> level).max(1));
    let s = (mw / ANALYSIS_W as usize).max(1);
    let f = (1usize << level) * s;
    let (ow, oh) = (mw / s, mh / s);
    let mut px = vec![0f32; ow * oh];
    let norm = 255.0 / 219.0 / (f * f) as f32;
    for oy in 0..oh {
        let out = &mut px[oy * ow..(oy + 1) * ow];
        for dy in 0..f {
            let Some(row) = base.get((y0 + oy * f + dy) * pitch + x0..).and_then(|r| r.get(..ow * f)) else { continue };
            for (ox, cell) in out.iter_mut().enumerate() {
                *cell += row[ox * f..(ox + 1) * f].iter().map(|&v| v as f32 - 16.0).sum::<f32>();
            }
        }
        out.iter_mut().for_each(|v| *v = (*v * norm).clamp(0.0, 255.0));
    }
    Gray { w: ow, h: oh, px }
}

/// A CPU-readable NV12 copy of one frame size, made once per size and device.
pub(super) struct Nv12Staging {
    key: (usize, u32, u32),
    texture: ID3D11Texture2D,
}

impl Nv12Staging {
    pub(super) fn for_frame(slot: &mut Option<Self>, device: &ID3D11Device, desc: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D, String> {
        use windows::core::Interface;
        let key = (device.as_raw() as usize, desc.Width, desc.Height);
        if let Some(s) = slot.as_ref().filter(|s| s.key == key) {
            return Ok(s.texture.clone());
        }
        *slot = None;
        let sd = D3D11_TEXTURE2D_DESC {
            Width: desc.Width,
            Height: desc.Height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        unsafe { device.CreateTexture2D(&sd, None, Some(&mut tex)) }.map_err(|e| format!("NV12 readback texture: {e}"))?;
        let texture = tex.ok_or("NV12 readback texture missing")?;
        *slot = Some(Self { key, texture: texture.clone() });
        Ok(texture)
    }
}

/// Map `staging` for reading without holding the device lock while the GPU works: try with
/// DO_NOT_WAIT under the lock, sleep outside it, give up after [`MAP_WAIT`].
pub(super) fn map_polled(hw: *mut AVD3D11VADeviceContext, context: &ID3D11DeviceContext, staging: &ID3D11Texture2D) -> Result<D3D11_MAPPED_SUBRESOURCE, String> {
    let started = std::time::Instant::now();
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    loop {
        let tried = {
            let _lock = unsafe { DeviceLock::take(hw) };
            unsafe { context.Map(staging, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut mapped)) }
        };
        match tried {
            Ok(()) => return Ok(mapped),
            Err(e) if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING => {
                if started.elapsed() >= MAP_WAIT {
                    return Err("readback: the GPU did not finish within 250 ms".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(e) => return Err(format!("readback map: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rgb;

    #[test]
    fn studio_range_bt601_decodes_to_the_rgb_it_came_from() {
        // (Y, Cb, Cr) of BT.601 studio-range colour bars, rounded to 8 bits.
        for (yuv, want) in [
            ((16, 128, 128), [0, 0, 0]),
            ((235, 128, 128), [255, 255, 255]),
            ((81, 90, 240), [255, 0, 0]),
            ((145, 54, 34), [0, 255, 0]),
            ((41, 240, 110), [0, 0, 255]),
            ((126, 128, 128), [128, 128, 128]),
        ] {
            let got = rgb(yuv.0, yuv.1, yuv.2);
            for c in 0..3 {
                assert!((got[c] as i32 - want[c] as i32).abs() <= 2, "{yuv:?} -> {got:?}, want {want:?}");
            }
        }
    }
}

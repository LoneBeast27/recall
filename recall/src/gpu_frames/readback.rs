//! On-demand CPU copy of one GPU frame (thumbnails, quiet-tail stills, screenshots): the
//! only place the zero-copy path reads pixels back, once per save or screenshot.

use std::ffi::c_void;

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;

use super::av::{DeviceLock, HwFrame};

/// Tightly packed RGBA8 (alpha 255) of `frame`: (width, height, bytes).
pub fn read_rgba(frame: &HwFrame) -> Result<(u32, u32, Vec<u8>), String> {
    let (tex, index) = frame.texture().ok_or("frame has no texture")?;
    let hw = frame.device_hwctx().ok_or("frame has no device")?;
    let (device, context) = unsafe {
        let dev = ID3D11Device::from_raw_borrowed(&*(&(*hw).device as *const *mut c_void)).ok_or("no device")?.clone();
        let ctx = ID3D11DeviceContext::from_raw_borrowed(&*(&(*hw).device_context as *const *mut c_void))
            .ok_or("no device context")?
            .clone();
        (dev, ctx)
    };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    let (w, h) = (frame.width().min(desc.Width), frame.height().min(desc.Height));
    let sd = D3D11_TEXTURE2D_DESC {
        Width: desc.Width,
        Height: desc.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: desc.Format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    unsafe { device.CreateTexture2D(&sd, None, Some(&mut staging)) }.map_err(|e| format!("readback texture: {e}"))?;
    let staging = staging.ok_or("readback texture missing")?;
    let mut out = vec![0u8; (w * h * 4) as usize];
    let _lock = unsafe { DeviceLock::take(hw) };
    unsafe {
        let sub = index * desc.MipLevels.max(1);
        context.CopySubresourceRegion(&staging, 0, 0, 0, 0, tex, sub, None);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("readback map: {e}"))?;
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData as *const u8;
        for y in 0..h as usize {
            let row = std::slice::from_raw_parts(base.add(y * pitch), w as usize * 4);
            let dst = &mut out[y * w as usize * 4..(y + 1) * w as usize * 4];
            // B8G8R8A8 -> RGBA8.
            for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        context.Unmap(&staging, 0);
    }
    Ok((w, h, out))
}

/// The HUD probe's reader (`hud_probe::Gray` frames), one per probe: the picture region
/// (`content`, the whole frame when None) is copied into a mip-mapped texture, the GPU
/// box-filters it down (GenerateMips) to the first level at most [`GRAY_MAX_W`] wide, and only
/// that level comes back to the CPU (about 1-2 MB, not the full frame). The textures are
/// made once and reused while the size and device stay the same.
///
/// The FFmpeg device lock (which the capture thread's render takes per frame) is held only
/// to record the copy, the mip filter and the small copy out, plus a `Flush`; the map then
/// polls with `D3D11_MAP_FLAG_DO_NOT_WAIT`, taking the lock per try and sleeping between
/// tries outside it, so this low-priority thread never holds the lock while the GPU works.
#[derive(Default)]
pub struct GrayReader {
    textures: Option<GrayTextures>,
}

/// Mip filtering stops at the first level no wider than this.
pub const GRAY_MAX_W: u32 = 960;
/// Give up on one map after this long.
const MAP_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

struct GrayTextures {
    key: (usize, u32, u32),
    mip: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    staging: ID3D11Texture2D,
    level: u32,
    size: (u32, u32),
}

/// The first mip level of a `w`-wide picture that is at most `GRAY_MAX_W` wide.
pub fn gray_level(w: u32) -> u32 {
    let mut level = 0;
    while (w >> level) > GRAY_MAX_W {
        level += 1;
    }
    level
}

impl GrayReader {
    pub fn read(&mut self, frame: &HwFrame, content: Option<crate::framing::Rect>) -> Result<crate::hud_probe::Gray, String> {
        let (tex, index) = frame.texture().ok_or("frame has no texture")?;
        let hw = frame.device_hwctx().ok_or("frame has no device")?;
        let (device, context) = unsafe {
            let dev = ID3D11Device::from_raw_borrowed(&*(&(*hw).device as *const *mut c_void)).ok_or("no device")?.clone();
            let ctx = ID3D11DeviceContext::from_raw_borrowed(&*(&(*hw).device_context as *const *mut c_void))
                .ok_or("no device context")?
                .clone();
            (dev, ctx)
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { tex.GetDesc(&mut desc) };
        if desc.Format != windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(format!("frame format {} is not BGRA8", desc.Format.0));
        }
        let (fw, fh) = (frame.width().min(desc.Width), frame.height().min(desc.Height));
        let r = content.filter(|r| r.w > 0 && r.h > 0 && r.x + r.w <= fw && r.y + r.h <= fh);
        let (x, y, w, h) = r.map_or((0, 0, fw, fh), |r| (r.x, r.y, r.w, r.h));
        if w < 16 || h < 16 {
            return Err(format!("picture {w}x{h} is too small"));
        }
        let key = (device.as_raw() as usize, w, h);
        if self.textures.as_ref().is_none_or(|t| t.key != key) {
            self.textures = None;
            self.textures = Some(GrayTextures::new(&device, key, desc.Format)?);
        }
        let t = self.textures.as_ref().ok_or("hud textures missing")?;
        let region = D3D11_BOX { left: x, top: y, front: 0, right: x + w, bottom: y + h, back: 1 };
        {
            let _lock = unsafe { DeviceLock::take(hw) };
            let sub = index * desc.MipLevels.max(1);
            unsafe {
                context.CopySubresourceRegion(&t.mip, 0, 0, 0, 0, tex, sub, Some(&region));
                context.GenerateMips(&t.srv);
                context.CopySubresourceRegion(&t.staging, 0, 0, 0, 0, &t.mip, t.level, None);
                context.Flush();
            }
        }
        let started = std::time::Instant::now();
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        loop {
            let tried = {
                let _lock = unsafe { DeviceLock::take(hw) };
                unsafe { context.Map(&t.staging, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut mapped)) }
            };
            match tried {
                Ok(()) => break,
                Err(e) if e.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING => {
                    if started.elapsed() >= MAP_WAIT {
                        return Err("hud readback: the GPU did not finish within 250 ms".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => return Err(format!("hud readback map: {e}")),
            }
        }
        let (sw, sh) = t.size;
        let pitch = mapped.RowPitch as usize;
        // SAFETY: the mapped staging texture holds `sh` rows of `pitch` bytes until Unmap.
        let bytes = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (sh as usize - 1) + sw as usize * 4) };
        let gray = crate::hud_probe::Gray::from_bgra(bytes, pitch, (0, 0, sw as usize, sh as usize));
        let _lock = unsafe { DeviceLock::take(hw) };
        unsafe { context.Unmap(&t.staging, 0) };
        Ok(gray)
    }
}

impl GrayTextures {
    fn new(device: &ID3D11Device, key: (usize, u32, u32), format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT) -> Result<Self, String> {
        let (_, w, h) = key;
        let level = gray_level(w);
        let size = ((w >> level).max(1), (h >> level).max(1));
        let mip_desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: level + 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
        };
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            BindFlags: 0,
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
            ..mip_desc
        };
        let (mut mip, mut staging, mut srv) = (None, None, None);
        unsafe {
            device.CreateTexture2D(&mip_desc, None, Some(&mut mip)).map_err(|e| format!("hud mip texture: {e}"))?;
            device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).map_err(|e| format!("hud staging texture: {e}"))?;
        }
        let mip = mip.ok_or("hud mip texture missing")?;
        unsafe { device.CreateShaderResourceView(&mip, None, Some(&mut srv)) }.map_err(|e| format!("hud mip view: {e}"))?;
        Ok(Self { key, mip, srv: srv.ok_or("hud mip view missing")?, staging: staging.ok_or("hud staging texture missing")?, level, size })
    }
}

#[cfg(test)]
mod gray_tests {
    use super::{gray_level, GRAY_MAX_W};

    #[test]
    fn the_gpu_filters_down_to_the_first_level_at_most_960_wide() {
        for (w, level, out) in [(3840, 2, 960), (2560, 2, 640), (1920, 1, 960), (1280, 1, 640), (960, 0, 960), (3440, 2, 860), (800, 0, 800)] {
            assert_eq!(gray_level(w), level, "{w}");
            assert_eq!(w >> level, out);
            assert!(w >> level <= GRAY_MAX_W);
        }
    }
}

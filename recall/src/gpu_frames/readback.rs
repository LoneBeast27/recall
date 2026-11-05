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

/// The HUD probe's grey analysis frame of `frame` (`hud_probe::Gray`): only the picture
/// region (`content`, the whole frame when None) is copied to a staging texture. The
/// device lock is held for the copy and for the map / unmap, not while the CPU averages.
pub fn read_gray(frame: &HwFrame, content: Option<crate::framing::Rect>) -> Result<crate::hud_probe::Gray, String> {
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
    let sd = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
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
    unsafe { device.CreateTexture2D(&sd, None, Some(&mut staging)) }.map_err(|e| format!("hud readback texture: {e}"))?;
    let staging = staging.ok_or("hud readback texture missing")?;
    let region = D3D11_BOX { left: x, top: y, front: 0, right: x + w, bottom: y + h, back: 1 };
    {
        let _lock = unsafe { DeviceLock::take(hw) };
        let sub = index * desc.MipLevels.max(1);
        unsafe { context.CopySubresourceRegion(&staging, 0, 0, 0, 0, tex, sub, Some(&region)) };
    }
    // Let the GPU finish the copy before the map waits on it under the lock.
    std::thread::sleep(std::time::Duration::from_millis(4));
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    {
        let _lock = unsafe { DeviceLock::take(hw) };
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(|e| format!("hud readback map: {e}"))?;
    }
    let pitch = mapped.RowPitch as usize;
    // SAFETY: the mapped staging texture holds `h` rows of `pitch` bytes until Unmap.
    let bytes = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * h as usize) };
    let gray = crate::hud_probe::Gray::from_bgra(bytes, pitch, (0, 0, w as usize, h as usize));
    let _lock = unsafe { DeviceLock::take(hw) };
    unsafe { context.Unmap(&staging, 0) };
    Ok(gray)
}

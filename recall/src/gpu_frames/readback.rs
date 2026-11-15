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
    read_from(frame, &tex.clone(), index)
}

/// A screenshot's exact BGRA picture with NV12 frames (`Pipeline::picture`): the canvas holding
/// the newest frame, and that frame (its device lock and size).
pub struct Picture {
    pub(super) frame: HwFrame,
    /// The canvas, or the held capture surface.
    pub(super) texture: ID3D11Texture2D,
    /// The picture's region of `texture` (a capture surface); None = the whole frame (canvas).
    pub(super) rect: Option<crate::framing::Rect>,
    /// Keeps a capture surface out of WGC's pool while this picture lives.
    pub(super) keep: Option<windows::Graphics::Capture::Direct3D11CaptureFrame>,
}

// SAFETY: like HwFrame; the canvas is only touched under the FFmpeg device lock.
unsafe impl Send for Picture {}

impl Picture {
    /// Another reference to the same frame and canvas (refcounts only).
    pub fn try_clone(&self) -> Option<Self> {
        Some(Self { frame: self.frame.try_clone()?, texture: self.texture.clone(), rect: self.rect, keep: self.keep.clone() })
    }
}

/// Tightly packed RGBA8 of a [`Picture`] (the canvas is copied under the device lock, so it is
/// one whole frame: this one or, if the next landed first, the next).
pub fn read_picture(picture: &Picture) -> Result<(u32, u32, Vec<u8>), String> {
    read_region(&picture.frame, &picture.texture, picture.rect)
}

/// `tex` slice `index` as RGBA8, `frame`'s size, on `frame`'s device.
fn read_from(frame: &HwFrame, tex: &ID3D11Texture2D, index: u32) -> Result<(u32, u32, Vec<u8>), String> {
    read_region_at(frame, tex, index, None)
}

/// `rect` of `tex` (None: the frame's size from the origin) as RGBA8.
fn read_region(frame: &HwFrame, tex: &ID3D11Texture2D, rect: Option<crate::framing::Rect>) -> Result<(u32, u32, Vec<u8>), String> {
    read_region_at(frame, tex, 0, rect)
}

fn read_region_at(frame: &HwFrame, tex: &ID3D11Texture2D, index: u32, rect: Option<crate::framing::Rect>) -> Result<(u32, u32, Vec<u8>), String> {
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
    let (w, h) = match rect {
        Some(r) if r.x + r.w <= desc.Width && r.y + r.h <= desc.Height => (r.w, r.h),
        Some(r) => return Err(format!("picture region {r:?} outside the {}x{} surface", desc.Width, desc.Height)),
        None => (frame.width().min(desc.Width), frame.height().min(desc.Height)),
    };
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
        let region = rect.map(|r| D3D11_BOX { left: r.x, top: r.y, front: 0, right: r.x + r.w, bottom: r.y + r.h, back: 1 });
        context.CopySubresourceRegion(&staging, 0, 0, 0, 0, tex, sub, region.as_ref().map(|b| b as *const _));
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("readback map: {e}"))?;
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData as *const u8;
        if super::nv12_read::is_nv12(&desc) {
            // SAFETY: a mapped NV12 surface holds Height luma rows plus Height/2 chroma rows.
            let rows = desc.Height as usize;
            let bytes = std::slice::from_raw_parts(base, pitch * (rows + rows.div_ceil(2)));
            out = super::nv12_read::rgba(bytes, pitch, rows, w as usize, h as usize);
            context.Unmap(&staging, 0);
            return Ok((w, h, out));
        }
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
/// to record the copy, the mip filter and the small copy out, a fence signal and a `Flush`;
/// the thread then sleeps on the fence's event outside the lock until the GPU is done, and
/// maps with `D3D11_MAP_FLAG_DO_NOT_WAIT`, so this low-priority thread never holds the lock
/// while the GPU works. Without a fence (no D3D11.4) the map polls as before.
#[derive(Default)]
pub struct GrayReader {
    textures: Option<GrayTextures>,
    nv12: Option<super::nv12_read::Nv12Staging>,
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
    /// Wakes the reader when the copy out is done (None without D3D11.4).
    fence: Option<super::fence::GpuFence>,
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
    /// The reader's textures wait on a fence (tests).
    #[cfg(test)]
    pub fn fenced(&self) -> bool {
        // BGRA frames read through `textures`, NV12 frames through `nv12`: each has its fence.
        self.textures.as_ref().is_some_and(|t| t.fence.is_some()) || self.nv12.as_ref().is_some_and(|s| s.fence.is_some())
    }

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
        if super::nv12_read::is_nv12(&desc) {
            return self.read_nv12(frame, content, &device, &context, &desc);
        }
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
        let t = self.textures.as_mut().ok_or("hud textures missing")?;
        let region = D3D11_BOX { left: x, top: y, front: 0, right: x + w, bottom: y + h, back: 1 };
        {
            let _lock = unsafe { DeviceLock::take(hw) };
            let sub = index * desc.MipLevels.max(1);
            unsafe {
                context.CopySubresourceRegion(&t.mip, 0, 0, 0, 0, tex, sub, Some(&region));
                context.GenerateMips(&t.srv);
                context.CopySubresourceRegion(&t.staging, 0, 0, 0, 0, &t.mip, t.level, None);
            }
            if t.fence.as_mut().is_some_and(|f| !f.signal(&context)) {
                t.fence = None; // the context cannot signal: poll from now on
            }
            unsafe { context.Flush() };
        }
        let started = std::time::Instant::now();
        if let Some(fence) = &t.fence {
            // Bounded: a hung GPU fails this read, as the polling did.
            if !fence.wait(MAP_WAIT.as_millis() as u32) {
                return Err("hud readback: the GPU did not finish within 250 ms".into());
            }
        }
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
                    // timer-ok: only without a fence (no D3D11.4), or a driver that maps late.
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

impl GrayReader {
    /// NV12 frames: the whole frame comes back (one copy, about 5.5 MB at 1440p, a few times per
    /// probe) and the luma plane is box-filtered on the CPU to the BGRA path's size.
    fn read_nv12(&mut self, frame: &HwFrame, content: Option<crate::framing::Rect>, device: &ID3D11Device, context: &ID3D11DeviceContext, desc: &D3D11_TEXTURE2D_DESC) -> Result<crate::hud_probe::Gray, String> {
        let (tex, index) = frame.texture().ok_or("frame has no texture")?;
        let hw = frame.device_hwctx().ok_or("frame has no device")?;
        let (fw, fh) = (frame.width().min(desc.Width), frame.height().min(desc.Height));
        let r = content.filter(|r| r.w > 0 && r.h > 0 && r.x + r.w <= fw && r.y + r.h <= fh);
        let (x, y, w, h) = r.map_or((0, 0, fw, fh), |r| (r.x, r.y, r.w, r.h));
        if w < 16 || h < 16 {
            return Err(format!("picture {w}x{h} is too small"));
        }
        let st = super::nv12_read::Nv12Staging::for_frame(&mut self.nv12, device, desc)?;
        {
            let _lock = unsafe { DeviceLock::take(hw) };
            unsafe { context.CopySubresourceRegion(&st.texture, 0, 0, 0, 0, tex, index * desc.MipLevels.max(1), None) };
            if st.fence.as_mut().is_some_and(|f| !f.signal(context)) {
                st.fence = None; // the context cannot signal: the map retries from now on
            }
            unsafe { context.Flush() };
        }
        if let Some(fence) = &st.fence {
            // Bounded: a hung GPU fails this read.
            if !fence.wait(MAP_WAIT.as_millis() as u32) {
                return Err("hud readback: the GPU did not finish within 250 ms".into());
            }
        }
        let staging = st.texture.clone();
        let mapped = super::nv12_read::map_polled(hw, context, &staging)?;
        let pitch = mapped.RowPitch as usize;
        // SAFETY: the mapped staging texture holds Height luma rows of `pitch` bytes until Unmap.
        let luma = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * desc.Height as usize) };
        let gray = super::nv12_read::gray(luma, pitch, (x as usize, y as usize, w as usize, h as usize), gray_level(w));
        let _lock = unsafe { DeviceLock::take(hw) };
        unsafe { context.Unmap(&staging, 0) };
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
        let fence = super::fence::GpuFence::new(device);
        Ok(Self { key, mip, srv: srv.ok_or("hud mip view missing")?, staging: staging.ok_or("hud staging texture missing")?, level, size, fence })
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

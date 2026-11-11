//! FFmpeg D3D11 hardware contexts on the capture device, and [`HwFrame`], an owned
//! `AVFrame` whose pixels live in a D3D11 texture (FFmpeg's `AV_PIX_FMT_D3D11`).
//!
//! The `AVD3D11VA*` structs mirror `libavutil/hwcontext_d3d11va.h` (FFmpeg 7.1); ffmpeg-sys
//! does not bind that header because it needs `d3d11.h`.

use std::ffi::c_void;

use ffmpeg_next::ffi as sys;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET};

#[repr(C)]
pub(crate) struct AVD3D11VADeviceContext {
    pub device: *mut c_void,
    pub device_context: *mut c_void,
    pub video_device: *mut c_void,
    pub video_context: *mut c_void,
    pub lock: Option<unsafe extern "C" fn(*mut c_void)>,
    pub unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    pub lock_ctx: *mut c_void,
}

#[repr(C)]
struct AVD3D11VAFramesContext {
    texture: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
    texture_infos: *mut c_void,
}

/// `AVERROR` code -> readable text.
pub(crate) fn av_err(code: i32) -> String {
    ffmpeg_next::Error::from(code).to_string()
}

/// An owned `AVBufferRef` (device or frames context). Unref'd on drop.
pub(crate) struct BufRef(*mut sys::AVBufferRef);

// SAFETY: AVBufferRef refcounting is atomic; the contexts are only mutated before init.
unsafe impl Send for BufRef {}
unsafe impl Sync for BufRef {}

impl BufRef {
    pub fn as_ptr(&self) -> *mut sys::AVBufferRef {
        self.0
    }
}

impl Drop for BufRef {
    fn drop(&mut self) {
        unsafe { sys::av_buffer_unref(&mut self.0) };
    }
}

/// The D3D11VA device context of an `AVHWDeviceContext` buffer.
pub(crate) unsafe fn d3d11_device_ctx(device_ref: *mut sys::AVBufferRef) -> *mut AVD3D11VADeviceContext {
    let dc = unsafe { (*device_ref).data } as *mut sys::AVHWDeviceContext;
    unsafe { (*dc).hwctx as *mut AVD3D11VADeviceContext }
}

/// Wrap the capture device for FFmpeg (it takes its own references). FFmpeg installs its
/// recursive device lock, which every user of the immediate context here also takes.
pub(crate) fn device_ctx(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<BufRef, String> {
    unsafe {
        let r = sys::av_hwdevice_ctx_alloc(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
        if r.is_null() {
            return Err("av_hwdevice_ctx_alloc(d3d11va) failed".into());
        }
        let buf = BufRef(r);
        let hw = d3d11_device_ctx(r);
        (*hw).device = device.clone().into_raw();
        (*hw).device_context = context.clone().into_raw();
        let ret = sys::av_hwdevice_ctx_init(r);
        if ret < 0 {
            return Err(format!("D3D11 device for FFmpeg: {}", av_err(ret)));
        }
        Ok(buf)
    }
}

/// A frames pool of `width`x`height` BGRA render-target textures (NVENC's ARGB input).
pub(super) fn frames_ctx(device: &BufRef, width: u32, height: u32) -> Result<BufRef, String> {
    frames_ctx_as(device, width, height, sys::AVPixelFormat::AV_PIX_FMT_BGRA)
}

/// A frames pool of `width`x`height` render-target textures of `sw_format` (BGRA for the
/// recording's tonemap, NV12 for the export's video processor output).
pub(crate) fn frames_ctx_as(device: &BufRef, width: u32, height: u32, sw_format: sys::AVPixelFormat) -> Result<BufRef, String> {
    unsafe {
        let r = sys::av_hwframe_ctx_alloc(device.as_ptr());
        if r.is_null() {
            return Err("av_hwframe_ctx_alloc failed".into());
        }
        let buf = BufRef(r);
        let fc = (*r).data as *mut sys::AVHWFramesContext;
        (*fc).format = sys::AVPixelFormat::AV_PIX_FMT_D3D11;
        (*fc).sw_format = sw_format;
        (*fc).width = width as i32;
        (*fc).height = height as i32;
        // 0 = grow on demand with single (non-array) textures: render targets cannot share
        // one array texture with NVENC's per-frame registration.
        (*fc).initial_pool_size = 0;
        let hw = (*fc).hwctx as *mut AVD3D11VAFramesContext;
        (*hw).bind_flags = D3D11_BIND_RENDER_TARGET.0 as u32;
        let ret = sys::av_hwframe_ctx_init(r);
        if ret < 0 {
            return Err(format!("D3D11 frames {width}x{height}: {}", av_err(ret)));
        }
        Ok(buf)
    }
}

/// One frame from a D3D11 frames pool. Owns its `AVFrame` (freed on drop, which returns the
/// texture to the pool once the encoder has let go of it too).
pub struct HwFrame(*mut sys::AVFrame);

// SAFETY: the AVFrame is owned by exactly one HwFrame; the texture's D3D11 use is serialized
// by the FFmpeg device lock and the device's multithread protection.
unsafe impl Send for HwFrame {}

impl HwFrame {
    /// A free frame from `frames`, or the FFmpeg error.
    pub(crate) fn get(frames: &BufRef) -> Result<Self, String> {
        unsafe {
            let f = sys::av_frame_alloc();
            if f.is_null() {
                return Err("av_frame_alloc failed".into());
            }
            let frame = HwFrame(f);
            let ret = sys::av_hwframe_get_buffer(frames.as_ptr(), f, 0);
            if ret < 0 {
                return Err(format!("no free D3D11 frame: {}", av_err(ret)));
            }
            Ok(frame)
        }
    }

    pub fn as_ptr(&self) -> *mut sys::AVFrame {
        self.0
    }

    /// The owned `AVFrame`, for a wrapper that frees it (`ff::frame::Video::wrap`).
    pub(crate) fn into_raw(self) -> *mut sys::AVFrame {
        let frame = self.0;
        std::mem::forget(self);
        frame
    }

    /// Another reference to the same texture (cheap refcount bump).
    pub fn try_clone(&self) -> Option<Self> {
        let f = unsafe { sys::av_frame_clone(self.0) };
        (!f.is_null()).then_some(HwFrame(f))
    }

    pub fn width(&self) -> u32 {
        unsafe { (*self.0).width as u32 }
    }

    pub fn height(&self) -> u32 {
        unsafe { (*self.0).height as u32 }
    }

    pub fn set_pts(&mut self, pts: i64) {
        unsafe { (*self.0).pts = pts };
    }

    /// The frame's texture and its array index (FFmpeg keeps them in `data[0]` / `data[1]`).
    pub(crate) fn texture(&self) -> Option<(&ID3D11Texture2D, u32)> {
        unsafe {
            let raw = &*((&(*self.0).data[0]) as *const *mut u8 as *const *mut c_void);
            let tex = ID3D11Texture2D::from_raw_borrowed(raw)?;
            Some((tex, (*self.0).data[1] as usize as u32))
        }
    }

    /// The frame's frames context (`hw_frames_ctx`), borrowed. Each frame carries its own
    /// reference, so compare pools with [`Self::pool_id`], never with this pointer.
    pub fn frames_ctx(&self) -> *mut sys::AVBufferRef {
        unsafe { (*self.0).hw_frames_ctx }
    }

    /// Identity of the frames pool (the shared `AVHWFramesContext`), equal for every frame
    /// of one pool.
    pub fn pool_id(&self) -> usize {
        let r = self.frames_ctx();
        if r.is_null() {
            return 0;
        }
        unsafe { (*r).data as usize }
    }

    /// The D3D11VA device context the frame's pool belongs to.
    pub(crate) fn device_hwctx(&self) -> Option<*mut AVD3D11VADeviceContext> {
        unsafe {
            let frames = (*self.0).hw_frames_ctx;
            if frames.is_null() {
                return None;
            }
            let fc = (*frames).data as *mut sys::AVHWFramesContext;
            Some(d3d11_device_ctx((*fc).device_ref))
        }
    }
}

impl Drop for HwFrame {
    fn drop(&mut self) {
        unsafe { sys::av_frame_free(&mut self.0) };
    }
}

/// Holds the FFmpeg device lock (recursive) for its lifetime.
pub(crate) struct DeviceLock(*mut AVD3D11VADeviceContext);

impl DeviceLock {
    pub unsafe fn take(hw: *mut AVD3D11VADeviceContext) -> Self {
        if let Some(lock) = unsafe { (*hw).lock } {
            unsafe { lock((*hw).lock_ctx) };
        }
        DeviceLock(hw)
    }
}

impl Drop for DeviceLock {
    fn drop(&mut self) {
        if let Some(unlock) = unsafe { (*self.0).unlock } {
            unsafe { unlock((*self.0).lock_ctx) };
        }
    }
}

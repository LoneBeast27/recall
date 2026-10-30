//! Zero-copy recording path (feature `encode`): the WGC capture texture is tonemapped on the
//! GPU, on the capture device, straight into an FFmpeg D3D11 frame that NVENC encodes from
//! VRAM. Measured 2026-10-28 (scripts/dev/engine-cost.ps1, 2560x1440 at 60 fps): the CPU
//! path (staging map + two row copies per frame on 32 rayon threads, a wgpu upload, a wgpu
//! readback, an RGB0 copy and NVENC's own upload copy, plus a busy present loop) cost 2.5
//! cores; this path records a draw call per frame. The old path stays as the fallback when
//! this one cannot start (logged), and for the RECALL_DUMP verification harness.

mod av;
mod pipeline;
mod readback;
#[cfg(test)]
mod tests;

pub use av::HwFrame;
pub use pipeline::Pipeline;
pub use readback::read_rgba;

/// A hardware D3D11 device (tests and `--gpu-smoke`; recording uses the capture device).
pub fn hardware_device() -> Option<(
    windows::Win32::Graphics::Direct3D11::ID3D11Device,
    windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
)> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
    use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
    let (mut dev, mut ctx) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            Some(&mut ctx),
        )
        .ok()?;
    }
    Some((dev?, ctx?))
}

/// Largest window-border jitter (px per axis) absorbed by cropping / padding instead of
/// reopening the encoder (encoder/frame_fit.rs keeps the same rule for CPU frames).
pub const MAX_JITTER_PIXELS: u32 = 8;

/// Encoder frame size for a `src` capture recorded at its own size (not on the windowed-game
/// canvas, `framing`): the current size while the source stays within [`MAX_JITTER_PIXELS`]
/// of it, else the source cropped to even dimensions (NVENC).
pub fn target_size(current: Option<(u32, u32)>, src: (u32, u32)) -> (u32, u32) {
    if let Some((w, h)) = current {
        if src.0.abs_diff(w) <= MAX_JITTER_PIXELS && src.1.abs_diff(h) <= MAX_JITTER_PIXELS {
            return (w, h);
        }
    }
    ((src.0 & !1).max(2), (src.1 & !1).max(2))
}

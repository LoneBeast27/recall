//! Dedicated VRAM of the GPU the capture device runs on, for `format::start_fp16` (a card
//! with 4 GiB or less keeps Bgra8 rather than fp16 capture ahead of time).
//!
//! The format is picked before the capture makes its device. The capture device is made on
//! the default adapter (`windows_capture::d3d11::create_d3d_device`: no adapter given, hardware
//! driver), which is DXGI adapter 0 for this process, so each start reads adapter 0's
//! description from a DXGI factory: no extra device at start (review 11-07, Rohan: nothing
//! but essentials at start-up). Desktop Duplication
//! refuses any adapter but that default one (`dda_dxgi::find_target`), so it is the capture device's adapter either way,
//! on a hybrid laptop too. Read per start, never cached: the default adapter can change
//! (GPU preference, eGPU, driver reset).

/// Dedicated VRAM of the adapter a capture device is created on now: the default adapter,
/// DXGI adapter 0, read from a factory (no device is made).
pub(crate) fn of_capture_device() -> Option<u64> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    let desc = unsafe { factory.EnumAdapters1(0).ok()?.GetDesc1().ok()? };
    Some(desc.DedicatedVideoMemory as u64)
}

//! Dedicated VRAM of the GPU the capture device runs on, for `format::start_fp16` (a card
//! with 4 GiB or less keeps Bgra8 rather than fp16 capture ahead of time).
//!
//! The format is picked before the capture makes its device, so each start makes a device
//! the way the WGC capture does (`windows_capture::d3d11::create_d3d_device`: default
//! adapter, hardware driver) and reads the adapter that device was created on
//! (IDXGIDevice -> GetAdapter -> GetDesc). Desktop Duplication refuses any adapter but that
//! default one (`dda_dxgi::find_target`), so it is the capture device's adapter either way,
//! on a hybrid laptop too. Read per start, never cached: the default adapter can change
//! (GPU preference, eGPU, driver reset).

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

/// Dedicated VRAM of the adapter `device` was created on, in bytes.
pub(crate) fn of_device(device: &ID3D11Device) -> Option<u64> {
    let dxgi = device.cast::<IDXGIDevice>().ok()?;
    let desc = unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok()? };
    Some(desc.DedicatedVideoMemory as u64)
}

/// Dedicated VRAM of the adapter a capture device is created on now (a device made the same
/// way, then dropped).
pub(crate) fn of_capture_device() -> Option<u64> {
    let (device, _context) = windows_capture::d3d11::create_d3d_device().ok()?;
    of_device(&device)
}

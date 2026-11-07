//! Dedicated VRAM of the GPU the capture device runs on, for `format::start_fp16` (a card
//! with 4 GiB or less keeps Bgra8 rather than fp16 capture ahead of time).
//!
//! The capture device's own adapter is read once per capture start on the GPU path
//! (`gpu_path`: IDXGIDevice -> GetAdapter -> GetDesc) and kept in
//! `CaptureShared::capture_vram`. Before any capture device exists (the first start), the
//! value comes from DXGI adapter 0: WGC's device is created on the default adapter
//! (windows-capture `create_d3d_device`, null adapter) and Desktop Duplication refuses any
//! other (`dda_dxgi::find_target`), so that is the adapter the device will be made on.

#[cfg(feature = "encode")]
use windows::{core::Interface, Win32::Graphics::Direct3D11::ID3D11Device, Win32::Graphics::Dxgi::IDXGIDevice};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

/// Dedicated VRAM of the adapter `device` runs on, in bytes.
#[cfg(feature = "encode")]
pub(crate) fn of_device(device: &ID3D11Device) -> Option<u64> {
    let dxgi = device.cast::<IDXGIDevice>().ok()?;
    let desc = unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok()? };
    Some(desc.DedicatedVideoMemory as u64)
}

/// Dedicated VRAM of DXGI adapter 0 (the default adapter capture devices are made on).
pub(crate) fn of_default_adapter() -> Option<u64> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    let desc = unsafe { factory.EnumAdapters1(0).ok()?.GetDesc1().ok()? };
    Some(desc.DedicatedVideoMemory as u64)
}

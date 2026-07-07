//! Primary-display HDR-state detection via DXGI.
//!
//! Ported from `recall/src/hdr.rs`. We need this because the NvAPI gamut clamp is
//! REJECTED while Windows HDR is ON (driver constraint since 531.79 -> error
//! -104). Before any clamp SET, and on any -104, we check this so we can abort
//! cleanly with an accurate diagnosis instead of blaming the FFI layer.

use windows::core::Interface;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};

/// Whether the primary display is currently in HDR (scRGB PQ colorspace).
/// `None` if DXGI enumeration fails (treat as "unknown", not "off").
pub fn primary_is_hdr() -> Option<bool> {
    unsafe {
        let primary = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;

        let mut ai = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            ai += 1;
            let mut oi = 0u32;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                oi += 1;
                let Ok(output6) = output.cast::<IDXGIOutput6>() else {
                    continue;
                };
                let Ok(desc) = output6.GetDesc1() else {
                    continue;
                };
                if desc.Monitor.0 != primary.0 {
                    continue;
                }
                return Some(desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
            }
        }
        None
    }
}

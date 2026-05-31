//! Primary-display HDR detection via DXGI.
//!
//! Used to auto-select tonemap (HDR source) vs passthrough (SDR source), and
//! re-checked periodically so toggling Windows HDR updates the mode live during
//! a session. Matches the DXGI output to the primary monitor by HMONITOR.

use windows::core::Interface;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY};

/// What we learned about the primary display's HDR state.
pub struct HdrInfo {
    pub is_hdr: bool,
    pub max_nits: f32,
}

/// Query the primary monitor's color space. `None` if DXGI enumeration fails.
pub fn primary_hdr() -> Option<HdrInfo> {
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
                let is_hdr = desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
                return Some(HdrInfo {
                    is_hdr,
                    max_nits: desc.MaxLuminance,
                });
            }
        }
        None
    }
}

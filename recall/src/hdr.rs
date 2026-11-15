//! Primary-display HDR detection via DXGI.
//!
//! Used to auto-select tonemap (HDR source) vs passthrough (SDR source), and
//! re-checked periodically so toggling Windows HDR updates the mode live during
//! a session. Matches the DXGI output to the primary monitor by HMONITOR.
//!
//! The check runs every second, but the outputs are enumerated only when something
//! changed: the DXGI factory is kept, and Windows marks it stale (`IsCurrent` false) when
//! an adapter or output changes, an HDR toggle included. A new primary monitor also
//! enumerates again, and so does a 30 s backstop ([`BACKSTOP`]): that an HDR toggle makes
//! the factory stale is documented (the D3D12 HDR sample relies on it) but was not checked
//! on this machine, since toggling HDR is a display setting change.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTOPRIMARY};

/// What we learned about the primary display's HDR state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrInfo {
    pub is_hdr: bool,
    pub max_nits: f32,
}

/// The last answer, valid while its factory is current and the primary monitor is the same.
struct Cached {
    factory: IDXGIFactory1,
    primary: HMONITOR,
    info: HdrInfo,
    at: Instant,
}

/// timer-ok: re-enumerate this often even while the factory says it is current, in case an
/// HDR toggle on some driver does not make it stale.
const BACKSTOP: Duration = Duration::from_secs(30);

thread_local! {
    static CACHE: RefCell<Option<Cached>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static ENUMERATIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Query the primary monitor's color space. `None` if DXGI enumeration fails (not cached:
/// the next call tries again).
pub fn primary_hdr() -> Option<HdrInfo> {
    let primary = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(c) = cache.as_ref().filter(|c| c.primary == primary && c.at.elapsed() < BACKSTOP && unsafe { c.factory.IsCurrent() }.as_bool()) {
            return Some(c.info);
        }
        *cache = None;
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
        let info = find(&factory, primary)?;
        *cache = Some(Cached { factory, primary, info, at: Instant::now() });
        Some(info)
    })
}

/// The primary monitor's output among `factory`'s adapters.
fn find(factory: &IDXGIFactory1, primary: HMONITOR) -> Option<HdrInfo> {
    #[cfg(test)]
    ENUMERATIONS.with(|n| n.set(n.get() + 1));
    unsafe {
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
                return Some(HdrInfo { is_hdr, max_nits: desc.MaxLuminance });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeat_check_reuses_the_factory_and_enumerates_nothing() {
        let first = primary_hdr();
        let after_first = ENUMERATIONS.with(|n| n.get());
        if first.is_none() {
            // No DXGI output for the primary monitor here (a headless runner): nothing cached.
            assert!(primary_hdr().is_none());
            return;
        }
        for _ in 0..5 {
            assert_eq!(primary_hdr(), first, "same answer while nothing changed");
        }
        assert_eq!(ENUMERATIONS.with(|n| n.get()), after_first, "no new enumeration while the factory is current");
        // A stale factory (an HDR toggle, a display change) enumerates again.
        CACHE.with(|c| c.borrow_mut().as_mut().unwrap().primary = HMONITOR(std::ptr::null_mut()));
        assert_eq!(primary_hdr(), first);
        assert_eq!(ENUMERATIONS.with(|n| n.get()), after_first + 1, "a changed primary monitor enumerates once");
        CACHE.with(|c| c.borrow_mut().as_mut().unwrap().at -= BACKSTOP);
        assert_eq!(primary_hdr(), first);
        assert_eq!(ENUMERATIONS.with(|n| n.get()), after_first + 2, "the 30 s backstop enumerates again");
    }
}

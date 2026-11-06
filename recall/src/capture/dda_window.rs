//! Game capture through monitor Desktop Duplication: a game window that covers its whole
//! monitor (borderless or exclusive fullscreen) shows exactly what that monitor shows, so it
//! can be recorded with monitor DDA (no WGC border) instead of WGC window capture. Behind
//! `RECALL_CAPTURE=dda` like the rest of DDA.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONULL};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GetWindowRect, IsIconic, IsWindow, GWL_STYLE};

use super::dda_logic::{covers, frameless, Bounds};

/// The monitor `hwnd` covers completely, if it does (and is not minimized).
pub(super) fn covering_monitor(hwnd: usize) -> Option<HMONITOR> {
    let window = HWND(hwnd as *mut _);
    unsafe {
        if !IsWindow(Some(window)).as_bool() || IsIconic(window).as_bool() {
            return None;
        }
        // Only a frameless game: a maximized window with a title bar stays on WGC window capture.
        if !frameless(GetWindowLongPtrW(window, GWL_STYLE) as u32) {
            return None;
        }
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONULL);
        if monitor.is_invalid() {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(window, &mut rect).ok()?;
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        covers(bounds(rect), bounds(info.rcMonitor)).then_some(monitor)
    }
}

/// `hwnd` still covers the monitor (an `HMONITOR` value) the capture duplicates.
pub(super) fn still_covers(hwnd: usize, monitor: usize) -> bool {
    covering_monitor(hwnd).is_some_and(|m| m.0 as usize == monitor)
}

fn bounds(r: RECT) -> Bounds {
    Bounds { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

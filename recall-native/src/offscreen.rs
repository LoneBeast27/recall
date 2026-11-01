//! `RECALL_NATIVE_OFFSCREEN=1` (scripted checks): the window runs outside every monitor, out of
//! the taskbar and Alt+Tab, and never activates. It stays shown, not hidden: the video child
//! window has to keep presenting frames for the checks to read the picture.
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, MonitorFromRect},
    System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX, SetErrorMode},
    UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetSystemMetrics, GetWindowLongPtrW, HWND_BOTTOM, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos,
        WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    },
};

/// Clear space between the window and the nearest monitor.
const GAP: i32 = 200;

pub fn enabled() -> bool {
    std::env::var_os("RECALL_NATIVE_OFFSCREEN").is_some()
}

/// No Windows error boxes: a test instance fails in its log, never in a dialog on the desktop.
pub fn quiet_errors() {
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX) };
}

pub fn show(window: &gpui::Window, width: f32, height: f32) {
    let Ok(raw) = crate::root::parent_hwnd(window) else { return };
    let hwnd = HWND(raw as *mut _);
    let s = window.scale_factor();
    let (w, h) = ((width * s).round() as i32, (height * s).round() as i32);
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & !(WS_EX_APPWINDOW.0 as isize);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) as isize);
    }
    let (x, y) = origin(desktop(), (w, h), |x, y| nearest_is_primary(x, y, w, h));
    let _ = unsafe { SetWindowPos(hwnd, Some(HWND_BOTTOM), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW) };
}

/// The virtual desktop: left, top, right, bottom.
fn desktop() -> (i32, i32, i32, i32) {
    let (left, top) = unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN)) };
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) };
    (left, top, left + width, top + height)
}

fn nearest_is_primary(x: i32, y: i32, w: i32, h: i32) -> bool {
    let rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) == MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) }
}

/// A spot past the desktop's edge. The first one whose nearest monitor is the primary wins: next
/// to a monitor at another scale, Windows would resize the window and the checks' clicks miss.
fn origin((left, top, right, bottom): (i32, i32, i32, i32), (w, h): (i32, i32), nearest_primary: impl Fn(i32, i32) -> bool) -> (i32, i32) {
    let spots = [(0, bottom + GAP), (right + GAP, 0), (left - w - GAP, 0), (0, top - h - GAP)];
    spots.into_iter().find(|&(x, y)| nearest_primary(x, y)).unwrap_or(spots[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spot_is_outside_the_desktop_and_the_primary_wins() {
        let desktop = (-1440, -300, 2560, 2260);
        let size = (1280, 820);
        let outside = |(x, y): (i32, i32)| x + size.0 <= desktop.0 || x >= desktop.2 || y + size.1 <= desktop.1 || y >= desktop.3;
        assert_eq!(origin(desktop, size, |_, _| true), (0, 2460));
        assert_eq!(origin(desktop, size, |_, y| y < 2000), (2760, 0), "below is nearest another monitor");
        assert_eq!(origin(desktop, size, |x, _| x < 0), (-2920, 0));
        assert_eq!(origin(desktop, size, |_, y| y < 0), (0, -1320));
        assert_eq!(origin(desktop, size, |_, _| false), (0, 2460), "no such spot: still off the desktop");
        assert!([(0, 2460), (2760, 0), (-2920, 0), (0, -1320)].into_iter().all(outside));
    }
}

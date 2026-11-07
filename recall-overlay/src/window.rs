//! The overlay window: a layered, click-through, no-activate, topmost tool
//! popup whose pixels come from `UpdateLayeredWindow` (so it must never call
//! `SetLayeredWindowAttributes`), and the monitor the island sits on.

use windows::core::{w, BOOL};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, EnumWindows, GetClientRect, GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, LoadCursorW,
    RegisterClassW, IDC_ARROW, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::placement::Rect;
use crate::window_proc;

pub(super) fn create_window() -> Result<HWND, String> {
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("RecallNativeStatusOverlay"),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("RegisterClassW failed".into());
        }
        let extended = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
        CreateWindowExW(
            extended,
            w!("RecallNativeStatusOverlay"),
            w!("Recall status"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))
    }
}

/// The monitor of the game window (`hwnd` while it exists and shows, else
/// the pid's largest visible top-level window), else the primary monitor,
/// and the area the island may use on it ([`island_area`]). `on_window`:
/// Dynamic, which places a windowed game's island on the game picture.
pub(super) fn target_monitor(hwnd: Option<isize>, pid: Option<u32>, on_window: bool) -> (Rect, Rect) {
    let window = hwnd
        .map(|h| HWND(h as *mut _))
        .filter(|&h| unsafe { IsWindow(Some(h)).as_bool() && IsWindowVisible(h).as_bool() })
        .or_else(|| pid.and_then(main_window_of));
    let monitor = match window {
        Some(window) => unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTOPRIMARY) },
        None => unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
    };
    let (full, work) = monitor_rects(monitor);
    let frame = window.and_then(|w| {
        let mut r = Default::default();
        unsafe { GetWindowRect(w, &mut r) }.ok().map(|()| rect(r))
    });
    (full, island_area(full, work, frame, window.and_then(client_rect), on_window))
}

fn rect(r: windows::Win32::Foundation::RECT) -> Rect {
    Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

/// The window's client area in screen coordinates (the game picture).
fn client_rect(window: HWND) -> Option<Rect> {
    let mut r = Default::default();
    unsafe { GetClientRect(window, &mut r) }.ok()?;
    let mut origin = POINT { x: 0, y: 0 };
    unsafe { ClientToScreen(window, &mut origin) }
        .as_bool()
        .then(|| Rect { left: origin.x, top: origin.y, right: origin.x + r.right, bottom: origin.y + r.bottom })
}

pub(super) fn monitor_rects(monitor: HMONITOR) -> (Rect, Rect) {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let fallback = Rect { left: 0, top: 0, right: 1920, bottom: 1080 };
        return (fallback, fallback);
    }
    (rect(info.rcMonitor), rect(info.rcWork))
}

struct Search {
    pid: u32,
    best: Option<(HWND, i64)>,
}

fn main_window_of(pid: u32) -> Option<HWND> {
    let mut search = Search { pid, best: None };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.best.map(|(hwnd, _)| hwnd)
}

unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = unsafe { &mut *(lparam.0 as *mut Search) };
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner as *mut u32)) };
    if owner == search.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
        let mut rect = Default::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
            let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
            if search.best.is_none_or(|(_, best)| area > best) {
                search.best = Some((hwnd, area));
            }
        }
    }
    BOOL(1)
}

/// Where the island may sit: the whole monitor while the game window covers
/// it, else the work area; with `on_window` (Dynamic) a windowed game's client
/// rectangle within the work area, so the HUD grid maps onto the game picture
/// (fullscreen hides the taskbar; the work area keeps it clear). A client
/// rectangle too small for the island (under 320 x 180 within the work area)
/// falls back to the work area.
pub fn island_area(full: Rect, work: Rect, window: Option<Rect>, client: Option<Rect>, on_window: bool) -> Rect {
    let covers = window.is_some_and(|r| r.left <= full.left && r.top <= full.top && r.right >= full.right && r.bottom >= full.bottom);
    if covers {
        return full;
    }
    let inside = client.filter(|_| on_window).map(|c| Rect {
        left: c.left.max(work.left),
        top: c.top.max(work.top),
        right: c.right.min(work.right),
        bottom: c.bottom.min(work.bottom),
    });
    inside.filter(|r| r.right - r.left >= 320 && r.height() >= 180).unwrap_or(work)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect { left, top, right, bottom }
    }

    #[test]
    fn dynamic_maps_a_windowed_game_onto_its_client_rect() {
        let (full, work) = (rect(0, 0, 2560, 1440), rect(0, 0, 2560, 1392));
        let frame = rect(92, 69, 1388, 830);
        let client = rect(100, 100, 1380, 820);
        assert_eq!(island_area(full, work, Some(frame), Some(client), true), client);
        assert_eq!(island_area(full, work, Some(frame), Some(client), false), work, "a fixed corner keeps the monitor");
        assert_eq!(island_area(full, work, Some(full), Some(full), true), full, "a covering game is the monitor");
        let hanging = rect(2000, 1000, 3280, 1720);
        assert_eq!(island_area(full, work, Some(hanging), Some(hanging), true), rect(2000, 1000, 2560, 1392), "clipped to the work area");
        let minimised = rect(-32000, -32000, -31840, -31972);
        assert_eq!(island_area(full, work, Some(minimised), Some(minimised), true), work);
        let tiny = rect(10, 10, 110, 60);
        assert_eq!(island_area(full, work, Some(tiny), Some(tiny), true), work, "too small for the island");
    }
}

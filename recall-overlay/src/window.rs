//! The overlay window: a layered, click-through, no-activate, topmost tool
//! popup whose pixels come from `UpdateLayeredWindow` (so it must never call
//! `SetLayeredWindowAttributes`), and the monitor the island sits on.

use windows::core::{w, BOOL};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, LoadCursorW,
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

/// The monitor of the game window (`hwnd`, else the pid's largest visible
/// top-level window), else the primary monitor.
/// The target's monitor, and the area the island may use on it: the whole
/// monitor while the game covers it (fullscreen hides the taskbar), else the
/// work area (the taskbar stays clear of bottom and side spots).
pub(super) fn target_monitor(hwnd: Option<isize>, pid: Option<u32>) -> (Rect, Rect) {
    let window = hwnd
        .map(|h| HWND(h as *mut _))
        .filter(|&h| unsafe { IsWindow(Some(h)).as_bool() })
        .or_else(|| pid.and_then(main_window_of));
    let monitor = match window {
        Some(window) => unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTOPRIMARY) },
        None => unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
    };
    let (full, work) = monitor_rects(monitor);
    let mut r = Default::default();
    let covers = window.is_some_and(|w| unsafe { GetWindowRect(w, &mut r) }.is_ok())
        && r.left <= full.left && r.top <= full.top && r.right >= full.right && r.bottom >= full.bottom;
    (full, if covers { full } else { work })
}

fn monitor_rects(monitor: HMONITOR) -> (Rect, Rect) {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let fallback = Rect { left: 0, top: 0, right: 1920, bottom: 1080 };
        return (fallback, fallback);
    }
    let rect = |r: windows::Win32::Foundation::RECT| Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
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

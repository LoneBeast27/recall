//! The Record picker's Windows reads: app windows (z-order, front first) and monitors. Read
//! once when the picker opens, off the UI thread; nothing is polled while it is closed.
use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, MONITORINFOF_PRIMARY};
use windows::core::BOOL;

use crate::services::audio_apps;
use crate::services::capture_targets::{MonitorCard, WindowCard};
use crate::services::game_apps;

/// Hidden by the shell (another virtual desktop, a suspended Store app): no picture to show.
fn cloaked(hwnd: HWND) -> bool {
    let mut value = 0u32;
    // SAFETY: a u32 out value of the size passed, for a handle EnumWindows passed in.
    let read = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut value as *mut u32).cast(), std::mem::size_of::<u32>() as u32) };
    read.is_ok() && value != 0
}

fn title(hwnd: HWND) -> String {
    // SAFETY: the buffer is sized from the reported length; a stale handle reads as empty.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; len as usize + 1];
        let read = GetWindowTextW(hwnd, &mut buffer);
        String::from_utf16_lossy(&buffer[..read.max(0) as usize])
    }
}

/// App windows with a title, not cloaked, not Recall's own, not the shell (the same list
/// Settings → Games never offers), front first.
pub fn windows_blocking() -> Vec<WindowCard> {
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec below, alive for the whole EnumWindows call.
        let found = unsafe { &mut *(data.0 as *mut Vec<(HWND, u32, String)>) };
        let info = crate::services::game_apps_win::window_info(hwnd);
        // A scripted check offers only its own fixture window (no real window shows).
        let fixture = only_pid().is_some_and(|pid| pid_of(hwnd) == pid && info.visible);
        if (fixture || game_apps::is_app_window(&info)) && !cloaked(hwnd) {
            let text = title(hwnd);
            let mut pid = 0;
            // SAFETY: plain query on a live handle.
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != 0 && !text.trim().is_empty() {
                found.push((hwnd, pid, text));
            }
        }
        BOOL(1)
    }
    let mut found: Vec<(HWND, u32, String)> = Vec::new();
    // SAFETY: the callback only pushes to `found`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut found as *mut Vec<(HWND, u32, String)> as isize)) };
    let own = std::process::id();
    let only = only_pid();
    found
        .into_iter()
        .filter(|(_, pid, _)| *pid != own && only.is_none_or(|o| o == *pid))
        .filter_map(|(hwnd, pid, title)| {
            let path = audio_apps::image_path(pid).unwrap_or_default();
            let exe = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (!exe.is_empty() && !game_apps::never_offered(&exe)).then_some(WindowCard { hwnd: hwnd.0 as isize, pid, title, exe, path })
        })
        .collect()
}

/// `RECALL_PICKER_ONLY_PID` (scripted checks): offer that process's windows only.
fn only_pid() -> Option<u32> {
    std::env::var("RECALL_PICKER_ONLY_PID").ok()?.trim().parse().ok()
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0;
    // SAFETY: plain query on a live handle.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// Every monitor: device name, desktop rect, primary.
pub fn monitors_blocking() -> Vec<MonitorCard> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, out: LPARAM) -> BOOL {
        // SAFETY: `out` is the Vec below, alive for the whole EnumDisplayMonitors call.
        let list = unsafe { &mut *(out.0 as *mut Vec<MonitorCard>) };
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
        // SAFETY: a sized MONITORINFOEXW for a handle the enumeration passed in.
        if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            let end = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
            let r = info.monitorInfo.rcMonitor;
            list.push(MonitorCard {
                device: String::from_utf16_lossy(&info.szDevice[..end]),
                rect: [r.left, r.top, r.right, r.bottom],
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        BOOL(1)
    }
    let mut list: Vec<MonitorCard> = Vec::new();
    // SAFETY: the callback only pushes to `list`, which outlives the call.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut list as *mut Vec<MonitorCard> as isize)) };
    list
}

/// How many monitors are connected (the picker shows with 2 or more). Cheap: one enumeration.
pub fn monitor_count() -> usize {
    monitors_blocking().len()
}

//! Source-name detection for the live SDR window's title.
//!
//! The streamed window is named simply `<source> SDR` (no "Recall" branding —
//! that belongs to the clipping/gallery side). Crucially, in a multi-monitor
//! setup we name after the **topmost real window on the captured monitor**, not
//! the system-wide foreground app — so clicking a window on a *secondary*
//! monitor never renames the source away from the game on the captured monitor.

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

/// Exe stems we never surface as the SDR source: shells, capture tools, ourselves.
const DENY_STEMS: &[&str] = &[
    "recall",
    "explorer",
    "obs64",
    "obs32",
    "obs",
    "discord",
    "applicationframehost",
    "shellexperiencehost",
    "searchhost",
    "startmenuexperiencehost",
    "textinputhost",
    "windowsterminal",
    "cmd",
    "conhost",
    "powershell",
    "pwsh",
    "code",
    "devenv",
];

/// Minimum window size (px) to be considered a real source (skips tooltips etc.).
const MIN_DIM: i32 = 200;

/// State threaded through the `EnumWindows` callback.
struct Scan {
    /// The captured monitor; only windows primarily on it qualify.
    target: HMONITOR,
    /// First (topmost in Z-order) qualifying window's source name.
    found: Option<String>,
    /// That window's process id.
    pid: Option<u32>,
}

/// Name of the source on the captured (primary) monitor, or `None` if nothing
/// qualifies — in which case the caller keeps the previous title.
pub fn captured_source() -> Option<String> {
    unsafe {
        let target = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut scan = Scan { target, found: None, pid: None };
        // EnumWindows visits top-level windows in Z-order (topmost first); the
        // callback stops at the first qualifying window on the target monitor.
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut scan as *mut Scan as isize));
        scan.found
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let scan = &mut *(lparam.0 as *mut Scan);

    // Cheap rejects first (avoid OpenProcess on most windows).
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return BOOL(1); // keep enumerating
    }
    let exstyle = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if exstyle & WS_EX_TOOLWINDOW.0 != 0 {
        return BOOL(1); // tool windows: overlays, our own off-screen window
    }
    if MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL).0 != scan.target.0 {
        return BOOL(1); // not on the captured monitor
    }
    let mut r = RECT::default();
    if GetWindowRect(hwnd, &mut r).is_err() || (r.right - r.left) < MIN_DIM || (r.bottom - r.top) < MIN_DIM
    {
        return BOOL(1);
    }

    // Process identity for the denylist (skip shells/capture tools/ourselves).
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    let stem = exe_stem(pid);
    if let Some(s) = &stem {
        if DENY_STEMS.contains(&s.to_ascii_lowercase().as_str()) {
            return BOOL(1);
        }
    }

    // Prefer the window title (games name their window); fall back to exe stem.
    let title = window_title(hwnd);
    let name = if title.trim().is_empty() {
        match stem {
            Some(s) => s,
            None => return BOOL(1),
        }
    } else {
        title.trim().to_string()
    };

    scan.found = Some(sanitize(&name));
    scan.pid = Some(pid);
    BOOL(0) // found the topmost qualifying window; stop enumerating
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

/// Full exe path of the topmost source window on the captured monitor (the game,
/// when one is up). The spill file avoids that exe's disk.
pub fn captured_exe_path() -> Option<std::path::PathBuf> {
    let pid = unsafe {
        let target = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut scan = Scan { target, found: None, pid: None };
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut scan as *mut Scan as isize));
        scan.pid?
    };
    exe_path(pid).map(std::path::PathBuf::from)
}

fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let res =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        res.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn exe_stem(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let res =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        res.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Some(
            std::path::Path::new(&path)
                .file_stem()?
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Collapse whitespace and cap length (on char boundaries) so titles stay tidy.
fn sanitize(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(64).collect()
}

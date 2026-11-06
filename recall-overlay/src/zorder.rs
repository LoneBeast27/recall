//! What sits over the island: the topmost re-raise (games such as Rocket
//! League make their own window topmost) and the facts the change-only log
//! (`trace`) records about it: the window found above, the foreground window,
//! and the shell's notification state (3 = a D3D exclusive fullscreen app).

use std::collections::HashMap;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, SetWindowPos, GWL_EXSTYLE, GW_HWNDPREV, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOOWNERZORDER, SWP_NOSIZE, WS_EX_TOPMOST,
};

use crate::window::monitor_rects;

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHQueryUserNotificationState(state: *mut i32) -> i32;
}

/// The shell's notification state, named (`QUNS_*`), or the failing HRESULT.
pub fn notification_state() -> String {
    let mut state = 0i32;
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    if hr != 0 {
        return format!("query failed {:#010x}", hr as u32);
    }
    let name = match state {
        1 => "not present (screen saver / locked / fast user switch)",
        2 => "busy (fullscreen app, not D3D exclusive)",
        3 => "running D3D fullscreen (exclusive)",
        4 => "presentation mode",
        5 => "accepts notifications",
        6 => "quiet time",
        7 => "UWP app fullscreen",
        _ => "unknown",
    };
    format!("{state} {name}")
}

/// The last Win32 error as `message (0x…)`.
pub fn win_error(error: &windows::core::Error) -> String {
    format!("{} ({:#010x})", error.message(), error.code().0 as u32)
}

/// The outcome of one re-raise check.
pub enum Raise {
    /// Nothing visible above overlaps the island.
    Clear,
    /// A window above overlapped it: its description and the SetWindowPos result.
    Raised { above: String, result: Result<(), String> },
}

/// Exe names by pid, so a raise every frame does not open the process each time.
#[derive(Default)]
pub struct Names {
    exes: HashMap<u32, String>,
}

impl Names {
    pub fn exe(&mut self, pid: u32) -> String {
        if self.exes.len() > 256 {
            self.exes.clear();
        }
        self.exes.entry(pid).or_insert_with(|| exe_of(pid)).clone()
    }

    /// `exe pid N class C rect [l,t,r,b] topmost yes|no`.
    pub fn describe(&mut self, hwnd: HWND) -> String {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let mut buf = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, &mut buf) }.max(0) as usize;
        let class = String::from_utf16_lossy(&buf[..len]);
        let r = rect_of(hwnd).unwrap_or_default();
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let topmost = if ex & WS_EX_TOPMOST.0 != 0 { "yes" } else { "no" };
        format!(
            "{} pid {pid} class {class} rect [{},{},{},{}] topmost {topmost}",
            self.exe(pid),
            r.left,
            r.top,
            r.right,
            r.bottom
        )
    }

    /// The foreground window and whether it covers its whole monitor.
    pub fn foreground(&mut self) -> String {
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_invalid() {
            return "none".into();
        }
        let covers = rect_of(fg).is_some_and(|r| {
            let (full, _) = monitor_rects(unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) });
            r.left <= full.left && r.top <= full.top && r.right >= full.right && r.bottom >= full.bottom
        });
        format!("{} covers monitor {}", self.describe(fg), if covers { "yes" } else { "no" })
    }
}

fn rect_of(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.is_ok().then_some(r)
}

fn exe_of(pid: u32) -> String {
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return "?".into();
    };
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok();
    unsafe {
        let _ = CloseHandle(handle);
    }
    if !ok {
        return "?".into();
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit('\\').next().unwrap_or(&path).to_string()
}

/// Games that make their own window topmost (Rocket League does) cover the
/// island once they are activated. Re-raise it, without focus, only when a
/// visible window above it overlaps it (Explorer keeps 1x1 helper windows on
/// top that never cover anything).
pub fn keep_on_top(hwnd: HWND, names: &mut Names) -> Raise {
    let Some(mine) = rect_of(hwnd) else { return Raise::Clear };
    let mut above = unsafe { GetWindow(hwnd, GW_HWNDPREV) }.unwrap_or_default();
    while !above.is_invalid() {
        let overlaps = rect_of(above)
            .is_some_and(|r| r.left < mine.right && mine.left < r.right && r.top < mine.bottom && mine.top < r.bottom);
        if overlaps && unsafe { IsWindowVisible(above) }.as_bool() {
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
            let result = unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags) }.map_err(|e| win_error(&e));
            return Raise::Raised { above: names.describe(above), result };
        }
        above = unsafe { GetWindow(above, GW_HWNDPREV) }.unwrap_or_default();
    }
    Raise::Clear
}

/// The stats are for the game only: true while the foreground window belongs
/// to the target game's process.
pub fn game_in_front(target_pid: Option<u32>) -> bool {
    let Some(pid) = target_pid else { return false };
    let mut owner = 0u32;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut owner)) };
    owner == pid
}

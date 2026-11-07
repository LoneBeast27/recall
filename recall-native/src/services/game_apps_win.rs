//! The running apps Settings → Games → "Add app…" offers: one per application window
//! (`game_apps::is_app_window`: borderless popup games too), in z-order. The app labelled
//! "in front" is the one whose window was the foreground when the menu opened
//! ([`foreground_pid`]), never simply the first window found. Blocks for a few ms: call
//! [`running_apps_blocking`] from a background task.
use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::BOOL;

use crate::services::audio_apps;
use crate::services::game_apps::{self, Pickable, WinInfo};

/// What `is_app_window` needs to know about `hwnd`.
fn window_info(hwnd: HWND) -> WinInfo {
    let mut rect = RECT::default();
    // SAFETY: plain queries on a window handle EnumWindows passed in.
    unsafe {
        let sized = GetWindowRect(hwnd, &mut rect).is_ok();
        WinInfo {
            visible: IsWindowVisible(hwnd).as_bool(),
            owned: GetWindow(hwnd, GW_OWNER).is_ok(),
            exstyle: GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32,
            size: if sized { (rect.right - rect.left, rect.bottom - rect.top) } else { (0, 0) },
        }
    }
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0;
    // SAFETY: plain query; a stale handle leaves `pid` at 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// Application windows' pids, in z-order (front first).
fn window_pids() -> Vec<u32> {
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let pids = unsafe { &mut *(data.0 as *mut Vec<u32>) };
        if game_apps::is_app_window(&window_info(hwnd)) {
            let pid = pid_of(hwnd);
            if pid != 0 && !pids.contains(&pid) {
                pids.push(pid);
            }
        }
        BOOL(1)
    }
    let mut pids: Vec<u32> = Vec::new();
    // SAFETY: `pids` outlives the call; the callback only pushes to it.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut pids as *mut Vec<u32> as isize)) };
    pids
}

/// The process of the foreground window now (`None` when there is none). Read when "Add
/// app…" opens; Recall's own window then is in front, which labels no app.
pub fn foreground_pid() -> Option<u32> {
    // SAFETY: plain query.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }
    Some(pid_of(hwnd)).filter(|&pid| pid != 0)
}

/// The apps to offer, without Recall and the shell (`game_apps::pickable`); `front_pid` =
/// [`foreground_pid`] when the menu opened.
pub fn running_apps_blocking(front_pid: Option<u32>) -> Pickable {
    let own = std::process::id();
    let found = window_pids()
        .into_iter()
        .filter(|&pid| pid != own)
        .filter_map(|pid| audio_apps::image_path(pid).map(|path| (pid, audio_apps::app_from_path(Path::new(&path)))))
        .collect();
    game_apps::pickable(found, front_pid)
}

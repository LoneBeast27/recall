//! The running apps Settings → Games → "Add app…" offers: one per visible application
//! window, front-most first (`EnumWindows` walks the z-order), so the app that was in front
//! before Recall comes first. Blocks for a few ms: call from a background task.
use std::path::Path;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
    WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

use crate::services::audio_apps::{self, PlayingApp};

/// Visible, titled, unowned application windows' pids, in z-order (front first).
fn window_pids() -> Vec<u32> {
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let pids = unsafe { &mut *(data.0 as *mut Vec<u32>) };
        // SAFETY: plain queries on a window handle EnumWindows passed in.
        let (visible, owned, titled) = unsafe { (IsWindowVisible(hwnd).as_bool(), GetWindow(hwnd, GW_OWNER).is_ok(), GetWindowTextLengthW(hwnd) > 0) };
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
        let extended = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let app = extended & WS_EX_TOOLWINDOW.0 == 0 && (extended & WS_EX_APPWINDOW.0 != 0 || style & WS_CAPTION.0 != 0);
        if visible && !owned && titled && app {
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
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

/// The apps to offer, front-most first, without Recall and the shell (`game_apps::pickable`).
pub fn running_apps_blocking() -> Vec<PlayingApp> {
    let own = std::process::id();
    let found = window_pids()
        .into_iter()
        .filter(|&pid| pid != own)
        .filter_map(audio_apps::image_path)
        .map(|path| audio_apps::app_from_path(Path::new(&path)))
        .collect();
    crate::services::game_apps::pickable(found)
}

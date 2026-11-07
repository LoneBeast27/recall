//! Which process a foreground window belongs to, beyond the plain
//! `GetWindowThreadProcessId` read (`watcher::build_event`). The decisions
//! are pure helpers; only [`hosted_identity`] and [`core_window_pid`] call Win32.
//!
//! - **UWP frames** (G6): a CoreWindow (UWP) app's top-level window is an
//!   `ApplicationFrameWindow` owned by `ApplicationFrameHost.exe`, even in
//!   fullscreen; the app's own `Windows.UI.Core.CoreWindow` is a child of it.
//!   The hosted window's process is the identity (exe, pid); the frame `hwnd`
//!   stays the window that is captured and measured.
//! - **Long paths** (G15): handled by the shared `image_path::exe_path`
//!   (one retry at 32 768 chars), which `watcher::exe_path` calls.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId};
use crate::game_library::Library;

/// The exe stem of the UWP frame host.
const FRAME_HOST_STEM: &str = "ApplicationFrameHost";

/// `stem` is the UWP frame host, whose hosted CoreWindow names the app.
pub(crate) fn is_frame_host(stem: &str) -> bool {
    stem.eq_ignore_ascii_case(FRAME_HOST_STEM)
}

/// The identity to use for a window owned by `frame` (pid, identity) with
/// exe stem `frame_stem`: the hosted CoreWindow's process when the frame is
/// the UWP frame host and a hosted window was found, else the frame's own.
pub(crate) fn effective_identity<T>(frame_stem: &str, frame: (u32, T), hosted: Option<(u32, T)>) -> (u32, T) {
    match hosted {
        Some(h) if is_frame_host(frame_stem) => h,
        _ => frame,
    }
}

/// The identity of a window owned by `pid`: for a UWP frame
/// (`ApplicationFrameHost`) the process of its hosted CoreWindow, so a Store
/// game is judged by its own exe; the frame `hwnd` stays the window (G6).
pub(super) fn hosted_identity(hwnd: HWND, pid: u32, frame: (String, String, Library)) -> (u32, (String, String, Library)) {
    if !is_frame_host(&frame.0) {
        return (pid, frame);
    }
    let hosted = core_window_pid(hwnd)
        .filter(|p| *p != pid && *p != std::process::id())
        .and_then(|p| super::exe_identity(p).map(|id| (p, id)));
    let stem = frame.0.clone();
    effective_identity(&stem, (pid, frame), hosted)
}

/// The process of the `Windows.UI.Core.CoreWindow` child of a UWP frame.
fn core_window_pid(frame: HWND) -> Option<u32> {
    // SAFETY: FindWindowExW tolerates a stale parent (it fails).
    let child = unsafe { FindWindowExW(Some(frame), None, w!("Windows.UI.Core.CoreWindow"), PCWSTR::null()) }.ok()?;
    let mut pid = 0u32;
    // SAFETY: GetWindowThreadProcessId tolerates a stale handle (pid stays 0).
    unsafe { GetWindowThreadProcessId(child, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uwp_frame_takes_the_hosted_windows_process() {
        let (pid, stem) = effective_identity("ApplicationFrameHost", (3, "ApplicationFrameHost"), Some((7, "Windows10Universal")));
        assert_eq!((pid, stem), (7, "Windows10Universal"));
        let (pid, _) = effective_identity("applicationframehost", (3, "x"), Some((7, "y")));
        assert_eq!(pid, 7, "case-insensitive");
    }

    #[test]
    fn a_frame_with_no_hosted_window_or_another_exe_keeps_its_own() {
        assert_eq!(effective_identity("ApplicationFrameHost", (3, "afh"), None), (3, "afh"), "suspended / settings frame");
        assert_eq!(effective_identity("Game", (3, "game"), Some((7, "other"))), (3, "game"), "only the frame host is resolved");
    }
}

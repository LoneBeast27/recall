//! Game capture through monitor Desktop Duplication: a game window that covers its whole
//! monitor (borderless or exclusive fullscreen) shows exactly what that monitor shows, so it
//! can be recorded with monitor DDA (no WGC border) instead of WGC window capture. Behind
//! `RECALL_CAPTURE=dda` like the rest of DDA. While following it, `snapshot` looks at the
//! window and what sits over it; `dda_logic::Follower` decides.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONULL};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindow, GetWindowDisplayAffinity, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindow, IsWindowVisible, GWL_EXSTYLE, GWL_STYLE, GW_HWNDPREV, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT,
};

use super::dda_logic::{covers, frameless, Above, Bounds, WindowState};

/// At most this many windows above the game are looked at (a z-order walk of a busy desktop
/// stays well under it).
const MAX_ABOVE: usize = 512;

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
        let rect = rect_of(window)?;
        covers(rect, monitor_bounds(monitor)?).then_some(monitor)
    }
}

/// The followed window `hwnd` and the windows above it, for the capture of `captured` (an
/// `HMONITOR` value). The z-order walk runs only while the game is not in front.
pub(super) fn snapshot(hwnd: usize, captured: usize) -> WindowState {
    let window = HWND(hwnd as *mut _);
    let mut state = WindowState::default();
    unsafe {
        if !IsWindow(Some(window)).as_bool() {
            return state;
        }
        state.exists = true;
        if IsIconic(window).as_bool() {
            state.iconic = true;
            return state;
        }
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONULL);
        state.monitor = monitor.0 as usize;
        state.covers = covering_monitor(hwnd).is_some_and(|m| m == monitor);
        state.monitor_rect = monitor_bounds(HMONITOR(captured as *mut _)).unwrap_or_default();
        let game = pid_of(window);
        state.foreground_is_game = game != 0 && pid_of(GetForegroundWindow()) == game;
        if !state.foreground_is_game {
            state.above = windows_above(window, game);
        }
    }
    state
}

/// Visible top-level windows above `window` in z-order, nearest first.
fn windows_above(window: HWND, game: u32) -> Vec<Above> {
    let own = unsafe { GetCurrentProcessId() };
    let mut found = Vec::new();
    let mut next = unsafe { GetWindow(window, GW_HWNDPREV) }.unwrap_or_default();
    while !next.is_invalid() && found.len() < MAX_ABOVE {
        if unsafe { IsWindowVisible(next) }.as_bool() {
            if let Some(rect) = rect_of(next) {
                found.push(describe(next, rect, game, own));
            }
        }
        next = unsafe { GetWindow(next, GW_HWNDPREV) }.unwrap_or_default();
    }
    found
}

fn describe(hwnd: HWND, rect: Bounds, game: u32, own: u32) -> Above {
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let mut cloaked = 0u32;
    let cloaked = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&raw mut cloaked).cast(), 4) }.is_ok() && cloaked != 0;
    // Any display affinity keeps the window's pixels out of a capture
    // (WDA_EXCLUDEFROMCAPTURE: Recall's island and editor guide; WDA_MONITOR: black).
    let mut affinity = 0u32;
    let shielded = unsafe { GetWindowDisplayAffinity(hwnd, &mut affinity) }.is_ok() && affinity != 0;
    let pid = pid_of(hwnd);
    Above {
        rect,
        cloaked,
        tool: ex & WS_EX_TOOLWINDOW.0 != 0,
        click_through: ex & WS_EX_TRANSPARENT.0 != 0 && ex & WS_EX_LAYERED.0 != 0,
        excluded: shielded || pid == own,
        game: pid == game,
    }
}

fn pid_of(hwnd: HWND) -> u32 {
    if hwnd.is_invalid() {
        return 0;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn rect_of(hwnd: HWND) -> Option<Bounds> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.ok()?;
    Some(bounds(r))
}

fn monitor_bounds(monitor: HMONITOR) -> Option<Bounds> {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool().then(|| bounds(info.rcMonitor))
}

fn bounds(r: RECT) -> Bounds {
    Bounds { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

//! DWM "Not Responding" ghost windows (third audit U3).
//!
//! When a window stops pumping messages for ~5 s and the user clicks it, Windows
//! (likely: on 10/11 the ghost is owned by `dwm.exe`) puts a ghost window of class
//! `Ghost` at the same rect and makes it the foreground. Left alone, the watcher
//! reports it as a fullscreen `dwm` window and the tracker retargets to it after
//! the 2 s debounce: engine restart, replay ring lost at the moment the game froze,
//! and a second restart when the game recovers.
//!
//! The watcher maps a ghost to the hung window it stands in for, so the tracker
//! sees the session's own window and pid. If the hung window can't be found the
//! ghost is reported as itself, and `dwm` is on the target deny list.

use std::sync::OnceLock;

use windows::core::{s, w};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;

/// Window class of a DWM ghost window.
pub const GHOST_CLASS: &str = "Ghost";

/// The window to classify for `hwnd`: its hung window when `hwnd` is a ghost
/// (class `Ghost` and `hung_of` finds one), else `hwnd` itself. `class_of` and
/// `hung_of` are the OS lookups, injected so tests need no real hang.
pub fn resolve_with(
    hwnd: isize,
    class_of: impl Fn(isize) -> Option<String>,
    hung_of: impl Fn(isize) -> isize,
) -> isize {
    let ghost = class_of(hwnd).is_some_and(|c| c.eq_ignore_ascii_case(GHOST_CLASS));
    if !ghost {
        return hwnd;
    }
    match hung_of(hwnd) {
        0 => hwnd,
        hung => hung,
    }
}

/// Live [`resolve_with`]: `GetClassNameW` + user32's `HungWindowFromGhostWindow`.
pub fn resolve(hwnd: HWND) -> HWND {
    HWND(resolve_with(hwnd.0 as isize, class_name, hung_window) as *mut _)
}

fn class_name(hwnd: isize) -> Option<String> {
    let mut buf = [0u16; 64];
    // SAFETY: plain query on a window handle; a dead handle returns 0.
    let len = unsafe { GetClassNameW(HWND(hwnd as *mut _), &mut buf) };
    (len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

type HungFromGhost = unsafe extern "system" fn(HWND) -> HWND;

/// `HungWindowFromGhostWindow` (user32, exported since Vista but in no SDK
/// header, so not in the `windows` crate). Looked up once; absent = no mapping.
fn hung_window(ghost: isize) -> isize {
    static FUNC: OnceLock<Option<HungFromGhost>> = OnceLock::new();
    let func = FUNC.get_or_init(|| {
        // SAFETY: user32 is loaded in every GUI process; the name is NUL-terminated.
        let module = unsafe { GetModuleHandleW(w!("user32.dll")) }.ok()?;
        let proc = unsafe { GetProcAddress(module, s!("HungWindowFromGhostWindow")) }?;
        // SAFETY: the export's signature is `HWND HungWindowFromGhostWindow(HWND)`.
        Some(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, HungFromGhost>(proc) })
    });
    match func {
        // SAFETY: a plain query on a window handle; returns null for a non-ghost.
        Some(f) => unsafe { f(HWND(ghost as *mut _)) }.0 as isize,
        None => 0,
    }
}

#[cfg(test)]
#[path = "ghost_window_tests.rs"]
mod tests;

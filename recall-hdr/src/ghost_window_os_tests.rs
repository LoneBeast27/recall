//! The live ghost lookups against this Windows (no hang, no visible window): the
//! real `resolve`, `GetClassNameW`, the owner lookup and the
//! `HungWindowFromGhostWindow` export. `ghost_window_tests.rs` only drives
//! `resolve_with` with injected lookups.

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetDesktopWindow, RegisterClassW, HWND_MESSAGE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
};

use super::*;

#[test]
fn the_hung_window_export_is_found_in_user32() {
    let Some(f) = hung_from_ghost_export() else {
        println!("SKIP: user32 has no HungWindowFromGhostWindow on this Windows");
        return;
    };
    // SAFETY: a plain query; the desktop is no ghost, so no hung window.
    let hung = unsafe { f(GetDesktopWindow()) };
    assert!(hung.0.is_null(), "the desktop has no hung window");
    assert_eq!(hung_window(unsafe { GetDesktopWindow() }.0 as isize), 0);
}

#[test]
fn resolve_keeps_the_desktop_window() {
    // SAFETY: plain query.
    let desktop = unsafe { GetDesktopWindow() };
    assert_eq!(class_name(desktop.0 as isize).as_deref(), Some("#32769"), "real class read");
    assert_eq!(resolve(desktop), desktop);
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: default handling for our own test window.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// A message-only window (never visible) of class `Ghost`, owned by this test
/// process; destroyed on drop.
struct FakeGhost(HWND);

impl FakeGhost {
    fn new() -> Self {
        // SAFETY: module handle of this exe; the class is local to this process
        // and registered once (a second register fails harmlessly).
        unsafe {
            let instance = GetModuleHandleW(None).expect("module handle");
            let class = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance.into(),
                lpszClassName: w!("Ghost"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("Ghost"),
                w!("recall-ghost-test"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )
            .expect("message-only window");
            FakeGhost(hwnd)
        }
    }
}

impl Drop for FakeGhost {
    fn drop(&mut self) {
        // SAFETY: our own window, destroyed on the thread that made it.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

#[test]
fn a_ghost_class_window_of_this_process_resolves_to_itself() {
    let ghost = FakeGhost::new();
    let h = ghost.0 .0 as isize;
    assert_eq!(class_name(h).as_deref(), Some(GHOST_CLASS));
    let own = crate::watcher::exe_stem(std::process::id()).expect("own stem");
    assert_eq!(owner_stem(h).as_deref(), Some(own.as_str()), "real owner lookup");
    assert!(!own.eq_ignore_ascii_case(GHOST_OWNER_STEM));
    assert_eq!(resolve(ghost.0), ghost.0);
}

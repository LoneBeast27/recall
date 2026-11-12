//! The island ends with the process that started it. A thread-pool wait on
//! the parent's handle posts `WM_CLOSE` when it exits, instead of the UI
//! asking `WaitForSingleObject(parent, 0)` on every tick.

use std::ffi::c_void;

use windows::Win32::Foundation::{HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, WPARAM};
use windows::Win32::System::Threading::{RegisterWaitForSingleObject, UnregisterWaitEx, INFINITE, WT_EXECUTEONLYONCE};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

/// A registered wait; dropping it unregisters (and waits out a running callback),
/// so the parent handle may be closed after.
#[derive(Debug)]
pub struct ParentWait {
    wait: HANDLE,
}

impl ParentWait {
    /// Posts `WM_CLOSE` to `hwnd` once `parent` is signaled (at once when it
    /// already is). None when the wait could not be registered.
    pub fn register(parent: HANDLE, hwnd: HWND) -> Option<Self> {
        let mut wait = HANDLE::default();
        let context = hwnd.0 as *const c_void;
        let ok = unsafe { RegisterWaitForSingleObject(&mut wait, parent, Some(on_exit), Some(context), INFINITE, WT_EXECUTEONLYONCE) };
        ok.ok().map(|()| Self { wait })
    }
}

unsafe extern "system" fn on_exit(context: *mut c_void, _timed_out: bool) {
    unsafe {
        let _ = PostMessageW(Some(HWND(context)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

impl Drop for ParentWait {
    fn drop(&mut self) {
        // INVALID_HANDLE_VALUE: block until a callback in flight has returned.
        unsafe {
            let _ = UnregisterWaitEx(self.wait, Some(INVALID_HANDLE_VALUE));
        }
    }
}

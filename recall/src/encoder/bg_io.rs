//! Background priority for the `recall-mover` thread: below-normal CPU, then
//! background mode (very low I/O and memory priority). Same Win32 approach as
//! recall-native's `services/storage/io_priority.rs`, copied rather than
//! imported so the engine stays independent of the app crate. Measured there
//! on this machine (Windows 11 26300, 2026-10-22):
//! `SetThreadPriority(THREAD_MODE_BACKGROUND_BEGIN)` moves the calling
//! thread's I/O priority from 2 (normal) to 0 (very low), read back with
//! `NtQueryInformationThread(ThreadIoPriority)`. Only the calling thread is
//! affected; the mover never starts a child process.

use std::ffi::c_void;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    THREAD_PRIORITY_BELOW_NORMAL,
};

/// `THREADINFOCLASS::ThreadIoPriority`.
const THREAD_IO_PRIORITY: u32 = 22;

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn NtQueryInformationThread(thread: HANDLE, class: u32, info: *mut c_void, length: u32, returned: *mut u32) -> i32;
}

/// Lower the calling thread: below-normal CPU priority, then background mode.
pub(crate) fn enter_background() -> Result<(), String> {
    // SAFETY: GetCurrentThread returns this thread's pseudo-handle; it needs no
    // closing and both calls apply to this thread only.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) }
        .map_err(|e| format!("below-normal priority: {e}"))?;
    // SAFETY: as above; background mode may only be entered by the thread itself.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) }
        .map_err(|e| format!("background mode: {e}"))
}

/// I/O priority of the calling thread (0 very low, 1 low, 2 normal, 3 high).
pub(crate) fn thread_io_priority() -> Option<u32> {
    let mut value = 0u32;
    // SAFETY: a 4-byte out buffer for ThreadIoPriority on this thread's pseudo-handle.
    let status = unsafe {
        NtQueryInformationThread(GetCurrentThread(), THREAD_IO_PRIORITY, (&mut value as *mut u32).cast(), 4, std::ptr::null_mut())
    };
    (status >= 0).then_some(value)
}

/// CPU priority level of the calling thread (`THREAD_PRIORITY_*`, -1 = below normal).
pub(crate) fn thread_priority() -> i32 {
    // SAFETY: reads this thread's own priority through its pseudo-handle.
    unsafe { GetThreadPriority(GetCurrentThread()) }
}

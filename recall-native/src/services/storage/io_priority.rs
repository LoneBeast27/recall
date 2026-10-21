//! Background I/O priority for lane threads and the child processes they start.
//!
//! Measured on this machine (Windows 11 26300, 2026-10-22):
//! * `SetThreadPriority(THREAD_MODE_BACKGROUND_BEGIN)` moves the calling thread's I/O
//!   priority from 2 (Normal) to 0 (Very low); `NtQueryInformationThread(ThreadIoPriority)`
//!   reads it back.
//! * A child process does NOT inherit it (a child started from a background thread reports
//!   process I/O priority 2), and BELOW_NORMAL / IDLE priority classes leave it at 2.
//! * `NtSetInformationProcess(ProcessIoPriority, 0)` on the child lowers the process and
//!   every one of its existing threads to 0 without elevation.
//!
//! So lane threads enter background mode once, and children that read an HDD (ffmpeg,
//! the engine) are created suspended, lowered, then resumed — before their first read.
use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Output};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};

pub(super) const IO_VERY_LOW: u32 = 0;
const PROCESS_IO_PRIORITY: u32 = 33;
const THREAD_IO_PRIORITY: u32 = 22;
/// CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS (as every editor helper process).
pub(super) const HIDDEN_LOW_PRIORITY: u32 = 0x0800_4000;
const CREATE_SUSPENDED: u32 = 0x0000_0004;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn NtQueryInformationProcess(process: HANDLE, class: u32, info: *mut c_void, length: u32, returned: *mut u32) -> i32;
    fn NtSetInformationProcess(process: HANDLE, class: u32, info: *const c_void, length: u32) -> i32;
    fn NtQueryInformationThread(thread: HANDLE, class: u32, info: *mut c_void, length: u32, returned: *mut u32) -> i32;
    fn NtResumeProcess(process: HANDLE) -> i32;
}

/// Puts the calling thread in background mode (very low I/O and memory priority).
pub(super) fn enter_background_mode() -> bool {
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) }.is_ok()
}

/// I/O priority of the calling thread (0 very low, 1 low, 2 normal, 3 high).
pub(super) fn current_thread_io_priority() -> Option<u32> {
    let mut value = 0u32;
    let status = unsafe { NtQueryInformationThread(GetCurrentThread(), THREAD_IO_PRIORITY, (&mut value as *mut u32).cast(), 4, std::ptr::null_mut()) };
    (status >= 0).then_some(value)
}

pub(super) fn process_io_priority(child: &Child) -> Option<u32> {
    let mut value = 0u32;
    let status = unsafe { NtQueryInformationProcess(handle(child), PROCESS_IO_PRIORITY, (&mut value as *mut u32).cast(), 4, std::ptr::null_mut()) };
    (status >= 0).then_some(value)
}

fn handle(child: &Child) -> HANDLE {
    HANDLE(child.as_raw_handle())
}

/// Starts `command` hidden at below-normal CPU priority; with `lowered`, the process is
/// created suspended, set to very low I/O priority, then resumed.
pub(super) fn spawn(command: &mut Command, lowered: bool) -> io::Result<Child> {
    if !lowered {
        return command.creation_flags(HIDDEN_LOW_PRIORITY).spawn();
    }
    let mut child = command.creation_flags(HIDDEN_LOW_PRIORITY | CREATE_SUSPENDED).spawn()?;
    let value = IO_VERY_LOW;
    unsafe {
        let _ = NtSetInformationProcess(handle(&child), PROCESS_IO_PRIORITY, (&value as *const u32).cast(), 4);
        if NtResumeProcess(handle(&child)) < 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("could not resume the helper process"));
        }
    }
    Ok(child)
}

/// [`spawn`] then collect stdout/stderr and the exit status.
pub(super) fn output(command: &mut Command, lowered: bool) -> io::Result<Output> {
    spawn(command, lowered)?.wait_with_output()
}

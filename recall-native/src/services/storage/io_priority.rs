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
use std::sync::{OnceLock, RwLock};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
use windows::core::PCWSTR;

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

/// The I/O priority of this process (what an unlowered child inherits).
#[cfg(test)]
pub(super) fn current_process_io_priority() -> Option<u32> {
    let mut value = 0u32;
    let process = unsafe { windows::Win32::System::Threading::GetCurrentProcess() };
    let status = unsafe { NtQueryInformationProcess(process, PROCESS_IO_PRIORITY, (&mut value as *mut u32).cast(), 4, std::ptr::null_mut()) };
    (status >= 0).then_some(value)
}

fn handle(child: &Child) -> HANDLE {
    HANDLE(child.as_raw_handle())
}

/// Helpers end with the app, however it ends: an engine that never returns must not outlive it.
/// Windows closes the job's handle with the process, and that ends what is in the job.
fn helpers_job() -> Option<HANDLE> {
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), size).ok()?;
        Some(job.0 as isize)
    });
    job.map(|raw| HANDLE(raw as *mut c_void))
}

/// True once the app is closing. Held for reading while a helper starts: a process left between
/// created and resumed by the app's exit stays suspended for good, in no job (it held a check's
/// output pipe for 46 minutes).
static CLOSING: RwLock<bool> = RwLock::new(false);

/// No helper starts from here on, and none is half started on return.
pub fn close_helpers() {
    *CLOSING.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
}

/// Starts `command` hidden at below-normal CPU priority, in the helpers' job; with `lowered`,
/// the process is created suspended, set to very low I/O priority, then resumed.
pub(super) fn spawn(command: &mut Command, lowered: bool) -> io::Result<Child> {
    start(&CLOSING, command, lowered, false)
}

/// [`spawn`] for long background encodes: very low I/O priority, below-normal CPU priority,
/// and the upper half of the CPUs (CCD1 on this desktop), all set before the process runs.
pub(super) fn spawn_pinned(command: &mut Command) -> io::Result<Child> {
    start(&CLOSING, command, true, true)
}

/// The upper half of the set bits in `mask`; `None` below 8 CPUs.
pub(super) fn upper_half(mask: usize) -> Option<usize> {
    let count = mask.count_ones();
    if count < 8 {
        return None;
    }
    let mut keep = count / 2;
    let mut upper = 0usize;
    for bit in (0..usize::BITS).rev() {
        if keep == 0 {
            break;
        }
        if mask & (1 << bit) != 0 {
            upper |= 1 << bit;
            keep -= 1;
        }
    }
    Some(upper)
}

fn start(closing: &RwLock<bool>, command: &mut Command, lowered: bool, pinned: bool) -> io::Result<Child> {
    let closing = closing.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    if *closing {
        return Err(io::Error::other("Recall is closing"));
    }
    let job = helpers_job();
    let mut child = command.creation_flags(HIDDEN_LOW_PRIORITY | if lowered { CREATE_SUSPENDED } else { 0 }).spawn()?;
    if let Some(job) = job {
        let _ = unsafe { AssignProcessToJobObject(job, handle(&child)) };
    }
    if !lowered {
        return Ok(child);
    }
    let value = IO_VERY_LOW;
    if pinned {
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessAffinityMask, SetProcessAffinityMask};
        let (mut process, mut system) = (0usize, 0usize);
        if unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) }.is_ok() {
            if let Some(mask) = upper_half(process) {
                let _ = unsafe { SetProcessAffinityMask(handle(&child), mask) };
            }
        }
        eco_qos(handle(&child));
    }
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

/// EcoQoS on a helper process: Windows runs it on the slowest cores at low clocks (E-cores on a
/// hybrid Intel CPU), away from a game. Set while the process is still suspended.
fn eco_qos(process: windows::Win32::Foundation::HANDLE) -> bool {
    use windows::Win32::System::Threading::{
        ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    // SAFETY: a correctly sized PROCESS_POWER_THROTTLING_STATE for a live child handle.
    unsafe { SetProcessInformation(process, ProcessPowerThrottling, (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(), std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32) }.is_ok()
}

/// [`spawn`] then collect stdout/stderr and the exit status.
pub(super) fn output(command: &mut Command, lowered: bool) -> io::Result<Output> {
    spawn(command, lowered)?.wait_with_output()
}

#[cfg(test)]
mod closing_tests {
    use super::*;

    #[test]
    fn a_closing_app_starts_no_helper() {
        let closing = RwLock::new(false);
        let mut command = Command::new("cmd.exe");
        command.args(["/c", "exit 0"]);
        assert!(start(&closing, &mut command, false, false).unwrap().wait().unwrap().success());
        *closing.write().unwrap() = true;
        assert!(start(&closing, &mut command, false, false).is_err());
        assert!(start(&closing, &mut command, true, false).is_err());
    }
}

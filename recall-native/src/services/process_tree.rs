//! Running processes for the Audio page's app meters: the root of an exe's largest process
//! tree (the recorder captures that tree for an app source), and a handle that says when a
//! process exits. Called on meter threads only.
use std::collections::HashMap;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

/// (pid, parent pid, image name) of every running process.
fn snapshot() -> Vec<(u32, u32, String)> {
    // SAFETY: the snapshot handle is checked, read with a sized entry and closed once.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut out = Vec::new();
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|unit| *unit == 0).unwrap_or(entry.szExeFile.len());
            out.push((entry.th32ProcessID, entry.th32ParentProcessID, String::from_utf16_lossy(&entry.szExeFile[..len])));
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        out
    }
}

/// Root pid of the largest tree of processes named `exe` (case-insensitive, `.exe` optional).
pub fn tree_root(exe: &str) -> Option<u32> {
    root_in(&snapshot(), exe)
}

/// `tree_root` over a given process list (tests). Ties go to the lowest pid.
pub fn root_in(procs: &[(u32, u32, String)], exe: &str) -> Option<u32> {
    let want = exe.trim().to_lowercase();
    let want = if want.ends_with(".exe") { want } else { format!("{want}.exe") };
    let parents: HashMap<u32, u32> = procs.iter().filter(|p| p.2.to_lowercase() == want).map(|p| (p.0, p.1)).collect();
    let root_of = |mut pid: u32| {
        for _ in 0..parents.len() {
            match parents.get(&pid) {
                Some(&parent) if parent != pid && parents.contains_key(&parent) => pid = parent,
                _ => break,
            }
        }
        pid
    };
    let mut sizes: HashMap<u32, usize> = HashMap::new();
    for &pid in parents.keys() {
        *sizes.entry(root_of(pid)).or_default() += 1;
    }
    sizes.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|(pid, _)| pid)
}

/// A process handle that can only wait.
pub struct Waitable(HANDLE);

impl Waitable {
    pub fn open(pid: u32) -> Option<Self> {
        // SAFETY: plain Win32 call; failure returns an error, not a handle.
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.ok().map(Self)
    }

    pub fn exited(&self) -> bool {
        // SAFETY: the handle is open for the life of `self`.
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for Waitable {
    fn drop(&mut self) {
        // SAFETY: closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: a process handle may be waited on from any thread.
unsafe impl Send for Waitable {}

/// `pid` still runs. One this user may not open (access denied) counts as running.
pub fn alive(pid: u32) -> bool {
    match Waitable::open(pid) {
        Some(process) => !process.exited(),
        // SAFETY: reads this thread's last error, set by the failed OpenProcess just now.
        None => (unsafe { windows::Win32::Foundation::GetLastError() }) == windows::Win32::Foundation::ERROR_ACCESS_DENIED,
    }
}

/// What can be said about a pid: it runs no process, it runs one (start time and image path),
/// or it can't be told (access denied, a failed query).
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessState {
    Gone,
    Running { started: std::time::SystemTime, image: String },
    Unknown,
}

/// [`ProcessState`] of `pid`, from one query handle.
pub fn state(pid: u32) -> ProcessState {
    use windows::Win32::Foundation::ERROR_INVALID_PARAMETER;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: plain Win32 call; the handle is closed below.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => {
            let state = state_of(handle);
            // SAFETY: closed exactly once.
            let _ = unsafe { CloseHandle(handle) };
            state
        }
        // No such process: the pid runs nothing. Anything else (access denied) can't be told.
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => ProcessState::Gone,
        Err(_) => ProcessState::Unknown,
    }
}

fn state_of(handle: HANDLE) -> ProcessState {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetExitCodeProcess, GetProcessTimes, PROCESS_NAME_WIN32, QueryFullProcessImageNameW};
    const STILL_ACTIVE: u32 = 259;
    let mut code = 0u32;
    // SAFETY: `handle` is open for query; every out-parameter is sized.
    unsafe {
        if GetExitCodeProcess(handle, &mut code).is_err() {
            return ProcessState::Unknown;
        }
        if code != STILL_ACTIVE {
            return ProcessState::Gone;
        }
        let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).is_err() {
            return ProcessState::Unknown;
        }
        let mut wide = vec![0u16; 32_768];
        let mut len = wide.len() as u32;
        if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(wide.as_mut_ptr()), &mut len).is_err() {
            return ProcessState::Unknown;
        }
        let ticks = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        // FILETIME counts 100 ns from 1601; the Unix epoch is 11 644 473 600 s later.
        let Some(unix) = ticks.checked_sub(116_444_736_000_000_000) else { return ProcessState::Unknown };
        let started = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(unix.saturating_mul(100));
        ProcessState::Running { started, image: String::from_utf16_lossy(&wide[..len as usize]) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_largest_tree_wins_and_the_extension_is_optional() {
        let procs: Vec<(u32, u32, String)> = [(100, 4, "player.exe"), (110, 100, "player.exe"), (120, 100, "Player.EXE"), (300, 4, "player.exe"), (400, 4, "other.exe")]
            .into_iter()
            .map(|(pid, parent, exe)| (pid, parent, exe.to_owned()))
            .collect();
        assert_eq!(root_in(&procs, "player"), Some(100));
        assert_eq!(root_in(&procs, " PLAYER.exe "), Some(100));
        assert_eq!(root_in(&procs, "missing.exe"), None);
    }

    #[test]
    fn this_process_runs_and_a_pid_nobody_holds_is_gone() {
        match state(std::process::id()) {
            ProcessState::Running { started, image } => {
                assert!(started <= std::time::SystemTime::now() && started > std::time::UNIX_EPOCH);
                assert!(image.to_ascii_lowercase().ends_with(".exe"), "{image}");
            }
            other => panic!("this test process: {other:?}"),
        }
        // Pids are multiples of 4 on Windows: an odd one is never a process.
        assert_eq!(state(4_000_000_001), ProcessState::Gone);
    }
}

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
}

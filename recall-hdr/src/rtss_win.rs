//! The real [`RtssHost`]: `%ProgramFiles%\Recall\rtss\RTSS.exe` (the only
//! copy that counts as Recall's; admin-only writable, see
//! `scripts/install-rtss.ps1`), `schtasks.exe` (hidden, no window), a process
//! scan for RTSS.exe with each one's image path, and the `RTSSSharedMemoryV2`
//! signature. Read-only except for running the two Recall tasks.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::rtss_ctl::{RtssHost, Scan};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// `dwSignature` of a live RTSS shared memory ('RTSS'; 0xDEAD while it is
/// deallocated, per `RTSSSharedMemory.h`).
const RTSS_SIGNATURE: u32 = 0x5254_5353;

pub struct SystemRtss {
    /// `%ProgramFiles%\Recall\rtss` (None when Program Files is unknown).
    dir: Option<PathBuf>,
}

impl SystemRtss {
    pub fn from_environment() -> Self {
        SystemRtss { dir: recall_rtss_dir(|name| std::env::var_os(name)) }
    }
}

/// Where the installer puts Recall's RTSS: the 64-bit Program Files
/// (`ProgramW6432`, then `ProgramFiles`) + `Recall\rtss`.
pub fn recall_rtss_dir(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(|name| var(name))
        .map(PathBuf::from)
        .find(|p| p.is_absolute())
        .map(|pf| pf.join(r"Recall\rtss"))
}

impl RtssHost for SystemRtss {
    fn copy_present(&self) -> bool {
        self.dir.as_ref().is_some_and(|d| d.join("RTSS.exe").is_file())
    }

    fn task_exists(&mut self, name: &str) -> bool {
        schtasks(&["/query", "/tn", name]).is_ok()
    }

    fn scan(&self) -> Scan {
        let mut scan = Scan { shm: shared_memory_live(), ..Scan::default() };
        for pid in pids_named("RTSS.exe") {
            match image_path(pid) {
                Some(path) if self.dir.as_deref().is_some_and(|d| is_under(&path, d)) => scan.ours = true,
                Some(_) => scan.foreign = true,
                None => scan.unknown = true,
            }
        }
        scan
    }

    fn run_task(&mut self, name: &str) -> Result<(), String> {
        schtasks(&["/run", "/tn", name])
    }
}

/// `path` lies inside `dir` (case-insensitive, whole path segments).
pub fn is_under(path: &Path, dir: &Path) -> bool {
    let norm = |p: &Path| {
        let text = p.to_string_lossy().replace('/', "\\").to_lowercase();
        text.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(text).trim_end_matches('\\').to_owned()
    };
    let (path, dir) = (norm(path), norm(dir));
    !dir.is_empty() && path.len() > dir.len() && path.starts_with(&dir) && path.as_bytes()[dir.len()] == b'\\'
}

/// Run `schtasks.exe` from System32 with no window; Ok on exit code 0.
fn schtasks(args: &[&str]) -> Result<(), String> {
    let exe = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join(r"System32\schtasks.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("schtasks.exe"));
    let out = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("schtasks: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Pids of every process whose exe name is `name` (case-insensitive).
fn pids_named(name: &str) -> Vec<u32> {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return Vec::new() };
    let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut pids = Vec::new();
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let len = entry.szExeFile.iter().position(|u| *u == 0).unwrap_or(entry.szExeFile.len());
        if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(name) {
            pids.push(entry.th32ProcessID);
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    pids
}

/// Full image path of `pid` (limited-information access works on elevated
/// processes); `None` when it cannot be read.
fn image_path(pid: u32) -> Option<PathBuf> {
    let handle: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok();
    let _ = unsafe { CloseHandle(handle) };
    ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// `RTSSSharedMemoryV2` opens and its signature says RTSS is alive (a
/// reader's open handle keeps a dead RTSS's mapping around as 'DEAD').
fn shared_memory_live() -> bool {
    let Ok(mapping) = (unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, w!("RTSSSharedMemoryV2")) }) else {
        return false;
    };
    let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 4) };
    let live = !view.Value.is_null() && unsafe { std::ptr::read_unaligned(view.Value as *const u32) } == RTSS_SIGNATURE;
    if !view.Value.is_null() {
        let _ = unsafe { UnmapViewOfFile(view) };
    }
    let _ = unsafe { CloseHandle(mapping) };
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_matches_whole_segments_case_insensitively() {
        let dir = Path::new(r"C:\Users\me\AppData\Local\Programs\Recall\rtss");
        assert!(is_under(Path::new(r"c:\users\ME\appdata\local\programs\recall\RTSS\RTSS.exe"), dir));
        assert!(is_under(Path::new(r"\\?\C:\Users\me\AppData\Local\Programs\Recall\rtss\RTSS.exe"), dir));
        assert!(is_under(Path::new(r"C:\Users\me\AppData\Local\Programs\Recall\rtss\sub\x.exe"), Path::new(r"C:\Users\me\AppData\Local\Programs\Recall\rtss\")));
        assert!(!is_under(Path::new(r"C:\Program Files (x86)\RivaTuner Statistics Server\RTSS.exe"), dir));
        assert!(!is_under(Path::new(r"C:\Users\me\AppData\Local\Programs\Recall\rtss-old\RTSS.exe"), dir), "sibling prefix");
        assert!(!is_under(dir, dir), "the folder itself is not inside it");
        assert!(!is_under(Path::new(r"C:\x.exe"), Path::new("")));
    }

    #[test]
    fn recalls_rtss_is_in_the_64_bit_program_files_only() {
        use std::ffi::OsString;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| OsString::from(*v))
        };
        let both = env(&[("ProgramW6432", r"C:\Program Files"), ("ProgramFiles", r"C:\Program Files (x86)")]);
        assert_eq!(recall_rtss_dir(both), Some(PathBuf::from(r"C:\Program Files\Recall\rtss")), "64-bit Program Files wins");
        let plain = env(&[("ProgramFiles", r"D:\Apps")]);
        assert_eq!(recall_rtss_dir(plain), Some(PathBuf::from(r"D:\Apps\Recall\rtss")));
        assert_eq!(recall_rtss_dir(env(&[("ProgramFiles", "relative")])), None, "never a relative folder");
        assert_eq!(recall_rtss_dir(env(&[])), None);
        let dir = recall_rtss_dir(env(&[("ProgramW6432", r"C:\Program Files")])).unwrap();
        assert!(is_under(Path::new(r"C:\Program Files\Recall\rtss\RTSS.exe"), &dir));
        assert!(!is_under(Path::new(r"C:\Users\me\AppData\Local\Programs\Recall\rtss\RTSS.exe"), &dir), "a user-writable copy is not Recall's");
    }

    #[test]
    fn scan_without_a_copy_folder_finds_nothing_of_ours() {
        // Read-only: whatever RTSS runs on this machine is never "ours" here.
        let host = SystemRtss { dir: Some(std::env::temp_dir().join("recall-no-rtss-here")) };
        assert!(!host.copy_present());
        assert!(!host.scan().ours);
    }
}

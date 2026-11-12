//! Is a Java game running from a given folder? Used by the fixer's helper to refuse config
//! edits while the game could overwrite them on exit. Reads process names and command lines
//! only (query-limited access).

use std::path::Path;

/// Any java/javaw process whose command line names `instance` as its game folder.
pub fn java_running_in(instance: &Path) -> bool {
    let want = instance.to_string_lossy().to_lowercase();
    java_pids().into_iter().any(|pid| crate::watcher::command_line_of(pid).is_some_and(|c| crate::fix_cwb::instance_of(&c).is_some_and(|p| p.to_string_lossy().to_lowercase() == want)))
}

fn java_pids() -> Vec<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let mut out = Vec::new();
    // SAFETY: a process snapshot, read and closed here; the entry struct is sized for the API.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let end = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..end]);
            if name.eq_ignore_ascii_case("javaw.exe") || name.eq_ignore_ascii_case("java.exe") {
                out.push(e.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

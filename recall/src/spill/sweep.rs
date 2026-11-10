//! Start-up sweep for spill files left behind. Delete-on-close cleans up after a normal
//! exit or a crash, but not after a power cut or a BSOD, and each run names its file by
//! pid, so leftovers would pile up. Runs on its own low-priority thread at engine start,
//! before any spill, and removes only `recall-replay-<pid>.spill` files (Recall's own
//! temporary buffer, never a clip) whose pid is not a running recall.exe.

use std::path::{Path, PathBuf};

/// The pid in a spill file's name, when the name is one of ours.
pub(super) fn spill_pid(name: &str) -> Option<u32> {
    name.strip_prefix("recall-replay-")?.strip_suffix(".spill")?.parse().ok()
}

/// Delete the leftover spill files in `dir`. Returns (files, bytes) removed.
pub(super) fn sweep_dir(dir: &Path, alive: &dyn Fn(u32) -> bool) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let (mut files, mut bytes) = (0, 0);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(spill_pid) else { continue };
        if alive(pid) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

/// Every folder a spill file may be in (`drives::dir_on` for each fixed drive).
fn spill_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = super::drives::sys::fixed_drives().iter().map(super::drives::dir_on).collect();
    dirs.push(std::env::temp_dir().join("recall-spill"));
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Sweep every spill folder on a background thread; logs what it freed.
pub(super) fn spawn(log: std::sync::Arc<dyn Fn(&str) + Send + Sync>) {
    let _ = std::thread::Builder::new().name("recall-spill-sweep".into()).spawn(move || {
        #[cfg(feature = "encode")]
        let _ = crate::encoder::enter_background();
        let own = std::process::id();
        let alive = |pid: u32| pid == own || is_recall(pid);
        let (mut files, mut bytes) = (0, 0);
        for dir in spill_dirs() {
            let (f, b) = sweep_dir(&dir, &alive);
            files += f;
            bytes += b;
        }
        if files > 0 {
            log(&format!(
                "replay buffer: removed {files} leftover spill file(s) from an earlier run, {} MiB freed",
                bytes / (1024 * 1024)
            ));
        }
    });
}

/// True when `pid` is a running process named recall.exe.
#[cfg(windows)]
pub(crate) fn is_recall(pid: u32) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: query-only handle, closed below; the buffer outlives the call.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let named = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        if named.is_err() {
            // Running, but its name is hidden from us: keep its file to be safe.
            return true;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&path)
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case("recall"))
    }
}

#[cfg(not(windows))]
pub(crate) fn is_recall(_pid: u32) -> bool {
    false
}

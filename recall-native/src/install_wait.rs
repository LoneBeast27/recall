//! An install in progress: `scripts/install-swap.ps1` writes `.installing` beside the program
//! file, holding the installer's pid (`Set-Content -Value $PID -NoNewline`), for the whole file
//! swap. A start in that window waits while the marker exists and that pid still runs, polling
//! every 250 ms for at most 30 s, then starts anyway with a log line. A marker whose installer
//! is gone, or whose contents are not a pid, is stale: the app starts at once (and says so).
//! With no marker the cost is one failed file open: no thread, no sleep.
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The marker's file name, in the folder that holds the running program.
pub const MARKER: &str = ".installing";
const INTERVAL: Duration = Duration::from_millis(250);
const TIMEOUT: Duration = Duration::from_secs(30);
/// Polls an empty or locked marker may take before it counts as stale: the installer
/// creates the file and writes its pid in one call, so either lasts a moment at most.
const SETTLE_POLLS: u32 = 4;

/// How the wait ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Waited {
    /// No marker at the first check.
    NoMarker,
    /// The marker went away after `polls` sleeps.
    Cleared { polls: u32 },
    /// The marker is not a running install (dead pid, garbage, unreadable): start now.
    Stale { reason: String, polls: u32 },
    /// The installer still ran at the timeout, after `polls` sleeps.
    TimedOut { polls: u32 },
}

/// One look at the marker.
enum Look {
    Gone,
    Live,
    /// Empty or locked: the installer may be writing it right now.
    Settling(String),
    Stale(String),
}

/// The pid in the marker's bytes: ASCII digits, optionally around whitespace, behind a UTF-8
/// BOM, or UTF-16 (a `Set-Content -Encoding Unicode`). None for anything else.
pub fn parse_pid(bytes: &[u8]) -> Option<u32> {
    let text = if let Some(wide) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = wide.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).ok()?
    } else {
        String::from_utf8(bytes.to_vec()).ok()?
    };
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u32>().ok().filter(|pid| *pid != 0)
}

fn look(marker: &Path, read: &mut impl FnMut(&Path) -> std::io::Result<Vec<u8>>, alive: &mut impl FnMut(u32) -> bool) -> Look {
    match read(marker) {
        Err(e) if e.kind() == ErrorKind::NotFound => Look::Gone,
        // ERROR_SHARING_VIOLATION: open for writing by the installer this instant.
        Err(e) if e.raw_os_error() == Some(32) => Look::Settling(format!("marker locked: {e}")),
        Err(e) => Look::Stale(format!("marker unreadable: {e}")),
        Ok(bytes) if bytes.is_empty() => Look::Settling("marker empty".into()),
        Ok(bytes) => match parse_pid(&bytes) {
            None => Look::Stale(format!("marker holds no pid: {:?}", String::from_utf8_lossy(&bytes[..bytes.len().min(32)]))),
            Some(pid) if alive(pid) => Look::Live,
            Some(pid) => Look::Stale(format!("installer pid {pid} is not running")),
        },
    }
}

/// Waits while the marker exists and names a running installer, sleeping `interval` between
/// looks, for at most `timeout`. Time is counted from the sleeps, so tests pass a `sleep`
/// that only records; `read` and `alive` stand in for the file and the process table.
pub fn wait_while_installing(
    marker: &Path,
    interval: Duration,
    timeout: Duration,
    mut read: impl FnMut(&Path) -> std::io::Result<Vec<u8>>,
    mut alive: impl FnMut(u32) -> bool,
    mut sleep: impl FnMut(Duration),
) -> Waited {
    let mut polls = 0;
    let mut settling = 0;
    let mut waited = Duration::ZERO;
    loop {
        match look(marker, &mut read, &mut alive) {
            Look::Gone if polls == 0 => return Waited::NoMarker,
            Look::Gone => return Waited::Cleared { polls },
            Look::Stale(reason) => return Waited::Stale { reason, polls },
            Look::Settling(reason) => {
                settling += 1;
                if settling > SETTLE_POLLS {
                    return Waited::Stale { reason, polls };
                }
            }
            Look::Live => settling = 0,
        }
        if waited >= timeout {
            return Waited::TimedOut { polls };
        }
        let step = interval.min(timeout - waited);
        sleep(step);
        waited += step;
        polls += 1;
    }
}

/// True while `pid` runs: it opens for query and its exit code is still STILL_ACTIVE.
/// A process this user may not query (access denied) exists, so it counts as running.
fn pid_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const STILL_ACTIVE: u32 = 259;
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => {
            let mut code = 0u32;
            let ok = unsafe { GetExitCodeProcess(handle, &mut code) }.is_ok();
            let _ = unsafe { CloseHandle(handle) };
            ok && code == STILL_ACTIVE
        }
        Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
    }
}

/// The marker beside the running program (normally `%LOCALAPPDATA%\Programs\Recall`).
pub fn marker_path() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join(MARKER))
}

/// A file's identity on its volume (serial, index): a renamed file keeps it, a new file at the
/// same path gets another. None when the file cannot be opened.
pub fn file_id(path: &Path) -> Option<(u32, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
    let file = std::fs::File::open(path).ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: a valid handle owned by `file` for the call; `info` is written by the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    Some((info.dwVolumeSerialNumber, (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)))
}

/// After a real wait, the program file at our path is no longer the one this process was started
/// from (the install swapped it: this process runs the old image, renamed into `.old`): start the
/// new program instead. No wait, or an unknown identity, never re-starts.
pub fn needs_restart(waited: &Waited, before: Option<(u32, u64)>, after: Option<(u32, u64)>) -> bool {
    matches!(waited, Waited::Cleared { .. } | Waited::TimedOut { .. }) && before.is_some() && after.is_some() && before != after
}

/// Called at start-up, before the single-instance check.
pub fn wait_for_install() {
    let Some(marker) = marker_path() else { return };
    // The program file this process was started from, by identity, before any swap can move it.
    let exe = std::env::current_exe().ok();
    let before = exe.as_deref().and_then(file_id);
    let waited = wait_while_installing(&marker, INTERVAL, TIMEOUT, |p| std::fs::read(p), pid_alive, std::thread::sleep);
    if let Some(exe) = exe.as_deref() {
        if needs_restart(&waited, before, file_id(exe)) {
            log("install restart", format!("the install replaced {}: starting the new program", exe.display()), serde_json::json!({ "exe": exe.display().to_string() }));
            // The single-instance lock is not taken yet: the new process takes it.
            if std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn().is_ok() {
                std::process::exit(0);
            }
        }
    }
    let ms = |polls: u32| polls as u64 * INTERVAL.as_millis() as u64;
    match waited {
        Waited::NoMarker => {}
        Waited::Cleared { polls } => log("install wait", format!("waited {} ms for an install to finish", ms(polls)), serde_json::json!({ "ms": ms(polls) })),
        Waited::Stale { reason, polls } => log(
            "install marker stale",
            format!("{} is stale ({reason}); starting now", marker.display()),
            serde_json::json!({ "marker": marker.display().to_string(), "reason": reason, "ms": ms(polls) }),
        ),
        Waited::TimedOut { .. } => log(
            "install wait timed out",
            format!("{} still present after {} s; starting anyway", marker.display(), TIMEOUT.as_secs()),
            serde_json::json!({ "marker": marker.display().to_string(), "secs": TIMEOUT.as_secs() }),
        ),
    }
}

fn log(event: &str, line: String, data: serde_json::Value) {
    eprintln!("Recall native: {line}");
    let _ = crate::trace::player_log(event, data);
}

#[cfg(test)]
#[path = "install_wait_tests.rs"]
mod tests;

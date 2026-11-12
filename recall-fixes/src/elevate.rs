//! Start the helper and wait for it: with the shell's `runas` verb when the
//! change needs an administrator (Windows shows its own prompt), else as a
//! plain child. Never from a test build.

use std::path::Path;

/// Windows' error when the user says No to the prompt (ERROR_CANCELLED).
pub const DECLINED: i32 = 1223;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    /// The helper ran and exited with this code.
    Exited(u32),
    /// The user said No to Windows' prompt.
    Declined,
    /// Test, offscreen or dev build: no prompt is ever shown.
    Blocked,
    /// Windows could not start it (the Win32 error).
    Failed(i32),
}

/// `blocked` = the caller's own "never prompt here" check (test builds, offscreen runs).
pub fn run(helper: &Path, params: &str, admin: bool, blocked: bool) -> Started {
    if blocked || cfg!(test) {
        return Started::Blocked;
    }
    run_windows(helper, params, admin)
}

#[cfg(windows)]
fn run_windows(helper: &Path, params: &str, admin: bool) -> Started {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, GetLastError};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let file = HSTRING::from(helper.as_os_str());
    let params = HSTRING::from(params);
    // SAFETY: the strings outlive the call; the process handle is closed below.
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = if admin { w!("runas") } else { w!("open") };
        info.lpFile = PCWSTR(file.as_ptr());
        info.lpParameters = PCWSTR(params.as_ptr());
        // The helper has no window of its own.
        info.nShow = SW_HIDE.0;
        if ShellExecuteExW(&mut info).is_err() {
            return match GetLastError().0 as i32 {
                DECLINED => Started::Declined,
                e => Started::Failed(e),
            };
        }
        if info.hProcess.is_invalid() {
            return Started::Failed(-1);
        }
        // One driver database save takes well under a second; 2 minutes is a hang.
        WaitForSingleObject(info.hProcess, 2 * 60 * 1000);
        let mut code = 0u32;
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        match got {
            Ok(()) => Started::Exited(code),
            Err(_) => Started::Failed(-1),
        }
    }
}

#[cfg(not(windows))]
fn run_windows(_: &Path, _: &str, _: bool) -> Started {
    Started::Failed(-1)
}

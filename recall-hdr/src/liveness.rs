//! Process liveness for a tracked pid (the HDR session's game, the capture
//! target's game, an env-pinned window's game). Extracted from the legacy app's
//! `check_session_liveness` / `open_session_handle` so every user shares one
//! implementation: one `OpenProcess` per tracked pid, a non-blocking
//! `WaitForSingleObject(0)` per tick.
//!
//! FAIL-SAFE: a handle that cannot be opened or waited on is reported as EXITED,
//! so a broken probe can never strand HDR on or pin a dead capture target.
//! Not `Send` (holds a raw `HANDLE`); lives on the pump thread.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

use crate::log;

pub struct ProcessWatch {
    label: &'static str,
    held: Option<(u32, HANDLE)>,
    /// One log line per handle for a failing wait (it would fire every tick).
    wait_failed_logged: bool,
}

impl ProcessWatch {
    pub fn new(label: &'static str) -> Self {
        ProcessWatch { label, held: None, wait_failed_logged: false }
    }

    /// Track `pid` (or nothing). Returns `Some(pid)` when the tracked process
    /// has exited -- or cannot be read (fail-safe) -- so the caller ends its
    /// session. Re-opens when the tracked pid changes.
    pub fn poll(&mut self, pid: Option<u32>) -> Option<u32> {
        match (&self.held, pid) {
            (Some((held, h)), Some(p)) if *held == p => {
                // The handle has SYNCHRONIZE (see `open`), so the wait returns
                // WAIT_OBJECT_0 once the process exits and WAIT_TIMEOUT while it
                // lives.
                let wait = unsafe { WaitForSingleObject(*h, 0) };
                let exited = wait == WAIT_OBJECT_0;
                let failed = wait == WAIT_FAILED;
                if failed && !self.wait_failed_logged {
                    self.wait_failed_logged = true;
                    log!(
                        "{}: pid {p} liveness WaitForSingleObject failed ({:?}); treating it as exited (fail-safe)",
                        self.label,
                        std::io::Error::last_os_error()
                    );
                }
                if exited || failed {
                    if exited {
                        log!("{}: pid {p} exited", self.label);
                    }
                    self.close();
                    return Some(p);
                }
                None
            }
            (Some(_), _) => {
                // Held handle is for a different / no-longer-current pid.
                self.close();
                pid.and_then(|p| self.open(p))
            }
            (None, Some(p)) => self.open(p),
            (None, None) => None,
        }
    }

    /// Open a liveness handle; `Some(pid)` (= already exited) if that fails.
    fn open(&mut self, pid: u32) -> Option<u32> {
        // PROCESS_SYNCHRONIZE is REQUIRED for WaitForSingleObject to signal on
        // exit; QLI alone makes the wait fail with ACCESS_DENIED.
        let rights = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
        self.wait_failed_logged = false; // fresh handle -> fresh log budget
        match unsafe { OpenProcess(rights, false, pid) } {
            Ok(h) if !h.is_invalid() => {
                self.held = Some((pid, h));
                None
            }
            Ok(_) => None,
            Err(e) => {
                // Gone between commit and here: report it so nothing is stranded.
                log!("{}: OpenProcess({pid}) failed ({e}); treating it as exited", self.label);
                Some(pid)
            }
        }
    }

    pub fn close(&mut self) {
        if let Some((_, h)) = self.held.take() {
            unsafe {
                let _ = CloseHandle(h);
            }
        }
        self.wait_failed_logged = false;
    }
}

impl Drop for ProcessWatch {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_alive_and_untracking_closes() {
        let mut w = ProcessWatch::new("test");
        let me = std::process::id();
        assert_eq!(w.poll(Some(me)), None, "open succeeds");
        assert_eq!(w.poll(Some(me)), None, "this process is alive");
        assert!(w.held.is_some());
        assert_eq!(w.poll(None), None);
        assert!(w.held.is_none(), "untracking releases the handle");
    }

    #[test]
    fn unopenable_pid_reports_exited() {
        // pid 0 is the System Idle Process: OpenProcess always fails for it.
        let mut w = ProcessWatch::new("test");
        assert_eq!(w.poll(Some(0)), Some(0));
    }
}

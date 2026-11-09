//! Engine start failures that no restart can fix (audit G7). Windows raises a "System Error:
//! ... .dll was not found" box over the game for every spawn of an engine whose DLLs are
//! missing (a half-finished install, an antivirus quarantine). The controller turns that box
//! off for itself and its children ([`quiet_error_dialogs`], inherited by every spawn), so the
//! engine exits with a loader status instead, and the supervisor stops retrying on it
//! ([`fatal_reason`]).

use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX};

/// No critical-error or missing-file boxes in this process or the processes it starts.
/// Called first thing in `main`.
pub fn quiet_error_dialogs() {
    // SAFETY: process-wide flag; no pointers.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }
}

/// STATUS_DLL_NOT_FOUND
const DLL_NOT_FOUND: u32 = 0xC000_0135;
/// STATUS_ENTRYPOINT_NOT_FOUND (a DLL from another FFmpeg build)
const ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;
/// STATUS_INVALID_IMAGE_FORMAT (a damaged or wrong-architecture file)
const INVALID_IMAGE_FORMAT: u32 = 0xC000_007B;

/// Why an engine exit code means the install is broken, or None for an ordinary exit
/// (those go through the restart policy).
pub fn fatal_reason(code: i32) -> Option<&'static str> {
    match code as u32 {
        DLL_NOT_FOUND => Some("a DLL the engine needs is missing: reinstall Recall"),
        ENTRYPOINT_NOT_FOUND => Some("a DLL next to the engine is from another version: reinstall Recall"),
        INVALID_IMAGE_FORMAT => Some("an engine file is damaged: reinstall Recall"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Diagnostics::Debug::GetErrorMode;

    #[test]
    fn loader_failures_are_named_and_ordinary_exits_are_not() {
        assert!(fatal_reason(0xC000_0135_u32 as i32).unwrap().contains("DLL"));
        assert!(fatal_reason(-1073741515).is_some(), "the same code as Rust's ExitStatus::code() gives it");
        assert!(fatal_reason(0xC000_0139_u32 as i32).is_some());
        assert!(fatal_reason(0xC000_007B_u32 as i32).is_some());
        for ordinary in [0, 1, 2, 101, -1, 0xC000_0005_u32 as i32] {
            assert_eq!(fatal_reason(ordinary), None, "{ordinary:#x}");
        }
    }

    #[test]
    fn a_busy_engine_slot_is_not_fatal_and_reaches_the_restart_policy() {
        // F-startup-5: 86 (another recorder still holds the engine slot) must go through the
        // restart policy's slot-held wait, not stop the supervisor as a broken install.
        assert_eq!(crate::engine_policy::EXIT_SLOT_BUSY, 86);
        assert_eq!(fatal_reason(crate::engine_policy::EXIT_SLOT_BUSY), None);
        // supervisor_manager's Clean arm: the fatal check gives up and `continue`s; every other
        // code, 86 included, is kept as `code = Some(c)` and handed to `policy.on_exit(code, ..)`.
        let sup = include_str!("supervisor_manager.rs");
        let arm = sup.find("ChildExit::Clean(c) => {").expect("the clean-exit arm");
        let fatal = arm + sup[arm..].find("if let Some(why) = crate::engine_exit::fatal_reason(c) {").expect("fatal check");
        let kept = arm + sup[arm..].find("code = Some(c);").expect("the code is kept");
        let busy = arm + sup[arm..].find("if c == EXIT_SLOT_BUSY {").expect("busy logging");
        let policy = arm + sup[arm..].find("policy.on_exit(code, now, started.elapsed())").expect("policy");
        assert!(fatal < kept && kept < busy && busy < policy);
        // Only the fatal branch leaves the arm early.
        assert_eq!(sup[fatal..kept].matches("continue;").count(), 1, "{}", &sup[fatal..kept]);
        assert!(!sup[kept..policy].contains("continue;") && !sup[kept..busy].contains("break"));
    }

    #[test]
    fn the_error_mode_hides_loader_dialogs() {
        quiet_error_dialogs();
        // SAFETY: reads a process-wide flag.
        let mode = unsafe { GetErrorMode() };
        assert_ne!(mode & SEM_FAILCRITICALERRORS.0, 0);
        assert_ne!(mode & SEM_NOOPENFILEERRORBOX.0, 0);
    }
}

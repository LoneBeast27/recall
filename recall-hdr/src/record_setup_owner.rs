//! The Setup that started this recorder paused (`RECALL_SETUP_OWNER_PID`). Setup resumes it
//! when it finishes; if Setup crashes or closes first, nothing would, and the recorder would
//! stay paused for good. So the pump watches the owner on its usual tick (one held handle,
//! a zero-wait poll, no thread) and resumes once the owner is gone.
//!
//! The owner is its pid plus its start time: the handle is opened at controller start and
//! held, so the pid cannot be reused under it, and a process that started after this
//! controller cannot be the Setup that spawned it (a pid reused before the open).
//! FAIL-SAFE: an owner that cannot be opened or read counts as gone (the recorder resumes).

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

use crate::log;
use crate::record_app::Controller;

/// Whether the owning Setup still runs.
pub(crate) trait OwnerProcess {
    fn exited(&mut self) -> bool;
}

/// The real owner: a held process handle (`None` = could not be opened or is not the owner).
pub(crate) struct OsOwner(Option<HANDLE>);

impl OsOwner {
    pub(crate) fn open(pid: u32) -> Self {
        let rights = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
        let Ok(handle) = (unsafe { OpenProcess(rights, false, pid) }) else {
            log!("record: setup owner {pid} could not be opened; treating it as gone");
            return OsOwner(None);
        };
        let mine = started(unsafe { GetCurrentProcess() });
        match (started(handle), mine) {
            (Some(owner), Some(mine)) if owner <= mine => OsOwner(Some(handle)),
            _ => {
                log!("record: pid {pid} is not the Setup that started this recorder; treating the owner as gone");
                unsafe {
                    let _ = CloseHandle(handle);
                }
                OsOwner(None)
            }
        }
    }
}

/// A process's creation time (100 ns ticks), `None` when it cannot be read.
fn started(process: HANDLE) -> Option<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    Some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}

impl OwnerProcess for OsOwner {
    fn exited(&mut self) -> bool {
        match self.0 {
            Some(handle) => (unsafe { WaitForSingleObject(handle, 0) }) != WAIT_TIMEOUT,
            None => true,
        }
    }
}

impl Drop for OsOwner {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }
}

/// Pump step decision: true when the recorder should resume because its owner is gone.
/// With no owner (resumed, finished, or never set) the watch is dropped and nothing is polled.
pub(crate) fn owner_gone<P: OwnerProcess>(owner: Option<u32>, watch: &mut Option<P>) -> bool {
    if owner.is_none() {
        *watch = None;
        return false;
    }
    watch.as_mut().is_some_and(|w| w.exited())
}

impl Controller {
    /// Resume a recorder whose Setup owner closed or crashed before resuming it.
    pub(crate) fn setup_owner_tick(&mut self) {
        if owner_gone(self.setup_owner, &mut self.owner_watch) {
            log!("record: the Setup that started this recorder paused is gone; resuming");
            self.owner_watch = None;
            self.set_paused(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeOwner {
        alive: std::rc::Rc<std::cell::Cell<bool>>,
        polls: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl OwnerProcess for FakeOwner {
        fn exited(&mut self) -> bool {
            self.polls.set(self.polls.get() + 1);
            !self.alive.get()
        }
    }

    #[test]
    fn the_recorder_resumes_once_its_setup_owner_exits() {
        let (alive, polls) = (std::rc::Rc::new(std::cell::Cell::new(true)), std::rc::Rc::new(std::cell::Cell::new(0)));
        let mut watch = Some(FakeOwner { alive: alive.clone(), polls: polls.clone() });
        assert!(!owner_gone(Some(4242), &mut watch), "Setup still open: stay paused");
        assert!(!owner_gone(Some(4242), &mut watch));
        alive.set(false);
        assert!(owner_gone(Some(4242), &mut watch), "Setup crashed: resume");
        assert_eq!(polls.get(), 3, "one zero-wait poll per pump tick");
    }

    #[test]
    fn a_finished_or_resumed_setup_drops_the_watch_and_polls_nothing() {
        let (alive, polls) = (std::rc::Rc::new(std::cell::Cell::new(false)), std::rc::Rc::new(std::cell::Cell::new(0)));
        let mut watch = Some(FakeOwner { alive, polls: polls.clone() });
        assert!(!owner_gone(None, &mut watch), "no owner any more: nothing to resume");
        assert!(watch.is_none(), "the handle is released");
        assert_eq!(polls.get(), 0);
    }

    #[test]
    fn a_real_owner_is_its_pid_and_start_time() {
        // This test process started before itself: it is a valid owner and alive.
        let mut me = OsOwner::open(std::process::id());
        assert!(!me.exited(), "a live owner that started first is watched");
        // pid 0 (System Idle) cannot be opened: fail-safe, gone.
        assert!(OsOwner::open(0).exited());
    }
}

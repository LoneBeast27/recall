//! Which handles the pump's wait holds (`record_pump`). A process handle stays signaled
//! after its process exits, so a watch the controller does not consume (a quit skips `tick`,
//! or a watch whose owner no longer polls it) would end every wait at once: the pump would
//! spin a core. So:
//! - during a quit only the pump event is waited on (`quitting_tick` re-arms nothing);
//! - a watched handle already signaled before a wait gets one immediate pass (its owner
//!   consumes it in `tick`); still signaled on the next wait, it is left out until it is
//!   replaced or no longer watched.

use windows::Win32::Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;

/// A handle is signaled now (a zero wait; a failed wait counts as signaled: it would
/// return at once too).
pub(crate) fn signaled(h: HANDLE) -> bool {
    // SAFETY: the caller holds the handle.
    let got = unsafe { WaitForSingleObject(h, 0) };
    got == WAIT_OBJECT_0 || got == WAIT_FAILED
}

/// The watched handles left signaled after their immediate pass.
#[derive(Debug, Default)]
pub(crate) struct WaitSet {
    stale: Vec<isize>,
}

impl WaitSet {
    /// The handles for this wait: the pump event (`wake`), then each watched process handle
    /// that is not stale. `at_once`: a watched handle just became signaled: run a pass now.
    pub(crate) fn arm(&mut self, wake: Option<HANDLE>, watched: &[HANDLE], quitting: bool, is_signaled: impl Fn(HANDLE) -> bool) -> (Vec<HANDLE>, bool) {
        let mut out: Vec<HANDLE> = wake.into_iter().collect();
        if quitting {
            return (out, false);
        }
        // A stale entry ends with its handle (closed, replaced) or its signal.
        self.stale.retain(|s| watched.iter().any(|h| h.0 as isize == *s) && is_signaled(HANDLE(*s as *mut _)));
        let mut at_once = false;
        for &h in watched {
            let key = h.0 as isize;
            if !is_signaled(h) {
                out.push(h);
            } else if !self.stale.contains(&key) {
                self.stale.push(key);
                at_once = true;
            }
        }
        (out, at_once)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{CreateEventW, SetEvent};
    use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, QS_ALLINPUT};

    /// A manual-reset event set once: stands in for an exited process (signaled for good).
    fn exited() -> HANDLE {
        let h = unsafe { CreateEventW(None, true, false, None) }.expect("event");
        unsafe { SetEvent(h) }.expect("set");
        h
    }

    /// The pump's loop shape for `run`: arm, wait, never consume the watch. Returns passes.
    fn passes(set: &mut WaitSet, watched: HANDLE, quitting: bool, timeout_ms: u32, run: Duration) -> u32 {
        let wake = unsafe { CreateEventW(None, false, false, None) }.expect("wake");
        let (t0, mut n) = (Instant::now(), 0);
        while t0.elapsed() < run {
            let (handles, at_once) = set.arm(Some(wake), &[watched], quitting, signaled);
            let ms = if at_once { 0 } else { timeout_ms };
            unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
            n += 1;
        }
        let _ = unsafe { CloseHandle(wake) };
        n
    }

    #[test]
    fn a_process_that_exits_during_a_quit_never_spins_the_pump() {
        let dead = exited();
        let mut set = WaitSet::default();
        let n = passes(&mut set, dead, true, 250, Duration::from_millis(600));
        assert!(n <= 4, "{n} passes in 600 ms (a spin is thousands)");
        let _ = unsafe { CloseHandle(dead) };
    }

    #[test]
    fn an_unconsumed_signaled_watch_gets_one_pass_then_is_left_out() {
        let dead = exited();
        let mut set = WaitSet::default();
        let n = passes(&mut set, dead, false, 250, Duration::from_millis(600));
        assert!(n <= 5, "{n} passes in 600 ms");
        let (handles, at_once) = set.arm(None, &[dead], false, signaled);
        assert!(handles.is_empty() && !at_once, "stale: left out");
        let (_, at_once) = set.arm(None, &[], false, signaled);
        assert!(!at_once && set.stale.is_empty(), "no longer watched: forgotten");
        let _ = unsafe { CloseHandle(dead) };
    }

    #[test]
    fn a_live_watch_is_waited_on_and_its_exit_gets_a_pass() {
        let live = unsafe { CreateEventW(None, true, false, None) }.expect("event");
        let mut set = WaitSet::default();
        let (handles, at_once) = set.arm(None, &[live], false, signaled);
        assert_eq!((handles, at_once), (vec![live], false));
        unsafe { SetEvent(live) }.expect("set");
        let (handles, at_once) = set.arm(None, &[live], false, signaled);
        assert!(handles.is_empty() && at_once, "exited before the wait: one pass now");
        let _ = unsafe { CloseHandle(live) };
    }
}

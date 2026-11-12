//! A track's wake: the one Win32 event its owner thread (a capture loop, an app watcher) sleeps
//! on next to its stream's own handles, instead of waking every 100 ms to look at flags.
//!
//! Who sets it: a device notice (`notify.rs`), the controller's sync / retire / new saved id
//! (`hold.rs`), and the app scanner's new process list (`apps.rs`). A quiet track still wakes
//! once a second to write its silence (`silence.rs`); a save never waits for it.

use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};

/// An auto-reset event, closed on drop.
struct OwnedEvent(HANDLE);

// A kernel event handle may be set and waited on from any thread.
unsafe impl Send for OwnedEvent {}
unsafe impl Sync for OwnedEvent {}

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// Sets a track's wake from another thread (a notice callback, the scanner).
#[derive(Clone)]
pub(crate) struct Waker(Arc<OwnedEvent>);

impl Waker {
    pub(crate) fn wake(&self) {
        if !self.0 .0.is_invalid() {
            let _ = unsafe { SetEvent(self.0 .0) };
        }
    }
}

/// The wake of one track (`hold::Live::wake`), or of the app scanner.
pub(crate) struct TrackWake {
    event: Waker,
}

impl Default for TrackWake {
    fn default() -> Self {
        // An event that cannot be made leaves an invalid handle: waits then only time out.
        let event = unsafe { CreateEventW(None, false, false, None) }.unwrap_or_default();
        TrackWake { event: Waker(Arc::new(OwnedEvent(event))) }
    }
}

/// What ended a wait.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Woke {
    /// `handles[i]` was signalled.
    Handle(usize),
    Wake,
    Timeout,
}

impl TrackWake {
    pub(crate) fn waker(&self) -> Waker {
        self.event.clone()
    }

    pub(crate) fn wake(&self) {
        self.event.wake();
    }

    /// Sleep until one of `handles`, this wake, or `timeout` (None: no timeout).
    pub(crate) fn wait(&self, handles: &[HANDLE], timeout: Option<Duration>) -> Woke {
        let mut all: Vec<HANDLE> = handles.to_vec();
        let own = !self.event.0 .0.is_invalid();
        if own {
            all.push(self.event.0 .0);
        }
        let ms = timeout.map_or(INFINITE, |t| t.as_millis().min(INFINITE as u128 - 1) as u32);
        if all.is_empty() {
            // Nothing to wait on (no event could be made): sleep the timeout, never forever.
            std::thread::sleep(timeout.unwrap_or(Duration::from_millis(100))); // timer-ok: no event exists here
            return Woke::Timeout;
        }
        let got = unsafe { WaitForMultipleObjects(&all, false, ms) };
        match got.0.wrapping_sub(WAIT_OBJECT_0.0) as usize {
            i if i < all.len() && own && i == all.len() - 1 => Woke::Wake,
            i if i < all.len() => Woke::Handle(i),
            _ if got == WAIT_TIMEOUT => Woke::Timeout,
            // WAIT_FAILED / abandoned: behave as a timeout (the callers look at their state).
            _ => {
                if timeout.is_none() {
                    std::thread::sleep(Duration::from_millis(100)); // timer-ok: never spin on a failed wait
                }
                Woke::Timeout
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_wait_ends_on_its_wake_its_timeout_or_another_handle() {
        let wake = Arc::new(TrackWake::default());
        let owner = {
            let wake = wake.clone();
            std::thread::spawn(move || wake.wait(&[], None))
        };
        wake.waker().wake();
        assert_eq!(owner.join().unwrap(), Woke::Wake, "a waker clone sets the same event");
        let started = Instant::now();
        assert_eq!(wake.wait(&[], Some(Duration::from_millis(30))), Woke::Timeout);
        assert!(started.elapsed() >= Duration::from_millis(25));
        let other = TrackWake::default();
        other.wake();
        assert_eq!(wake.wait(&[other.event.0 .0], Some(Duration::from_secs(5))), Woke::Handle(0));
    }
}

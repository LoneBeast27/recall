//! Wake a waiting thread from another one: an auto-reset Win32 event.
//!
//! The record controller's pump used to wake every 50 ms because pipe commands and
//! engine events arrive on `mpsc` channels, which a `MsgWaitForMultipleObjectsEx` cannot
//! see. Now every sender also calls [`wake`] (the process-wide pump event), and the pump
//! waits on that event with its messages and the process handles it watches
//! (`record_pump`). The engine supervisor's manager thread waits on its own
//! [`WakeEvent`] the same way (`supervisor_manager`).
//!
//! Waiters re-read their state after every return, so a wake is only a hint: one too
//! many costs a pass, and the event being auto-reset never loses a wake that lands
//! between the state read and the wait (it stays signaled until the wait).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject};

/// How often a waiter re-reads its state when no event could be made (never seen): the
/// supervisor's waits and the record pump's (`record_pump::wait_once`) both fall back to
/// this, the old 50 ms cadence, so nothing is lost if `CreateEventW` ever fails.
pub const FALLBACK_POLL: Duration = Duration::from_millis(50);

/// What ended a [`WakeEvent::wait_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    /// [`WakeEvent::wake`] was called.
    Woken,
    /// The other handle (a process) is signaled.
    Other,
    /// The timeout passed (or the wait failed: the caller re-reads its state either way).
    Timeout,
}

/// An auto-reset event (`raw` 0 = none could be made: waits fall back to [`FALLBACK_POLL`]).
pub struct WakeEvent {
    raw: isize,
    wakes: AtomicU64,
}

// SAFETY: a kernel event handle may be set and waited on from any thread.
unsafe impl Send for WakeEvent {}
unsafe impl Sync for WakeEvent {}

impl WakeEvent {
    pub fn new() -> Self {
        // SAFETY: an unnamed auto-reset event, initially not signaled.
        let raw = unsafe { CreateEventW(None, false, false, None) }.map_or(0, |h| h.0 as isize);
        WakeEvent { raw, wakes: AtomicU64::new(0) }
    }

    /// The event's handle, for a caller's own wait array.
    pub fn handle(&self) -> Option<HANDLE> {
        (self.raw != 0).then_some(HANDLE(self.raw as *mut _))
    }

    /// Wake the waiter (cheap; never blocks).
    pub fn wake(&self) {
        self.wakes.fetch_add(1, Ordering::Relaxed);
        if let Some(h) = self.handle() {
            // SAFETY: our own live event.
            let _ = unsafe { SetEvent(h) };
        }
    }

    /// How many times [`Self::wake`] was called (tests, the wake-rate log).
    pub fn wakes(&self) -> u64 {
        self.wakes.load(Ordering::Relaxed)
    }

    /// Wait until woken, `other` is signaled, or `timeout` passes.
    pub fn wait_with(&self, other: Option<HANDLE>, timeout: Duration) -> Woke {
        let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        let Some(own) = self.handle() else {
            return fallback_wait(other, timeout);
        };
        // SAFETY: both handles are live for the wait (the caller holds `other`).
        let got = unsafe {
            match other {
                Some(other) => WaitForMultipleObjects(&[own, other], false, ms),
                None => WaitForSingleObject(own, ms),
            }
        };
        match got.0.wrapping_sub(WAIT_OBJECT_0.0) {
            0 => Woke::Woken,
            1 if other.is_some() => Woke::Other,
            _ => Woke::Timeout,
        }
    }
}

/// No event: wait on `other` (or sleep) for at most [`FALLBACK_POLL`].
fn fallback_wait(other: Option<HANDLE>, timeout: Duration) -> Woke {
    let step = timeout.min(FALLBACK_POLL);
    match other {
        // SAFETY: the caller holds `other` for the wait.
        Some(h) if unsafe { WaitForSingleObject(h, step.as_millis() as u32) } != WAIT_TIMEOUT => Woke::Other,
        Some(_) => Woke::Timeout,
        None => {
            std::thread::sleep(step);
            Woke::Timeout
        }
    }
}

impl Default for WakeEvent {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WakeEvent {
    fn drop(&mut self) {
        if let Some(h) = self.handle() {
            // SAFETY: our own handle, closed once.
            let _ = unsafe { CloseHandle(h) };
        }
    }
}

/// The record controller pump's event (made on first use, lives for the process).
pub fn pump() -> &'static WakeEvent {
    static PUMP: OnceLock<WakeEvent> = OnceLock::new();
    PUMP.get_or_init(WakeEvent::new)
}

/// Wake the record controller's pump: a pipe command, an engine event or an engine
/// state change is waiting for it. Harmless with no pump (legacy mode, tests).
pub fn wake() {
    pump().wake();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_wake_before_the_wait_is_not_lost_and_is_used_once() {
        let ev = WakeEvent::new();
        assert!(ev.handle().is_some());
        ev.wake();
        assert_eq!(ev.wait_with(None, Duration::from_secs(5)), Woke::Woken, "set before the wait: returns at once");
        let t = Instant::now();
        assert_eq!(ev.wait_with(None, Duration::from_millis(30)), Woke::Timeout, "auto-reset: used up");
        assert!(t.elapsed() >= Duration::from_millis(25));
        assert_eq!(ev.wakes(), 1);
    }

    #[test]
    fn a_wake_from_another_thread_ends_a_long_wait() {
        let ev = std::sync::Arc::new(WakeEvent::new());
        let waker = ev.clone();
        let t = Instant::now();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            waker.wake();
        });
        assert_eq!(ev.wait_with(None, Duration::from_secs(10)), Woke::Woken);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        h.join().unwrap();
    }

    #[test]
    fn a_signaled_other_handle_ends_the_wait() {
        let ev = WakeEvent::new();
        let other = WakeEvent::new();
        other.wake();
        assert_eq!(ev.wait_with(other.handle(), Duration::from_secs(5)), Woke::Other);
    }
}

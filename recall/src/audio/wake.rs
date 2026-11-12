//! A track's wake: the one Win32 event its owner thread (a capture loop, an app watcher) sleeps
//! on next to its stream's own handles, instead of waking every 100 ms to fill silence and look
//! at flags.
//!
//! Who sets it: a device notice (`notify.rs`), the controller's sync / retire / new saved id
//! (`hold.rs`), the app scanner's new process list (`apps.rs`), and a save that needs the ring
//! level with the clock (`level_rings`).
//!
//! Silence is filled when it is needed, not on a timer: a quiet track's ring may lag the clock
//! while nothing reads it. A save asks every track to level its ring first and waits for the
//! answers (bounded): the owner thread writes the silence owed up to the clock (`Leveller`) and
//! answers. The samples are the same as a 100 ms fill writes; only when they are written moves.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};

use super::TrackRing;

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

/// The wake of one track (`hold::Live::wake`).
pub(crate) struct TrackWake {
    event: Waker,
    /// Level requests asked, and the newest the owner answered.
    asked: AtomicU64,
    answered: Mutex<u64>,
    answer: Condvar,
    /// An owner thread sleeps on this wake (set by its first wait): only then is a level
    /// request answered, so a ring with no owner (tests) is never waited for.
    owned: AtomicBool,
}

impl Default for TrackWake {
    fn default() -> Self {
        // An event that cannot be made leaves an invalid handle: waits on it fail and the
        // callers fall back on their timeout.
        let event = unsafe { CreateEventW(None, false, false, None) }.unwrap_or_default();
        TrackWake { event: Waker(Arc::new(OwnedEvent(event))), asked: AtomicU64::new(0), answered: Mutex::new(0), answer: Condvar::new(), owned: AtomicBool::new(false) }
    }
}

/// What ended a wait.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Woke {
    /// `handles[i]` was signalled (the track's own wake is the last handle).
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
        self.owned.store(true, Ordering::SeqCst);
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

    /// The owner's side: answer every level request asked so far. `fill` writes the silence
    /// owed up to the clock when the track is quiet (it may write nothing while sound plays).
    pub(crate) fn serve(&self, fill: impl FnOnce()) {
        let asked = self.asked.load(Ordering::SeqCst);
        fill();
        let mut answered = self.answered.lock().unwrap_or_else(|p| p.into_inner());
        if asked > *answered {
            *answered = asked;
            self.answer.notify_all();
        }
    }

    /// A reader's side: ask for a level ring. Returns the ticket `await_level` waits for, or
    /// None when no owner thread serves this ring.
    fn ask(&self) -> Option<u64> {
        if !self.owned.load(Ordering::SeqCst) {
            return None;
        }
        let ticket = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        self.wake();
        Some(ticket)
    }

    /// Wait until the owner answered `ticket`, at most until `deadline`.
    fn await_level(&self, ticket: u64, deadline: Instant) -> bool {
        let mut answered = self.answered.lock().unwrap_or_else(|p| p.into_inner());
        while *answered < ticket {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            answered = self.answer.wait_timeout(answered, left).unwrap_or_else(|p| p.into_inner()).0;
        }
        true
    }
}

/// How long a save waits for every track to level its ring. An owner busy opening a device
/// answers late; the save then cuts that track as it is (as a 100 ms fill could leave it).
pub(crate) const LEVEL_WAIT: Duration = Duration::from_millis(50);

/// Ask every ring's owner to bring it level with the clock, then wait for all (at most `wait`
/// in total). Returns how many answered.
pub(crate) fn level_rings(rings: &[Arc<TrackRing>], wait: Duration) -> usize {
    let tickets: Vec<Option<u64>> = rings.iter().map(|r| r.live.wake.ask()).collect();
    let deadline = Instant::now() + wait;
    rings.iter().zip(tickets).filter(|(r, t)| t.is_some_and(|t| r.live.wake.await_level(t, deadline))).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_waits_for_its_answer_and_the_owner_wakes_for_it() {
        let wake = Arc::new(TrackWake::default());
        let owner = {
            let wake = wake.clone();
            std::thread::spawn(move || {
                // The owner sleeps with no timeout; only the request wakes it.
                assert_eq!(wake.wait(&[], None), Woke::Wake);
                let mut filled = 0;
                wake.serve(|| filled += 1);
                filled
            })
        };
        let ticket = loop {
            // The owner's first wait marks the ring as served.
            if let Some(t) = wake.ask() {
                break t;
            }
            std::thread::yield_now();
        };
        assert!(wake.await_level(ticket, Instant::now() + Duration::from_secs(5)), "answered");
        assert_eq!(owner.join().unwrap(), 1, "filled once");
    }

    #[test]
    fn an_owner_that_does_not_answer_costs_the_reader_its_bound_only() {
        let wake = TrackWake::default();
        assert!(wake.ask().is_none(), "no owner: nothing to wait for");
        wake.owned.store(true, Ordering::SeqCst);
        let started = Instant::now();
        let ticket = wake.ask().unwrap();
        assert!(!wake.await_level(ticket, Instant::now() + Duration::from_millis(30)));
        assert!(started.elapsed() < Duration::from_millis(500));
        // The request stays: the next serve answers it.
        wake.serve(|| {});
        assert!(wake.await_level(ticket, Instant::now()));
    }

    #[test]
    fn a_wait_ends_on_its_timeout_or_another_handle() {
        let wake = TrackWake::default();
        let started = Instant::now();
        assert_eq!(wake.wait(&[], Some(Duration::from_millis(30))), Woke::Timeout);
        assert!(started.elapsed() >= Duration::from_millis(25));
        let other = TrackWake::default();
        other.wake();
        assert_eq!(wake.wait(&[other.event.0 .0], Some(Duration::from_secs(5))), Woke::Handle(0));
        wake.waker().wake();
        assert_eq!(wake.wait(&[], Some(Duration::from_secs(5))), Woke::Wake, "a waker clone sets the same event");
    }
}

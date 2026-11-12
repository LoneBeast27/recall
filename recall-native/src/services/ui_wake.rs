//! Wakes the UI's upkeep pass (`root_poll`) when something it reads changed, instead of a
//! fixed timer. Any thread calls [`wake`] after it publishes state the pass reads (a worker
//! result, a recorder line, an update check); the pass itself sleeps in [`wait`] until a wake
//! or its next real deadline (a toast running out, a debounced save).
//!
//! One pass per process: the library app has one window.
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SendError, Sender};
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

struct Slot {
    pending: bool,
    waker: Option<Waker>,
}

/// One wake point: a pending flag and the waiting task.
pub struct Hub {
    slot: Mutex<Slot>,
}

static HUB: Hub = Hub::new();
/// Wakes asked for, for the before/after wake count.
static WAKES: AtomicU64 = AtomicU64::new(0);

impl Hub {
    pub const fn new() -> Self {
        Self { slot: Mutex::new(Slot { pending: false, waker: None }) }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn wake(&self) {
        let waker = {
            let mut slot = self.slot();
            slot.pending = true;
            slot.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Waits for a wake on this hub, or for `timer` (any timer future: gpui's `Timer::after`).
    pub fn wait<T: Future + Unpin>(&self, timer: Option<T>) -> Wait<'_, T> {
        Wait { hub: self, timer }
    }
}

/// Asks the UI pass to run soon. Cheap, callable from any thread; wakes coalesce.
pub fn wake() {
    WAKES.fetch_add(1, Ordering::Relaxed);
    HUB.wake();
}

/// How many times [`wake`] was called since start.
pub fn wakes() -> u64 {
    WAKES.load(Ordering::Relaxed)
}

/// Why [`Wait`] finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Woke {
    Asked,
    Deadline,
}

/// Resolves on the next wake (one that came before it was polled counts too) or once `timer`
/// fires.
pub struct Wait<'a, T> {
    hub: &'a Hub,
    timer: Option<T>,
}

/// Waits for a [`wake`], or for `timer` when given.
pub fn wait<T: Future + Unpin>(timer: Option<T>) -> Wait<'static, T> {
    HUB.wait(timer)
}

impl<T: Future + Unpin> Future for Wait<'_, T> {
    type Output = Woke;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Woke> {
        {
            let mut slot = self.hub.slot();
            if slot.pending {
                slot.pending = false;
                slot.waker = None;
                return Poll::Ready(Woke::Asked);
            }
            slot.waker = Some(cx.waker().clone());
        }
        if let Some(timer) = self.timer.as_mut()
            && Pin::new(timer).poll(cx).is_ready()
        {
            return Poll::Ready(Woke::Deadline);
        }
        Poll::Pending
    }
}

/// A channel sender that wakes the UI pass after each send (the pass drains the receiver).
pub struct WakeSender<T>(Sender<T>);

impl<T> Clone for WakeSender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> WakeSender<T> {
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        let sent = self.0.send(value);
        if sent.is_ok() {
            wake();
        }
        sent
    }
}

/// `mpsc::channel` whose sender wakes the UI pass.
pub fn channel<T>() -> (WakeSender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel();
    (WakeSender(tx), rx)
}

/// The earliest of `deadlines` from `now`, as a wait (zero when one is already due). `None`
/// when there is none: the pass then waits for a wake only.
pub fn earliest(now: std::time::Instant, deadlines: impl IntoIterator<Item = Option<std::time::Instant>>) -> Option<Duration> {
    deadlines.into_iter().flatten().min().map(|at| at.saturating_duration_since(now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::task::Wake;
    use std::time::Instant;

    struct Count(AtomicU64);
    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A timer that never fires.
    struct Never;
    impl Future for Never {
        type Output = ();
        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            Poll::Pending
        }
    }

    #[test]
    fn a_wake_from_another_thread_resolves_the_wait_once() {
        let count = Arc::new(Count(AtomicU64::new(0)));
        let waker = Waker::from(count.clone());
        let mut cx = Context::from_waker(&waker);
        let hub = Arc::new(Hub::new());

        let mut first = hub.wait(Some(Never));
        assert_eq!(Pin::new(&mut first).poll(&mut cx), Poll::Pending, "nothing asked yet");
        let other = hub.clone();
        std::thread::spawn(move || other.wake()).join().unwrap();
        assert_eq!(count.0.load(Ordering::SeqCst), 1, "the wake reached the waiting task");
        assert_eq!(Pin::new(&mut first).poll(&mut cx), Poll::Ready(Woke::Asked));

        let mut second = hub.wait(Some(Never));
        assert_eq!(Pin::new(&mut second).poll(&mut cx), Poll::Pending, "the wake was used up");
        hub.wake();
        hub.wake();
        assert_eq!(Pin::new(&mut second).poll(&mut cx), Poll::Ready(Woke::Asked), "two wakes coalesce");
        let mut third = hub.wait(Some(Never));
        assert_eq!(Pin::new(&mut third).poll(&mut cx), Poll::Pending);

        let mut timed = hub.wait(Some(std::future::ready(())));
        assert_eq!(Pin::new(&mut timed).poll(&mut cx), Poll::Ready(Woke::Deadline), "a due timer ends the wait");
    }

    #[test]
    fn a_wake_sender_wakes_after_each_send() {
        let (tx, rx) = channel::<u32>();
        let before = wakes();
        std::thread::spawn(move || tx.send(7).unwrap()).join().unwrap();
        assert!(wakes() > before, "the send asked for a pass");
        assert_eq!(rx.try_recv(), Ok(7));
    }

    #[test]
    fn earliest_picks_the_nearest_deadline() {
        let now = Instant::now();
        let at = |ms| Some(now + Duration::from_millis(ms));
        assert_eq!(earliest(now, [None, None]), None, "nothing scheduled: wait for a wake only");
        assert_eq!(earliest(now, [at(3000), None, at(1500)]), Some(Duration::from_millis(1500)));
        assert_eq!(earliest(now, [Some(now - Duration::from_secs(1)), at(10)]), Some(Duration::ZERO), "overdue runs at once");
    }
}

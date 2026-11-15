//! A Win32 event object, owned and closed on drop, and a wait on several handles: the stop and
//! wake signals of the service threads that used to sleep and re-check.
use std::time::Duration;

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects},
};
use windows::core::HSTRING;

pub struct Event(HANDLE);

// SAFETY: an event handle may be set and waited on from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    /// An unnamed event: `manual` stays set until reset (a stop flag), else it resets when one
    /// waiter wakes (a wake signal).
    pub fn new(manual: bool) -> Option<Self> {
        unsafe { CreateEventW(None, manual, false, windows::core::PCWSTR::null()) }.ok().map(Self)
    }

    /// A named event in this session (`Local\<name>`), created or opened: another process
    /// raises it. Auto-reset.
    pub fn named(name: &str) -> Option<Self> {
        let name = HSTRING::from(format!("Local\\{name}"));
        unsafe { CreateEventW(None, false, false, &name) }.ok().map(Self)
    }

    pub fn set(&self) {
        let _ = unsafe { SetEvent(self.0) };
    }

    pub fn handle(&self) -> HANDLE {
        self.0
    }

    /// True when the event was set within `timeout` (`None`: no limit).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn wait(&self, timeout: Option<Duration>) -> bool {
        wait_any(&[self.0], timeout) == Some(0)
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Threads waiting on one kind of notification: each keeps an auto-reset [`Event`] that every
/// [`raise`](Self::raise) sets, for as long as it holds it.
pub struct Subscribers {
    events: std::sync::Mutex<Vec<std::sync::Weak<Event>>>,
    raised: std::sync::atomic::AtomicU64,
}

impl Subscribers {
    pub const fn new() -> Self {
        Self { events: std::sync::Mutex::new(Vec::new()), raised: std::sync::atomic::AtomicU64::new(0) }
    }

    pub fn subscribe(&self) -> Option<std::sync::Arc<Event>> {
        let event = std::sync::Arc::new(Event::new(false)?);
        self.events.lock().unwrap_or_else(|p| p.into_inner()).push(std::sync::Arc::downgrade(&event));
        Some(event)
    }

    /// Sets every live subscriber's event and moves [`raised`](Self::raised) on.
    pub fn raise(&self) {
        self.raised.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let mut events = self.events.lock().unwrap_or_else(|p| p.into_inner());
        events.retain(|event| event.strong_count() > 0);
        for event in events.iter().filter_map(std::sync::Weak::upgrade) {
            event.set();
        }
    }

    /// How many times this was raised.
    pub fn raised(&self) -> u64 {
        self.raised.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Waits for the first of `handles` to be signalled: its index, or `None` on timeout (`None`
/// timeout: no limit) or a failed wait. At most 64 handles.
pub fn wait_any(handles: &[HANDLE], timeout: Option<Duration>) -> Option<usize> {
    let ms = timeout.map_or(INFINITE, |t| t.as_millis().min(u128::from(INFINITE - 1)) as u32);
    let result = unsafe { WaitForMultipleObjects(handles, false, ms) };
    let index = result.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
    (index < handles.len()).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn a_set_from_another_thread_ends_the_wait_at_once() {
        let event = Arc::new(Event::new(false).unwrap());
        let other = event.clone();
        let started = Instant::now();
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            other.set();
        });
        assert!(event.wait(Some(Duration::from_secs(10))));
        assert!(started.elapsed() < Duration::from_secs(5), "woken by the set, not the timeout");
        setter.join().unwrap();
        assert!(!event.wait(Some(Duration::ZERO)), "auto-reset: used up by the wake");
    }

    #[test]
    fn wait_any_names_the_signalled_handle_or_times_out() {
        let (a, b) = (Event::new(true).unwrap(), Event::new(true).unwrap());
        assert_eq!(wait_any(&[a.handle(), b.handle()], Some(Duration::from_millis(10))), None);
        b.set();
        assert_eq!(wait_any(&[a.handle(), b.handle()], Some(Duration::ZERO)), Some(1));
        assert_eq!(wait_any(&[a.handle(), b.handle()], None), Some(1), "manual reset stays set");
    }

    #[test]
    fn a_raise_sets_each_live_subscriber_once() {
        let subscribers = Subscribers::new();
        let (a, b) = (subscribers.subscribe().unwrap(), subscribers.subscribe().unwrap());
        drop(b);
        subscribers.raise();
        assert!(a.wait(Some(Duration::ZERO)));
        assert!(!a.wait(Some(Duration::ZERO)), "auto-reset");
        assert_eq!(subscribers.raised(), 1);
        assert_eq!(subscribers.events.lock().unwrap().len(), 1, "a dropped subscriber is forgotten");
    }

    #[test]
    fn a_named_event_is_shared_by_name() {
        let name = format!("recall-native-test-{}", std::process::id());
        let (mine, theirs) = (Event::named(&name).unwrap(), Event::named(&name).unwrap());
        theirs.set();
        assert!(mine.wait(Some(Duration::from_secs(1))));
    }
}

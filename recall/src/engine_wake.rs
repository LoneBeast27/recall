//! Wakes the engine's event loop when there is something to do, instead of a timer.
//!
//! The loop sleeps (`ControlFlow::WaitUntil`) until the next 1 s periodic deadline
//! (`periodic_update`: the status heartbeat the controller relies on). Everything else
//! that used to wait for the 50 ms idle tick, or that kept the CPU fallback's redraw loop
//! spinning, wakes it through a winit user event:
//! - [`Wake::Quit`]: the stdin thread set `quit` (`engine_stdin`).
//! - [`Wake::Screenshot`]: a screenshot was asked (`Screenshots::request`).
//! - [`Wake::Frame`]: the capture thread stored a CPU frame in `latest` (fallback path
//!   only; the GPU path never sends it). Coalesced: at most one is in flight, so a loop
//!   blocked in a vsync present is not flooded.
//!
//! Before [`install`] (tests, one-shot commands) every wake is a no-op.

use std::sync::atomic::{AtomicBool, Ordering::AcqRel, Ordering::Release};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

/// Why the loop was woken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    Quit,
    Screenshot,
    Frame,
}

static PROXY: OnceLock<Mutex<EventLoopProxy<Wake>>> = OnceLock::new();
/// A [`Wake::Frame`] was sent and the loop has not taken it yet.
static FRAME_PENDING: AtomicBool = AtomicBool::new(false);

/// The event loop's proxy, once, before `run_app`.
pub(crate) fn install(proxy: EventLoopProxy<Wake>) {
    let _ = PROXY.set(Mutex::new(proxy));
}

/// Wake the loop; false when there is no loop (not installed, or it already exited).
pub(crate) fn wake(why: Wake) -> bool {
    let Some(proxy) = PROXY.get() else { return false };
    proxy.lock().map(|p| p.send_event(why).is_ok()).unwrap_or(false)
}

/// The capture thread stored a CPU frame: wake the loop unless a frame wake is pending.
pub(crate) fn frame() {
    if claim(&FRAME_PENDING) && !wake(Wake::Frame) {
        FRAME_PENDING.store(false, Release);
    }
}

/// The loop took the pending [`Wake::Frame`] (before it redraws, so a frame stored after
/// this sends a new wake and none is missed).
pub(crate) fn frame_taken() {
    FRAME_PENDING.store(false, Release);
}

/// True when the caller set `pending` and so must send the wake.
fn claim(pending: &AtomicBool) -> bool {
    !pending.swap(true, AcqRel)
}

/// Whether a wake asks for a CPU-path redraw (only a new frame does; quit, screenshots and
/// the periodic deadline are handled in `about_to_wait` without one).
pub(crate) fn redraws(why: Wake, cpu_path: bool) -> bool {
    cpu_path && why == Wake::Frame
}

/// The periodic work is due (`periodic_update`'s own gate, read before calling it).
pub(crate) fn periodic_due(last: Option<Instant>, every: Duration, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= every)
}

/// When the loop must wake next by itself: the next periodic deadline, never in the past.
pub(crate) fn next_deadline(last: Option<Instant>, every: Duration, now: Instant) -> Instant {
    last.map_or(now, |t| t + every).max(now)
}

#[cfg(test)]
#[path = "engine_wake_tests.rs"]
mod tests;

//! When the overlay wakes. The 4 Hz base tick runs only while there is
//! something to keep up: the island on screen, the stats poller alive, the
//! island editor, a `--smoke-state` fixture. Otherwise the UI thread sleeps
//! in `GetMessageW` until a message comes (a controller line, a window event,
//! the parent exiting) or a one-shot timer reaches the next deadline (a
//! notice ending, a save's wait, the status feed going stale, a HUD probe).
//! While the island shows, the window checks (z-order re-raise, where the
//! game is, the foreground facts in the log) run on window events and on a
//! slow safety re-check, not on every frame.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{App, ANIM_MS, ANIM_TIMER, BASE_MS, BASE_TIMER};
use crate::parent_wait::ParentWait;
use crate::zorder::{self, keep_on_top, Raise};

/// The safety re-check of the window checks while the island shows: a game
/// that resizes its own window or makes itself topmost without any window
/// event. timer-ok: no OS event reports a window's own z-order or size change
/// short of EVENT_OBJECT_LOCATIONCHANGE, which fires on every cursor move.
pub const SAFETY_EVERY: Duration = Duration::from_secs(1);
/// `SetTimer`'s floor (`USER_TIMER_MINIMUM`).
const MIN_MS: u32 = 10;

/// How the base timer is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The 4 Hz base tick.
    Every,
    /// One tick after this many ms (the next deadline).
    After(u32),
    /// No timer: only messages wake the overlay.
    Sleep,
}

/// What keeps the base tick running: the island on screen (or fading), the
/// stats poller (its samples and its grace), the island editor, a smoke
/// fixture's timeline, or a parent that can only be polled.
pub fn needs_base(shown: bool, poller: bool, editing: bool, smoke: bool, parent_polled: bool) -> bool {
    shown || poller || editing || smoke || parent_polled
}

/// The base timer for now: every tick while `active`, else one tick at the
/// earliest deadline still ahead, else none.
pub fn plan(active: bool, now: Instant, deadlines: &[Option<Instant>]) -> Wake {
    if active {
        return Wake::Every;
    }
    match deadlines.iter().flatten().filter(|at| **at > now).min() {
        // Rounded up: a tick never lands before the deadline it is for.
        Some(at) => Wake::After(at.duration_since(now).as_micros().div_ceil(1000).clamp(MIN_MS as u128, i32::MAX as u128) as u32),
        None => Wake::Sleep,
    }
}

/// The window checks and the timers' state.
#[derive(Debug)]
pub(super) struct Checks {
    /// Re-raise over a window above (the foreground changed, or the safety re-check).
    pub raise_due: bool,
    /// Look where the game is again (`probe_place`).
    pub place_due: bool,
    /// Note the foreground and notification state in the log.
    trace_due: bool,
    last_safety: Option<Instant>,
    armed: Wake,
    parent: Option<ParentWait>,
    /// The parent wait could not be registered: asked on each tick instead.
    parent_polled: bool,
}

impl Default for Checks {
    fn default() -> Self {
        Self { raise_due: true, place_due: true, trace_due: true, last_safety: None, armed: Wake::Sleep, parent: None, parent_polled: false }
    }
}

impl App {
    /// Closes the overlay (`WM_CLOSE`) when the parent exits; if that wait
    /// cannot be registered, each base tick asks instead (and the tick keeps running).
    pub fn watch_parent(&mut self, hwnd: HWND) {
        let Some(handle) = self.parent_handle else { return };
        self.checks.parent = ParentWait::register(handle, hwnd);
        self.checks.parent_polled = self.checks.parent.is_none();
        if self.checks.parent_polled {
            self.trace.event("parent wait unavailable: polling the parent each tick");
        }
    }

    /// The parent handle to close, after its wait is gone.
    pub fn release_parent(&mut self) -> Option<HANDLE> {
        self.checks.parent = None;
        self.parent_handle.take()
    }

    /// Only without a registered wait: the parent exited.
    pub(super) fn parent_gone(&self) -> bool {
        self.checks.parent_polled
            && self.parent_handle.is_some_and(|handle| unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0)
    }

    /// At the start of a tick: window events since the last one, or the
    /// safety re-check while the island shows, make the window checks due.
    pub(super) fn take_window_events(&mut self, now: Instant) {
        let event = crate::foreground::take();
        let safety = self.island.shown() && self.checks.last_safety.is_none_or(|at| now.saturating_duration_since(at) >= SAFETY_EVERY);
        if safety {
            crate::foreground::refresh();
            self.checks.last_safety = Some(now);
        }
        if event || safety {
            self.checks.raise_due = true;
            self.checks.place_due = true;
            self.checks.trace_due = true;
        }
    }

    /// The foreground window and the shell's notification state, in the
    /// change-only log, when the window checks are due.
    pub(super) fn note_windows(&mut self) {
        if std::mem::take(&mut self.checks.trace_due) && self.trace.enabled() {
            let foreground = self.names.foreground();
            self.trace.note("foreground", foreground);
            self.trace.note("notification state", zorder::notification_state());
        }
    }

    /// The topmost re-raise, when due (see [`keep_on_top`]).
    pub(super) fn keep_raised(&mut self, hwnd: HWND) {
        if !std::mem::take(&mut self.checks.raise_due) {
            return;
        }
        match keep_on_top(hwnd, &mut self.names) {
            Raise::Clear => {}
            Raise::Raised { above, result: Ok(()) } => {
                self.trace.note("raise", format!("re-raised over {above}"));
                crate::act::raised(true);
            }
            Raise::Raised { above, result: Err(error) } => {
                self.trace.note("raise", format!("re-raise failed: {error}, over {above}"));
                crate::act::raised(false);
            }
        }
    }

    /// At the end of a tick: the base timer for what is running now.
    pub(super) fn arm_base(&mut self, hwnd: HWND, now: Instant) {
        let active = needs_base(self.island.shown(), self.poller.is_some(), self.editing(), self.smoke.is_some(), self.checks.parent_polled);
        let probe = self.prober.next_wake(self.probe_active());
        let wake = plan(active, now, &[self.reducer.next_deadline(now), probe]);
        // A running 4 Hz timer keeps its cadence; a one-shot is set again from now.
        if wake == Wake::Every && self.checks.armed == Wake::Every {
            return;
        }
        unsafe {
            match wake {
                Wake::Every => {
                    SetTimer(Some(hwnd), BASE_TIMER, BASE_MS, None);
                }
                Wake::After(ms) => {
                    SetTimer(Some(hwnd), BASE_TIMER, ms, None);
                }
                Wake::Sleep if self.checks.armed != Wake::Sleep => {
                    let _ = KillTimer(Some(hwnd), BASE_TIMER);
                }
                Wake::Sleep => {}
            }
        }
        self.checks.armed = wake;
    }

    pub(super) fn set_anim(&mut self, hwnd: HWND, on: bool) {
        if on == self.anim_running {
            return;
        }
        unsafe {
            if on {
                SetTimer(Some(hwnd), ANIM_TIMER, ANIM_MS, None);
            } else {
                let _ = KillTimer(Some(hwnd), ANIM_TIMER);
            }
        }
        self.anim_running = on;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_base_timer_while_hidden_with_nothing_due() {
        let now = Instant::now();
        assert!(!needs_base(false, false, false, false, false), "hidden, no poller, no editor, no fixture");
        assert_eq!(plan(false, now, &[None, None]), Wake::Sleep);
        assert_eq!(plan(false, now, &[]), Wake::Sleep);
        assert_eq!(plan(false, now, &[Some(now), Some(now - Duration::from_secs(1))]), Wake::Sleep, "passed deadlines never spin");
    }

    #[test]
    fn anything_on_screen_or_running_keeps_the_base_tick() {
        for i in 0..5 {
            let mut on = [false; 5];
            on[i] = true;
            assert!(needs_base(on[0], on[1], on[2], on[3], on[4]), "input {i}");
        }
        let now = Instant::now();
        assert_eq!(plan(true, now, &[Some(now + Duration::from_millis(30))]), Wake::Every, "the 4 Hz tick covers deadlines");
    }

    #[test]
    fn hidden_wakes_once_at_the_earliest_deadline() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(plan(false, now, &[Some(now + ms(2_501)), Some(now + ms(3_000))]), Wake::After(2_501));
        assert_eq!(plan(false, now, &[None, Some(now + ms(90_000))]), Wake::After(90_000));
        assert_eq!(plan(false, now, &[Some(now + Duration::from_micros(2_500_200))]), Wake::After(2_501), "rounded up, never early");
        assert_eq!(plan(false, now, &[Some(now + Duration::from_micros(300))]), Wake::After(MIN_MS), "SetTimer's floor");
    }
}

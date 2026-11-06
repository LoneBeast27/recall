//! Game detection inputs for the record mode, all on the pump thread:
//!
//! - foreground changes (the watcher's `EVENT_SYSTEM_FOREGROUND` hook),
//! - fullscreen flips of the foreground window: [`Controller::follow_tick`]
//!   re-reads only the followed window's geometry every [`FOLLOW_EVERY`], in
//!   a pump wake that happens anyway (the pump wakes at least every 50 ms),
//!   and probes the window in full only when the verdict flips. A scoped
//!   `EVENT_OBJECT_LOCATIONCHANGE` hook was measured first and dropped: the
//!   thread filter does not hold back cursor moves (5,506 of 5,510 events
//!   in 30 s were `OBJID_CURSOR`, 184/s, each one a pump wake),
//! - the 1 s foreground re-probe (safety net for both),
//! - window liveness of the target and the pinned window, every pump tick.
//!
//! Timing: a first game commits [`crate::record_target::START_DEBOUNCE`] after
//! it is first seen as a target; the commit's wait is logged on the
//! `record: target ->` line ("detected in N ms").

use std::time::{Duration, Instant};

use crate::log;
use crate::record_app::Controller;
use crate::record_target::is_target;
use crate::watcher::{self, WatchEvent};

/// How often the followed foreground's fullscreen verdict is re-read.
pub(crate) const FOLLOW_EVERY: Duration = Duration::from_millis(100);

impl Controller {
    /// A foreground change from the hook: feeds the tracker and HDR.
    pub(crate) fn on_foreground_event(&mut self, ev: &WatchEvent, now: Instant) {
        let (candidate, traits) = self.candidate(ev, now);
        self.last_fed = Some((ev.hwnd, candidate.is_some()));
        self.note_followable(ev);
        // An unidentifiable window is "not a target" (cancels a pending game, T11).
        self.tracker.on_foreground_with(candidate, traits, now);
        if !ev.is_unknown() {
            self.hdr.on_foreground(ev, &self.cfg, now);
        }
    }

    /// A foreground observation from the probe or the location hook: fed to
    /// the tracker only when (window, verdict) changed.
    pub(crate) fn on_foreground_seen(&mut self, ev: &WatchEvent, now: Instant) {
        self.note_followable(ev);
        let (candidate, traits) = self.candidate(ev, now);
        let key = (ev.hwnd, candidate.is_some());
        if self.last_fed != Some(key) {
            self.last_fed = Some(key);
            self.tracker.on_foreground_with(candidate, traits, now);
        }
    }

    /// Remember the foreground window when it could become a target by going
    /// fullscreen (not denied, no `none` override).
    fn note_followable(&mut self, ev: &WatchEvent) {
        self.followable = is_target(&ev.exe_stem, true, &self.cfg).then_some(ev.hwnd);
    }

    /// Re-read the foreground (startup, the 1 s safety net, resume).
    pub(crate) fn probe_foreground(&mut self, now: Instant, initial: bool) {
        self.last_probe = now;
        let Some(ev) = watcher::probe_foreground(self.target_monitor) else { return };
        if initial {
            self.on_foreground_event(&ev, now);
        } else {
            self.on_foreground_seen(&ev, now);
        }
    }

    /// Drop a target / pin whose window no longer exists. `IsWindow` only, so
    /// it runs every pump tick (at most 50 ms after the window closes).
    pub(crate) fn check_windows(&mut self) {
        if let Some(t) = self.pinned.as_ref().filter(|t| !watcher::window_alive(t.hwnd)) {
            log!("record: pinned window {:#x} is gone; auto-detection resumes", t.hwnd);
            self.pinned = None;
        }
        if let Some(hwnd) = self.tracker.current().map(|t| t.hwnd).filter(|h| !watcher::window_alive(*h)) {
            log!("record: target window {hwnd:#x} is gone");
            self.tracker.on_window_gone_at(hwnd, Instant::now());
        }
    }

    /// The window worth following: the foreground, when it could become a
    /// target and is not already the session's window. None while paused or
    /// pinned, so nothing is checked during gameplay at all. A window that
    /// lost the foreground (to one the watcher could not identify) or closed
    /// is dropped at once.
    pub(crate) fn follow_wanted(&self) -> Option<isize> {
        if self.paused || self.pinned.is_some() {
            return None;
        }
        let hwnd = self.followable?;
        if self.tracker.current().is_some_and(|t| t.hwnd == hwnd) || !watcher::is_foreground(hwnd) {
            return None;
        }
        Some(hwnd)
    }

    /// Follow [`Self::follow_wanted`] ([`Follow`]); a due probe feeds the
    /// tracker like the 1 s probe does.
    pub(crate) fn follow_tick(&mut self, now: Instant) {
        let wanted = self.follow_wanted();
        if let Some(hwnd) = self.follow.step(wanted, now, watcher::window_fullscreen) {
            if let Some(ev) = watcher::probe_window(hwnd, self.target_monitor) {
                self.on_foreground_seen(&ev, now);
            }
        }
    }

    /// Note how long the target that just committed waited, for the log line.
    pub(crate) fn note_commit(&mut self) {
        let wait = self.tracker.take_last_wait();
        if let (Some(t), Some(wait)) = (self.tracker.current(), wait) {
            self.detected = Some((t.hwnd, wait));
        }
    }

    /// " (detected in N ms)" for the target `hwnd`, once.
    pub(crate) fn take_detected(&mut self, hwnd: isize) -> String {
        match self.detected.take() {
            Some((h, wait)) if h == hwnd => format!(" (detected in {} ms)", ms(wait)),
            _ => String::new(),
        }
    }
}

fn ms(d: Duration) -> u128 {
    d.as_millis()
}

/// The followed window's last fullscreen verdict. A newly followed window is
/// probed once (catches a flip between its foreground event and now); after
/// that only its verdict is re-read every [`FOLLOW_EVERY`], and a flip asks
/// for a full probe.
pub(crate) struct Follow {
    window: Option<(isize, bool)>,
    checked: Instant,
}

impl Follow {
    pub(crate) fn new(now: Instant) -> Self {
        Follow { window: None, checked: now }
    }

    /// One pump tick. `read` = the fullscreen verdict of a window. Returns the
    /// window to probe in full, if any.
    pub(crate) fn step(&mut self, wanted: Option<isize>, now: Instant, read: impl Fn(isize) -> bool) -> Option<isize> {
        let Some(hwnd) = wanted else {
            self.window = None;
            return None;
        };
        let same = matches!(self.window, Some((h, _)) if h == hwnd);
        if same && now.duration_since(self.checked) < FOLLOW_EVERY {
            return None;
        }
        self.checked = now;
        let fullscreen = read(hwnd);
        if same && self.window == Some((hwnd, fullscreen)) {
            return None;
        }
        self.window = Some((hwnd, fullscreen));
        Some(hwnd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn a_new_window_is_probed_once_then_only_on_a_flip() {
        let t0 = Instant::now();
        let mut f = Follow::new(t0);
        let fs = Cell::new(false);
        let reads = Cell::new(0);
        let read = |_| {
            reads.set(reads.get() + 1);
            fs.get()
        };
        assert_eq!(f.step(Some(7), t0, read), Some(7), "new window: probe once");
        assert_eq!(f.step(Some(7), t0 + Duration::from_millis(50), read), None, "not due yet");
        assert_eq!(reads.get(), 1, "no read before FOLLOW_EVERY");
        assert_eq!(f.step(Some(7), t0 + FOLLOW_EVERY, read), None, "due, no flip");
        assert_eq!(reads.get(), 2);
        fs.set(true);
        let flip = t0 + FOLLOW_EVERY * 2;
        assert_eq!(f.step(Some(7), flip, read), Some(7), "flip to fullscreen: probe");
        assert_eq!(f.step(Some(7), flip + FOLLOW_EVERY, read), None, "still fullscreen");
        fs.set(false);
        assert_eq!(f.step(Some(7), flip + FOLLOW_EVERY * 2, read), Some(7), "flip back: probe");
    }

    #[test]
    fn nothing_wanted_reads_nothing_and_forgets_the_window() {
        let t0 = Instant::now();
        let mut f = Follow::new(t0);
        let seen = RefCell::new(Vec::new());
        let read = |h| {
            seen.borrow_mut().push(h);
            false
        };
        assert_eq!(f.step(Some(7), t0, read), Some(7));
        assert_eq!(f.step(None, t0 + Duration::from_secs(1), read), None);
        assert_eq!(seen.borrow().len(), 1, "no read while nothing is followed");
        // Following the same window again is a new follow: probed at once.
        assert_eq!(f.step(Some(7), t0 + Duration::from_millis(1010), read), Some(7));
    }

    #[test]
    fn switching_windows_probes_the_new_one_at_once() {
        let t0 = Instant::now();
        let mut f = Follow::new(t0);
        assert_eq!(f.step(Some(7), t0, |_| false), Some(7));
        assert_eq!(f.step(Some(8), t0 + Duration::from_millis(10), |_| false), Some(8));
    }
}

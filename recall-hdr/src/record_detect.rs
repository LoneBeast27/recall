//! Game detection inputs for the record mode, all on the pump thread:
//!
//! - foreground changes (the watcher's `EVENT_SYSTEM_FOREGROUND` hook),
//! - fullscreen flips of the foreground window (the watcher's scoped
//!   location hook, see [`Controller::sync_follow`]),
//! - the 1 s foreground re-probe (safety net for both),
//! - window liveness of the target and the pinned window, every pump tick.
//!
//! Timing: a first game commits [`crate::record_target::START_DEBOUNCE`] after
//! it is first seen as a target; the commit's wait is logged on the
//! `record: target ->` line ("detected in N ms").

use std::time::{Duration, Instant};

use crate::log;
use crate::record_app::Controller;
use crate::record_target::{classify_event, is_target};
use crate::watcher::{self, WatchEvent, Watcher};

impl Controller {
    /// A foreground change from the hook: feeds the tracker and HDR.
    pub(crate) fn on_foreground_event(&mut self, ev: &WatchEvent, now: Instant) {
        let candidate = classify_event(ev, &self.cfg);
        self.last_fed = Some((ev.hwnd, candidate.is_some()));
        self.note_followable(ev);
        self.tracker.on_foreground(candidate, now);
        self.hdr.on_foreground(ev, &self.cfg, now);
    }

    /// A foreground observation from the probe or the location hook: fed to
    /// the tracker only when (window, verdict) changed.
    pub(crate) fn on_foreground_seen(&mut self, ev: &WatchEvent, now: Instant) {
        self.note_followable(ev);
        let candidate = classify_event(ev, &self.cfg);
        let key = (ev.hwnd, candidate.is_some());
        if self.last_fed != Some(key) {
            self.last_fed = Some(key);
            self.tracker.on_foreground(candidate, now);
        }
    }

    /// A fullscreen flip from the location hook. Dropped unless it is still
    /// the followed foreground: a move queued before a focus change in the
    /// same pump wake must not feed a background window.
    pub(crate) fn on_follow_event(&mut self, ev: &WatchEvent, now: Instant) {
        if self.followable == Some(ev.hwnd) {
            self.on_foreground_seen(ev, now);
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
            self.tracker.on_window_gone(hwnd);
        }
    }

    /// The window whose location changes are worth an event: the foreground,
    /// when it could become a target and is not already the session's
    /// window. None while paused or pinned (no hook during gameplay at all).
    pub(crate) fn follow_wanted(&self) -> Option<isize> {
        if self.paused || self.pinned.is_some() {
            return None;
        }
        let hwnd = self.followable?;
        if self.tracker.current().is_some_and(|t| t.hwnd == hwnd) || !watcher::window_alive(hwnd) {
            return None;
        }
        Some(hwnd)
    }

    /// Move the watcher's scoped location hook to [`Self::follow_wanted`].
    /// Free when unchanged; runs at the end of every pump tick.
    pub(crate) fn sync_follow(&mut self, watcher: Option<&mut Watcher>) {
        if let Some(w) = watcher {
            w.follow(self.follow_wanted());
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

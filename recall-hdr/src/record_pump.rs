//! The record controller's pump (`record_app`): one `MsgWaitForMultipleObjectsEx` per pass,
//! then the message drain, the channels and `tick`.
//!
//! It used to wake every 50 ms whatever happened, because pipe commands and engine events
//! arrive on `mpsc` channels (20 wakes/s on an idle PC). Now it sleeps until one of:
//! - a window message: the foreground hook, the target's window events
//!   (`record_window_events`), the tray, the hotkeys, the session window, power notices;
//! - the pump event (`pump_wake`): every sender of a pipe command or an engine event, and
//!   every engine state change on the supervisor's threads, sets it;
//! - a watched process ending: the target's, the pinned window's, the overlay's, the Setup
//!   owner's and the HDR session's game (their handles are in the wait array);
//! - the next deadline ([`Controller::next_deadline`]): the debounces, the linger, the
//!   overlay respawn, the hidden-window rule, the island editor's answer, the followed
//!   window's 100 ms read, the fullscreen-only read, and the 1 s foreground re-probe.
//!
//! The 1 s re-probe is the floor: every check with a period of a second or more (the
//! save, quit, recovery and capture-watch timers on the awake clock, the monitor and
//! hotkey retries, the storage schedule) still runs in `tick` on each pass, so it is
//! served at most 1 s after it falls due, as before within its own period.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::UI::WindowsAndMessaging::{MsgWaitForMultipleObjectsEx, MWMO_INPUTAVAILABLE, QS_ALLINPUT};

use crate::control::Inbound;
use crate::log;
use crate::record_app::Controller;
use crate::record_quit::QuitReason;
use crate::watcher::WatchEvent;

/// Foreground re-probe cadence (catches games that go fullscreen after focus).
// timer-ok: no event fires when the foreground window restyles itself in place.
pub(crate) const PROBE_EVERY: Duration = Duration::from_secs(1);
/// A session whose only proof is `fullscreen_games` is re-read at least this often.
// timer-ok: an in-place Alt+Enter to a window has no event (LOCATIONCHANGE is
// excluded, `record_detect`); minimize, foreground and move/size end wake the pump.
pub(crate) const FULLSCREEN_ONLY_EVERY: Duration = Duration::from_millis(500);
/// Hung-safety cap on one wait (the 1 s re-probe deadline is always sooner).
const BACKSTOP_MS: u32 = 1_000;

/// Passes the pump made, for the wake-rate line at shutdown.
#[derive(Debug)]
pub(crate) struct PumpStats {
    since: Instant,
    passes: u64,
}

impl PumpStats {
    pub(crate) fn new(now: Instant) -> Self {
        PumpStats { since: now, passes: 0 }
    }

    pub(crate) fn line(&self, now: Instant) -> String {
        let secs = now.saturating_duration_since(self.since).as_secs_f64().max(0.001);
        format!("record: pump made {} passes in {:.0} s ({:.2}/s)", self.passes, secs, self.passes as f64 / secs)
    }
}

impl Controller {
    pub(super) fn pump(&mut self, watch_rx: &Receiver<WatchEvent>, ctl_rx: &Receiver<Inbound>) -> i32 {
        let mut stats = PumpStats::new(Instant::now());
        let code = self.pump_loop(watch_rx, ctl_rx, &mut stats);
        log!("{} ({} pump wakes asked by other threads)", stats.line(Instant::now()), super::pump_wake::pump().wakes());
        code
    }

    fn pump_loop(&mut self, watch_rx: &Receiver<WatchEvent>, ctl_rx: &Receiver<Inbound>, stats: &mut PumpStats) -> i32 {
        loop {
            self.wait_once();
            stats.passes += 1;
            // Drives the WinEvent hook callbacks, tray menu, hotkeys and the session window.
            if crate::record_session_win::drain_messages(self) {
                if self.quit.is_none() {
                    log!("record: WM_QUIT received");
                }
                self.quit.get_or_insert(QuitReason::WindowsQuit);
            }
            if self.shut_down {
                return 0; // the session ended (`WM_ENDSESSION`)
            }
            let now = Instant::now();
            while let Ok(ev) = watch_rx.try_recv() {
                self.on_foreground_event(&ev, now);
            }
            if now.duration_since(self.last_probe) >= PROBE_EVERY {
                self.probe_foreground(now, false);
            }
            for cmd in self.tray.poll() {
                self.on_tray(cmd);
            }
            if let Some(fired) = self.hotkeys.as_ref().map(|h| h.poll()) {
                self.on_hotkeys(fired);
            }
            self.hotkey_retry_tick(now);
            while let Ok(inbound) = ctl_rx.try_recv() {
                self.handle_inbound(inbound);
            }
            self.session_end_tick(now);
            // A quit waits for the save being written (`record_quit_wait`), then returns.
            if self.quit.is_some() {
                if self.quit_ready(now) {
                    return 0;
                }
                self.quitting_tick(now);
                continue;
            }
            crate::record_power_host::PowerHost::power_tick(self);
            self.session_tick(now);
            self.tick(now);
            self.follow_window_events();
        }
    }

    /// One wait: messages, the pump event, the watched processes, the next deadline.
    fn wait_once(&mut self) {
        let now = Instant::now();
        let timeout = crate::record_quit_wait::pump_wait_ms(self.quit.is_some(), self.next_deadline(now), now, BACKSTOP_MS);
        let handles = self.wait_handles();
        // SAFETY: every handle is held by its owner until after the wait.
        unsafe {
            MsgWaitForMultipleObjectsEx(Some(&handles), timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }

    /// The pump event and the process handles the controller already holds (no new open).
    fn wait_handles(&self) -> Vec<HANDLE> {
        [
            super::pump_wake::pump().handle(),
            self.target_watch.handle(),
            self.pin_watch.handle(),
            self.overlay.handle(),
            self.owner_watch.as_ref().and_then(crate::record_setup_owner::OsOwner::handle),
            self.hdr.liveness_handle(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// The earliest time `tick` has something to do with no event to wake it.
    pub(super) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let follow = self.follow_wanted().map(|hwnd| self.follow.due(hwnd));
        let fullscreen_only = self.tracker.current().is_some_and(|_| self.tracker.fullscreen_only());
        [
            self.tracker.next_deadline(),
            self.planner.linger_deadline(),
            self.hdr.next_deadline(),
            self.janitor.next_deadline(),
            self.overlay.deadline(),
            Some(self.last_probe + PROBE_EVERY),
            follow,
            fullscreen_only.then(|| now + FULLSCREEN_ONLY_EVERY),
            self.hidden.deadline(),
            self.editor_ask.deadline(),
            self.primary.deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Hook the windows of the target's and the pinned window's process (their owner,
    /// read off the window: a UWP target's frame belongs to its host).
    fn follow_window_events(&mut self) {
        let hwnds = [self.tracker.current().map(|t| t.hwnd), self.pinned.as_ref().map(|t| t.hwnd)];
        let pids: Vec<u32> = hwnds.into_iter().flatten().filter_map(super::window_events::window_pid).collect();
        self.window_events.follow(&pids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(deadline: Option<Instant>, now: Instant) -> u32 {
        crate::record_quit_wait::pump_wait_ms(false, deadline, now, BACKSTOP_MS)
    }

    #[test]
    fn the_wait_runs_to_the_next_deadline_only() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(wait(None, now), BACKSTOP_MS, "nothing due: the backstop");
        assert_eq!(wait(Some(now + ms(300)), now), 300);
        assert_eq!(wait(Some(now + ms(5_000)), now), BACKSTOP_MS, "capped");
        assert_eq!(wait(Some(now - ms(10)), now), 0, "passed: at once");
    }

    #[test]
    fn idle_the_pump_sleeps_a_second_not_50_ms() {
        // Only the 1 s re-probe is due: one pass a second (was 20).
        let now = Instant::now();
        assert_eq!(wait(Some(now + PROBE_EVERY), now), 1_000);
        assert_eq!(crate::record_quit_wait::pump_wait_ms(true, Some(now), now, BACKSTOP_MS), crate::record_quit_wait::QUIT_PUMP_WAIT_MS, "a quit's wait is unchanged");
    }

    #[test]
    fn the_stats_line_reads_as_a_rate() {
        let t0 = Instant::now();
        let mut s = PumpStats::new(t0);
        s.passes = 30;
        assert_eq!(s.line(t0 + Duration::from_secs(30)), "record: pump made 30 passes in 30 s (1.00/s)");
    }
}

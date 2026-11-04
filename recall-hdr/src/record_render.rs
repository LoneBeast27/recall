//! The render check on the record path: an unknown fullscreen window (not in
//! a Steam library, no game override) commits as a game only if it renders
//! like one (`render_verdict`). While such a candidate waits for its start
//! debounce, `render_probe` watches its presents; the probe exists only while
//! that candidate is pending and is dropped (session stopped) the moment it
//! commits, is rejected, or stops being the candidate.
//!
//! Without trace rights (not an elevated administrator, not in Performance
//! Log Users) or when the session cannot start, today's rule stands: an
//! unknown fullscreen window is a game, the deny lists still apply.

use std::time::Instant;

use crate::log;
use crate::record_app::Controller;
use crate::record_target::{classify_event, Admit, Target};
use crate::render_probe::{self, RenderProbe, StartError};
use crate::render_verdict::{gate, Gate, Rejected, Verdict, Watch};
use crate::rules;
use crate::watcher::WatchEvent;

pub(crate) struct RenderCheck {
    /// This token may start an ETW session.
    available: bool,
    probe: Option<RenderProbe>,
    /// (pid, hwnd) of the last classified candidate that needs the check.
    wanted: Option<(u32, isize)>,
    rejected: Rejected,
    /// A pid whose probe would not start: today's rule for it.
    failed_for: Option<u32>,
}

impl RenderCheck {
    pub(crate) fn new(available: bool) -> Self {
        if render_probe::stop_stale() {
            log!("record: render check: stopped a session left by an earlier controller");
        }
        if !available {
            log!("record: render check off (no trace rights): any fullscreen app that is not denied counts as a game");
        }
        RenderCheck { available, probe: None, wanted: None, rejected: Rejected::default(), failed_for: None }
    }

    /// Drop the probe (stops its session).
    pub(crate) fn stop(&mut self) {
        self.probe = None;
    }

    /// `t` needs the check: it came from the fullscreen rule alone, and it is
    /// not a new window of the process already recorded (a mode switch).
    fn needed_for(&self, t: &Target, session_pid: Option<u32>) -> bool {
        self.wanted == Some((t.pid, t.hwnd)) && session_pid != Some(t.pid)
    }

    /// The pending candidate that needs the check: keep a probe on exactly it.
    fn follow(&mut self, pending: Option<&Target>, session_pid: Option<u32>) {
        let want = pending.filter(|t| self.needed_for(t, session_pid)).map(|t| t.pid);
        let have = self.probe.as_ref().map(|p| p.pid);
        if want == have {
            return;
        }
        self.probe = None;
        let Some(pid) = want.filter(|pid| self.available && self.failed_for != Some(*pid)) else { return };
        match RenderProbe::start(pid) {
            Ok(probe) => self.probe = Some(probe),
            Err(StartError::Denied) => {
                log!("record: render check refused (no trace rights); fullscreen apps count as games");
                self.available = false;
            }
            Err(StartError::Failed(why)) => {
                log!("record: render check could not start for pid {pid} ({why}); today's rule applies to it");
                self.failed_for = Some(pid);
            }
        }
    }

    /// The tracker's question for a due candidate.
    fn admit(&mut self, t: &Target, session_pid: Option<u32>, now: Instant) -> Admit {
        if !self.needed_for(t, session_pid) {
            return Admit::Commit;
        }
        let Some(probe) = self.probe.as_ref().filter(|p| p.pid == t.pid) else { return Admit::Commit };
        let stats = probe.stats();
        let watched = now.saturating_duration_since(probe.since).as_millis();
        let (gate, verdict) = gate(Watch::Watching { since: probe.since, stats }, now);
        match (gate, verdict) {
            (Gate::Hold(at), _) => return Admit::Hold(at),
            (Gate::Commit, Verdict::Game(why)) => {
                log!("record: render check: {} renders like a game ({}) after {watched} ms: {stats:?}", t.exe, why.as_str());
            }
            (Gate::Commit, _) => {}
            (Gate::Reject(why), _) => {
                self.rejected.note(t.pid, t.hwnd, now);
                let again = self.rejected.until().map_or(0, |u| u.saturating_duration_since(now).as_secs());
                log!(
                    "record: render check: {} is not a game ({}) after {watched} ms: {stats:?}; checked again in {again} s",
                    t.exe,
                    why.as_str()
                );
            }
        }
        self.probe = None;
        if matches!(gate, Gate::Reject(_)) { Admit::Reject } else { Admit::Commit }
    }
}

impl Controller {
    /// [`classify_event`] plus the render check's bookkeeping: a window judged
    /// not a game is no candidate until its recheck time.
    pub(crate) fn classify(&mut self, ev: &WatchEvent, now: Instant) -> Option<Target> {
        let candidate = classify_event(ev, &self.cfg)?;
        let needs = !ev.steam_game && rules::override_kind(&self.cfg, &ev.exe_stem).is_none();
        if needs && self.render.rejected.holds(ev.pid, ev.hwnd, now) {
            return None;
        }
        self.render.wanted = needs.then_some((ev.pid, ev.hwnd));
        Some(candidate)
    }

    /// Before the tracker polls: watch the pending candidate, if it needs it.
    /// Also while paused: the tracker still commits then, and an unchecked
    /// commit would be recorded on resume.
    pub(crate) fn render_tick(&mut self) {
        let session = self.tracker.current().map(|t| t.pid);
        self.render.follow(self.tracker.pending(), session);
    }

    /// The tracker's poll with the render check asked before a commit.
    pub(crate) fn poll_tracker(&mut self, now: Instant) -> bool {
        let session = self.tracker.current().map(|t| t.pid);
        let render = &mut self.render;
        let changed = self.tracker.poll_admit(now, |t| render.admit(t, session, now));
        // A rejected or committed candidate is no longer pending: no ETW session.
        self.render_tick();
        changed
    }
}

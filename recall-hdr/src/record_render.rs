//! The render check on the record path: an unknown fullscreen window (not in
//! a Steam library, no game override, not a known game:
//! `record_target::skips_render_check`) commits as a game only if it renders
//! like one (`render_verdict`). While such a candidate waits for its start
//! debounce, `render_probe` watches its presents; the probe exists only while
//! that candidate is pending and is dropped (session stopped) the moment it
//! commits, is rejected, or stops being the candidate.
//!
//! Without trace rights (not an elevated administrator, not in Performance
//! Log Users) or when the session cannot start, today's rule stands: an
//! unknown fullscreen window is a game, the deny lists still apply.
//!
//! A fullscreen exe that the GPU vendor's driver lists as a game
//! (`gpu_games`) skips the check: it commits on the 300 ms rule, which also
//! records composed borderless games the check would reject. The vendor is
//! asked only after the deny lists passed the window (`classify_event`).

use std::time::Instant;

use crate::gpu_games::KnownGames;
use crate::log;
use crate::record_app::Controller;
use crate::record_composed::{notice_wanted, NOTICE_DETAIL};
use crate::record_engine::EngineTarget;
use crate::record_target::{classify_event, skips_render_check, Admit, Target};
use crate::record_transition::Transitions;
use crate::render_probe::{self, RenderProbe, StartError};
use crate::render_verdict::{gate, ComposedNotice, Gate, Rejected, Verdict, Watch};
use crate::watcher::WatchEvent;

pub(crate) struct RenderCheck {
    /// This token may start an ETW session.
    available: bool,
    probe: Option<RenderProbe>,
    /// (pid, hwnd) of the last classified candidate that needs the check.
    wanted: Option<(u32, isize)>,
    rejected: Rejected,
    /// G4: names a composed fullscreen window left unrecorded, once per exe.
    composed: ComposedNotice,
    /// A pid whose probe would not start: today's rule for it.
    failed_for: Option<u32>,
    /// The GPU vendor's game list (lazy worker, cached per exe path).
    games: KnownGames,
}

impl RenderCheck {
    pub(crate) fn new(available: bool) -> Self {
        if render_probe::stop_stale() {
            log!("record: render check: stopped a session left by an earlier controller");
        }
        if !available {
            log!("record: render check off (no trace rights): any fullscreen app that is not denied counts as a game");
        }
        RenderCheck { available, probe: None, wanted: None, rejected: Rejected::default(), composed: ComposedNotice::default(), failed_for: None, games: KnownGames::new() }
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
        // The vendor's answer can arrive while the candidate waits.
        if self.games.for_pid(t.pid, &t.exe) == Some(true) {
            log!("record: render check skipped: {} lists {} as a game", self.games.vendor(), t.exe);
            self.probe = None;
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
                self.rejected.note(t.pid, t.hwnd, why, stats, now);
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
        let Some(candidate) = classify_event(ev, &self.cfg) else {
            self.note_composed(None, now);
            return None;
        };
        // A Steam-library exe, an override, a `detect.fullscreen_games` entry or
        // a known game keeps the 300 ms rule with no check.
        let plain = !skips_render_check(ev, &self.cfg);
        // Asked only when the check would run: without trace rights every
        // fullscreen window commits anyway.
        let vendor = (plain && self.render.available).then(|| self.render.games.for_pid(ev.pid, &ev.exe_name)).flatten();
        let needs = needs_check(plain, vendor);
        let composed = needs && self.composed_in_front(ev);
        self.note_composed(composed.then_some(ev), now);
        if needs && self.render.rejected.holds(ev.pid, ev.hwnd, now) {
            return None;
        }
        self.render.wanted = needs.then_some((ev.pid, ev.hwnd));
        Some(candidate)
    }

    /// `ev` is a fullscreen window in front, last judged composed by DWM at a
    /// game-like rate, not what is recorded, and not a meeting / office app
    /// or a desktop capture (G4, `record_composed`).
    fn composed_in_front(&self, ev: &WatchEvent) -> bool {
        let recorded = self.tracker.current().is_some_and(|t| (t.pid, t.hwnd) == (ev.pid, ev.hwnd));
        let desktop = *self.planner.applied() == EngineTarget::Desktop;
        let stats = self.render.rejected.composed(ev.pid, ev.hwnd);
        ev.fullscreen && !self.paused && !recorded && notice_wanted(&ev.exe_stem, stats, desktop)
    }

    /// G4: a composed fullscreen window in front for over 20 s is named
    /// once per exe this run (island + log; no auto-record).
    fn note_composed(&mut self, front: Option<&WatchEvent>, now: Instant) {
        let seen = front.map(|e| (e.pid, e.hwnd, e.exe_name.as_str()));
        let Some(exe) = self.render.composed.observe(seen, now) else { return };
        log!("record: not recording {exe}: DWM composes its fullscreen window (no game signal for over 20 s)");
        self.shortcut_notice(&format!("Not recording {exe}"), NOTICE_DETAIL);
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
        self.feed_drain(now);
        let session = self.tracker.current().map(|t| t.pid);
        let render = &mut self.render;
        let changed = self.tracker.poll_admit(now, |t| render.admit(t, session, now));
        // A rejected or committed candidate is no longer pending: no ETW session.
        self.render_tick();
        changed
    }
}

/// A fullscreen-rule candidate (`plain`: not skipped by
/// `record_target::skips_render_check`) needs the render check unless the GPU
/// vendor lists it as a game. `vendor` = `None` while the lookup is on its way.
fn needs_check(plain: bool, vendor: Option<bool>) -> bool {
    plain && vendor != Some(true)
}

#[cfg(test)]
mod tests {
    use super::needs_check;

    #[test]
    fn a_vendor_listed_game_skips_the_render_check() {
        assert!(!needs_check(true, Some(true)), "NVIDIA lists it: 300 ms rule");
        assert!(needs_check(true, Some(false)), "NVIDIA does not: the check decides");
        assert!(needs_check(true, None), "lookup pending (admit asks again) or no vendor");
        assert!(!needs_check(false, None), "Steam library / override / fullscreen_games");
        assert!(!needs_check(false, Some(false)));
    }
}

//! The render check on the record path: an unknown fullscreen window (not in
//! a Steam library, no game override, not a known game:
//! `record_target::skips_render_check`) commits as a game only if it renders
//! like one (`render_verdict`). While such a candidate waits for its start
//! debounce, `render_probe` watches its presents; the probe exists only while
//! that candidate is pending and is dropped (session stopped) the moment it
//! commits, is rejected, or stops being the candidate.
//!
//! Without trace rights (not an elevated administrator, not in Performance
//! Log Users) or when the session cannot start there is no verdict, and an
//! unknown fullscreen window with no other official proof is not recorded
//! (the maintainer 2026-11-07: "use official proof + support for manual application
//! registering"): it is logged once and, after 20 s in front, named once by
//! the G4 notice "Not recording <exe>: add it as a game in Settings"
//! (`record_hosted::check_proof`, [`Controller::classify_unproven`]).
//!
//! A fullscreen exe that the GPU vendor's driver lists as a game
//! (`gpu_games`) skips the check: it commits on the 300 ms rule, which also
//! records composed borderless games the check would reject. The vendor is
//! asked only after the deny lists passed the window (`classify_event`).
//!
//! A fullscreen window only the private list blocks is watched too, but never
//! committed: its verdict feeds the G4 notice (`record_private_watch`).

use std::collections::HashSet;
use std::time::Instant;

use crate::gpu_games::KnownGames;
use crate::log;
use crate::record_app::Controller;
use crate::record_composed::{composed_front, notice_step, notice_text};
use crate::record_hosted::{check_proof, Checked, Classified};
use crate::record_private_watch::{blocked_private, notices_visible, private_admit, private_front, watch_wanted, PrivateWatch};
use crate::record_engine::EngineTarget;
use crate::record_target::{Admit, Target};
use crate::record_transition::Transitions;
use crate::render_probe::{self, RenderProbe, StartError};
use crate::render_verdict::{gate, ComposedNotice, Gate, Rejected, RenderStats, Verdict, Watch, Why};
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
    /// A pid whose probe would not start: no verdict for it.
    failed_for: Option<u32>,
    /// The GPU vendor's game list (lazy worker, cached per exe path).
    games: KnownGames,
    /// A fullscreen window the private list blocks, watched for G4 only.
    private: PrivateWatch,
    /// Exes (lower case) not recorded for want of proof, logged once each.
    unproven: HashSet<String>,
}

impl RenderCheck {
    pub(crate) fn new(available: bool) -> Self {
        let left = render_probe::stop_left_sessions(&render_probe::own_session_name(), render_probe::stop_named);
        if left.own {
            log!("record: render check: stopped a session left by an earlier controller");
        }
        if left.legacy {
            log!("record: render check: stopped the machine-wide {} session an older controller left", render_probe::LEGACY_SESSION);
        }
        if !available {
            log!("record: render check off (no trace rights): an unknown fullscreen app is recorded only with official proof (Steam, launcher or Xbox/Store install, known game, GPU vendor list) or when added as a game in Settings");
        }
        let (rejected, composed) = (Rejected::default(), ComposedNotice::default());
        let (games, private, unproven) = (KnownGames::new(), PrivateWatch::default(), HashSet::new());
        RenderCheck { available, probe: None, wanted: None, rejected, composed, failed_for: None, games, private, unproven }
    }

    /// The check can run for `pid`: trace rights, and its probe did not fail.
    fn can_check(&self, pid: u32) -> bool {
        self.available && self.failed_for != Some(pid)
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
                log!("record: render check refused (no trace rights); unknown fullscreen apps need official proof or a Settings entry");
                self.available = false;
            }
            Err(StartError::Failed(why)) => {
                log!("record: render check could not start for pid {pid} ({why}); no verdict for it, so it is not recorded without other proof");
                self.failed_for = Some(pid);
            }
        }
    }

    /// The tracker's question for a due candidate.
    fn admit(&mut self, t: &Target, session_pid: Option<u32>, now: Instant) -> Admit {
        match admit_route(self.private.is_watched(t.pid, t.hwnd), self.needed_for(t, session_pid)) {
            Route::Private => return self.admit_private(t, now),
            Route::Commit => return Admit::Commit,
            Route::Check => {}
        }
        // The vendor's answer can arrive while the candidate waits.
        if self.games.for_pid(t.pid, &t.exe) == Some(true) {
            log!("record: render check skipped: {} lists {} as a game", self.games.vendor(), t.exe);
            self.probe = None;
            return Admit::Commit;
        }
        let Some(probe) = self.probe.as_ref().filter(|p| p.pid == t.pid) else {
            // No probe (it would not start, or trace rights went away): no
            // verdict, so not a game (no "fullscreen = game" fallback).
            let (Gate::Reject(why), _) = gate(Watch::Unavailable, now) else { return Admit::Reject };
            log!("record: render check: {} not recorded (no render verdict and no other proof that it is a game)", t.exe);
            self.rejected.note(t.pid, t.hwnd, why, RenderStats::default(), now);
            return Admit::Reject;
        };
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

    /// A watched private-list window fell due: note what it rendered (for the
    /// G4 notice) and reject it; ask again while the watch is unsure.
    fn admit_private(&mut self, t: &Target, now: Instant) -> Admit {
        let Some(probe) = self.probe.as_ref().filter(|p| p.pid == t.pid) else {
            self.rejected.note(t.pid, t.hwnd, Why::NoFrames, RenderStats::default(), now);
            return Admit::Reject;
        };
        let stats = probe.stats();
        let watched = now.saturating_duration_since(probe.since).as_millis();
        let (gate, verdict) = gate(Watch::Watching { since: probe.since, stats }, now);
        match private_admit(gate, verdict) {
            Err(at) => Admit::Hold(at),
            Ok(why) => {
                self.rejected.note(t.pid, t.hwnd, why, stats, now);
                log!("record: render check: {} not recorded (private list): {} after {watched} ms: {stats:?}", t.exe, why.as_str());
                self.probe = None;
                Admit::Reject
            }
        }
    }
}

impl Controller {
    /// [`classify_event`] plus the render check's bookkeeping: a window judged
    /// not a game is no candidate until its recheck time.
    pub(crate) fn classify(&mut self, ev: &WatchEvent, now: Instant) -> Option<Target> {
        let can_check = self.render.can_check(ev.pid);
        let games = &mut self.render.games;
        let vendor = || games.for_pid(ev.pid, &ev.exe_name);
        let Classified { target: candidate, needs_check: needs } = match check_proof(ev, &self.cfg, can_check, vendor) {
            Checked::Candidate(c) => c,
            Checked::NoVerdict => return self.classify_unproven(ev, now),
            Checked::No => return self.classify_private(ev, now),
        };
        if self.render.private.is_watched(ev.pid, ev.hwnd) {
            // Opted in since (an override, `fullscreen_games`): no longer watched only.
            self.render.private.window = None;
        }
        let composed = if needs { self.composed_in_front(ev) } else { None };
        self.note_composed(composed.map(|steady| (ev, steady)), now);
        if needs && self.render.rejected.holds(ev.pid, ev.hwnd, now) {
            return None;
        }
        self.render.wanted = needs.then_some((ev.pid, ev.hwnd));
        Some(candidate)
    }

    /// `Some(steady)` when `ev` is a fullscreen window in front, last judged
    /// composed by DWM, not what is recorded, and not a Store app, a meeting /
    /// office app or a desktop capture; `steady` = that watch saw a game-like rate
    /// (G4, `record_composed`).
    fn composed_in_front(&self, ev: &WatchEvent) -> Option<bool> {
        let recorded = self.tracker.current().is_some_and(|t| (t.pid, t.hwnd) == (ev.pid, ev.hwnd));
        let desktop = *self.planner.applied() == EngineTarget::Desktop;
        let stats = self.render.rejected.composed(ev.pid, ev.hwnd);
        (ev.fullscreen && !self.paused && !recorded).then(|| composed_front(&ev.exe_stem, ev.hosted, stats, desktop)).flatten()
    }

    /// A window `classify_checked` turned down. One the private list alone
    /// blocks is logged once per exe, runs the G4 clock on its last verdict,
    /// and is fed as a watch-only candidate while the check can run
    /// (`record_private_watch`); anything else is no candidate.
    fn classify_private(&mut self, ev: &WatchEvent, now: Instant) -> Option<Target> {
        if !blocked_private(ev, &self.cfg) {
            self.note_composed(None, now);
            return None;
        }
        if self.render.private.first_block(&ev.exe_name) {
            log!("record: not recording {}: a call, chat, office or media app (private list); add it as a game in Settings to record it", ev.exe_name);
        }
        let recorded = self.tracker.current().is_some_and(|t| (t.pid, t.hwnd) == (ev.pid, ev.hwnd));
        let desktop = *self.planner.applied() == EngineTarget::Desktop;
        let last = self.render.rejected.verdict(ev.pid, ev.hwnd);
        let front = (!self.paused && !recorded).then(|| private_front(last, desktop)).flatten();
        self.note_composed(front.map(|game_like| (ev, game_like)), now);
        let named = self.render.composed.named(&ev.exe_name);
        if !watch_wanted(self.render.can_check(ev.pid), notices_visible(&self.cfg.overlay), named) || self.render.rejected.holds(ev.pid, ev.hwnd, now) {
            return None;
        }
        self.render.private.window = Some((ev.pid, ev.hwnd));
        self.render.wanted = Some((ev.pid, ev.hwnd));
        Some(Target::from_event(ev))
    }

    /// A candidate with no proof and no verdict possible (`Checked::NoVerdict`):
    /// not recorded. Logged once per exe; while it stays in front, fullscreen,
    /// unrecorded and not on a desktop capture, the G4 clock runs and names it
    /// once after 20 s ("Not recording <exe>: add it as a game in Settings").
    pub(crate) fn classify_unproven(&mut self, ev: &WatchEvent, now: Instant) -> Option<Target> {
        if self.render.unproven.insert(ev.exe_name.to_ascii_lowercase()) {
            log!("record: not recording {}: no proof that it is a game and no render verdict (no trace rights, or the check would not start); add it as a game in Settings to record it", ev.exe_name);
        }
        let recorded = self.tracker.current().is_some_and(|t| (t.pid, t.hwnd) == (ev.pid, ev.hwnd));
        let desktop = *self.planner.applied() == EngineTarget::Desktop;
        let front = unproven_front(ev, self.paused, recorded, desktop);
        self.note_composed(front.map(|steady| (ev, steady)), now);
        None
    }

    /// G4: a composed fullscreen window in front for over 20 s is named
    /// once per exe this run (island + log; no auto-record). Not counted
    /// while notices are off, so it is not used up unseen (R4-detect-2).
    fn note_composed(&mut self, front: Option<(&WatchEvent, bool)>, now: Instant) {
        let seen = front.map(|(e, steady)| ((e.pid, e.hwnd, e.exe_name.as_str()), steady));
        let Some(exe) = notice_step(&mut self.render.composed, seen, notices_visible(&self.cfg.overlay), now) else { return };
        let (title, detail) = notice_text(&exe);
        if self.render.unproven.contains(&exe.to_ascii_lowercase()) {
            log!("record: {title}: {detail} (no proof that it is a game, and no render verdict)");
        } else if crate::private_apps::is_private(&exe) {
            log!("record: {title}: {detail} (on the private list, but it renders like a game)");
        } else {
            log!("record: {title}: {detail} (DWM composes its fullscreen window; no game signal for over 20 s)");
        }
        self.shortcut_notice(&title, detail);
        // The tray's "Add <exe> as a game" registers it in one click.
        self.tray.offer_game(Some(&exe));
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
pub(crate) fn needs_check(plain: bool, vendor: Option<bool>) -> bool {
    plain && vendor != Some(true)
}

/// `Some(true)` while the G4 clock may run for a fullscreen window left
/// unrecorded for want of proof: in front, not paused, not the recorded
/// window, no desktop capture (then it is on the clip), and a plain window
/// (a Store app Recall resolved from its frame is never named, R4-detect-3).
pub(crate) fn unproven_front(ev: &WatchEvent, paused: bool, recorded: bool, desktop: bool) -> Option<bool> {
    (ev.fullscreen && !ev.hosted && !paused && !recorded && !desktop).then_some(true)
}

/// Which way [`RenderCheck::admit`] takes a due candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// A watched private-list window: never committed (`admit_private`).
    Private,
    /// No render check needed: the 300 ms rule commits it.
    Commit,
    /// The render check decides.
    Check,
}

/// `watched` = the candidate is the watched private-list window, `needed` =
/// it needs the render check. A watched window takes the private route first,
/// whatever else holds, so a game verdict never commits it (R7-detect-4).
pub(crate) fn admit_route(watched: bool, needed: bool) -> Route {
    if watched {
        Route::Private
    } else if needed {
        Route::Check
    } else {
        Route::Commit
    }
}

#[cfg(test)]
mod tests {
    use super::{admit_route, needs_check, Route};

    #[test]
    fn a_watched_private_window_never_takes_a_committing_route() {
        assert_eq!(admit_route(true, true), Route::Private, "watched wins over the check");
        assert_eq!(admit_route(true, false), Route::Private, "watched wins over the 300 ms rule");
        assert_eq!(admit_route(false, true), Route::Check);
        assert_eq!(admit_route(false, false), Route::Commit);
    }

    #[test]
    fn a_vendor_listed_game_skips_the_render_check() {
        assert!(!needs_check(true, Some(true)), "NVIDIA lists it: 300 ms rule");
        assert!(needs_check(true, Some(false)), "NVIDIA does not: the check decides");
        assert!(needs_check(true, None), "lookup pending (admit asks again) or no vendor");
        assert!(!needs_check(false, None), "Steam library / override / fullscreen_games");
        assert!(!needs_check(false, Some(false)));
    }
}

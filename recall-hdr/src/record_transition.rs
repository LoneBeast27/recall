//! The controller's side of the transition rules (`record_target_gate`):
//! what the tracker is told about a foreground window, the closed game's
//! drain, and the island notice when a retarget cuts that drain short.
//!
//! The rules live in [`Transitions`]' provided methods, so the record
//! controller and the transition-matrix harness (`transitions_sim::Sim`)
//! run the same code; each only supplies its state and the OS reads
//! (the render check's classification, a window's client size, the island).

use std::time::Instant;

use crate::awake_clock::Awake;
use crate::record_activity::EngineActivity;

use crate::config::Config;
use crate::log;
use crate::record_app::Controller;
use crate::record_engine::{EnginePlanner, EngineTarget};
use crate::record_target::{Target, TargetTracker};
use crate::record_registry::session_ends;
use crate::record_target_gate::{adopted_candidate, drain_cut, fullscreen_only_opt_in, too_small, traits_of, Traits};
use crate::watcher::{self, WatchEvent};

/// The state and reads the transition rules need; the rules themselves are
/// the provided methods.
pub(crate) trait Transitions {
    fn cfg(&self) -> &Config;
    fn tracker(&self) -> &TargetTracker;
    fn tracker_mut(&mut self) -> &mut TargetTracker;
    fn planner(&self) -> &EnginePlanner;
    fn planner_mut(&mut self) -> &mut EnginePlanner;
    /// `record_engine::wanted` for the current state.
    fn wanted_now(&self) -> EngineTarget;
    /// Start, restart or stop the engine for `target` now (`apply_engine_target`).
    fn apply_now(&mut self, target: &EngineTarget);
    /// The privacy stop: the engine stops capture now, finishes its saves, then `target`
    /// starts (`release_engine_to`). `false`: an older engine without `stop_capture`.
    fn release_now(&mut self, target: &EngineTarget) -> bool;
    /// A save or screenshot is in flight, past any timer or cap (`EngineActivity::save_in_flight`).
    fn saving(&mut self) -> bool;
    fn activity_mut(&mut self) -> &mut EngineActivity;
    /// The running engine's hello says `drain_heartbeat`.
    fn engine_heartbeats(&self) -> bool;
    /// Now on the awake-only clock (`Controller::awake`).
    fn awake_now(&self) -> Awake;
    fn now(&self) -> Instant;
    fn pinned(&self) -> Option<&Target>;
    fn is_paused(&self) -> bool;
    /// The render check's classification (`Controller::classify`).
    fn classify_seen(&mut self, ev: &WatchEvent, now: Instant) -> Option<Target>;
    /// The window's client area (width, height).
    fn client_of(&self, hwnd: isize) -> (i32, i32);
    /// An island notice, shortcut-style.
    fn island(&mut self, title: &str, detail: &str);

    /// The tracker's view of a foreground window: a new window of a process
    /// whose session window was just destroyed is adopted when it is
    /// fullscreen or at least `MIN_CLIENT` (T5a; a MessageBox or a small
    /// launcher is not); otherwise the render check's classification, minus a
    /// windowed Steam-library window under the minimum size (T10).
    fn candidate(&mut self, ev: &WatchEvent, now: Instant) -> (Option<Target>, Traits) {
        let client = self.client_of(ev.hwnd);
        if self.tracker().adopts(ev.pid, now) {
            if let Some(t) = adopted_candidate(ev, self.cfg(), client) {
                let fullscreen_only = fullscreen_only_opt_in(ev, self.cfg());
                return (Some(t), Traits { fullscreen: ev.fullscreen, fullscreen_only, ..Traits::ADOPTED });
            }
        }
        let Some(t) = self.classify_seen(ev, now) else { return (None, Traits::FULLSCREEN) };
        let traits = traits_of(ev, self.cfg());
        if too_small(traits, client) {
            return (None, traits);
        }
        (Some(t), traits)
    }

    /// R8-detect-1: the session's own window seen windowed ends a session whose
    /// only proof is `fullscreen_games` (fullscreen Discord after Esc), and the
    /// engine leaves it at once ([`Self::drop_ring`]). Every observation of the
    /// foreground calls this first.
    fn end_if_windowed(&mut self, ev: &WatchEvent) {
        if ev.fullscreen || ev.is_unknown() {
            return;
        }
        let Some(t) = self.tracker().current().filter(|t| (t.pid, t.hwnd) == (ev.pid, ev.hwnd)).cloned() else { return };
        let only_by_list = fullscreen_only_opt_in(ev, self.cfg());
        if self.tracker_mut().on_windowed(ev.hwnd, only_by_list) {
            log!("record: {} left fullscreen; it is recorded only fullscreen ([detect] fullscreen_games): its session ends", t.exe);
            self.drop_ring(&t);
        }
    }

    /// R8-detect-2: a session that ended for privacy (windowed `fullscreen_games`
    /// app, Never record, a removed registration) stops being captured now: no
    /// drain linger on its still-open window, and its unsaved replay ring goes
    /// with the engine. No clip is made. A pin elsewhere takes over.
    ///
    /// F-detect-1: a save in flight is never cut. The engine is released (only then:
    /// R2-detect-r10-1, a release can hold the engine slot while it drains): it stops
    /// capturing at once, takes no new save, and ends after the saves it has; the
    /// planner already counts the next target, so the app is not "recorded" meanwhile.
    /// An older engine without `stop_capture` keeps capturing until its save lands
    /// (the planner holds the stop, no drain linger), then stops.
    fn drop_ring(&mut self, t: &Target) {
        if !matches!(self.planner().applied(), EngineTarget::Window(w) if w.pid == t.pid) {
            return;
        }
        let wanted = self.wanted_now();
        if wanted == *self.planner().applied() {
            return; // the user pinned this very window
        }
        // R2-detect-r10-1: only a save in flight is worth a release; otherwise the plain quit.
        if self.saving() && self.release_now(&wanted) {
            // Design P2: stamped here, in the shared rule, so the Sim runs this very stamp (p6-2).
            let (now, heartbeats) = (self.awake_now(), self.engine_heartbeats());
            self.activity_mut().released(now, heartbeats);
            log!("record: {}'s replay buffer dropped (not saved); capture stopped now, saves in flight finish", t.exe);
            self.planner_mut().force(wanted);
            return;
        }
        if self.saving() {
            log!("record: {}'s session ended during a save; this engine stops when the save lands (no stop_capture)", t.exe);
            let now = self.now();
            self.planner_mut().cut_drain(now);
            return;
        }
        log!("record: {}'s replay buffer dropped (not saved); the engine stops capturing it now", t.exe);
        self.planner_mut().force(wanted.clone());
        self.apply_now(&wanted);
    }

    /// R8-detect-2 / -5: the registrations went from `old` to the current config;
    /// end the live session they took away ([`session_ends`]; `probed` = the
    /// session's window as it is now, `None` when it could not be read) and drop
    /// its ring. Returns `true` when it ended.
    fn end_for_registration(&mut self, old: &Config, probed: Option<&WatchEvent>) -> bool {
        let Some(t) = self.tracker().current().cloned() else { return false };
        if !session_ends(old, self.cfg(), &t, probed) {
            return false;
        }
        log!("record: {} is no longer registered as a game in Settings; its session ends", t.exe);
        self.tracker_mut().on_process_exit(t.pid);
        self.drop_ring(&t);
        true
    }

    /// The closed game's drain while it is still saveable, by the planner's
    /// clock (`plan_engine` names it when a retarget cuts it).
    fn running_drain(&self, now: Instant) -> Option<Target> {
        self.planner().drain().filter(|(_, at, len)| now < *at + *len).map(|(t, _, _)| t.clone())
    }

    /// [`Self::running_drain`], also on the tick the session ended (before
    /// `plan_engine` starts the clock): a game that was due the same tick must
    /// not end a drain that has not started yet.
    fn live_drain(&self, now: Instant) -> Option<Target> {
        if self.planner().drain().is_some() {
            return self.running_drain(now);
        }
        match self.planner().applied() {
            EngineTarget::Window(t) if self.tracker().current().is_none() && self.pinned().is_none() && !self.is_paused() => Some(t.clone()),
            _ => None,
        }
    }

    /// Before the tracker polls: tell it whose drain runs (T2).
    fn feed_drain(&mut self, now: Instant) {
        let pid = self.live_drain(now).map(|t| t.pid);
        self.tracker_mut().set_draining(pid);
    }

    /// A retarget that ended a drain early: say so on the island (not for a
    /// pin, the player's own choice).
    fn note_drain_cut(&mut self, drained: Option<&Target>, applied: &EngineTarget) {
        let pinned = self.pinned().cloned();
        let (Some(a), Some(started)) = (drained, drain_cut(drained, applied, pinned.as_ref())) else { return };
        log!("record: {:?}'s last clip dropped: {started:?} started during its drain", a.name);
        self.island("Last clip dropped", &format!("{started} started"));
    }

    /// A long recording stopped for a retarget: the game closed when the
    /// planner says so, or when a closed game's drain was running (a window
    /// that waited for the drain commits on the tick it runs out).
    fn closed_game(&self, step_says_closed: bool) -> bool {
        step_says_closed || self.planner().drain().is_some()
    }
}

impl Transitions for Controller {
    fn cfg(&self) -> &Config {
        &self.cfg
    }
    fn tracker(&self) -> &TargetTracker {
        &self.tracker
    }
    fn tracker_mut(&mut self) -> &mut TargetTracker {
        &mut self.tracker
    }
    fn planner(&self) -> &EnginePlanner {
        &self.planner
    }
    fn planner_mut(&mut self) -> &mut EnginePlanner {
        &mut self.planner
    }
    fn wanted_now(&self) -> EngineTarget {
        self.wanted()
    }
    fn apply_now(&mut self, target: &EngineTarget) {
        self.apply_engine_target(target);
    }
    fn release_now(&mut self, target: &EngineTarget) -> bool {
        self.release_engine_to(target)
    }
    fn saving(&mut self) -> bool {
        self.activity.save_in_flight()
    }
    fn activity_mut(&mut self) -> &mut EngineActivity {
        &mut self.activity
    }
    fn engine_heartbeats(&self) -> bool {
        self.supervisor.engine_takes_drain_heartbeat()
    }
    fn awake_now(&self) -> Awake {
        self.awake()
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn pinned(&self) -> Option<&Target> {
        self.pinned.as_ref()
    }
    fn is_paused(&self) -> bool {
        self.paused
    }
    fn classify_seen(&mut self, ev: &WatchEvent, now: Instant) -> Option<Target> {
        self.classify(ev, now)
    }
    fn client_of(&self, hwnd: isize) -> (i32, i32) {
        watcher::client_size(hwnd)
    }
    fn island(&mut self, title: &str, detail: &str) {
        self.shortcut_notice(title, detail);
    }
}

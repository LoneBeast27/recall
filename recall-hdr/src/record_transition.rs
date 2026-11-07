//! The controller's side of the transition rules (`record_target_gate`):
//! what the tracker is told about a foreground window, the closed game's
//! drain, and the island notice when a retarget cuts that drain short.
//!
//! The rules live in [`Transitions`]' provided methods, so the record
//! controller and the transition-matrix harness (`transitions_sim::Sim`)
//! run the same code; each only supplies its state and the OS reads
//! (the render check's classification, a window's client size, the island).

use std::time::Instant;

use crate::config::Config;
use crate::log;
use crate::record_app::Controller;
use crate::record_engine::{EnginePlanner, EngineTarget};
use crate::record_target::{Target, TargetTracker};
use crate::record_target_gate::{adopted_candidate, drain_cut, too_small, traits_of, Traits};
use crate::watcher::{self, WatchEvent};

/// The state and reads the transition rules need; the rules themselves are
/// the provided methods.
pub(crate) trait Transitions {
    fn cfg(&self) -> &Config;
    fn tracker(&self) -> &TargetTracker;
    fn tracker_mut(&mut self) -> &mut TargetTracker;
    fn planner(&self) -> &EnginePlanner;
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
                return (Some(t), Traits { fullscreen: ev.fullscreen, ..Traits::ADOPTED });
            }
        }
        let Some(t) = self.classify_seen(ev, now) else { return (None, Traits::FULLSCREEN) };
        let traits = traits_of(ev, self.cfg());
        if too_small(traits, client) {
            return (None, traits);
        }
        (Some(t), traits)
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
